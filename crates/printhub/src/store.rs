//! SQLite persistence: one file under `DATA_DIR`, schema changes as embedded migrations.

use std::{path::Path, str::FromStr, time::Duration};

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

pub type Db = SqlitePool;

pub const DB_FILE: &str = "printhub.db";

pub async fn open(data_dir: &Path) -> anyhow::Result<Db> {
    tokio::fs::create_dir_all(data_dir)
        .await
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let options = SqliteConnectOptions::new()
        .filename(data_dir.join(DB_FILE))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .context("opening the database")?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Every connection to `:memory:` is a separate empty database, so the pool holds exactly one
/// connection and never retires it.
pub async fn open_in_memory() -> anyhow::Result<Db> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &Db) -> anyhow::Result<()> {
    sqlx::migrate!()
        .run(pool)
        .await
        .context("applying database migrations")
}

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}
