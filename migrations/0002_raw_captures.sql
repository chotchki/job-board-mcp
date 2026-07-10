-- Raw request/response capture ledger. Every successful ATS fetch lands its raw body
-- here, keyed by board, so a real sample can be dumped and handed back when an adapter
-- needs to be built or fixed against the actual shape (an EquityPercentage whose units
-- aren't pinned, a board that quietly changed its API). Bounded by a retention window —
-- each write purges rows past it — so the ledger never grows without limit.
CREATE TABLE raw_captures (
    id           INTEGER PRIMARY KEY,
    board_id     TEXT    NOT NULL,
    ats          TEXT    NOT NULL,  -- JSON-encoded Ats, same encoding as boards.ats
    url          TEXT    NOT NULL,
    method       TEXT    NOT NULL,  -- GET | POST
    request_body TEXT,              -- the POST body, when there is one (Workday/Workable)
    status       INTEGER NOT NULL,  -- HTTP status of the captured response
    captured_at  TEXT    NOT NULL,  -- RFC3339, injected — the store reads no clock
    body         TEXT    NOT NULL   -- the raw response body, verbatim
) STRICT;

-- Purge and per-board listing both filter on (board_id, captured_at).
CREATE INDEX raw_captures_board_captured ON raw_captures (board_id, captured_at);
