//! Replays the threshold strategy against historical daily prices,
//! reusing `rebalancer_core::{compute_allocation, needs_rebalance}`
//! unchanged - the exact same pure functions the live scheduler and the
//! on-chain `vault` contract's own math mirror (see `rebalancer-core`'s
//! crate doc comment) - so a backtest result reflects what the real
//! decision logic would actually have done, not a separate
//! reimplementation that could quietly drift out of sync with it.
//!
//! What this *doesn't* model: trade fees, slippage, or execution price
//! impact (that's Phase 4's "fee-aware execution" differentiator) and
//! cost-basis/tax-lot tracking (a separate Phase 3 item, not built yet).
//! Every simulated rebalance here is frictionless - it moves the
//! portfolio to land exactly on target, the same simplification
//! `frontend`'s demo mode uses for the same reason: there's no router
//! wired into the real contract yet either (see the `contracts` repo),
//! so this isn't even a simplification relative to what production can
//! actually do today.

use chrono::NaiveDate;
use rebalancer_core::{compute_allocation, needs_rebalance, AllocationEntry, AssetState, TargetWeight};
use std::collections::BTreeMap;

/// One asset's daily USD-scaled price history, sorted ascending by date.
/// "Scaled" mirrors `rebalancer_oracle::divergence_bps`'s convention:
/// a plain USD float multiplied by `10^decimals` and rounded to an
/// `i128`, so a backtest built from real CoinGecko data uses the same
/// fixed-point representation `vault`/`oracle_adapter` do on-chain.
pub struct AssetPriceSeries {
    pub symbol: String,
    pub prices: Vec<(NaiveDate, i128)>,
}

#[derive(Debug)]
pub enum BacktestError {
    /// No dates were present in every configured asset's price series -
    /// nothing to replay.
    NoAlignedPriceData,
    /// A target asset's symbol never appeared in any price series at
    /// all - almost certainly a config mistake (wrong symbol string),
    /// not a data gap.
    UnknownAsset(String),
}

impl std::fmt::Display for BacktestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAlignedPriceData => {
                write!(f, "no date has price data for every target asset")
            }
            Self::UnknownAsset(symbol) => write!(f, "no price series at all for asset {symbol}"),
        }
    }
}

impl std::error::Error for BacktestError {}

#[derive(Debug, Clone)]
pub struct RebalanceRecord {
    pub date: NaiveDate,
    /// Allocation as it stood immediately before this rebalance -
    /// what actually triggered it.
    pub allocation_before: Vec<AllocationEntry<String>>,
}

#[derive(Debug)]
pub struct BacktestReport {
    /// (date, total portfolio value) for every replayed day, in the same
    /// scaled units as the input prices - not literal USD unless the
    /// caller's `initial_value` and price series both used `10^decimals`
    /// scaling consistently (see `AssetPriceSeries`'s doc comment).
    pub equity_curve: Vec<(NaiveDate, i128)>,
    pub rebalances: Vec<RebalanceRecord>,
    pub final_value: i128,
    pub total_return_bps: i32,
}

/// Intersects every series down to only the dates present in *all* of
/// them - a backtest needs a price for every target asset on a given day
/// to compute that day's allocation at all, so a day missing from even
/// one series can't be replayed.
pub fn align_daily(series: &[AssetPriceSeries]) -> Vec<(NaiveDate, Vec<(String, i128)>)> {
    let maps: Vec<(&str, BTreeMap<NaiveDate, i128>)> = series
        .iter()
        .map(|s| (s.symbol.as_str(), s.prices.iter().copied().collect()))
        .collect();

    let Some((_, first_map)) = maps.first() else {
        return Vec::new();
    };
    let mut dates: Vec<NaiveDate> = first_map.keys().copied().collect();
    dates.retain(|date| maps.iter().all(|(_, m)| m.contains_key(date)));
    dates.sort();

    dates
        .into_iter()
        .map(|date| {
            let prices = maps
                .iter()
                .map(|(symbol, m)| (symbol.to_string(), *m.get(&date).unwrap()))
                .collect();
            (date, prices)
        })
        .collect()
}

/// Replays the threshold strategy day by day: seeds initial balances
/// from `initial_value` split across `targets` at the first aligned
/// day's prices, then for each subsequent day recomputes drift and, if
/// `needs_rebalance` says so, resets balances to land exactly on target
/// at that day's prices (see the crate doc comment on why this is
/// frictionless).
pub fn run_backtest(
    targets: &[TargetWeight<String>],
    threshold_bps: u32,
    initial_value: i128,
    daily_prices: &[(NaiveDate, Vec<(String, i128)>)],
) -> Result<BacktestReport, BacktestError> {
    let Some((_, first_prices)) = daily_prices.first() else {
        return Err(BacktestError::NoAlignedPriceData);
    };

    let mut balances: BTreeMap<String, i128> = BTreeMap::new();
    for target in targets {
        let price = price_for(first_prices, &target.asset)?;
        let target_value = initial_value.saturating_mul(target.weight_bps as i128) / rebalancer_core::BPS_DENOM;
        balances.insert(
            target.asset.clone(),
            if price > 0 { target_value / price } else { 0 },
        );
    }

    let mut equity_curve = Vec::with_capacity(daily_prices.len());
    let mut rebalances = Vec::new();

    for (date, prices) in daily_prices {
        let mut states = Vec::with_capacity(targets.len());
        for target in targets {
            let price = price_for(prices, &target.asset)?;
            states.push(AssetState {
                asset: target.asset.clone(),
                balance: *balances.get(&target.asset).unwrap_or(&0),
                price,
            });
        }

        let allocation = compute_allocation(targets, &states);
        let total_value: i128 = states
            .iter()
            .map(|s| s.balance.saturating_mul(s.price))
            .fold(0i128, |acc, v| acc.saturating_add(v));
        equity_curve.push((*date, total_value));

        if needs_rebalance(&allocation, threshold_bps) {
            rebalances.push(RebalanceRecord {
                date: *date,
                allocation_before: allocation,
            });
            for target in targets {
                let price = price_for(prices, &target.asset)?;
                let target_value =
                    total_value.saturating_mul(target.weight_bps as i128) / rebalancer_core::BPS_DENOM;
                balances.insert(
                    target.asset.clone(),
                    if price > 0 { target_value / price } else { 0 },
                );
            }
        }
    }

    let final_value = equity_curve.last().map(|(_, v)| *v).unwrap_or(initial_value);
    let total_return_bps = if initial_value > 0 {
        (final_value
            .saturating_sub(initial_value)
            .saturating_mul(rebalancer_core::BPS_DENOM)
            / initial_value) as i32
    } else {
        0
    };

    Ok(BacktestReport {
        equity_curve,
        rebalances,
        final_value,
        total_return_bps,
    })
}

fn price_for(prices: &[(String, i128)], asset: &str) -> Result<i128, BacktestError> {
    prices
        .iter()
        .find(|(symbol, _)| symbol == asset)
        .map(|(_, price)| *price)
        .ok_or_else(|| BacktestError::UnknownAsset(asset.to_string()))
}

#[cfg(test)]
mod test;
