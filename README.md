# job-board-mcp

[![CI](https://github.com/chotchki/job-board-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/chotchki/job-board-mcp/actions/workflows/ci.yml)

An MCP server that turns job-board scraping into a typed, deterministic tool call. It fetches postings from hosted ATS APIs (greenhouse, Ashby and Lever first; Workday and friends after), normalizes them to one schema, snapshots them in SQLite, and answers "what changed since yesterday" as a QUERY — not as a prose diff some agent re-derives from its own notes every morning. That division of labor is the whole point. Parsing a location field, or catching a title that quietly edited itself from Staff down to Senior, is mechanical work: typed code with tests does it perfectly, and an LLM does it wrong often enough that a verification phase has to exist to catch it. So the server owns the mechanics and holds no opinions, and the client model spends its tokens on the part that actually needs judgment — does this role fit, is that band real.

Design, the change semantics, and the per-platform quirk table live in [SPEC.md](SPEC.md). Build order lives in [PLAN.md](PLAN.md).

**Status:** pre-v0.1, under construction. Nothing here is stable yet.

## Development

Enable the local pre-push gate (fmt + clippy + test, mirroring CI) once per clone:

    git config core.hooksPath .githooks

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
