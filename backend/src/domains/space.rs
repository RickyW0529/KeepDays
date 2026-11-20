use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{sse::Sse, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;

use crate::{
    domains::{
        auth::init_base_space_modules,
        media::resolve_asset_urls,
        profile::{load_users_map, UserSummaryDto},
        timeline::record_timeline_event,
    },
    error::{AppError, AppResult},
    infra::{
        cache::{CachedAssetUrls, PresenceFocus},
        dict::dim,
    },
    middleware::{
        guards::{AuthUser, SpaceGuard},
        rate_limit::enforce_rate_limit,
    },
    snowflake::{i64_str, opt_i64_str},
    state::AppState,
    time_bj::now_ms,
};

pub fn root_router() -> Router<AppState> {
    Router::new()
        .route("/spaces", get(list_my_spaces).post(create_shared_space))
        .route("/invitations/pending", get(list_pending_invitations))
        .route("/invitations/:invite_code/respond", post(respond_invitation))
}

pub fn member_router() -> Router<AppState> {
    Router::new()
        .route("/invitations", post(create_invitation))
        .route("/presence", get(get_presence).put(update_presence))
        .route("/events", get(space_sse_stream))
}

#[derive(Serialize)]
pub struct SpaceMemberDto {
    #[serde(with = "i64_str")]
    pub user_id: i64,
    pub role_id: i64,
    pub alias_name: Option<String>,
    pub joined_at: i64,
}

#[derive(Serialize)]
pub struct SpaceDto {
    #[serde(with = "i64_str")]
    pub id: i64,
    pub name: String,
    pub space_type_id: i64,
    pub is_shared: bool,
    #[serde(with = "i64_str")]
    pub owner_id: i64,
    pub created_at: i64,
    pub members: Vec<SpaceMemberDto>,
}

#[derive(Serialize)]
pub struct SpaceListResp {
    pub spaces: Vec<SpaceDto>,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn list_my_spaces(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<SpaceListResp>> {
    let space_rows = sqlx::query(
        "SELECT s.id, s.name, s.space_type_id, s.owner_id, s.created_at
         FROM spaces s
         JOIN space_members sm ON sm.space_id = s.id
         WHERE sm.user_id = ?1
         ORDER BY s.space_type_id ASC, s.created_at ASC",
    )
    .bind(auth.user_id)
    .fetch_all(&state.db.read)
    .await?;

    let mut spaces = Vec::with_capacity(space_rows.len());
    let mut all_uids = Vec::new();

    for r in space_rows {
        let sid: i64 = r.get("id");
        let st_id: i64 = r.get("space_type_id");

        let mem_rows = sqlx::query(
            "SELECT user_id, role_id, alias_name, joined_at
             FROM space_members WHERE space_id = ?1 ORDER BY joined_at ASC",
        )
        .bind(sid)
        .fetch_all(&state.db.read)
        .await?;

        let mut members = Vec::with_capacity(mem_rows.len());
        for mr in mem_rows {
            let uid: i64 = mr.get("user_id");
            all_uids.push(uid);
            members.push(SpaceMemberDto {
                user_id: uid,
                role_id: mr.get("role_id"),
                alias_name: mr.get("alias_name"),
                joined_at: mr.get("joined_at"),
            });
        }

        spaces.push(SpaceDto {
            id: sid,
            name: r.get("name"),
            space_type_id: st_id,
            is_shared: st_id == dim::SPACE_SHARED,
            owner_id: r.get("owner_id"),
            created_at: r.get("created_at"),
            members,
        });
    }

    let (users, avatar_assets) = load_users_map(&state, &all_uids).await?;
    let asset_urls = resolve_asset_urls(&state, &avatar_assets).await?;

    Ok(Json(SpaceListResp {
        spaces,
        users,
        asset_urls,
    }))
}

#[derive(Deserialize)]
pub struct CreateSpaceReq {
    pub name: String,
    pub alias_name: Option<String>,
}

async fn create_shared_space(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateSpaceReq>,
) -> AppResult<Json<SpaceDto>> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 48 {
        return Err(AppError::BadRequest(
            "空间名称需在 1 ~ 48 个字符之间".to_string(),
        ));
    }

    let space_id = state.snowflake.next_id();
    let now = now_ms();

    let mut tx = state.db.write.begin().await?;

    sqlx::query(
        "INSERT INTO spaces (id, name, space_type_id, owner_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(space_id)
    .bind(name)
    .bind(dim::SPACE_SHARED)
    .bind(auth.user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO space_members (space_id, user_id, role_id, alias_name, joined_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(space_id)
    .bind(auth.user_id)
    .bind(dim::ROLE_OWNER)
    .bind(req.alias_name.as_deref())
    .bind(now)
    .execute(&mut *tx)
    .await?;

    init_base_space_modules(&mut tx, space_id, true, now).await?;
    tx.commit().await?;

    state
        .cache
        .set_space_access(auth.user_id, space_id, dim::SPACE_SHARED, dim::ROLE_OWNER)
        .await;

    Ok(Json(SpaceDto {
        id: space_id,
        name: name.to_string(),
        space_type_id: dim::SPACE_SHARED,
        is_shared: true,
        owner_id: auth.user_id,
        created_at: now,
        members: vec![SpaceMemberDto {
            user_id: auth.user_id,
            role_id: dim::ROLE_OWNER,
            alias_name: req.alias_name,
            joined_at: now,
        }],
    }))
}

#[derive(Deserialize)]
pub struct CreateInvitationReq {
    pub target_doorplate_no: Option<String>,
    pub ttl_minutes: Option<u64>,
}

#[derive(Serialize)]
pub struct InvitationDto {
    #[serde(with = "i64_str")]
    pub id: i64,
    #[serde(with = "i64_str")]
    pub space_id: i64,
    pub space_name: String,
    #[serde(with = "i64_str")]
    pub inviter_id: i64,
    #[serde(with = "opt_i64_str")]
    pub invitee_id: Option<i64>,
    pub invite_code: String,
    pub expires_at: i64,
    pub created_at: i64,
}

async fn create_invitation(
    guard: SpaceGuard,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateInvitationReq>,
) -> AppResult<Json<InvitationDto>> {
    enforce_rate_limit(&state, &headers, "space_invite", 15, 60).await?;

    if !guard.is_shared() {
        return Err(AppError::BadRequest(
            "个人独处空间为严格私密隔离区域，不支持邀请他人；请先新建共享空间".to_string(),
        ));
    }

    let now = now_ms();
    let space_row = sqlx::query("SELECT name FROM spaces WHERE id = ?1")
        .bind(guard.space_id)
        .fetch_one(&state.db.read)
        .await?;
    let space_name: String = space_row.get("name");

    let mut target_uid: Option<i64> = None;
    if let Some(raw_dp) = &req.target_doorplate_no {
        let clean_dp = raw_dp.trim().trim_start_matches('#');
        if !clean_dp.is_empty() {
            let row = sqlx::query("SELECT id FROM users WHERE doorplate_no = ?1")
                .bind(clean_dp)
                .fetch_optional(&state.db.read)
                .await?
                .ok_or_else(|| AppError::NotFound("未找到该门牌号对应的用户".to_string()))?;
            let uid: i64 = row.get("id");
            if uid == guard.user_id {
                return Err(AppError::BadRequest("不能邀请自己".to_string()));
            }
            let already = sqlx::query(
                "SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2",
            )
            .bind(guard.space_id)
            .bind(uid)
            .fetch_optional(&state.db.read)
            .await?;
            if already.is_some() {
                return Err(AppError::Conflict("对方已经是该共享空间的成员啦".to_string()));
            }

            // Deduplicate active pending targeted invitation for `(space_id, invitee_id)`
            if let Some(existing_inv) = sqlx::query(
                "SELECT id, inviter_id, invite_code, expires_at, created_at
                 FROM space_invitations
                 WHERE space_id = ?1 AND invitee_id = ?2 AND status_id = ?3 AND expires_at > ?4
                 LIMIT 1",
            )
            .bind(guard.space_id)
            .bind(uid)
            .bind(dim::STATUS_PENDING)
            .bind(now)
            .fetch_optional(&state.db.read)
            .await?
            {
                return Ok(Json(InvitationDto {
                    id: existing_inv.get("id"),
                    space_id: guard.space_id,
                    space_name,
                    inviter_id: existing_inv.get("inviter_id"),
                    invitee_id: Some(uid),
                    invite_code: existing_inv.get("invite_code"),
                    expires_at: existing_inv.get("expires_at"),
                    created_at: existing_inv.get("created_at"),
                }));
            }

            target_uid = Some(uid);
        }
    }

    let invite_id = state.snowflake.next_id();
    let invite_code: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(10)
        .map(char::from)
        .collect::<String>()
        .to_uppercase();

    let ttl_ms = (req.ttl_minutes.unwrap_or(1440).clamp(5, 10080) * 60 * 1000) as i64;
    let expires_at = now + ttl_ms;

    sqlx::query(
        "INSERT INTO space_invitations (id, space_id, inviter_id, invitee_id, invite_code, status_id, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(invite_id)
    .bind(guard.space_id)
    .bind(guard.user_id)
    .bind(target_uid)
    .bind(&invite_code)
    .bind(dim::STATUS_PENDING)
    .bind(expires_at)
    .bind(now)
    .execute(&state.db.write)
    .await?;

    Ok(Json(InvitationDto {
        id: invite_id,
        space_id: guard.space_id,
        space_name,
        inviter_id: guard.user_id,
        invitee_id: target_uid,
        invite_code,
        expires_at,
        created_at: now,
    }))
}

#[derive(Serialize)]
pub struct PendingInvitationsResp {
    pub invitations: Vec<InvitationDto>,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn list_pending_invitations(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<PendingInvitationsResp>> {
    let now = now_ms();
    let rows = sqlx::query(
        "SELECT i.id, i.space_id, s.name AS space_name, i.inviter_id, i.invitee_id, i.invite_code, i.expires_at, i.created_at
         FROM space_invitations i
         JOIN spaces s ON s.id = i.space_id
         WHERE i.invitee_id = ?1 AND i.status_id = ?2 AND i.expires_at > ?3
         ORDER BY i.created_at DESC",
    )
    .bind(auth.user_id)
    .bind(dim::STATUS_PENDING)
    .bind(now)
    .fetch_all(&state.db.read)
    .await?;

    let mut invitations = Vec::with_capacity(rows.len());
    let mut inviter_uids = Vec::with_capacity(rows.len());

    for r in rows {
        let inv_uid: i64 = r.get("inviter_id");
        inviter_uids.push(inv_uid);
        invitations.push(InvitationDto {
            id: r.get("id"),
            space_id: r.get("space_id"),
            space_name: r.get("space_name"),
            inviter_id: inv_uid,
            invitee_id: r.get("invitee_id"),
            invite_code: r.get("invite_code"),
            expires_at: r.get("expires_at"),
            created_at: r.get("created_at"),
        });
    }

    let (users, avatar_assets) = load_users_map(&state, &inviter_uids).await?;
    let asset_urls = resolve_asset_urls(&state, &avatar_assets).await?;

    Ok(Json(PendingInvitationsResp {
        invitations,
        users,
        asset_urls,
    }))
}

#[derive(Deserialize)]
pub struct RespondInviteReq {
    pub accept: bool,
    pub alias_name: Option<String>,
}

/// Atomic Mutual Invite -> Consent Handshake inside serialized write transaction
async fn respond_invitation(
    auth: AuthUser,
    Path(invite_code): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RespondInviteReq>,
) -> AppResult<Json<serde_json::Value>> {
    enforce_rate_limit(&state, &headers, "invite_respond", 10, 60).await?;

    let now = now_ms();
    let code = invite_code.trim().to_uppercase();

    let mut tx = state.db.write.begin().await?;

    let row = sqlx::query(
        "SELECT id, space_id, inviter_id, invitee_id, status_id, expires_at
         FROM space_invitations WHERE invite_code = ?1",
    )
    .bind(&code)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("邀请口令无效或不存在".to_string()))?;

    let inv_id: i64 = row.get("id");
    let space_id: i64 = row.get("space_id");
    let inviter_id: i64 = row.get("inviter_id");
    let invitee_id: Option<i64> = row.get("invitee_id");
    let status_id: i64 = row.get("status_id");
    let expires_at: i64 = row.get("expires_at");

    if status_id != dim::STATUS_PENDING {
        return Err(AppError::Conflict("该邀请已被处理或已失效".to_string()));
    }
    if now > expires_at {
        let _ = sqlx::query("UPDATE space_invitations SET status_id = ?1 WHERE id = ?2")
            .bind(dim::STATUS_EXPIRED)
            .bind(inv_id)
            .execute(&mut *tx)
            .await;
        tx.commit().await?;
        return Err(AppError::BadRequest("该邀请函已过期".to_string()));
    }
    if let Some(expected_uid) = invitee_id {
        if expected_uid != auth.user_id {
            return Err(AppError::Forbidden("该定向邀请仅限受邀本人确认".to_string()));
        }
    }
    if inviter_id == auth.user_id {
        return Err(AppError::BadRequest("无需接受自己发出的邀请".to_string()));
    }

    // Check if user is already a member (prevents owner role cache downgrade & duplicate events!)
    let existing_mem = sqlx::query(
        "SELECT role_id FROM space_members WHERE space_id = ?1 AND user_id = ?2",
    )
    .bind(space_id)
    .bind(auth.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if existing_mem.is_some() {
        return Err(AppError::Conflict("你已经是该共享空间的成员啦".to_string()));
    }

    if !req.accept {
        let upd = sqlx::query(
            "UPDATE space_invitations
             SET status_id = ?1, invitee_id = ?2
             WHERE id = ?3 AND status_id = ?4",
        )
        .bind(dim::STATUS_REJECTED)
        .bind(auth.user_id)
        .bind(inv_id)
        .bind(dim::STATUS_PENDING)
        .execute(&mut *tx)
        .await?;
        if upd.rows_affected() == 0 {
            return Err(AppError::Conflict("该邀请已被处理".to_string()));
        }
        tx.commit().await?;

        return Ok(Json(serde_json::json!({
            "accepted": false,
            "space_id": space_id.to_string(),
        })));
    }

    let upd = sqlx::query(
        "UPDATE space_invitations
         SET status_id = ?1, invitee_id = ?2
         WHERE id = ?3 AND status_id = ?4",
    )
    .bind(dim::STATUS_READY)
    .bind(auth.user_id)
    .bind(inv_id)
    .bind(dim::STATUS_PENDING)
    .execute(&mut *tx)
    .await?;
    if upd.rows_affected() == 0 {
        return Err(AppError::Conflict("该邀请已被处理".to_string()));
    }

    sqlx::query(
        "INSERT INTO space_members (space_id, user_id, role_id, alias_name, joined_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(space_id)
    .bind(auth.user_id)
    .bind(dim::ROLE_MEMBER)
    .bind(req.alias_name.as_deref())
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    state
        .cache
        .set_space_access(auth.user_id, space_id, dim::SPACE_SHARED, dim::ROLE_MEMBER)
        .await;

    let _ = record_timeline_event(
        &state,
        space_id,
        auth.user_id,
        dim::EVT_MEMBER_JOINED,
        serde_json::json!({ "joined_at": now }),
    )
    .await;

    state
        .hub
        .publish(
            space_id,
            "SPACE_UPDATED",
            serde_json::json!({ "joined_user_id": auth.user_id.to_string() }),
        )
        .await;

    Ok(Json(serde_json::json!({
        "accepted": true,
        "space_id": space_id.to_string(),
    })))
}

#[derive(Deserialize)]
pub struct UpdatePresenceReq {
    pub focus: String,
    #[serde(default, with = "opt_i64_str")]
    pub photo_id: Option<i64>,
}

#[derive(Serialize)]
pub struct SpacePresenceResp {
    pub active_members: Vec<PresenceFocus>,
    pub partner_in_fridge: bool,
    pub viewing_photos: HashMap<String, Vec<String>>,
}

async fn update_presence(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<UpdatePresenceReq>,
) -> AppResult<Json<SpacePresenceResp>> {
    let now = now_ms();
    let focus_item = PresenceFocus {
        user_id: guard.user_id,
        focus: req.focus.to_uppercase(),
        photo_id: req.photo_id,
        updated_at_ms: now,
    };

    state
        .cache
        .update_presence(guard.space_id, focus_item)
        .await;

    let all = state.cache.list_presence(guard.space_id, now).await;
    state
        .hub
        .publish(
            guard.space_id,
            "PRESENCE_SYNC",
            serde_json::to_value(&all).unwrap_or_default(),
        )
        .await;

    Ok(Json(build_presence_resp(&all, guard.user_id)))
}

async fn get_presence(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<SpacePresenceResp>> {
    let all = state.cache.list_presence(guard.space_id, now_ms()).await;
    Ok(Json(build_presence_resp(&all, guard.user_id)))
}

fn build_presence_resp(all: &[PresenceFocus], self_uid: i64) -> SpacePresenceResp {
    let mut partner_in_fridge = false;
    let mut viewing_photos: HashMap<String, Vec<String>> = HashMap::new();

    for p in all {
        if p.user_id != self_uid {
            if p.focus == "FRIDGE" {
                partner_in_fridge = true;
            }
            if p.focus == "PHOTO" {
                if let Some(pid) = p.photo_id {
                    viewing_photos
                        .entry(pid.to_string())
                        .or_default()
                        .push(p.user_id.to_string());
                }
            }
        }
    }

    SpacePresenceResp {
        active_members: all.to_vec(),
        partner_in_fridge,
        viewing_photos,
    }
}

async fn space_sse_stream(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let presence = state.cache.list_presence(guard.space_id, now_ms()).await;
    let init = serde_json::json!({
        "presence": build_presence_resp(&presence, guard.user_id)
    });
    let sse: Sse<_> = state.hub.into_sse_stream(guard.space_id, init).await;
    sse
}
