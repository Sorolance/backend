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

#[derive(Debug)]
pub enum ChainError {
    /// Couldn't even run the `stellar` binary (not on `PATH`, etc).
    Spawn(std::io::Error),
    /// The contract itself rejected the call - a normal, expected outcome
    /// (e.g. `RouterNotConfigured` until Phase 4 wires a router), not a
    /// scheduler bug.
    Contract(VaultError),
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
            Self::Cli(s) => write!(f, "stellar CLI error: {s}"),
            Self::UnexpectedOutput(s) => write!(f, "unexpected stellar CLI output: {s}"),
        }
    }
}

impl std::error::Error for ChainError {}

pub struct RebalanceOutcome {
    pub tx_hash: String,
}

impl ChainClient {
    fn common_args(&self) -> Vec<String> {
        vec![
            "contract".into(),
            "invoke".into(),
            "--id".into(),
            self.vault_contract_id.clone(),
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
        let out = self.run(&args)?;
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
        let out = self.run(&args)?;
        match out.stdout.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(ChainError::UnexpectedOutput(other.to_string())),
        }
    }

    /// Submits `vault::rebalance(caller = keeper, trades = [])`, signed by
    /// the keeper identity. No router is wired in as of Phase 1, so this
    /// is *expected* to fail closed with `RouterNotConfigured` until
    /// Phase 4 - callers should treat `Err(ChainError::Contract(_))` as a
    /// normal, loggable outcome.
    pub fn submit_rebalance(&self) -> Result<RebalanceOutcome, ChainError> {
        let mut args = self.common_args();
        args.push("--send=yes".into());
        args.push("--".into());
        args.push("rebalance".into());
        args.push("--caller".into());
        args.push(self.keeper_address.clone());
        args.push("--trades".into());
        args.push("[]".into());
        let out = self.run(&args)?;

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

    fn run(&self, args: &[String]) -> Result<CommandOutput, ChainError> {
        let output = Command::new(&self.stellar_cli)
            .args(args)
            .output()
            .map_err(ChainError::Spawn)?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            Ok(CommandOutput { stdout, stderr })
        } else if let Some(code) = extract_contract_error_code(&stderr) {
            Err(ChainError::Contract(VaultError::from_code(code)))
        } else {
            Err(ChainError::Cli(stderr.trim().to_string()))
        }
    }
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
