# PLAN — job-board-mcp

Build order follows the payoff: prove the MCP plumbing first, then the schema everything hangs on, then adapters in board-population order (greenhouse covers the most boards, Ashby second), then the store + diff (the actual point of the project), then the public surface. Wave-2 adapters land after v0.1 — the server is useful with three.

<!--
FORMATv2: `## Phase <ID> - <Title>` headers, `- [ ] <ID>.<N> - <task>` lines;
[x] done, [>] deferred. Tick per task as you go, not in batches. If
claude-plan-bridge is wired, drive this file through TaskCreate/TaskUpdate
(letter phase ids); otherwise hand-edit with the same conventions.
Completed phases sweep to PLAN_ARCHIVE.md.
-->

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
