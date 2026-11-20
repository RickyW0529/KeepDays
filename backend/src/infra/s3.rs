use chrono::{NaiveDateTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path as StdPath, PathBuf},
};

use crate::{
    config::AppConfig,
    infra::{
        cache::{CacheService, CachedAssetUrls},
        dict::DictRegistry,
    },
};

type HmacSha256 = Hmac<Sha256>;

const LOCAL_S3_SIGNING_HOST: &str = "local-s3.keepdays.internal";

/// Lightweight, Zero-Bloat AWS Signature V4 S3 / Cloudflare R2 + Origin-Relative Local S3 Client
#[derive(Clone)]
pub struct S3Service {
    endpoint: String,
    host_header: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    path_style: bool,
    local_mode: bool,
    local_s3_dir: String,
    presign_ttl_secs: u64,
    http: reqwest::Client,
}

impl S3Service {
    pub fn new(cfg: &AppConfig) -> Self {
        let trimmed = cfg.s3_endpoint.trim_end_matches('/').to_string();
        let local_mode = trimmed == "/local-s3" || trimmed.ends_with("/local-s3");

        let host_header = if local_mode {
            LOCAL_S3_SIGNING_HOST.to_string()
        } else {
            let without_scheme = trimmed
                .strip_prefix("https://")
                .or_else(|| trimmed.strip_prefix("http://"))
                .unwrap_or(&trimmed);
            let raw_host = without_scheme.split('/').next().unwrap_or("127.0.0.1:9000");
            if cfg.s3_path_style {
                raw_host.to_string()
            } else {
                format!("{}.{}", cfg.s3_bucket, raw_host)
            }
        };

        Self {
            endpoint: if local_mode {
                "/local-s3".to_string()
            } else {
                trimmed
            },
            host_header,
            region: cfg.s3_region.clone(),
            bucket: cfg.s3_bucket.clone(),
            access_key: cfg.s3_access_key.clone(),
            secret_key: cfg.s3_secret_key.clone(),
            path_style: if local_mode { true } else { cfg.s3_path_style },
            local_mode,
            local_s3_dir: cfg.local_s3_dir.clone(),
            presign_ttl_secs: cfg.s3_presign_ttl_secs,
            http: reqwest::Client::new(),
        }
    }

    pub fn is_local_s3(&self) -> bool {
        self.local_mode
    }

    pub fn resolve_local_path(&self, bucket: &str, key: &str) -> Option<PathBuf> {
        if bucket != self.bucket {
            return None;
        }
        let mut out = PathBuf::from(&self.local_s3_dir);
        for seg in StdPath::new(bucket).components() {
            match seg {
                Component::Normal(s) => out.push(s),
                _ => return None,
            }
        }
        for seg in StdPath::new(key).components() {
            match seg {
                Component::Normal(s) => out.push(s),
                _ => return None,
            }
        }
        Some(out)
    }

    pub fn object_key(s3_dir: &str, asset_id: i64, ext: &str) -> String {
        let safe_dir: String = s3_dir
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let dir = if safe_dir.is_empty() { "misc" } else { &safe_dir };
        format!(
            "{}/{}.{}",
            dir,
            asset_id,
            ext.trim_start_matches('.')
        )
    }

    pub fn thumb_key(s3_dir: &str, asset_id: i64) -> String {
        let safe_dir: String = s3_dir
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let dir = if safe_dir.is_empty() { "misc" } else { &safe_dir };
        format!("{}/{}_thumb.webp", dir, asset_id)
    }

    pub fn presign_put(&self, object_key: &str, ttl_secs: u64) -> String {
        self.presign("PUT", object_key, ttl_secs)
    }

    pub fn presign_get(&self, object_key: &str) -> String {
        self.presign("GET", object_key, self.presign_ttl_secs)
    }

