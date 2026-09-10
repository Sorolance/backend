#![cfg(test)]

use super::*;

#[test]
fn matches_openssl_reference_vector() {
    // Ground truth computed independently, not by this code:
    //   printf '%s' '{"event":"rebalance.completed"}' \
    //     | openssl dgst -sha256 -hmac "test-secret"
    // => 3afa495916bcbfd7d66a291dd1b85e72c1b0d84b0410aa1eb30bc0f227f17057
    let signature = sign(
        "test-secret",
        br#"{"event":"rebalance.completed"}"#,
    );
    assert_eq!(
        signature,
        "3afa495916bcbfd7d66a291dd1b85e72c1b0d84b0410aa1eb30bc0f227f17057"
    );
}

#[test]
fn different_secrets_produce_different_signatures() {
    let body = b"same body";
    assert_ne!(sign("secret-a", body), sign("secret-b", body));
}

#[test]
fn different_bodies_produce_different_signatures() {
    assert_ne!(sign("secret", b"body a"), sign("secret", b"body b"));
}

#[test]
fn verify_signature_accepts_a_correctly_signed_body() {
    let body = br#"{"reason":"custom_condition"}"#;
    let header = format!("sha256={}", sign("test-secret", body));
    assert!(verify_signature("test-secret", body, &header));
}

#[test]
fn verify_signature_rejects_wrong_secret() {
    let body = b"trigger me";
    let header = format!("sha256={}", sign("right-secret", body));
    assert!(!verify_signature("wrong-secret", body, &header));
}

#[test]
fn verify_signature_rejects_tampered_body() {
    let header = format!("sha256={}", sign("secret", b"original"));
    assert!(!verify_signature("secret", b"tampered", &header));
}

#[test]
fn verify_signature_rejects_malformed_header() {
    let body = b"trigger me";
    assert!(!verify_signature("secret", body, &sign("secret", body)));
    assert!(!verify_signature("secret", body, "sha256=not-hex"));
    assert!(!verify_signature("secret", body, ""));
}
