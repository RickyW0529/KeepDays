use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    infra::dict::{DimDomain, DimItem},
    middleware::guards::AuthUser,
    state::AppState,
    time_bj::{current_ambient_status, AmbientStatus},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .route("/ambient", get(get_ambient))
        .route("/dict", get(get_dictionary))
        .route("/dict/items", post(upsert_dict_item))
}

#[derive(Serialize)]
struct HealthResp {
    service: &'static str,
    status: &'static str,
    ambient: AmbientStatus,
}

async fn health_check() -> Json<HealthResp> {
    Json(HealthResp {
        service: "KeepDays-Rust-Backend",
        status: "ok",
        ambient: current_ambient_status(),
    })
}

async fn get_ambient() -> Json<AmbientStatus> {
    Json(current_ambient_status())
}

#[derive(Serialize)]
struct DictResp {
    domains: Vec<DimDomain>,
    items: Vec<DimItem>,
}

async fn get_dictionary(State(state): State<AppState>) -> Json<DictResp> {
    let (domains, items) = state.dict.export_all().await;
    Json(DictResp { domains, items })
}

#[derive(Deserialize)]
struct UpsertDictItemReq {
    id: i64,
    domain_id: i64,
    code: String,
    label: String,
    #[serde(default)]
    sort_order: i64,
    #[serde(default = "default_true")]
    is_active: bool,
    #[serde(default = "empty_obj")]
    config: serde_json::Value,
}

fn default_true() -> bool {
    true
}

fn empty_obj() -> serde_json::Value {
    serde_json::json!({})
}

/// Protected operator endpoint: requires `X-Admin-Key` matching `ADMIN_API_KEY`
async fn upsert_dict_item(
    _auth: AuthUser,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(req): Json<UpsertDictItemReq>,
) -> AppResult<Json<DimItem>> {
    let expected_admin = state
        .cfg
        .admin_api_key
        .as_deref()
        .ok_or_else(|| AppError::Forbidden("系统字典写保护已开启".to_string()))?;

    let provided = headers
        .get("x-admin-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided != expected_admin {
        return Err(AppError::Forbidden("缺少有效的管理员密钥".to_string()));
    }

    if req.code.trim().is_empty() || req.label.trim().is_empty() {
        return Err(AppError::BadRequest("字典编码和名称不能为空".to_string()));
    }

    let domain_exists = sqlx::query("SELECT 1 FROM dim_domains WHERE id = ?1")
        .bind(req.domain_id)
        .fetch_optional(&state.db.read)
        .await?;
    if domain_exists.is_none() {
        return Err(AppError::BadRequest("无效的维度域 domain_id".to_string()));
    }

    let cfg_str = serde_json::to_string(&req.config)?;

    sqlx::query(
        "INSERT INTO dim_items (id, domain_id, code, label, sort_order, is_active, config_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            code = excluded.code,
            label = excluded.label,
            sort_order = excluded.sort_order,
            is_active = excluded.is_active,
            config_json = excluded.config_json",
    )
    .bind(req.id)
    .bind(req.domain_id)
    .bind(req.code.trim())
    .bind(req.label.trim())
    .bind(req.sort_order)
    .bind(if req.is_active { 1_i64 } else { 0_i64 })
    .bind(cfg_str)
    .execute(&state.db.write)
    .await?;

    state.dict.reload(&state.db.write).await?;

    let item = state
        .dict
        .get_item(req.id)
        .await
        .ok_or_else(|| AppError::Internal("字典刷新失败".to_string()))?;

    Ok(Json(item))
}