    /// Constant-time cryptographic verification of AWS SigV4 presigned URLs for `/local-s3`
    pub fn verify_local_presign(
        &self,
        method: &str,
        bucket: &str,
        object_key: &str,
        query: &HashMap<String, String>,
    ) -> bool {
        if !self.local_mode || bucket != self.bucket {
            return false;
        }
        let Some(alg) = query.get("X-Amz-Algorithm") else {
            return false;
        };
        if alg != "AWS4-HMAC-SHA256" {
            return false;
        }
        let Some(amz_date) = query.get("X-Amz-Date") else {
            return false;
        };
        let Some(expires_str) = query.get("X-Amz-Expires") else {
            return false;
        };
        let Some(provided_sig) = query.get("X-Amz-Signature") else {
            return false;
        };
        let Some(cred) = query.get("X-Amz-Credential") else {
            return false;
        };

        let Ok(expires_secs) = expires_str.parse::<i64>() else {
            return false;
        };
        if !(1..=86_400).contains(&expires_secs) {
            return false;
        }

        let Ok(naive) = NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ") else {
            return false;
        };
        let issued_at = Utc.from_utc_datetime(&naive);
        let now = Utc::now();
        // Allow up to 60s clock skew in the past, strict expiry enforcement
        if now.timestamp() < issued_at.timestamp() - 60
            || now.timestamp() > issued_at.timestamp() + expires_secs
        {
            return false;
        }

