use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;

use crate::{
    error::{AppError, AppResult},
    infra::{
        cache::CachedAssetUrls,
        dict::dim,
        s3::{ext_from_mime, normalize_mime, S3Service},
    },
    middleware::guards::AuthUser,
    snowflake::i64_str,
    state::AppState,
    time_bj::now_ms,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/presign-upload", post(presign_upload))
        .route("/:asset_id/confirm", post(confirm_upload))
        .route("/hydrate", get(hydrate_query_assets))
}

#[derive(Deserialize)]
pub struct PresignUploadReq {
    pub kind_id: i64,
    pub mime_type: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub has_thumb: bool,
}

#[derive(Serialize)]
pub struct PresignUploadResp {
    #[serde(with = "i64_str")]
    pub asset_id: i64,
    pub kind_code: String,
    pub put_url: String,
    pub thumb_put_url: Option<String>,
    pub expires_in_secs: u64,
}

async fn presign_upload(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<PresignUploadReq>,
) -> AppResult<Json<PresignUploadResp>> {
    if req.size_bytes <= 0 {
        return Err(AppError::BadRequest("文件大小必须大于 0".to_string()));
    }

    let clean_mime = normalize_mime(&req.mime_type);
    let (dim_item, rule) = state.dict.get_asset_rule(req.kind_id).await?;

    if req.size_bytes > rule.max_bytes {
        return Err(AppError::BadRequest(format!(
            "{} 文件体积超出上限 ({} MB)",
            dim_item.label,
            rule.max_bytes / (1024 * 1024)
        )));
    }

    if !rule.mimes.is_empty() && !rule.mimes.iter().any(|m| m.eq_ignore_ascii_case(&clean_mime)) {
        return Err(AppError::BadRequest(format!(
            "不支持的媒体格式: {}",
            req.mime_type
        )));
    }

    let asset_id = state.snowflake.next_id();
    let now = now_ms();

    sqlx::query(
        "INSERT INTO media_assets (id, uploader_id, kind_id, status_id, mime_type, size_bytes, has_thumb, ref_count, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
    )
    .bind(asset_id)
    .bind(auth.user_id)
    .bind(req.kind_id)
    .bind(dim::STATUS_PENDING)
    .bind(&clean_mime)
    .bind(req.size_bytes)
    .bind(if req.has_thumb { 1_i64 } else { 0_i64 })
    .bind(now)
    .execute(&state.db.write)
    .await?;

    let ext = ext_from_mime(&clean_mime);
    let main_key = S3Service::object_key(&rule.s3_dir, asset_id, ext);
    let put_url = state.s3.presign_put(&main_key, 300);
    let thumb_put_url = if req.has_thumb {
        let t_key = S3Service::thumb_key(&rule.s3_dir, asset_id);
        Some(state.s3.presign_put(&t_key, 300))
    } else {
        None
    };

    Ok(Json(PresignUploadResp {
        asset_id,
        kind_code: dim_item.code,
        put_url,
        thumb_put_url,
        expires_in_secs: 300,
    }))
}

#[derive(Serialize)]
struct ConfirmResp {
    #[serde(with = "i64_str")]
    asset_id: i64,
    ready: bool,
}

async fn confirm_upload(
    auth: AuthUser,
    axum::extract::Path(raw_asset_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<ConfirmResp>> {
    let asset_id = raw_asset_id
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的 asset_id".to_string()))?;

    let res = sqlx::query(
        "UPDATE media_assets SET status_id = ?1
         WHERE id = ?2 AND uploader_id = ?3",
    )
    .bind(dim::STATUS_READY)
    .bind(asset_id)
    .bind(auth.user_id)
    .execute(&state.db.write)
    .await?;

    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("媒体资产不存在或无权限".to_string()));
    }

    Ok(Json(ConfirmResp {
        asset_id,
        ready: true,
    }))
}

#[derive(Deserialize)]
struct HydrateQuery {
    ids: String,
}

