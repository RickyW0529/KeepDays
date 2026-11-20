use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet};

use crate::{
    domains::{
        media::{bind_owned_asset_in_tx, release_asset_ref_by, resolve_asset_urls},
        timeline::record_timeline_event,
    },
    error::{AppError, AppResult},
    infra::{cache::CachedAssetUrls, dict::dim},
    middleware::guards::AuthUser,
    snowflake::{i64_str, opt_i64_str},
    state::AppState,
    time_bj::now_ms,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(get_my_profile).put(update_my_profile))
        .route("/:user_id", get(get_user_profile))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileHighlight {
    pub year: String,
    pub title: String,
    #[serde(default, with = "opt_i64_str")]
    pub photo_asset_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSection {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub highlights: Vec<ProfileHighlight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfileDoc {
    pub nickname: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default, with = "opt_i64_str")]
    pub avatar_asset_id: Option<i64>,
    #[serde(default, with = "opt_i64_str")]
    pub banner_asset_id: Option<i64>,
    #[serde(default)]
    pub sections: Vec<ProfileSection>,
}

impl UserProfileDoc {
    pub fn default_for(nickname: &str) -> Self {
        Self {
            nickname: nickname.to_string(),
            signature: "慢慢生活，好好记录。".to_string(),
            avatar_asset_id: None,
            banner_asset_id: None,
            sections: vec![
                ProfileSection {
                    id: "sec_taste".to_string(),
                    title: "饮食与口味偏好".to_string(),
                    items: Vec::new(),
                    highlights: Vec::new(),
                },
                ProfileSection {
                    id: "sec_memo".to_string(),
                    title: "日常心愿与备忘".to_string(),
                    items: Vec::new(),
                    highlights: Vec::new(),
                },
            ],
        }
    }

    pub fn referenced_assets(&self) -> Vec<i64> {
        let mut ids = Vec::new();
        if let Some(a) = self.avatar_asset_id {
            ids.push(a);
        }
        if let Some(b) = self.banner_asset_id {
            ids.push(b);
        }
        for sec in &self.sections {
            for hl in &sec.highlights {
                if let Some(pid) = hl.photo_asset_id {
                    ids.push(pid);
                }
            }
        }
        ids
    }

