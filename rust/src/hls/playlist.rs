//! Bounded, loss-minimizing HLS playlist URI rewriting.
//!
//! The scanner deliberately does not parse a playlist into a model. It copies
//! every byte which is not a URI span, while normalizing line endings to LF.
//! All resources are validated and staged before one atomic token-store batch.
//! The token store may prune pre-existing expired entries during that batch;
//! this module guarantees only that a failed batch adds no new entries.

use reqwest::Url;

use crate::error::HlsError;
use crate::lifecycle::CancellationToken;

use super::tokens::{Resource, TokenStore};

#[cfg(test)]
#[path = "playlist_tests.rs"]
mod playlist_tests;

/// Maximum accepted playlist body and produced body size.
pub const PLAYLIST_MAX_BYTES: usize = 16 * 1024 * 1024;
/// Maximum accepted logical line length, excluding its line ending.
pub const PLAYLIST_MAX_LINE_BYTES: usize = 1024 * 1024;
/// Maximum URI references in one playlist.
pub const PLAYLIST_MAX_URI_REFERENCES: usize = 100_000;
const PLAYLIST_MAX_RESOLVED_URL_BYTES: usize = 16 * 1024 * 1024;
const TOKEN_PREFIX: &[u8] = b"/_hls/";

/// Rewritten playlist bytes and the resources registered for its URI spans.
#[derive(Debug, PartialEq, Eq)]
pub struct PlaylistRewrite {
    /// LF-normalized response body.
    pub body: Vec<u8>,
    /// Resources in source order, including duplicate URI occurrences.
    pub resources: Vec<Resource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpanKind {
    Attribute,
    UriLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    kind: SpanKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedSpan {
    start: usize,
    end: usize,
    resource_index: usize,
}

/// Rewrites all supported HLS URI references without contacting an upstream.
///
/// # Errors
/// Returns a fixed HLS error when the body, line, URI, URL, cancellation, or
/// token-store bounds are invalid. No new token is registered before all
/// input validation and staging checks complete.
pub fn rewrite_playlist(
    body: &[u8],
    final_url: &Url,
    cancellation: &CancellationToken,
    token_store: &TokenStore,
) -> Result<PlaylistRewrite, HlsError> {
    if cancellation.is_cancelled() {
        return Err(HlsError::Cancelled);
    }
    if body.len() > PLAYLIST_MAX_BYTES {
        return Err(HlsError::ResourceUnavailable);
    }
    let text = std::str::from_utf8(body).map_err(|_| HlsError::ResourceUnavailable)?;
    let spans = scan_playlist(body, text)?;
    let (staged, resources) = stage_resources(text, final_url, cancellation, token_store, &spans)?;
    let output = render_playlist(body, staged, &resources)?;
    Ok(PlaylistRewrite {
        body: output,
        resources,
    })
}

fn scan_playlist(body: &[u8], text: &str) -> Result<Vec<Span>, HlsError> {
    let mut spans = Vec::new();
    let mut line_start = 0;
    while line_start < body.len() {
        let (line_end, next) = line_bounds(body, line_start)?;
        if line_end - line_start > PLAYLIST_MAX_LINE_BYTES {
            return Err(HlsError::ResourceUnavailable);
        }
        if body.get(line_start) == Some(&b'#') {
            scan_attributes(text, line_start, line_end, &mut spans)?;
        } else if !body[line_start..line_end]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t'))
        {
            let start = trim_ascii_space_tab(body, line_start, line_end).0;
            let end = trim_ascii_space_tab(body, line_start, line_end).1;
            if start < end {
                spans
                    .try_reserve(1)
                    .map_err(|_| HlsError::ResourceUnavailable)?;
                spans.push(Span {
                    start,
                    end,
                    kind: SpanKind::UriLine,
                });
            }
        }
        line_start = next;
    }
    if spans.len() > PLAYLIST_MAX_URI_REFERENCES {
        return Err(HlsError::ResourceUnavailable);
    }
    Ok(spans)
}

fn stage_resources(
    text: &str,
    final_url: &Url,
    cancellation: &CancellationToken,
    token_store: &TokenStore,
    spans: &[Span],
) -> Result<(Vec<StagedSpan>, Vec<Resource>), HlsError> {
    let mut staged = Vec::new();
    let mut urls = Vec::new();
    let mut resolved_bytes = 0_usize;
    let mut output_capacity = text
        .len()
        .checked_add(1)
        .ok_or(HlsError::ResourceUnavailable)?;
    for span in spans {
        if cancellation.is_cancelled() {
            return Err(HlsError::Cancelled);
        }
        let reference = text
            .get(span.start..span.end)
            .ok_or(HlsError::ResourceUnavailable)?;
        let resolved = final_url
            .join(reference)
            .map_err(|_| HlsError::ResourceUnavailable)?;
        let resolved_text = resolved.to_string().replace("%EF%BB%BF", "\u{feff}");
        resolved_bytes = resolved_bytes
            .checked_add(resolved_text.len())
            .ok_or(HlsError::ResourceUnavailable)?;
        if resolved_bytes > PLAYLIST_MAX_RESOLVED_URL_BYTES {
            return Err(HlsError::ResourceUnavailable);
        }
        let suffix = suffix_for(&resolved, span.kind);
        let replacement_len = TOKEN_PREFIX
            .len()
            .checked_add(24)
            .and_then(|length| length.checked_add(suffix.len()))
            .ok_or(HlsError::ResourceUnavailable)?;
        output_capacity = output_capacity
            .checked_sub(span.end - span.start)
            .and_then(|length| length.checked_add(replacement_len))
            .ok_or(HlsError::ResourceUnavailable)?;
        if output_capacity > PLAYLIST_MAX_BYTES {
            return Err(HlsError::ResourceUnavailable);
        }
        urls.try_reserve(1)
            .map_err(|_| HlsError::ResourceUnavailable)?;
        urls.push((resolved_text, suffix));
        staged
            .try_reserve(1)
            .map_err(|_| HlsError::ResourceUnavailable)?;
        staged.push(StagedSpan {
            start: span.start,
            end: span.end,
            resource_index: urls.len() - 1,
        });
    }

    let resources = token_store
        .register_batch(urls)
        .map_err(|_| HlsError::ResourceUnavailable)?;
    Ok((staged, resources))
}

fn render_playlist(
    body: &[u8],
    staged: Vec<StagedSpan>,
    resources: &[Resource],
) -> Result<Vec<u8>, HlsError> {
    let mut output_capacity = body
        .len()
        .checked_add(1)
        .ok_or(HlsError::ResourceUnavailable)?;
    for span in &staged {
        let resource = resources
            .get(span.resource_index)
            .ok_or(HlsError::ResourceUnavailable)?;
        output_capacity = output_capacity
            .checked_sub(span.end - span.start)
            .and_then(|length| {
                length
                    .checked_add(TOKEN_PREFIX.len())
                    .and_then(|length| length.checked_add(resource.token.len()))
            })
            .ok_or(HlsError::ResourceUnavailable)?;
    }
    let mut output = Vec::new();
    output
        .try_reserve(output_capacity.min(PLAYLIST_MAX_BYTES))
        .map_err(|_| HlsError::ResourceUnavailable)?;
    let mut source = 0_usize;
    for span in staged {
        copy_normalized_range(body, source, span.start, &mut output)?;
        output.extend_from_slice(TOKEN_PREFIX);
        let resource = resources
            .get(span.resource_index)
            .ok_or(HlsError::ResourceUnavailable)?;
        output.extend_from_slice(resource.token.as_bytes());
        source = span.end;
    }
    copy_normalized_range(body, source, body.len(), &mut output)?;
    if output.last() != Some(&b'\n') {
        output.push(b'\n');
    }
    if output.len() > PLAYLIST_MAX_BYTES {
        return Err(HlsError::ResourceUnavailable);
    }
    Ok(output)
}

fn line_bounds(body: &[u8], start: usize) -> Result<(usize, usize), HlsError> {
    let mut cursor = start;
    while let Some(byte) = body.get(cursor) {
        if *byte == b'\n' || *byte == b'\r' {
            let next = if *byte == b'\r' && body.get(cursor + 1) == Some(&b'\n') {
                cursor.checked_add(2)
            } else {
                cursor.checked_add(1)
            };
            return next
                .map(|next_index| (cursor, next_index))
                .ok_or(HlsError::ResourceUnavailable);
        }
        cursor = cursor.checked_add(1).ok_or(HlsError::ResourceUnavailable)?;
    }
    Ok((body.len(), body.len()))
}

fn trim_ascii_space_tab(body: &[u8], start: usize, end: usize) -> (usize, usize) {
    let mut left = start;
    let mut right = end;
    while body
        .get(left)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        left += 1;
    }
    while right > left
        && body
            .get(right - 1)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        right -= 1;
    }
    (left, right)
}

