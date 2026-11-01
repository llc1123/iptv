//! Wire-layout and padding invariants for Option 60, padding, and
//! authenticator fixture cases.

use super::{error_kind, field_hex, field_str, field_u64, invariant, is_error_case};
use crate::loader::{Case, GoldenError};

/// Check an Option 60 encode case against the documented wire layout.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when the case contradicts the layout.
pub(crate) fn option60_encode_layout(case: &Case) -> Result<(), GoldenError> {
    if is_error_case(case) || case.expected.get("wire_hex").is_none() {
        return Ok(());
    }
    let wire = field_hex(&case.expected, "wire_hex", &case.id)?;
    let login = field_str(&case.input, "login", &case.id)?;
    let reason = |text: &str| invariant(&case.id, text);
    if wire.len() < 37 {
        return Err(reason("wire image is shorter than the fixed fields"));
    }
    if wire[0..3] != [0x00, 0x00, 0x1f] {
        return Err(reason("wire prefix is not 00 00 1f"));
    }
    if wire[4] != 1 {
        return Err(reason("wire method byte is not 1"));
    }
    let declared_u64 = field_u64(&case.expected, "declared_length", &case.id)?;
    let Ok(declared) = usize::try_from(declared_u64) else {
        return Err(reason("declared length does not fit usize"));
    };
    if declared != wire.len() - 4 || usize::from(wire[3]) != declared {
        return Err(reason("declared length does not match the wire layout"));
    }
    if field_hex(&case.expected, "hash_key_hex", &case.id)? != wire[21..37] {
        return Err(reason("hash key field does not match the wire image"));
    }
    let ciphertext = field_hex(&case.expected, "ciphertext_hex", &case.id)?;
    if ciphertext != wire[37..] {
        return Err(reason("ciphertext field does not match the wire image"));
    }
    if ciphertext.len() % 8 != 0 {
        return Err(reason("ciphertext is not a whole number of DES blocks"));
    }
    let padded_len = login.len() + (8 - login.len() % 8);
    if ciphertext.len() != padded_len {
        return Err(reason("ciphertext length does not match zero padding rule"));
    }
    Ok(())
}

/// Check that an Option 60 decode case really violates its claimed rule.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when the vector does not violate the
/// rule it documents.
pub(crate) fn option60_reject_violation(case: &Case) -> Result<(), GoldenError> {
    if case
        .input
        .get("direction")
        .and_then(serde_json::Value::as_str)
        != Some("decode")
    {
        return Ok(());
    }
    let wire = field_hex(&case.input, "wire_hex", &case.id)?;
    let violates = field_str(&case.input, "violates", &case.id)?;
    let kind = error_kind(&case.expected).unwrap_or("");
    let reason = |text: &str| invariant(&case.id, text);
    match violates.as_str() {
        "prefix" => {
            if wire.len() < 3 || wire[0..3] == [0x00, 0x00, 0x1f] {
                return Err(reason("prefix vector does not violate the prefix rule"));
            }
            if kind != "option60-malformed-prefix" {
                return Err(reason("prefix vector claims the wrong error kind"));
            }
        }
        "method" => {
            if wire.len() < 5 || wire[4] == 1 {
                return Err(reason("method vector does not violate the method rule"));
            }
            if kind != "option60-malformed-method" {
                return Err(reason("method vector claims the wrong error kind"));
            }
        }
        "length" => {
            if wire.len() < 4 || usize::from(wire[3]) == wire.len() - 4 {
                return Err(reason("length vector does not violate the length rule"));
            }
            if kind != "option60-malformed-length" {
                return Err(reason("length vector claims the wrong error kind"));
            }
        }
        other => return Err(reason(&format!("unknown violation claim {other}"))),
    }
    Ok(())
}

/// Check a padding vector against its declared padding rule.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when the padded bytes break the rule.
pub(crate) fn padding_rule(case: &Case) -> Result<(), GoldenError> {
    let kind = field_str(&case.input, "kind", &case.id)?;
    let plaintext = field_hex(&case.input, "plaintext_hex", &case.id)?;
    let padded = field_hex(&case.expected, "padded_hex", &case.id)?;
    let reason = |text: &str| invariant(&case.id, text);
    let block_pad = 8 - plaintext.len() % 8;
    if padded.len() != plaintext.len() + block_pad || padded[..plaintext.len()] != plaintext[..] {
        return Err(reason("padded value does not extend the plaintext"));
    }
    let tail = &padded[plaintext.len()..];
    match kind.as_str() {
        "pkcs7" => {
            if !tail.iter().all(|byte| usize::from(*byte) == block_pad) {
                return Err(reason("PKCS#7 tail bytes do not carry the pad length"));
            }
        }
        "option60-zero" => {
            if !tail.iter().all(|byte| *byte == 0) {
                return Err(reason("zero-padding tail bytes are not zero"));
            }
        }
        other => return Err(reason(&format!("unknown padding kind {other}"))),
    }
    Ok(())
}

/// Check an authenticator case pins uppercase hex of the PKCS#7 length.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when the authenticator shape breaks.
pub(crate) fn epg_auth_shape(case: &Case) -> Result<(), GoldenError> {
    let authenticator = field_str(&case.expected, "authenticator_hex", &case.id)?;
    let reason = |text: &str| invariant(&case.id, text);
    if authenticator.is_empty()
        || !authenticator
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return Err(reason("authenticator is not uppercase hex"));
    }
    let random_value = field_u64(&case.input, "random_value", &case.id)?;
    let plaintext = format!(
        "{}${}${}${}${}${}${}",
        random_value,
        field_str(&case.input, "encrypt_token", &case.id)?,
        field_str(&case.input, "user_id", &case.id)?,
        field_str(&case.input, "stb_id", &case.id)?,
        field_str(&case.input, "ip_address", &case.id)?,
        field_str(&case.input, "mac", &case.id)?,
        field_str(&case.input, "custom_str", &case.id)?,
    );
    let padded_len = plaintext.len() + (8 - plaintext.len() % 8);
    if authenticator.len() != padded_len * 2 {
        return Err(reason("authenticator length does not match PKCS#7 padding"));
    }
    Ok(())
}
