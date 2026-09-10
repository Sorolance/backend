//! Talks to the deployed `vault` contract via the `stellar` CLI rather
//! than a hand-rolled Soroban RPC/XDR client - there's no first-party Rust
//! client for that, and the CLI is exactly what this project's own
//! testnet deploys and smoke tests already use (see `PROJECT.md`), so
//! behavior here matches what's been manually verified to work rather
//! than a fresh, untested integration.
//!
//! Exact CLI output shapes below (stdout/stderr split, success/error
//! text) were confirmed live against the deployed testnet vault
//! (`CCECBZTH32DPRA4ZUHNVR5M2JRSS5A6RDS3HZTTCORIVVM6TMAFHEMMI`) with
//! `stellar` 27.0.0, not guessed from documentation.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct ChainClient {
    pub stellar_cli: String,
    pub rpc_url: String,
    pub network_passphrase: String,
    pub vault_contract_id: String,
    pub keeper_identity: String,
    pub keeper_address: String,
    /// The deployed `router` (see `contracts/contracts/router`) - needed
    /// directly by fee-aware execution's `quote_swap` (a plain read-only
    /// call the vault itself doesn't proxy) even though `vault::rebalance`
    /// already knows its own router internally.
    pub router_contract_id: String,
}

/// Mirrors `contracts::vault::Error`
/// (`contracts/contracts/vault/src/lib.rs`) - kept in sync by hand since
/// the scheduler only ever sees the numeric code over the CLI's stderr,
/// never the contract's own enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    NotInitialized,
    AlreadyInitialized,
    InvalidTargets,
    InvalidThreshold,
    InvalidAmount,
    AssetNotInTargets,
    Paused,
    Unauthorized,
    BelowThreshold,
    RouterNotConfigured,
    Unknown(u32),
}

impl VaultError {
    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::NotInitialized,
            2 => Self::AlreadyInitialized,
            3 => Self::InvalidTargets,
            4 => Self::InvalidThreshold,
            5 => Self::InvalidAmount,
            6 => Self::AssetNotInTargets,
            7 => Self::Paused,
            8 => Self::Unauthorized,
            9 => Self::BelowThreshold,
            10 => Self::RouterNotConfigured,
            other => Self::Unknown(other),
        }
    }
}

/// Mirrors `contracts::router::Error`
/// (`contracts/contracts/router/src/lib.rs`) - the router's own error
/// numbering is unrelated to `VaultError`'s, so a call that reaches the
/// router directly (`quote_swap`) needs its own decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterError {
    NotInitialized,
    AlreadyInitialized,
    InvalidFee,
    InvalidTokenPair,
    UnsupportedAsset,
    InvalidAmount,
    InsufficientLiquidity,
    SlippageExceeded,
    Unknown(u32),
}

impl RouterError {
    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::NotInitialized,
            2 => Self::AlreadyInitialized,
            3 => Self::InvalidFee,
            4 => Self::InvalidTokenPair,
            5 => Self::UnsupportedAsset,
            6 => Self::InvalidAmount,
            7 => Self::InsufficientLiquidity,
            8 => Self::SlippageExceeded,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug)]
pub enum ChainError {
    /// Couldn't even run the `stellar` binary (not on `PATH`, etc).
    Spawn(std::io::Error),
    /// The vault contract itself rejected the call - a normal, expected
    /// outcome (e.g. `RouterNotConfigured` until Phase 4 wires a router),
    /// not a scheduler bug.
    Contract(VaultError),
    /// The router contract rejected a direct call (`quote_swap`) - same
    /// "expected, not a bug" status as `Contract`, e.g.
    /// `InsufficientLiquidity` on a pair with no seeded liquidity yet.
    RouterContract(RouterError),
    /// The CLI failed in a way that wasn't a contract error (network
    /// issue, bad args, CLI version skew) - stderr verbatim.
    Cli(String),
    /// The call reported success but stdout/stderr weren't in the shape
    /// this client expects - most likely a `stellar` CLI version bump
    /// changed its output format.
    UnexpectedOutput(String),
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "failed to run stellar CLI: {e}"),
            Self::Contract(e) => write!(f, "vault contract rejected the call: {e:?}"),
            Self::RouterContract(e) => write!(f, "router contract rejected the call: {e:?}"),
            Self::Cli(s) => write!(f, "stellar CLI error: {s}"),
            Self::UnexpectedOutput(s) => write!(f, "unexpected stellar CLI output: {s}"),
        }
    }
}

impl std::error::Error for ChainError {}

pub struct RebalanceOutcome {
    pub tx_hash: String,
}

