//! CTC DHCP Option 60 encoding and validation.

use md5::{Digest, Md5};

use super::{CryptoError, ede3_encrypt, zero_pad};

const PREFIX: [u8; 3] = [0, 0, 0x1f];
const METHOD: u8 = 1;

/// Inputs for one deterministic Option 60 encoding.
#[derive(Debug, Clone, Copy)]
pub struct Option60Input<'a> {
    /// DHCP password used in the MD5 input.
    pub password: &'a str,
    /// Login encrypted as the payload.
    pub login: &'a str,
    /// Injected eight-byte little-endian random value.
    pub random: [u8; 8],
    /// Injected eight-byte little-endian monotonic timestamp.
    pub monotonic_ms: [u8; 8],
}

/// The validated fixed fields of an Option 60 value.
#[derive(Debug, PartialEq, Eq)]
pub struct Option60Fields<'a> {
    /// Eight-byte little-endian random field.
    pub random: &'a [u8],
    /// Eight-byte little-endian monotonic timestamp field.
    pub monotonic_ms: &'a [u8],
    /// Sixteen-byte MD5 hash key.
    pub hash_key: &'a [u8],
    /// Ciphertext following the fixed fields.
    pub ciphertext: &'a [u8],
}

/// Encode the exact CTC Option 60 wire value.
///
/// # Errors
/// Returns a typed error when the random value or one-byte wire length is invalid.
pub fn encode(input: &Option60Input<'_>) -> Result<Vec<u8>, CryptoError> {
    if u64::from_le_bytes(input.random) > 0x7fff_ffff {
        return Err(CryptoError::Option60RandomOutOfRange);
    }
    let random = input.random;
    let timestamp = input.monotonic_ms;
    let mut hash = Md5::new();
    hash.update(random);
    hash.update(input.password.as_bytes());
    hash.update(timestamp);
    let hash_key = hash.finalize();
    let mut session_key = [0_u8; 24];
    session_key[..8].copy_from_slice(&random);
    session_key[8..16].copy_from_slice(&timestamp);
    let ciphertext = ede3_encrypt(&session_key, &zero_pad(input.login.as_bytes()))?;
    let declared_length = 1_usize + 8 + 8 + 16 + ciphertext.len();
    let declared_length =
        u8::try_from(declared_length).map_err(|_| CryptoError::Option60OversizedPayload)?;
    let mut wire = Vec::with_capacity(4 + usize::from(declared_length));
    wire.extend_from_slice(&PREFIX);
    wire.push(declared_length);
    wire.push(METHOD);
    wire.extend_from_slice(&random);
    wire.extend_from_slice(&timestamp);
    wire.extend_from_slice(&hash_key);
    wire.extend_from_slice(&ciphertext);
    Ok(wire)
}

/// Validate and split an Option 60 wire value without decrypting its payload.
///
/// # Errors
/// Returns a typed error for a malformed prefix, length, or encryption method.
pub fn decode(wire: &[u8]) -> Result<Option60Fields<'_>, CryptoError> {
    if !wire.starts_with(&PREFIX) {
        return Err(CryptoError::Option60MalformedPrefix);
    }
    let declared = wire
        .get(3)
        .copied()
        .ok_or(CryptoError::Option60MalformedLength)?;
    let expected = 4_usize
        .checked_add(usize::from(declared))
        .ok_or(CryptoError::Option60MalformedLength)?;
    if wire.len() != expected {
        return Err(CryptoError::Option60MalformedLength);
    }
    if wire.get(4).copied() != Some(METHOD) {
        return Err(CryptoError::Option60MalformedMethod);
    }
    if wire.len() < 45 {
        return Err(CryptoError::Option60MalformedLength);
    }
    let fields = wire.get(5..).ok_or(CryptoError::Option60MalformedLength)?;
    let (random, fields) = fields.split_at(8);
    let (monotonic_ms, fields) = fields.split_at(8);
    let (hash_key, ciphertext) = fields.split_at(16);
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

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "fixture tests fail clearly when checked-in vectors are malformed"
)]
mod tests {
    use super::{Option60Input, decode, encode};
    use crate::crypto::CryptoError;

    fn input(login: &str) -> Option60Input<'_> {
        Option60Input {
            password: "fixture-dhcp-password",
            login,
            random: 0x1234_5678_u64.to_le_bytes(),
            monotonic_ms: hex::decode("cb047b4514000000")
                .expect("fixture timestamp is valid")
                .try_into()
                .expect("fixture timestamp has eight bytes"),
        }
    }

    #[test]
    fn fixture_encode_cases_are_byte_exact() {
        let family: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/option60.json"))
                .expect("option60 fixture is valid");
        for case in family["cases"]
            .as_array()
            .expect("fixture cases are an array")
        {
            let input_value = &case["input"];
            if input_value["direction"] != "encode" {
                continue;
            }
            let random = hex::decode(input_value["r_hex"].as_str().expect("random is text"))
                .expect("random is hex")
                .try_into()
                .expect("random has eight bytes");
            let monotonic_ms =
                hex::decode(input_value["ts_hex"].as_str().expect("timestamp is text"))
                    .expect("timestamp is hex")
                    .try_into()
                    .expect("timestamp has eight bytes");
            let request = Option60Input {
                password: input_value["password"].as_str().expect("password is text"),
                login: input_value["login"].as_str().expect("login is text"),
                random,
                monotonic_ms,
            };
            if case["expected"]["wire_hex"].is_null() {
                continue;
            }
            let expected = hex::decode(
                case["expected"]["wire_hex"]
                    .as_str()
                    .expect("expected wire is text"),
            )
            .expect("expected wire is hex");
            assert_eq!(encode(&request).expect("fixture input is valid"), expected);
        }
    }

    #[test]
    fn malformed_wire_fields_have_fixed_error_kinds() {
        let wire = encode(&input("fixture-user")).expect("fixture input is valid");
        let mut prefix = wire.clone();
        prefix[2] = 0x1e;
        assert_eq!(decode(&prefix), Err(CryptoError::Option60MalformedPrefix));
        assert_eq!(
            CryptoError::Option60MalformedPrefix.operation().as_str(),
            "dhcp"
        );
        let mut length = wire.clone();
        length[3] = length[3].saturating_add(1);
        assert_eq!(decode(&length), Err(CryptoError::Option60MalformedLength));
        let mut method = wire;
        method[4] = 2;
        assert_eq!(decode(&method), Err(CryptoError::Option60MalformedMethod));
    }

    #[test]
    fn oversized_login_is_rejected_without_truncation() {
        assert_eq!(
            encode(&input(&"x".repeat(220))),
            Err(CryptoError::Option60OversizedPayload)
        );
    }

    #[test]
    fn non_ascii_login_is_encoded_as_utf8() {
        let wire = encode(&input("用户")).expect("UTF-8 login is valid");
        assert_eq!(
            decode(&wire)
                .expect("encoded wire is valid")
                .ciphertext
                .len(),
            8
        );
    }
}
