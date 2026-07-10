//! The SQLite snapshot store. Opens a database, runs migrations, and (from D.2 on)
//! owns the write path whose one invariant is that a failed fetch never lands.
//!
//! Queries go through sqlx's `query!` macros, which type-check against the schema at
//! COMPILE time — a query naming a column that a migration renamed or dropped is a build
//! error, not a runtime surprise. `build.rs` migrates a scratch schema and points
//! `DATABASE_URL` at it, so that check needs no committed query cache.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use crate::config::BoardConfig;
use crate::model::{Ats, BoardId, ObitKind, Posting, ReqId};

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

/// The delta for one board: what a fetch produced, or what [`Store::diff_board`]
/// reconstructs from the last two snapshots. Obit-suppressed rows are excluded (D.4).
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct BoardDiff {
    /// req_ids first seen at the latest snapshot.
    pub new: Vec<ReqId>,
    /// req_ids whose material content changed, with the fields that moved.
    pub changed: Vec<ChangedPosting>,
    /// req_ids present in the previous snapshot but absent from the latest fetch.
    pub dead: Vec<ReqId>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ChangedPosting {
    pub req_id: ReqId,
    pub changed_fields: Vec<String>,
}

/// One row of the obit ledger.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ObitRecord {
    pub board_id: BoardId,
    pub key: String,
    pub kind: ObitKind,
    pub reason: String,
    pub marked_at: String,
}

/// A raw response to record in the capture ledger. Borrows everything so the (possibly
/// large) body is never copied on the way in.
pub struct RawCapture<'a> {
    pub board_id: &'a BoardId,
    pub ats: Ats,
    pub url: &'a str,
    pub method: &'a str,
    pub request_body: Option<&'a str>,
    pub status: u16,
    pub body: &'a str,
}

/// A capture ledger row WITHOUT its body — the metadata `list_captures` returns for
/// audit, so listing the ledger never drags hundreds of KB of bodies into memory.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CaptureMeta {
    pub id: i64,
    pub board_id: BoardId,
    pub ats: Ats,
    pub url: String,
    pub method: String,
    pub status: i64,
    pub captured_at: String,
    /// Body length in bytes — the size of the sample without carrying the sample itself.
    pub bytes: i64,
}

/// A capture ledger row WITH its body — what `dump_captures` reads to write sample files.
#[derive(Debug)]
pub struct CaptureRecord {
    pub id: i64,
    pub board_id: BoardId,
    pub ats: Ats,
    pub url: String,
    pub captured_at: String,
    pub body: String,
}

/// The stored material fields of a posting, as strings, for change detection. Comparing
/// serialized forms is exact (the encoding is canonical) and skips a round-trip through
/// the typed values.
struct StoredMaterial {
    content_hash: String,
    title: String,
    locations: String,
    workplace_type: String,
    comp: String,
}

