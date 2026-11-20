use std::env;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    /// Tuned profile for 1vCPU / 512MB RAM / 10GB SSD micro-instances
    pub low_mem_mode: bool,
    /// Built-in SPA static directory (eliminates separate Nginx process)
    pub static_dir: String,
    /// Persistent local S3 directory when S3_ENDPOINT uses `/local-s3`
    pub local_s3_dir: String,
    /// Isolated filesystem root for Pet Agent sandbox (`~/private` & `~/shared`)
    pub agent_fs_dir: String,
    pub snowflake_worker_id: u16,
    pub database_url: String,
    pub redis_url: String,
    pub jwt_secret: String,
    pub jwt_access_ttl_secs: u64,
    pub jwt_refresh_ttl_secs: u64,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub s3_path_style: bool,
    pub s3_presign_ttl_secs: u64,
    pub cors_allowed_origin: String,
    pub admin_api_key: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv()
            .or_else(|_| dotenvy::from_filename("backend/.env"))
            .or_else(|_| dotenvy::from_filename("KeepDays/backend/.env"));

        let host = env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let raw_secret = env::var("JWT_SECRET").unwrap_or_else(|_| {
            "keepdays_dev_secret_do_not_use_in_prod_9a8b7c6d5e4f3a2b1c".to_string()
        });

        let is_loopback = host == "127.0.0.1" || host == "::1" || host == "localhost";
        if !is_loopback
            && (raw_secret.len() < 32
                || raw_secret.starts_with("REPLACE_WITH_")
                || raw_secret.contains("do_not_use_in_prod"))
        {
            panic!(
                "SECURITY ERROR: Refusing to bind non-loopback HOST={} with placeholder/weak JWT_SECRET! Set a strong >=32-byte JWT_SECRET in keepdays.env.",
                host
            );
        }

        Self {
            host,
            port: env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000),
            low_mem_mode: env::var("LOW_MEM_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
            static_dir: env::var("STATIC_DIR")
                .unwrap_or_else(|_| "../frontend/dist".to_string()),
            local_s3_dir: env::var("LOCAL_S3_DIR")
                .unwrap_or_else(|_| "data/uploads".to_string()),
            agent_fs_dir: env::var("AGENT_FS_DIR")
                .unwrap_or_else(|_| "data/agent_fs".to_string()),
            snowflake_worker_id: env::var("SNOWFLAKE_WORKER_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
                & 0x3FF,
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://data/keepdays.db?mode=rwc".to_string()),
            redis_url: env::var("REDIS_URL").unwrap_or_else(|_| "memory://".to_string()),
            jwt_secret: raw_secret,
            jwt_access_ttl_secs: env::var("JWT_ACCESS_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            jwt_refresh_ttl_secs: env::var("JWT_REFRESH_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(604_800),
            s3_endpoint: env::var("S3_ENDPOINT")
                .unwrap_or_else(|_| "/local-s3".to_string()),
            s3_region: env::var("S3_REGION").unwrap_or_else(|_| "auto".to_string()),
            s3_bucket: env::var("S3_BUCKET").unwrap_or_else(|_| "keepdays-private".to_string()),
            s3_access_key: env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "keepdays_dev".to_string()),
            s3_secret_key: env::var("S3_SECRET_KEY")
                .unwrap_or_else(|_| "keepdays_dev_secret".to_string()),
            s3_path_style: env::var("S3_PATH_STYLE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
            s3_presign_ttl_secs: env::var("S3_PRESIGN_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800),
            cors_allowed_origin: env::var("CORS_ALLOWED_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:5173".to_string()),
            admin_api_key: env::var("ADMIN_API_KEY")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| s.len() >= 16),
        }
    }
}
