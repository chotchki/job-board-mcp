# Failure modes — job-board-mcp

The thesis behind the whole sweep: today's two loud bugs were both "one bad input → total,
illegible failure." Find the latent versions before they're loud. The **contract** below is
what the sweep bought; the **census** (phase G) and **remediations** (phases H/I) beneath it
are the evidence. Each census entry: what fails, the blast radius, what the CALLER sees, where
the fix lands.

---

## The failure-mode contract

What this server now guarantees a caller, and what an error means:

1. **No total-listing failure from a schema construct.** Every emitted tool schema is
   client-safe — no boolean subschemas, no `$ref`/`$defs` — enforced by the
   `tool_schemas_are_client_safe` e2e test. (Closes the class of both the boolean-schema bug
   and the `mark_obit` `$ref`.)

2. **No silent hang.** Every failure returns a response. Known panic sources (persisted-JSON
   reads) are typed errors; any unforeseen panic is caught and returned as `internal_error`.
   No caller request can black-hole.

3. **No garbage postings.** A required field (`req_id`, `title`, `url`) missing from an ATS
   payload is a hard `ParseDrift`, never a silently-defaulted empty — the store never holds a
   posting you can't apply to.

4. **Every error is legible AND machine-branchable.** The message is source-chained (store
   errors carry the sqlx cause); the JSON-RPC-error channel also carries
   `data: {"kind", "retryable"}`:

   | kind               | JSON-RPC        | retryable | means                                            |
   |--------------------|-----------------|-----------|--------------------------------------------------|
   | `bad_input`        | invalid_params  | false     | wrong board / req id / arg — fix the input        |
   | `transient_remote` | internal_error  | **true**  | the ATS was unreachable / timed out — retry later |
   | `broken_adapter`   | internal_error  | false     | the feed's shape drifted — the adapter needs a fix|
   | `store`            | internal_error  | false     | persistence failure, incl. a corrupt row          |
   | `internal`         | internal_error  | false     | a caught panic or an io error                     |

   Caveat: `data` rides only the JSON-RPC-error channel (arg-deserialize errors are tool
   `isError` text, no `data`) and its surfacing to the agent is client-dependent — the message
   is the portable signal, `data` the upside where clients honor it.

---

## G.1 — rmcp handler-panic containment — SETTLED (empirical)

**Question:** does a `panic!` inside a `#[tool]` handler kill one request or the whole
server?

**Probe:** a throwaway `debug_panic` tool that panics; drove `initialize` → call
`debug_panic` → call `list_boards` over stdio, watching process liveness and whether the
post-panic call still answered.

**Result — the panic is CONTAINED, but the request BLACK-HOLES:**
- Process survives (`poll()` = None after the panic fired).
- Connection survives — `list_boards` called AFTER the panic returned a normal result.
- The panicking request gets NO response — no result, no JSON-RPC error. The caller's
  request id hangs until the client's own timeout. The panic text
  (`... panicked at src/server.rs:NNN`) goes to STDERR, which an MCP client never sees.

**Why:** rmcp dispatches each request on its own spawned tokio task (`tokio-rt-worker`),
and the default unwind profile (no `panic = "abort"`) means an unwinding panic kills only
that task — not the runtime, not the stdio loop.

**Blast radius:** ONE request. This is NOT the schema-bug class (one bad thing → every
tool dead). It's a localized SILENT HANG — arguably harder to diagnose than a clean error
because the caller just waits.

**Implications for phase H:**
- H.2 (panic boundary) drops from "prevent total outage" to "convert a silent hang into a
  legible `McpError`." Still worth doing under the no-illegible-failures thesis, but not
  urgent-catastrophic.
- H.1 (kill the read-path `.expect()`s) stays the priority: its worst case is now "the one
  call touching a corrupt row hangs forever," localized but still a silent black-hole.

---

## G.2 — hot-path panic-site census — DONE

