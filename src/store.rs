//! The SQLite snapshot store. Opens a database, runs migrations, and (from D.2 on)
//! owns the write path whose one invariant is that a failed fetch never lands.
//!
//! Queries go through sqlx's `query!` macros, which type-check against the schema at
//! COMPILE time — a query naming a column that a migration renamed or dropped is a build
//! error, not a runtime surprise. `build.rs` migrates a scratch schema and points
//! `DATABASE_URL` at it, so that check needs no committed query cache.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use crate::config::BoardConfig;
use crate::model::{BoardId, Posting};

/// Things that go wrong opening, migrating, or writing the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("opening the store")]
    Open(#[source] sqlx::Error),
    #[error("migrating the store")]
    Migrate(#[source] sqlx::migrate::MigrateError),
    #[error("writing the store")]
    Write(#[source] sqlx::Error),
}

/// JSON-encode a model value for a TEXT column. The Rust type stays the single source of
/// truth; SQLite just holds its serialization.
fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("model types serialize")
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

    /// Number of configured boards mirrored into the store.
    pub async fn board_count(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!("SELECT COUNT(*) AS count FROM boards")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.count)
    }

    /// Mirror a config board into the store so snapshots and postings have something to
    /// reference. The config file stays the source of truth; this refreshes the mirror.
    pub async fn upsert_board(&self, board: &BoardConfig) -> Result<(), StoreError> {
        let id = board.id.as_str();
        let ats = json(&board.ats);
        let token = board.token.as_str();
        sqlx::query!(
            "INSERT INTO boards (id, ats, token, comp_site_only, updated_at_unreliable)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 ats = excluded.ats,
                 token = excluded.token,
                 comp_site_only = excluded.comp_site_only,
                 updated_at_unreliable = excluded.updated_at_unreliable",
            id,
            ats,
            token,
            board.comp_site_only,
            board.updated_at_unreliable,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::Write)?;
        Ok(())
    }

    /// Record a snapshot and the current state of its postings, at the caller-supplied
    /// `taken_at`. Time is a PARAMETER, never read here — the store is deterministic, and
    /// the one clock reader lives up in the MCP handler. That injection is what lets D.5
    /// assert first_seen/last_seen across a day boundary.
    ///
    /// The whole thing runs in one transaction, so a mid-write failure rolls back rather
    /// than leaving a partial snapshot. The *other* half of the invariant — that a failed
    /// FETCH never gets here at all — is the caller's: it calls this only on `Ok`, never
    /// on an `AdapterError`, so a board in maintenance mode is never recorded as empty.
    /// Returns the new snapshot's id.
    pub async fn record_snapshot(
        &self,
        board_id: &BoardId,
        taken_at: DateTime<Utc>,
        postings: &[Posting],
    ) -> Result<i64, StoreError> {
        let board = board_id.as_str();
        let taken = taken_at.to_rfc3339();
        let count = postings.len() as i64;

        let mut tx = self.pool.begin().await.map_err(StoreError::Write)?;

        let snapshot_id = sqlx::query!(
            "INSERT INTO snapshots (board_id, taken_at, posting_count) VALUES (?1, ?2, ?3)
             RETURNING id",
            board,
            taken,
            count,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(StoreError::Write)?
        .id
        .expect("INSERT ... RETURNING id yields the new snapshot id");

        for posting in postings {
            let req_id = posting.req_id.as_str();
            let locations = json(&posting.locations);
            let workplace_type = json(&posting.workplace_type);
            let comp = json(&posting.comp);
            let content_hash = posting.content_hash.to_hex();
            let posted_at = posting.posted_at.map(|d| d.to_rfc3339());
            let updated_at = posting.updated_at.map(|d| d.to_rfc3339());

            // first_seen is set only on insert; on conflict it's preserved and last_seen
            // moves forward — so "when did we first see this req" survives forever.
            sqlx::query!(
                "INSERT INTO postings (
                     board_id, req_id, first_seen, last_seen, title, url, locations,
                     workplace_type, remote_scope, comp, posted_at, updated_at,
                     updated_at_unreliable, department, employment_type, content_hash)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT (board_id, req_id) DO UPDATE SET
                     last_seen = excluded.last_seen,
                     title = excluded.title,
                     url = excluded.url,
                     locations = excluded.locations,
                     workplace_type = excluded.workplace_type,
                     remote_scope = excluded.remote_scope,
                     comp = excluded.comp,
                     posted_at = excluded.posted_at,
                     updated_at = excluded.updated_at,
                     updated_at_unreliable = excluded.updated_at_unreliable,
                     department = excluded.department,
                     employment_type = excluded.employment_type,
                     content_hash = excluded.content_hash",
                board,
                req_id,
                taken,
                posting.title,
                posting.url,
                locations,
                workplace_type,
                posting.remote_scope,
                comp,
                posted_at,
                updated_at,
                posting.updated_at_unreliable,
                posting.department,
                posting.employment_type,
                content_hash,
            )
            .execute(&mut *tx)
            .await
            .map_err(StoreError::Write)?;
        }

        tx.commit().await.map_err(StoreError::Write)?;
        Ok(snapshot_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, Comp, ReqId, WorkplaceType, content_hash};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("gitlab"),
            ats: Ats::Greenhouse,
            token: AtsToken::new("gitlab"),
            comp_site_only: false,
            updated_at_unreliable: false,
        }
    }

    fn posting(req: &str, title: &str) -> Posting {
        let comp = Comp::None;
        Posting {
            ats: Ats::Greenhouse,
            board_id: BoardId::new("gitlab"),
            req_id: ReqId::new(req),
            title: title.to_owned(),
            url: format!("https://example.test/{req}"),
            locations: vec!["Remote".to_owned()],
            workplace_type: WorkplaceType::Remote,
            remote_scope: None,
            comp: comp.clone(),
            posted_at: None,
            updated_at: None,
            updated_at_unreliable: false,
            department: None,
            employment_type: None,
            content_hash: content_hash(
                title,
                &["Remote".to_owned()],
                WorkplaceType::Remote,
                &comp,
                "",
            ),
        }
    }

    fn day(n: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + n * 86_400, 0).unwrap()
    }

    #[tokio::test]
    async fn migrations_apply_and_the_query_pipeline_works() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(store.board_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn record_snapshot_writes_the_snapshot_and_current_postings() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert_board(&board()).await.unwrap();
        assert_eq!(store.board_count().await.unwrap(), 1);

        let id = BoardId::new("gitlab");
        let snap = store
            .record_snapshot(
                &id,
                day(0),
                &[posting("1", "Engineer"), posting("2", "Designer")],
            )
            .await
            .unwrap();
        assert!(snap > 0);

        let snaps = sqlx::query!("SELECT COUNT(*) AS c FROM snapshots")
            .fetch_one(&store.pool)
            .await
            .unwrap()
            .c;
        let posts = sqlx::query!("SELECT COUNT(*) AS c FROM postings")
            .fetch_one(&store.pool)
            .await
            .unwrap()
            .c;
        let declared = sqlx::query!("SELECT posting_count FROM snapshots WHERE id = ?1", snap)
            .fetch_one(&store.pool)
            .await
            .unwrap()
            .posting_count;
        assert_eq!(snaps, 1);
        assert_eq!(posts, 2);
        assert_eq!(declared, 2);
    }

    #[tokio::test]
    async fn first_seen_is_pinned_while_last_seen_moves_with_the_clock() {
        // This is exactly what injecting taken_at buys: two snapshots a day apart, and
        // first_seen holds day 0 while last_seen advances to day 1. Unprovable if the
        // store read its own clock.
        let store = Store::open_in_memory().await.unwrap();
        store.upsert_board(&board()).await.unwrap();
        let id = BoardId::new("gitlab");

        store
            .record_snapshot(&id, day(0), &[posting("1", "Engineer")])
            .await
            .unwrap();
        store
            .record_snapshot(&id, day(1), &[posting("1", "Engineer")])
            .await
            .unwrap();

        let row = sqlx::query!("SELECT first_seen, last_seen FROM postings WHERE req_id = '1'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(row.first_seen, day(0).to_rfc3339());
        assert_eq!(row.last_seen, day(1).to_rfc3339());
    }

    #[tokio::test]
    async fn an_empty_successful_fetch_records_an_empty_snapshot() {
        // A board that legitimately has zero postings records a count-0 snapshot. The
        // dangerous case — an empty body from a FAILED fetch — never reaches here,
        // because the caller only calls this on Ok (see the doc comment).
        let store = Store::open_in_memory().await.unwrap();
        store.upsert_board(&board()).await.unwrap();
        let snap = store
            .record_snapshot(&BoardId::new("gitlab"), day(0), &[])
            .await
            .unwrap();
        let row = sqlx::query!("SELECT posting_count FROM snapshots WHERE id = ?1", snap)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(row.posting_count, 0);
    }
}
