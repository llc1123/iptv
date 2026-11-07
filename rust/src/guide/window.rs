//! Configured-zone guide windows and catch-up timestamp parsing.

use std::{fmt, str::FromStr};

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use thiserror::Error;

/// The configured timezone used for calendar calculations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuideZone {
    /// A fixed offset from UTC in minutes.
    Fixed {
        /// Offset east of UTC in minutes.
        offset_minutes: i32,
    },
    /// An embedded IANA timezone name.
    Iana(Tz),
}

/// Errors raised while resolving civil guide times.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GuideTimeError {
    /// The timezone name or fixed offset is invalid.
    #[error("invalid guide timezone")]
    InvalidZone,
    /// A local wall-clock value occurs twice in the selected timezone.
    #[error("ambiguous local guide time")]
    AmbiguousLocalTime,
    /// A local wall-clock value does not occur in the selected timezone.
    #[error("nonexistent local guide time")]
    NonexistentLocalTime,
    /// A timestamp or date could not be represented.
    #[error("invalid guide time")]
    InvalidTime,
    /// A catch-up value was empty or malformed.
    #[error("invalid catch-up start time")]
    InvalidCatchup,
}

impl FromStr for GuideZone {
    type Err = GuideTimeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "Z" || value == "UTC" {
            return Ok(Self::Fixed { offset_minutes: 0 });
        }
        if let Some(offset) = parse_fixed_offset(value) {
            return Ok(Self::Fixed {
                offset_minutes: offset,
            });
        }
        value
            .parse::<Tz>()
            .map(Self::Iana)
            .map_err(|_| GuideTimeError::InvalidZone)
    }
}

impl GuideZone {
    /// Formats a Unix-millisecond timestamp using this configured guide zone.
    ///
    /// # Errors
    /// Returns [`GuideTimeError::InvalidTime`] when the timestamp cannot be represented.
    pub fn format_timestamp_ms(&self, timestamp_ms: i64) -> Result<String, GuideTimeError> {
        let timestamp = chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .ok_or(GuideTimeError::InvalidTime)?;
        let formatted = match self {
            Self::Fixed { offset_minutes } => {
                let offset = chrono::FixedOffset::east_opt(offset_minutes.saturating_mul(60))
                    .ok_or(GuideTimeError::InvalidZone)?;
                timestamp.with_timezone(&offset).format("%Y%m%d%H%M%S %z")
            }
            Self::Iana(zone) => timestamp.with_timezone(zone).format("%Y%m%d%H%M%S %z"),
        };
        Ok(formatted.to_string())
    }

    fn resolve(&self, local: NaiveDateTime) -> Result<DateTime<Utc>, GuideTimeError> {
        match self {
            Self::Fixed { offset_minutes } => {
                let offset = chrono::FixedOffset::east_opt(offset_minutes.saturating_mul(60))
                    .ok_or(GuideTimeError::InvalidZone)?;
                match offset.from_local_datetime(&local) {
                    LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
                    LocalResult::Ambiguous(_, _) => Err(GuideTimeError::AmbiguousLocalTime),
                    LocalResult::None => Err(GuideTimeError::NonexistentLocalTime),
                }
            }
            Self::Iana(zone) => match zone.from_local_datetime(&local) {
                LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
                LocalResult::Ambiguous(_, _) => Err(GuideTimeError::AmbiguousLocalTime),
                LocalResult::None => Err(GuideTimeError::NonexistentLocalTime),
            },
        }
    }

    fn resolve_midnight(&self, mut local: NaiveDateTime) -> Result<DateTime<Utc>, GuideTimeError> {
        loop {
            match self {
                Self::Fixed { .. } => return self.resolve(local),
                Self::Iana(zone) => match zone.from_local_datetime(&local) {
                    LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => {
                        return Ok(value.with_timezone(&Utc));
                    }
                    LocalResult::None => {
                        local = local
                            .checked_add_signed(Duration::seconds(1))
                            .ok_or(GuideTimeError::InvalidTime)?;
                    }
                },
            }
        }
    }