        let Some(date_stamp) = amz_date.get(0..8) else {
            return false;
        };
        let expected_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.region);
        let expected_cred = format!("{}/{}", self.access_key, expected_scope);
        if cred != &expected_cred {
            return false;
        }

        let clean_key = object_key.trim_start_matches('/');
        let canonical_uri = format!(
            "/{}/{}",
            uri_encode(&self.bucket, true),
            uri_encode(clean_key, false)
        );
        let canonical_query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders=host",
            uri_encode(&expected_cred, true),
            amz_date,
            expires_secs
        );
        let canonical_headers = format!("host:{}\n", self.host_header);
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\nhost\nUNSIGNED-PAYLOAD",
            method, canonical_uri, canonical_query, canonical_headers
        );

        let cr_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, expected_scope, cr_hash
        );

        let signing_key = derive_signing_key(&self.secret_key, date_stamp, &self.region, "s3");
        let expected_sig = hex::encode(hmac_sign(&signing_key, string_to_sign.as_bytes()));

        constant_time_eq(provided_sig.as_bytes(), expected_sig.as_bytes())
    }

    pub async fn hydrate_asset_urls(
        &self,
        cache: &CacheService,
        dict: &DictRegistry,
        assets: &[(i64, i64, String, bool)],
    ) -> HashMap<String, CachedAssetUrls> {
        let mut out = HashMap::with_capacity(assets.len());
        let mut seen = HashSet::new();
        let cache_ttl = self.presign_ttl_secs.saturating_sub(300).max(60);

        for (asset_id, kind_id, mime, has_thumb) in assets {
            if !seen.insert(*asset_id) {
                continue;
            }
            if let Some(cached) = cache.get_presigned(*asset_id).await {
                out.insert(asset_id.to_string(), cached);
                continue;
            }

            let s3_dir = dict
                .get_asset_rule(*kind_id)
                .await
                .map(|(_, r)| r.s3_dir)
                .unwrap_or_else(|_| "photos".to_string());
            let ext = ext_from_mime(mime);
            let main_key = Self::object_key(&s3_dir, *asset_id, ext);
            let full_url = self.presign_get(&main_key);
            let thumb_url = if *has_thumb {
                Some(self.presign_get(&Self::thumb_key(&s3_dir, *asset_id)))
            } else {
                None
            };

            let pair = CachedAssetUrls {
                full_url,
                thumb_url,
            };
            cache.set_presigned(*asset_id, &pair, cache_ttl).await;
            out.insert(asset_id.to_string(), pair);
        }

        out
    }

    /// Physical Object Deletion (invoked asynchronously ONLY when `media_assets.ref_count == 0`)
    pub async fn delete_object(&self, object_key: &str) {
        if self.local_mode {
            if let Some(path) = self.resolve_local_path(&self.bucket, object_key) {
                let _ = tokio::fs::remove_file(path).await;
            }
            return;
        }

        let delete_url = self.presign("DELETE", object_key, 120);
        if let Err(e) = self.http.delete(&delete_url).send().await {
            tracing::warn!("S3 delete object {} best-effort warning: {}", object_key, e);
        }
    }

    /// Direct internal object write (used by Pet Agent S3 Session & Memory Archiver).
    /// Seamlessly writes atomically to `local_s3_dir` in `/local-s3` mode or PUTs via SigV4 to R2/S3.
    pub async fn put_bytes(
        &self,
        object_key: &str,
        content_type: &str,
        data: &[u8],
    ) -> Result<(), String> {
        let clean_key = object_key.trim_start_matches('/');
        if self.local_mode {
            let path = self
                .resolve_local_path(&self.bucket, clean_key)
                .ok_or_else(|| "非法 S3 对象路径".to_string())?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("创建本地 S3 目录失败: {e}"))?;
            }
            let tmp = path.with_extension(format!(
                "tmp.{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            tokio::fs::write(&tmp, data)
                .await
                .map_err(|e| format!("写入本地 S3 临时文件失败: {e}"))?;
            tokio::fs::rename(&tmp, &path)
                .await
                .map_err(|e| format!("原子提交本地 S3 对象失败: {e}"))?;
            return Ok(());
        }

        let put_url = self.presign("PUT", clean_key, 300);
        let resp = self
            .http
            .put(&put_url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| format!("S3 PUT 网络错误: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("S3 PUT 状态异常: {}", resp.status()));
        }
        Ok(())
    }

    /// Direct internal object read (returns `Ok(None)` when object does not exist).
    pub async fn get_bytes(&self, object_key: &str) -> Result<Option<Vec<u8>>, String> {
        let clean_key = object_key.trim_start_matches('/');
        if self.local_mode {
            let path = self
                .resolve_local_path(&self.bucket, clean_key)
                .ok_or_else(|| "非法 S3 对象路径".to_string())?;
            match tokio::fs::read(&path).await {
                Ok(bytes) => return Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("读取本地 S3 对象失败: {e}")),
            }
        }

        let get_url = self.presign("GET", clean_key, 300);
        let resp = self
            .http
            .get(&get_url)
            .send()
            .await
            .map_err(|e| format!("S3 GET 网络错误: {e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(format!("S3 GET 状态异常: {}", resp.status()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("读取 S3 响应字节失败: {e}"))?;
        Ok(Some(bytes.to_vec()))
    }

    fn presign(&self, method: &str, object_key: &str, expires_secs: u64) -> String {
        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        let clean_key = object_key.trim_start_matches('/');
        let (canonical_uri, base_url) = if self.path_style {
            (
                format!(
                    "/{}/{}",
                    uri_encode(&self.bucket, true),
                    uri_encode(clean_key, false)
                ),
                self.endpoint.clone(),
            )
        } else {
            let scheme = if self.endpoint.starts_with("https://") {
                "https"
            } else {
                "http"
            };
            (
                format!("/{}", uri_encode(clean_key, false)),
                format!("{}://{}", scheme, self.host_header),
            )
        };

        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.region);
        let credential = format!("{}/{}", self.access_key, credential_scope);

        let canonical_query = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders=host",
            uri_encode(&credential, true),
            amz_date,
            expires_secs
        );

        let canonical_headers = format!("host:{}\n", self.host_header);
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\nhost\nUNSIGNED-PAYLOAD",
            method, canonical_uri, canonical_query, canonical_headers
        );

        let cr_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, cr_hash
        );

        let signing_key = derive_signing_key(&self.secret_key, &date_stamp, &self.region, "s3");
        let signature = hex::encode(hmac_sign(&signing_key, string_to_sign.as_bytes()));

        format!(
            "{}{}?{}&X-Amz-Signature={}",
            base_url, canonical_uri, canonical_query, signature
        )
    }
}

pub fn normalize_mime(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

pub fn ext_from_mime(mime: &str) -> &'static str {
    match normalize_mime(mime).as_str() {
        "image/webp" => "webp",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "audio/webm" => "webm",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        _ => "bin",
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac_sign(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sign(format!("AWS4{}", secret).as_bytes(), date.as_bytes());
    let k_region = hmac_sign(&k_date, region.as_bytes());
    let k_service = hmac_sign(&k_region, service.as_bytes());
    hmac_sign(&k_service, b"aws4_request")
}

fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved || (!encode_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
