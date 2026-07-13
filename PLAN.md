# PLAN — job-board-mcp

Build order follows the payoff: prove the MCP plumbing first, then the schema everything hangs on, then adapters in board-population order (greenhouse covers the most boards, Ashby second), then the store + diff (the actual point of the project), then the public surface. Wave-2 adapters land after v0.1 — the server is useful with three.

<!--
FORMATv2: `## Phase <ID> - <Title>` headers, `- [ ] <ID>.<N> - <task>` lines;
[x] done, [>] deferred. Tick per task as you go, not in batches. If
claude-plan-bridge is wired, drive this file through TaskCreate/TaskUpdate
(letter phase ids); otherwise hand-edit with the same conventions.
Completed phases sweep to PLAN_ARCHIVE.md.
-->
## Phase G - Failure-mode census
- [x] G.1 - Determine rmcp handler-panic behavior empirically
- [x] G.2 - Census hot-path panic sites on untrusted data
- [x] G.3 - Census emitted schema constructs for client-rejection risk
- [x] G.4 - Census error legibility end-to-end
## Phase H - Contain catastrophic failures
- [x] H.1 - Replace read-path expects on persisted JSON with typed errors
- [x] H.2 - Add a handler panic boundary if needed
- [x] H.3 - Harden JSON access across all 8 adapters
## Phase I - Contract & legibility guardrails
- [x] I.1 - Add schema-conformance e2e against a real validator
- [x] I.2 - Spike structured (typed data) errors on McpError
- [x] I.3 - Document the failure-mode contract
- [x] I.4 - Inline enum schemas — drop $ref/$defs from the emitted contract

## Backlog (not yet phased)

- **Workday pagination speed — bounded concurrency vs politeness** — added 2026-07-10.
- **diff_boards NEW rows carry only req_ids — triage needs titles** — the 7/13 scan burned an 8-agent fan-out (~490K subagent tokens) just to turn 60 NEW ids into title/location/comp rows before any judgment could happen. An opt-in `include_summary` on diff_boards (title, locations, comp band for NEW/CHANGED rows, from the snapshot already in the store — no refetch) collapses that whole step. Added 2026-07-13.
- **Dead reqs are unservable post-mortem** — fetch_posting on a req that just went DEAD returns PostingNotFound (sofi 7601581003/7782185003, 7/13 scan), so "what WAS that req?" — the first question DEAD-triage asks — has no answer outside a manual capture-ledger dig. Serve the last-known snapshot with a `dead_as_of` marker, or grow a `fetch_dead_posting`. Added 2026-07-13.