fn scan_attributes(
    text: &str,
    start: usize,
    end: usize,
    spans: &mut Vec<Span>,
) -> Result<(), HlsError> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < end {
        let key_start = cursor;
        while cursor < end && (bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'-') {
            cursor += 1;
        }
        let key = &bytes[key_start..cursor];
        while cursor < end && matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if cursor >= end || bytes[cursor] != b'=' {
            cursor = key_start.saturating_add(1);
            continue;
        }
        cursor += 1;
        while cursor < end && matches!(bytes[cursor], b' ' | b'\t') {
            cursor += 1;
        }
        if cursor >= end || bytes[cursor] != b'"' {
            cursor = key_start.saturating_add(1);
            continue;
        }
        let value_start = cursor + 1;
        cursor += 1;
        while cursor < end && bytes[cursor] != b'"' {
            cursor += 1;
        }
        if cursor >= end {
            if key == b"URI" || key == b"SERVER-URI" {
                return Err(HlsError::ResourceUnavailable);
            }
            return Ok(());
        }
        if (key == b"URI" || key == b"SERVER-URI") && value_start < cursor {
            spans
                .try_reserve(1)
                .map_err(|_| HlsError::ResourceUnavailable)?;
            spans.push(Span {
                start: value_start,
                end: cursor,
                kind: SpanKind::Attribute,
            });
        }
        cursor += 1;
    }
    Ok(())
}
fn suffix_for(url: &Url, kind: SpanKind) -> String {
    let path = url.path();
    let suffix = path.rsplit('/').next().unwrap_or_default();
    let extension = suffix.rsplit_once('.').map(|(_, extension)| extension);
    let allowed = extension.and_then(|extension| match extension.to_ascii_lowercase().as_str() {
        "ts" => Some(".ts"),
        "m3u8" => Some(".m3u8"),
        "bin" => Some(".bin"),
        "mp4" => Some(".mp4"),
        _ => None,
    });
    match (kind, allowed) {
        (SpanKind::UriLine | SpanKind::Attribute, Some(value)) => value.to_owned(),
        (SpanKind::UriLine, None) => ".ts".to_owned(),
        (SpanKind::Attribute, None) => String::new(),
    }
}

fn copy_normalized_range(
    body: &[u8],
    start: usize,
    end: usize,
    output: &mut Vec<u8>,
) -> Result<(), HlsError> {
    let mut cursor = start;
    while cursor < end {
        let byte = *body.get(cursor).ok_or(HlsError::ResourceUnavailable)?;
        if byte == b'\r' {
            output.push(b'\n');
            if body.get(cursor + 1) == Some(&b'\n') && cursor + 1 < end {
                cursor += 2;
            } else {
                cursor += 1;
            }
        } else if byte == b'\n' {
            output.push(b'\n');
            cursor += 1;
        } else {
            output.push(byte);
            cursor += 1;
        }
        if output.len() > PLAYLIST_MAX_BYTES {
            return Err(HlsError::ResourceUnavailable);
        }
    }
    Ok(())
}
