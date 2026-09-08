//! Reads prices from the deployed `oracle_adapter` contract (Reflector,
//! staleness-checked - see the `contracts` repo) via the `stellar` CLI,
//! the same approach `rebalancer_scheduler::chain` uses for `vault` and
//! for the same reason: no first-party Rust client for Soroban RPC
//! exists yet. This is a read-only call (`get_price` requires no auth on
//! `oracle_adapter`), so `source_account` just needs to be *any* funded
//! identity, not a privileged one.

use std::process::Command;

#[derive(Clone)]
pub struct OnChainPriceReader {
    pub stellar_cli: String,
    pub rpc_url: String,
    pub network_passphrase: String,
    pub oracle_adapter_id: String,
    pub source_account: String,
}

#[derive(Debug)]
pub enum OnChainPriceError {
    Spawn(std::io::Error),
    /// The CLI exited non-zero - covers both a contract-level error (e.g.
    /// `PriceStale`/`PriceUnavailable` from `oracle_adapter`) and a CLI/
    /// network-level failure alike, since this crate only needs to know
    /// "did I get a trustworthy price", not which specific reason it
    /// didn't.
    Cli(String),
    UnexpectedOutput(String),
}

impl std::fmt::Display for OnChainPriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to run stellar CLI: {e}"),
            Self::Cli(s) => write!(f, "oracle_adapter rejected the read: {s}"),
            Self::UnexpectedOutput(s) => write!(f, "unexpected stellar CLI output: {s}"),
        }
    }
}

impl std::error::Error for OnChainPriceError {}

impl OnChainPriceReader {
    fn common_args(&self) -> Vec<String> {
        vec![
            "contract".into(),
            "invoke".into(),
            "--id".into(),
            self.oracle_adapter_id.clone(),
            "--source-account".into(),
            self.source_account.clone(),
            "--rpc-url".into(),
            self.rpc_url.clone(),
            "--network-passphrase".into(),
            self.network_passphrase.clone(),
        ]
    }

    /// `price_asset_kind`/`price_asset_value` mirror
    /// `rebalancer_db::Target`'s own split (`"stellar"` + a token
    /// address, or `"other"` + a symbol like `"XLM"`) - matches
    /// `oracle_common::Asset`.
    pub fn get_price(
        &self,
        price_asset_kind: &str,
        price_asset_value: &str,
    ) -> Result<i128, OnChainPriceError> {
        let asset_json = match price_asset_kind {
            "stellar" | "Stellar" => format!(r#"{{"Stellar":"{price_asset_value}"}}"#),
            _ => format!(r#"{{"Other":"{price_asset_value}"}}"#),
        };
        let mut args = self.common_args();
        args.push("--".into());
        args.push("get_price".into());
        args.push("--asset".into());
        args.push(asset_json);

        let out = self.run(&args)?;
        parse_i128(&out)
    }

    pub fn decimals(&self) -> Result<u32, OnChainPriceError> {
        let mut args = self.common_args();
        args.push("--".into());
        args.push("decimals".into());
        let out = self.run(&args)?;
        out.trim()
            .parse()
            .map_err(|_| OnChainPriceError::UnexpectedOutput(out))
    }

    fn run(&self, args: &[String]) -> Result<String, OnChainPriceError> {
        let output = Command::new(&self.stellar_cli)
            .args(args)
            .output()
            .map_err(OnChainPriceError::Spawn)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(OnChainPriceError::Cli(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ))
        }
    }
}

/// `get_price` returns an i128 as a JSON-quoted string (e.g.
/// `"18808876553202"`, quoted because it can exceed a JS-safe integer) -
/// confirmed against the real deployed `oracle_adapter`.
fn parse_i128(stdout: &str) -> Result<i128, OnChainPriceError> {
    stdout
        .trim()
        .trim_matches('"')
        .parse()
        .map_err(|_| OnChainPriceError::UnexpectedOutput(stdout.to_string()))
}

#[cfg(test)]
mod test;
