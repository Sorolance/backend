//! Replays any of `rebalancer_core::Strategy`'s templates against
//! historical daily prices, reusing `rebalancer_core::compute_allocation`
//! and each strategy's own decision primitives unchanged - the exact
//! same pure functions the live scheduler and the on-chain `vault`
//! contract's own math mirror for the threshold case (see
//! `rebalancer-core`'s crate doc comment) - so a backtest result
//! reflects what the real decision logic would actually have done, not a
//! separate reimplementation that could quietly drift out of sync with
//! it. Calendar and volatility-band strategies have no on-chain
//! counterpart yet (see `contracts/contracts/strategy_registry`'s
//! "not yet built" status in PROJECT.md), so for those two this crate is
//! currently the only place the decision logic runs at all.
//!
//! Every rebalance's realized gain/loss is tracked too, via
//! `rebalancer_core::dispose_fifo` (PROJECT.md differentiator #3) -
//! every net-bought asset opens a new lot at that day's price, every
//! net-sold asset disposes FIFO against its existing lots.
//!
//! What this *doesn't* model: trade fees, slippage, or execution price
//! impact - that's Phase 4's "fee-aware execution" differentiator.
//! Every simulated rebalance here is frictionless - it moves the
//! portfolio to land exactly on target, the same simplification
//! `frontend`'s demo mode uses for the same reason: there's no router
//! wired into the real contract yet either (see the `contracts` repo),
//! so this isn't even a simplification relative to what production can
//! actually do today.

use chrono::NaiveDate;
use rebalancer_core::{
    calendar_due, compute_allocation, dispose_fifo, needs_rebalance, needs_rebalance_per_asset,
    realized_volatility_bps, volatility_adjusted_threshold_bps, AllocationEntry, AssetState, Lot,
    Strategy, TargetWeight,
};
use std::collections::BTreeMap;

/// Trailing window (in aligned trading days) used to estimate an asset's
/// realized volatility for `Strategy::VolatilityBand`. 14 days balances
/// reacting to a real regime change against not just chasing single-day
/// noise; not something this project has tuned against real performance
/// data yet.
const VOLATILITY_LOOKBACK_DAYS: usize = 14;

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
    /// A rebalance tried to dispose more of an asset than its tracked
    /// lots cover. Should never actually happen - every disposal here is
    /// bounded by the balance this same code just tracked buying - so
    /// this indicates an internal bookkeeping bug in this crate, not a
    /// bad input; surfaced as a typed error rather than a panic anyway.
    LotAccountingInconsistency(String),
}

impl std::fmt::Display for BacktestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAlignedPriceData => {
                write!(f, "no date has price data for every target asset")
            }
            Self::UnknownAsset(symbol) => write!(f, "no price series at all for asset {symbol}"),
            Self::LotAccountingInconsistency(asset) => write!(
                f,
                "tried to dispose more {asset} than its tracked lots cover - internal bug"
            ),
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
    /// Sum of realized gain/loss (see `rebalancer_core::dispose_fifo`)
    /// across every asset net-sold in this rebalance, in the same scaled
    /// units as the input prices. 0 is a real possible value (a disposal
    /// at exactly its cost basis), not "nothing happened" - see
    /// `rebalances` for whether this record exists at all.
    pub realized_gain_loss: i128,
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
    /// Sum of every rebalance's `realized_gain_loss` - unrealized
    /// gain/loss on whatever's still held at `final_value` is not
    /// included, since PROJECT.md's differentiator scopes this to
    /// gain/loss "computed per rebalance event".
    pub total_realized_gain_loss: i128,
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

