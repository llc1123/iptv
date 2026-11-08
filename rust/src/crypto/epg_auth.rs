use super::{CryptoError, ede3_encrypt, md5_parts, pkcs7_pad};
use std::fmt;

#[derive(Clone, Copy)]
pub struct AuthenticatorInput<'a> {
    pub encrypt_token: &'a str,
    pub user_id: &'a str,
    pub stb_id: &'a str,
    pub ip_address: &'a str,
    pub mac: &'a str,
    pub password: &'a str,
    pub custom_str: &'a str,
    pub random_value: u32,
}

impl fmt::Debug for AuthenticatorInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticatorInput")
            .field("encrypt_token", &"[REDACTED]")
            .field("user_id", &"[REDACTED]")
            .field("stb_id", &"[REDACTED]")
            .field("ip_address", &"[REDACTED]")
            .field("mac", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("custom_str", &"[REDACTED]")
            .field("random_value", &self.random_value)
            .finish()
    }
}

pub fn identity_encode(session_id: &str, share_key: &str) -> String {
    hex::encode_upper(md5_parts(&[session_id.as_bytes(), share_key.as_bytes()]))
}

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
    key[..password.len().min(24)].copy_from_slice(&password[..password.len().min(24)]);
    Ok(hex::encode_upper(ede3_encrypt(
        &key,
        &pkcs7_pad(plaintext.as_bytes()),
    )?))
}
