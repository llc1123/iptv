//! Rewrites upstream HLS playlists so every resource goes through `/_hls`.

use super::tokens::{CAPACITY, TokenStore, TokenStoreError};
use reqwest::Url;

/// Largest upstream playlist the proxy reads.
pub const PLAYLIST_MAX_BYTES: usize = 4 * 1024 * 1024;

enum Part<'a> {
    Text(&'a str),
    Resource(usize),
    EndOfLine,
}

/// Replaces each segment line and each `URI="..."` attribute with a token
/// route, resolving relative references against `base_url`. Resource URLs
/// lose their fragment.
pub fn rewrite_playlist(
    body: &[u8],
    base_url: &Url,
    tokens: &TokenStore,
) -> Result<Vec<u8>, TokenStoreError> {
    let text = String::from_utf8_lossy(body)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut parts = Vec::new();
    let mut resources = Vec::new();
    let mut add = |parts: &mut Vec<Part<'_>>, value: &str, default_suffix: &str| {
        parts.push(Part::Resource(resources.len()));
        resources.push(resource(base_url, value, default_suffix)?);
        if resources.len() > CAPACITY {
            return Err(TokenStoreError::Capacity);
        }
        Ok(())
    };
    for line in text.lines() {
        if line.starts_with('#') {
            let mut rest = line;
            while let Some(start) = rest.find("URI=\"") {
                let value_start = start + 5;
                let Some(length) = rest[value_start..].find('"') else {
                    break;
                };
                parts.push(Part::Text(&rest[..value_start]));
                add(&mut parts, &rest[value_start..value_start + length], "")?;
                rest = &rest[value_start + length..];
            }
            parts.push(Part::Text(rest));
        } else if !line.trim().is_empty() {
            add(&mut parts, line.trim(), ".ts")?;
        } else {
            parts.push(Part::Text(line));
        }
        parts.push(Part::EndOfLine);
    }
    let tokens = tokens.register_batch(&resources)?;
    let mut output = String::with_capacity(text.len() + tokens.len() * 32);
    for part in parts {
        match part {
            Part::Text(text) => output.push_str(text),
            Part::Resource(index) => {
                output.push_str("/_hls/");
                output.push_str(&tokens[index]);
            }
            Part::EndOfLine => output.push('\n'),
        }
    }
    Ok(output.into_bytes())
}

/// The absolute, fragment-free URL and the token suffix for a reference.
fn resource(
    base: &Url,
    value: &str,
    default_suffix: &str,
) -> Result<(String, String), TokenStoreError> {
    let mut url = base.join(value).map_err(|_| TokenStoreError::Invalid)?;
    url.set_fragment(None);
    let name = url.path().rsplit('/').next().unwrap_or_default();
    let suffix = match name.rfind('.') {
        Some(dot) if name.len() - dot <= 6 => name[dot..].to_ascii_lowercase(),
        _ => default_suffix.to_owned(),
    };
    Ok((url.into(), suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_registers_attribute_and_segment_urls_without_fragments() {
        let tokens = TokenStore::default();
        let base = Url::parse("http://upstream.test/live/index.m3u8").unwrap();
        let body = rewrite_playlist(
            b"#EXT-X-MAP:URI=\"init.mp4#part\"\r\nsegment\r\n",
            &base,
            &tokens,
        )
        .unwrap();
        let body = String::from_utf8(body).unwrap();
        let mut lines = body.lines();
        let map = lines.next().unwrap();
        let map_token = map
            .strip_prefix("#EXT-X-MAP:URI=\"/_hls/")
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap();
        assert_eq!(&map_token[24..], ".mp4");
        assert_eq!(
            tokens.resolve(map_token).unwrap(),
            "http://upstream.test/live/init.mp4"
        );
        let segment = lines.next().unwrap().strip_prefix("/_hls/").unwrap();
        assert_eq!(&segment[24..], ".ts");
        assert_eq!(
            tokens.resolve(segment).unwrap(),
            "http://upstream.test/live/segment"
        );
        assert!(body.ends_with('\n'));
    }
}
