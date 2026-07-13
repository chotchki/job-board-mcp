<!-- plan-bridge:phase-high-water=I -->
# PLAN — job-board-mcp

Build order follows the payoff: prove the MCP plumbing first, then the schema everything hangs on, then adapters in board-population order (greenhouse covers the most boards, Ashby second), then the store + diff (the actual point of the project), then the public surface. Wave-2 adapters land after v0.1 — the server is useful with three.

<!--
FORMATv2: `## Phase <ID> - <Title>` headers, `- [ ] <ID>.<N> - <task>` lines;
[x] done, [>] deferred. Tick per task as you go, not in batches. If
claude-plan-bridge is wired, drive this file through TaskCreate/TaskUpdate
(letter phase ids); otherwise hand-edit with the same conventions.
Completed phases sweep to PLAN_ARCHIVE.md.
-->
## Phase J - diff_boards include_summary
the 7/13 scan burned an 8-agent fan-out (~490K subagent tokens) just to turn 60 NEW ids into title/location/comp rows before any judgment could happen. An opt-in `include_summary` on diff_boards (title, locations, comp band for NEW/CHANGED rows, from the snapshot already in the store — no refetch) collapses that whole step. Added 2026-07-13.
- [x] J.1 - Store: enriched diff for NEW+CHANGED reqs
- [x] J.2 - Server: include_summary arg on diff_boards
- [x] J.3 - Tests + docs for include_summary
## Phase K - Dead-req post-mortem serving
fetch_posting on a req that just went DEAD returns PostingNotFound (sofi 7601581003/7782185003, 7/13 scan), so "what WAS that req?" — the first question DEAD-triage asks — has no answer outside a manual capture-ledger dig. Serve the last-known snapshot with a `dead_as_of` marker, or grow a `fetch_dead_posting`. Added 2026-07-13.
- [ ] K.1 - Store: last-known posting getter by req
- [ ] K.2 - Server: fetch_posting dead-req fallback
- [ ] K.3 - Tests + docs for dead-req serving

## Backlog (not yet phased)

- **Workday pagination speed — bounded concurrency vs politeness** — added 2026-07-10.
