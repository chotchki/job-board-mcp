//! job-board-mcp — an MCP server exposing typed, deterministic job-board tools.
//!
//! See `SPEC.md` for the design, the change semantics and the per-platform quirk table.

pub mod adapter;
pub mod config;
pub mod http;
pub mod model;
pub mod server;
pub mod store;
