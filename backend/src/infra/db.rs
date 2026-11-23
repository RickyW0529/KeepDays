use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use std::{path::Path, str::FromStr, time::Duration};

#[derive(Clone)]
pub struct DbPools {
    /// Parallel lock-free WAL readers
    pub read: SqlitePool,
    /// Single serialized writer eliminating SQLITE_BUSY completely
    pub write: SqlitePool,
}

impl DbPools {
    pub async fn connect_and_migrate(
        database_url: &str,
        low_mem_mode: bool,
    ) -> Result<Self, sqlx::Error> {
        let is_memory = database_url.contains(":memory:") || database_url.contains("mode=memory");

        if !is_memory {
            if let Some(raw_path) = database_url.strip_prefix("sqlite://") {
                let file_part = raw_path.split('?').next().unwrap_or(raw_path);
                if let Some(parent) = Path::new(file_part).parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
            }
        }

        // In 512MB mode: cap each pool's page cache at 4MB (~8MB total) & cap WAL file at 16MB on 10GB SSD
        let (cache_kb_pragma, read_conns) = if low_mem_mode {
            ("-4000", 4)
        } else {
            ("-32000", 16)
        };

        let write_opts = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .journal_mode(if is_memory {
                SqliteJournalMode::Memory
            } else {
                SqliteJournalMode::Wal
            })
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true)
            .pragma("cache_size", cache_kb_pragma)
            .pragma("temp_store", "MEMORY")
            .pragma("journal_size_limit", "16777216");

        let write = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(write_opts)
            .await?;

        // Execute Snowflake dimensional + hybrid document + Pet Agent Kernel DDL migrations
        let init_sql = include_str!("../../migrations/0001_init_snowflake_schema.sql");
        sqlx::raw_sql(init_sql).execute(&write).await?;
        let agent_sql = include_str!("../../migrations/0002_pet_agent_memory_and_sessions.sql");
        sqlx::raw_sql(agent_sql).execute(&write).await?;
        ensure_additive_columns(&write).await?;

        let read = if is_memory {
            write.clone()
        } else {
            let read_opts = SqliteConnectOptions::from_str(database_url)?
                .read_only(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5))
                .foreign_keys(true)
                .pragma("cache_size", cache_kb_pragma)
                .pragma("temp_store", "MEMORY");

            SqlitePoolOptions::new()
                .max_connections(read_conns)
                .idle_timeout(Duration::from_secs(120))
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(read_opts)
                .await?
        };

        Ok(Self { read, write })
    }

    /// Reclaims `.db-wal` disk space on small 10GB SSDs without blocking active readers
    pub async fn passive_wal_checkpoint(&self) {
        let _ = sqlx::query("PRAGMA wal_checkpoint(PASSIVE);")
            .execute(&self.write)
            .await;
    }
}

async fn ensure_additive_columns(write: &SqlitePool) -> Result<(), sqlx::Error> {
    add_column_if_missing(
        write,
        "agent_sessions",
        "compacted_turn_count",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    add_column_if_missing(
        write,
        "agent_stm_segments",
        "promote_attempts",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    Ok(())
}

async fn add_column_if_missing(
    write: &SqlitePool,
    table: &str,
    column: &str,
    col_def: &str,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(write)
        .await?;
    let exists = rows
        .iter()
        .any(|r| r.get::<String, _>("name").eq_ignore_ascii_case(column));
    if !exists {
        sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} {col_def}"))
            .execute(write)
            .await?;
    }
    Ok(())
}
