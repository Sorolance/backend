#![cfg(test)]

use super::*;
use chrono::NaiveDate;

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
}

fn targets() -> Vec<TargetWeight<String>> {
    vec![
        TargetWeight {
            asset: "XLM".to_string(),
            weight_bps: 6_000,
        },
        TargetWeight {
            asset: "USDC".to_string(),
            weight_bps: 4_000,
        },
    ]
}

#[test]
fn align_daily_keeps_only_dates_present_in_every_series() {
    let xlm = AssetPriceSeries {
        symbol: "XLM".to_string(),
        prices: vec![(date(1), 100), (date(2), 110), (date(3), 120)],
    };
    // Missing day 2 - day 2 must be dropped from the aligned result even
    // though XLM has it.
    let usdc = AssetPriceSeries {
        symbol: "USDC".to_string(),
        prices: vec![(date(1), 100), (date(3), 100)],
    };

    let aligned = align_daily(&[xlm, usdc]);
    let dates: Vec<NaiveDate> = aligned.iter().map(|(d, _)| *d).collect();
    assert_eq!(dates, vec![date(1), date(3)]);
}

#[test]
fn empty_series_produces_no_aligned_days() {
    assert!(align_daily(&[]).is_empty());
}

#[test]
fn never_rebalances_when_prices_stay_exactly_on_target() {
    // Both assets flat at the same price every day - starting balances
    // are already an exact 60/40 split, so drift stays at zero.
    let daily_prices = vec![
        (date(1), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
        (date(2), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
        (date(3), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
    ];

    let report = run_backtest(&targets(), 500, 10_000, &daily_prices).unwrap();

    assert!(report.rebalances.is_empty());
    assert_eq!(report.equity_curve.len(), 3);
    assert_eq!(report.total_return_bps, 0);
}

#[test]
fn rebalances_when_a_price_move_pushes_drift_past_threshold() {
    // XLM starts at 100, then 10x's - massively overweight, well past a
    // 5% threshold.
    let daily_prices = vec![
        (date(1), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
        (date(2), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]),
    ];

    let report = run_backtest(&targets(), 500, 10_000, &daily_prices).unwrap();

    assert_eq!(report.rebalances.len(), 1);
    assert_eq!(report.rebalances[0].date, date(2));
    // The move happened, so equity must reflect the appreciation even
    // before/at the moment of rebalancing.
    assert!(report.total_return_bps > 0);
}

#[test]
fn a_rebalanced_portfolio_does_not_immediately_re_trigger_on_a_quiet_day() {
    let daily_prices = vec![
        (date(1), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
        (date(2), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]), // triggers rebalance
        (date(3), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]), // same prices - now on target
    ];

    let report = run_backtest(&targets(), 500, 10_000, &daily_prices).unwrap();

    assert_eq!(
        report.rebalances.len(),
        1,
        "day 3 is quiet relative to day 2's already-rebalanced portfolio"
    );
}

#[test]
fn unknown_target_asset_is_a_typed_error_not_a_panic() {
    let daily_prices = vec![(date(1), vec![("XLM".to_string(), 100)])]; // no USDC price at all

    let err = run_backtest(&targets(), 500, 10_000, &daily_prices).unwrap_err();
    assert!(matches!(err, BacktestError::UnknownAsset(symbol) if symbol == "USDC"));
}

#[test]
fn no_price_data_is_a_typed_error_not_a_panic() {
    let err = run_backtest(&targets(), 500, 10_000, &[]).unwrap_err();
    assert!(matches!(err, BacktestError::NoAlignedPriceData));
}
