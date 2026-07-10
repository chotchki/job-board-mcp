## 2026-07-10

## Phase A - Scaffold
- [x] A.1 - cargo scaffold + rmcp stdio server with a single ping tool, verified end-to-end from a real MCP client (verify current rmcp API against the rust-sdk repo first — SPEC deliberately doesn't pin it)
- [x] A.2 - CI: fmt + clippy + test on push
- [x] A.3 - chris: pick license
- [x] A.4 - README stub (one paragraph + build badge; real README is E.3)
- [x] A.5 - Lint + local harness: [lints.clippy] allow_attributes_without_reason=deny + allow_attributes=warn (reasons live in source #[expect], the manifest silently drops them); clippy.toml disallowed-methods banning SystemTime::now, chrono/rand entries commented until those deps land (an unresolved ban is silently ignored, so it is indistinguishable from a typo); .githooks/pre-push via core.hooksPath
- [x] A.6 - CI hardening: ubuntu-only lint job + tri-OS test matrix (fail-fast: false); -D warnings scoped to the clippy step rather than a global RUSTFLAGS (which pollutes the rust-cache key); rust-cache skipped on windows, where it crashes mid-restore


---

## 2026-07-10

## Phase B - Core model
- [x] B.1 - Posting + PostingDetail: serde(default) + skip_serializing_if on every optional, NO deny_unknown_fields (cross-version machine data); content_hash over material fields via a canonical encoding, never derive(Hash) on memory layout; round-trip tests assert struct-eq not bytes, plus a pinned content_hash known-answer vector; synthetic Postings built through serde_json (never format!), absolute paths branched on cfg!(windows)
- [x] B.2 - BoardConfig TOML + config.example.toml (db_path, per-board comp_site_only + updated_at_unreliable): serde(default) for forward-compat AND deny_unknown_fields — config is human-authored and single-reader, so a typo'd key must fail LOUD rather than silently default; test that example.toml parses and that an old-shape config loads with new fields defaulting
- [x] B.3 - Adapter trait + error taxonomy (UnknownBoard, BoardUnreachable{status}, ParseDrift — ParseDrift fails loudly, never guesses fields)
- [x] B.4 - HTTP layer: desktop-browser UA, per-host serialization + politeness delay, hard timeouts, no retry storms
- [x] B.5 - Type-system spine: newtype BoardId/ReqId/AtsToken via one serde(transparent)+sqlx(transparent) macro (gives Type+Encode+Decode from one derive), ContentHash (hex TEXT) + Currency (validated) via hand-written sqlx Type/Encode/Decode delegating to String; closed enums for Ats/workplace_type/comp interval+source/obit-kind; free text stays String; comp as integer minor units with #![deny(clippy::float_arithmetic)] atop comp.rs — note clippy silently suppresses that lint inside #[test] fns, so the i64 newtype is the real guard. Store access is sqlx (compile-time queries), not rusqlite — see D.1.


---

## 2026-07-10

## Phase C - Adapters wave 1
- [x] C.1 - greenhouse adapter + fixtures (comp.source: site_only flag path, hosted-URL variants)
- [x] C.2 - ashby adapter + fixtures (workplaceType = truth, isRemote = noise; comp extraction from descriptionHtml; browser-UA 403 path)
- [x] C.3 - lever adapter + fixtures
- [x] C.4 - live smoke tests, #[ignore] by default, against 2-3 large public boards; a scheduled (weekly, not per-push) CI job runs them, so an #[ignore]d test cannot rot green-forever when an API shifts under it


