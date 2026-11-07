use super::tokens::{Resource, TokenStore};
use reqwest::Url;

pub const PLAYLIST_MAX_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub struct PlaylistRewrite {
    pub body: Vec<u8>,
    pub resources: Vec<Resource>,
}

pub fn rewrite_playlist(
    body: &[u8],
    base_url: &Url,
    tokens: &TokenStore,
) -> Result<PlaylistRewrite, super::tokens::TokenStoreError> {
    let text = String::from_utf8_lossy(body);
    let mut lines = Vec::new();
    let mut pending = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            let mut rewritten = String::with_capacity(line.len());
            let mut cursor = 0;
            let mut copied = 0;
            let bytes = line.as_bytes();
            while cursor + 5 <= bytes.len() {
                if &bytes[cursor..cursor + 5] != b"URI=\"" {
                    cursor += 1;
                    continue;
                }
                let value_start = cursor + 5;
                let mut end = value_start;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += 1;
                }
                if end == bytes.len() {
                    break;
                }
                rewritten.push_str(&line[copied..value_start]);
                let index = pending.len();
                pending.push(resource_value(base_url, &line[value_start..end], "")?);
                rewritten.push('\0');
                rewritten.push_str(&index.to_string());
                rewritten.push('\0');
                cursor = end + 1;
                copied = cursor;
            }
            rewritten.push_str(&line[copied..]);
            lines.push(rewritten);
        } else if !line.trim().is_empty() {
            let index = pending.len();
            pending.push(resource_value(base_url, line.trim(), ".ts")?);
            lines.push(format!("\u{0}{index}\u{0}"));
        } else {
            lines.push(String::new());
        }
        if lines.iter().map(String::len).sum::<usize>() > PLAYLIST_MAX_BYTES {
            return Err(super::tokens::TokenStoreError::Capacity);
        }
    }
    let resources = tokens.register_batch(&pending)?;
    let mut output = String::new();
    for line in lines {
        let mut cursor = 0;
        while let Some(start) = line[cursor..].find('\0') {
            let start = cursor + start;
            let Some(end_offset) = line[start + 1..].find('\0') else {
                break;
            };
            let end = start + 1 + end_offset;
            output.push_str(&line[cursor..start]);
            let index = line[start + 1..end]
                .parse::<usize>()
                .map_err(|_| super::tokens::TokenStoreError::Invalid)?;
            let resource = resources
                .get(index)
                .ok_or(super::tokens::TokenStoreError::Invalid)?;
            output.push_str("/_hls/");
            output.push_str(&resource.token);
            cursor = end + 1;
        }
        output.push_str(&line[cursor..]);
        output.push('\n');
        if output.len() > PLAYLIST_MAX_BYTES {
            return Err(super::tokens::TokenStoreError::Capacity);
        }
    }
    Ok(PlaylistRewrite {
        body: output.into_bytes(),
        resources,
    })
}

fn resource_value(
    base: &Url,
    value: &str,
    default: &str,
) -> Result<(String, String), super::tokens::TokenStoreError> {
    let mut url = base
        .join(value)
        .map_err(|_| super::tokens::TokenStoreError::Invalid)?;
    url.set_fragment(None);
    let suffix = resource_suffix(&url, default);
    Ok((url.to_string(), suffix))
}

fn resource_suffix(url: &Url, default: &str) -> String {
    let name = url.path().rsplit('/').next().unwrap_or_default();
    let Some((_, extension)) = name.rsplit_once('.') else {
        return default.to_owned();
    };
    let extension = &name[name.len() - extension.len() - 1..];
    if extension.len() <= 6 {
        extension.to_ascii_lowercase()
    } else {
        default.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_registers_attribute_and_segment_urls_without_fragments() {
        let tokens = TokenStore::default();
        let base = Url::parse("http://upstream.test/live/index.m3u8").unwrap();
        let result = rewrite_playlist(
            b"#EXT-X-MAP:URI=\"init.mp4#part\"\r\nsegment\r\n",
            &base,
            &tokens,
        )
        .unwrap();
        assert_eq!(result.resources.len(), 2);
        assert_eq!(
            result.resources[0].url,
            "http://upstream.test/live/init.mp4"
        );
        assert_eq!(
            result.resources[0].token,
            format!("{}{}", &result.resources[0].token[..24], ".mp4")
        );
        assert!(result.body.ends_with(b"\n"));
        assert!(String::from_utf8(result.body).unwrap().contains("/_hls/"));
    }
}
