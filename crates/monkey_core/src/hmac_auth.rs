use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, PartialEq, Eq, Error)]
pub enum HmacError {
    #[error("missing signature header")]
    MissingSignature,
    #[error("missing timestamp header")]
    MissingTimestamp,
    #[error("bad timestamp")]
    BadTimestamp,
    #[error("timestamp outside skew window")]
    TimestampSkew,
    #[error("unsupported signature scheme")]
    UnsupportedScheme,
    #[error("signature mismatch")]
    SignatureMismatch,
}

fn hmac_digest_hex(secret: &str, payload: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

pub fn hmac_sign(secret: &str, body: &[u8]) -> String {
    format!("sha256={}", hmac_digest_hex(secret, body))
}

fn timestamped_payload(timestamp: i64, body: &[u8]) -> Vec<u8> {
    let timestamp = timestamp.to_string();
    let mut payload = Vec::with_capacity(timestamp.len() + 1 + body.len());
    payload.extend_from_slice(timestamp.as_bytes());
    payload.push(b':');
    payload.extend_from_slice(body);
    payload
}

pub fn hmac_sign_with_timestamp(secret: &str, body: &[u8], timestamp: i64) -> String {
    format!(
        "sha256={}",
        hmac_digest_hex(secret, &timestamped_payload(timestamp, body))
    )
}

fn signature_hex(signature_header: &str) -> Result<&str, HmacError> {
    signature_header
        .strip_prefix("sha256=")
        .ok_or(HmacError::UnsupportedScheme)
}

pub fn verify_github_signature(
    secret: &str,
    body: &[u8],
    signature_header: Option<&str>,
) -> Result<(), HmacError> {
    let signature = signature_header.ok_or(HmacError::MissingSignature)?;
    let provided = signature_hex(signature)?;
    if signatures_match(provided, &hmac_digest_hex(secret, body)) {
        Ok(())
    } else {
        Err(HmacError::SignatureMismatch)
    }
}

pub fn verify_internal_signature(
    key: &str,
    body: &[u8],
    signature_header: Option<&str>,
    timestamp_header: Option<&str>,
    skew_seconds: u64,
) -> Result<(), HmacError> {
    let signature = signature_header.ok_or(HmacError::MissingSignature)?;
    let timestamp_header = timestamp_header.ok_or(HmacError::MissingTimestamp)?;

    let timestamp = timestamp_header
        .parse::<i64>()
        .map_err(|_| HmacError::BadTimestamp)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // abs_diff survives attacker-controlled timestamps like i64::MIN that a
    // plain `now - timestamp` subtraction would overflow into a bogus skew.
    if now.abs_diff(timestamp) > skew_seconds {
        return Err(HmacError::TimestampSkew);
    }

    let provided = signature_hex(signature)?;
    if signatures_match(
        provided,
        &hmac_digest_hex(key, &timestamped_payload(timestamp, body)),
    ) {
        Ok(())
    } else {
        Err(HmacError::SignatureMismatch)
    }
}

fn signatures_match(provided_hex: &str, expected_hex: &str) -> bool {
    let (Some(provided), Some(expected)) =
        (decode_digest(provided_hex), decode_digest(expected_hex))
    else {
        return false;
    };
    provided.as_slice().ct_eq(expected.as_slice()).into()
}

// Hex decoding normalizes casing and rejects malformed input before the
// constant-time compare, so uppercase signatures still verify while the
// comparison itself stays over raw equal-length digests.
fn decode_digest(hex_text: &str) -> Option<[u8; 32]> {
    if hex_text.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    hex::decode_to_slice(hex_text, &mut digest).ok()?;
    Some(digest)
}
