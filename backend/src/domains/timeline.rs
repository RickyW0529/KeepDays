use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;

use crate::{
    domains::{
        media::resolve_asset_urls,
        profile::{load_users_map, UserSummaryDto},
    },
    error::AppResult,
    infra::cache::CachedAssetUrls,
    middleware::guards::SpaceGuard,
    snowflake::i64_str,
    state::AppState,
    time_bj::now_ms,
};

pub fn space_router() -> Router<AppState> {
    Router::new().route("/", get(get_space_timeline))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineItemDto {
    #[serde(with = "i64_str")]
    pub id: i64,
    #[serde(with = "i64_str")]
    pub space_id: i64,
    #[serde(with = "i64_str")]
    pub actor_id: i64,
    pub event_type_id: i64,
    pub event_code: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

#[derive(Deserialize)]
pub struct TimelineCursorQuery {
    pub before_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct TimelinePageResp {
    pub items: Vec<TimelineItemDto>,
    pub next_cursor: Option<String>,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

/// Twitter-style Fan-out-on-Write Dispatcher:
/// 1. Persists durable row in `timeline_events` (Snowflake ID = monotonic cursor)
/// 2. Pre-assembles JSON card & pushes into Redis/Memory hot ZSet (`timeline:space:{id}`)
/// 3. Broadcasts `TIMELINE_NEW` over SSE so online partners pre-decode offscreen with Zero Jank!
pub async fn record_timeline_event(
    state: &AppState,
    space_id: i64,
    actor_id: i64,
    event_type_id: i64,
    payload: serde_json::Value,
) -> AppResult<TimelineItemDto> {
    let event_id = state.snowflake.next_id();
    let now = now_ms();
    let event_code = state
        .dict
        .get_item(event_type_id)
        .await
        .map(|d| d.code)
        .unwrap_or_else(|| "EVENT".to_string());

    let raw_payload = serde_json::to_string(&payload)?;

    sqlx::query(
        "INSERT INTO timeline_events (id, space_id, actor_id, event_type_id, payload_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(event_id)
    .bind(space_id)
    .bind(actor_id)
    .bind(event_type_id)
    .bind(&raw_payload)
    .bind(now)
    .execute(&state.db.write)
    .await?;

    let item = TimelineItemDto {
        id: event_id,
        space_id,
        actor_id,
        event_type_id,
        event_code,
        payload,
        created_at: now,
    };

    if let Ok(card_json) = serde_json::to_string(&item) {
        state
            .cache
            .push_timeline_item(space_id, event_id, &card_json)
            .await;
    }

    state
        .hub
        .publish(
            space_id,
            "TIMELINE_NEW",
            serde_json::to_value(&item).unwrap_or_default(),
        )
        .await;

    Ok(item)
}

async fn get_space_timeline(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Query(q): Query<TimelineCursorQuery>,
) -> AppResult<Json<TimelinePageResp>> {
    let limit = q.limit.unwrap_or(24).clamp(1, 100);
    let before_id = q
        .before_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());

    // 1. Try 0.2ms hot Redis/Memory timeline slice first
    let items: Vec<TimelineItemDto> = if let Some(raw_list) = state
        .cache
        .get_hot_timeline(guard.space_id, before_id, limit)
        .await
    {
        raw_list
            .into_iter()
            .filter_map(|s| serde_json::from_str::<TimelineItemDto>(&s).ok())
            .collect()
    } else {
        // 2. Seamless SQLite Snowflake Keyset Cursor fallback
        let rows = match before_id {
            Some(cid) => {
                sqlx::query(
                    "SELECT id, space_id, actor_id, event_type_id, payload_json, created_at
                     FROM timeline_events
                     WHERE space_id = ?1 AND id < ?2
                     ORDER BY id DESC LIMIT ?3",
                )
                .bind(guard.space_id)
                .bind(cid)
                .bind(limit as i64)
                .fetch_all(&state.db.read)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, space_id, actor_id, event_type_id, payload_json, created_at
                     FROM timeline_events
                     WHERE space_id = ?1
                     ORDER BY id DESC LIMIT ?2",
                )
                .bind(guard.space_id)
                .bind(limit as i64)
                .fetch_all(&state.db.read)
                .await?
            }
        };

        let mut list = Vec::with_capacity(rows.len());
        for r in rows {
            let et_id: i64 = r.get("event_type_id");
            let code = state
                .dict
                .get_item(et_id)
                .await
                .map(|d| d.code)
                .unwrap_or_else(|| "EVENT".to_string());
            let p_str: String = r.get("payload_json");
            let payload = serde_json::from_str(&p_str).unwrap_or_default();
            list.push(TimelineItemDto {
                id: r.get("id"),
                space_id: r.get("space_id"),
                actor_id: r.get("actor_id"),
                event_type_id: et_id,
                event_code: code,
                payload,
                created_at: r.get("created_at"),
            });
        }
        list
    };

    let next_cursor = if items.len() == limit {
        items.last().map(|last| last.id.to_string())
    } else {
        None
    };

    // Collect referenced actor_ids & asset_ids inside timeline payloads for one-pass hydration
    let mut uids = Vec::with_capacity(items.len());
    let mut asset_ids = Vec::new();
    for item in &items {
        uids.push(item.actor_id);
        extract_asset_ids_from_value(&item.payload, &mut asset_ids);
    }

    let (users, mut avatar_assets) = load_users_map(&state, &uids).await?;
    asset_ids.append(&mut avatar_assets);
    let asset_urls = resolve_asset_urls(&state, &asset_ids).await?;

    Ok(Json(TimelinePageResp {
        items,
        next_cursor,
        users,
        asset_urls,
    }))
}

fn extract_asset_ids_from_value(v: &serde_json::Value, out: &mut Vec<i64>) {
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if k.ends_with("asset_id") {
                if let Some(s) = val.as_str().and_then(|s| s.parse::<i64>().ok()) {
                    out.push(s);
                } else if let Some(n) = val.as_i64() {
                    out.push(n);
                }
            } else {
                extract_asset_ids_from_value(val, out);
            }
        }
    } else if let Some(arr) = v.as_array() {
        for el in arr {
            extract_asset_ids_from_value(el, out);
        }
    }
}
