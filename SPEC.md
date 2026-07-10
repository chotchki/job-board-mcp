# job-board-mcp — SPEC

An MCP server exposing typed, deterministic job-board tools: fetch postings from hosted ATS APIs, normalize them to one schema, snapshot them in SQLite and answer "what changed since yesterday" as a query. The point is a clean division of labor — an LLM agent running a job-search scan should spend its tokens on JUDGMENT (does this role fit, is this band real) and none of them on scraping, parsing or re-deriving deltas from its own prior notes.

Origin story, since it explains every design call below: my daily job-search scan ran as a fleet of ~11 LLM agents that re-learned the endpoint quirks from the previous day's notes every single morning, fetched every board from scratch and diffed against prose. On one representative day a THIRD of the rows needed field corrections (location, level, comp band) — parsing errors typed code with tests simply does not make — and an entire adversarial verification phase existed mostly to catch them. That whole layer is mechanical. This server replaces it.

## Scope

**In:**
- Adapters for hosted ATS platforms with real JSON APIs (greenhouse, Ashby, Lever first; Workday, Workable, SmartRecruiters, Rippling and board-specific JSON APIs later).
- Normalization to a single `Posting` schema, including the per-platform quirk handling documented below.
- SQLite snapshot store: every posting ever seen, with field history.
- Deterministic diffing (NEW / CHANGED / DEAD) plus an obit ledger so dead rows and aggregator ghosts never resurface.
- MCP server over stdio exposing the above as tools.

**Out (and who owns it instead):**
- Fit scoring, ranking, "should I apply" — the CLIENT model's job. This server never holds an opinion.
- JS-rendered or bot-gated career sites (Google, Apple, Eightfold, Radancy tenants). Those stay agent work until someone writes an adapter; the server should not grow a headless browser.
- Writing to anything external. Read-only against the boards, no exceptions.
- Crawling. The server fetches exactly the boards in its config — it discovers nothing on its own.
- The user's watch list. Which boards to track is private by nature; this repo ships a `config.example.toml` with a few large public boards and the real config lives wherever the user keeps private things.

The determinism boundary in one sentence: everything that CAN be code IS code, and the only LLM anywhere in the system is the client calling the tools.

## Tool surface

| Tool | Input | Returns |
|---|---|---|
| `list_boards` | — | configured boards: id, ats, last successful snapshot time |
| `fetch_board` | `board_id` | live fetch → normalized postings; records a snapshot on success |
| `fetch_posting` | `board_id`, `req_id` | full detail incl. description text/html (for JD capture at apply time) |
| `diff_boards` | optional `board_ids[]` | NEW / CHANGED / DEAD per board vs the previous snapshot, obits excluded |
| `mark_obit` | `board_id`, `req_id` or freeform key, `kind`, `reason` | suppresses the row from future NEW results |
| `list_obits` | optional `board_id` | the ledger, for audit |

Obit kinds: `dead` (req vanished, confirmed), `rejected` (applied and closed), `out_of_scope` (looked at it, ruled out), `ghost` (aggregator listing that never existed on a primary source — these re-bite scans endlessly without a ledger).