    fn local_date(&self, now: DateTime<Utc>) -> Result<NaiveDate, GuideTimeError> {
        match self {
            Self::Fixed { offset_minutes } => {
                let seconds = offset_minutes.saturating_mul(60);
                let offset =
                    chrono::FixedOffset::east_opt(seconds).ok_or(GuideTimeError::InvalidZone)?;
                Ok(now.with_timezone(&offset).date_naive())
            }
            Self::Iana(zone) => Ok(now.with_timezone(zone).date_naive()),
        }
    }
}

fn parse_fixed_offset(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    if bytes.len() != 6 || !matches!(bytes.first(), Some(b'+' | b'-')) || bytes[3] != b':' {
        return None;
    }
    if !bytes[1..3].iter().all(u8::is_ascii_digit) || !bytes[4..6].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let hours = i32::from(bytes[1] - b'0') * 10 + i32::from(bytes[2] - b'0');
    let minutes = i32::from(bytes[4] - b'0') * 10 + i32::from(bytes[5] - b'0');
    if hours > 23 || minutes > 59 {
        return None;
    }
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    Some(sign * (hours * 60 + minutes))
}

/// A half-open UTC guide interval represented in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuideWindow {
    /// Inclusive UTC start in Unix milliseconds.
    pub start_ms: i64,
    /// Exclusive UTC end in Unix milliseconds.
    pub end_ms: i64,
}

/// A wall-clock source returning the current UTC instant.
pub trait GuideClock: Send + Sync {
    /// Returns the current wall-clock UTC instant.
    fn now(&self) -> DateTime<Utc>;
}

/// The production wall-clock implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGuideClock;

impl GuideClock for SystemGuideClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A deterministic wall-clock source for callers and tests.
#[derive(Clone, Copy, Debug)]
pub struct FixedGuideClock(pub DateTime<Utc>);

impl GuideClock for FixedGuideClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Builds guide windows using an injected wall-clock source.
///
/// # Errors
/// Returns [`GuideTimeError`] when a configured local midnight cannot be resolved.
pub fn build_windows_from_clock<C: GuideClock>(
    clock: &C,
    zone: &GuideZone,
    past_days: u16,
    future_days: u16,
) -> Result<Vec<GuideWindow>, GuideTimeError> {
    build_windows(clock.now(), zone, past_days, future_days)
}

/// Builds one local-calendar window for every past, current, and future day.
///
/// # Errors
/// Returns [`GuideTimeError`] when the day limits or a local midnight are invalid.
pub fn build_windows(
    now: DateTime<Utc>,
    zone: &GuideZone,
    past_days: u16,
    future_days: u16,
) -> Result<Vec<GuideWindow>, GuideTimeError> {
    let count = usize::from(past_days)
        .checked_add(usize::from(future_days))
        .and_then(|value| value.checked_add(1))
        .ok_or(GuideTimeError::InvalidTime)?;
    if past_days > 31 || future_days > 31 || count > 63 {
        return Err(GuideTimeError::InvalidTime);
    }
    let first_date = zone
        .local_date(now)?
        .checked_sub_signed(Duration::days(i64::from(past_days)))
        .ok_or(GuideTimeError::InvalidTime)?;
    (0..count)
        .map(|index| {
            let days = i64::try_from(index).map_err(|_| GuideTimeError::InvalidTime)?;
            let date = first_date
                .checked_add_signed(Duration::days(days))
                .ok_or(GuideTimeError::InvalidTime)?;
            let next = date.succ_opt().ok_or(GuideTimeError::InvalidTime)?;
            let start = zone.resolve_midnight(
                date.and_hms_opt(0, 0, 0)
                    .ok_or(GuideTimeError::InvalidTime)?,
            )?;
            let end = zone.resolve_midnight(
                next.and_hms_opt(0, 0, 0)
                    .ok_or(GuideTimeError::InvalidTime)?,
            )?;
            Ok(GuideWindow {
                start_ms: start.timestamp_millis(),
                end_ms: end.timestamp_millis(),
            })
        })
        .collect()
}

