//! Configured-zone guide windows and catch-up timestamp parsing.

use std::str::FromStr;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use thiserror::Error;

/// The configured guide timezone (an IANA name such as `Asia/Shanghai`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuideZone(pub Tz);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GuideTimeError {
    #[error("invalid guide timezone")]
    InvalidZone,
    /// A local wall-clock value occurs twice in the zone.
    #[error("ambiguous local guide time")]
    AmbiguousLocalTime,
    /// A local wall-clock value does not occur in the zone.
    #[error("nonexistent local guide time")]
    NonexistentLocalTime,
    #[error("invalid guide time")]
    InvalidTime,
    #[error("invalid catch-up start time")]
    InvalidCatchup,
}

impl FromStr for GuideZone {
    type Err = GuideTimeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self)
            .map_err(|_| GuideTimeError::InvalidZone)
    }
}

impl GuideZone {
    /// Formats a Unix-millisecond timestamp as XMLTV's `YYYYmmddHHMMSS +zzzz`.
    pub fn format_timestamp_ms(self, timestamp_ms: i64) -> Result<String, GuideTimeError> {
        let timestamp =
            DateTime::from_timestamp_millis(timestamp_ms).ok_or(GuideTimeError::InvalidTime)?;
        Ok(timestamp
            .with_timezone(&self.0)
            .format("%Y%m%d%H%M%S %z")
            .to_string())
    }

    fn resolve(self, local: NaiveDateTime) -> Result<DateTime<Utc>, GuideTimeError> {
        match self.0.from_local_datetime(&local) {
            LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(_, _) => Err(GuideTimeError::AmbiguousLocalTime),
            LocalResult::None => Err(GuideTimeError::NonexistentLocalTime),
        }
    }

    /// A local midnight as an instant: the earlier one when it occurs twice,
    /// the first valid second after it when a DST gap skips it.
    fn resolve_midnight(self, mut local: NaiveDateTime) -> Result<DateTime<Utc>, GuideTimeError> {
        loop {
            match self.0.from_local_datetime(&local) {
                LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => {
                    return Ok(value.with_timezone(&Utc));
                }
                LocalResult::None => {
                    local = local
                        .checked_add_signed(Duration::seconds(1))
                        .ok_or(GuideTimeError::InvalidTime)?;
                }
            }
        }
    }
}

/// A half-open UTC guide interval represented in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuideWindow {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Builds one local-calendar window for every past, current, and future day.
///
/// # Errors
/// Returns [`GuideTimeError`] when the day limits or a local midnight are invalid.
pub fn build_windows(
    now: DateTime<Utc>,
    zone: GuideZone,
    past_days: u16,
    future_days: u16,
) -> Result<Vec<GuideWindow>, GuideTimeError> {
    if past_days > 31 || future_days > 31 {
        return Err(GuideTimeError::InvalidTime);
    }
    let today = now.with_timezone(&zone.0).date_naive();
    let midnight = |offset: i64| {
        let date = today
            .checked_add_signed(Duration::days(offset))
            .ok_or(GuideTimeError::InvalidTime)?;
        zone.resolve_midnight(date.and_time(chrono::NaiveTime::MIN))
    };
    (-i64::from(past_days)..=i64::from(future_days))
        .map(|day| {
            Ok(GuideWindow {
                start_ms: midnight(day)?.timestamp_millis(),
                end_ms: midnight(day + 1)?.timestamp_millis(),
            })
        })
        .collect()
}

/// Parses a catch-up value as seconds, milliseconds, or a configured-zone time.
///
/// # Errors
/// Returns [`GuideTimeError::InvalidCatchup`] for malformed values and the typed
/// local-time variants for ambiguous or nonexistent configured-zone values.
pub fn parse_catchup_start(value: &str, zone: GuideZone) -> Result<i64, GuideTimeError> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = value
            .parse::<i64>()
            .map_err(|_| GuideTimeError::InvalidCatchup)?;
        // Values up to 10^10 are seconds, larger ones milliseconds.
        return if number > 10_000_000_000 {
            Ok(number)
        } else {
            Ok(number * 1_000)
        };
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.timestamp_millis());
    }
    let local = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f"))
        .or_else(|_| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map(|date| date.and_time(chrono::NaiveTime::MIN))
        })
        .map_err(|_| GuideTimeError::InvalidCatchup)?;
    zone.resolve(local).map(|time| time.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_follow_dst_transition_without_24_hour_addition() {
        let zone = GuideZone::from_str("America/New_York").unwrap();
        let now = DateTime::parse_from_rfc3339("2024-03-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let windows = build_windows(now, zone, 1, 1).unwrap();
        assert_eq!(
            windows[1].end_ms - windows[1].start_ms,
            23 * 60 * 60 * 1_000
        );
        assert_eq!(windows[2].start_ms, windows[1].end_ms);
    }

    #[test]
    fn catchup_numeric_threshold_is_in_seconds() {
        let zone = GuideZone::from_str("Etc/GMT-8").unwrap();
        assert_eq!(parse_catchup_start("0", zone).unwrap(), 0);
        assert_eq!(
            parse_catchup_start("10000000000", zone).unwrap(),
            10_000_000_000_000
        );
        assert_eq!(
            parse_catchup_start("10000000001", zone).unwrap(),
            10_000_000_001
        );
        assert!(parse_catchup_start("-1", zone).is_err());
        assert!(parse_catchup_start(" 1", zone).is_err());
    }

    #[test]
    fn explicit_offsets_preserve_instants_and_naive_values_use_the_zone() {
        let zone = GuideZone::from_str("Asia/Shanghai").unwrap();
        assert_eq!(
            parse_catchup_start("2024-01-01T00:00:00+08:00", zone).unwrap(),
            parse_catchup_start("2023-12-31T16:00:00Z", zone).unwrap()
        );
        assert_eq!(
            parse_catchup_start("2024-01-01", zone).unwrap(),
            parse_catchup_start("2024-01-01T00:00:00+08:00", zone).unwrap()
        );
    }

    #[test]
    fn ambiguous_and_nonexistent_local_times_are_typed_errors() {
        let zone = GuideZone::from_str("America/New_York").unwrap();
        assert_eq!(
            parse_catchup_start("2024-11-03T01:30:00", zone),
            Err(GuideTimeError::AmbiguousLocalTime)
        );
        assert_eq!(
            parse_catchup_start("2024-03-10T02:30:00", zone),
            Err(GuideTimeError::NonexistentLocalTime)
        );
    }

    #[test]
    fn windows_resolve_midnight_dst_gap_to_first_valid_instant() {
        let zone = GuideZone::from_str("America/Santiago").unwrap();
        let now = DateTime::parse_from_rfc3339("2018-08-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let windows = build_windows(now, zone, 0, 0).unwrap();
        assert_eq!(windows[0].start_ms, 1_534_046_400_000);
    }
}
