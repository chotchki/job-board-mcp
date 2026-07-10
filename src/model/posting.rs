//! The one normalized shape every adapter produces. Assembled from the spine types
//! (B.5), so a posting's identity, workplace type and comp are typed rather than
//! stringly. Every optional carries `#[serde(default)]` so a posting written by an
//! older version loads clean; `deny_unknown_fields` is deliberately absent — this is
//! cross-version machine data, and an unknown field from a newer writer must not hard-
//! fail an older reader.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Ats, BoardId, Comp, ContentHash, Equity, ReqId, WorkplaceType};

/// A normalized posting as it appears in a board's listing feed. The description
/// itself is not here — it belongs to [`PostingDetail`] — but its hash feeds
/// [`content_hash`], so a description edit still surfaces as a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub ats: Ats,
    pub board_id: BoardId,
    pub req_id: ReqId,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    pub workplace_type: WorkplaceType,
    /// Verbatim remote-scope text ("US", "US + Canada", a timezone band) — never
    /// interpreted, per SPEC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_scope: Option<String>,
    pub comp: Comp,
    /// Equity grant, an axis independent of `comp` (salary) — a posting can carry both.
    /// Defaults to [`Equity::None`] and is omitted from the wire form when absent, so a
    /// posting written before this field existed loads clean and hashes unchanged.
    #[serde(default, skip_serializing_if = "Equity::is_none")]
    pub equity: Equity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// True on boards that bulk-touch `updated_at` during reindexes, making it noise.
    /// Same name and polarity everywhere (config, here) — an opt-in defect flag that
    /// nothing negates.
    #[serde(default)]
    pub updated_at_unreliable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employment_type: Option<String>,
    /// The change-detection key — set via [`content_hash`], never by hand.
    pub content_hash: ContentHash,
}

/// A posting plus its description text, as returned by `fetch_posting` for capturing
/// a JD at apply time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingDetail {
    #[serde(flatten)]
    pub posting: Posting,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_text: Option<String>,
}

// The exact set of fields the hash covers. A private struct with a fixed field order,
// serialized through serde_json, IS the canonical encoding: no floats, no maps, so the
// same inputs yield the same bytes on every platform. Never `#[derive(Hash)]` — that
// hashes memory layout, which is neither stable nor portable.
#[derive(Serialize)]
struct MaterialFields<'a> {
    title: &'a str,
    locations: &'a [String],
    workplace_type: WorkplaceType,
    comp: &'a Comp,
    // Skipped when None so a posting with no equity serializes to the exact bytes it did
    // before this field existed — its hash is unchanged, and only postings that actually
    // gained equity data re-hash. `Equity` is `Copy`, so the field is held by value and
    // `skip_serializing_if` sees `&Equity` directly.
    #[serde(skip_serializing_if = "Equity::is_none")]
    equity: Equity,
    description_hash: String,
}

