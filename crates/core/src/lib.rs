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

#[cfg(test)]
mod test;
