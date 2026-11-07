use super::RenderError;
use std::{fmt, net::Ipv6Addr};

const MAX: usize = 2048;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BaseUrl(String);
impl fmt::Debug for BaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BaseUrl").field(&"<redacted>").finish()
    }
}

impl BaseUrl {
    pub fn parse(value: &str) -> Result<Self, RenderError> {
        if value.is_empty()
            || value.len() > MAX
            || value.chars().any(char::is_control)
            || value.to_ascii_lowercase().contains("forwarded")
        {
            return Err(RenderError::InvalidBaseUrl);
        }
        let Some((scheme, rest)) = value.split_once("://") else {
            return Err(RenderError::InvalidBaseUrl);
        };
        if !matches!(scheme, "http" | "https")
            || rest.is_empty()
            || rest.bytes().any(|b| matches!(b, b'?' | b'#'))
        {
            return Err(RenderError::InvalidBaseUrl);
        }
        let end = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(end);
        if authority.is_empty()
            || path.chars().any(|c| c != '/')
            || authority
                .bytes()
                .any(|b| matches!(b, b'@' | b' ' | b'\\' | b'"' | b'\'' | b'<' | b'>' | b'&'))
        {
            return Err(RenderError::InvalidBaseUrl);
        }
        if authority.starts_with('[') {
            let Some(end) = authority.find(']') else {
                return Err(RenderError::InvalidBaseUrl);
            };
            if authority[..end].contains('[')
                || authority[end + 1..].contains(['[', ']'])
                || authority[1..end].parse::<Ipv6Addr>().is_err()
            {
                return Err(RenderError::InvalidBaseUrl);
            }
            if let Some(port) = authority[end + 1..].strip_prefix(':') {
                valid_port(port)?;
            } else if end + 1 != authority.len() {
                return Err(RenderError::InvalidBaseUrl);
            }
        } else {
            if authority.contains(['[', ']']) {
                return Err(RenderError::InvalidBaseUrl);
            }
            let (host, port) = authority
                .split_once(':')
                .map_or((authority, None), |(host, port)| (host, Some(port)));
            if authority.matches(':').count() > 1 {
                return Err(RenderError::InvalidBaseUrl);
            }
            if host.is_empty()
                || host.starts_with('.')
                || host.ends_with('.')
                || host
                    .split('.')
                    .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
                || host
                    .bytes()
                    .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')))
            {
                return Err(RenderError::InvalidBaseUrl);
            }
            if let Some(port) = port {
                valid_port(port)?;
            }
        }
        Ok(Self(value.to_owned()))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(super) fn joined(&self) -> &str {
        self.0.trim_end_matches('/')
    }
}

fn valid_port(port: &str) -> Result<(), RenderError> {
    if port.is_empty() || port.parse::<u16>().ok().is_none_or(|port| port == 0) {
        Err(RenderError::InvalidBaseUrl)
    } else {
        Ok(())
    }
}
