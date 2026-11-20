#![deny(clippy::indexing_slicing)]

pub fn with_auth_retry(
    client: reqwest::Client,
    credentials: std::sync::Arc<dyn xai_grok_auth::AuthCredentialProvider>,
) -> reqwest_middleware::ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(xai_grok_auth::AuthRetryMiddleware::new(credentials, 1))
        .build()
}

pub fn sha256_hex(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

pub fn sha256_hex_from_file(
    path: &std::path::Path,
    max_bytes: Option<u64>,
) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut reader: Box<dyn Read> = if let Some(limit) = max_bytes {
        Box::new(file.take(limit))
    } else {
        Box::new(file)
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..bytes_read) {
            hasher.update(chunk);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub mod s3 {
    use chrono::Utc;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    #[derive(Clone)]
    pub struct S3StaticCredentials {
        pub access_key_id: String,
        pub secret_access_key: String,
    }

    impl std::fmt::Debug for S3StaticCredentials {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("S3StaticCredentials")
                .field("access_key_id", &"[redacted]")
                .field("secret_access_key", &"[redacted]")
                .finish()
        }
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut mac = HmacSha256::new_from_slice(key)
            .map_err(|e| anyhow::anyhow!("HMAC key error: {e}"))?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn uri_encode(input: &str, encode_slash: bool) -> String {
        let mut out = String::with_capacity(input.len() * 2);
        for &b in input.as_bytes() {
            let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
            if unreserved || (!encode_slash && b == b'/') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    fn presign_sigv4(
        method: &str,
        region: &str,
        endpoint_url: Option<&str>,
        creds: &S3StaticCredentials,
        bucket: &str,
        key: &str,
        content_type: Option<&str>,
        expires_in: std::time::Duration,
    ) -> anyhow::Result<String> {
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let expires_secs = expires_in.as_secs().clamp(1, 604_800).to_string();

        let (scheme, host, canonical_uri) = if let Some(ep) = endpoint_url {
            let parsed = url::Url::parse(ep)?;
            let host = match parsed.port() {
                Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or("localhost")),
                None => parsed.host_str().unwrap_or("localhost").to_string(),
            };
            let base_path = parsed.path().trim_end_matches('/');
            let raw_path = format!(
                "{}/{}/{}",
                base_path,
                bucket.trim_matches('/'),
                key.trim_start_matches('/')
            );
            (
                parsed.scheme().to_string(),
                host,
                uri_encode(&raw_path, false),
            )
        } else {
            let host = format!("{bucket}.s3.{region}.amazonaws.com");
            let raw_path = format!("/{}", key.trim_start_matches('/'));
            ("https".to_string(), host, uri_encode(&raw_path, false))
        };

        let credential_scope = format!("{date_stamp}/{region}/s3/aws4_request");
        let credential = format!("{}/{credential_scope}", creds.access_key_id);
        let signed_headers = if content_type.is_some() {
            "content-type;host"
        } else {
            "host"
        };

        let mut query_pairs = vec![
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", credential),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", expires_secs),
            ("X-Amz-SignedHeaders", signed_headers.to_string()),
        ];
        query_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_query: String = query_pairs
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect::<Vec<_>>()
            .join("&");

        let canonical_headers = if let Some(ct) = content_type {
            format!("content-type:{}\nhost:{host}\n", ct.trim())
        } else {
            format!("host:{host}\n")
        };

        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
        );
        let hashed_canonical_request = format!("{:x}", Sha256::digest(canonical_request.as_bytes()));
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{hashed_canonical_request}");

        let k_date = hmac_sha256(
            format!("AWS4{}", creds.secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        )?;
        let k_region = hmac_sha256(&k_date, region.as_bytes())?;
        let k_service = hmac_sha256(&k_region, b"s3")?;
        let k_signing = hmac_sha256(&k_service, b"aws4_request")?;
        let sig_bytes = hmac_sha256(&k_signing, string_to_sign.as_bytes())?;
        let signature: String = sig_bytes.iter().map(|b| format!("{b:02x}")).collect();

        Ok(format!(
            "{scheme}://{host}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}"
        ))
    }

    pub async fn presign_put_url(
        region: &str,
        endpoint_url: Option<&str>,
        creds: &S3StaticCredentials,
        bucket: &str,
        key: &str,
        content_type: &str,
        expires_in: std::time::Duration,
    ) -> anyhow::Result<String> {
        presign_sigv4(
            "PUT",
            region,
            endpoint_url,
            creds,
            bucket,
            key,
            Some(content_type),
            expires_in,
        )
    }

    pub async fn presign_get_url(
        region: &str,
        endpoint_url: Option<&str>,
        creds: &S3StaticCredentials,
        bucket: &str,
        key: &str,
        expires_in: std::time::Duration,
    ) -> anyhow::Result<String> {
        presign_sigv4(
            "GET",
            region,
            endpoint_url,
            creds,
            bucket,
            key,
            None,
            expires_in,
        )
    }
}