/// One trade to submit as part of `vault::rebalance` - mirrors the
/// contract's own `TradeInstruction`, adding a real `min_amount_out` on
/// top of `rebalancer_core::TradeIntent`'s `amount_in` once a caller has
/// quoted it via `quote_swap`.
#[derive(Debug, Clone)]
pub struct SignedTrade {
    pub asset_in: String,
    pub asset_out: String,
    pub amount_in: i128,
    pub min_amount_out: i128,
}

/// Live network fee conditions from `stellar fees stats`, in stroops -
/// the same fixed-point units as a native-XLM token balance (both are
/// literally counted in stroops), so a caller can turn `p99` directly into
/// a value comparable to a trade's own value by multiplying by XLM's
/// price, no separate decimals conversion needed. `p99` (not `mode`/`min`)
/// is what `run_once` feeds into `rebalancer_core::total_cost_bps` as
/// "the network fee": it's the live, observable worst-case-this-tick fee,
/// so a real congestion spike shows up as a higher `p99` without this
/// client needing to track any history of its own to detect one.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct FeeStats {
    #[serde(rename = "sorobanInclusionFee")]
    pub soroban_inclusion_fee: InclusionFeeStats,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct InclusionFeeStats {
    #[serde(deserialize_with = "deserialize_stroop_str")]
    pub p50: i128,
    #[serde(deserialize_with = "deserialize_stroop_str")]
    pub p99: i128,
}

fn deserialize_stroop_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
    let s = <String as serde::Deserialize>::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// One entry from `vault::compute_allocation` - mirrors the contract's own
/// `AllocationEntry`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AllocationEntry {
    pub asset: String,
    pub target_weight_bps: u32,
    #[allow(dead_code)] // parsed for completeness; run_once only needs target_weight_bps + drift_bps today
    pub current_weight_bps: u32,
    pub drift_bps: i32,
}

impl ChainClient {
    fn common_args(&self) -> Vec<String> {
        self.invoke_args(&self.vault_contract_id)
    }

    fn invoke_args(&self, contract_id: &str) -> Vec<String> {
        vec![
            "contract".into(),
            "invoke".into(),
            "--id".into(),
            contract_id.into(),
            "--source-account".into(),
            self.keeper_identity.clone(),
            "--rpc-url".into(),
            self.rpc_url.clone(),
            "--network-passphrase".into(),
            self.network_passphrase.clone(),
        ]
    }