Swept every `unwrap` / `expect` / `panic` / index in the request path (real code, before
each file's `#[cfg(test)]`), ranked by likelihood × blast on data we don't control. The
finding is itself reassuring: the panic surface is SMALL and CONCENTRATED, not a sprawl.

**Tier 1 — persisted-JSON read path (the real risk; == H.1):**
- `store.rs:454` changed_fields, `:532` obit kind, `:606`/`:641` ats — all
  `from_str(...).expect("... is JSON we wrote")`. A corrupt / stale / migration-drifted row
  → panic → (per G.1) that one read call silently hangs. Low-but-real likelihood, localized
  blast. THE target.

**Tier 2 — dissolved on inspection (guarded by construction):**
- `comp.rs:45` currency `bytes[0..3]` — sits inside `if bytes.len() == 3`, can't panic.
- `rippling.rs:110` `remove().expect("uuid was inserted")` — `order` and `merged` are built
  in lockstep (a uuid is pushed to `order` only in the same branch that inserts it), so the
  remove can't miss.
- `http.rs:171` std-`Mutex` `.expect("poisoned")` — the critical section is a `HashMap`
  entry + `Arc` clone with no panic-prone work under the lock, so poison is OOM-only
  (abort-level anyway). Theoretical, not a practical "one panic cascades" vector.
- Adapters: ZERO `.unwrap()` on payload data — `.get()` / `.first()` / `ParseDrift`
  throughout. Defensively written already, which lowers the H.3 risk going in.

**Tier 3 — our-own-invariant expects (near-zero; leave, or fold into H.1 for uniformity):**
- `server.rs:514` to_value, `store.rs:33` to_string, `store.rs:283` INSERT…RETURNING,
  `posting.rs:115` to_vec, `comp.rs:53` from_utf8, `github_careers.rs:200` "USD" constant.

**Verdict:** the persisted-JSON read path IS the panic surface. No hidden sprawl. H.1's
four sites are the whole practical exposure; H.2 stays as the backstop for the unforeseen.

---

## G.3 — emitted schema-construct census — DONE

Dumped all 8 tools' input + output schemas from the live server and enumerated every
construct. One real hit, two low notes.

**Finding 1 (real — the next boolean-bug candidate): `$ref` + `$defs` in `mark_obit.in`.**
`mark_obit`'s input renders `kind` as `{"$ref": "#/$defs/ObitKind"}` with a sibling
`$defs: {ObitKind: {enum...}}` — schemars' default for a named enum type. Spec-legal, but
it is the SAME shape of risk as the boolean bug: a strict-but-incomplete client validator
(Claude Code's is Zod-based and has demonstrable gaps) must RESOLVE the internal `$ref` or
the tool fails to validate — and one failed tool killed the entire listing last time.
- NOT biting Claude Code today: the server loads and `mark_obit` works in dogfooding, so CC
  resolves the ref. But it's a portability landmine for any client with a weaker validator,
  and it's needless fragility.
- Fix: inline the enum (drop `$ref`/`$defs`) so `kind` carries `{"type":"string","enum":[...]}`
  inline — the same robustness move as the `JsonObject` pin. → spawns a remediation task.

**Finding 2 (low): non-standard `format` values — `int64` (×3), `uint` (×1).**
Not in the JSON Schema format registry. `format` is annotation-only by spec, so a
conformant validator ignores unknowns — but a client that asserts format could choke. Low
risk; leave unless the I.1 validator flags it.

**Finding 3 (low): `$schema` declaration on every tool-schema root (×15).**
schemars stamps `"$schema": ".../2020-12/schema"`. CC accepts it; some clients strip it or
pin a draft. Low risk; note only.

**Non-issue:** the probe flagged `fetch_board.in/properties/full/default: false` as a
boolean subschema — FALSE POSITIVE. That's a boolean *value* for the `default` keyword (the
default of the `full` bool arg), not a subschema. The real boolean-subschema class stays
clean (`JsonObject` pin + e2e walker hold).

**Verdict:** one real portability/fragility hit (`$ref`/`$defs` in `mark_obit`), two low
notes. I.1's conformance validator should assert "no `$ref`/`$defs` in emitted schemas" so
this can't regress, and the `ObitKind` enum should be inlined. → new remediation task I.4.

---

## G.4 — error-legibility census — DONE

Classified every failure path by what the CALLER sees. Two error channels, both legible:
JSON-RPC protocol errors (code `-32602` invalid_params) for our `McpError`, and tool-level
`isError` text for rmcp arg-deserialization.

**LEGIBLE & caller-actionable (fix your input):**
- unknown board → `invalid_params "unknown board: {id}"`
- posting not found → `invalid_params "posting not found: {req_id}"`
- missing / wrong-type args → tool `isError` "failed to deserialize parameters: missing
  field `req_id`" / "invalid type: integer `123`, expected a string" (rmcp-owned; names the
  field + the type)
