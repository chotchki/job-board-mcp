# Failure modes — job-board-mcp

Working census (phase G) that phase I.3 graduates into a failure-mode contract. Each
entry: what fails, the blast radius, what the CALLER sees, and where the fix lands.

The thesis behind the whole sweep: today's two loud bugs were both "one bad input →
total, illegible failure." Find the latent versions before they're loud.

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