/// Replays `strategy` day by day: seeds initial balances from
/// `initial_value` split across `targets` at the first aligned day's
/// prices, then for each subsequent day recomputes drift and, if the
/// strategy's own rule says a rebalance is due, resets balances to land
/// exactly on target at that day's prices (see the crate doc comment on
/// why this is frictionless).
pub fn run_backtest(
    strategy: &Strategy,
    targets: &[TargetWeight<String>],
    initial_value: i128,
    daily_prices: &[(NaiveDate, Vec<(String, i128)>)],
) -> Result<BacktestReport, BacktestError> {
    let Some((first_date, first_prices)) = daily_prices.first() else {
        return Err(BacktestError::NoAlignedPriceData);
    };
    let first_date = *first_date;

    let mut balances = balances_at_target(targets, first_prices, initial_value)?;
    let mut lots: BTreeMap<String, Vec<Lot>> = targets
        .iter()
        .map(|t| {
            let qty = *balances.get(&t.asset).unwrap_or(&0);
            let price = price_for(first_prices, &t.asset).unwrap_or(0);
            let opening_lots = if qty > 0 { vec![Lot { qty, price }] } else { Vec::new() };
            (t.asset.clone(), opening_lots)
        })
        .collect();

    let mut equity_curve = Vec::with_capacity(daily_prices.len());
    let mut rebalances = Vec::new();
    let mut total_realized_gain_loss: i128 = 0;
    let mut last_rebalance_date = first_date;

    for (index, (date, prices)) in daily_prices.iter().enumerate() {
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

        let due = match strategy {
            Strategy::Threshold { threshold_bps } => needs_rebalance(&allocation, *threshold_bps),
            Strategy::Calendar { interval_days } => {
                let days_elapsed = (*date - last_rebalance_date).num_days().max(0) as u32;
                calendar_due(days_elapsed, *interval_days)
            }
            Strategy::VolatilityBand {
                vol_multiplier_bps,
                min_threshold_bps,
                max_threshold_bps,
            } => needs_rebalance_per_asset(&allocation, |asset| {
                let vol = realized_volatility_for_asset(daily_prices, index, asset);
                volatility_adjusted_threshold_bps(
                    vol,
                    *vol_multiplier_bps,
                    *min_threshold_bps,
                    *max_threshold_bps,
                )
            }),
        };

        if due {
            let new_balances = balances_at_target(targets, prices, total_value)?;
            let mut rebalance_gain_loss: i128 = 0;
            for target in targets {
                let asset = &target.asset;
                let price = price_for(prices, asset)?;
                let old_qty = *balances.get(asset).unwrap_or(&0);
                let new_qty = *new_balances.get(asset).unwrap_or(&0);
                let asset_lots = lots.entry(asset.clone()).or_default();
                match new_qty.cmp(&old_qty) {
                    std::cmp::Ordering::Greater => {
                        asset_lots.push(Lot {
                            qty: new_qty - old_qty,
                            price,
                        });
                    }
                    std::cmp::Ordering::Less => {
                        let (realized, remaining) = dispose_fifo(asset_lots, old_qty - new_qty, price)
                            .map_err(|_| BacktestError::LotAccountingInconsistency(asset.clone()))?;
                        *asset_lots = remaining;
                        rebalance_gain_loss = rebalance_gain_loss.saturating_add(realized.gain_loss);
                    }
                    std::cmp::Ordering::Equal => {}
                }
            }
            total_realized_gain_loss = total_realized_gain_loss.saturating_add(rebalance_gain_loss);
            rebalances.push(RebalanceRecord {
                date: *date,
                allocation_before: allocation,
                realized_gain_loss: rebalance_gain_loss,
            });
            balances = new_balances;
            last_rebalance_date = *date;
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
        total_realized_gain_loss,
    })
}

/// Splits `total_value` across `targets` at `prices`, in whole asset
/// units - used both to seed the initial portfolio and to reset balances
/// on every rebalance, so the two can never drift apart in behavior.
fn balances_at_target(
    targets: &[TargetWeight<String>],
    prices: &[(String, i128)],
    total_value: i128,
) -> Result<BTreeMap<String, i128>, BacktestError> {
    let mut balances = BTreeMap::new();
    for target in targets {
        let price = price_for(prices, &target.asset)?;
        let target_value =
            total_value.saturating_mul(target.weight_bps as i128) / rebalancer_core::BPS_DENOM;
        balances.insert(
            target.asset.clone(),
            if price > 0 { target_value / price } else { 0 },
        );
    }
    Ok(balances)
}

fn price_for(prices: &[(String, i128)], asset: &str) -> Result<i128, BacktestError> {
    prices
        .iter()
        .find(|(symbol, _)| symbol == asset)
        .map(|(_, price)| *price)
        .ok_or_else(|| BacktestError::UnknownAsset(asset.to_string()))
}

/// Realized volatility of `asset` over the trailing `VOLATILITY_LOOKBACK_DAYS`
/// aligned trading days up to and including `daily_prices[up_to_index]` -
/// deliberately inclusive of "today" (not just prior days) since the
/// strategy is deciding using today's own close, the same price it just
/// computed drift from.
fn realized_volatility_for_asset(
    daily_prices: &[(NaiveDate, Vec<(String, i128)>)],
    up_to_index: usize,
    asset: &str,
) -> u32 {
    let start = up_to_index.saturating_sub(VOLATILITY_LOOKBACK_DAYS.saturating_sub(1));
    let prices: Vec<i128> = daily_prices[start..=up_to_index]
        .iter()
        .filter_map(|(_, prices)| {
            prices
                .iter()
                .find(|(symbol, _)| symbol == asset)
                .map(|(_, price)| *price)
        })
        .collect();
    realized_volatility_bps(&prices)
}

#[cfg(test)]
mod test;
