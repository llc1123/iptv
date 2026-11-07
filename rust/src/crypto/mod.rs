use des::TdesEde3;
use des::cipher::{Block, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};

pub mod ctc_option60;
pub mod epg_auth;

const BLOCK: usize = 8;

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

fn pad(input: &[u8], byte: u8) -> Vec<u8> {
    let padding = BLOCK - input.len() % BLOCK;
    let mut output = Vec::with_capacity(input.len() + padding);
    output.extend_from_slice(input);
    output.resize(input.len() + padding, byte);
    output
}

pub(crate) fn zero_pad(input: &[u8]) -> Vec<u8> {
    pad(input, 0)
}
pub(crate) fn pkcs7_pad(input: &[u8]) -> Vec<u8> {
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

#[cfg(test)]
mod golden_padding_tests {
    use super::{pkcs7_pad, zero_pad};
    use serde_json::Value;

    #[test]
    fn padding_fixture_cases_match_python_bytes() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/padding.json"))
                .expect("padding fixture");
        let cases = fixture["cases"].as_array().expect("padding cases");
        assert_eq!(cases.len(), 6);
        let mut assertions = 0;
        for case in cases {
            let input = &case["input"];
            let plaintext = hex::decode(input["plaintext_hex"].as_str().expect("plaintext"))
                .expect("plaintext hex");
            let padded = match input["kind"].as_str().expect("padding kind") {
                "pkcs7" => pkcs7_pad(&plaintext),
                "option60-zero" => zero_pad(&plaintext),
                kind => panic!("unknown padding kind: {kind}"),
            };
            assert_eq!(
                hex::encode(padded),
                case["expected"]["padded_hex"].as_str().expect("padded hex"),
                "fixture {}",
                case["id"]
            );
            assertions += 1;
        }
        assert_eq!(assertions, cases.len());
    }
}
