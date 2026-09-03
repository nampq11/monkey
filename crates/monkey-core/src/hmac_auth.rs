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

pub fn hmac_sign(secret: &str, body: &[u8]) -> String {
    hmac_sign_payload(secret, body)
}

pub fn hmac_sign_with_timestamp(secret: &str, body: &[u8], timestamp: i64) -> String {
    let timestamp = timestamp.to_string();
    let mut payload = Vec::with_capacity(timestamp.len() + 1 + body.len());
    payload.extend_from_slice(timestamp.as_bytes());
    payload.push(b':');
    payload.extend_from_slice(body);
    hmac_sign_payload(secret, &payload)
}

fn hmac_sign_payload(secret: &str, payload: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload);
    let result = mac.finalize().into_bytes();
    format!("sha256={}", hex::encode(result))
}

pub fn verify_github_signature(
    secret: &str,
    body: &[u8],
    signature_header: Option<&str>,
) -> Result<(), HmacError> {
    let sig = signature_header.ok_or(HmacError::MissingSignature)?;
    if !sig.starts_with("sha256=") {
        return Err(HmacError::UnsupportedScheme);
    }
    let provided = &sig["sha256=".len()..];
    let expected = hmac_sign(secret, body);
    let expected_hex = &expected["sha256=".len()..];

    if provided.as_bytes().ct_eq(expected_hex.as_bytes()).into() {
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
    skew_seconds: i64,
) -> Result<(), HmacError> {
    let sig = signature_header.ok_or(HmacError::MissingSignature)?;
    let ts_str = timestamp_header.ok_or(HmacError::MissingTimestamp)?;

    let ts = ts_str.parse::<i64>().map_err(|_| HmacError::BadTimestamp)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if (now - ts).abs() > skew_seconds {
        return Err(HmacError::TimestampSkew);
    }

    if !sig.starts_with("sha256=") {
        return Err(HmacError::UnsupportedScheme);
    }
    let provided = &sig["sha256=".len()..];
    let expected = hmac_sign_with_timestamp(key, body, ts);
    let expected_hex = &expected["sha256=".len()..];

    if provided.as_bytes().ct_eq(expected_hex.as_bytes()).into() {
        Ok(())
    } else {
        Err(HmacError::SignatureMismatch)
    }
}
