#![cfg(test)]

use super::*;

/// Verbatim stderr captured from a real failed `rebalance` call against
/// the deployed testnet vault (RouterNotConfigured = error code 10) -
/// not a guessed shape.
const ROUTER_NOT_CONFIGURED_STDERR: &str = "\
❌ error: transaction simulation failed: HostError: Error(Contract, #10)

Event log (newest first):
   0: [Diagnostic Event] contract:CCECBZTH32DPRA4ZUHNVR5M2JRSS5A6RDS3HZTTCORIVVM6TMAFHEMMI, topics:[error, Error(Contract, #10)], data:\"escalating Ok(ScErrorType::Contract) frame-exit to Err\"
";

#[test]
fn extracts_contract_error_code_from_real_cli_stderr() {
    assert_eq!(
        extract_contract_error_code(ROUTER_NOT_CONFIGURED_STDERR),
        Some(10)
    );
}

#[test]
fn maps_known_codes_to_vault_error_variants() {
    assert_eq!(VaultError::from_code(9), VaultError::BelowThreshold);
    assert_eq!(VaultError::from_code(10), VaultError::RouterNotConfigured);
    assert_eq!(VaultError::from_code(8), VaultError::Unauthorized);
}

#[test]
fn unrecognized_code_falls_back_to_unknown_rather_than_panicking() {
    assert_eq!(VaultError::from_code(255), VaultError::Unknown(255));
}

#[test]
fn no_contract_error_marker_returns_none() {
    assert_eq!(
        extract_contract_error_code("connection refused: could not reach RPC endpoint"),
        None
    );
}

/// Verbatim stderr from a real successful `set_keeper` call, used here as
/// a stand-in for what a successful `rebalance` looks like once Phase 4
/// wires a router - same CLI progress-line shape either way.
const SUCCESSFUL_SEND_STDERR: &str = "\
ℹ️  Simulating transaction…
ℹ️  Signing transaction: 865275702e1d5a29ba083156bbacc6dbff6eff9be0ba74f92be46aa6d5d917a7
🌎 Sending transaction…
✅ Transaction submitted successfully!
🔗 https://stellar.expert/explorer/testnet/tx/865275702e1d5a29ba083156bbacc6dbff6eff9be0ba74f92be46aa6d5d917a7
";

#[test]
fn extracts_tx_hash_from_signing_line() {
    assert_eq!(
        extract_tx_hash(SUCCESSFUL_SEND_STDERR).as_deref(),
        Some("865275702e1d5a29ba083156bbacc6dbff6eff9be0ba74f92be46aa6d5d917a7")
    );
}

#[test]
fn no_signing_line_returns_none() {
    assert_eq!(extract_tx_hash(ROUTER_NOT_CONFIGURED_STDERR), None);
}