Errors are typed and loud: `UnknownBoard`, `BoardUnreachable { status }`, `ParseDrift` (the feed's shape changed — fail, NEVER guess at fields; a wrong location or band silently propagating into a decision is the exact failure this project exists to kill).

## Posting schema

```
Posting {
  ats, board_id, req_id, title, url,
  locations: [string],
  workplace_type: onsite | hybrid | remote | unknown,
  remote_scope: string?,          // "US", "US + Canada", timezone constraints — verbatim, not interpreted
  comp: { min?, max?, currency?, interval?, source: api | body | site_only | none },
  posted_at?, updated_at?,
  updated_at_reliable: bool,      // false on bulk-touch boards (see quirks)
  department?, employment_type?,
  content_hash,                   // over material fields only — the change-detection key
}
```

`fetch_posting` detail adds `description_html` and `description_text`. `comp.source` is honest about WHERE the number came from: some platforms publish comp in the API, some only in the description body, some only on the company's own rendered site (`site_only` means "the API will never tell you — a client that wants the band must fetch the listing page").

## Change semantics

- `content_hash` covers title, locations, workplace_type, comp and a hash of the description — NOT `updated_at`. Several boards bulk-touch `updated_at` across every posting during reindexes, which makes it pure noise as a change signal; the per-board `updated_at_unreliable` flag records this.
- **NEW** = req_id never seen on this board. **CHANGED** = hash delta, with the changed fields recorded (in-place down-levels and band cuts are real and worth catching — a title quietly editing from Staff to Senior is a signal). **DEAD** = present in the previous snapshot, absent from the current successful FEED fetch. A single page 404 is never evidence of death — pages 404 for fetch-artifact reasons all the time; only the board's own listing feed counts.
- **A failed or partial fetch never writes a snapshot.** This is the one invariant that protects the whole diff: a tenant in maintenance mode or a 403 that returned an empty body must surface as `BoardUnreachable`, because recording it as a snapshot would mark every posting on the board DEAD and poison the next diff.

## Adapters and the quirk table

One trait, per-ATS implementations:

```rust
trait Adapter {
    async fn list(&self, board: &BoardConfig) -> Result<Vec<Posting>, AdapterError>;
    async fn detail(&self, board: &BoardConfig, req_id: &str) -> Result<PostingDetail, AdapterError>;
}
```

The quirks below are the accumulated field knowledge that used to live in prose and get re-taught to agents daily. They are the real content of this project — each one belongs in code, behind a test.

| Platform | Endpoint shape | Quirks the adapter must own |
|---|---|---|
| greenhouse | `boards-api.greenhouse.io/v1/boards/{token}/jobs?content=true` | Comp is frequently absent from API content even when published on the company's own site (`comp.source: site_only` per-board flag). Hosted-page URL varies (`job-boards.` vs `boards.` vs company-hosted). |
| Ashby | `api.ashbyhq.com/posting-api/job-board/{token}` | `isRemote` is board-wide metadata noise — `workplaceType` + description body are the only trustworthy location signals. Comp sometimes appears only inside `descriptionHtml`. May 403 a bare client UA. |
| Lever | `api.lever.co/v0/postings/{token}?mode=json` | Comparatively sane. |
| Workday | `{tenant}.wd{N}.myworkdayjobs.com/wday/cxs/{tenant}/{site}/jobs` (POST search) | `startDate` is the post date. Tenants go into maintenance mode during migrations — MUST surface as `BoardUnreachable`, never as an empty board. |
| Workable | `apply.workable.com/api/v3/accounts/{token}/jobs` | — |
| SmartRecruiters | `api.smartrecruiters.com/v1/companies/{token}/postings` | — |
| Rippling | `api.rippling.com/platform/api/ats/v1/board/{token}/jobs` | Per-job ground truth lives in the page's `__NEXT_DATA__`, feed is the listing source. |
| github.careers | `www.github.careers/api/jobs?keyword=&page=1` | The HTML `?query=` param is ignored server-side; rendered pages embed a no-results i18n string that defeats naive grepping. Board-specific adapter, wave 2. |

Cross-platform rules: always send a desktop-browser User-Agent (several platforms 403 default client UAs), serialize requests per host with a politeness delay, hard timeouts, no automatic retry storms.

## Store

SQLite via rusqlite. Tables, roughly:

- `boards` — mirror of config (id, ats, token, flags) so snapshots have something to reference; config file remains the source of truth.
- `snapshots` — board_id, taken_at, posting_count. Only written on a successful, well-formed fetch.
- `postings` — (board_id, req_id) identity, first_seen, last_seen, current field values, current content_hash.
- `posting_versions` — one row per observed change: seen_at, changed fields as JSON, snapshot reference. History is the free by-product of diffing and it answers "when did this band change" forever.
- `obits` — board_id, key, kind, reason, marked_at.

## Config

TOML, path from `--config` or `JOB_BOARD_MCP_CONFIG`. Ships as `config.example.toml`; the real file is the user's.

```toml
db_path = "~/.local/share/job-board-mcp/store.sqlite"

[[board]]
id = "stripe"
ats = "greenhouse"
token = "stripe"
comp_site_only = true          # bands publish on stripe.com, never in the API

[[board]]
id = "openai"
ats = "ashby"
token = "openai"
```

## MCP integration

Rust, using [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (the official SDK), stdio transport, tools returning JSON content. (Full disclosure for the builder: rmcp's API surface moves fast and this spec deliberately does NOT pin its shapes — verify the current tool-definition macros and server setup against the rust-sdk repo and docs.rs at build time rather than trusting anything written here about it.)

## Testing

- Checked-in fixtures captured from real public API responses, truncated to a few postings each; adapter tests parse fixtures, never the network.
- Diff semantics get their own suite: bulk-touched `updated_at` producing zero CHANGED rows, the failed-fetch-never-snapshots invariant, obit suppression, in-place field edits surfacing as CHANGED with the right field list.
- Live smoke tests exist but are `#[ignore]` by default — CI never hits the network.

## Success criterion

One `diff_boards()` call returns the same deltas a morning's agent fleet would have found on adapter-covered boards, with zero field errors — validated by running both side by side on a real morning before the fleet retires. Not a benchmark, a replacement test.
