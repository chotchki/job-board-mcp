# PLAN — job-board-mcp

Build order follows the payoff: prove the MCP plumbing first, then the schema everything hangs on, then adapters in board-population order (greenhouse covers the most boards, Ashby second), then the store + diff (the actual point of the project), then the public surface. Wave-2 adapters land after v0.1 — the server is useful with three.

<!--
FORMATv2: `## Phase <ID> - <Title>` headers, `- [ ] <ID>.<N> - <task>` lines;
[x] done, [>] deferred. Tick per task as you go, not in batches. If
claude-plan-bridge is wired, drive this file through TaskCreate/TaskUpdate
(letter phase ids); otherwise hand-edit with the same conventions.
Completed phases sweep to PLAN_ARCHIVE.md.
-->

## Phase A - Scaffold
- [x] A.1 - cargo scaffold + rmcp stdio server with a single ping tool, verified end-to-end from a real MCP client (verify current rmcp API against the rust-sdk repo first — SPEC deliberately doesn't pin it)
- [x] A.2 - CI: fmt + clippy + test on push
- [x] A.3 - chris: pick license
- [x] A.4 - README stub (one paragraph + build badge; real README is E.3)
- [x] A.5 - Lint + local harness: [lints.clippy] allow_attributes_without_reason=deny + allow_attributes=warn (reasons live in source #[expect], the manifest silently drops them); clippy.toml disallowed-methods banning SystemTime::now, chrono/rand entries commented until those deps land (an unresolved ban is silently ignored, so it is indistinguishable from a typo); .githooks/pre-push via core.hooksPath
- [x] A.6 - CI hardening: ubuntu-only lint job + tri-OS test matrix (fail-fast: false); -D warnings scoped to the clippy step rather than a global RUSTFLAGS (which pollutes the rust-cache key); rust-cache skipped on windows, where it crashes mid-restore

## Phase B - Core model
- [ ] B.1 - Posting + PostingDetail: serde(default) + skip_serializing_if on every optional, NO deny_unknown_fields (cross-version machine data); content_hash over material fields via a canonical encoding, never derive(Hash) on memory layout; round-trip tests assert struct-eq not bytes, plus a pinned content_hash known-answer vector; synthetic Postings built through serde_json (never format!), absolute paths branched on cfg!(windows)
- [ ] B.2 - BoardConfig TOML + config.example.toml (db_path, per-board comp_site_only + updated_at_unreliable): serde(default) for forward-compat AND deny_unknown_fields — config is human-authored and single-reader, so a typo'd key must fail LOUD rather than silently default; test that example.toml parses and that an old-shape config loads with new fields defaulting
- [ ] B.3 - Adapter trait + error taxonomy (UnknownBoard, BoardUnreachable{status}, ParseDrift — ParseDrift fails loudly, never guesses fields)
- [ ] B.4 - HTTP layer: desktop-browser UA, per-host serialization + politeness delay, hard timeouts, no retry storms
- [ ] B.5 - Type-system spine: newtype BoardId/ReqId/AtsToken/ContentHash/Currency via one serde(transparent)+FromSql/ToSql macro (ContentHash as hex, not transparent); closed enums for Ats/workplace_type/comp interval+source/obit-kind; free text stays String; comp as integer minor units with #![deny(clippy::float_arithmetic)] atop comp.rs — note clippy silently suppresses that lint inside #[test] fns, so the i64 newtype is the real guard

## Phase C - Adapters wave 1
- [ ] C.1 - greenhouse adapter + fixtures (comp.source: site_only flag path, hosted-URL variants)
- [ ] C.2 - ashby adapter + fixtures (workplaceType = truth, isRemote = noise; comp extraction from descriptionHtml; browser-UA 403 path)
- [ ] C.3 - lever adapter + fixtures
- [ ] C.4 - live smoke tests, #[ignore] by default, against 2-3 large public boards; a scheduled (weekly, not per-push) CI job runs them, so an #[ignore]d test cannot rot green-forever when an API shifts under it

## Phase D - Store + diff
- [ ] D.1 - SQLite schema + migrations (boards, snapshots, postings, posting_versions, obits)
- [ ] D.2 - snapshot write path — the invariant: a failed or partial fetch NEVER writes a snapshot; store write methods take taken_at as an explicit param (no now() in the store — the MCP handler is the one clock reader); activate the SystemTime/Utc::now ban with an #[expect(clippy::disallowed_methods, reason=...)] at that single handler call site, which both proves the ban still fires and documents the sole legitimate reader
- [ ] D.3 - diff_boards: NEW / CHANGED / DEAD vs previous snapshot, changed-field list recorded per CHANGED row
- [ ] D.4 - obit ledger + suppression (dead | rejected | out_of_scope | ghost)
- [ ] D.5 - diff semantics suite from HAND-DERIVED vectors, authored from SPEC prose and run through the real diff_boards (never golden-captured from current output, which freezes today's bugs): bulk-touch updated_at → zero CHANGED, empty-fetch guard, obit suppression, in-place down-level → CHANGED; a field-materiality law (mutate each field, hash changes iff the field is material); a semantic-lock snapshot of the diff CLASSIFICATION (which req_ids are NEW/CHANGED/DEAD plus changed-field lists) at a pinned fixture pair

## Phase E - MCP surface + v0.1
- [ ] E.1 - expose tools: list_boards, fetch_board, fetch_posting, diff_boards, mark_obit, list_obits
- [ ] E.2 - end-to-end test driving the stdio server from an MCP client harness
- [ ] E.3 - README: install, config schema, tool reference, the quirk table (lifted from SPEC)
- [ ] E.4 - tag v0.1.0

## Phase F - Adapters wave 2 (post-v0.1)
- [ ] F.1 - workday (CXS POST search; startDate = post date; maintenance mode → BoardUnreachable, never an empty board)
- [ ] F.2 - workable + smartrecruiters
- [ ] F.3 - rippling (feed = listing source, __NEXT_DATA__ = per-job ground truth)
- [ ] F.4 - github.careers (?query= ignored server-side; no-results i18n grep trap)

## Backlog (not yet phased)

- **Comp::Equity variant — model equity grants outside the currency path** — added 2026-07-10.
