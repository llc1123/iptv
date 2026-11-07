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
    let mut output = String::new();
    let mut resources = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            let mut rewritten = String::with_capacity(line.len());
            let mut rest = line;
            while let Some(start) = rest.find("URI=\"") {
                let value_start = start + 5;
                let Some(end_offset) = rest[value_start..].find('"') else {
                    rewritten.push_str(rest);
                    break;
                };
                let end = value_start + end_offset;
                rewritten.push_str(&rest[..value_start]);
                let resource = register_uri(tokens, base_url, &rest[value_start..end], "")?;
                rewritten.push_str("/_hls/");
                rewritten.push_str(&resource.token);
                resources.push(resource);
                rest = &rest[end..];
            }
            if !rest.is_empty() && !rewritten.ends_with(rest) {
                rewritten.push_str(rest);
            }
            output.push_str(&rewritten);
        } else if !line.trim().is_empty() {
            let resource = register_uri(tokens, base_url, line.trim(), ".ts")?;
            output.push_str("/_hls/");
            output.push_str(&resource.token);
            resources.push(resource);
        }
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

fn register_uri(
    tokens: &TokenStore,
    base: &Url,
    value: &str,
    default: &str,
) -> Result<Resource, super::tokens::TokenStoreError> {
    let mut url = base
        .join(value)
        .map_err(|_| super::tokens::TokenStoreError::Invalid)?;
    url.set_fragment(None);
    let suffix = resource_suffix(&url, default);
    tokens.register(url.to_string(), &suffix)
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
