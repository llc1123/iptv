use std::{fmt, net::Ipv6Addr};

use super::RenderError;

const MAX_BASE_URL_BYTES: usize = 2048;

/// A validated direct request origin retained exactly for cache identity.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BaseUrl(String);

impl fmt::Debug for BaseUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BaseUrl")
            .field(&"<redacted>")
            .finish()
    }
}

impl BaseUrl {
    /// Validates a direct `http` or `https` scheme and authority without headers or paths.
    ///
    /// # Errors
    /// Returns [`RenderError::InvalidBaseUrl`] for malformed, oversized, or control-bearing input.
    pub fn parse(value: &str) -> Result<Self, RenderError> {
        if value.is_empty()
            || value.len() > MAX_BASE_URL_BYTES
            || value.chars().any(char::is_control)
            || value.to_ascii_lowercase().contains("forwarded")
        {
            return Err(RenderError::InvalidBaseUrl);
        }
        let Some((scheme, remainder)) = value.split_once("://") else {
            return Err(RenderError::InvalidBaseUrl);
        };
        if !matches!(scheme, "http" | "https") || remainder.is_empty() {
            return Err(RenderError::InvalidBaseUrl);
        }
        if remainder.bytes().any(|byte| matches!(byte, b'?' | b'#')) {
            return Err(RenderError::InvalidBaseUrl);
        }
        let authority_end = remainder.find('/').unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        let path = &remainder[authority_end..];
        if authority.is_empty() || path.chars().any(|character| character != '/') {
            return Err(RenderError::InvalidBaseUrl);
        }
        if authority.bytes().any(|byte| {
            matches!(
                byte,
                b'@' | b' ' | b'\\' | b'"' | b'\'' | b'<' | b'>' | b'&'
            )
        }) {
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
            let suffix = &authority[end + 1..];
            if !suffix.is_empty() {
                let Some(port) = suffix.strip_prefix(':') else {
                    return Err(RenderError::InvalidBaseUrl);
                };
                valid_port(port)?;
            }
        } else {
            if authority.contains(['[', ']']) {
                return Err(RenderError::InvalidBaseUrl);
            }
            let (host, port) = match authority.matches(':').count() {
                0 => (authority, None),
                1 => {
                    let (host, port) = authority
                        .rsplit_once(':')
                        .ok_or(RenderError::InvalidBaseUrl)?;
                    (host, Some(port))
                }
                _ => return Err(RenderError::InvalidBaseUrl),
            };
            validate_host(host)?;
            if let Some(port) = port {
                valid_port(port)?;
            }
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact input used as the cache key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(super) fn joined(&self) -> &str {
        self.0.trim_end_matches('/')
    }
}

fn validate_host(host: &str) -> Result<(), RenderError> {
    if host.is_empty() || host.starts_with('.') || host.ends_with('.') {
        return Err(RenderError::InvalidBaseUrl);
    }
    if host
        .split('.')
        .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
    {
        return Err(RenderError::InvalidBaseUrl);
    }
    if host
        .bytes()
        .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
    {
        return Err(RenderError::InvalidBaseUrl);
    }
    Ok(())
}

fn valid_port(port: &str) -> Result<(), RenderError> {
    if port.is_empty() || port.parse::<u16>().ok().is_none_or(|value| value == 0) {
        Err(RenderError::InvalidBaseUrl)
    } else {
        Ok(())
    }
}