/// Which material fields moved between the stored posting and the incoming one. `url`,
/// `posted_at` and `updated_at` are deliberately NOT here — they don't feed content_hash.
/// If the hash moved but no named field did, the description changed (it's in the hash
/// but not stored field-by-field), so it's named by elimination.
fn changed_fields(old: &StoredMaterial, new: &Posting) -> Vec<String> {
    let mut fields = Vec::new();
    if old.title != new.title {
        fields.push("title".to_owned());
    }
    if old.locations != json(&new.locations) {
        fields.push("locations".to_owned());
    }
    if old.workplace_type != json(&new.workplace_type) {
        fields.push("workplace_type".to_owned());
    }
    if old.comp != json(&new.comp) {
        fields.push("comp".to_owned());
    }
    if fields.is_empty() && old.content_hash != new.content_hash.to_hex() {
        fields.push("description".to_owned());
    }
    fields
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

    /// The time of the most recent snapshot for a board (RFC3339), or `None` if it's never
    /// been fetched successfully.
    pub async fn last_snapshot_at(&self, board_id: &BoardId) -> Result<Option<String>, StoreError> {
        let board = board_id.as_str();
        // MAX can be NULL (no snapshots), and sqlx can't infer a type for it — override to
        // a nullable String.
        let row = sqlx::query!(
            r#"SELECT MAX(taken_at) AS "taken?: String" FROM snapshots WHERE board_id = ?1"#,
            board,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::Write)?;
        Ok(row.taken)
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

        // A board that HAD postings and is now empty is the silent-migration shape: Lever
        // and Workable return 200 + [] when a company moves off, indistinguishable from a
        // genuinely-empty board. We still record it (an empty board is legitimate), but
        // warn — the drop from non-empty to empty is exactly the maintenance-mode signal
        // worth a human's eyes.
        if count == 0 {
            let prev = sqlx::query!(
                "SELECT posting_count FROM snapshots WHERE board_id = ?1 ORDER BY id DESC LIMIT 1",
                board,
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(StoreError::Write)?;
            if let Some(row) = prev {
                if row.posting_count > 0 {
                    tracing::warn!(
                        board = %board_id,
                        previous = row.posting_count,
                        "board returned zero postings after a non-empty snapshot — possible migration off this ATS"
                    );
                }
            }
        }

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

        // The previous fetch's state, read BEFORE the upsert overwrites it — this is what
        // NEW vs CHANGED is measured against.
        let stored: HashMap<String, StoredMaterial> = sqlx::query!(
            "SELECT req_id, content_hash, title, locations, workplace_type, comp
             FROM postings WHERE board_id = ?1",
            board,
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StoreError::Write)?
        .into_iter()
        .map(|r| {
            (
                r.req_id,
                StoredMaterial {
                    content_hash: r.content_hash,
                    title: r.title,
                    locations: r.locations,
                    workplace_type: r.workplace_type,
                    comp: r.comp,
                },
            )
        })
        .collect();

        for posting in postings {
            let req_id = posting.req_id.as_str();
            let locations = json(&posting.locations);
            let workplace_type = json(&posting.workplace_type);
            let comp = json(&posting.comp);
            let content_hash = posting.content_hash.to_hex();
            let posted_at = posting.posted_at.map(|d| d.to_rfc3339());
            let updated_at = posting.updated_at.map(|d| d.to_rfc3339());

            // A req we've seen before whose hash moved is CHANGED — record one version row
            // with the field list. A NEW req (not in `stored`) gets no version row; its
            // first_seen in `postings` is what marks it new. An unchanged req: nothing.
            if let Some(old) = stored.get(req_id) {
                if old.content_hash != content_hash {
                    let fields = json(&changed_fields(old, posting));
                    sqlx::query!(
                        "INSERT INTO posting_versions
                             (board_id, req_id, seen_at, snapshot_id, changed_fields, content_hash)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        board,
                        req_id,
                        taken,
                        snapshot_id,
                        fields,
                        content_hash,
                    )
                    .execute(&mut *tx)
                    .await
                    .map_err(StoreError::Write)?;
                }
            }

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

    /// Reconstruct the delta of the latest snapshot vs the one before it, from stored
    /// data alone — this is `diff_boards` for one board, and it does NOT fetch.
    ///
    /// - NEW  = postings whose first_seen is the latest snapshot's time.
    /// - CHANGED = the version rows written at the latest snapshot, with their field lists.
    /// - DEAD = postings whose last_seen is the PREVIOUS snapshot's time — alive up to the
    ///   last fetch, untouched by the latest, so they've vanished from the feed. This is
    ///   only about that one transition: a posting that died several fetches ago has an
    ///   older last_seen and is not re-reported.
    ///
    /// With zero snapshots the diff is empty; with exactly one, everything is NEW and
    /// nothing is DEAD (there's no prior to have vanished from).
    pub async fn diff_board(&self, board_id: &BoardId) -> Result<BoardDiff, StoreError> {
        let board = board_id.as_str();
        let snaps = sqlx::query!(
            "SELECT taken_at FROM snapshots WHERE board_id = ?1 ORDER BY id DESC LIMIT 2",
            board,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?;

        let Some(latest) = snaps.first() else {
            return Ok(BoardDiff::default());
        };
        let latest_taken = latest.taken_at.clone();
        let prev_taken = snaps.get(1).map(|s| s.taken_at.clone());

        // NEW excludes anything in the obit ledger — a ghost aggregator listing or a
        // rejected req must not re-surface as new.
        let new = sqlx::query!(
            "SELECT req_id FROM postings
             WHERE board_id = ?1 AND first_seen = ?2
               AND req_id NOT IN (SELECT key FROM obits WHERE board_id = ?1)
             ORDER BY req_id",
            board,
            latest_taken,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?
        .into_iter()
        .map(|r| ReqId::new(r.req_id))
        .collect();

        let changed = sqlx::query!(
            "SELECT v.req_id AS req_id, v.changed_fields AS changed_fields
             FROM posting_versions v
             JOIN snapshots s ON s.id = v.snapshot_id
             WHERE s.board_id = ?1 AND s.taken_at = ?2
             ORDER BY v.req_id",
            board,
            latest_taken,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?
        .into_iter()
        .map(|r| ChangedPosting {
            req_id: ReqId::new(r.req_id),
            changed_fields: serde_json::from_str(&r.changed_fields)
                .expect("changed_fields is JSON we wrote"),
        })
        .collect();

        let dead = match prev_taken {
            None => Vec::new(),
            Some(prev) => sqlx::query!(
                "SELECT req_id FROM postings WHERE board_id = ?1 AND last_seen = ?2
                 ORDER BY req_id",
                board,
                prev,
            )
            .fetch_all(&self.pool)
            .await
            .map_err(StoreError::Write)?
            .into_iter()
            .map(|r| ReqId::new(r.req_id))
            .collect(),
        };

        Ok(BoardDiff { new, changed, dead })
    }

    /// Mark a posting (by req_id) or a freeform key dead, rejected, out-of-scope or a
    /// ghost, so it stops surfacing as NEW. Re-marking the same key updates it. `marked_at`
    /// is injected, like every other timestamp — the store reads no clock.
    pub async fn mark_obit(
        &self,
        board_id: &BoardId,
        key: &str,
        kind: ObitKind,
        reason: &str,
        marked_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let board = board_id.as_str();
        let kind = json(&kind);
        let marked = marked_at.to_rfc3339();
        sqlx::query!(
            "INSERT INTO obits (board_id, key, kind, reason, marked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (board_id, key) DO UPDATE SET
                 kind = excluded.kind,
                 reason = excluded.reason,
                 marked_at = excluded.marked_at",
            board,
            key,
            kind,
            reason,
            marked,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::Write)?;
        Ok(())
    }

    /// The obit ledger, for audit. Scoped to one board, or all boards if `board_id` is
    /// `None`.
    pub async fn list_obits(
        &self,
        board_id: Option<&BoardId>,
    ) -> Result<Vec<ObitRecord>, StoreError> {
        let filter = board_id.map(BoardId::as_str);
        let rows = sqlx::query!(
            "SELECT board_id, key, kind, reason, marked_at FROM obits
             WHERE ?1 IS NULL OR board_id = ?1
             ORDER BY marked_at",
            filter,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?;

        Ok(rows
            .into_iter()
            .map(|r| ObitRecord {
                board_id: BoardId::new(r.board_id),
                key: r.key,
                kind: serde_json::from_str(&r.kind).expect("obit kind is JSON we wrote"),
                reason: r.reason,
                marked_at: r.marked_at,
            })
            .collect())
    }

    /// Record a raw response and, in the same call, purge anything older than the
    /// retention window — both stamped at the caller-supplied `captured_at`, because the
    /// store reads no clock. Best-effort by contract: the HTTP layer treats a failure
    /// here as non-fatal, so a capture write never breaks the fetch it rode in on.
    pub async fn record_capture(
        &self,
        capture: &RawCapture<'_>,
        captured_at: DateTime<Utc>,
        retain_days: u32,
    ) -> Result<(), StoreError> {
        let board = capture.board_id.as_str();
        let ats = json(&capture.ats);
        let status = i64::from(capture.status);
        let captured = captured_at.to_rfc3339();
        sqlx::query!(
            "INSERT INTO raw_captures
                 (board_id, ats, url, method, request_body, status, captured_at, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            board,
            ats,
            capture.url,
            capture.method,
            capture.request_body,
            status,
            captured,
            capture.body,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::Write)?;

        // Sweep everything past the window. RFC3339 in a single (UTC) offset sorts
        // lexicographically, which is why a string comparison is a valid time comparison.
        let cutoff = (captured_at - chrono::Duration::days(i64::from(retain_days))).to_rfc3339();
        sqlx::query!("DELETE FROM raw_captures WHERE captured_at < ?1", cutoff)
            .execute(&self.pool)
            .await
            .map_err(StoreError::Write)?;
        Ok(())
    }

    /// The capture ledger, newest first, metadata only (no bodies). Scoped to one board or
    /// all boards, capped at `limit`.
    pub async fn list_captures(
        &self,
        board_id: Option<&BoardId>,
        limit: i64,
    ) -> Result<Vec<CaptureMeta>, StoreError> {
        let filter = board_id.map(BoardId::as_str);
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", board_id, ats, url, method, status,
                      captured_at, LENGTH(body) AS "bytes!: i64"
               FROM raw_captures
               WHERE ?1 IS NULL OR board_id = ?1
               ORDER BY captured_at DESC, id DESC
               LIMIT ?2"#,
            filter,
            limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?;
        Ok(rows
            .into_iter()
            .map(|r| CaptureMeta {
                id: r.id,
                board_id: BoardId::new(r.board_id),
                ats: serde_json::from_str(&r.ats).expect("ats is JSON we wrote"),
                url: r.url,
                method: r.method,
                status: r.status,
                captured_at: r.captured_at,
                bytes: r.bytes,
            })
            .collect())
    }

    /// The capture ledger WITH bodies, newest first — what `dump_captures` reads to write
    /// sample files. Same scoping and `limit` as [`list_captures`](Self::list_captures).
    pub async fn dump_captures(
        &self,
        board_id: Option<&BoardId>,
        limit: i64,
    ) -> Result<Vec<CaptureRecord>, StoreError> {
        let filter = board_id.map(BoardId::as_str);
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", board_id, ats, url, captured_at, body
               FROM raw_captures
               WHERE ?1 IS NULL OR board_id = ?1
               ORDER BY captured_at DESC, id DESC
               LIMIT ?2"#,
            filter,
            limit,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::Write)?;
        Ok(rows
            .into_iter()
            .map(|r| CaptureRecord {
                id: r.id,
                board_id: BoardId::new(r.board_id),
                ats: serde_json::from_str(&r.ats).expect("ats is JSON we wrote"),
                url: r.url,
                captured_at: r.captured_at,
                body: r.body,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ats, AtsToken, Comp, Equity, ReqId, WorkplaceType, content_hash};

    fn board() -> BoardConfig {
        BoardConfig {
            id: BoardId::new("gitlab"),
            ats: Ats::Greenhouse,
            token: AtsToken::new("gitlab"),
            site: None,
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
            equity: Equity::None,
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
                Equity::None,
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

    async fn seeded() -> (Store, BoardId) {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert_board(&board()).await.unwrap();
        (store, BoardId::new("gitlab"))
    }

    #[tokio::test]
    async fn first_snapshot_is_all_new() {
        let (store, id) = seeded().await;
        store
            .record_snapshot(
                &id,
                day(0),
                &[posting("1", "Engineer"), posting("2", "Designer")],
            )
            .await
            .unwrap();
        let diff = store.diff_board(&id).await.unwrap();
        assert_eq!(diff.new, vec![ReqId::new("1"), ReqId::new("2")]);
        assert!(diff.changed.is_empty());
        assert!(diff.dead.is_empty());
    }

    #[tokio::test]
    async fn a_retitled_posting_is_changed_with_the_field_named() {
        let (store, id) = seeded().await;
        store
            .record_snapshot(&id, day(0), &[posting("1", "Engineer")])
            .await
            .unwrap();
        // Same req, new title → CHANGED, and "title" is the field that moved.
        store
            .record_snapshot(&id, day(1), &[posting("1", "Senior Engineer")])
            .await
            .unwrap();

        let diff = store.diff_board(&id).await.unwrap();
        assert!(diff.new.is_empty(), "not new on the second sighting");
        assert_eq!(diff.dead, Vec::<ReqId>::new());
        assert_eq!(
            diff.changed,
            vec![ChangedPosting {
                req_id: ReqId::new("1"),
                changed_fields: vec!["title".to_owned()],
            }]
        );
    }

    #[tokio::test]
    async fn a_vanished_posting_is_dead() {
        let (store, id) = seeded().await;
        store
            .record_snapshot(
                &id,
                day(0),
                &[posting("1", "Engineer"), posting("2", "Designer")],
            )
            .await
            .unwrap();
        // Second fetch drops req 2.
        store
            .record_snapshot(&id, day(1), &[posting("1", "Engineer")])
            .await
            .unwrap();

        let diff = store.diff_board(&id).await.unwrap();
        assert_eq!(diff.dead, vec![ReqId::new("2")]);
        assert!(diff.new.is_empty());
        assert!(
            diff.changed.is_empty(),
            "req 1 was identical, so not changed"
        );
    }

    #[tokio::test]
    async fn an_identical_refetch_shows_no_deltas() {
        let (store, id) = seeded().await;
        store
            .record_snapshot(&id, day(0), &[posting("1", "Engineer")])
            .await
            .unwrap();
        store
            .record_snapshot(&id, day(1), &[posting("1", "Engineer")])
            .await
            .unwrap();
        let diff = store.diff_board(&id).await.unwrap();
        assert_eq!(diff, BoardDiff::default());
    }

    #[tokio::test]
    async fn obit_round_trips_and_re_marking_updates() {
        let (store, id) = seeded().await;
        store
            .mark_obit(&id, "1", ObitKind::OutOfScope, "not my stack", day(0))
            .await
            .unwrap();
        let obits = store.list_obits(Some(&id)).await.unwrap();
        assert_eq!(obits.len(), 1);
        assert_eq!(obits[0].kind, ObitKind::OutOfScope);
        assert_eq!(obits[0].key, "1");

        // Re-marking the same key updates rather than duplicating.
        store
            .mark_obit(&id, "1", ObitKind::Rejected, "applied, closed", day(1))
            .await
            .unwrap();
        let obits = store.list_obits(Some(&id)).await.unwrap();
        assert_eq!(obits.len(), 1);
        assert_eq!(obits[0].kind, ObitKind::Rejected);
        assert_eq!(obits[0].reason, "applied, closed");
    }

    #[tokio::test]
    async fn an_obit_suppresses_a_new_result() {
        let (store, id) = seeded().await;
        // A ghost that never existed on a primary source, marked before the fetch.
        store
            .mark_obit(
                &id,
                "ghost-1",
                ObitKind::Ghost,
                "aggregator phantom",
                day(0),
            )
            .await
            .unwrap();
        store
            .record_snapshot(
                &id,
                day(1),
                &[
                    posting("ghost-1", "Phantom Role"),
                    posting("real-1", "Real Role"),
                ],
            )
            .await
            .unwrap();

        let diff = store.diff_board(&id).await.unwrap();
        // Only the real posting is NEW; the ghost is suppressed.
        assert_eq!(diff.new, vec![ReqId::new("real-1")]);
    }

    fn cap<'a>(board: &'a BoardId, body: &'a str) -> RawCapture<'a> {
        RawCapture {
            board_id: board,
            ats: Ats::Greenhouse,
            url: "https://boards-api.greenhouse.io/x/jobs",
            method: "GET",
            request_body: None,
            status: 200,
            body,
        }
    }

    #[tokio::test]
    async fn capture_records_and_lists_metadata_without_the_body() {
        let store = Store::open_in_memory().await.unwrap();
        let id = BoardId::new("gitlab");
        store
            .record_capture(&cap(&id, r#"{"jobs":[]}"#), day(0), 7)
            .await
            .unwrap();

        let metas = store.list_captures(Some(&id), 10).await.unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].ats, Ats::Greenhouse);
        assert_eq!(metas[0].status, 200);
        // The size of the sample travels; the sample itself does not.
        assert_eq!(metas[0].bytes, r#"{"jobs":[]}"#.len() as i64);
    }

    #[tokio::test]
    async fn capture_purges_everything_past_the_retention_window() {
        let store = Store::open_in_memory().await.unwrap();
        let id = BoardId::new("gitlab");
        // An old capture, then a new one ten days later under a 7-day window: recording
        // the new one sweeps the old, so the ledger self-bounds without a cron.
        store
            .record_capture(&cap(&id, "old"), day(0), 7)
            .await
            .unwrap();
        store
            .record_capture(&cap(&id, "new"), day(10), 7)
            .await
            .unwrap();

        let metas = store.list_captures(None, 100).await.unwrap();
        assert_eq!(
            metas.len(),
            1,
            "day-0 capture is past the 7-day window at day 10"
        );
        let dumped = store.dump_captures(None, 100).await.unwrap();
        assert_eq!(dumped.len(), 1);
        assert_eq!(dumped[0].body, "new");
    }

    #[tokio::test]
    async fn capture_list_is_scoped_by_board() {
        let store = Store::open_in_memory().await.unwrap();
        let a = BoardId::new("alpha");
        let b = BoardId::new("beta");
        store
            .record_capture(&cap(&a, "a"), day(0), 7)
            .await
            .unwrap();
        store
            .record_capture(&cap(&b, "b"), day(0), 7)
            .await
            .unwrap();
        assert_eq!(store.list_captures(Some(&a), 100).await.unwrap().len(), 1);
        assert_eq!(store.list_captures(None, 100).await.unwrap().len(), 2);
    }
}
