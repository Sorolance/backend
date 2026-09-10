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

#[test]
fn maps_known_codes_to_router_error_variants() {
    assert_eq!(RouterError::from_code(7), RouterError::InsufficientLiquidity);
    assert_eq!(RouterError::from_code(8), RouterError::SlippageExceeded);
    assert_eq!(RouterError::from_code(255), RouterError::Unknown(255));
}

#[test]
fn encode_trades_matches_the_vaults_own_cli_shape() {
    // Confirmed live against the deployed vault's `-- rebalance --help`:
    // `--trades '[ { "amount_in": "1", "asset_in": "G...", "asset_out":
    // "G...", "min_amount_out": "1" } ]'` - i128 fields as JSON strings,
    // not numbers.
    let trades = vec![SignedTrade {
        asset_in: "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC".into(),
        asset_out: "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA".into(),
        amount_in: 40,
        min_amount_out: 5,
    }];
    let encoded = encode_trades(&trades);
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(parsed[0]["amount_in"], "40");
    assert_eq!(parsed[0]["min_amount_out"], "5");
    assert_eq!(
        parsed[0]["asset_in"],
        "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC"
    );
}

#[test]
fn encode_trades_empty_is_an_empty_json_array() {
    assert_eq!(encode_trades(&[]), "[]");
}

/// Verbatim stdout captured from a real `vault::compute_allocation` call
/// against the deployed testnet vault while empty.
const COMPUTE_ALLOCATION_STDOUT: &str = r#"[{"asset":"CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC","current_weight_bps":0,"drift_bps":-6000,"target_weight_bps":6000},{"asset":"CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA","current_weight_bps":0,"drift_bps":-4000,"target_weight_bps":4000}]"#;

#[test]
fn parses_real_compute_allocation_output() {
    let entries: Vec<AllocationEntry> = serde_json::from_str(COMPUTE_ALLOCATION_STDOUT).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].target_weight_bps, 6_000);
    assert_eq!(entries[0].drift_bps, -6_000);
    assert_eq!(entries[1].target_weight_bps, 4_000);
}

/// Verbatim stdout captured from a real `stellar fees stats --output
/// json` call against testnet.
const FEE_STATS_STDOUT: &str = r#"{"sorobanInclusionFee":{"max":"200","min":"100","mode":"100","p10":"100","p20":"100","p30":"100","p40":"100","p50":"100","p60":"100","p70":"100","p80":"100","p90":"100","p95":"100","p99":"200","transactionCount":546,"ledgerCount":50},"inclusionFee":{"max":"100","min":"100","mode":"100","p10":"100","p20":"100","p30":"100","p40":"100","p50":"100","p60":"100","p70":"100","p80":"100","p90":"100","p95":"100","p99":"100","transactionCount":28,"ledgerCount":10},"latestLedger":4595628}"#;

#[test]
fn parses_real_fee_stats_output() {
    let stats: FeeStats = serde_json::from_str(FEE_STATS_STDOUT).unwrap();
    assert_eq!(stats.soroban_inclusion_fee.p50, 100);
    assert_eq!(stats.soroban_inclusion_fee.p99, 200);
}
