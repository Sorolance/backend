//! Pure drift-calculation logic, mirroring the on-chain `vault` contract's
//! math exactly (see `contracts/contracts/vault/src/lib.rs` in the sibling
//! `contracts` repo) so the scheduler and backtester reason about drift
//! identically to what the contract actually enforces on-chain. No I/O -
//! callers supply balances/prices already fetched from wherever (RPC,
//! indexer, historical data for a backtest).

/// Basis-point denominator: weights and drift are expressed out of 10_000.
pub const BPS_DENOM: i128 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetWeight<Asset> {
    pub asset: Asset,
    pub weight_bps: u32,
}

/// A snapshot of one asset's on-chain balance and current price, in
/// whatever consistent decimals the caller uses across all assets (see
/// the note on `compute_allocation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetState<Asset> {
    pub asset: Asset,
    pub balance: i128,
    pub price: i128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationEntry<Asset> {
    pub asset: Asset,
    pub target_weight_bps: u32,
    pub current_weight_bps: u32,
    pub drift_bps: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    Empty,
    ZeroWeight,
    DuplicateAsset,
    WeightsDontSumTo10000,
    InvalidThreshold,
}

/// Weights must be non-zero, sum to `BPS_DENOM`, and reference no asset
/// more than once - matches `vault::validate_targets` on-chain.
pub fn validate_targets<Asset: PartialEq>(
    targets: &[TargetWeight<Asset>],
) -> Result<(), ValidationError> {
    if targets.is_empty() {
        return Err(ValidationError::Empty);
    }
    let mut sum: i128 = 0;
    for (i, t) in targets.iter().enumerate() {
        if t.weight_bps == 0 {
            return Err(ValidationError::ZeroWeight);
        }
        sum += t.weight_bps as i128;
        if targets[i + 1..].iter().any(|other| other.asset == t.asset) {
            return Err(ValidationError::DuplicateAsset);
        }
    }
    if sum != BPS_DENOM {
        return Err(ValidationError::WeightsDontSumTo10000);
    }
    Ok(())
}

/// Matches `vault::validate_threshold` on-chain: must be strictly between
/// 0 and `BPS_DENOM`.
pub fn validate_threshold(threshold_bps: u32) -> Result<(), ValidationError> {
    if threshold_bps == 0 || threshold_bps as i128 >= BPS_DENOM {
        return Err(ValidationError::InvalidThreshold);
    }
    Ok(())
}

/// Computes each target asset's current vs. target weight and drift, given
/// live balances and prices. `value = balance * price` for each asset -
/// this is only a valid proportion (not an absolute USD value) unless
/// every asset shares the same token decimals and is priced by the same
/// oracle feed decimals, exactly as documented on the `vault` contract.
/// An asset in `targets` with no matching entry in `states` (or an
/// all-zero-value portfolio) is treated as a zero balance rather than an
/// error - callers that need "price unavailable" to be a hard failure
/// should check for that before calling this.
pub fn compute_allocation<Asset: Clone + PartialEq>(
    targets: &[TargetWeight<Asset>],
    states: &[AssetState<Asset>],
) -> Vec<AllocationEntry<Asset>> {
    let values: Vec<i128> = targets
        .iter()
        .map(|t| {
            states
                .iter()
                .find(|s| s.asset == t.asset)
                .map(|s| s.balance.saturating_mul(s.price))
                .unwrap_or(0)
        })
        .collect();
    let total_value: i128 = values.iter().fold(0i128, |acc, v| acc.saturating_add(*v));

    targets
        .iter()
        .zip(values)
        .map(|(t, value)| {
            let current_weight_bps: u32 = if total_value > 0 {
                (value.saturating_mul(BPS_DENOM) / total_value) as u32
            } else {
                0
            };
            let drift_bps = current_weight_bps as i32 - t.weight_bps as i32;
            AllocationEntry {
                asset: t.asset.clone(),
                target_weight_bps: t.weight_bps,
                current_weight_bps,
                drift_bps,
            }
        })
        .collect()
}

/// True if any asset's live drift meets or exceeds `threshold_bps` -
/// matches `vault::needs_rebalance` on-chain.
pub fn needs_rebalance<Asset>(allocation: &[AllocationEntry<Asset>], threshold_bps: u32) -> bool {
    allocation
        .iter()
        .any(|e| e.drift_bps.unsigned_abs() >= threshold_bps)
}

/// Same decision as `needs_rebalance`, but with a per-asset threshold
/// instead of one global value - what `Strategy::VolatilityBand` needs,
/// since each asset's effective threshold depends on its own realized
/// volatility (see `volatility_adjusted_threshold_bps`).
pub fn needs_rebalance_per_asset<Asset>(
    allocation: &[AllocationEntry<Asset>],
    threshold_bps: impl Fn(&Asset) -> u32,
) -> bool {
    allocation
        .iter()
        .any(|e| e.drift_bps.unsigned_abs() >= threshold_bps(&e.asset))
}

