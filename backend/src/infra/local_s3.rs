use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::put,
    Router,
};
use futures_util::{stream, StreamExt};
use std::{collections::HashMap, io::SeekFrom};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::state::AppState;

const MAX_LOCAL_OBJECT_BYTES: usize = 20 * 1024 * 1024;
const STREAM_CHUNK_SIZE: usize = 32 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/:bucket/*key",
            put(put_local_object)
                .get(get_local_object)
                .delete(delete_local_object),
        )
        .layer(DefaultBodyLimit::max(MAX_LOCAL_OBJECT_BYTES))
}

async fn put_local_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    body: Body,
) -> Response {
    if !state.s3.verify_local_presign("PUT", &bucket, &key, &q) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(file_path) = state.s3.resolve_local_path(&bucket, &key) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if let Some(parent) = file_path.parent() {
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    let tmp_path = file_path.with_extension(format!("part.{}", state.snowflake.next_id()));
    let Ok(mut file) = tokio::fs::File::create(&tmp_path).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let mut stream = body.into_data_stream();
    let mut written: usize = 0;

    while let Some(chunk_res) = stream.next().await {
        let Ok(chunk) = chunk_res else {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return StatusCode::BAD_REQUEST.into_response();
        };
        written += chunk.len();
        if written > MAX_LOCAL_OBJECT_BYTES {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        if file.write_all(&chunk).await.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if file.flush().await.is_err() || tokio::fs::rename(&tmp_path, &file_path).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::OK.into_response()
}

fn stream_file_bounded(file: tokio::fs::File, remaining: u64) -> Body {
    let reader_stream = stream::unfold((file, remaining), |(mut f, rem)| async move {
        if rem == 0 {
            return None;
        }
        let to_read = (rem as usize).min(STREAM_CHUNK_SIZE);
        let mut buf = vec![0u8; to_read];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), (f, rem - n as u64)))
            }
            Err(e) => Some((Err(e), (f, 0))),
        }
    });
    Body::from_stream(reader_stream)
}

async fn get_local_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !state.s3.verify_local_presign("GET", &bucket, &key, &q) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(file_path) = state.s3.resolve_local_path(&bucket, &key) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(mut file) = tokio::fs::File::open(&file_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(meta) = file.metadata().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let total_len = meta.len();

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext {
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => {
            if key.starts_with("live/") {
                "video/webm"
            } else {
                "audio/webm"
            }
        }
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    };

    if let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        if total_len == 0 {
            return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        }
        if let Some((start, end)) = parse_byte_range(range_hdr, total_len) {
            if file.seek(SeekFrom::Start(start)).await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let chunk_len = end - start + 1;
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, mime)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, chunk_len.to_string())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", start, end, total_len),
                )
                .header(header::CACHE_CONTROL, "private, max-age=1800")
                .body(stream_file_bounded(file, chunk_len))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        } else {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", total_len))
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response());
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total_len.to_string())
        .header(header::CACHE_CONTROL, "private, max-age=1800")
        .body(stream_file_bounded(file, total_len))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn delete_local_object(
    State(state): State<AppState>,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !state.s3.verify_local_presign("DELETE", &bucket, &key, &q) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(file_path) = state.s3.resolve_local_path(&bucket, &key) {
        let _ = tokio::fs::remove_file(file_path).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

fn parse_byte_range(hdr: &str, total: u64) -> Option<(u64, u64)> {
    let spec = hdr.strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start_s, end_s) = spec.split_once('-')?;
    if start_s.is_empty() {
        let suffix: u64 = end_s.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = total.saturating_sub(suffix);
        Some((start, total - 1))
    } else {
        let start: u64 = start_s.parse().ok()?;
        if start >= total {
            return None;
        }
        let end = if end_s.is_empty() {
            total - 1
        } else {
            end_s.parse::<u64>().ok()?.min(total - 1)
        };
        if start > end {
            None
        } else {
            Some((start, end))
        }
    }
}
