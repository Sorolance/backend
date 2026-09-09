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

#[cfg(test)]
mod test;
