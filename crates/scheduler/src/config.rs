//! Environment-based configuration. There's no multi-tenant onboarding
//! flow yet (no `api` crate, no portfolio-creation UI - see PROJECT.md),
//! so this scheduler runs against exactly one vault, the same one the
//! frontend currently hardcodes.

use std::env;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub rpc_url: String,
    pub network_passphrase: String,
    /// Path to the `stellar` CLI binary. Defaults to relying on `PATH`.
    pub stellar_cli: String,
    pub vault_contract_id: String,
    /// A `stellar keys` identity name (not a raw secret key) that the CLI
    /// resolves locally - keeps the keeper's secret out of this process's
    /// environment. See `README.md` for provisioning it.
    pub keeper_identity: String,
    /// The keeper's own `G...` address, passed as `rebalance`'s `caller`
    /// argument - separate from `keeper_identity` because the CLI needs
    /// the address as plain contract-call data, not just as a signer.
    pub keeper_address: String,
    /// Mirrored into `portfolios.owner_address` for the dashboard/API to
    /// query by owner - the contract itself remains the source of truth
    /// for who can actually withdraw or reconfigure.
    pub owner_address: String,
    pub portfolio_name: String,
    /// Mirrored into `portfolios.threshold_bps` for display purposes only.
    /// The contract enforces its own on-chain threshold independently;
    /// this must be kept in sync by hand until `vault` exposes a getter
    /// for it (it doesn't today).
    pub threshold_bps: i32,
    pub poll_interval_secs: u64,
    /// The deployed `oracle_adapter` this vault reads from - needed here
    /// (separately from `vault_contract_id`) because `observe_market_prices`
    /// reads Reflector prices directly, for the CoinGecko cross-check.
    pub oracle_adapter_contract_id: String,
    /// How far apart the Reflector- and CoinGecko-derived prices for the
    /// same asset can be, in bps of the CoinGecko price, before it's
    /// logged as a warning. Purely observational today (see
    /// `rebalancer_oracle`'s crate doc comment for why) - nothing acts on
    /// this beyond a log line.
    pub price_divergence_warn_bps: u32,
}

#[derive(Debug)]
pub enum ConfigError {
    Missing(&'static str),
    Invalid(&'static str, String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "missing required environment variable {key}"),
            Self::Invalid(key, value) => {
                write!(f, "invalid value for {key}: {value:?}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            database_url: require("DATABASE_URL")?,
            rpc_url: optional("RPC_URL", "https://soroban-testnet.stellar.org"),
            network_passphrase: optional(
                "NETWORK_PASSPHRASE",
                "Test SDF Network ; September 2015",
            ),
            stellar_cli: optional("STELLAR_CLI", "stellar"),
            vault_contract_id: require("VAULT_CONTRACT_ID")?,
            keeper_identity: require("KEEPER_IDENTITY")?,
            keeper_address: require("KEEPER_ADDRESS")?,
            owner_address: require("OWNER_ADDRESS")?,
            portfolio_name: optional("PORTFOLIO_NAME", "Demo Portfolio"),
            threshold_bps: parse_optional("THRESHOLD_BPS", 500)?,
            poll_interval_secs: parse_optional("POLL_INTERVAL_SECS", 300)?,
            oracle_adapter_contract_id: require("ORACLE_ADAPTER_CONTRACT_ID")?,
            price_divergence_warn_bps: parse_optional("PRICE_DIVERGENCE_WARN_BPS", 300)?,
        })
    }
}

fn require(key: &'static str) -> Result<String, ConfigError> {
    env::var(key).map_err(|_| ConfigError::Missing(key))
}

fn optional(key: &'static str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn parse_optional<T: FromStr>(key: &'static str, default: T) -> Result<T, ConfigError> {
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|_| ConfigError::Invalid(key, value)),
        Err(_) => Ok(default),
    }
}
