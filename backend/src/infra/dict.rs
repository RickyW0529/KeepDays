use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

use crate::error::{AppError, AppResult};

// Well-known core Snowflake Dimension Item IDs (seeded in 0001_init_snowflake_schema.sql)
pub mod dim {
    // Domain 10: Space Types
    pub const SPACE_PERSONAL: i64 = 1001;
    pub const SPACE_SHARED: i64 = 1002;

    // Domain 20: Member Roles
    pub const ROLE_OWNER: i64 = 2001;
    pub const ROLE_MEMBER: i64 = 2002;

    // Domain 30: Common Lifecycle Status
    pub const STATUS_PENDING: i64 = 3001;
    pub const STATUS_READY: i64 = 3002;
    pub const STATUS_REJECTED: i64 = 3003;
    pub const STATUS_EXPIRED: i64 = 3004;

    // Domain 40: S3 Media Asset Kinds (extensible via dim_items without DDL changes!)
    pub const ASSET_PHOTO: i64 = 4001;
    pub const ASSET_LIVE_MP4: i64 = 4002;
    pub const ASSET_VOICE: i64 = 4003;
    pub const ASSET_AVATAR: i64 = 4004;
    pub const ASSET_BANNER: i64 = 4005;
    pub const ASSET_DISH_IMG: i64 = 4006;

    // Domain 60: Space Interactive Modules
    pub const MOD_FRIDGE: i64 = 6001;
    pub const MOD_MENU: i64 = 6002;
    pub const MOD_BOARD: i64 = 6003;
    pub const MOD_PET: i64 = 6004;

    // Domain 70: Twitter-style Timeline Event Types
    pub const EVT_PHOTO_POSTED: i64 = 7001;
    pub const EVT_PHOTO_COMMENTED: i64 = 7002;
    pub const EVT_PHOTO_LIKED: i64 = 7003;
    pub const EVT_MENU_PINNED: i64 = 7004;
    pub const EVT_BOARD_UPDATED: i64 = 7005;
    pub const EVT_PROFILE_UPDATED: i64 = 7006;
    pub const EVT_PET_MOMENT: i64 = 7007;
    pub const EVT_MEMBER_JOINED: i64 = 7008;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssetKindRule {
    #[serde(default = "default_s3_dir")]
    pub s3_dir: String,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: i64,
    #[serde(default)]
    pub mimes: Vec<String>,
}

fn default_s3_dir() -> String {
    "misc".to_string()
}

fn default_max_bytes() -> i64 {
    10 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimDomain {
    pub id: i64,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimItem {
    pub id: i64,
    pub domain_id: i64,
    pub code: String,
    pub label: String,
    pub sort_order: i64,
    pub is_active: bool,
    pub config: serde_json::Value,
}

#[derive(Default)]
struct DictSnapshot {
    domains: Vec<DimDomain>,
    items_by_id: HashMap<i64, DimItem>,
    items_by_code: HashMap<String, i64>,
}

/// Zero-JOIN In-Memory Snowflake Dimension Dictionary Registry
#[derive(Clone)]
pub struct DictRegistry {
    inner: Arc<RwLock<DictSnapshot>>,
}

impl DictRegistry {
    pub async fn load_from_db(pool: &SqlitePool) -> Result<Self, sqlx::Error> {
        let registry = Self {
            inner: Arc::new(RwLock::new(DictSnapshot::default())),
        };
        registry.reload(pool).await?;
        Ok(registry)
    }

    pub async fn reload(&self, pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let domain_rows = sqlx::query("SELECT id, code, name FROM dim_domains ORDER BY id ASC")
            .fetch_all(pool)
            .await?;

        let domains = domain_rows
            .into_iter()
            .map(|r| DimDomain {
                id: r.get("id"),
                code: r.get("code"),
                name: r.get("name"),
            })
            .collect();

        let item_rows = sqlx::query(
            "SELECT id, domain_id, code, label, sort_order, is_active, config_json
             FROM dim_items ORDER BY domain_id ASC, sort_order ASC, id ASC",
        )
        .fetch_all(pool)
        .await?;

        let mut items_by_id = HashMap::with_capacity(item_rows.len());
        let mut items_by_code = HashMap::with_capacity(item_rows.len());

        for r in item_rows {
            let id: i64 = r.get("id");
            let code: String = r.get("code");
            let raw_cfg: String = r.get("config_json");
            let config = serde_json::from_str(&raw_cfg).unwrap_or_else(|_| serde_json::json!({}));
            let item = DimItem {
                id,
                domain_id: r.get("domain_id"),
                code: code.clone(),
                label: r.get("label"),
                sort_order: r.get("sort_order"),
                is_active: r.get::<i64, _>("is_active") != 0,
                config,
            };
            items_by_code.insert(code, id);
            items_by_id.insert(id, item);
        }

        let mut guard = self.inner.write().await;
        guard.domains = domains;
        guard.items_by_id = items_by_id;
        guard.items_by_code = items_by_code;
        Ok(())
    }

    pub async fn get_item(&self, id: i64) -> Option<DimItem> {
        self.inner.read().await.items_by_id.get(&id).cloned()
    }

    pub async fn get_asset_rule(&self, kind_id: i64) -> AppResult<(DimItem, AssetKindRule)> {
        let item = self
            .get_item(kind_id)
            .await
            .filter(|i| i.domain_id == 40 && i.is_active)
            .ok_or_else(|| {
                AppError::BadRequest(format!("Unsupported media asset kind_id: {}", kind_id))
            })?;

        let rule: AssetKindRule =
            serde_json::from_value(item.config.clone()).unwrap_or_default();
        Ok((item, rule))
    }

    pub async fn export_all(&self) -> (Vec<DimDomain>, Vec<DimItem>) {
        let guard = self.inner.read().await;
        let mut items: Vec<DimItem> = guard.items_by_id.values().cloned().collect();
        items.sort_by_key(|i| (i.domain_id, i.sort_order, i.id));
        (guard.domains.clone(), items)
    }
}
