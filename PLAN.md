<!-- plan-bridge:phase-high-water=K -->
# PLAN — job-board-mcp

Build order follows the payoff: prove the MCP plumbing first, then the schema everything hangs on, then adapters in board-population order (greenhouse covers the most boards, Ashby second), then the store + diff (the actual point of the project), then the public surface. Wave-2 adapters land after v0.1 — the server is useful with three.

<!--
FORMATv2: `## Phase <ID> - <Title>` headers, `- [ ] <ID>.<N> - <task>` lines;
[x] done, [>] deferred. Tick per task as you go, not in batches. If
claude-plan-bridge is wired, drive this file through TaskCreate/TaskUpdate
(letter phase ids); otherwise hand-edit with the same conventions.
Completed phases sweep to PLAN_ARCHIVE.md.
-->

## Backlog (not yet phased)

- **Workday pagination speed — bounded concurrency vs politeness** — added 2026-07-10.
