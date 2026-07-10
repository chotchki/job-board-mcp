# job-board-mcp

[![CI](https://github.com/chotchki/job-board-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/chotchki/job-board-mcp/actions/workflows/ci.yml)

An MCP server that turns job-board scraping into a typed, deterministic tool call. It fetches postings from hosted ATS APIs (greenhouse, Ashby and Lever today; Workday and friends after), normalizes them to one schema, snapshots them in SQLite, and answers "what changed since yesterday" as a QUERY — not as a prose diff some agent re-derives from its own notes every morning. That division of labor is the whole point. Parsing a location field, or catching a title that quietly edited itself from Staff down to Senior, is mechanical work: typed code with tests does it perfectly, and an LLM does it wrong often enough that a verification phase has to exist to catch it. So the server owns the mechanics and holds no opinions, and the client model spends its tokens on the part that actually needs judgment — does this role fit, is that band real.

The full design, the change semantics, and the reasoning behind each per-platform quirk live in [SPEC.md](SPEC.md).

> **New here?** [docs/USAGE.md](docs/USAGE.md) is the practical walkthrough — install, wiring it into your MCP client, and a daily-scan workflow. This page is the reference.

## Install

Rust 1.85+ (edition 2024). SQLite is bundled — no system library needed.

```
cargo install --path .
```

That installs the `job-board-mcp` binary to `~/.cargo/bin`. (Not on crates.io — this is a personal tool, so `publish` is off; install from a local checkout.) It speaks MCP over stdio and takes its board list from a config file (below). Point your MCP client at it, e.g.:

```json
{
  "mcpServers": {
    "job-board": {
      "command": "/path/to/job-board-mcp",
      "args": ["--config", "/path/to/your/config.toml"]
    }
  }
}
```

The config path comes from `--config <path>` or the `JOB_BOARD_MCP_CONFIG` environment variable.

## Config

TOML. Copy [`config.example.toml`](config.example.toml), edit it, and keep it wherever you keep private things — your watch list is nobody's business, and only the example ships here.

```toml
db_path = "~/.local/share/job-board-mcp/store.sqlite"

[[board]]
id = "stripe"                  # your name for the board; also its snapshot key
ats = "greenhouse"             # greenhouse | ashby | lever
token = "stripe"               # the ATS tenant slug in the board's API URL
comp_site_only = true          # optional: bands publish on the company site, not the API
updated_at_unreliable = false  # optional: this board bulk-touches updated_at
```

A misspelled key is a hard error, not a silent default — config is yours to get right, and a typo that quietly turned a flag off would mislead a decision. Naming an ATS this build doesn't implement is likewise a loud parse failure, not a board that silently fetches nothing.

## Tools

| Tool | Input | Returns |
|---|---|---|
| `list_boards` | — | configured boards: `id`, `ats`, last successful snapshot time |
| `fetch_board` | `board_id` | live fetch → normalized postings; records a snapshot on success |
| `fetch_posting` | `board_id`, `req_id` | full detail incl. description text/html, for JD capture at apply time |
| `diff_boards` | optional `board_ids[]` | NEW / CHANGED / DEAD per board vs the previous snapshot, obits excluded |
| `mark_obit` | `board_id`, `key`, `kind`, `reason` | suppresses the row from future NEW results |
| `list_obits` | optional `board_id` | the ledger, for audit |

**NEW** is a req_id never seen on this board. **CHANGED** is a material-content change (title, locations, workplace type, comp, or description) with the moved fields named — a quiet Staff→Senior edit shows up here. **DEAD** is a posting present in the previous snapshot and absent from the latest successful feed; a single page 404 never counts, only the board's own listing. A bulk-touched `updated_at` is not a change, because the change key deliberately excludes it. And a failed or partial fetch NEVER records a snapshot — a board in maintenance mode surfaces as an error, because recording it as empty would mark everything DEAD and poison the next diff.

Obit kinds: `dead` (req vanished, confirmed), `rejected` (applied and closed), `out_of_scope` (looked at it, ruled out), `ghost` (an aggregator listing that never existed on a primary source — these re-bite a scan endlessly without a ledger).

## The quirk table

The accumulated per-platform field knowledge that used to live in prose and get re-taught to an agent every morning. Each one lives in code, behind a test.

| Platform | Endpoint | Quirks the adapter owns |
|---|---|---|
| greenhouse | `boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true` | Comp is usually absent from the API even when the company publishes it (`comp_site_only` per-board flag). `absolute_url` is authoritative, so hosted-URL variants (`job-boards.` vs `boards.` vs company-hosted) are a non-issue. Workplace type isn't a field — inferred as Remote only when the location literally says so, else Unknown. |
| Ashby | `api.ashbyhq.com/posting-api/job-board/{token}` | `workplaceType` is the location truth; `isRemote` is board-wide noise and is never read. Comp is structured in the API (`compensation.summaryComponents`); equity components are out of the v0.1 comp model. No single-job endpoint, so detail re-fetches the board. |
| Lever | `api.lever.co/v0/postings/{token}?mode=json` | Comparatively sane: `workplaceType` is a clean lowercase enum, `salaryRange` is structured, `createdAt` is epoch millis. The top level is a bare array. |
| Workday, Workable, SmartRecruiters, Rippling, github.careers | — | Wave 2, not yet implemented. Naming one in config is a loud parse error until its adapter lands. |

Comp is always integer minor units — a band encodes to the same integers on every parse, so `content_hash` is stable and a band that didn't change never reports CHANGED. An unrecognized currency or pay interval is a loud `ParseDrift`, never a guess: a wrong band silently reaching a decision is the exact failure this project exists to kill.

## Development

Enable the local pre-push gate (fmt + clippy + test, mirroring CI) once per clone:

```
git config core.hooksPath .githooks
```

Adapter tests parse checked-in fixtures captured from real API responses and never touch the network. Live smoke tests exist but are `#[ignore]`d — a scheduled weekly job runs them, so an ignored test can't rot green while a board quietly changes its API. Build order and history live in [PLAN.md](PLAN.md) and [PLAN_ARCHIVE.md](PLAN_ARCHIVE.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
