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
