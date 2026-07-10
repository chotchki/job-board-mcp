#![deny(clippy::float_arithmetic)]
//! Compensation as integer minor units, end to end. The ban above is the point:
//! money never touches a float on this path, so `"$180,000"` encodes to the same
//! integer on every parse and a band that didn't change hashes the same. A float
//! path invites `parse::<f64>() * 100.0` and the rounding drift behind it — which
//! would report a spurious CHANGE on every fetch, the exact failure this project
//! exists to kill.
//!
//! Caveat worth knowing (clippy 1.96): `float_arithmetic` is silently suppressed
//! inside `#[test]` fns, so the real guard is the `i64` type, not the lint — the
//! lint just backs it up in non-test code.

use rusqlite::types::{FromSql, FromSqlResult, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// Things that go wrong constructing compensation from untrusted board data.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompError {
    #[error("comp band min {min} exceeds max {max}")]
    MinExceedsMax { min: i64, max: i64 },
    #[error("comp amount {0} is negative")]
    NegativeAmount(i64),
    #[error("invalid ISO-4217 currency code: {0:?}")]
    InvalidCurrency(String),
}

/// A validated ISO-4217 alpha-3 currency code (three ASCII uppercase letters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Currency([u8; 3]);

// Minor-unit exponents that deviate from the default of 2, per ISO-4217. Presentation
// only — never needed to hash or compare, so this table can grow without touching the
// change-detection path.
const EXPONENT_0: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "UYI", "VND",
    "VUV", "XAF", "XOF", "XPF",
];
const EXPONENT_3: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];
const EXPONENT_4: &[&str] = &["CLF", "UYW"];