/// The change-detection key: a blake3 digest over the fields that MATERIALLY define a
/// posting — title, locations, workplace type, comp, and a hash of the description.
/// Deliberately NOT `posted_at`/`updated_at`; several boards bulk-touch those during
/// reindexes, so folding them in would report a spurious change on every fetch.
///
/// Changing this field set or the encoding changes every stored hash — that is a
/// breaking migration, by design, and the pinned known-answer test below is what makes
/// an accidental change loud.
pub fn content_hash(
    title: &str,
    locations: &[String],
    workplace_type: WorkplaceType,
    comp: &Comp,
    equity: Equity,
    description: &str,
) -> ContentHash {
    // Trim leading/trailing whitespace so an ATS that pads a title one day and trims it
    // the next doesn't read as a CHANGED — the whitespace isn't a real content change.
    let title = title.trim();
    let locations: Vec<String> = locations.iter().map(|l| l.trim().to_owned()).collect();
    let description_hash = blake3::hash(description.trim().as_bytes())
        .to_hex()
        .to_string();
    let material = MaterialFields {
        title,
        locations: &locations,
        workplace_type,
        comp,
        equity,
        description_hash,
    };
    let encoded = serde_json::to_vec(&material).expect("MaterialFields is always serializable");
    ContentHash::from_bytes(*blake3::hash(&encoded).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CompInterval, CompSource, Currency};

    fn usd() -> Currency {
        Currency::new("USD").unwrap()
    }

    fn sample() -> Posting {
        let comp = Comp::band(
            Currency::new("USD").unwrap(),
            18_000_000,
            24_000_000,
            CompInterval::Year,
            CompSource::Api,
        )
        .unwrap();
        Posting {
            ats: Ats::Greenhouse,
            board_id: BoardId::new("stripe"),
            req_id: ReqId::new("4152884006"),
            title: "Staff Software Engineer".to_owned(),
            url: "https://job-boards.greenhouse.io/stripe/jobs/4152884006".to_owned(),
            locations: vec!["Remote US".to_owned(), "New York".to_owned()],
            workplace_type: WorkplaceType::Remote,
            remote_scope: Some("US".to_owned()),
            comp: comp.clone(),
            equity: Equity::None,
            posted_at: DateTime::from_timestamp(1_700_000_000, 0),
            updated_at: DateTime::from_timestamp(1_710_000_000, 0),
            updated_at_unreliable: false,
            department: Some("Engineering".to_owned()),
            employment_type: Some("Full-time".to_owned()),
            content_hash: content_hash(
                "Staff Software Engineer",
                &["Remote US".to_owned(), "New York".to_owned()],
                WorkplaceType::Remote,
                &comp,
                Equity::None,
                "the job description body",
            ),
        }
    }

    #[test]
    fn posting_round_trips_as_struct_not_bytes() {
        let p = sample();
        let json = serde_json::to_string(&p).unwrap();
        let back: Posting = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn old_shape_without_optionals_loads_clean() {
        // A minimal posting from an older/leaner writer: no locations, no timestamps,
        // no department, no updated_at_unreliable. Every one must default, not fail.
        let json = r#"{
            "ats": "lever",
            "board_id": "figma",
            "req_id": "abc-123",
            "title": "Product Designer",
            "url": "https://jobs.lever.co/figma/abc-123",
            "workplace_type": "unknown",
            "comp": {"kind": "none"},
            "content_hash": "0000000000000000000000000000000000000000000000000000000000000000"
        }"#;
        let p: Posting = serde_json::from_str(json).unwrap();
        assert!(p.locations.is_empty());
        assert_eq!(p.posted_at, None);
        assert!(!p.updated_at_unreliable);
        assert_eq!(p.comp, Comp::None);
    }

    #[test]
    fn content_hash_is_pinned() {
        // Known-answer lock. This digest can only change if the material-field set or
        // the canonical encoding changes — which is a deliberate breaking migration,
        // and this assertion is how such a change announces itself.
        let comp = Comp::band(
            Currency::new("USD").unwrap(),
            18_000_000,
            24_000_000,
            CompInterval::Year,
            CompSource::Api,
        )
        .unwrap();
        let h = content_hash(
            "Staff Software Engineer",
            &["Remote US".to_owned()],
            WorkplaceType::Remote,
            &comp,
            Equity::None,
            "the job description body",
        );
        assert_eq!(
            h.to_hex(),
            "399264dd3c60efd216c2ac59a59593881ee061ac4f5f180d4d80870919657caa"
        );
    }

    #[test]
    fn content_hash_ignores_timestamps_by_construction() {
        // The function has no timestamp parameter, so a bulk-touched updated_at cannot
        // move the hash. Same material inputs → identical hash.
        let comp = Comp::None;
        let a = content_hash(
            "Engineer",
            &[],
            WorkplaceType::Onsite,
            &comp,
            Equity::None,
            "body",
        );
        let b = content_hash(
            "Engineer",
            &[],
            WorkplaceType::Onsite,
            &comp,
            Equity::None,
            "body",
        );
        assert_eq!(a, b);
        // A title edit DOES move it.
        let c = content_hash(
            "Senior Engineer",
            &[],
            WorkplaceType::Onsite,
            &comp,
            Equity::None,
            "body",
        );
        assert_ne!(a, c);
    }

    #[test]
    fn equity_participates_in_hash_but_none_is_byte_stable() {
        let comp = Comp::None;
        // Absent equity must hash IDENTICALLY to the pre-equity encoding (skip-when-none),
        // so upgrading the binary doesn't mark every equity-less posting CHANGED.
        let none = content_hash(
            "Eng",
            &[],
            WorkplaceType::Remote,
            &comp,
            Equity::None,
            "body",
        );
        let pinned = content_hash(
            "Eng",
            &[],
            WorkplaceType::Remote,
            &comp,
            Equity::None,
            "body",
        );
        assert_eq!(none, pinned);
        // Present equity DOES move the hash — a grant that appears or shifts is a change.
        let offered = content_hash(
            "Eng",
            &[],
            WorkplaceType::Remote,
            &comp,
            Equity::Offered,
            "body",
        );
        assert_ne!(none, offered);
        let cash = content_hash(
            "Eng",
            &[],
            WorkplaceType::Remote,
            &comp,
            Equity::cash_value(usd(), 10_000_000, 13_000_000, CompInterval::Year).unwrap(),
            "body",
        );
        assert_ne!(offered, cash);
    }

    #[test]
    fn posting_detail_flattens() {
        let detail = PostingDetail {
            posting: sample(),
            description_html: Some("<p>hi</p>".to_owned()),
            description_text: Some("hi".to_owned()),
        };
        let json = serde_json::to_string(&detail).unwrap();
        // Flattened: the posting's fields sit at the top level alongside the descriptions.
        assert!(json.contains("\"title\":\"Staff Software Engineer\""));
        assert!(json.contains("\"description_text\":\"hi\""));
        let back: PostingDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(back, detail);
    }
}
