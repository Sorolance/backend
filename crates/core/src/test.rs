#![cfg(test)]

use super::*;

fn targets() -> Vec<TargetWeight<&'static str>> {
    vec![
        TargetWeight {
            asset: "XLM",
            weight_bps: 6_000,
        },
        TargetWeight {
            asset: "USDC",
            weight_bps: 4_000,
        },
    ]
}

#[test]
fn validate_targets_rejects_empty() {
    let empty: Vec<TargetWeight<&str>> = vec![];
    assert_eq!(validate_targets(&empty), Err(ValidationError::Empty));
}

#[test]
fn validate_targets_rejects_zero_weight() {
    let t = vec![TargetWeight {
        asset: "XLM",
        weight_bps: 0,
    }];
    assert_eq!(validate_targets(&t), Err(ValidationError::ZeroWeight));
}

#[test]
fn validate_targets_rejects_duplicate_asset() {
    let t = vec![
        TargetWeight {
            asset: "XLM",
            weight_bps: 5_000,
        },
        TargetWeight {
            asset: "XLM",
            weight_bps: 5_000,
        },
    ];
    assert_eq!(validate_targets(&t), Err(ValidationError::DuplicateAsset));
}

#[test]
fn validate_targets_rejects_weights_not_summing_to_10000() {
    let t = vec![TargetWeight {
        asset: "XLM",
        weight_bps: 9_000,
    }];
    assert_eq!(
        validate_targets(&t),
        Err(ValidationError::WeightsDontSumTo10000)
    );
}

#[test]
fn validate_targets_accepts_valid_set() {
    assert_eq!(validate_targets(&targets()), Ok(()));
}

#[test]
fn validate_threshold_rejects_zero_and_10000_plus() {
    assert_eq!(
        validate_threshold(0),
        Err(ValidationError::InvalidThreshold)
    );
    assert_eq!(
        validate_threshold(10_000),
        Err(ValidationError::InvalidThreshold)
    );
    assert_eq!(validate_threshold(500), Ok(()));
}

#[test]
fn compute_allocation_on_target_has_zero_drift() {
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 600,
            price: 1,
        },
        AssetState {
            asset: "USDC",
            balance: 400,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    assert_eq!(allocation[0].current_weight_bps, 6_000);
    assert_eq!(allocation[0].drift_bps, 0);
    assert_eq!(allocation[1].current_weight_bps, 4_000);
    assert_eq!(allocation[1].drift_bps, 0);
}

#[test]
fn compute_allocation_reflects_price_moves_not_just_balances() {
    // Equal balances, but XLM priced 3x USDC - value-weighted, not
    // balance-weighted.
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 100,
            price: 3,
        },
        AssetState {
            asset: "USDC",
            balance: 100,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    // total value = 300 + 100 = 400; XLM = 300/400 = 7500bps
    assert_eq!(allocation[0].current_weight_bps, 7_500);
    assert_eq!(allocation[0].drift_bps, 1_500);
    assert_eq!(allocation[1].current_weight_bps, 2_500);
    assert_eq!(allocation[1].drift_bps, -1_500);
}

#[test]
fn compute_allocation_empty_portfolio_is_all_zero_not_div_by_zero() {
    let states: Vec<AssetState<&str>> = vec![];
    let allocation = compute_allocation(&targets(), &states);
    assert_eq!(allocation[0].current_weight_bps, 0);
    assert_eq!(allocation[0].drift_bps, -6_000);
    assert_eq!(allocation[1].current_weight_bps, 0);
    assert_eq!(allocation[1].drift_bps, -4_000);
}

#[test]
fn compute_allocation_missing_asset_state_treated_as_zero_balance() {
    let states = vec![AssetState {
        asset: "XLM",
        balance: 1_000,
        price: 1,
    }];
    let allocation = compute_allocation(&targets(), &states);
    assert_eq!(allocation[0].current_weight_bps, 10_000);
    assert_eq!(allocation[1].current_weight_bps, 0);
}

