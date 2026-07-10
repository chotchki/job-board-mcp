//! Parsing helpers shared across adapters. One home for the field-level conversions
//! several ATSes need, so timestamp and money parsing isn't re-implemented per adapter.
//! Every one fails loud on a present-but-malformed value ([`AdapterError::ParseDrift`])
//! and treats a genuinely-absent value as `None` — never a guess.

use chrono::{DateTime, Utc};

use super::AdapterError;
use crate::model::{CompInterval, decimal_to_minor};

/// An RFC 3339 timestamp string. Present-but-unparseable is drift; absent is `None`.
pub(crate) fn rfc3339(
    context: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, AdapterError> {
    match value {
        None => Ok(None),
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|e| AdapterError::drift(context, format!("bad timestamp {s:?}: {e}"))),
    }
}

/// A Unix epoch in milliseconds (Lever's `createdAt`). Out-of-range or absent is `None`.
pub(crate) fn epoch_millis(value: Option<i64>) -> Option<DateTime<Utc>> {
    value.and_then(DateTime::<Utc>::from_timestamp_millis)
}

/// A bare calendar date `YYYY-MM-DD` (Workday's `startDate`), taken as midnight UTC.
/// Present-but-unparseable is drift; absent is `None`.
pub(crate) fn date(
    context: &str,
    value: Option<&str>,
) -> Result<Option<DateTime<Utc>>, AdapterError> {
    match value {
        None => Ok(None),
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Some(dt.and_utc()))
            .ok_or_else(|| AdapterError::drift(context, format!("bad date {s:?}"))),
    }
}

/// Map an ATS interval string onto [`CompInterval`] by its unit word — handles both
/// Ashby's `"1 YEAR"` and Lever's `"per-year-salary"`. An unrecognized unit is drift,
/// not a guessed `Year`, so a new cadence can't silently mislabel a band.
pub(crate) fn interval(context: &str, value: Option<&str>) -> Result<CompInterval, AdapterError> {
    let raw = value.unwrap_or_default().to_uppercase();
    if raw.contains("YEAR") {
        Ok(CompInterval::Year)
    } else if raw.contains("MONTH") {
        Ok(CompInterval::Month)
    } else if raw.contains("WEEK") {
        Ok(CompInterval::Week)
    } else if raw.contains("DAY") {
        Ok(CompInterval::Day)
    } else if raw.contains("HOUR") {
        Ok(CompInterval::Hour)
    } else {
        Err(AdapterError::drift(
            context,
            format!("unrecognized interval {value:?}"),
        ))
    }
}

/// Convert a JSON number (a currency amount in major units) into integer minor units —
/// through the string form, so no float ever touches the money path. A value that can't
/// be represented (too much precision for the currency, negative) is drift.
pub(crate) fn number_to_minor(
    context: &str,
    value: Option<&serde_json::Number>,
    exponent: u32,
) -> Result<Option<i64>, AdapterError> {
    match value {
        None => Ok(None),
        Some(n) => decimal_to_minor(&n.to_string(), exponent)
            .map(Some)
            .ok_or_else(|| {
                AdapterError::drift(context, format!("cannot represent {n} in minor units"))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_reads_the_unit_word() {
        // Ashby's "1 YEAR" and Lever's "per-year-salary" both resolve.
        assert_eq!(interval("ctx", Some("1 YEAR")).unwrap(), CompInterval::Year);
        assert_eq!(
            interval("ctx", Some("per-year-salary")).unwrap(),
            CompInterval::Year
        );
        assert_eq!(
            interval("ctx", Some("per-hour")).unwrap(),
            CompInterval::Hour
        );
        assert!(interval("ctx", Some("1 FORTNIGHT")).is_err());
        assert!(interval("ctx", None).is_err());
    }

    #[test]
    fn number_to_minor_is_drift_when_unrepresentable() {
        let n: serde_json::Number = serde_json::from_str("63000").unwrap();
        assert_eq!(
            number_to_minor("ctx", Some(&n), 2).unwrap(),
            Some(6_300_000)
        );
        assert_eq!(number_to_minor("ctx", None, 2).unwrap(), None);
        // A negative amount can't become minor units → drift, not a silent 0.
        let neg: serde_json::Number = serde_json::from_str("-5").unwrap();
        assert!(number_to_minor("ctx", Some(&neg), 2).is_err());
    }

    #[test]
    fn epoch_millis_round_trips() {
        // Lever createdAt: a real millisecond epoch.
        assert!(epoch_millis(Some(1_767_044_568_142)).is_some());
        assert_eq!(epoch_millis(None), None);
    }
}
