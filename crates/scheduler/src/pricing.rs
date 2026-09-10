//! Independently polls both price sources for every configured asset and
//! records what each one said - separate from `run_once`'s own
//! rebalance-decision logic (which still asks the deployed `vault`
//! directly, and stays Reflector-only no matter what this module
//! observes; see `rebalancer_oracle`'s crate doc comment for the scope
//! note on what "CoinGecko fallback" does and doesn't mean here).

use bigdecimal::BigDecimal;
use chrono::Utc;
use rebalancer_db::PgPool;
use rebalancer_oracle::coingecko::CoinGeckoClient;
use rebalancer_oracle::on_chain::OnChainPriceReader;
use std::str::FromStr;
use tracing::{error, info, warn};

/// This project's fixed, small asset universe (see PROJECT.md's "Asset
/// universe for v1" decision) - not meant to grow without a matching
/// change to `vault`'s own targets, so it's a plain constant here rather
/// than something pulled from config or a database table.
pub struct AssetConfig {
    pub symbol: &'static str,
    /// Matches `rebalancer_db::Target.price_asset_kind`/`oracle_common::Asset`.
    pub price_asset_kind: &'static str,
    pub price_asset_value: &'static str,
    /// CoinGecko's own id for this asset (not its ticker symbol).
    pub coingecko_id: &'static str,
    /// The SAC (Stellar Asset Contract) address this asset is actually
    /// custodied under - `vault::TargetWeight.asset` and
    /// `router::{Token{A,B}}`'s own values, needed by fee-aware execution
    /// (`chain::token_balance`, `chain::quote_swap`) to look up a live
    /// balance or router quote for a `vault::compute_allocation` entry's
    /// address without a separate on-chain lookup. Hardcoded alongside
    /// `coingecko_id` for the same reason: this project's fixed asset
    /// universe (see PROJECT.md's "Asset universe for v1" decision), not
    /// deployment config like `VAULT_CONTRACT_ID` - it changes only if the
    /// asset itself changes, not on every redeploy of `vault`/`router`
    /// around it.
    pub token_contract_id: &'static str,
}

pub const ASSETS: &[AssetConfig] = &[
    AssetConfig {
        symbol: "XLM",
        price_asset_kind: "other",
        price_asset_value: "XLM",
        coingecko_id: "stellar",
        // Testnet's native XLM SAC - deterministic per network, not a
        // deploy artifact (`stellar contract id asset --asset native
        // --network testnet`).
        token_contract_id: "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC",
    },
    AssetConfig {
        symbol: "USDC",
        price_asset_kind: "other",
        price_asset_value: "USDC",
        coingecko_id: "usd-coin",
        // Circle's testnet USDC issuer SAC - see PROJECT.md's deployment
        // table.
        token_contract_id: "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA",
    },
];

/// Looks up an `ASSETS` entry by its custodied token address - the
/// direction `vault::compute_allocation`'s entries (keyed by that address)
/// need to find a price symbol, since the contract itself only knows
/// `price_asset`, not this backend's `coingecko_id`/symbol pairing.
pub fn asset_by_token_contract_id(token_contract_id: &str) -> Option<&'static AssetConfig> {
    ASSETS.iter().find(|a| a.token_contract_id == token_contract_id)
}

/// One poll of every configured asset against both sources. Never
/// returns `Err` for a single source failing (a stale Reflector feed or a
/// CoinGecko hiccup are expected, loggable conditions, not scheduler
/// bugs) - only for something systemic like the DB connection itself
/// being down.
pub async fn observe_market_prices(
    pool: &PgPool,
    on_chain: &OnChainPriceReader,
    coingecko: &CoinGeckoClient,
    divergence_warn_bps: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let reader = on_chain.clone();
    let decimals = match tokio::task::spawn_blocking(move || reader.decimals()).await? {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "couldn't read oracle_adapter decimals, skipping this poll");
            return Ok(());
        }
    };

    for asset in ASSETS {
        let on_chain_price = observe_on_chain(pool, on_chain, asset).await;
        let coingecko_price = observe_coingecko(pool, coingecko, asset, decimals).await;

        if let (Some(on_chain_price), Some(coingecko_price)) = (on_chain_price, coingecko_price) {
            let bps = rebalancer_oracle::divergence_bps(on_chain_price, coingecko_price, decimals);
            if bps >= divergence_warn_bps {
                warn!(
                    symbol = asset.symbol,
                    divergence_bps = bps,
                    "reflector and coingecko prices have diverged"
                );
            } else {
                info!(symbol = asset.symbol, divergence_bps = bps, "price cross-check ok");
            }
        }
    }
    Ok(())
}

async fn observe_on_chain(
    pool: &PgPool,
    on_chain: &OnChainPriceReader,
    asset: &AssetConfig,
) -> Option<i128> {
    let reader = on_chain.clone();
    let kind = asset.price_asset_kind;
    let value = asset.price_asset_value.to_string();
    let price = match tokio::task::spawn_blocking(move || reader.get_price(kind, &value)).await {
        Ok(Ok(price)) => price,
        Ok(Err(e)) => {
            warn!(symbol = asset.symbol, error = %e, "on-chain price read failed");
            return None;
        }
        Err(e) => {
            error!(symbol = asset.symbol, error = %e, "on-chain price task panicked");
            return None;
        }
    };

    let price_decimal = BigDecimal::from_str(&price.to_string()).expect("i128 always parses");
    if let Err(e) = rebalancer_db::insert_price_snapshot(
        pool,
        asset.price_asset_kind,
        asset.price_asset_value,
        price_decimal,
        "reflector",
        None,
        Utc::now(),
    )
    .await
    {
        error!(symbol = asset.symbol, error = %e, "failed to record reflector price snapshot");
    }
    Some(price)
}

async fn observe_coingecko(
    pool: &PgPool,
    coingecko: &CoinGeckoClient,
    asset: &AssetConfig,
    decimals: u32,
) -> Option<f64> {
    let usd = match coingecko.price_usd(asset.coingecko_id).await {
        Ok(usd) => usd,
        Err(e) => {
            warn!(symbol = asset.symbol, error = %e, "coingecko price read failed");
            return None;
        }
    };

    // Scaled to the same fixed-point base as the on-chain price so the
    // two are directly comparable in price_snapshots without a
    // normalization step at read time.
    let scaled = (usd * 10f64.powi(decimals as i32)).round();
    let price_decimal = BigDecimal::from_str(&format!("{scaled:.0}"))
        .expect("a rounded f64 formatted with no decimal places always parses");
    if let Err(e) = rebalancer_db::insert_price_snapshot(
        pool,
        asset.price_asset_kind,
        asset.price_asset_value,
        price_decimal,
        "coingecko",
        None,
        Utc::now(),
    )
    .await
    {
        error!(symbol = asset.symbol, error = %e, "failed to record coingecko price snapshot");
    }
    Some(usd)
}