- dump_captures file ops → `internal_error "creating/writing {path}: {io err}"`

**LEGIBLE, not caller-fixable (honest "broken / transient"):**
- board unreachable → `internal_error "board unreachable: HTTP {status}"`  (retry signal)
- transport → `internal_error "transport error: {detail}"`  (retry signal)
- parse drift → `internal_error "parse drift while reading {context}: {detail}"`  (dev-facing)
- store → `internal_error "store error: {source chain}"`  (FIXED 0.5.2; was the one
  dropped-detail gap)

**ILLEGIBLE — the one hole:**
- handler panic → NO response, silent hang (per G.1). No code, no message. THE legibility
  failure. → H.1 removes the sources, H.2 converts the residual into a legible
  `internal_error`.

**Startup (operator-facing, not a caller path):**
- no / bad config → clean stderr + nonzero exit.
- schema-contract rejection (`$ref` / boolean) → opaque "tools fetch failed" on the CLIENT;
  not our message to own — mitigation is prevention (G.3, I.1).

**Cross-cutting → feeds I.2:** every legible error is PROSE. A calling agent must
string-match to pick retry (transient) vs fix-input (invalid_params) vs report (parse
drift / store). The census yields the taxonomy a typed `data` payload would encode:
`bad_input | transient_remote | broken_adapter | store | internal(panic)`. That taxonomy
is the concrete input to the I.2 structured-error spike.

---

## Phase G verdict

The server is markedly more robust than today's two loud bugs implied. Real exposure is
narrow and now mapped:

1. **Persisted-JSON read-path panics → silent hang** (H.1) — the ONLY illegible caller
   failure, and the whole practical panic surface (G.1 + G.2).
2. **`$ref`/`$defs` in `mark_obit`** (I.4) — a portability landmine, same class as the
   boolean bug: spec-legal, but a weak client validator can reject it and one bad tool
   sinks the listing.

Everything else is legible and/or guarded by construction. H.2 (panic boundary), H.3
(adapter audit), and the I.* guardrails are hardening + regression prevention, not
fire-fighting. The two loud bugs were the exception, not the tip of an iceberg.

---

## Remediations

### H.1 — read-path expects → `StoreError::Corrupt` — DONE

The four persisted-JSON reads (changed_fields, obit kind, capture ats ×2) now propagate a
typed `StoreError::Corrupt { what, source }` instead of `.expect()`-panicking. Each
map-closure collects into `Result<Vec<_>, StoreError>`; the error flows through `store_err`'s
source-chain walk unchanged.

Verified end-to-end: corrupted `obits.kind` under a running server → `list_obits` returned
in 0.05s with `-32603 "store error: corrupt stored data: obit kind: expected value at line
1 column 1"` (all three layers present), and the next call still succeeded. The silent hang
(G.1) is gone for the read path. Unit test: `a_corrupt_stored_row_is_a_typed_error_not_a_panic`.

### H.2 — handler panic boundary — DONE

G.1 showed rmcp contains a panic (process + connection survive) but black-holes the request
(silent hang). H.1 removed the known panic sources; this is the backstop for the unforeseen.
`call_tool` is now hand-written (the `#[tool_handler]` macro only generates it when we don't,
so `list_tools`/`get_tool` still come from the macro) and wraps the router call in
`catch_handler_panic` — `AssertUnwindSafe(fut).catch_unwind()`, sound because server state is
an `Arc<Inner>` with no torn-invariant risk. A caught panic becomes
`internal_error "tool handler panicked: {msg}"`.

Verified end-to-end: a temp panicking tool that black-holed in G.1 now returns in 0.22s with
`-32603 "tool handler panicked: ..."`, connection still alive. Unit tests:
`catch_handler_panic_turns_a_panic_into_a_legible_error`, `..._passes_a_normal_result_through`.

