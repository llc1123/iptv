//! Tolerant parser for the provider's non-JSON boot-channel response.

use std::fmt;

use super::models::BootChannel;

/// Typed parser operation failure without retaining provider payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootParserError {
    /// Stable operation name.
    pub operation: &'static str,
    /// Stable failure class.
    pub kind: BootParserErrorKind,
}

/// Failure classes emitted by the boot parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootParserErrorKind {
    /// No complete record was found.
    NoRecords,
}

impl fmt::Display for BootParserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            BootParserErrorKind::NoRecords => {
                write!(
                    formatter,
                    "{}: no boot channel records were found",
                    self.operation
                )
            }
        }
    }
}

impl std::error::Error for BootParserError {}

fn html5_unescape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'&' {
            let character = value[index..]
                .chars()
                .next()
                .map_or('\u{FFFD}', |character| character);
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if let Some((length, replacement)) = named_entity_without_semicolon(&value[index + 1..]) {
            output.push_str(replacement);
            index += length + 1;
            continue;
        }
        let Some(relative_end) = value[index + 1..].find(';') else {
            output.push('&');
            index += 1;
            continue;
        };
        let end = index + 1 + relative_end;
        let entity = &value[index + 1..end];
        if let Some(replacement) = named_entity(entity) {
            output.push_str(replacement);
            index = end + 1;
        } else if let Some(replacement) = numeric_entity(entity) {
            output.push(replacement);
            index = end + 1;
        } else {
            output.push('&');
            index += 1;
        }
    }
    output
}

fn named_entity(entity: &str) -> Option<&'static str> {
    match entity {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        _ => None,
    }
}

fn named_entity_without_semicolon(value: &str) -> Option<(usize, &'static str)> {
    ["amp", "lt", "gt", "quot", "apos"].iter().find_map(|name| {
        value.strip_prefix(name).and_then(|rest| {
            if rest.is_empty()
                || (rest.as_bytes()[0] != b';' && !rest.as_bytes()[0].is_ascii_alphanumeric())
            {
                named_entity(name).map(|replacement| (name.len(), replacement))
            } else {
                None
            }
        })
    })
}

fn numeric_entity(entity: &str) -> Option<char> {
    let (radix, digits) = entity
        .strip_prefix("#x")
        .map_or((10, entity.strip_prefix('#')?), |digits| (16, digits));
    let number = u32::from_str_radix(digits, radix).ok()?;
    char::from_u32(number)
}

fn channel_start(body: &str, from: usize) -> Option<usize> {
    let pattern = b"ChannelID=\"";
    let bytes = body.as_bytes();
    (from..=bytes.len().saturating_sub(pattern.len())).find(|&index| {
        bytes[index..].starts_with(pattern)
            && (index == 0 || !bytes[index - 1].is_ascii_alphabetic())
    })
}

fn record_fields(body: &str) -> Vec<(String, String)> {
    let bytes = body.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphabetic() {
            index += 1;
            continue;
        }
        let name_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        if index + 1 >= bytes.len() || bytes[index] != b'=' || bytes[index + 1] != b'"' {
            continue;
        }
        let name = body[name_start..index].to_owned();
        index += 2;
        let value_start = index;
        while index < bytes.len() && bytes[index] != b'"' {
            index += 1;
        }
        let value = html5_unescape(&body[value_start..index]);
        result.push((name, value));
        if index < bytes.len() {
            index += 1;
        }
    }
    result
}

/// Parses boot records while preserving the Python negative-lookbehind boundary.
///
/// # Errors
/// Returns [`BootParserErrorKind::NoRecords`] when no complete record survives.
pub fn parse_boot_channels(body: &str) -> Result<Vec<BootChannel>, BootParserError> {
    let mut starts = Vec::new();
    let mut cursor = 0;
    while let Some(start) = channel_start(body, cursor) {
        starts.push(start);
        cursor = start + 1;
    }
    let mut channels = Vec::new();
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(body.len());
        let mut values = std::collections::BTreeMap::new();
        for (name, value) in record_fields(&body[*start..end]) {
            values.insert(name, value);
        }
        let required = ["ChannelID", "ChannelName", "UserChannelID", "TimeShiftURL"];
        if required.iter().all(|field| values.contains_key(*field)) {
            let value = serde_json::to_value(values).map_err(|_| BootParserError {
                operation: "epg-channels",
                kind: BootParserErrorKind::NoRecords,
            })?;
            if let Ok(channel) = serde_json::from_value::<BootChannel>(value) {
                channels.push(channel);
            }
        }
    }
    if channels.is_empty() {
        return Err(BootParserError {
            operation: "epg-channels",
            kind: BootParserErrorKind::NoRecords,
        });
    }
    Ok(channels)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "parser tests use fixed valid fixture inputs"
)]
mod tests {
    use super::{BootParserErrorKind, parse_boot_channels};

    #[test]
    fn parser_keeps_last_duplicate_and_unescapes_html_entities() {
        let channels = parse_boot_channels(
            r#"x ChannelID="c1" ChannelName="first" ChannelName="News &amp Sports &amp; Live" UserChannelID="7" TimeShiftURL="t1""#,
        )
        .expect("fixture boot record is valid");
        assert_eq!(channels[0].name, "News & Sports & Live");
    }

    #[test]
    fn parser_skips_incomplete_records_and_rejects_missing_records() {
        let body = r#"x ChannelID="bad" ChannelName="missing" UserChannelID="1"; ChannelID="good" ChannelName="ok" UserChannelID="2" TimeShiftURL="t2""#;
        assert_eq!(
            parse_boot_channels(body).expect("complete record remains")[0].channel_id,
            "good"
        );
        let error = parse_boot_channels("ChannelID=\"x\"").expect_err("no complete record");
        assert_eq!(error.kind, BootParserErrorKind::NoRecords);
    }

    #[test]
    fn parser_applies_manual_ascii_negative_lookbehind() {
        let body = r#"XChannelID="bad" ChannelID="good" ChannelName="ok" UserChannelID="2" TimeShiftURL="t2""#;
        assert_eq!(
            parse_boot_channels(body).expect("valid record")[0].channel_id,
            "good"
        );
    }
}