impl Currency {
    pub fn new(code: &str) -> Result<Self, CompError> {
        let bytes = code.as_bytes();
        if bytes.len() == 3 && bytes.iter().all(|b| b.is_ascii_uppercase()) {
            Ok(Self([bytes[0], bytes[1], bytes[2]]))
        } else {
            Err(CompError::InvalidCurrency(code.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        // Safe by construction: `new` admits only ASCII uppercase bytes.
        std::str::from_utf8(&self.0).expect("Currency holds validated ASCII")
    }

    /// Decimal places this currency's minor unit carries — 2 for most, 0 for the
    /// likes of JPY, 3 for KWD. Derived here so it lives in exactly one place.
    pub fn minor_unit_exponent(&self) -> u32 {
        let code = self.as_str();
        if EXPONENT_0.contains(&code) {
            0
        } else if EXPONENT_3.contains(&code) {
            3
        } else if EXPONENT_4.contains(&code) {
            4
        } else {
            2
        }
    }
}

impl TryFrom<String> for Currency {
    type Error = CompError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl From<Currency> for String {
    fn from(value: Currency) -> Self {
        value.as_str().to_owned()
    }
}

impl FromSql for Currency {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        Self::new(s).map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

impl rusqlite::ToSql for Currency {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// The period a comp figure is quoted over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompInterval {
    Year,
    Month,
    Week,
    Day,
    Hour,
}

/// Where an amount-bearing comp figure was read from. `site_only` and `none` are
/// [`Comp`] variants, not sources — a source always accompanies a real number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompSource {
    Api,
    Body,
}

/// Compensation, as a closed set so illegal states can't be represented: no
/// min-without-max, no currency-without-amount, no band whose ends are different
/// currencies. Construct amount-bearing variants through [`Comp::point`] /
/// [`Comp::band`] (or deserialize, which runs the same checks) — never a struct
/// literal, which would skip the invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "CompRaw")]
pub enum Comp {
    /// Nothing published anywhere — including a board that says "competitive".
    None,
    /// A band exists, but only on the company's rendered site; the API won't tell you.
    SiteOnly,
    /// A single figure rather than a range.
    Point {
        currency: Currency,
        amount_minor: i64,
        interval: CompInterval,
        source: CompSource,
    },
    /// A `[min, max]` range in one currency, `min_minor <= max_minor` guaranteed.
    Band {
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
        source: CompSource,
    },
}

impl Comp {
    pub fn point(
        currency: Currency,
        amount_minor: i64,
        interval: CompInterval,
        source: CompSource,
    ) -> Result<Self, CompError> {
        if amount_minor < 0 {
            return Err(CompError::NegativeAmount(amount_minor));
        }
        Ok(Self::Point {
            currency,
            amount_minor,
            interval,
            source,
        })
    }

    pub fn band(
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
        source: CompSource,
    ) -> Result<Self, CompError> {
        if min_minor < 0 {
            return Err(CompError::NegativeAmount(min_minor));
        }
        if max_minor < min_minor {
            return Err(CompError::MinExceedsMax {
                min: min_minor,
                max: max_minor,
            });
        }
        Ok(Self::Band {
            currency,
            min_minor,
            max_minor,
            interval,
            source,
        })
    }
}

// Deserialize mirror: serde fills this un-validated shape, then `TryFrom` runs the
// same checks the constructors do, so a hand-edited DB or a bad fixture can't load an
// inverted band. Kept structurally identical to `Comp`; the round-trip test below
// fails if the two shapes ever drift.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CompRaw {
    None,
    SiteOnly,
    Point {
        currency: Currency,
        amount_minor: i64,
        interval: CompInterval,
        source: CompSource,
    },
    Band {
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
        source: CompSource,
    },
}

impl TryFrom<CompRaw> for Comp {
    type Error = CompError;
    fn try_from(raw: CompRaw) -> Result<Self, Self::Error> {
        match raw {
            CompRaw::None => Ok(Comp::None),
            CompRaw::SiteOnly => Ok(Comp::SiteOnly),
            CompRaw::Point {
                currency,
                amount_minor,
                interval,
                source,
            } => Comp::point(currency, amount_minor, interval, source),
            CompRaw::Band {
                currency,
                min_minor,
                max_minor,
                interval,
                source,
            } => Comp::band(currency, min_minor, max_minor, interval, source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd() -> Currency {
        Currency::new("USD").unwrap()
    }

    #[test]
    fn currency_validates() {
        assert!(Currency::new("USD").is_ok());
        assert!(Currency::new("usd").is_err());
        assert!(Currency::new("US").is_err());
        assert!(Currency::new("USDD").is_err());
        assert!(Currency::new("U$D").is_err());
    }

    #[test]
    fn minor_unit_exponents() {
        assert_eq!(Currency::new("USD").unwrap().minor_unit_exponent(), 2);
        assert_eq!(Currency::new("JPY").unwrap().minor_unit_exponent(), 0);
        assert_eq!(Currency::new("KWD").unwrap().minor_unit_exponent(), 3);
        assert_eq!(Currency::new("CLF").unwrap().minor_unit_exponent(), 4);
    }

    #[test]
    fn currency_serializes_as_a_bare_string() {
        assert_eq!(serde_json::to_string(&usd()).unwrap(), "\"USD\"");
        assert_eq!(
            serde_json::from_str::<Currency>("\"EUR\"")
                .unwrap()
                .as_str(),
            "EUR"
        );
        assert!(serde_json::from_str::<Currency>("\"eur\"").is_err());
    }

    #[test]
    fn band_enforces_min_le_max_and_non_negative() {
        assert!(
            Comp::band(
                usd(),
                18_000_000,
                24_000_000,
                CompInterval::Year,
                CompSource::Api
            )
            .is_ok()
        );
        assert!(Comp::band(usd(), 100, 100, CompInterval::Year, CompSource::Api).is_ok());
        assert_eq!(
            Comp::band(
                usd(),
                24_000_000,
                18_000_000,
                CompInterval::Year,
                CompSource::Api
            ),
            Err(CompError::MinExceedsMax {
                min: 24_000_000,
                max: 18_000_000
            })
        );
        assert_eq!(
            Comp::point(usd(), -1, CompInterval::Hour, CompSource::Body),
            Err(CompError::NegativeAmount(-1))
        );
    }

    #[test]
    fn comp_round_trips_through_json() {
        for comp in [
            Comp::None,
            Comp::SiteOnly,
            Comp::point(usd(), 20_000_000, CompInterval::Year, CompSource::Api).unwrap(),
            Comp::band(
                usd(),
                18_000_000,
                24_000_000,
                CompInterval::Year,
                CompSource::Body,
            )
            .unwrap(),
        ] {
            let json = serde_json::to_string(&comp).unwrap();
            let back: Comp = serde_json::from_str(&json).unwrap();
            assert_eq!(back, comp, "round-trip drifted for {json}");
        }
    }

    #[test]
    fn deserializing_an_inverted_band_is_rejected() {
        // The untrusted-input guard: a persisted or fixture band with min > max must
        // not load, or the invariant means nothing.
        let json = r#"{"kind":"band","currency":"USD","min_minor":500,"max_minor":100,"interval":"year","source":"api"}"#;
        assert!(serde_json::from_str::<Comp>(json).is_err());
    }

    #[test]
    fn none_serializes_as_a_tagged_object() {
        assert_eq!(
            serde_json::to_string(&Comp::None).unwrap(),
            r#"{"kind":"none"}"#
        );
        assert_eq!(
            serde_json::to_string(&Comp::SiteOnly).unwrap(),
            r#"{"kind":"site_only"}"#
        );
    }
}
