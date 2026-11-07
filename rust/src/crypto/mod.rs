use des::TdesEde3;
use des::cipher::{Block, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};

pub mod ctc_option60;
pub mod epg_auth;

const BLOCK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoOperation {
    Dhcp,
    EpgAuth,
}

impl CryptoOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dhcp => "dhcp",
            Self::EpgAuth => "epg-auth",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("option 60 has a malformed prefix")]
    Option60MalformedPrefix,
    #[error("option 60 has a malformed length")]
    Option60MalformedLength,
    #[error("option 60 has a malformed method")]
    Option60MalformedMethod,
    #[error("CTC Option 60 exceeds the one-byte length field")]
    Option60OversizedPayload,
    #[error("CTC random value is outside the 31-bit range")]
    Option60RandomOutOfRange,
    #[error("EPG random value is outside the eight-digit range")]
    EpgRandomOutOfRange,
    #[error("authentication key has an invalid length")]
    InvalidKeyLength,
}

impl CryptoError {
    #[must_use]
    pub const fn operation(self) -> CryptoOperation {
        match self {
            Self::Option60MalformedPrefix
            | Self::Option60MalformedLength
            | Self::Option60MalformedMethod
            | Self::Option60OversizedPayload
            | Self::Option60RandomOutOfRange => CryptoOperation::Dhcp,
            Self::EpgRandomOutOfRange | Self::InvalidKeyLength => CryptoOperation::EpgAuth,
        }
    }
}

fn pad(input: &[u8], byte: u8) -> Vec<u8> {
    let padding = BLOCK - input.len() % BLOCK;
    let mut output = Vec::with_capacity(input.len() + padding);
    output.extend_from_slice(input);
    output.resize(input.len() + padding, byte);
    output
}

pub(super) fn zero_pad(input: &[u8]) -> Vec<u8> {
    pad(input, 0)
}
pub(super) fn pkcs7_pad(input: &[u8]) -> Vec<u8> {
    let padding = u8::try_from(BLOCK - input.len() % BLOCK).unwrap_or(0);
    pad(input, padding)
}

pub(super) fn ede3_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = TdesEde3::new_from_slice(key).map_err(|_| CryptoError::InvalidKeyLength)?;
    let mut output = Vec::with_capacity(plaintext.len());
    for chunk in plaintext.chunks_exact(BLOCK) {
        let mut block = Block::<TdesEde3>::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        output.extend_from_slice(&block);
    }
    Ok(output)
}

pub(super) fn md5_parts(parts: &[&[u8]]) -> [u8; 16] {
    let mut digest = Md5::new();
    for part in parts {
        digest.update(part);
    }
    digest.finalize().into()
}
