//! The SQLite snapshot store. Opens a database, runs migrations, and (from D.2 on)
//! owns the write path whose one invariant is that a failed fetch never lands.
//!
//! Queries go through sqlx's `query!` macros, which type-check against the schema at
//! COMPILE time — a query naming a column that a migration renamed or dropped is a build
//! error, not a runtime surprise. `build.rs` migrates a scratch schema and points
//! `DATABASE_URL` at it, so that check needs no committed query cache.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

/// Things that go wrong opening or migrating the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("opening the store")]
    Open(#[source] sqlx::Error),
    #[error("migrating the store")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

/// A handle to the snapshot database.
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open the store at `path`, creating it if absent and bringing it up to the current
    /// schema. Foreign keys are enforced on every connection.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        Self::from_options(options).await
    }

    /// Open an in-memory store — used by tests, and proof the migrations apply cleanly.
    pub async fn open_in_memory() -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        Self::from_options(options).await
    }

    async fn from_options(options: SqliteConnectOptions) -> Result<Self, StoreError> {
        // SQLite is single-writer, and an in-memory pool would otherwise hand each
        // connection its OWN separate database — so a single connection is both correct
        // and the safe choice against lock contention.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(StoreError::Open)?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(StoreError::Migrate)?;
        Ok(Self { pool })
    }

    /// Number of configured boards mirrored into the store. Trivial today — its job is to
    /// prove the compile-time `query!` pipeline works end to end against the schema.
    pub async fn board_count(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!("SELECT COUNT(*) AS count FROM boards")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_and_the_query_pipeline_works() {
        let store = Store::open_in_memory().await.unwrap();
        // A freshly-migrated store has no boards, and the compile-time-checked query runs.
        assert_eq!(store.board_count().await.unwrap(), 0);
    }
}
