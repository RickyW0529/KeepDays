use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::{
    agent::AgentRuntime,
    config::AppConfig,
    infra::{cache::CacheService, db::DbPools, dict::DictRegistry, s3::S3Service},
    realtime::hub::RealtimeHub,
    snowflake::SnowflakeGenerator,
};

pub struct InnerState {
    pub cfg: AppConfig,
    pub db: DbPools,
    pub dict: DictRegistry,
    pub cache: CacheService,
    pub s3: S3Service,
    pub snowflake: SnowflakeGenerator,
    pub hub: RealtimeHub,
    pub agent: AgentRuntime,
    /// Caps concurrent Argon2id allocations so burst logins can NEVER OOM a 512MB RAM server!
    pub argon2_sem: Semaphore,
    /// Domain event bus for asynchronous Pet Economy / Timeline hooks
    pub pet_reward_tx: tokio::sync::mpsc::UnboundedSender<PetLifeEvent>,
}

pub type AppState = Arc<InnerState>;

/// Domain events from Space actions that organically nourish the Shared Space Pet
#[derive(Debug, Clone)]
pub struct PetLifeEvent {
    pub space_id: i64,
    pub actor_id: i64,
    pub action: PetLifeTrigger,
}

#[derive(Debug, Clone, Copy)]
pub enum PetLifeTrigger {
    PhotoUploaded,
    PhotoCommented,
    MenuPinned,
    TodoCompleted,
}
