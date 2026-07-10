#![deny(clippy::float_arithmetic)]
//! Equity as its own axis, held separately from salary [`Comp`]. A posting routinely
//! publishes BOTH — Ashby carries a `Salary` and an `EquityCashValue` component on the
//! same job — so equity is deliberately NOT a `Comp` variant: folding it in would force
//! salary XOR equity and silently drop one. The two dimensions are independent, so they
//! get independent fields.
//!
//! The integer discipline from [`super::comp`] carries over. A cash-value grant rides
//! the same minor-units path money does, and a percentage grant lives in BASIS POINTS
//! (1 bp = 0.01%), never a float — because the same re-parse must hash identically or a
//! band that didn't move reports CHANGED, the exact failure this project exists to kill.

use serde::{Deserialize, Serialize};

use super::comp::{CompError, CompInterval, Currency};

/// Equity compensation, as a closed set. `Copy` so it can sit by value in the
/// content-hash material struct (which lets `skip_serializing_if` see `&Equity`
/// directly). Construct the amount-bearing variants through [`Equity::cash_value`] /
/// [`Equity::percent`] — or deserialize, which runs the same checks — never a struct
/// literal, which would skip the invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "EquityRaw")]
pub enum Equity {
    /// No equity mentioned anywhere.
    #[default]
    None,
    /// Equity is offered but no usable numbers are published — a component present with
    /// null min/max (OpenAI's unfilled tiers), or a form this build won't yet quantify
    /// without guessing its units (see [`Equity::Percent`]). Distinct from `None`:
    /// "offers equity" is real signal even without a figure.
    Offered,
    /// An annualized cash-value grant in one currency's integer minor units.
    /// `min_minor == max_minor` is a point rather than a range. This is Ashby's
    /// `EquityCashValue`.
    CashValue {
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
    },
    /// A percentage grant in BASIS POINTS (1 bp = 0.01%, so 0.25% == 25 bp). Integer, so
    /// a re-parse hashes identically. This is Ashby's `EquityPercentage` — but it is NOT
    /// emitted until a populated sample pins the provider's raw scale; a guessed
    /// multiplier is the silent-wrong-number failure this project refuses. Until then a
    /// percentage grant surfaces as [`Equity::Offered`].
    Percent { min_bps: i64, max_bps: i64 },
}

impl Equity {
    /// True only for [`Equity::None`]. Used by the content-hash to skip the field when
    /// absent, so a posting with no equity hashes byte-identically to one written before
    /// this field existed — no spurious CHANGED on upgrade.
    pub fn is_none(&self) -> bool {
        matches!(self, Equity::None)
    }

    pub fn cash_value(
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
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
        Ok(Self::CashValue {
            currency,
            min_minor,
            max_minor,
            interval,
        })
    }

    pub fn percent(min_bps: i64, max_bps: i64) -> Result<Self, CompError> {
        if min_bps < 0 {
            return Err(CompError::NegativeAmount(min_bps));
        }
        if max_bps < min_bps {
            return Err(CompError::MinExceedsMax {
                min: min_bps,
                max: max_bps,
            });
        }
        Ok(Self::Percent { min_bps, max_bps })
    }
}

// Deserialize mirror: serde fills this un-validated shape, then `TryFrom` re-runs the
// constructor checks, so a hand-edited DB or a bad fixture can't load an inverted band.
// Structurally identical to `Equity`; the round-trip test fails if the two ever drift.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EquityRaw {
    None,
    Offered,
    CashValue {
        currency: Currency,
        min_minor: i64,
        max_minor: i64,
        interval: CompInterval,
    },
    Percent {
        min_bps: i64,
        max_bps: i64,
    },
}

impl TryFrom<EquityRaw> for Equity {
    type Error = CompError;
    fn try_from(raw: EquityRaw) -> Result<Self, Self::Error> {
        match raw {
            EquityRaw::None => Ok(Equity::None),
            EquityRaw::Offered => Ok(Equity::Offered),
            EquityRaw::CashValue {
                currency,
                min_minor,
                max_minor,
                interval,
            } => Equity::cash_value(currency, min_minor, max_minor, interval),
            EquityRaw::Percent { min_bps, max_bps } => Equity::percent(min_bps, max_bps),
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
    fn cash_value_enforces_non_negative_and_min_le_max() {
        assert!(Equity::cash_value(usd(), 10_100_000, 13_800_000, CompInterval::Year).is_ok());
        // min == max is a point, allowed.
        assert!(Equity::cash_value(usd(), 500, 500, CompInterval::Year).is_ok());
        assert_eq!(
            Equity::cash_value(usd(), 13_800_000, 10_100_000, CompInterval::Year),
            Err(CompError::MinExceedsMax {
                min: 13_800_000,
                max: 10_100_000
            })
        );
        assert_eq!(
            Equity::cash_value(usd(), -1, 100, CompInterval::Year),
            Err(CompError::NegativeAmount(-1))
        );
    }

    #[test]
    fn percent_lives_in_basis_points_and_validates() {
        assert!(Equity::percent(5, 25).is_ok());
        assert_eq!(
            Equity::percent(25, 5),
            Err(CompError::MinExceedsMax { min: 25, max: 5 })
        );
        assert_eq!(Equity::percent(-3, 5), Err(CompError::NegativeAmount(-3)));
    }

    #[test]
    fn round_trips_through_json() {
        for equity in [
            Equity::None,
            Equity::Offered,
            Equity::cash_value(usd(), 10_100_000, 13_800_000, CompInterval::Year).unwrap(),
            Equity::percent(5, 25).unwrap(),
        ] {
            let json = serde_json::to_string(&equity).unwrap();
            let back: Equity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, equity, "round-trip drifted for {json}");
        }
    }

    #[test]
    fn deserializing_an_inverted_cash_band_is_rejected() {
        let json = r#"{"kind":"cash_value","currency":"USD","min_minor":500,"max_minor":100,"interval":"year"}"#;
        assert!(serde_json::from_str::<Equity>(json).is_err());
    }

    #[test]
    fn none_and_offered_serialize_as_tagged_objects() {
        assert_eq!(
            serde_json::to_string(&Equity::None).unwrap(),
            r#"{"kind":"none"}"#
        );
        assert_eq!(
            serde_json::to_string(&Equity::Offered).unwrap(),
            r#"{"kind":"offered"}"#
        );
    }

    #[test]
    fn is_none_only_for_none() {
        assert!(Equity::None.is_none());
        assert!(!Equity::Offered.is_none());
        assert!(!Equity::percent(5, 25).unwrap().is_none());
    }
}