#[test]
fn needs_rebalance_false_when_on_target() {
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 600,
            price: 1,
        },
        AssetState {
            asset: "USDC",
            balance: 400,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    assert!(!needs_rebalance(&allocation, 500));
}

#[test]
fn needs_rebalance_true_when_drift_meets_threshold() {
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 1_000,
            price: 1,
        },
        AssetState {
            asset: "USDC",
            balance: 0,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    // drift is 4000bps, well past a 500bps threshold
    assert!(needs_rebalance(&allocation, 500));
}

#[test]
fn needs_rebalance_boundary_is_inclusive() {
    // Exactly at threshold should trigger (>=, matching the contract).
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 6_500,
            price: 1,
        },
        AssetState {
            asset: "USDC",
            balance: 3_500,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    assert_eq!(allocation[0].drift_bps, 500);
    assert!(needs_rebalance(&allocation, 500));
}

#[test]
fn needs_rebalance_per_asset_uses_each_assets_own_threshold() {
    let states = vec![
        AssetState {
            asset: "XLM",
            balance: 6_500,
            price: 1,
        },
        AssetState {
            asset: "USDC",
            balance: 3_500,
            price: 1,
        },
    ];
    let allocation = compute_allocation(&targets(), &states);
    // Both legs drift +/-500bps. A threshold fn that only lets USDC
    // through (XLM's own threshold set higher than its drift) must still
    // report true, since `.any()` checks every entry against its own
    // threshold independently.
    assert!(needs_rebalance_per_asset(&allocation, |asset| match *asset {
        "XLM" => 1_000,
        _ => 100,
    }));
    // Raise both above their drift - now neither leg qualifies.
    assert!(!needs_rebalance_per_asset(&allocation, |_| 1_000));
}

#[test]
fn calendar_due_is_false_before_interval_and_true_at_or_past_it() {
    assert!(!calendar_due(29, 30));
    assert!(calendar_due(30, 30));
    assert!(calendar_due(31, 30));
}

#[test]
fn calendar_due_treats_zero_interval_as_never_due() {
    assert!(!calendar_due(0, 0));
    assert!(!calendar_due(100, 0));
}

#[test]
fn realized_volatility_is_zero_for_flat_prices() {
    assert_eq!(realized_volatility_bps(&[100, 100, 100, 100]), 0);
}

#[test]
fn realized_volatility_is_zero_with_fewer_than_two_returns() {
    assert_eq!(realized_volatility_bps(&[]), 0);
    assert_eq!(realized_volatility_bps(&[100]), 0);
    // A single return has zero deviation from its own mean by
    // definition - not enough data to call it "volatile".
    assert_eq!(realized_volatility_bps(&[100, 110]), 0);
}

#[test]
fn realized_volatility_is_nonzero_and_scales_with_swing_size() {
    let mild = realized_volatility_bps(&[100, 105, 100, 105, 100]);
    let wild = realized_volatility_bps(&[100, 150, 100, 150, 100]);
    assert!(mild > 0);
    assert!(wild > mild, "bigger swings must produce higher realized volatility");
}

#[test]
fn realized_volatility_skips_non_positive_prices_instead_of_dividing_by_zero() {
    // A non-positive price can't happen from a real oracle, but must not
    // panic if it ever shows up in a data feed.
    let vol = realized_volatility_bps(&[100, 0, 100, 105, 100]);
    assert!(vol < u32::MAX);
}

#[test]
fn volatility_adjusted_threshold_scales_with_volatility_and_clamps() {
    // 1:1 multiplier, mid-range volatility stays within the band.
    assert_eq!(volatility_adjusted_threshold_bps(300, BPS_DENOM as u32, 100, 2_000), 300);
    // A perfectly calm asset (0 realized volatility) is floored at the
    // minimum, never at an unusable 0.
    assert_eq!(volatility_adjusted_threshold_bps(0, BPS_DENOM as u32, 100, 2_000), 100);
    // A wildly volatile asset is capped at the maximum.
    assert_eq!(volatility_adjusted_threshold_bps(50_000, BPS_DENOM as u32, 100, 2_000), 2_000);
}
