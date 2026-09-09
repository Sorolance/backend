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

/// Verbatim (truncated to a few points) response captured from a real
/// call to
/// `https://api.coingecko.com/api/v3/coins/stellar/market_chart?vs_currency=usd&days=7&interval=daily`.
const REAL_MARKET_CHART: &str = r#"{"prices":[[1788307200000,0.17571705014001854],[1788393600000,0.17548159483107895],[1788480000000,0.18453942912808005]],"market_caps":[[1788307200000,6097216301.800812]],"total_volumes":[[1788307200000,112881236.55143628]]}"#;

#[test]
fn parses_real_captured_market_chart() {
    let points = parse_market_chart(REAL_MARKET_CHART).unwrap();
    assert_eq!(points.len(), 3);
    assert_eq!(points[0].price_usd, 0.17571705014001854);
    assert_eq!(points[0].timestamp.timestamp_millis(), 1788307200000);
    assert_eq!(points[2].price_usd, 0.18453942912808005);
}

#[test]
fn market_chart_malformed_body_is_a_typed_error() {
    let err = parse_market_chart("not json").unwrap_err();
    assert!(matches!(err, CoinGeckoError::Decode(_)));
}
