## 2026-07-10

## Phase A - Scaffold
- [x] A.1 - cargo scaffold + rmcp stdio server with a single ping tool, verified end-to-end from a real MCP client (verify current rmcp API against the rust-sdk repo first — SPEC deliberately doesn't pin it)
- [x] A.2 - CI: fmt + clippy + test on push
- [x] A.3 - chris: pick license
- [x] A.4 - README stub (one paragraph + build badge; real README is E.3)
- [x] A.5 - Lint + local harness: [lints.clippy] allow_attributes_without_reason=deny + allow_attributes=warn (reasons live in source #[expect], the manifest silently drops them); clippy.toml disallowed-methods banning SystemTime::now, chrono/rand entries commented until those deps land (an unresolved ban is silently ignored, so it is indistinguishable from a typo); .githooks/pre-push via core.hooksPath
- [x] A.6 - CI hardening: ubuntu-only lint job + tri-OS test matrix (fail-fast: false); -D warnings scoped to the clippy step rather than a global RUSTFLAGS (which pollutes the rust-cache key); rust-cache skipped on windows, where it crashes mid-restore


