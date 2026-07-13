# Using job-board-mcp

A practical walkthrough for running your own daily scan. The [README](../README.md) is the reference (tool signatures, the quirk table); this is the "how do I actually live with it" guide.

## Install

From a checkout:

```
cargo install --path .
```

That drops the `job-board-mcp` binary in `~/.cargo/bin` (on your PATH if cargo is set up normally). It is NOT on crates.io — this is a personal tool, so `publish` is off and local-checkout install is the only path. Uninstall with `cargo uninstall job-board-mcp`.

Rust 1.85+ (edition 2024). SQLite is compiled in — there is no system library to install, on any OS.

## Your config

The board list is yours and private, so only [`config.example.toml`](../config.example.toml) ships. Copy it somewhere outside the repo (a dotfiles dir, `~/.config/`, wherever) and edit it:

```toml
db_path = "~/.local/share/job-board-mcp/store.sqlite"

[[board]]
id = "stripe"
ats = "greenhouse"     # greenhouse | ashby | lever
token = "stripe"
comp_site_only = true  # optional
```

`id` is your name for the board and the key everything is stored under — pick it once and keep it, because renaming it orphans that board's history. `token` is the ATS's own slug, the bit in its API URL. A typo'd key is a hard error, not a silent default; naming an unimplemented ATS is a loud parse failure. Both are on purpose — a config that quietly did the wrong thing would defeat the point.

## The store

One SQLite file, at `db_path` (the `~` expands). It's created on first run, along with any missing parent directories. That file IS your history — every successful fetch appends a snapshot, and the version log answers "when did this band change" for as long as you keep it. Back it up like a dotfile; delete it to start clean. Nothing is written anywhere else, and nothing leaves your machine except read-only GETs to the boards.

## Wire it into your MCP client

**Claude Code:**

```
claude mcp add job-board -- job-board-mcp --config ~/path/to/your/config.toml
```

**Any MCP client** (Claude Desktop, etc.) — add to the client's server config:

```json
{
  "mcpServers": {
    "job-board": {
      "command": "job-board-mcp",
      "args": ["--config", "/absolute/path/to/your/config.toml"]
    }
  }
}
```

The config path can also come from the `JOB_BOARD_MCP_CONFIG` environment variable instead of `--config`.

## A morning scan

The division of labor is the whole point: the tools do the mechanical part with zero field errors, and you (or the model driving them) spend judgment only where judgment is needed.

1. **`fetch_board`** each board (the model can loop over `list_boards`). Each successful fetch records a snapshot. A board in maintenance mode returns an error and records nothing — it is never mistaken for an empty board.
2. **`diff_boards`** — NEW / CHANGED / DEAD since your previous scan, per board. CHANGED names the fields that moved, so a title quietly edited from Staff to Senior, or a band cut, shows up as a real signal rather than getting lost in noise. A board that just bulk-touched every `updated_at` during a reindex produces zero CHANGED, by design. Pass `include_summary: true` to fold each NEW/CHANGED row's last-known title, locations, comp and workplace into the diff — straight from the stored snapshot, no refetch — so triage reads a real row instead of a bare req id. DEAD rows stay id-only (use `fetch_posting` for a dead req's post-mortem).
3. **`mark_obit`** the rows you're done with, so tomorrow's scan stays quiet (see below).
4. **`fetch_posting`** the ones you're actually applying to — it returns the full description text/html for capturing the JD at apply time.

## Obit hygiene — how the scan stays quiet

Without a ledger, the same rejected roles and aggregator phantoms show up as NEW every single morning. `mark_obit` suppresses a row from future NEW results. Four kinds:

- `ghost` — an aggregator listing that never existed on a primary source. These re-bite endlessly; this is the one the ledger exists for.
- `rejected` — you applied and it's closed.
- `out_of_scope` — you looked and ruled it out.
- `dead` — you confirmed the req is gone.

`list_obits` is the audit view. Re-marking a key updates it in place.

## Handing back a sample

When a board's numbers look wrong, an adapter drifts (`ParseDrift`), or you want a platform this build doesn't cover yet, the fix wants the board's ACTUAL response — not a description of it. That's what capture is for. Every successful fetch already logged its raw body to the store (controlled by `raw_capture_days`, default 7).

1. **`list_captures`** — the ledger, newest first, metadata only (board, url, status, size). Find the one you want; the bodies aren't dragged along.
2. **`dump_captures`** — writes the raw bodies to files and returns their paths. Pass `out_dir` to choose where they land (a leading `~/` expands); omit it and they go to a `captures/` directory beside the store. Scope with `board_id` and cap with `limit`. The bodies are never echoed inline — a single board is hundreds of KB, and that belongs in a file, not your context window.

Then hand the file back. A real captured response is what turns "the equity number looks off" into a fixed adapter with a test pinned to the actual shape.

## Troubleshooting

| Symptom | What it means |
|---|---|
| `Error: loading config` on startup | The `--config` path (or `JOB_BOARD_MCP_CONFIG`) is wrong or unreadable. |
| `unknown board: X` | `X` isn't an `id` in your config. |
| a board fails to parse in config | Its `ats` isn't implemented yet (Workday and friends are wave 2), or a key is misspelled. |
| `board unreachable: HTTP <status>` | The board's API returned a non-success. Nothing is recorded — that's the invariant, not a bug. Retry later. |
| `parse drift while reading …` | The board changed its API shape. The adapter needs updating; this is a loud stop rather than a guess, so file it, don't work around it. |

Logs go to stderr (stdout is the MCP protocol channel). Set `RUST_LOG=debug` for detail while wiring things up.
