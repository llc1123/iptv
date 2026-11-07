use std::{
    collections::HashMap,
    fmt,
    sync::Mutex,
    time::{Duration, Instant},
};

use ring::rand::{SecureRandom, SystemRandom};

const TTL: Duration = Duration::from_secs(300);
const CAPACITY: usize = 500_000;

#[derive(Clone, PartialEq, Eq)]
pub struct Resource {
    pub token: String,
    pub url: String,
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resource")
            .field("token", &"[redacted]")
            .field("url", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreError {
    Invalid,
    Unavailable,
    Capacity,
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("request unavailable")
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

impl fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenStore { entries: [redacted] }")
    }
}
impl Default for TokenStore {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

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
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(TokenStoreError::Invalid);
        }
        let token = random_token()?;
        let resource = Resource {
            token: format!("{token}{suffix}"),
            url,
        };
        let mut entries = self.entries.lock().map_err(|_| TokenStoreError::Capacity)?;
        entries.retain(|_, entry| entry.expires_at > Instant::now());
        if entries.len() >= CAPACITY {
            return Err(TokenStoreError::Capacity);
        }
        entries.insert(
            resource.token.clone(),
            Entry {
                url: resource.url.clone(),
                expires_at: Instant::now() + TTL,
            },
        );
        Ok(resource)
    }

    pub fn register_batch<I, U>(&self, values: I) -> Result<Vec<Resource>, TokenStoreError>
    where
        I: IntoIterator<Item = (U, String)>,
        U: Into<String>,
    {
        values
            .into_iter()
            .map(|(url, suffix)| self.register(url, &suffix))
            .collect()
    }

    pub fn resolve(&self, token: &str) -> Result<Resource, TokenStoreError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        let now = Instant::now();
        entries.retain(|_, entry| entry.expires_at > now);
        let entry = entries.get_mut(token).ok_or(TokenStoreError::Unavailable)?;
        entry.expires_at = now + TTL;
        Ok(Resource {
            token: token.to_owned(),
            url: entry.url.clone(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
            output.push(ALPHABET[((value >> bits) & 63) as usize] as char);
        }
    }
    Ok(output)
}
