//! Python-compatible M3U and XMLTV rendering.

use std::fmt::Write as _;
use std::path::Path;

use thiserror::Error;

use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideTimeError, GuideZone};
use crate::logos::logo_path;

/// The two independently cached feed formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderKind {
    M3u,
    Xmltv,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The request's Host header did not form a plain origin.
    #[error("invalid render base URL")]
    InvalidBaseUrl,
    /// A timestamp could not be represented in the configured zone.
    #[error("invalid render timestamp")]
    InvalidTimestamp,
}

impl From<GuideTimeError> for RenderError {
    fn from(_: GuideTimeError) -> Self {
        Self::InvalidTimestamp
    }
}

/// The client-facing origin the feeds link back to, from the request's
/// scheme and Host header.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BaseUrl(String);

impl BaseUrl {
    /// Accepts `scheme://host[:port][/]` with an IPv6 literal or a host of
    /// `[A-Za-z0-9.-]`, so the value can be embedded in feeds unescaped.
    pub fn parse(value: &str) -> Result<Self, RenderError> {
        let parsed = reqwest::Url::parse(value).map_err(|_| RenderError::InvalidBaseUrl)?;
        // IPv6 literals come back bracketed and already validated.
        let host_ok = parsed.host_str().is_some_and(|host| {
            host.starts_with('[')
                || host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        });
        let origin_only = matches!(parsed.scheme(), "http" | "https")
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if host_ok && origin_only && !value.chars().any(char::is_control) {
            Ok(Self(value.to_owned()))
        } else {
            Err(RenderError::InvalidBaseUrl)
        }
    }

    /// The base exactly as received.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The base without trailing slashes, for joining paths.
    fn joined(&self) -> &str {
        self.0.trim_end_matches('/')
    }
}

/// The proxied logo route for a channel.
pub fn logo_url(base: &BaseUrl, channel_id: &str) -> String {
    let mut url = format!("{}/logos/", base.joined());
    push_component(&mut url, channel_id);
    url
}

/// Renders the exact Python-compatible M3U bytes.
pub fn render_m3u(channels: &[BootChannel], base: &BaseUrl, catchup_days: u16) -> Vec<u8> {
    render_m3u_with_logos(channels, base, catchup_days, None)
}

/// Renders the exact Python-compatible XMLTV bytes.
///
/// # Errors
/// Returns [`RenderError::InvalidTimestamp`] for a programme time the zone cannot format.
pub fn render_xmltv(
    channels: &[BootChannel],
    programmes: &[Programme],
    zone: GuideZone,
    base: &BaseUrl,
) -> Result<Vec<u8>, RenderError> {
    render_xmltv_with_logos(channels, programmes, zone, base, None)
}

/// The logo URL a feed advertises: the `/logos` route when `logos` holds the
/// channel's file, or the channel's own URL when no directory is used.
fn channel_logo(channel: &BootChannel, base: &BaseUrl, logos: Option<&Path>) -> Option<String> {
    match logos {
        Some(dir) => {
            logo_path(dir, &channel.channel_id).map(|_| logo_url(base, &channel.channel_id))
        }
        None => (!channel.logo_url.is_empty()).then(|| channel.logo_url.clone()),
    }
}

pub(crate) fn render_m3u_with_logos(
    channels: &[BootChannel],
    base: &BaseUrl,
    catchup_days: u16,
    logos: Option<&Path>,
) -> Vec<u8> {
    let root = base.joined();
    let mut out = format!("#EXTM3U x-tvg-url=\"{root}/xmltv.xml\"\n");
    for channel in channels {
        let _ = write!(
            out,
            "#EXTINF:-1 tvg-id=\"{}\" tvg-name=\"{}\"",
            channel.channel_id, channel.name
        );
        if !channel.group_title.is_empty() {
            let _ = write!(out, " group-title=\"{}\"", channel.group_title);
        }
        if channel.time_shift == "1" && catchup_days > 0 {
            let _ = write!(
                out,
                " catchup=\"append\" catchup-days=\"{catchup_days}\" catchup-source=\"{root}/catchup/"
            );
            push_component(&mut out, &channel.channel_id);
            out.push_str(".m3u8?utc={utc}&lutc={lutc}\"");
        }
        if let Some(logo) = channel_logo(channel, base, logos) {
            let _ = write!(out, " tvg-logo=\"{logo}\"");
        }
        let _ = write!(out, ",{}\n{}/stream/", channel.name, base.as_str());
        push_component(&mut out, &channel.channel_id);
        out.push_str(".m3u8\n");
    }
    out.into_bytes()
}

