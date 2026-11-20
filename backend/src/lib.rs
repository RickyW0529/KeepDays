pub mod agent;
pub mod config;
pub mod domains;
pub mod error;
pub mod infra;
pub mod middleware;
pub mod realtime;
pub mod snowflake;
pub mod spa;
pub mod state;
pub mod time_bj;

use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Request},
    http::{header, HeaderValue, Method},
    middleware::Next,
    response::Response,
    Router,
};
use sqlx::Row;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tower_http::{
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};

use crate::{
    config::AppConfig,
    domains::pet::{engine::evaluate_pet_lazy_tick, model::PetStateDoc},
    infra::{
        cache::CacheService,
        db::DbPools,
        dict::{dim, DictRegistry},
        s3::{ext_from_mime, S3Service},
    },
    realtime::hub::RealtimeHub,
    snowflake::SnowflakeGenerator,
    spa::spa_fallback_handler,
    state::{AppState, InnerState},
    time_bj::now_ms,
};

pub async fn build_app_state(cfg: AppConfig) -> Result<AppState, Box<dyn std::error::Error>> {
    let db = DbPools::connect_and_migrate(&cfg.database_url, cfg.low_mem_mode).await?;
    let dict = DictRegistry::load_from_db(&db.read).await?;
    let cache = CacheService::connect(&cfg.redis_url, cfg.low_mem_mode).await;
    let s3 = S3Service::new(&cfg);
    let snowflake = SnowflakeGenerator::new(cfg.snowflake_worker_id);
    let hub = RealtimeHub::new();
    let agent = agent::AgentRuntime::new();
    let argon2_permits = if cfg.low_mem_mode { 1 } else { 4 };
    let argon2_sem = Semaphore::new(argon2_permits);
    let (pet_reward_tx, pet_reward_rx) = tokio::sync::mpsc::unbounded_channel();

    let state: AppState = Arc::new(InnerState {
        cfg,
        db,
        dict,
        cache,
        s3,
        snowflake,
        hub,
        agent,
        argon2_sem,
        pet_reward_tx,
    });

    domains::pet::engine::spawn_pet_life_listener(state.clone(), pet_reward_rx);
    spawn_maintenance_worker(state.clone());

    Ok(state)
}

pub fn create_router(state: AppState) -> Router {
    let is_loopback = state.cfg.host == "127.0.0.1"
        || state.cfg.host == "::1"
        || state.cfg.host == "localhost";

    let cors_origin = if state.cfg.cors_allowed_origin == "*" && is_loopback {
        AllowOrigin::mirror_request()
    } else {
        let origins: Vec<HeaderValue> = state
            .cfg
            .cors_allowed_origin
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "*")
            .filter_map(|s| s.parse::<HeaderValue>().ok())
            .collect();
        AllowOrigin::list(origins)
    };

    let cors = CorsLayer::new()
        .allow_origin(cors_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        .allow_credentials(true);

    let space_scoped_router = domains::space::member_router()
        .nest("/photos", domains::photo::space_router())
        .nest("/fridge", domains::fridge::space_router())
        .nest("/pet", domains::pet::space_router())
        .nest("/timeline", domains::timeline::space_router());

    let api_v1 = Router::new()
        .nest("/system", domains::system::router())
        .nest("/auth", domains::auth::router())
        .nest("/profile", domains::profile::router())
        .nest("/media", domains::media::router())
        .merge(domains::space::root_router())
        .nest("/spaces/:space_id", space_scoped_router)
        .layer(DefaultBodyLimit::max(1024 * 1024));

    let mut root = Router::new().nest("/api/v1", api_v1);
    if state.s3.is_local_s3() {
        root = root.nest("/local-s3", infra::local_s3::router());
    }

    root.fallback(spa_fallback_handler)
        .layer(axum::middleware::from_fn(inject_peer_ip_header))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}

async fn inject_peer_ip_header(mut req: Request, next: Next) -> Response {
    req.headers_mut().remove("x-internal-peer-ip");
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() {
        if let Ok(val) = HeaderValue::from_str(&addr.ip().to_string()) {
            req.headers_mut().insert("x-internal-peer-ip", val);
        }
    }
    next.run(req).await
}

fn spawn_maintenance_worker(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.tick().await;
        loop {
            ticker.tick().await;

            // 1. Reclaim RAM, expired refresh sessions & SSD WAL space proactively
            state.cache.evict_expired_memory().await;
            state.hub.prune_idle_rooms().await;
            let now = now_ms();
            let _ = sqlx::query("DELETE FROM refresh_sessions WHERE expires_at < ?1")
                .bind(now)
                .execute(&state.db.write)
                .await;
            state.db.passive_wal_checkpoint().await;
            agent::tick_agent_maintenance(&state).await;

            // 2. Settle returning adventures atomically via serialized writer
            if let Ok(rows) = sqlx::query(
                "SELECT space_id FROM space_modules
                 WHERE module_type_id = ?1 AND data_json LIKE '%ADVENTURING%'",
            )
            .bind(dim::MOD_PET)
            .fetch_all(&state.db.read)
            .await
            {
                for r in rows {
                    let sid: i64 = r.get("space_id");
                    if let Ok(mut tx) = state.db.write.begin().await {
                        if let Ok(Some(w_row)) = sqlx::query(
                            "SELECT data_json FROM space_modules
                             WHERE space_id = ?1 AND module_type_id = ?2",
                        )
                        .bind(sid)
                        .bind(dim::MOD_PET)
                        .fetch_optional(&mut *tx)
                        .await
                        {
                            if let Ok(mut pet) =
                                serde_json::from_str::<PetStateDoc>(w_row.get("data_json"))
                            {
                                if evaluate_pet_lazy_tick(&state, sid, &mut pet).await {
                                    if let Ok(raw) = serde_json::to_string(&pet) {
                                        let _ = sqlx::query(
                                            "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
                                             WHERE space_id = ?3 AND module_type_id = ?4",
                                        )
                                        .bind(raw)
                                        .bind(now_ms())
                                        .bind(sid)
                                        .bind(dim::MOD_PET)
                                        .execute(&mut *tx)
                                        .await;
                                        let _ = tx.commit().await;

                                        state
                                            .hub
                                            .publish(
                                                sid,
                                                "PET_UPDATED",
                                                serde_json::to_value(&pet).unwrap_or_default(),
                                            )
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 3. Orphan S3 Asset GC (`ref_count == 0` older than 2 hours & unreferenced)
            let cutoff = now_ms() - 2 * 3600 * 1000;
            if let Ok(stale) = sqlx::query(
                "DELETE FROM media_assets
                 WHERE ref_count = 0 AND created_at < ?1
                   AND NOT EXISTS (SELECT 1 FROM photos WHERE asset_id = media_assets.id)
                 RETURNING id, kind_id, mime_type, has_thumb",
            )
            .bind(cutoff)
            .fetch_all(&state.db.write)
            .await
            {
                for r in stale {
                    let aid: i64 = r.get("id");
                    let kid: i64 = r.get("kind_id");
                    let mime: String = r.get("mime_type");
                    let has_thumb = r.get::<i64, _>("has_thumb") != 0;
                    if let Ok((_, rule)) = state.dict.get_asset_rule(kid).await {
                        let key = S3Service::object_key(&rule.s3_dir, aid, ext_from_mime(&mime));
                        state.s3.delete_object(&key).await;
                        if has_thumb {
                            state
                                .s3
                                .delete_object(&S3Service::thumb_key(&rule.s3_dir, aid))
                                .await;
                        }
                    }
                }
            }
        }
    });
}
