#![cfg(test)]

use super::*;

#[test]
fn parses_quoted_i128_stdout() {
    // Real stdout captured from a live `oracle_adapter.get_price` call.
    assert_eq!(parse_i128("\"18808876553202\"\n").unwrap(), 18_808_876_553_202);
}

#[test]
fn parses_unquoted_stdout_too() {
    // `decimals` returns a plain, unquoted integer - same parser handles
    // both since trim_matches('"') is a no-op when there's nothing to trim.
    assert_eq!(parse_i128("14\n").unwrap(), 14);
}

#[test]
fn non_numeric_output_is_a_typed_error_not_a_panic() {
    assert!(matches!(
        parse_i128("null"),
        Err(OnChainPriceError::UnexpectedOutput(_))
    ));
}