pub(crate) fn render_xmltv_with_logos(
    channels: &[BootChannel],
    programmes: &[Programme],
    zone: GuideZone,
    base: &BaseUrl,
    logos: Option<&Path>,
) -> Result<Vec<u8>, RenderError> {
    let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
    if channels.is_empty() && programmes.is_empty() {
        out.push_str("<tv generator-info-name=\"iptv-media-server\" />");
        return Ok(out.into_bytes());
    }
    out.push_str("<tv generator-info-name=\"iptv-media-server\">\n");
    for channel in channels {
        out.push_str("  <channel id=\"");
        push_xml(&mut out, &channel.channel_id, true);
        out.push_str("\">\n");
        push_element(&mut out, "display-name", &channel.name);
        if let Some(logo) = channel_logo(channel, base, logos) {
            out.push_str("    <icon src=\"");
            push_xml(&mut out, &logo, true);
            out.push_str("\" />\n");
        }
        out.push_str("  </channel>\n");
    }
    // Like Python: programmes of listed channels only, first of each
    // (channel, start, stop, title) kept, ordered by channel then start.
    let ids: std::collections::HashSet<&str> = channels
        .iter()
        .map(|channel| channel.channel_id.as_str())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<&Programme> = programmes
        .iter()
        .filter(|programme| ids.contains(programme.channel_id.as_str()))
        .filter(|programme| {
            seen.insert((
                programme.channel_id.as_str(),
                programme.start_time_ms,
                programme.end_time_ms,
                programme.title.as_str(),
            ))
        })
        .collect();
    ordered.sort_by(|left, right| {
        left.channel_id
            .cmp(&right.channel_id)
            .then(left.start_time_ms.cmp(&right.start_time_ms))
    });
    let root = base.joined();
    for programme in ordered {
        let _ = write!(
            out,
            "  <programme start=\"{}\" stop=\"{}\" channel=\"",
            zone.format_timestamp_ms(programme.start_time_ms)?,
            zone.format_timestamp_ms(programme.end_time_ms)?
        );
        push_xml(&mut out, &programme.channel_id, true);
        out.push_str("\">\n");
        push_element(&mut out, "title", &programme.title);
        if !programme.programme_id.is_empty() {
            out.push_str("    <episode-num system=\"iptv-id\">");
            push_xml(&mut out, &programme.programme_id, false);
            out.push_str("</episode-num>\n    <url>");
            push_xml(&mut out, root, false);
            out.push_str("/replay/");
            push_component(&mut out, &programme.channel_id);
            out.push('/');
            push_component(&mut out, &programme.programme_id);
            out.push_str(".m3u8</url>\n");
        }
        out.push_str("  </programme>\n");
    }
    out.push_str("</tv>");
    Ok(out.into_bytes())
}

/// `<name>value</name>`, or `<name />` when empty, as `ElementTree` writes it.
fn push_element(out: &mut String, name: &str, value: &str) {
    if value.is_empty() {
        let _ = writeln!(out, "    <{name} />");
    } else {
        let _ = write!(out, "    <{name}>");
        push_xml(out, value, false);
        let _ = writeln!(out, "</{name}>");
    }
}

/// Escapes like `ElementTree` (attributes also escape quotes and
/// whitespace). Characters XML 1.0 forbids are dropped so one bad provider
/// title cannot invalidate the whole document.
fn push_xml(out: &mut String, value: &str, attribute: bool) {
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\r' if attribute => out.push_str("&#13;"),
            '\n' if attribute => out.push_str("&#10;"),
            '\t' if attribute => out.push_str("&#09;"),
            '\t'
            | '\n'
            | '\r'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'.. => {
                out.push(character);
            }
            _ => {}
        }
    }
}

/// Percent-encodes all but RFC 3986 unreserved bytes, like Python's
/// `quote(value, safe="")`.
fn push_component(out: &mut String, value: &str) {
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

#[cfg(test)]
#[path = "render_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
