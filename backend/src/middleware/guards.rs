use axum::{
    extract::{FromRequestParts, Path, Query},
    http::{header::AUTHORIZATION, request::Parts},
};
use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;

use crate::{
    error::AppError,
    infra::dict::dim,
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Snowflake user_id as string
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
}

/// Authenticated User Extractor (verifies both JWT HMAC and live account existence)
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: i64,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let bearer_token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.trim().to_string());

        let token = match bearer_token {
            Some(t) if !t.is_empty() => t,
            _ => {
                // Fallback to ?token= query parameter for browser native EventSource (SSE)
                let Query(q) = Query::<TokenQuery>::from_request_parts(parts, state)
                    .await
                    .unwrap_or(Query(TokenQuery { token: None }));
                q.token.ok_or_else(|| {
                    AppError::Unauthorized("缺少登录凭证，请先登录".to_string())
                })?
            }
        };

        let token_data = decode::<JwtClaims>(
            &token,
            &DecodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
            &Validation::default(),
        )
        .map_err(|_| AppError::Unauthorized("登录凭证已过期或无效".to_string()))?;

        let user_id = token_data
            .claims
            .sub
            .parse::<i64>()
            .map_err(|_| AppError::Unauthorized("无效的用户标识".to_string()))?;

        // Prevent zombie JWTs (e.g. after DB reset/account removal) from triggering 500 FK errors!
        let exists_key = format!("user:ok:{}", user_id);
        if state.cache.get_str(&exists_key).await.is_none()
            && state
                .cache
                .get_str(&format!("user:profile:{}", user_id))
                .await
                .is_none()
        {
            let exists = sqlx::query("SELECT 1 FROM users WHERE id = ?1")
                .bind(user_id)
                .fetch_optional(&state.db.read)
                .await?;
            if exists.is_none() {
                return Err(AppError::Unauthorized(
                    "账户会话已失效，请重新登录".to_string(),
                ));
            }
            state.cache.set_str(&exists_key, "1", 3600).await;
        }

        Ok(AuthUser { user_id })
    }
}

/// Compile-Time Multi-Tenant Isolation Guard (`SpaceGuard`)
#[derive(Debug, Clone)]
pub struct SpaceGuard {
    pub user_id: i64,
    pub space_id: i64,
    pub space_type_id: i64,
    pub role_id: i64,
}

impl SpaceGuard {
    pub fn is_shared(&self) -> bool {
        self.space_type_id == dim::SPACE_SHARED
    }

    pub fn is_owner(&self) -> bool {
        self.role_id == dim::ROLE_OWNER
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for SpaceGuard {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;

        let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::BadRequest("无效的空间路径参数".to_string()))?;

        let raw_space_id = params
            .get("space_id")
            .ok_or_else(|| AppError::BadRequest("缺少 space_id 参数".to_string()))?;

        let space_id = raw_space_id
            .parse::<i64>()
            .map_err(|_| AppError::NotFound("空间不存在".to_string()))?;

        if let Some((space_type_id, role_id)) =
            state.cache.get_space_access(auth.user_id, space_id).await
        {
            return Ok(SpaceGuard {
                user_id: auth.user_id,
                space_id,
                space_type_id,
                role_id,
            });
        }

        let row = sqlx::query(
            "SELECT s.space_type_id, sm.role_id
             FROM space_members sm
             JOIN spaces s ON s.id = sm.space_id
             WHERE sm.space_id = ?1 AND sm.user_id = ?2",
        )
        .bind(space_id)
        .bind(auth.user_id)
        .fetch_optional(&state.db.read)
        .await?;

        let Some(r) = row else {
            return Err(AppError::NotFound("空间不存在或无访问权限".to_string()));
        };

        let space_type_id: i64 = r.get("space_type_id");
        let role_id: i64 = r.get("role_id");

        state
            .cache
            .set_space_access(auth.user_id, space_id, space_type_id, role_id)
            .await;

        Ok(SpaceGuard {
            user_id: auth.user_id,
            space_id,
            space_type_id,
            role_id,
        })
    }
}
