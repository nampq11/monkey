use monkey::hmac_auth::{HmacError, hmac_sign, verify_github_signature, verify_internal_signature};
use std::time::{SystemTime, UNIX_EPOCH};

const SECRET: &str = "test-secret";
const BODY: &[u8] = b"{\"action\":\"opened\",\"issue\":{\"number\":123}}";

#[test]
fn test_valid_github_signature_passes() {
    let sig = hmac_sign(SECRET, BODY);
    assert!(verify_github_signature(SECRET, BODY, Some(&sig)).is_ok());
}

#[test]
fn test_wrong_secret_raises() {
    let sig = hmac_sign("other-secret", BODY);
    assert_eq!(
        verify_github_signature(SECRET, BODY, Some(&sig)),
        Err(HmacError::SignatureMismatch)
    );
}

#[test]
fn test_tampered_body_raises() {
    let sig = hmac_sign(SECRET, BODY);
    let mut tampered = BODY.to_vec();
    tampered.push(b'x');
    assert_eq!(
        verify_github_signature(SECRET, &tampered, Some(&sig)),
        Err(HmacError::SignatureMismatch)
    );
}

#[test]
fn test_missing_header_raises() {
    assert_eq!(
        verify_github_signature(SECRET, BODY, None),
        Err(HmacError::MissingSignature)
    );
}

#[test]
fn test_unsupported_scheme_raises() {
    assert_eq!(
        verify_github_signature(SECRET, BODY, Some("sha1=abc")),
        Err(HmacError::UnsupportedScheme)
    );
}

#[test]
fn test_internal_signature_within_skew_passes() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let sig = hmac_sign(SECRET, BODY);
    assert!(verify_internal_signature(SECRET, BODY, Some(&sig), Some(&ts.to_string()), 30).is_ok());
}

#[test]
fn test_internal_signature_replay_rejected() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 100; // outside +-30s skew
    let sig = hmac_sign(SECRET, BODY);
    assert_eq!(
        verify_internal_signature(SECRET, BODY, Some(&sig), Some(&ts.to_string()), 30),
        Err(HmacError::TimestampSkew)
    );
}
