use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use std::path::{Component, Path, PathBuf};

use crate::state::AppState;

/// Built-in Zero-Nginx SPA Static File Host:
/// - Serves Vite compiled frontend (`dist/`) directly from the single Rust binary (saves ~25MB Nginx RAM!)
/// - Blocks directory traversal (`..` components rejected)
/// - Sets 1-year `immutable` cache headers on hashed `/assets/*` bundles for 0ms repeat visits
/// - Falls back to `index.html` (`no-cache`) for client-side React routes
pub async fn spa_fallback_handler(State(state): State<AppState>, uri: Uri) -> Response {
    let req_path = uri.path();
    if req_path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"code":404,"error":"NOT_FOUND","message":"API route not found"}"#,
        )
            .into_response();
    }

    let root = Path::new(&state.cfg.static_dir);
    let clean_rel = sanitize_relative_path(req_path);

    if !clean_rel.as_os_str().is_empty() {
        let candidate = root.join(&clean_rel);
        if let Ok(meta) = tokio::fs::metadata(&candidate).await {
            if meta.is_file() {
                if let Ok(bytes) = tokio::fs::read(&candidate).await {
                    let mime = guess_mime(&candidate);
                    let cache_ctrl = if req_path.starts_with("/assets/") {
                        "public, max-age=31536000, immutable"
                    } else {
                        "public, max-age=3600"
                    };
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, mime)
                        .header(header::CACHE_CONTROL, cache_ctrl)
                        .body(Body::from(bytes))
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
        }
    }

    // Fallback to SPA entry `index.html`
    let index_file = root.join("index.html");
    if let Ok(bytes) = tokio::fs::read(&index_file).await {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        r#"{"service":"KeepDays API Server","hint":"Frontend dist/ not built yet; API ready at /api/v1"}"#,
    )
        .into_response()
}

fn sanitize_relative_path(raw: &str) -> PathBuf {
    let mut clean = PathBuf::new();
    for comp in Path::new(raw.trim_start_matches('/')).components() {
        if let Component::Normal(seg) = comp {
            clean.push(seg);
        }
    }
    clean
}

fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}
