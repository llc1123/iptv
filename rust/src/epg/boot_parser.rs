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
        if let Some((length, replacement)) = numeric_entity(&value[index + 1..]) {
            output.push(replacement);
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
        "nbsp" => Some("\u{a0}"),
        "apos" => Some("'"),
        "copy" => Some("©"),
        "reg" => Some("®"),
        "not" => Some("¬"),
        "alpha" => Some("α"),
        "beta" => Some("β"),
        "eacute" => Some("é"),
        "Aacute" => Some("Á"),
        _ => None,
    }
}

fn named_entity_without_semicolon(value: &str) -> Option<(usize, &'static str)> {
    [
        "eacute", "Aacute", "alpha", "beta", "nbsp", "copy", "quot", "amp", "lt", "gt", "reg",
        "not",
    ]
    .iter()
    .find_map(|name| {
        value
            .strip_prefix(name)
            .and_then(|rest| (rest.is_empty() || rest.as_bytes()[0] != b';').then_some(()))
            .and_then(|()| named_entity(name).map(|replacement| (name.len(), replacement)))
    })
}

fn numeric_entity(value: &str) -> Option<(usize, char)> {
    let (radix, digits) = if let Some(digits) = value.strip_prefix("#x") {
        (16, digits)
    } else if let Some(digits) = value.strip_prefix("#X") {
        (16, digits)
    } else {
        (10, value.strip_prefix('#')?)
    };
    let digit_count = digits
        .bytes()
        .take_while(|byte| match radix {
            10 => byte.is_ascii_digit(),
            16 => byte.is_ascii_hexdigit(),
            _ => false,
        })
        .count();
    if digit_count == 0 {
        return None;
    }
    let number = u32::from_str_radix(&digits[..digit_count], radix).ok();
    let replacement = match number {
        None | Some(0 | 0xD800..=0xDFFF | 0x0011_0000..=u32::MAX) => '\u{FFFD}',
        Some(number @ 0x80..=0x9F) => char::from_u32(
            [
                0x20AC, 0x81, 0x201A, 0x192, 0x201E, 0x2026, 0x2020, 0x2021, 0x2C6, 0x2030, 0x160,
                0x2039, 0x152, 0x8D, 0x17D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
                0x2013, 0x2014, 0x2DC, 0x2122, 0x161, 0x203A, 0x153, 0x9D, 0x17E, 0x178,
            ][usize::try_from(number - 0x80).ok()?],
        )
        .map_or('\u{FFFD}', |character| character),
        Some(number) => char::from_u32(number).map_or('\u{FFFD}', |character| character),
    };
    let has_semicolon = digits.as_bytes().get(digit_count) == Some(&b';');
    Some((
        if radix == 16 { 2 } else { 1 } + digit_count + usize::from(has_semicolon),
        replacement,
    ))
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

    #[test]
    fn parser_matches_python_html5_entity_boundaries_and_replacements() {
        let body = r#"x ChannelID="c1" ChannelName="&nbsp; &ampfoo &apos &#38; &#x1F642; &unknown; &#xD800;" UserChannelID="7" TimeShiftURL="t1""#;
        let channels = parse_boot_channels(body).expect("valid entity record");
        assert_eq!(channels[0].name, "\u{a0} &foo &apos & 🙂 &unknown; �");
    }
}
