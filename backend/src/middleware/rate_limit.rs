use axum::http::HeaderMap;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Extracts client IP safely:
/// - Trusts `CF-Connecting-IP` / `X-Real-IP` / `X-Forwarded-For` ONLY when bound to loopback
///   (behind local reverse proxy / Cloudflare Tunnel) or when `x-peer-loopback` is set.
/// - Otherwise uses the actual TCP peer IP injected from `ConnectInfo<SocketAddr>`.
pub fn extract_client_ip(headers: &HeaderMap, state: &AppState) -> String {
    let peer_ip = headers
        .get("x-internal-peer-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);

    let behind_trusted_local_proxy = match peer_ip {
        Some(ip) => ip == "127.0.0.1" || ip == "::1",
        None => {
            state.cfg.host == "127.0.0.1"
                || state.cfg.host == "::1"
                || state.cfg.host == "localhost"
        }
    };

    if behind_trusted_local_proxy {
        if let Some(cf_ip) = headers
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
        {
            let trimmed = cf_ip.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(real_ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            let trimmed = real_ip.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                let trimmed = first.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }

    peer_ip.unwrap_or("127.0.0.1").to_string()
}

pub async fn enforce_rate_limit(
    state: &AppState,
    headers: &HeaderMap,
    endpoint_tag: &str,
    limit: u64,
    window_secs: u64,
) -> AppResult<()> {
    let ip = extract_client_ip(headers, state);
    let key = format!("ip:{}:{}", ip, endpoint_tag);
    if !state.cache.check_rate_limit(&key, limit, window_secs).await {
        return Err(AppError::TooManyRequests(
            "请求过于频繁，请稍后再试".to_string(),
        ));
    }
    Ok(())
}
