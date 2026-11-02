//! EPG authenticator encoding.

use std::fmt;

use md5::{Digest, Md5};

use super::{CryptoError, ede3_encrypt, pkcs7_pad};

/// Inputs for one deterministic EPG authenticator.
#[derive(Clone, Copy)]
pub struct AuthenticatorInput<'a> {
    /// Provider encryption token.
    pub encrypt_token: &'a str,
    /// EPG user identifier.
    pub user_id: &'a str,
    /// Set-top-box identifier.
    pub stb_id: &'a str,
    /// Lease-bound IP address.
    pub ip_address: &'a str,
    /// Set-top-box MAC address.
    pub mac: &'a str,
    /// Provider password, padded or truncated to 24 bytes.
    pub password: &'a str,
    /// Provider custom suffix.
    pub custom_str: &'a str,
    /// Injected decimal nonce in the inclusive range 0..=99,999,999.
    pub random_value: u32,
}

impl fmt::Debug for AuthenticatorInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatorInput")
            .field("encrypt_token", &"[REDACTED]")
            .field("user_id", &"[REDACTED]")
            .field("stb_id", &"[REDACTED]")
            .field("ip_address", &self.ip_address)
            .field("mac", &self.mac)
            .field("password", &"[REDACTED]")
            .field("custom_str", &"[REDACTED]")
            .field("random_value", &self.random_value)
            .finish()
    }
}

/// Return the uppercase MD5 identity value used by the EPG login.
#[must_use]
pub fn identity_encode(session_id: &str, share_key: &str) -> String {
    let mut hash = Md5::new();
    hash.update(session_id.as_bytes());
    hash.update(share_key.as_bytes());
    hex::encode_upper(hash.finalize())
}

/// Build the uppercase hexadecimal EPG authenticator.
///
/// # Errors
/// Returns a typed error when the injected nonce cannot be represented by the
/// provider's eight-digit random range or a DES key cannot be constructed.
pub fn make_authenticator(input: &AuthenticatorInput<'_>) -> Result<String, CryptoError> {
    if input.random_value > 99_999_999 {
        return Err(CryptoError::EpgRandomOutOfRange);
    }
    let plaintext = format!(
        "{}${}${}${}${}${}${}",
        input.random_value,
        input.encrypt_token,
        input.user_id,
        input.stb_id,
        input.ip_address,
        input.mac,
        input.custom_str
    );
    let mut key = [b'0'; 24];
    let password = input.password.as_bytes();
    let key_len = password.len().min(key.len());
    key[..key_len].copy_from_slice(&password[..key_len]);
    let ciphertext = ede3_encrypt(&key, &pkcs7_pad(plaintext.as_bytes()))?;
    Ok(hex::encode_upper(ciphertext))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "fixture tests fail clearly when checked-in vectors are malformed"
)]
mod tests {
    use super::{AuthenticatorInput, identity_encode, make_authenticator};

    fn input(password: &str, random_value: u32) -> AuthenticatorInput<'_> {
        AuthenticatorInput {
            encrypt_token: "FIXTURE-ENCRYPT-TOKEN",
            user_id: "fixture-user",
            stb_id: "0102030405060708",
            ip_address: "192.0.2.10",
            mac: "02:00:00:00:00:01",
            password,
            custom_str: "$CTC",
            random_value,
        }
    }

    #[test]
    fn fixture_authenticator_vectors_are_byte_exact() {
        let family: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/epg_auth.json"))
                .expect("epg auth fixture is valid");
        for case in family["cases"]
            .as_array()
            .expect("fixture cases are an array")
        {
            let input_value = &case["input"];
            let request = AuthenticatorInput {
                encrypt_token: input_value["encrypt_token"]
                    .as_str()
                    .expect("token is text"),
                user_id: input_value["user_id"].as_str().expect("user id is text"),
                stb_id: input_value["stb_id"].as_str().expect("stb id is text"),
                ip_address: input_value["ip_address"].as_str().expect("IP is text"),
                mac: input_value["mac"].as_str().expect("MAC is text"),
                password: input_value["password"].as_str().expect("password is text"),
                custom_str: input_value["custom_str"]
                    .as_str()
                    .expect("custom string is text"),
                random_value: input_value["random_value"]
                    .as_u64()
                    .expect("random is an integer")
                    .try_into()
                    .expect("random fits u32"),
            };
            assert_eq!(
                make_authenticator(&request).expect("fixture input is valid"),
                case["expected"]["authenticator_hex"]
                    .as_str()
                    .expect("expected authenticator is text")
            );
        }
    }

    #[test]
    fn nonce_edges_and_key_truncation_match_fixture_contract() {
        let zero = make_authenticator(&input("fixture-epg-password", 0)).expect("fixture is valid");
        let boundary = make_authenticator(&input("fixture-epg-password", 99_999_999))
            .expect("fixture is valid");
        assert_eq!(&zero[..16], "B2E04076F0675566");
        assert_eq!(&boundary[..16], "04272F8C2920D367");
        assert_eq!(
            make_authenticator(&input("fixture-password-24chars-overlong", 12_345_678))
                .expect("fixture is valid"),
            make_authenticator(&input("fixture-password-24chars", 12_345_678))
                .expect("fixture is valid")
        );
    }

    #[test]
    fn identity_is_uppercase_md5_hex() {
        assert_eq!(
            identity_encode("session", "share"),
            "A870BEBF2BF97445CB2E758D2D39A6F3"
        );
    }

    #[test]
    fn debug_redacts_credential_values() {
        let rendered = format!("{:?}", input("fixture-epg-password-secret", 7));
        assert!(!rendered.contains("FIXTURE-ENCRYPT-TOKEN"));
        assert!(!rendered.contains("fixture-user"));
        assert!(!rendered.contains("fixture-epg-password-secret"));
        assert!(rendered.contains("random_value"));
        assert!(rendered.contains("REDACTED"));
    }
}