/// Which rule decides *when* a rebalance fires - the three templates from
/// PROJECT.md section 3. `Threshold` is what `vault::needs_rebalance`
/// already enforces on-chain today; `Calendar` and `VolatilityBand` exist
/// here (and in `rebalancer-backtest`) before anything on-chain
/// represents them - see `contracts/contracts/strategy_registry`'s
/// "not yet built" status in PROJECT.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// Rebalance whenever any asset's drift meets or exceeds
    /// `threshold_bps`.
    Threshold { threshold_bps: u32 },
    /// Rebalance every `interval_days`, regardless of drift.
    Calendar { interval_days: u32 },
    /// Threshold-based, but each asset's effective threshold is derived
    /// from that asset's own realized volatility instead of staying
    /// fixed - see `volatility_adjusted_threshold_bps`.
    VolatilityBand {
        /// Effective threshold = realized volatility (bps) * this,
        /// scaled by `BPS_DENOM` (so `BPS_DENOM` itself means "threshold
        /// tracks volatility 1:1").
        vol_multiplier_bps: u32,
        min_threshold_bps: u32,
        max_threshold_bps: u32,
    },
}

/// True once at least `interval_days` have elapsed since the last
/// rebalance - the calendar strategy's whole decision rule. Takes a
/// plain day count rather than a specific date type so callers (chrono's
/// `NaiveDate` in the backtester, a Soroban ledger timestamp on-chain)
/// don't need to share a date library.
pub fn calendar_due(days_since_last_rebalance: u32, interval_days: u32) -> bool {
    interval_days > 0 && days_since_last_rebalance >= interval_days
}

/// Realized volatility of a price series, as the standard deviation of
/// simple period-over-period returns, expressed in bps (so directly
/// comparable to a drift value). Needs at least 2 prices to produce a
/// single return, and at least 2 *returns* (3 prices) to produce a
/// non-zero standard deviation - fewer than that returns 0, treating
/// "not enough history yet" the same as "observed to be perfectly calm"
/// rather than as an error, since a strategy built on this should keep
/// running through a short history, not fail closed on day one.
/// Zero-or-negative prices in the series are skipped (division by zero,
/// and a real oracle price is never non-positive) rather than treated as
/// a hard error, consistent with `compute_allocation`'s own leniency.
pub fn realized_volatility_bps(prices: &[i128]) -> u32 {
    let returns: Vec<f64> = prices
        .windows(2)
        .filter(|w| w[0] > 0)
        .map(|w| (w[1] - w[0]) as f64 / w[0] as f64)
        .collect();
    if returns.len() < 2 {
        return 0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance =
        returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    let bps = (variance.sqrt() * BPS_DENOM as f64).round();
    if bps <= 0.0 {
        0
    } else if bps >= u32::MAX as f64 {
        u32::MAX
    } else {
        bps as u32
    }
}

/// Scales `realized_volatility_bps` by `vol_multiplier_bps` to get an
/// effective drift threshold, clamped to `[min_threshold_bps,
/// max_threshold_bps]` so a perfectly calm asset can't collapse to a
/// threshold of 0 (any nonzero drift would perpetually "trigger") and a
/// wildly volatile one can't push the threshold to an unusable extreme.
pub fn volatility_adjusted_threshold_bps(
    realized_volatility_bps: u32,
    vol_multiplier_bps: u32,
    min_threshold_bps: u32,
    max_threshold_bps: u32,
) -> u32 {
    let scaled = (realized_volatility_bps as u64).saturating_mul(vol_multiplier_bps as u64)
        / BPS_DENOM as u64;
    (scaled as u32).clamp(min_threshold_bps, max_threshold_bps)
}

/// One trade needed to move `asset_in` value into `asset_out`, in
/// `asset_in`'s own native units - what `compute_rebalance_trades` emits
/// and the shape a caller (the scheduler) turns into a `vault::rebalance`
/// `TradeInstruction` once it's added a `min_amount_out` from a real
/// router quote (this crate stays oracle/router-agnostic, so it has no
/// opinion on slippage tolerance - see `slippage_bps`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeIntent<Asset> {
    pub asset_in: Asset,
    pub asset_out: Asset,
    pub amount_in: i128,
}

