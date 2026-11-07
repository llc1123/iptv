use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(300);
const CAPACITY: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub token: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreError {
    Invalid,
    Unavailable,
    Capacity,
}
impl std::fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HLS token unavailable")
    }
}
impl std::error::Error for TokenStoreError {}

struct Entry {
    url: String,
    expires_at: Instant,
}
pub struct TokenStore {
    entries: Mutex<HashMap<String, Entry>>,
}
impl Default for TokenStore {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl TokenStore {
    pub fn register(
        &self,
        url: impl Into<String>,
        suffix: &str,
    ) -> Result<Resource, TokenStoreError> {
        let url = url.into();
        if url.is_empty()
            || suffix.len() > 6
            || suffix
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(TokenStoreError::Invalid);
        }
        let token = format!("{}{}", random_token()?, suffix);
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        entries.retain(|_, entry| entry.expires_at > now);
        if entries.len() >= CAPACITY {
            return Err(TokenStoreError::Capacity);
        }
        entries.insert(
            token.clone(),
            Entry {
                url: url.clone(),
                expires_at: now + TTL,
            },
        );
        Ok(Resource { token, url })
    }
    pub fn resolve(&self, token: &str) -> Result<Resource, TokenStoreError> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        entries.retain(|_, entry| entry.expires_at > now);
        let entry = entries.get_mut(token).ok_or(TokenStoreError::Unavailable)?;
        entry.expires_at = now + TTL;
        Ok(Resource {
            token: token.to_owned(),
            url: entry.url.clone(),
        })
    }
}

fn random_token() -> Result<String, TokenStoreError> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0_u8; 18];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| TokenStoreError::Unavailable)?;
    let mut output = String::with_capacity(24);
    let mut value = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        value = (value << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let index =
                usize::try_from((value >> bits) & 63).map_err(|_| TokenStoreError::Unavailable)?;
            output.push(char::from(ALPHABET[index]));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_tokens_resolve_and_slide() {
        let store = TokenStore::default();
        let resource = store
            .register("http://upstream.test/segment.ts", ".ts")
            .unwrap();
        assert_eq!(store.resolve(&resource.token).unwrap().url, resource.url);
    }
}
