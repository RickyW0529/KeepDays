use redis::{aio::ConnectionManager, AsyncCommands};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

use crate::snowflake::{i64_str, opt_i64_str};

const MAX_MEM_KV_ENTRIES: usize = 8_000;
const MAX_MEM_COUNTERS: usize = 4_000;
const MAX_MEM_TIMELINE_ROOMS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceFocus {
    #[serde(with = "i64_str")]
    pub user_id: i64,
    /// "IDLE" | "FRIDGE" | "PHOTO"
    pub focus: String,
    #[serde(default, with = "opt_i64_str")]
    pub photo_id: Option<i64>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAssetUrls {
    pub full_url: String,
    pub thumb_url: Option<String>,
}

#[derive(Default)]
struct MemoryState {
    kv: HashMap<String, (String, Instant)>,
    counters: HashMap<String, (u64, Instant)>,
    timelines: HashMap<i64, VecDeque<(i64, String)>>,
    presence: HashMap<i64, HashMap<i64, (PresenceFocus, Instant)>>,
    pet_touches: HashMap<i64, HashMap<i64, Instant>>,
    pet_resonance_cd: HashMap<i64, Instant>,
}

#[derive(Clone)]
enum CacheBackend {
    Redis(ConnectionManager),
    Memory(Arc<RwLock<MemoryState>>),
}

/// Unified High-Performance Cache Service
/// Supports Production Redis (`redis://...`) and Bounded Memory (`memory://`)
#[derive(Clone)]
pub struct CacheService {
    backend: CacheBackend,
    timeline_cap: usize,
}

impl CacheService {
    pub async fn connect(redis_url: &str, low_mem_mode: bool) -> Self {
        let timeline_cap = if low_mem_mode { 200 } else { 500 };

        if redis_url.starts_with("memory://") || redis_url.is_empty() {
            tracing::info!(
                "CacheService initialized in bounded memory mode (timeline_cap={})",
                timeline_cap
            );
            return Self {
                backend: CacheBackend::Memory(Arc::new(RwLock::new(MemoryState::default()))),
                timeline_cap,
            };
        }

        match redis::Client::open(redis_url) {
            Ok(client) => match ConnectionManager::new(client).await {
                Ok(mgr) => {
                    tracing::info!("CacheService connected to Redis: {}", redis_url);
                    Self {
                        backend: CacheBackend::Redis(mgr),
                        timeline_cap,
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Redis connection failed ({}), falling back to in-memory cache",
                        e
                    );
                    Self {
                        backend: CacheBackend::Memory(Arc::new(RwLock::new(MemoryState::default()))),
                        timeline_cap,
                    }
                }
            },
            Err(e) => {
                tracing::warn!("Invalid REDIS_URL ({}), using in-memory cache", e);
                Self {
                    backend: CacheBackend::Memory(Arc::new(RwLock::new(MemoryState::default()))),
                    timeline_cap,
                }
            }
        }
    }

    pub async fn evict_expired_memory(&self) {
        if let CacheBackend::Memory(mem) = &self.backend {
            let mut g = mem.write().await;
            let now = Instant::now();
            g.kv.retain(|_, (_, exp)| *exp > now);
            g.counters.retain(|_, (_, exp)| *exp > now);
            g.pet_resonance_cd.retain(|_, exp| *exp > now);
            g.presence.retain(|_, room| {
                room.retain(|_, (_, exp)| *exp > now);
                !room.is_empty()
            });
            g.pet_touches.retain(|_, touches| {
                touches.retain(|_, exp| *exp > now);
                !touches.is_empty()
            });
            g.kv.shrink_to_fit();
            g.counters.shrink_to_fit();
        }
    }

    // =========================================================================
    // 1. Bounded Rate Limiter (`rate:ip:{ip}:{ep}`)
    // =========================================================================
    pub async fn check_rate_limit(&self, bucket_key: &str, limit: u64, window_secs: u64) -> bool {
        let full_key = format!("rate:{}", bucket_key);
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let count: u64 = redis::pipe()
                    .atomic()
                    .incr(&full_key, 1u64)
                    .expire(&full_key, window_secs as i64)
                    .query_async::<Vec<u64>>(&mut conn)
                    .await
                    .ok()
                    .and_then(|v| v.first().copied())
                    .unwrap_or(1);
                count <= limit
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                let now = Instant::now();
                if g.counters.len() >= MAX_MEM_COUNTERS && !g.counters.contains_key(&full_key) {
                    g.counters.retain(|_, (_, exp)| *exp > now);
                    if g.counters.len() >= MAX_MEM_COUNTERS {
                        if let Some(any_key) = g.counters.keys().next().cloned() {
                            g.counters.remove(&any_key);
                        }
                    }
                }
                let entry = g
                    .counters
                    .entry(full_key)
                    .or_insert((0, now + Duration::from_secs(window_secs)));
                if now >= entry.1 {
                    *entry = (1, now + Duration::from_secs(window_secs));
                    true
                } else {
                    entry.0 += 1;
                    entry.0 <= limit
                }
            }
        }
    }

    // =========================================================================
    // 2. Generic KV with TTL (User Profiles, Space Membership, S3 Presigns)
    // =========================================================================
    pub async fn get_str(&self, key: &str) -> Option<String> {
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                conn.get(key).await.ok()
            }
            CacheBackend::Memory(mem) => {
                let g = mem.read().await;
                g.kv.get(key).and_then(|(val, exp)| {
                    if Instant::now() < *exp {
                        Some(val.clone())
                    } else {
                        None
                    }
                })
            }
        }
    }

    pub async fn set_str(&self, key: &str, val: &str, ttl_secs: u64) {
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let _: Result<(), _> = conn.set_ex(key, val, ttl_secs).await;
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                let now = Instant::now();
                if g.kv.len() >= MAX_MEM_KV_ENTRIES && !g.kv.contains_key(key) {
                    g.kv.retain(|_, (_, exp)| *exp > now);
                    if g.kv.len() >= MAX_MEM_KV_ENTRIES {
                        if let Some(any_key) = g.kv.keys().next().cloned() {
                            g.kv.remove(&any_key);
                        }
                    }
                }
                g.kv.insert(
                    key.to_string(),
                    (val.to_string(), now + Duration::from_secs(ttl_secs)),
                );
            }
        }
    }

    pub async fn del_key(&self, key: &str) {
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let _: Result<(), _> = conn.del(key).await;
            }
            CacheBackend::Memory(mem) => {
                mem.write().await.kv.remove(key);
            }
        }
    }

    // =========================================================================
    // 3. Space Tenant Membership Cache
    // =========================================================================
    pub async fn get_space_access(&self, user_id: i64, space_id: i64) -> Option<(i64, i64)> {
        let key = format!("space:acc:{}:{}", user_id, space_id);
        let raw = self.get_str(&key).await?;
        let mut parts = raw.split(':');
        let space_type_id = parts.next()?.parse().ok()?;
        let role_id = parts.next()?.parse().ok()?;
        Some((space_type_id, role_id))
    }

    pub async fn set_space_access(
        &self,
        user_id: i64,
        space_id: i64,
        space_type_id: i64,
        role_id: i64,
    ) {
        let key = format!("space:acc:{}:{}", user_id, space_id);
        self.set_str(&key, &format!("{}:{}", space_type_id, role_id), 3600)
            .await;
    }

    pub async fn revoke_space_access(&self, user_id: i64, space_id: i64) {
        let key = format!("space:acc:{}:{}", user_id, space_id);
        self.del_key(&key).await;
    }

    // =========================================================================
    // 4. S3 Fixed-Window (25 min) Presigned URL Cache
    // =========================================================================
    pub async fn get_presigned(&self, asset_id: i64) -> Option<CachedAssetUrls> {
        let raw = self.get_str(&format!("s3:sign:{}", asset_id)).await?;
        serde_json::from_str(&raw).ok()
    }

    pub async fn set_presigned(&self, asset_id: i64, urls: &CachedAssetUrls, ttl_secs: u64) {
        if let Ok(raw) = serde_json::to_string(urls) {
            self.set_str(&format!("s3:sign:{}", asset_id), &raw, ttl_secs)
                .await;
        }
    }

    // =========================================================================
    // 5. Silent Co-Presence (`presence:{space_id}`)
    // =========================================================================
    pub async fn update_presence(&self, space_id: i64, focus: PresenceFocus) {
        let key = format!("presence:{}", space_id);
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                if let Ok(raw) = serde_json::to_string(&focus) {
                    let _: Result<(), _> = conn.hset(&key, focus.user_id.to_string(), raw).await;
                    let _: Result<(), _> = conn.expire(&key, 60).await;
                }
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                let room = g.presence.entry(space_id).or_default();
                room.insert(
                    focus.user_id,
                    (focus, Instant::now() + Duration::from_secs(45)),
                );
            }
        }
    }

    pub async fn list_presence(&self, space_id: i64, now_ms: i64) -> Vec<PresenceFocus> {
        let key = format!("presence:{}", space_id);
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let map: HashMap<String, String> = conn.hgetall(&key).await.unwrap_or_default();
                map.into_values()
                    .filter_map(|v| serde_json::from_str::<PresenceFocus>(&v).ok())
                    .filter(|p| now_ms - p.updated_at_ms <= 45_000)
                    .collect()
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                let now = Instant::now();
                if let Some(room) = g.presence.get_mut(&space_id) {
                    room.retain(|_, (_, exp)| *exp > now);
                    room.values().map(|(f, _)| f.clone()).collect()
                } else {
                    Vec::new()
                }
            }
        }
    }

    // =========================================================================
    // 6. Pet 5-Minute Co-Op Resonance Window (Single-Use Consumed Pair + Cooldown!)
    // =========================================================================
    pub async fn record_pet_touch(&self, space_id: i64, actor_id: i64, now_ms: i64) -> Option<i64> {
        let key = format!("pet:touch:{}", space_id);
        let cd_key = format!("pet:res_cd:{}", space_id);
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let in_cooldown: bool = conn.exists(&cd_key).await.unwrap_or(false);
                let existing: HashMap<String, i64> = conn.hgetall(&key).await.unwrap_or_default();
                let partner = if in_cooldown {
                    None
                } else {
                    existing
                        .into_iter()
                        .filter_map(|(k, ts)| k.parse::<i64>().ok().map(|uid| (uid, ts)))
                        .find(|(uid, ts)| *uid != actor_id && now_ms - *ts <= 300_000)
                        .map(|(uid, _)| uid)
                };

                if let Some(p_uid) = partner {
                    // Consume partner touch & arm 60s resonance cooldown so spam-clicking cannot farm coins
                    let _: Result<(), _> = conn.hdel(&key, p_uid.to_string()).await;
                    let _: Result<(), _> = conn.hdel(&key, actor_id.to_string()).await;
                    let _: Result<(), _> = conn.set_ex(&cd_key, "1", 60).await;
                } else {
                    let _: Result<(), _> = conn.hset(&key, actor_id.to_string(), now_ms).await;
                    let _: Result<(), _> = conn.expire(&key, 300).await;
                }
                partner
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                let now = Instant::now();
                let in_cooldown = g
                    .pet_resonance_cd
                    .get(&space_id)
                    .map(|exp| *exp > now)
                    .unwrap_or(false);

                let touches = g.pet_touches.entry(space_id).or_default();
                touches.retain(|_, exp| *exp > now);

                let partner = if in_cooldown {
                    None
                } else {
                    touches.keys().copied().find(|uid| *uid != actor_id)
                };

                if partner.is_some() {
                    touches.clear();
                    g.pet_resonance_cd
                        .insert(space_id, now + Duration::from_secs(60));
                } else {
                    touches.insert(actor_id, now + Duration::from_secs(300));
                }
                partner
            }
        }
    }

    // =========================================================================
    // 7. Twitter-Style Hot Timeline (`LIST` in Redis — zero f64 Snowflake rounding!)
    // =========================================================================
    pub async fn push_timeline_item(&self, space_id: i64, event_id: i64, item_json: &str) {
        let key = format!("timeline:list:{}", space_id);
        let cap = self.timeline_cap;
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let packed = format!("{}:{}", event_id, item_json);
                let _: Result<(), _> = redis::pipe()
                    .lpush(&key, packed)
                    .ltrim(&key, 0, (cap as isize) - 1)
                    .query_async(&mut conn)
                    .await;
            }
            CacheBackend::Memory(mem) => {
                let mut g = mem.write().await;
                if g.timelines.len() >= MAX_MEM_TIMELINE_ROOMS && !g.timelines.contains_key(&space_id)
                {
                    if let Some(any_sid) = g.timelines.keys().next().copied() {
                        g.timelines.remove(&any_sid);
                    }
                }
                let q = g.timelines.entry(space_id).or_default();
                q.push_front((event_id, item_json.to_string()));
                while q.len() > cap {
                    q.pop_back();
                }
            }
        }
    }

    /// Serves from hot cache ONLY when a full `limit` page is satisfied within the cached window;
    /// otherwise returns `None` so SQLite keyset pagination seamlessly serves historical events!
    pub async fn get_hot_timeline(
        &self,
        space_id: i64,
        before_id: Option<i64>,
        limit: usize,
    ) -> Option<Vec<String>> {
        let key = format!("timeline:list:{}", space_id);
        match &self.backend {
            CacheBackend::Redis(mgr) => {
                let mut conn = mgr.clone();
                let raw_entries: Vec<String> = conn
                    .lrange(&key, 0, (self.timeline_cap as isize) - 1)
                    .await
                    .ok()?;
                if raw_entries.is_empty() {
                    return None;
                }
                let slice: Vec<String> = raw_entries
                    .into_iter()
                    .filter_map(|entry| {
                        let (id_str, json_str) = entry.split_once(':')?;
                        let id = id_str.parse::<i64>().ok()?;
                        if before_id.map(|c| id < c).unwrap_or(true) {
                            Some(json_str.to_string())
                        } else {
                            None
                        }
                    })
                    .take(limit)
                    .collect();

                if slice.len() == limit {
                    Some(slice)
                } else {
                    None
                }
            }
            CacheBackend::Memory(mem) => {
                let g = mem.read().await;
                let q = g.timelines.get(&space_id)?;
                if q.is_empty() {
                    return None;
                }
                let slice: Vec<String> = q
                    .iter()
                    .filter(|(id, _)| before_id.map(|c| *id < c).unwrap_or(true))
                    .take(limit)
                    .map(|(_, raw)| raw.clone())
                    .collect();

                if slice.len() == limit {
                    Some(slice)
                } else {
                    None
                }
            }
        }
    }
}