With H.1 + H.2, the "silent hang" failure mode is closed: known panic sources are typed
errors, and any unforeseen panic is caught and reported. No caller request can black-hole.

### H.3 — adapter JSON-access audit + fix — DONE

Audited all 8 adapters + parse.rs for required-field (req_id / title / url) access that
silently yields empty/default instead of `ParseDrift`. Verdict: 7 GUARDED, 1 GAP (fixed).

- GUARDED: required fields are non-`Option` serde fields, or built via `format!` from required
  parts (smartrecruiters, workable, workday LIST path), or fall back to a VALID constructed
  url (github_careers → `https://www.github.careers/jobs/{slug}`). Optional fields
  (description, locations, comp, dates, workplace) default correctly. `parse.rs::interval`
  defaults to `""` → `ParseDrift` on an unrecognized unit (by design). Zero `.unwrap()` on
  payload data (confirms G.2).
- GAP → FIXED: workday `detail_from` (the `fetch_posting` path) did
  `url: external_url.unwrap_or_default()` — a required url silently emptied when Workday's
  detail omits `externalUrl`. Now `ParseDrift`, matching the list path (already hard on a
  titled posting missing its req id). No existing detail test broke, so healthy Workday
  details carry `externalUrl`; the drift fires only on genuine absence. Test:
  `a_detail_with_no_external_url_is_drift_not_an_empty_url`.

Note: the list path's `"<unknown>"` req label (workday.rs:131) is SAFE — it labels a SKIPPED
stub (no title/path), never a real posting.

## Phase H verdict

The silent-failure surface is closed. Panics (read-path + unforeseen) are legible errors, and
the one adapter gap that let a garbage posting through is a hard drift. Only I.4 (inline the
`mark_obit` enum) remains as a concrete fragility fix; the rest of I is regression guards.

### I.4 — inline the `ObitKind` enum — DONE

`#[schemars(inline)]` on `ObitKind` (its `inline_schema()` → true). `mark_obit.kind` now renders
`{"enum":["dead","rejected","out_of_scope","ghost"],"type":"string"}` inline — no `$ref`, no
`$defs`. Live probe confirms ZERO tools carry `$ref`/`$defs` anywhere. The doc comment survives
as the schema `description`.

### I.1 — schema client-safety conformance test — DONE

Broadened the e2e schema walker (was boolean-subschemas only) into `assert_client_safe_schema`,
run over every tool's input + output schema in `tool_schemas_are_client_safe`. It now bans BOTH
failure classes this server has hit: boolean subschemas in `properties`/`items` AND
`$ref`/`$defs`/`definitions` anywhere. Negative control verified: removing `#[schemars(inline)]`
makes it fail with `client-unsafe $defs at mark_obit/inputSchema/$defs`. A future enum/struct
added to a tool without inlining now fails CI instead of shipping a listing-killer.

This is a client-COMPAT conformance check, not JSON-Schema meta-validation: both banned
constructs are valid schema — the point is what a real MCP client's validator REJECTS. A
meta-schema validator (the `jsonschema` crate) would pass all of them and miss the actual risk,
so it's deliberately not used.

### I.2 — typed error `data` payload — DONE (adopted)

The spike found `ErrorData` has a `data: Option<Value>` we were passing `None`. Adopted: an
`ErrorKind` taxonomy (`bad_input` | `transient_remote` | `broken_adapter` | `store` |
`internal`) mirrored into `data` as `{"kind","retryable"}` on every `McpError`. Only
`transient_remote` is `retryable: true`. Wire-verified: unknown board returns
`{"code":-32602,"message":"unknown board: nope","data":{"kind":"bad_input","retryable":false}}`.

Caveats (from the spike, unchanged by adopting): `data` rides only the JSON-RPC-error channel —
arg-deserialize errors come back as tool `isError` text with no `data` — and whether a client
surfaces `error.data` to the agent is client-dependent. So the source-chained MESSAGE stays the
portable, always-visible signal; `data` is the machine-branchable upside where clients honor it.
Tests: `error_kind_data_encodes_kind_and_retryable`, `adapter_err_tags_*`.
