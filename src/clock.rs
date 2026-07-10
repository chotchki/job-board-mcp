//! The one wall-clock read in the whole process. Every recorded timestamp — a
//! snapshot's `taken_at`, an obit's `marked_at`, a raw capture's `captured_at` — comes
//! from [`now`], so the store never reads a clock and its diffs stay reproducible. The
//! `#[expect]` is the single sanctioned exception to the determinism ban; if it ever
//! stops being needed (no `now()` call remains) the lint says so loudly.

use chrono::{DateTime, Utc};

#[expect(
    clippy::disallowed_methods,
    reason = "the sole sanctioned clock reader; the store and adapters take time as a parameter"
)]
pub fn now() -> DateTime<Utc> {
    Utc::now()
}
