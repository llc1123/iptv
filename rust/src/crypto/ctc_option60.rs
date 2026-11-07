use super::{CryptoError, ede3_encrypt, md5_parts, zero_pad};
use std::fmt;

const PREFIX: [u8; 3] = [0, 0, 0x1f];

#[derive(Clone, Copy)]
pub struct Option60Input<'a> {
    pub password: &'a str,
    pub login: &'a str,
    pub random: [u8; 8],
    pub monotonic_ms: [u8; 8],
}

impl fmt::Debug for Option60Input<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Option60Input")
            .field("password", &"[REDACTED]")
            .field("login", &"[REDACTED]")
            .field("random", &self.random)
            .field("monotonic_ms", &self.monotonic_ms)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Option60Fields<'a> {
    pub random: &'a [u8],
    pub monotonic_ms: &'a [u8],
    pub hash_key: &'a [u8],
    pub ciphertext: &'a [u8],
}

pub fn encode(input: &Option60Input<'_>) -> Result<Vec<u8>, CryptoError> {
    if u64::from_le_bytes(input.random) > 0x7fff_ffff {
        return Err(CryptoError::Option60RandomOutOfRange);
    }
    let hash = md5_parts(&[
        &input.random,
        input.password.as_bytes(),
        &input.monotonic_ms,
    ]);
    let mut key = [0; 24];
    key[..8].copy_from_slice(&input.random);
    key[8..16].copy_from_slice(&input.monotonic_ms);
    let ciphertext = ede3_encrypt(&key, &zero_pad(input.login.as_bytes()))?;
    let length = u8::try_from(1 + 8 + 8 + 16 + ciphertext.len())
        .map_err(|_| CryptoError::Option60OversizedPayload)?;
    let mut wire = Vec::with_capacity(4 + usize::from(length));
    wire.extend_from_slice(&PREFIX);
    wire.push(length);
    wire.push(1);
    wire.extend_from_slice(&input.random);
    wire.extend_from_slice(&input.monotonic_ms);
    wire.extend_from_slice(&hash);
    wire.extend_from_slice(&ciphertext);
    Ok(wire)
}

pub fn decode(wire: &[u8]) -> Result<Option60Fields<'_>, CryptoError> {
    if !wire.starts_with(&PREFIX) {
        return Err(CryptoError::Option60MalformedPrefix);
    }
    let declared = *wire.get(3).ok_or(CryptoError::Option60MalformedLength)?;
    if wire.len() != 4 + usize::from(declared) {
        return Err(CryptoError::Option60MalformedLength);
    }
    if wire.get(4) != Some(&1) || wire.len() < 45 {
        return Err(if wire.get(4) == Some(&1) {
            CryptoError::Option60MalformedLength
        } else {
            CryptoError::Option60MalformedMethod
        });
    }
    let (random, rest) = wire[5..].split_at(8);
    let (monotonic_ms, rest) = rest.split_at(8);
    let (hash_key, ciphertext) = rest.split_at(16);
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(8) {
        return Err(CryptoError::Option60MalformedLength);
    }
    Ok(Option60Fields {
        random,
        monotonic_ms,
        hash_key,
        ciphertext,
    })
}
