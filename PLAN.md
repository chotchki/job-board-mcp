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
- [ ] A.2 - CI: fmt + clippy + test on push
- [x] A.3 - chris: pick license
- [x] A.4 - README stub (one paragraph + build badge; real README is E.3)

## Phase B - Core model
- [ ] B.1 - Posting + PostingDetail structs, content_hash over material fields, serde round-trip tests
- [ ] B.2 - BoardConfig TOML loading + config.example.toml (db_path, per-board flags: comp_site_only, updated_at_unreliable)
- [ ] B.3 - Adapter trait + error taxonomy (UnknownBoard, BoardUnreachable{status}, ParseDrift — ParseDrift fails loudly, never guesses fields)
- [ ] B.4 - HTTP layer: desktop-browser UA, per-host serialization + politeness delay, hard timeouts, no retry storms

## Phase C - Adapters wave 1
- [ ] C.1 - greenhouse adapter + fixtures (comp.source: site_only flag path, hosted-URL variants)
- [ ] C.2 - ashby adapter + fixtures (workplaceType = truth, isRemote = noise; comp extraction from descriptionHtml; browser-UA 403 path)
- [ ] C.3 - lever adapter + fixtures
- [ ] C.4 - live smoke tests, #[ignore] by default, against 2-3 large public boards

## Phase D - Store + diff
- [ ] D.1 - SQLite schema + migrations (boards, snapshots, postings, posting_versions, obits)
- [ ] D.2 - snapshot write path — the invariant: a failed or partial fetch NEVER writes a snapshot
- [ ] D.3 - diff_boards: NEW / CHANGED / DEAD vs previous snapshot, changed-field list recorded per CHANGED row
- [ ] D.4 - obit ledger + suppression (dead | rejected | out_of_scope | ghost)
- [ ] D.5 - diff semantics suite: bulk-touch updated_at → zero CHANGED, empty-fetch guard, obit suppression, in-place down-level surfaces as CHANGED

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