/// Computes the trades needed to bring every asset back to target, given
/// live balances/prices - the multi-asset generalization of "sell the
/// overweight asset, buy the underweight one" that `vault::rebalance`
/// itself has no opinion on (it just executes whatever `TradeInstruction`s
/// it's handed - see the `contracts` repo). Overweight assets (`value >
/// target_value`) are sources, underweight ones are sinks; each source's
/// excess value is allocated across sinks in the order both appear in
/// `targets`, greedily filling one sink before moving to the next, until
/// every source and sink is exhausted. An asset can only ever be a source,
/// a sink, or exactly on target - never both - so this never emits a
/// self-trade. `amount_in` is `matched_value / price`, floor division: the
/// vault only ever needs to move at most its actual excess, so
/// undershooting the ideal split by a rounding remainder is harmless;
/// overshooting isn't possible this way. A source whose price is 0
/// (unpriced/misconfigured) is skipped entirely rather than dividing by
/// it, consistent with `compute_allocation`'s own leniency toward missing
/// price/balance data.
pub fn compute_rebalance_trades<Asset: Clone + PartialEq>(
    targets: &[TargetWeight<Asset>],
    states: &[AssetState<Asset>],
) -> Vec<TradeIntent<Asset>> {
    struct Entry<A> {
        asset: A,
        price: i128,
        excess_value: i128,
    }

    let values_and_prices: Vec<(Asset, i128, i128)> = targets
        .iter()
        .map(|t| {
            let (balance, price) = states
                .iter()
                .find(|s| s.asset == t.asset)
                .map(|s| (s.balance, s.price))
                .unwrap_or((0, 0));
            (t.asset.clone(), balance.saturating_mul(price), price)
        })
        .collect();
    let total_value: i128 = values_and_prices
        .iter()
        .fold(0i128, |acc, (_, v, _)| acc.saturating_add(*v));

    let mut entries: Vec<Entry<Asset>> = targets
        .iter()
        .zip(values_and_prices)
        .map(|(t, (asset, value, price))| {
            let target_value = total_value.saturating_mul(t.weight_bps as i128) / BPS_DENOM;
            Entry {
                asset,
                price,
                excess_value: value - target_value,
            }
        })
        .collect();

    let mut trades = Vec::new();
    let mut source_idx = 0;
    let mut sink_idx = 0;
    while source_idx < entries.len() && sink_idx < entries.len() {
        if entries[source_idx].excess_value <= 0 || entries[source_idx].price <= 0 {
            source_idx += 1;
            continue;
        }
        if entries[sink_idx].excess_value >= 0 {
            sink_idx += 1;
            continue;
        }
        let deficit = -entries[sink_idx].excess_value;
        let matched_value = entries[source_idx].excess_value.min(deficit);
        let amount_in = matched_value / entries[source_idx].price;
        if amount_in > 0 {
            trades.push(TradeIntent {
                asset_in: entries[source_idx].asset.clone(),
                asset_out: entries[sink_idx].asset.clone(),
                amount_in,
            });
        }
        entries[source_idx].excess_value -= matched_value;
        entries[sink_idx].excess_value += matched_value;
    }
    trades
}

/// bps by which a router's quoted `amount_out` for `amount_in` falls short
/// of the oracle-implied "fair" amount out at the same instant - the
/// AMM's real price impact (plus its own swap fee, which is an equally
/// real cost of executing the trade) for this trade size against current
/// liquidity. `price_in`/`price_out` must share `AssetState::price`'s
/// convention. Returns 0 rather than a negative bps if the quote is at or
/// above fair value (possible with rounding, or a pool briefly favoring
/// the trader) - "no measurable slippage" is the right floor, not a
/// negative cost.
pub fn slippage_bps(amount_in: i128, price_in: i128, price_out: i128, quoted_amount_out: i128) -> u32 {
    if amount_in <= 0 || price_in <= 0 || price_out <= 0 {
        return 0;
    }
    let fair_amount_out = amount_in.saturating_mul(price_in) / price_out;
    if fair_amount_out <= 0 || quoted_amount_out >= fair_amount_out {
        return 0;
    }
    let shortfall = fair_amount_out - quoted_amount_out;
    let bps = shortfall.saturating_mul(BPS_DENOM) / fair_amount_out;
    bps.clamp(0, u32::MAX as i128) as u32
}

/// Total expected cost of executing a rebalance, in bps of `trade_value` -
/// the network fee (already converted by the caller into the same value
/// units as `trade_value`, e.g. `fee_in_native_units * xlm_price`) plus
/// the worst per-trade `slippage_bps` observed across the batch. `<= 0`
/// trade value can't amortize any cost over it (nothing to divide by, and
/// it shouldn't happen for a real rebalance) - treated as maximally
/// expensive rather than dividing by zero, so a caller's cost gate always
/// rejects it rather than silently passing.
pub fn total_cost_bps(trade_value: i128, network_fee_value: i128, worst_slippage_bps: u32) -> u32 {
    if trade_value <= 0 {
        return u32::MAX;
    }
    let fee_bps = (network_fee_value.saturating_mul(BPS_DENOM) / trade_value).clamp(0, u32::MAX as i128) as u32;
    fee_bps.saturating_add(worst_slippage_bps)
}

