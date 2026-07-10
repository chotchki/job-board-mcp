-- Initial schema. Tables mirror SPEC's Store section.
--
-- Timestamps are RFC3339 TEXT. `comp` and `locations` and `changed_fields` are JSON
-- TEXT (serde_json of the model types) — SQLite has no array/struct, and JSON keeps a
-- single source of truth (the Rust type) rather than shredding a Comp across columns.
-- content_hash is lowercase hex TEXT, so the store is inspectable from the sqlite3 CLI.

-- Mirror of the config's boards, so snapshots and postings have something to reference.
-- The config file remains the source of truth; this is refreshed from it on each run.
CREATE TABLE boards (
    id                    TEXT PRIMARY KEY NOT NULL,
    ats                   TEXT NOT NULL,
    token                 TEXT NOT NULL,
    comp_site_only        INTEGER NOT NULL DEFAULT 0,
    updated_at_unreliable INTEGER NOT NULL DEFAULT 0
) STRICT;

-- One row per successful, well-formed fetch. A failed or partial fetch NEVER writes
-- one — that invariant (enforced in D.2) is what protects every diff.
CREATE TABLE snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    board_id      TEXT NOT NULL REFERENCES boards(id),
    taken_at      TEXT NOT NULL,
    posting_count INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_snapshots_board ON snapshots(board_id, taken_at);

-- Current state of every posting ever seen, keyed by its (board_id, req_id) identity.
CREATE TABLE postings (
    board_id              TEXT NOT NULL REFERENCES boards(id),
    req_id                TEXT NOT NULL,
    first_seen            TEXT NOT NULL,
    last_seen             TEXT NOT NULL,
    title                 TEXT NOT NULL,
    url                   TEXT NOT NULL,
    locations             TEXT NOT NULL,   -- JSON array of strings
    workplace_type        TEXT NOT NULL,
    remote_scope          TEXT,
    comp                  TEXT NOT NULL,   -- JSON of Comp
    posted_at             TEXT,
    updated_at            TEXT,
    updated_at_unreliable INTEGER NOT NULL DEFAULT 0,
    department            TEXT,
    employment_type       TEXT,
    content_hash          TEXT NOT NULL,
    PRIMARY KEY (board_id, req_id)
) STRICT;

-- One row per observed change to a posting — history is the free by-product of diffing,
-- and it answers "when did this band change" forever.
CREATE TABLE posting_versions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    board_id       TEXT NOT NULL,
    req_id         TEXT NOT NULL,
    seen_at        TEXT NOT NULL,
    snapshot_id    INTEGER NOT NULL REFERENCES snapshots(id),
    changed_fields TEXT NOT NULL,   -- JSON array of field names
    content_hash   TEXT NOT NULL,
    FOREIGN KEY (board_id, req_id) REFERENCES postings(board_id, req_id)
) STRICT;

CREATE INDEX idx_versions_posting ON posting_versions(board_id, req_id, seen_at);

-- The obit ledger: rows suppressed from future NEW results. A (board_id, key) is unique
-- so re-marking is an upsert, not a duplicate.
CREATE TABLE obits (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    board_id  TEXT NOT NULL,
    key       TEXT NOT NULL,
    kind      TEXT NOT NULL,   -- dead | rejected | out_of_scope | ghost
    reason    TEXT NOT NULL,
    marked_at TEXT NOT NULL,
    UNIQUE (board_id, key)
) STRICT;
