use monkey::hmac_auth::{
    HmacError, hmac_sign, hmac_sign_with_timestamp, verify_github_signature,
    verify_internal_signature,
};
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
fn test_github_signature_hex_is_case_insensitive() {
    let signature = hmac_sign(SECRET, BODY);
    let hex_part = &signature["sha256=".len()..];
    let upper_signature = format!("sha256={}", hex_part.to_uppercase());

    assert!(verify_github_signature(SECRET, BODY, Some(&upper_signature)).is_ok());
}

#[test]
fn test_github_signature_wrong_length_is_rejected() {
    assert_eq!(
        verify_github_signature(SECRET, BODY, Some("sha256=abcd")),
        Err(HmacError::SignatureMismatch)
    );
}

#[test]
fn test_internal_signature_extreme_timestamp_does_not_overflow() {
    // i64::MIN would overflow a plain (now - timestamp) subtraction.
    let signature = hmac_sign_with_timestamp(SECRET, BODY, 0);
    assert_eq!(
        verify_internal_signature(
            SECRET,
            BODY,
            Some(&signature),
            Some(&i64::MIN.to_string()),
            30,
        ),
        Err(HmacError::TimestampSkew)
    );
}

#[test]
fn test_internal_signature_within_skew_passes() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let sig = hmac_sign_with_timestamp(SECRET, BODY, ts);
    assert!(verify_internal_signature(SECRET, BODY, Some(&sig), Some(&ts.to_string()), 30).is_ok());
}

#[test]
fn test_internal_signature_replay_rejected() {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 100; // outside +-30s skew
    let sig = hmac_sign_with_timestamp(SECRET, BODY, ts);
    assert_eq!(
        verify_internal_signature(SECRET, BODY, Some(&sig), Some(&ts.to_string()), 30),
        Err(HmacError::TimestampSkew)
    );
}

#[test]
fn test_internal_signature_rejects_fresh_timestamp_replay() {
    let old_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 3600;
    let fresh_timestamp = old_timestamp + 3600;
    let captured_signature = hmac_sign_with_timestamp(SECRET, BODY, old_timestamp);

    assert_eq!(
        verify_internal_signature(
            SECRET,
            BODY,
            Some(&captured_signature),
            Some(&fresh_timestamp.to_string()),
            30,
        ),
        Err(HmacError::SignatureMismatch)
    );
}

#[test]
fn test_internal_signature_rejects_unbound_signature() {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let unbound_signature = hmac_sign(SECRET, BODY);

    assert_eq!(
        verify_internal_signature(
            SECRET,
            BODY,
            Some(&unbound_signature),
            Some(&timestamp.to_string()),
            30,
        ),
        Err(HmacError::SignatureMismatch)
    );
}