    /// Read-only simulation of `vault::needs_rebalance` - no fee, no auth
    /// required. Note: as of this writing the on-chain contract's own
    /// drift math reports an *empty* vault (zero balance) as needing a
    /// rebalance too (it compares 0% actual against the nonzero target,
    /// which reads as maximum drift) - see `README.md`. That's a contract
    /// bug to fix upstream, not something to paper over here; this client
    /// just reports whatever the contract says.
    pub fn needs_rebalance(&self) -> Result<bool, ChainError> {
        let mut args = self.common_args();
        args.push("--".into());
        args.push("needs_rebalance".into());
        let out = self.run_vault(&args)?;
        match out.stdout.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(ChainError::UnexpectedOutput(other.to_string())),
        }
    }

    /// Submits `vault::observe_risk(caller = keeper)`, signed by the
    /// keeper identity - feeds current oracle prices into the configured
    /// `risk_guard` so its circuit breaker stays current independent of
    /// whether a rebalance is imminent (see the `contracts` repo).
    /// Returns `true` if this call just tripped the breaker (it wasn't
    /// already tripped); `false` covers "no risk_guard configured", "no
    /// trip", and "was already tripped" alike - callers that need to
    /// distinguish those should call `vault::is_tripped` via
    /// `risk_guard` directly, which this client doesn't wrap since
    /// nothing here needs to yet.
    pub fn observe_risk(&self) -> Result<bool, ChainError> {
        let mut args = self.common_args();
        args.push("--send=yes".into());
        args.push("--".into());
        args.push("observe_risk".into());
        args.push("--caller".into());
        args.push(self.keeper_address.clone());
        let out = self.run_vault(&args)?;
        match out.stdout.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(ChainError::UnexpectedOutput(other.to_string())),
        }
    }

    /// Submits `vault::rebalance(caller = keeper, trades)`, signed by the
    /// keeper identity. Before Phase 4 this always sent `trades = []`
    /// (matching the era before a router existed); now that
    /// `run_once` computes real `TradeIntent`s and quotes them via
    /// `quote_swap`, `trades` here is the real, fee-aware-approved set to
    /// execute this tick - a caller passing `[]` gets the pre-router
    /// behavior back (a genuine no-op rebalance, still gated the same way
    /// on-chain). Callers should still treat `Err(ChainError::Contract(_))`
    /// as a normal, loggable outcome (e.g. `RouterNotConfigured` if
    /// `set_router` was never called for this vault).
    pub fn submit_rebalance(&self, trades: &[SignedTrade]) -> Result<RebalanceOutcome, ChainError> {
        let mut args = self.common_args();
        args.push("--send=yes".into());
        args.push("--".into());
        args.push("rebalance".into());
        args.push("--caller".into());
        args.push(self.keeper_address.clone());
        args.push("--trades".into());
        args.push(encode_trades(trades));
        let out = self.run_vault(&args)?;

        // On success `rebalance` returns `()`, which the CLI prints to
        // stdout as `null`; the tx hash itself only appears on stderr, on
        // the "Signing transaction: <hash>" progress line.
        if out.stdout.trim() != "null" {
            return Err(ChainError::UnexpectedOutput(out.stdout));
        }
        let tx_hash = extract_tx_hash(&out.stderr)
            .ok_or_else(|| ChainError::UnexpectedOutput(out.stderr.clone()))?;
        Ok(RebalanceOutcome { tx_hash })
    }

    /// Read-only `vault::compute_allocation` - the per-asset target
    /// weight/current weight/drift the vault itself is enforcing right
    /// now. `run_once` uses this (rather than mirroring target weights in
    /// off-chain config, which could drift out of sync with a real
    /// `set_targets` call) as the source of truth for which assets to
    /// build `rebalancer_core::TargetWeight`s from, and for the batch's
    /// `max_drift_bps` that feeds the urgency override in
    /// `rebalancer_core::evaluate_fee_aware_execution`.
    pub fn compute_allocation(&self) -> Result<Vec<AllocationEntry>, ChainError> {
        let mut args = self.common_args();
        args.push("--".into());
        args.push("compute_allocation".into());
        let out = self.run_vault(&args)?;
        serde_json::from_str(out.stdout.trim())
            .map_err(|_| ChainError::UnexpectedOutput(out.stdout))
    }

    /// Read-only SEP-41 `balance(id)` on an arbitrary token contract - used
    /// to get each target asset's real live balance (fee-aware execution
    /// needs absolute values, not just the weight ratios
    /// `compute_allocation` reports) via `pricing::ASSETS`' fixed
    /// `token_contract_id`s. No auth required, so `--source-account` here
    /// just needs to be any funded identity, not a privileged one - same
    /// as `rebalancer_oracle::on_chain::OnChainPriceReader`.
    pub fn token_balance(&self, token_contract_id: &str, holder: &str) -> Result<i128, ChainError> {
        let mut args = self.invoke_args(token_contract_id);
        args.push("--".into());
        args.push("balance".into());
        args.push("--id".into());
        args.push(holder.into());
        let out = self.run_opaque(&args)?;
        out.stdout
            .trim()
            .trim_matches('"')
            .parse()
            .map_err(|_| ChainError::UnexpectedOutput(out.stdout))
    }

    /// Read-only `router::get_amount_out` - the real AMM quote for a
    /// candidate trade, fed into `rebalancer_core::slippage_bps` alongside
    /// the oracle-implied fair price to measure this trade's price impact
    /// before deciding whether to execute it this tick. No auth required.
    pub fn quote_swap(&self, asset_in: &str, asset_out: &str, amount_in: i128) -> Result<i128, ChainError> {
        let mut args = self.invoke_args(&self.router_contract_id);
        args.push("--".into());
        args.push("get_amount_out".into());
        args.push("--asset_in".into());
        args.push(asset_in.into());
        args.push("--asset_out".into());
        args.push(asset_out.into());
        args.push("--amount_in".into());
        args.push(amount_in.to_string());
        let out = self.run_router(&args)?;
        out.stdout
            .trim()
            .trim_matches('"')
            .parse()
            .map_err(|_| ChainError::UnexpectedOutput(out.stdout))
    }

    /// Live network fee percentiles via `stellar fees stats` - see
    /// `FeeStats`'s doc comment for why `p99` (not `mode`) is what
    /// fee-aware execution actually reads. Doesn't go through `run`/
    /// `common_args`: this is a CLI subcommand of its own
    /// (`stellar fees stats`), not a `contract invoke`, so it takes no
    /// `--id`/`--source-account`.
    pub fn fee_stats(&self) -> Result<FeeStats, ChainError> {
        let output = Command::new(&self.stellar_cli)
            .args([
                "fees",
                "stats",
                "--output",
                "json",
                "--rpc-url",
                &self.rpc_url,
                "--network-passphrase",
                &self.network_passphrase,
            ])
            .output()
            .map_err(ChainError::Spawn)?;
        if !output.status.success() {
            return Err(ChainError::Cli(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        serde_json::from_str(stdout.trim()).map_err(|_| ChainError::UnexpectedOutput(stdout))
    }

    fn run_vault(&self, args: &[String]) -> Result<CommandOutput, ChainError> {
        self.run(args).map_err(|raw| raw.into_chain_error(ChainError::Contract, VaultError::from_code))
    }

    fn run_router(&self, args: &[String]) -> Result<CommandOutput, ChainError> {
        self.run(args)
            .map_err(|raw| raw.into_chain_error(ChainError::RouterContract, RouterError::from_code))
    }

    /// For calls against a plain SEP-41 token contract (`token_balance`) -
    /// no `VaultError`/`RouterError` variant applies, so a contract error
    /// (which shouldn't happen for a no-auth `balance` read in practice)
    /// surfaces as an opaque `ChainError::Cli` rather than being
    /// mislabeled as one of those two enums.
    fn run_opaque(&self, args: &[String]) -> Result<CommandOutput, ChainError> {
        self.run(args)
            .map_err(|raw| raw.into_chain_error(ChainError::Cli, |code| format!("contract error #{code}")))
    }

    fn run(&self, args: &[String]) -> Result<CommandOutput, RawCliError> {
        let output = Command::new(&self.stellar_cli)
            .args(args)
            .output()
            .map_err(|e| RawCliError::Spawn(ChainError::Spawn(e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            Ok(CommandOutput { stdout, stderr })
        } else if let Some(code) = extract_contract_error_code(&stderr) {
            Err(RawCliError::Contract(code))
        } else {
            Err(RawCliError::Spawn(ChainError::Cli(stderr.trim().to_string())))
        }
    }
}

/// Intermediate outcome of a raw CLI invocation, before the caller decides
/// which contract's error enum a numeric code should decode into -
/// `run_vault`/`run_router` are the two real decoders; `run` itself stays
/// contract-agnostic so it can back both `token_balance` (a plain SEP-41
/// token, no meaningful error enum of its own to decode) and `fee_stats`-
/// adjacent future callers without inventing a third meaning for "no
/// error variant applies".
enum RawCliError {
    /// Not a contract error - already a fully-formed `ChainError` (spawn
    /// failure or opaque CLI error).
    Spawn(ChainError),
    /// A contract error code was found - the caller picks how to decode it.
    Contract(u32),
}

impl RawCliError {
    fn into_chain_error<E>(self, wrap: impl Fn(E) -> ChainError, decode: impl Fn(u32) -> E) -> ChainError {
        match self {
            Self::Spawn(e) => e,
            Self::Contract(code) => wrap(decode(code)),
        }
    }
}

/// Encodes `trades` into the `--trades '[ { ... } ]'` JSON shape the
/// vault's implicit CLI expects (`i128`/`Address` fields as strings -
/// confirmed live against the deployed vault's own `-- rebalance --help`,
/// not guessed from the SDK's XDR encoding).
fn encode_trades(trades: &[SignedTrade]) -> String {
    let entries: Vec<serde_json::Value> = trades
        .iter()
        .map(|t| {
            serde_json::json!({
                "amount_in": t.amount_in.to_string(),
                "asset_in": t.asset_in,
                "asset_out": t.asset_out,
                "min_amount_out": t.min_amount_out.to_string(),
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

struct CommandOutput {
    stdout: String,
    stderr: String,
}

/// Pulls the numeric code out of `Error(Contract, #10)`, however deep in
/// the CLI's diagnostic event log it's buried.
fn extract_contract_error_code(stderr: &str) -> Option<u32> {
    let marker = "Error(Contract, #";
    let start = stderr.find(marker)? + marker.len();
    let rest = &stderr[start..];
    let end = rest.find(')')?;
    rest[..end].parse().ok()
}

/// Pulls the hash out of the CLI's `ℹ️  Signing transaction: <hash>`
/// progress line. Uses `find` rather than `strip_prefix` on a trimmed
/// line because the line starts with an emoji glyph, not whitespace, so
/// `trim()` alone doesn't get past it.
fn extract_tx_hash(stderr: &str) -> Option<String> {
    let marker = "Signing transaction: ";
    let line = stderr.lines().find(|line| line.contains(marker))?;
    let start = line.find(marker)? + marker.len();
    Some(line[start..].trim().to_string())
}

#[cfg(test)]
mod test;
