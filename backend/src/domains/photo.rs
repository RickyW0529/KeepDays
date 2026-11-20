use axum::{
    extract::{Path, Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;

use crate::{
    domains::{
        media::{
            bind_owned_asset_in_tx, clone_existing_asset_ref_in_tx, release_asset_ref,
            resolve_asset_urls,
        },
        pet::model::PetStateDoc,
        profile::{load_users_map, UserSummaryDto},
        timeline::record_timeline_event,
    },
    error::{AppError, AppResult},
    infra::{cache::CachedAssetUrls, dict::dim},
    middleware::guards::SpaceGuard,
    snowflake::{i64_str, opt_i64_str, vec_i64_str},
    state::{AppState, PetLifeEvent, PetLifeTrigger},
    time_bj::now_ms,
};

pub fn space_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_photos).post(create_photo))
        .route("/:photo_id", get(get_photo_detail).delete(delete_photo))
        .route("/:photo_id/like", post(toggle_like))
        .route("/:photo_id/comments", post(add_comment))
        .route("/:photo_id/comments/:comment_id", delete(delete_comment))
        .route("/:photo_id/project", post(project_to_shared_space))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PhotoExifMeta {
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lng: Option<f64>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub lens: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoComment {
    #[serde(with = "i64_str")]
    pub id: i64,
    #[serde(with = "i64_str")]
    pub uid: i64,
    #[serde(default, with = "opt_i64_str")]
    pub reply_to_comment_id: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, with = "opt_i64_str")]
    pub audio_asset_id: Option<i64>,
    #[serde(default)]
    pub duration_sec: Option<u8>,
    #[serde(default)]
    pub waveform: Vec<u8>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoPostDoc {
    #[serde(with = "i64_str")]
    pub uploader_id: i64,
    #[serde(default, with = "opt_i64_str")]
    pub live_asset_id: Option<i64>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub exif: PhotoExifMeta,
    #[serde(default, with = "vec_i64_str")]
    pub likes: Vec<i64>,
    #[serde(default)]
    pub comments: Vec<PhotoComment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhotoCardDto {
    #[serde(with = "i64_str")]
    pub id: i64,
    #[serde(with = "i64_str")]
    pub space_id: i64,
    #[serde(with = "i64_str")]
    pub asset_id: i64,
    pub taken_at: i64,
    pub created_at: i64,
    pub post: PhotoPostDoc,
}

#[derive(Deserialize)]
pub struct PhotoWallQuery {
    pub before_taken_at: Option<i64>,
    pub before_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct PhotoWallResp {
    pub photos: Vec<PhotoCardDto>,
    pub next_before_taken_at: Option<i64>,
    pub next_before_id: Option<String>,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn list_photos(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Query(q): Query<PhotoWallQuery>,
) -> AppResult<Json<PhotoWallResp>> {
    let limit = q.limit.unwrap_or(36).clamp(1, 100);
    if q.before_taken_at.is_some() != q.before_id.is_some() {
        return Err(AppError::BadRequest(
            "分页游标需同时携带 before_taken_at 与 before_id".to_string(),
        ));
    }

    let before_id = match &q.before_id {
        Some(s) => Some(
            s.parse::<i64>()
                .map_err(|_| AppError::BadRequest("无效的 before_id 游标".to_string()))?,
        ),
        None => None,
    };

    let rows = match (q.before_taken_at, before_id) {
        (Some(bt), Some(bid)) => {
            sqlx::query(
                "SELECT id, space_id, asset_id, taken_at, post_json, created_at
                 FROM photos
                 WHERE space_id = ?1 AND (taken_at < ?2 OR (taken_at = ?2 AND id < ?3))
                 ORDER BY taken_at DESC, id DESC
                 LIMIT ?4",
            )
            .bind(guard.space_id)
            .bind(bt)
            .bind(bid)
            .bind(limit as i64)
            .fetch_all(&state.db.read)
            .await?
        }
        _ => {
            sqlx::query(
                "SELECT id, space_id, asset_id, taken_at, post_json, created_at
                 FROM photos
                 WHERE space_id = ?1
                 ORDER BY taken_at DESC, id DESC
                 LIMIT ?2",
            )
            .bind(guard.space_id)
            .bind(limit as i64)
            .fetch_all(&state.db.read)
            .await?
        }
    };

    let mut photos = Vec::with_capacity(rows.len());
    let mut uids = Vec::new();
    let mut asset_ids = Vec::with_capacity(rows.len() * 2);

    for r in rows {
        let id: i64 = r.get("id");
        let asset_id: i64 = r.get("asset_id");
        let raw_post: String = r.get("post_json");
        let post: PhotoPostDoc = serde_json::from_str(&raw_post)?;

        asset_ids.push(asset_id);
        collect_post_refs(&post, &mut uids, &mut asset_ids);

        photos.push(PhotoCardDto {
            id,
            space_id: r.get("space_id"),
            asset_id,
            taken_at: r.get("taken_at"),
            created_at: r.get("created_at"),
            post,
        });
    }

    let (next_before_taken_at, next_before_id) = if photos.len() == limit {
        photos
            .last()
            .map(|p| (Some(p.taken_at), Some(p.id.to_string())))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };

    let (users, mut avatar_assets) = load_users_map(&state, &uids).await?;
    asset_ids.append(&mut avatar_assets);
    let asset_urls = resolve_asset_urls(&state, &asset_ids).await?;

    Ok(Json(PhotoWallResp {
        photos,
        next_before_taken_at,
        next_before_id,
        users,
        asset_urls,
    }))
}

#[derive(Deserialize)]
pub struct CreatePhotoReq {
    #[serde(with = "i64_str")]
    pub asset_id: i64,
    #[serde(default, with = "opt_i64_str")]
    pub live_asset_id: Option<i64>,
    pub width: u32,
    pub height: u32,
    pub taken_at: Option<i64>,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub exif: PhotoExifMeta,
}

#[derive(Serialize)]
pub struct SinglePhotoResp {
    pub photo: PhotoCardDto,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn create_photo(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<CreatePhotoReq>,
) -> AppResult<Json<SinglePhotoResp>> {
    if req.width == 0 || req.height == 0 {
        return Err(AppError::BadRequest("照片宽高尺寸无效".to_string()));
    }

    let photo_id = state.snowflake.next_id();
    let now = now_ms();
    let taken_at = req.taken_at.unwrap_or(now);

    let post = PhotoPostDoc {
        uploader_id: guard.user_id,
        live_asset_id: req.live_asset_id,
        width: req.width,
        height: req.height,
        caption: req.caption.trim().to_string(),
        exif: req.exif,
        likes: Vec::new(),
        comments: Vec::new(),
    };
    let post_json = serde_json::to_string(&post)?;

    let mut tx = state.db.write.begin().await?;
    bind_owned_asset_in_tx(&mut tx, req.asset_id, guard.user_id, dim::ASSET_PHOTO, 1).await?;
    if let Some(live_id) = req.live_asset_id {
        bind_owned_asset_in_tx(&mut tx, live_id, guard.user_id, dim::ASSET_LIVE_MP4, 1).await?;
    }

    sqlx::query(
        "INSERT INTO photos (id, space_id, asset_id, taken_at, post_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(photo_id)
    .bind(guard.space_id)
    .bind(req.asset_id)
    .bind(taken_at)
    .bind(&post_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let _ = record_timeline_event(
        &state,
        guard.space_id,
        guard.user_id,
        dim::EVT_PHOTO_POSTED,
        serde_json::json!({
            "photo_id": photo_id.to_string(),
            "asset_id": req.asset_id.to_string(),
            "width": req.width,
            "height": req.height,
            "caption": post.caption,
        }),
    )
    .await;

    if guard.is_shared() {
        let _ = state.pet_reward_tx.send(PetLifeEvent {
            space_id: guard.space_id,
            actor_id: guard.user_id,
            action: PetLifeTrigger::PhotoUploaded,
        });
    }

    let resp = load_single_photo_resp(&state, guard.space_id, photo_id).await?;
    state
        .hub
        .publish(
            guard.space_id,
            "PHOTO_CREATED",
            serde_json::to_value(&resp).unwrap_or_default(),
        )
        .await;
    Ok(Json(resp))
}

async fn get_photo_detail(
    guard: SpaceGuard,
    Path((_sid, raw_pid)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<Json<SinglePhotoResp>> {
    let photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;
    load_single_photo_resp(&state, guard.space_id, photo_id)
        .await
        .map(Json)
}

async fn toggle_like(
    guard: SpaceGuard,
    Path((_sid, raw_pid)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<Json<SinglePhotoResp>> {
    let photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query("SELECT post_json FROM photos WHERE id = ?1 AND space_id = ?2")
        .bind(photo_id)
        .bind(guard.space_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("照片不存在".to_string()))?;

    let mut post: PhotoPostDoc = serde_json::from_str(row.get("post_json"))?;
    let liked_now = if let Some(idx) = post.likes.iter().position(|u| *u == guard.user_id) {
        post.likes.remove(idx);
        false
    } else {
        post.likes.push(guard.user_id);
        true
    };

    let new_json = serde_json::to_string(&post)?;
    sqlx::query("UPDATE photos SET post_json = ?1 WHERE id = ?2 AND space_id = ?3")
        .bind(new_json)
        .bind(photo_id)
        .bind(guard.space_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    if liked_now {
        // Deduplicate rapid like/unlike toggles within 10 minutes
        let dedup_key = format!("like:evt:{}:{}", guard.user_id, photo_id);
        if state.cache.get_str(&dedup_key).await.is_none() {
            state.cache.set_str(&dedup_key, "1", 600).await;
            let _ = record_timeline_event(
                &state,
                guard.space_id,
                guard.user_id,
                dim::EVT_PHOTO_LIKED,
                serde_json::json!({ "photo_id": photo_id.to_string() }),
            )
            .await;
        }
    }

    let resp = load_single_photo_resp(&state, guard.space_id, photo_id).await?;
    state
        .hub
        .publish(
            guard.space_id,
            "PHOTO_MUTATED",
            serde_json::to_value(&resp).unwrap_or_default(),
        )
        .await;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub struct AddCommentReq {
    #[serde(default, with = "opt_i64_str")]
    pub reply_to_comment_id: Option<i64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, with = "opt_i64_str")]
    pub audio_asset_id: Option<i64>,
    #[serde(default)]
    pub duration_sec: Option<u8>,
    #[serde(default)]
    pub waveform: Vec<u8>,
}

async fn add_comment(
    guard: SpaceGuard,
    Path((_sid, raw_pid)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<AddCommentReq>,
) -> AppResult<Json<SinglePhotoResp>> {
    let photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;

    let clean_text = req
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if clean_text.is_none() && req.audio_asset_id.is_none() {
        return Err(AppError::BadRequest(
            "留言内容或声波语音不能同时为空".to_string(),
        ));
    }

    let normalized_dur = if req.audio_asset_id.is_some() {
        let d = req.duration_sec.unwrap_or(1);
        if d == 0 || d > 15 {
            return Err(AppError::BadRequest(
                "声波语音条时长需在 1 ~ 15 秒之间".to_string(),
            ));
        }
        Some(d)
    } else {
        None
    };

    let mut tx = state.db.write.begin().await?;
    let p_row = sqlx::query("SELECT post_json FROM photos WHERE id = ?1 AND space_id = ?2")
        .bind(photo_id)
        .bind(guard.space_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("照片不存在".to_string()))?;

    if let Some(reply_id) = req.reply_to_comment_id {
        let existing_post: PhotoPostDoc = serde_json::from_str(p_row.get("post_json"))?;
        if !existing_post.comments.iter().any(|c| c.id == reply_id) {
            return Err(AppError::BadRequest("回复的目标留言不存在".to_string()));
        }
    }

    if let Some(aid) = req.audio_asset_id {
        bind_owned_asset_in_tx(&mut tx, aid, guard.user_id, dim::ASSET_VOICE, 1).await?;
    }

    let comment = PhotoComment {
        id: state.snowflake.next_id(),
        uid: guard.user_id,
        reply_to_comment_id: req.reply_to_comment_id,
        text: clean_text.clone(),
        audio_asset_id: req.audio_asset_id,
        duration_sec: normalized_dur,
        waveform: req.waveform.into_iter().take(32).collect(),
        at: now_ms(),
    };
    let comment_json = serde_json::to_string(&comment)?;

    sqlx::query(
        "UPDATE photos
         SET post_json = json_insert(post_json, '$.comments[#]', json(?1))
         WHERE id = ?2 AND space_id = ?3",
    )
    .bind(&comment_json)
    .bind(photo_id)
    .bind(guard.space_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let _ = record_timeline_event(
        &state,
        guard.space_id,
        guard.user_id,
        dim::EVT_PHOTO_COMMENTED,
        serde_json::json!({
            "photo_id": photo_id.to_string(),
            "comment_id": comment.id.to_string(),
            "text": clean_text,
            "is_voice": req.audio_asset_id.is_some(),
        }),
    )
    .await;

    if guard.is_shared() {
        let _ = state.pet_reward_tx.send(PetLifeEvent {
            space_id: guard.space_id,
            actor_id: guard.user_id,
            action: PetLifeTrigger::PhotoCommented,
        });
    }

    let resp = load_single_photo_resp(&state, guard.space_id, photo_id).await?;
    state
        .hub
        .publish(
            guard.space_id,
            "PHOTO_MUTATED",
            serde_json::to_value(&resp).unwrap_or_default(),
        )
        .await;
    Ok(Json(resp))
}

async fn delete_comment(
    guard: SpaceGuard,
    Path((_sid, raw_pid, raw_cid)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> AppResult<Json<SinglePhotoResp>> {
    let photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;
    let comment_id = raw_cid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的留言 ID".to_string()))?;

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query("SELECT post_json FROM photos WHERE id = ?1 AND space_id = ?2")
        .bind(photo_id)
        .bind(guard.space_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("照片不存在".to_string()))?;

    let mut post: PhotoPostDoc = serde_json::from_str(row.get("post_json"))?;
    let idx = post
        .comments
        .iter()
        .position(|c| c.id == comment_id)
        .ok_or_else(|| AppError::NotFound("留言不存在".to_string()))?;

    if post.comments[idx].uid != guard.user_id && !guard.is_owner() {
        return Err(AppError::Forbidden("仅留言作者或空间主理人可删除留言".to_string()));
    }

    let removed = post.comments.remove(idx);
    let new_json = serde_json::to_string(&post)?;

    sqlx::query("UPDATE photos SET post_json = ?1 WHERE id = ?2 AND space_id = ?3")
        .bind(new_json)
        .bind(photo_id)
        .bind(guard.space_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    if let Some(aud_id) = removed.audio_asset_id {
        let _ = release_asset_ref(&state, aud_id).await;
    }

    let resp = load_single_photo_resp(&state, guard.space_id, photo_id).await?;
    state
        .hub
        .publish(
            guard.space_id,
            "PHOTO_MUTATED",
            serde_json::to_value(&resp).unwrap_or_default(),
        )
        .await;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub struct ProjectPhotoReq {
    #[serde(with = "i64_str")]
    pub target_space_id: i64,
    pub override_caption: Option<String>,
}

async fn project_to_shared_space(
    guard: SpaceGuard,
    Path((_sid, raw_pid)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<ProjectPhotoReq>,
) -> AppResult<Json<SinglePhotoResp>> {
    let source_photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;

    if req.target_space_id == guard.space_id {
        return Err(AppError::BadRequest("目标空间不能与当前空间相同".to_string()));
    }

    let target_mem = sqlx::query(
        "SELECT s.space_type_id FROM space_members sm
         JOIN spaces s ON s.id = sm.space_id
         WHERE sm.space_id = ?1 AND sm.user_id = ?2",
    )
    .bind(req.target_space_id)
    .bind(guard.user_id)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("目标空间不存在或无访问权限".to_string()))?;

    let target_space_type: i64 = target_mem.get("space_type_id");

    let mut tx = state.db.write.begin().await?;
    let src_row = sqlx::query(
        "SELECT asset_id, taken_at, post_json FROM photos WHERE id = ?1 AND space_id = ?2",
    )
    .bind(source_photo_id)
    .bind(guard.space_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("原照片不存在".to_string()))?;

    let asset_id: i64 = src_row.get("asset_id");
    let taken_at: i64 = src_row.get("taken_at");
    let src_post: PhotoPostDoc = serde_json::from_str(src_row.get("post_json"))?;

    let new_photo_id = state.snowflake.next_id();
    let now = now_ms();
    let new_post = PhotoPostDoc {
        uploader_id: guard.user_id,
        live_asset_id: src_post.live_asset_id,
        width: src_post.width,
        height: src_post.height,
        caption: req
            .override_caption
            .map(|c| c.trim().to_string())
            .unwrap_or(src_post.caption),
        exif: src_post.exif,
        likes: Vec::new(),
        comments: Vec::new(),
    };
    let new_post_json = serde_json::to_string(&new_post)?;

    clone_existing_asset_ref_in_tx(&mut tx, asset_id).await?;
    if let Some(live_id) = new_post.live_asset_id {
        clone_existing_asset_ref_in_tx(&mut tx, live_id).await?;
    }

    sqlx::query(
        "INSERT INTO photos (id, space_id, asset_id, taken_at, post_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(new_photo_id)
    .bind(req.target_space_id)
    .bind(asset_id)
    .bind(taken_at)
    .bind(&new_post_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let _ = record_timeline_event(
        &state,
        req.target_space_id,
        guard.user_id,
        dim::EVT_PHOTO_POSTED,
        serde_json::json!({
            "photo_id": new_photo_id.to_string(),
            "asset_id": asset_id.to_string(),
            "width": new_post.width,
            "height": new_post.height,
            "caption": new_post.caption,
            "projected": true,
        }),
    )
    .await;

    if target_space_type == dim::SPACE_SHARED {
        let _ = state.pet_reward_tx.send(PetLifeEvent {
            space_id: req.target_space_id,
            actor_id: guard.user_id,
            action: PetLifeTrigger::PhotoUploaded,
        });
    }

    let resp = load_single_photo_resp(&state, req.target_space_id, new_photo_id).await?;
    state
        .hub
        .publish(
            req.target_space_id,
            "PHOTO_CREATED",
            serde_json::to_value(&resp).unwrap_or_default(),
        )
        .await;
    Ok(Json(resp))
}

async fn delete_photo(
    guard: SpaceGuard,
    Path((_sid, raw_pid)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let photo_id = raw_pid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的照片 ID".to_string()))?;

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query(
        "DELETE FROM photos WHERE id = ?1 AND space_id = ?2 RETURNING asset_id, post_json",
    )
    .bind(photo_id)
    .bind(guard.space_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("照片不存在".to_string()))?;

    let asset_id: i64 = row.get("asset_id");
    let raw_post: String = row.get("post_json");

    // Scrub dangling references in space Pet module if any (`carrying_old_photo_id`, `focus_photo_id`, `souvenirs`)
    if guard.is_shared() {
        if let Some(p_row) = sqlx::query(
            "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
        )
        .bind(guard.space_id)
        .bind(dim::MOD_PET)
        .fetch_optional(&mut *tx)
        .await?
        {
            if let Ok(mut pet) = serde_json::from_str::<PetStateDoc>(p_row.get("data_json")) {
                let mut touched = false;
                if pet.current.focus_photo_id == Some(photo_id) {
                    pet.current.focus_photo_id = None;
                    touched = true;
                }
                if pet.current.carrying_old_photo_id == Some(photo_id) {
                    pet.current.carrying_old_photo_id = None;
                    pet.current.carrying_old_asset_id = None;
                    if pet.current.behavior == "CARRYING_OLD_PHOTO" {
                        pet.current.behavior = "SUNBATHING_FRIDGE".to_string();
                    }
                    touched = true;
                }
                for sv in &mut pet.souvenirs {
                    if sv.ref_photo_id == Some(photo_id) {
                        sv.ref_photo_id = None;
                        sv.ref_asset_id = None;
                        touched = true;
                    }
                }
                if touched {
                    if let Ok(raw_pet) = serde_json::to_string(&pet) {
                        let _ = sqlx::query(
                            "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
                             WHERE space_id = ?3 AND module_type_id = ?4",
                        )
                        .bind(raw_pet)
                        .bind(now_ms())
                        .bind(guard.space_id)
                        .bind(dim::MOD_PET)
                        .execute(&mut *tx)
                        .await;
                    }
                }
            }
        }
    }

    tx.commit().await?;

    release_asset_ref(&state, asset_id).await?;

    if let Ok(post) = serde_json::from_str::<PhotoPostDoc>(&raw_post) {
        if let Some(live_id) = post.live_asset_id {
            let _ = release_asset_ref(&state, live_id).await;
        }
        for c in post.comments {
            if let Some(aud_id) = c.audio_asset_id {
                let _ = release_asset_ref(&state, aud_id).await;
            }
        }
    }

    state
        .hub
        .publish(
            guard.space_id,
            "PHOTO_DELETED",
            serde_json::json!({
                "photo_id": photo_id.to_string(),
                "actor_id": guard.user_id.to_string(),
            }),
        )
        .await;

    Ok(Json(serde_json::json!({
        "deleted": true,
        "photo_id": photo_id.to_string()
    })))
}

async fn load_single_photo_resp(
    state: &AppState,
    space_id: i64,
    photo_id: i64,
) -> AppResult<SinglePhotoResp> {
    let r = sqlx::query(
        "SELECT id, space_id, asset_id, taken_at, post_json, created_at
         FROM photos WHERE id = ?1 AND space_id = ?2",
    )
    .bind(photo_id)
    .bind(space_id)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("照片不存在".to_string()))?;

    let asset_id: i64 = r.get("asset_id");
    let raw_post: String = r.get("post_json");
    let post: PhotoPostDoc = serde_json::from_str(&raw_post)?;

    let mut uids = Vec::new();
    let mut asset_ids = vec![asset_id];
    collect_post_refs(&post, &mut uids, &mut asset_ids);

    let (users, mut avatar_assets) = load_users_map(state, &uids).await?;
    asset_ids.append(&mut avatar_assets);
    let asset_urls = resolve_asset_urls(state, &asset_ids).await?;

    Ok(SinglePhotoResp {
        photo: PhotoCardDto {
            id: r.get("id"),
            space_id: r.get("space_id"),
            asset_id,
            taken_at: r.get("taken_at"),
            created_at: r.get("created_at"),
            post,
        },
        users,
        asset_urls,
    })
}

fn collect_post_refs(post: &PhotoPostDoc, uids: &mut Vec<i64>, asset_ids: &mut Vec<i64>) {
    uids.push(post.uploader_id);
    for l in &post.likes {
        uids.push(*l);
    }
    if let Some(lid) = post.live_asset_id {
        asset_ids.push(lid);
    }
    for c in &post.comments {
        uids.push(c.uid);
        if let Some(aid) = c.audio_asset_id {
            asset_ids.push(aid);
        }
    }
}