/// Parses a catch-up value as seconds, milliseconds, or a configured-zone time.
///
/// # Errors
/// Returns [`GuideTimeError::InvalidCatchup`] for malformed values and the typed
/// local-time variants for ambiguous or nonexistent configured-zone values.
pub fn parse_catchup_start(value: &str, zone: &GuideZone) -> Result<i64, GuideTimeError> {
    if value.is_empty() {
        return Err(GuideTimeError::InvalidCatchup);
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = value
            .parse::<u64>()
            .map_err(|_| GuideTimeError::InvalidCatchup)?;
        if number > 10_000_000_000 {
            return i64::try_from(number).map_err(|_| GuideTimeError::InvalidCatchup);
        }
        let millis = number
            .checked_mul(1_000)
            .ok_or(GuideTimeError::InvalidCatchup)?;
        return i64::try_from(millis).map_err(|_| GuideTimeError::InvalidCatchup);
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.timestamp_millis());
    }
    let local = match NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") {
        Ok(local) => local,
        Err(_) => match NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f") {
            Ok(local) => local,
            Err(_) => NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| GuideTimeError::InvalidCatchup)?
                .and_hms_opt(0, 0, 0)
                .ok_or(GuideTimeError::InvalidCatchup)?,
        },
    };
    zone.resolve(local)
        .map(|time| time.timestamp_millis())
        .map_err(|error| match error {
            GuideTimeError::AmbiguousLocalTime | GuideTimeError::NonexistentLocalTime => error,
            GuideTimeError::InvalidZone
            | GuideTimeError::InvalidTime
            | GuideTimeError::InvalidCatchup => GuideTimeError::InvalidCatchup,
        })
}

impl fmt::Display for GuideZone {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed { offset_minutes } => write!(formatter, "{offset_minutes:+}m"),
            Self::Iana(zone) => zone.fmt(formatter),
        }
    }
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
        let windows = build_windows(now, &zone, 1, 1).unwrap();
        assert_eq!(
            windows[1].end_ms - windows[1].start_ms,
            23 * 60 * 60 * 1_000
        );
        assert_eq!(windows[2].start_ms, windows[1].end_ms);
    }

    #[test]
    fn catchup_numeric_threshold_is_in_seconds() {
        let zone = GuideZone::from_str("+08:00").unwrap();
        assert_eq!(parse_catchup_start("0", &zone).unwrap(), 0);
        assert_eq!(
            parse_catchup_start("10000000000", &zone).unwrap(),
            10_000_000_000_000
        );
        assert_eq!(
            parse_catchup_start("10000000001", &zone).unwrap(),
            10_000_000_001
        );
        assert!(parse_catchup_start("-1", &zone).is_err());
        assert!(parse_catchup_start(" 1", &zone).is_err());
    }

    #[test]
    fn explicit_offsets_preserve_instants_and_naive_values_use_the_zone() {
        let zone = GuideZone::from_str("Asia/Shanghai").unwrap();
        assert_eq!(
            parse_catchup_start("2024-01-01T00:00:00+08:00", &zone).unwrap(),
            parse_catchup_start("2023-12-31T16:00:00Z", &zone).unwrap()
        );
        assert_eq!(
            parse_catchup_start("2024-01-01", &zone).unwrap(),
            parse_catchup_start("2024-01-01T00:00:00+08:00", &zone).unwrap()
        );
    }

    #[test]
    fn ambiguous_and_nonexistent_local_times_are_typed_errors() {
        let zone = GuideZone::from_str("America/New_York").unwrap();
        assert_eq!(
            parse_catchup_start("2024-11-03T01:30:00", &zone),
            Err(GuideTimeError::AmbiguousLocalTime)
        );
        assert_eq!(
            parse_catchup_start("2024-03-10T02:30:00", &zone),
            Err(GuideTimeError::NonexistentLocalTime)
        );
    }

    #[test]
    fn windows_resolve_midnight_dst_gap_to_first_valid_instant() {
        let zone = GuideZone::from_str("America/Santiago").unwrap();
        let now = DateTime::parse_from_rfc3339("2018-08-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let windows = build_windows(now, &zone, 0, 0).unwrap();
        assert_eq!(windows[0].start_ms, 1_534_046_400_000);
    }
}