/// Whether a rebalance that's already past its drift threshold should
/// actually execute now, or be deferred to the next tick because it's not
/// urgent and costs too much relative to its own value right now (fees
/// spiking, or a router quote showing heavy slippage) - PROJECT.md's
/// "fee-aware execution" item. `urgent_drift_multiplier` makes urgency
/// override cost entirely: once drift reaches `threshold_bps *
/// urgent_drift_multiplier`, the portfolio is far enough off target that
/// waiting for cheaper conditions risks drifting further, so it executes
/// regardless of `total_cost_bps`. A multiplier of 0 disables the override
/// (cost always gates execution); note the deferred trade isn't lost - the
/// same drift (or more) will still be there next tick, still gated the
/// same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeAwareDecision {
    pub execute: bool,
    pub urgent: bool,
    pub total_cost_bps: u32,
}

pub fn evaluate_fee_aware_execution(
    max_drift_bps: u32,
    threshold_bps: u32,
    urgent_drift_multiplier: u32,
    total_cost_bps: u32,
    max_cost_bps: u32,
) -> FeeAwareDecision {
    let urgent = urgent_drift_multiplier > 0
        && max_drift_bps as u64 >= threshold_bps as u64 * urgent_drift_multiplier as u64;
    FeeAwareDecision {
        execute: urgent || total_cost_bps <= max_cost_bps,
        urgent,
        total_cost_bps,
    }
}

/// One cost-basis lot: `qty` of an asset acquired at `price` (same
/// fixed-point convention as `AssetState::price` elsewhere in this
/// crate). Ordering across a `Vec<Lot>` matters - `dispose_fifo` always
/// consumes from the front, so callers must keep lots sorted
/// oldest-acquired-first themselves; this type carries no timestamp of
/// its own, mirroring `calendar_due` staying date-library-free for the
/// same reason (see `crates/db`'s `lots` table migration, which does
/// keep `acquired_at` for exactly this ordering, on the caller's side of
/// that split).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lot {
    pub qty: i128,
    pub price: i128,
}

/// Realized gain/loss from one `dispose_fifo` call, in the same scaled
/// units as `Lot::price` times `Lot::qty` (not necessarily USD - see
/// `AssetPriceSeries`'s doc comment in `rebalancer-backtest` for the
/// scaling convention a caller sourcing real prices should follow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RealizedGainLoss {
    pub qty_disposed: i128,
    pub proceeds: i128,
    pub cost_basis: i128,
    pub gain_loss: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LotError {
    /// Asked to dispose more than the supplied lots actually cover - a
    /// caller bug (tracking a sale larger than the tracked balance), not
    /// a data condition worth silently clamping around.
    InsufficientLots,
}

/// Matches `qty_to_dispose` against `lots` oldest-first (FIFO - the
/// convention `crates/db`'s `lots` table migration already documents),
/// returning the realized gain/loss at `disposal_price` and the lots
/// left afterward (the consumed lot removed or reduced). Never mutates
/// `lots` in place, so a caller unsure whether to commit a tentative
/// disposal can just discard the result.
pub fn dispose_fifo(
    lots: &[Lot],
    qty_to_dispose: i128,
    disposal_price: i128,
) -> Result<(RealizedGainLoss, Vec<Lot>), LotError> {
    let mut remaining_to_dispose = qty_to_dispose;
    let mut cost_basis: i128 = 0;
    let mut remaining_lots = Vec::new();

    for lot in lots {
        if remaining_to_dispose <= 0 {
            remaining_lots.push(*lot);
            continue;
        }
        if lot.qty <= remaining_to_dispose {
            cost_basis = cost_basis.saturating_add(lot.qty.saturating_mul(lot.price));
            remaining_to_dispose -= lot.qty;
        } else {
            cost_basis =
                cost_basis.saturating_add(remaining_to_dispose.saturating_mul(lot.price));
            remaining_lots.push(Lot {
                qty: lot.qty - remaining_to_dispose,
                price: lot.price,
            });
            remaining_to_dispose = 0;
        }
    }

    if remaining_to_dispose > 0 {
        return Err(LotError::InsufficientLots);
    }

    let proceeds = qty_to_dispose.saturating_mul(disposal_price);
    Ok((
        RealizedGainLoss {
            qty_disposed: qty_to_dispose,
            proceeds,
            cost_basis,
            gain_loss: proceeds.saturating_sub(cost_basis),
        },
        remaining_lots,
    ))
}

#[cfg(test)]
mod test;