/// Tenant-scoped media hydration: only resolves assets owned by `auth.user_id`
/// or attached to a space that `auth.user_id` is a member of!
async fn hydrate_query_assets(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<HydrateQuery>,
) -> AppResult<Json<HashMap<String, CachedAssetUrls>>> {
    let candidate_ids: Vec<i64> = q
        .ids
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .take(100)
        .collect();
    if candidate_ids.is_empty() {
        return Ok(Json(HashMap::new()));
    }

    let placeholders: Vec<String> = (2..=candidate_ids.len() + 1)
        .map(|i| format!("?{}", i))
        .collect();
    let sql = format!(
        "SELECT m.id FROM media_assets m
         WHERE m.id IN ({})
           AND (
             m.uploader_id = ?1
             OR EXISTS (
               SELECT 1 FROM photos p
               JOIN space_members sm ON sm.space_id = p.space_id
               WHERE sm.user_id = ?1
                 AND (p.asset_id = m.id OR instr(p.post_json, CAST(m.id AS TEXT)) > 0)
             )
             OR EXISTS (
               SELECT 1 FROM space_modules md
               JOIN space_members sm ON sm.space_id = md.space_id
               WHERE sm.user_id = ?1
                 AND instr(md.data_json, CAST(m.id AS TEXT)) > 0
             )
             OR EXISTS (
               SELECT 1 FROM users u
               JOIN space_members sm_peer ON sm_peer.user_id = u.id
               JOIN space_members sm_me ON sm_me.space_id = sm_peer.space_id
               WHERE sm_me.user_id = ?1
                 AND instr(u.profile_json, CAST(m.id AS TEXT)) > 0
             )
           )",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql).bind(auth.user_id);
    for id in &candidate_ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(&state.db.read).await?;
    let authorized: Vec<i64> = rows.into_iter().map(|r| r.get("id")).collect();

    let urls = resolve_asset_urls(&state, &authorized).await?;
    Ok(Json(urls))
}

pub async fn resolve_asset_urls(
    state: &AppState,
    asset_ids: &[i64],
) -> AppResult<HashMap<String, CachedAssetUrls>> {
    if asset_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=asset_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT id, kind_id, mime_type, has_thumb FROM media_assets WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql);
    for id in asset_ids {
        query = query.bind(*id);
    }

    let rows = query.fetch_all(&state.db.read).await?;
    let meta: Vec<(i64, i64, String, bool)> = rows
        .into_iter()
        .map(|r| {
            (
                r.get("id"),
                r.get("kind_id"),
                r.get("mime_type"),
                r.get::<i64, _>("has_thumb") != 0,
            )
        })
        .collect();

    Ok(state.s3.hydrate_asset_urls(&state.cache, &state.dict, &meta).await)
}

/// Binds an asset uploaded by `expected_owner_uid` with `expected_kind_id`, incrementing `ref_count` by `count`
pub async fn bind_owned_asset_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_id: i64,
    expected_owner_uid: i64,
    expected_kind_id: i64,
    count: i64,
) -> AppResult<()> {
    if count <= 0 {
        return Ok(());
    }
    let res = sqlx::query(
        "UPDATE media_assets
         SET ref_count = ref_count + ?1, status_id = ?2
         WHERE id = ?3 AND uploader_id = ?4 AND kind_id = ?5",
    )
    .bind(count)
    .bind(dim::STATUS_READY)
    .bind(asset_id)
    .bind(expected_owner_uid)
    .bind(expected_kind_id)
    .execute(&mut **tx)
    .await?;

    if res.rows_affected() == 0 {
        return Err(AppError::BadRequest(format!(
            "媒体资产 ({}) 不存在、类型不匹配或无所有权",
            asset_id
        )));
    }
    Ok(())
}

/// Increments `ref_count` for an already-authorized cross-space zero-copy projection
pub async fn clone_existing_asset_ref_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    asset_id: i64,
) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE media_assets
         SET ref_count = ref_count + 1, status_id = ?1
         WHERE id = ?2",
    )
    .bind(dim::STATUS_READY)
    .bind(asset_id)
    .execute(&mut **tx)
    .await?;

    if res.rows_affected() == 0 {
        return Err(AppError::BadRequest(format!(
            "引用的媒体资产 ({}) 不存在",
            asset_id
        )));
    }
    Ok(())
}

/// Decrements `ref_count` by `count` and physically deletes from S3 ONLY when `ref_count == 0`
/// and no `photos` row still references it!
pub async fn release_asset_ref_by(state: &AppState, asset_id: i64, count: i64) -> AppResult<()> {
    if count <= 0 {
        return Ok(());
    }
    let row = sqlx::query(
        "UPDATE media_assets
         SET ref_count = MAX(0, ref_count - ?2)
         WHERE id = ?1
         RETURNING ref_count, kind_id, mime_type, has_thumb",
    )
    .bind(asset_id)
    .bind(count)
    .fetch_optional(&state.db.write)
    .await?;

    if let Some(r) = row {
        let remaining: i64 = r.get("ref_count");
        if remaining == 0 {
            let kind_id: i64 = r.get("kind_id");
            let mime: String = r.get("mime_type");
            let has_thumb: bool = r.get::<i64, _>("has_thumb") != 0;

            let del = sqlx::query(
                "DELETE FROM media_assets
                 WHERE id = ?1 AND ref_count = 0
                   AND NOT EXISTS (SELECT 1 FROM photos WHERE asset_id = ?1)",
            )
            .bind(asset_id)
            .execute(&state.db.write)
            .await?;

            if del.rows_affected() > 0 {
                state.cache.del_key(&format!("s3:sign:{}", asset_id)).await;

                let s3 = state.s3.clone();
                let dict = state.dict.clone();
                tokio::spawn(async move {
                    let s3_dir = dict
                        .get_asset_rule(kind_id)
                        .await
                        .map(|(_, rule)| rule.s3_dir)
                        .unwrap_or_else(|_| "photos".to_string());
                    let main_key = S3Service::object_key(&s3_dir, asset_id, ext_from_mime(&mime));
                    s3.delete_object(&main_key).await;
                    if has_thumb {
                        s3.delete_object(&S3Service::thumb_key(&s3_dir, asset_id))
                            .await;
                    }
                });
            }
        }
    }
    Ok(())
}

pub async fn release_asset_ref(state: &AppState, asset_id: i64) -> AppResult<()> {
    release_asset_ref_by(state, asset_id, 1).await
}
