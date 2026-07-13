//! Closed enums over what used to be stringly-typed status fields. Exhaustive
//! `match` beats string comparison, and an unknown value fails loud at deserialize
//! instead of silently flowing through as data.

use serde::{Deserialize, Serialize};

/// The ATS platform backing a board. Only the wave-1 adapters exist as variants —
/// a config naming an unimplemented platform fails to deserialize with a clear
/// "unknown variant" error rather than parsing into a board nothing can fetch.
/// Wave 2 (Workday, Workable, SmartRecruiters, Rippling, github.careers) adds
/// variants here, and the compiler then forces every `match` to handle them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ats {
    Greenhouse,
    Ashby,
    Lever,
    Workday,
    #[serde(rename = "smartrecruiters")]
    SmartRecruiters,
    Rippling,
    GithubCareers,
    Workable,
}

/// Where the work happens. `Unknown` is the honest default for a board that doesn't
/// say — never guessed from noise like Ashby's board-wide `isRemote` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkplaceType {
    Onsite,
    Hybrid,
    Remote,
    #[default]
    Unknown,
}

/// Why a posting is suppressed from future NEW results. `ghost` is the load-bearing
/// one: an aggregator listing that never existed on a primary source, which re-bites
/// a scan endlessly without a ledger to remember it's a phantom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
// Inline the enum into any schema that references it, rather than emitting a `$ref` into a
// `$defs` block. A `$ref` is spec-legal but a weak MCP client validator may not resolve it,
// and one rejected tool sinks the whole listing (docs/failure-modes.md G.3) — same class as
// the boolean-subschema bug. mark_obit is the only tool carrying this enum.
#[schemars(inline)]
pub enum ObitKind {
    Dead,
    Rejected,
    OutOfScope,
    Ghost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_use_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_string(&Ats::Greenhouse).unwrap(),
            "\"greenhouse\""
        );
        assert_eq!(
            serde_json::to_string(&WorkplaceType::Onsite).unwrap(),
            "\"onsite\""
        );
        assert_eq!(
            serde_json::to_string(&ObitKind::OutOfScope).unwrap(),
            "\"out_of_scope\""
        );
    }

    #[test]
    fn unknown_ats_variant_fails_loud() {
        // A wave-2 board named before its adapter exists must NOT parse silently.
        let err = serde_json::from_str::<Ats>("\"jobvite\"");
        assert!(err.is_err(), "unimplemented ATS should fail to deserialize");
    }

    #[test]
    fn workplace_type_defaults_to_unknown() {
        assert_eq!(WorkplaceType::default(), WorkplaceType::Unknown);
    }
}