    pub fn asset_counts_with_kind(&self) -> HashMap<(i64, i64), i64> {
        let mut map = HashMap::new();
        if let Some(a) = self.avatar_asset_id {
            *map.entry((a, dim::ASSET_AVATAR)).or_insert(0) += 1;
        }
        if let Some(b) = self.banner_asset_id {
            *map.entry((b, dim::ASSET_BANNER)).or_insert(0) += 1;
        }
        for sec in &self.sections {
            for hl in &sec.highlights {
                if let Some(pid) = hl.photo_asset_id {
                    *map.entry((pid, dim::ASSET_PHOTO)).or_insert(0) += 1;
                }
            }
        }
        map
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummaryDto {
    #[serde(with = "i64_str")]
    pub id: i64,
    pub username: String,
    pub doorplate_no: String,
    pub profile: UserProfileDoc,
}

#[derive(Serialize)]
pub struct FullProfileResp {
    pub user: UserSummaryDto,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn get_my_profile(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<FullProfileResp>> {
    build_profile_resp(&state, auth.user_id).await.map(Json)
}

async fn get_user_profile(
    auth: AuthUser,
    Path(raw_uid): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<FullProfileResp>> {
    let target_uid = raw_uid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的用户 ID".to_string()))?;

    if target_uid != auth.user_id {
        let now = now_ms();
        // Strict privacy: Only allow viewing profiles of co-space members, OR an inviter who sent YOU an active pending invite
        let shared = sqlx::query(
            "SELECT 1 FROM space_members m1
             JOIN space_members m2 ON m1.space_id = m2.space_id
             WHERE m1.user_id = ?1 AND m2.user_id = ?2
             UNION ALL
             SELECT 1 FROM space_invitations
             WHERE inviter_id = ?2 AND invitee_id = ?1 AND status_id = ?3 AND expires_at > ?4
             LIMIT 1",
        )
        .bind(auth.user_id)
        .bind(target_uid)
        .bind(dim::STATUS_PENDING)
        .bind(now)
        .fetch_optional(&state.db.read)
        .await?;

        if shared.is_none() {
            return Err(AppError::NotFound("用户不存在或尚未建立空间连接".to_string()));
        }
    }

    build_profile_resp(&state, target_uid).await.map(Json)
}

async fn update_my_profile(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(mut new_doc): Json<UserProfileDoc>,
) -> AppResult<Json<FullProfileResp>> {
    new_doc.nickname = new_doc.nickname.trim().to_string();
    if new_doc.nickname.is_empty() || new_doc.nickname.len() > 36 {
        return Err(AppError::BadRequest("昵称需在 1 ~ 36 个字符之间".to_string()));
    }

    let raw_json = serde_json::to_string(&new_doc)?;
    let now = now_ms();

    let mut tx = state.db.write.begin().await?;
    let u_row = sqlx::query("SELECT username, doorplate_no, profile_json FROM users WHERE id = ?1")
        .bind(auth.user_id)
        .fetch_one(&mut *tx)
        .await?;
    let username: String = u_row.get("username");
    let doorplate_no: String = u_row.get("doorplate_no");
    let old_profile = serde_json::from_str::<UserProfileDoc>(u_row.get("profile_json"))
        .unwrap_or_else(|_| UserProfileDoc::default_for(&username));

    let old_counts = old_profile.asset_counts_with_kind();
    let new_counts = new_doc.asset_counts_with_kind();

    let mut all_keys: HashSet<(i64, i64)> = old_counts.keys().copied().collect();
    all_keys.extend(new_counts.keys().copied());

    let mut to_release: Vec<(i64, i64)> = Vec::new();
    for key @ (aid, kind_id) in all_keys {
        let prev_c = old_counts.get(&key).copied().unwrap_or(0);
        let next_c = new_counts.get(&key).copied().unwrap_or(0);
        if next_c > prev_c {
            bind_owned_asset_in_tx(&mut tx, aid, auth.user_id, kind_id, next_c - prev_c).await?;
        } else if prev_c > next_c {
            to_release.push((aid, prev_c - next_c));
        }
    }

    sqlx::query("UPDATE users SET profile_json = ?1, updated_at = ?2 WHERE id = ?3")
        .bind(&raw_json)
        .bind(now)
        .bind(auth.user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    for (aid, dec) in to_release {
        let _ = release_asset_ref_by(&state, aid, dec).await;
    }

    let updated_summary = UserSummaryDto {
        id: auth.user_id,
        username,
        doorplate_no,
        profile: new_doc.clone(),
    };
    if let Ok(cached_str) = serde_json::to_string(&updated_summary) {
        state
            .cache
            .set_str(&format!("user:profile:{}", auth.user_id), &cached_str, 21600)
            .await;
    }

    let shared_rows = sqlx::query(
        "SELECT sm.space_id FROM space_members sm
         JOIN spaces s ON s.id = sm.space_id
         WHERE sm.user_id = ?1 AND s.space_type_id = ?2",
    )
    .bind(auth.user_id)
    .bind(dim::SPACE_SHARED)
    .fetch_all(&state.db.read)
    .await?;

    for row in shared_rows {
        let sid: i64 = row.get("space_id");
        let _ = record_timeline_event(
            &state,
            sid,
            auth.user_id,
            dim::EVT_PROFILE_UPDATED,
            serde_json::json!({
                "nickname": new_doc.nickname,
                "signature": new_doc.signature,
                "avatar_asset_id": new_doc.avatar_asset_id.map(|v| v.to_string()),
            }),
        )
        .await;
        state
            .hub
            .publish(
                sid,
                "SPACE_UPDATED",
                serde_json::json!({ "updated_user_id": auth.user_id.to_string() }),
            )
            .await;
    }

    build_profile_resp(&state, auth.user_id).await.map(Json)
}

async fn build_profile_resp(state: &AppState, user_id: i64) -> AppResult<FullProfileResp> {
    let user = fetch_user_summary(state, user_id).await?;
    let asset_ids = user.profile.referenced_assets();
    let asset_urls = resolve_asset_urls(state, &asset_ids).await?;
    Ok(FullProfileResp { user, asset_urls })
}

pub async fn fetch_user_summary(state: &AppState, user_id: i64) -> AppResult<UserSummaryDto> {
    let cache_key = format!("user:profile:{}", user_id);
    if let Some(raw) = state.cache.get_str(&cache_key).await {
        if let Ok(dto) = serde_json::from_str::<UserSummaryDto>(&raw) {
            return Ok(dto);
        }
    }

    let row = sqlx::query(
        "SELECT id, username, doorplate_no, profile_json FROM users WHERE id = ?1",
    )
    .bind(user_id)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;

    let raw_profile: String = row.get("profile_json");
    let username: String = row.get("username");
    let profile = serde_json::from_str::<UserProfileDoc>(&raw_profile)
        .unwrap_or_else(|_| UserProfileDoc::default_for(&username));

    let dto = UserSummaryDto {
        id: row.get("id"),
        username,
        doorplate_no: row.get("doorplate_no"),
        profile,
    };

    if let Ok(s) = serde_json::to_string(&dto) {
        state.cache.set_str(&cache_key, &s, 21600).await;
    }
    Ok(dto)
}

pub async fn load_users_map(
    state: &AppState,
    user_ids: &[i64],
) -> AppResult<(HashMap<String, UserSummaryDto>, Vec<i64>)> {
    let mut map = HashMap::new();
    let mut avatar_assets = Vec::new();
    let mut uniq = HashSet::new();

    for uid in user_ids {
        if !uniq.insert(*uid) {
            continue;
        }
        if let Ok(u) = fetch_user_summary(state, *uid).await {
            if let Some(aid) = u.profile.avatar_asset_id {
                avatar_assets.push(aid);
            }
            map.insert(uid.to_string(), u);
        }
    }

    Ok((map, avatar_assets))
}
