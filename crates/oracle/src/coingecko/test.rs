#![cfg(test)]

use super::*;

/// Verbatim response captured from a real call to
/// `https://api.coingecko.com/api/v3/simple/price?ids=stellar,usd-coin&vs_currencies=usd` -
/// not a guessed shape.
const REAL_RESPONSE: &str = r#"{"stellar":{"usd":0.188497},"usd-coin":{"usd":0.99985}}"#;

#[test]
fn parses_real_captured_response() {
    assert_eq!(parse_simple_price(REAL_RESPONSE, "stellar").unwrap(), 0.188497);
    assert_eq!(parse_simple_price(REAL_RESPONSE, "usd-coin").unwrap(), 0.99985);
}

#[test]
fn missing_id_is_a_typed_error_not_a_panic() {
    let err = parse_simple_price(REAL_RESPONSE, "bitcoin").unwrap_err();
    assert!(matches!(err, CoinGeckoError::MissingPrice(id) if id == "bitcoin"));
}

#[test]
fn malformed_body_is_a_typed_error_not_a_panic() {
    let err = parse_simple_price("not json", "stellar").unwrap_err();
    assert!(matches!(err, CoinGeckoError::Decode(_)));
}
