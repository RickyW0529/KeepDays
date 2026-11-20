use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use axum::{
    extract::State,
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jsonwebtoken::{encode, EncodingKey, Header};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};

use crate::{
    domains::profile::{fetch_user_summary, UserProfileDoc, UserSummaryDto},
    error::{AppError, AppResult},
    infra::dict::dim,
    middleware::{guards::JwtClaims, rate_limit::enforce_rate_limit},
    snowflake::i64_str,
    state::AppState,
    time_bj::now_ms,
};

const REFRESH_COOKIE_NAME: &str = "kd_refresh_token";
const REFRESH_COOKIE_PATH: &str = "/api/v1/auth";
const ROTATION_GRACE_MS: i64 = 10_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/refresh", post(refresh_session))
        .route("/logout", post(logout))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub username: String,
    pub password: String,
    pub nickname: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct AuthSessionResp {
    pub access_token: String,
    pub expires_in_secs: u64,
    pub user: UserSummaryDto,
    #[serde(with = "i64_str")]
    pub default_personal_space_id: i64,
}

fn hash_refresh_token(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

fn is_https_request(headers: &HeaderMap, state: &AppState) -> bool {
    let fwd_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    fwd_https || state.cfg.cors_allowed_origin.starts_with("https://")
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<RegisterReq>,
) -> AppResult<(CookieJar, Json<AuthSessionResp>)> {
    enforce_rate_limit(&state, &headers, "auth_register", 5, 60).await?;

    let username = req.username.trim().to_lowercase();
    if username.len() < 3 || username.len() > 64 {
        return Err(AppError::BadRequest(
            "账号长度需在 3 ~ 64 个字符之间".to_string(),
        ));
    }
    if req.password.len() < 8 || req.password.len() > 128 {
        return Err(AppError::BadRequest(
            "密码长度需在 8 ~ 128 个字符之间".to_string(),
        ));
    }

    let existing = sqlx::query("SELECT 1 FROM users WHERE username = ?1")
        .bind(&username)
        .fetch_optional(&state.db.read)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("该账号已被注册".to_string()));
    }

    let _permit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.argon2_sem.acquire(),
    )
    .await
    .map_err(|_| AppError::TooManyRequests("服务器繁忙，请稍后重试".to_string()))?
    .map_err(|_| AppError::Internal("密码队列不可用".to_string()))?;

    let low_mem = state.cfg.low_mem_mode;
    let pwd_plain = req.password.clone();
    let password_hash = tokio::task::spawn_blocking(move || -> AppResult<String> {
        let salt = SaltString::generate(&mut OsRng);
        let params = if low_mem {
            Params::new(12_288, 3, 1, None).unwrap_or_default()
        } else {
            Params::default()
        };
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(pwd_plain.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AppError::Internal(format!("密码加密失败: {}", e)))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    drop(_permit);

    let user_id = state.snowflake.next_id();
    let personal_space_id = state.snowflake.next_id();
    let now = now_ms();

    let display_name = req
        .nickname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&username)
        .to_string();

    let profile = UserProfileDoc::default_for(&display_name);
    let profile_json = serde_json::to_string(&profile)?;

    let mut tx = state.db.write.begin().await?;
    let doorplate_no = generate_unique_doorplate(&mut tx).await?;

    sqlx::query(
        "INSERT INTO users (id, username, doorplate_no, password_hash, profile_json, updated_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
    )
    .bind(user_id)
    .bind(&username)
    .bind(&doorplate_no)
    .bind(&password_hash)
    .bind(&profile_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO spaces (id, name, space_type_id, owner_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(personal_space_id)
    .bind("我的独处空间")
    .bind(dim::SPACE_PERSONAL)
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO space_members (space_id, user_id, role_id, alias_name, joined_at)
         VALUES (?1, ?2, ?3, NULL, ?4)",
    )
    .bind(personal_space_id)
    .bind(user_id)
    .bind(dim::ROLE_OWNER)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    init_base_space_modules(&mut tx, personal_space_id, false, now).await?;
    tx.commit().await?;

    state
        .cache
        .set_str(&format!("user:ok:{}", user_id), "1", 3600)
        .await;
    state
        .cache
        .set_space_access(user_id, personal_space_id, dim::SPACE_PERSONAL, dim::ROLE_OWNER)
        .await;

    issue_session_response(&state, &headers, jar, user_id, personal_space_id).await
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<LoginReq>,
) -> AppResult<(CookieJar, Json<AuthSessionResp>)> {
    enforce_rate_limit(&state, &headers, "auth_login", 8, 60).await?;
    if req.password.len() > 128 {
        return Err(AppError::Unauthorized("账号或密码错误".to_string()));
    }

    let username = req.username.trim().to_lowercase();
    let row = sqlx::query("SELECT id, password_hash FROM users WHERE username = ?1")
        .bind(&username)
        .fetch_optional(&state.db.read)
        .await?
        .ok_or_else(|| AppError::Unauthorized("账号或密码错误".to_string()))?;

    let user_id: i64 = row.get("id");
    let stored_hash: String = row.get("password_hash");
    let pwd_plain = req.password.clone();

    let _permit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.argon2_sem.acquire(),
    )
    .await
    .map_err(|_| AppError::TooManyRequests("登录排队中，请稍后重试".to_string()))?
    .map_err(|_| AppError::Internal("密码队列不可用".to_string()))?;

    let valid = tokio::task::spawn_blocking(move || -> bool {
        let Ok(parsed) = PasswordHash::new(&stored_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(pwd_plain.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .unwrap_or(false);
    drop(_permit);

    if !valid {
        return Err(AppError::Unauthorized("账号或密码错误".to_string()));
    }

    state
        .cache
        .set_str(&format!("user:ok:{}", user_id), "1", 3600)
        .await;
    let personal_space_id = find_personal_space_id(&state, user_id).await?;
    issue_session_response(&state, &headers, jar, user_id, personal_space_id).await
}

async fn refresh_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> AppResult<(CookieJar, Json<AuthSessionResp>)> {
    enforce_rate_limit(&state, &headers, "auth_refresh", 30, 60).await?;

    let raw_cookie = jar
        .get(REFRESH_COOKIE_NAME)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Unauthorized("缺少刷新凭证，请重新登录".to_string()))?;

    let token_hash = hash_refresh_token(&raw_cookie);
    let now = now_ms();

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query(
        "SELECT user_id, grace_until, expires_at FROM refresh_sessions WHERE token_hash = ?1",
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Unauthorized("刷新凭证已失效，请重新登录".to_string()))?;

    let user_id: i64 = row.get("user_id");
    let grace_until: Option<i64> = row.get("grace_until");
    let expires_at: i64 = row.get("expires_at");

    if now > expires_at {
        let _ = sqlx::query("DELETE FROM refresh_sessions WHERE token_hash = ?1")
            .bind(&token_hash)
            .execute(&mut *tx)
            .await;
        tx.commit().await?;
        return Err(AppError::Unauthorized("刷新凭证已过期，请重新登录".to_string()));
    }

    if let Some(g_until) = grace_until {
        if now > g_until {
            // Replay attempt outside 10s grace window!
            let _ = sqlx::query("DELETE FROM refresh_sessions WHERE token_hash = ?1")
                .bind(&token_hash)
                .execute(&mut *tx)
                .await;
            tx.commit().await?;
            return Err(AppError::Unauthorized("刷新凭证已轮换失效".to_string()));
        }
    } else {
        // First use: mark rotated with 10s grace window so multi-tab simultaneous refreshes succeed cleanly
        let g_exp = (now + ROTATION_GRACE_MS).min(expires_at);
        sqlx::query(
            "UPDATE refresh_sessions SET grace_until = ?1, expires_at = ?1 WHERE token_hash = ?2",
        )
        .bind(g_exp)
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let personal_space_id = find_personal_space_id(&state, user_id).await?;
    issue_session_response(&state, &headers, jar, user_id, personal_space_id).await
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> (CookieJar, Json<serde_json::Value>) {
    if let Some(c) = jar.get(REFRESH_COOKIE_NAME) {
        let th = hash_refresh_token(c.value());
        let _ = sqlx::query("DELETE FROM refresh_sessions WHERE token_hash = ?1")
            .bind(th)
            .execute(&state.db.write)
            .await;
    }
    let secure_attr = if is_https_request(&headers, &state) {
        "; Secure"
    } else {
        ""
    };
    let removal = Cookie::parse(format!(
        "{}=; Path={}; HttpOnly; SameSite=Lax; Max-Age=0{}",
        REFRESH_COOKIE_NAME, REFRESH_COOKIE_PATH, secure_attr
    ))
    .unwrap_or_else(|_| Cookie::new(REFRESH_COOKIE_NAME, ""));
    (jar.add(removal), Json(serde_json::json!({ "ok": true })))
}

async fn issue_session_response(
    state: &AppState,
    headers: &HeaderMap,
    jar: CookieJar,
    user_id: i64,
    personal_space_id: i64,
) -> AppResult<(CookieJar, Json<AuthSessionResp>)> {
    let now = now_ms();
    let now_sec = (now / 1000) as usize;
    let claims = JwtClaims {
        sub: user_id.to_string(),
        iat: now_sec,
        exp: now_sec + state.cfg.jwt_access_ttl_secs as usize,
    };

    let access_token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("签发 JWT 失败: {}", e)))?;

    let refresh_token = format!(
        "{}_{:016x}{:016x}",
        state.snowflake.next_id(),
        rand::thread_rng().gen::<u64>(),
        rand::thread_rng().gen::<u64>()
    );
    let token_hash = hash_refresh_token(&refresh_token);
    let expires_at = now + (state.cfg.jwt_refresh_ttl_secs as i64) * 1000;

    sqlx::query(
        "INSERT INTO refresh_sessions (token_hash, user_id, grace_access_uid, grace_until, expires_at, created_at)
         VALUES (?1, ?2, NULL, NULL, ?3, ?4)",
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(expires_at)
    .bind(now)
    .execute(&state.db.write)
    .await?;

    let secure_attr = if is_https_request(headers, state) {
        "; Secure"
    } else {
        ""
    };
    let cookie_raw = format!(
        "{}={}; Path={}; HttpOnly; SameSite={}; Max-Age={}{}",
        REFRESH_COOKIE_NAME,
        refresh_token,
        REFRESH_COOKIE_PATH,
        SameSite::Lax,
        state.cfg.jwt_refresh_ttl_secs,
        secure_attr
    );
    let cookie = Cookie::parse(cookie_raw)
        .unwrap_or_else(|_| Cookie::new(REFRESH_COOKIE_NAME, refresh_token));

    let user = fetch_user_summary(state, user_id).await?;

    Ok((
        jar.add(cookie),
        Json(AuthSessionResp {
            access_token,
            expires_in_secs: state.cfg.jwt_access_ttl_secs,
            user,
            default_personal_space_id: personal_space_id,
        }),
    ))
}

async fn find_personal_space_id(state: &AppState, user_id: i64) -> AppResult<i64> {
    let row = sqlx::query(
        "SELECT s.id FROM spaces s
         JOIN space_members sm ON sm.space_id = s.id
         WHERE sm.user_id = ?1 AND s.space_type_id = ?2
         LIMIT 1",
    )
    .bind(user_id)
    .bind(dim::SPACE_PERSONAL)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::Internal("未找到默认个人空间".to_string()))?;

    Ok(row.get("id"))
}

async fn generate_unique_doorplate(tx: &mut Transaction<'_, Sqlite>) -> AppResult<String> {
    for _ in 0..20 {
        let num = rand::thread_rng().gen_range(10_000..999_999);
        let code = num.to_string();
        let exists = sqlx::query("SELECT 1 FROM users WHERE doorplate_no = ?1")
            .bind(&code)
            .fetch_optional(&mut **tx)
            .await?;
        if exists.is_none() {
            return Ok(code);
        }
    }
    Err(AppError::Internal("生成专属门牌号失败，请重试".to_string()))
}

pub async fn init_base_space_modules(
    tx: &mut Transaction<'_, Sqlite>,
    space_id: i64,
    is_shared: bool,
    now: i64,
) -> AppResult<()> {
    let fridge_init = r#"{"recent_slips":[]}"#;
    let menu_init = r#"{"categories":["荤菜","素菜","汤羹","主食","甜饮"],"dishes":[]}"#;
    let board_init = r#"{"notes":[],"todos":[]}"#;

    for (mod_id, json_str) in [
        (dim::MOD_FRIDGE, fridge_init),
        (dim::MOD_MENU, menu_init),
        (dim::MOD_BOARD, board_init),
    ] {
        sqlx::query(
            "INSERT INTO space_modules (space_id, module_type_id, version, data_json, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4)",
        )
        .bind(space_id)
        .bind(mod_id)
        .bind(json_str)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }

    if is_shared {
        let pet_init =
            serde_json::to_string(&crate::domains::pet::model::PetStateDoc::unadopted())?;
        sqlx::query(
            "INSERT INTO space_modules (space_id, module_type_id, version, data_json, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4)",
        )
        .bind(space_id)
        .bind(dim::MOD_PET)
        .bind(pet_init)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}
