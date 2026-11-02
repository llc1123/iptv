//! Byte-compatible IPTV authentication primitives.

use des::TdesEde3;
use des::cipher::{Block, BlockCipherEncrypt, KeyInit};

pub mod ctc_option60;
pub mod epg_auth;

const BLOCK_SIZE: usize = 8;

/// The protocol operation associated with a crypto failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoOperation {
    /// DHCP CTC Option 60.
    Dhcp,
    /// EPG authenticator.
    EpgAuth,
}

impl CryptoOperation {
    /// Returns the stable operation code used by fixture errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dhcp => "dhcp",
            Self::EpgAuth => "epg-auth",
        }
    }
}

/// Typed failures from the authentication wire formats.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    /// Option 60 does not begin with its fixed prefix.
    #[error("option 60 has a malformed prefix")]
    Option60MalformedPrefix,
    /// Option 60's length byte does not describe the complete value.
    #[error("option 60 has a malformed length")]
    Option60MalformedLength,
    /// Option 60 declares an unsupported encryption method.
    #[error("option 60 has a malformed method")]
    Option60MalformedMethod,
    /// The encoded Option 60 value cannot fit in its one-byte length field.
    #[error("CTC Option 60 exceeds the one-byte length field")]
    Option60OversizedPayload,
    /// A caller supplied a random value outside the protocol's 31-bit range.
    #[error("CTC random value is outside the 31-bit range")]
    Option60RandomOutOfRange,
    /// A caller supplied an EPG nonce outside the protocol's decimal range.
    #[error("EPG random value is outside the eight-digit range")]
    EpgRandomOutOfRange,
    /// A cryptographic key could not be constructed.
    #[error("authentication key has an invalid length")]
    InvalidKeyLength,
}

impl CryptoError {
    /// Returns the stable operation code for this failure.
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

fn zero_pad(input: &[u8]) -> Vec<u8> {
    let padding = BLOCK_SIZE - input.len() % BLOCK_SIZE;
    let mut padded = Vec::with_capacity(input.len() + padding);
    padded.extend_from_slice(input);
    padded.resize(input.len() + padding, 0);
    padded
}

fn pkcs7_pad(input: &[u8]) -> Vec<u8> {
    let padding = BLOCK_SIZE - input.len() % BLOCK_SIZE;
    let mut padded = Vec::with_capacity(input.len() + padding);
    padded.extend_from_slice(input);
    padded.resize(input.len() + padding, u8::try_from(padding).unwrap_or(0));
    padded
}

fn ede3_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = TdesEde3::new_from_slice(key).map_err(|_| CryptoError::InvalidKeyLength)?;
    let mut ciphertext = Vec::with_capacity(plaintext.len());
    for chunk in plaintext.chunks_exact(BLOCK_SIZE) {
        let mut block = Block::<TdesEde3>::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        ciphertext.extend_from_slice(&block);
    }
    Ok(ciphertext)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "fixture tests fail clearly when checked-in vectors are malformed"
)]
mod tests {
    use super::{pkcs7_pad, zero_pad};

    #[test]
    fn padding_has_a_full_block_at_aligned_lengths() {
        assert_eq!(zero_pad(&[]), vec![0; 8]);
        assert_eq!(zero_pad(&[0, 1, 2, 3, 4, 5, 6, 7]).len(), 16);
        assert_eq!(pkcs7_pad(&[]), vec![8; 8]);
        assert_eq!(pkcs7_pad(&[0, 1, 2, 3, 4, 5, 6, 7]).len(), 16);
    }

    #[test]
    fn padding_fixture_cases_match_both_protocol_rules() {
        let family: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/padding.json"))
                .expect("padding fixture is valid");
        for case in family["cases"]
            .as_array()
            .expect("fixture cases are an array")
        {
            let input = hex::decode(
                case["input"]["plaintext_hex"]
                    .as_str()
                    .expect("plaintext is text"),
            )
            .expect("plaintext is hex");
            let expected = hex::decode(
                case["expected"]["padded_hex"]
                    .as_str()
                    .expect("padded output is text"),
            )
            .expect("padded output is hex");
            let kind = case["input"]["kind"]
                .as_str()
                .expect("padding kind is text");
            let actual = match kind {
                "pkcs7" => pkcs7_pad(&input),
                "option60-zero" => zero_pad(&input),
                other => {
                    assert_eq!(other, "option60-zero");
                    zero_pad(&input)
                }
            };
            assert_eq!(actual, expected);
        }
    }
}
