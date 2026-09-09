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

    let report = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();

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

    let report = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();

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

    let report = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();

    assert_eq!(
        report.rebalances.len(),
        1,
        "day 3 is quiet relative to day 2's already-rebalanced portfolio"
    );
}

#[test]
fn unknown_target_asset_is_a_typed_error_not_a_panic() {
    let daily_prices = vec![(date(1), vec![("XLM".to_string(), 100)])]; // no USDC price at all

    let err = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap_err();
    assert!(matches!(err, BacktestError::UnknownAsset(symbol) if symbol == "USDC"));
}

#[test]
fn no_price_data_is_a_typed_error_not_a_panic() {
    let err = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &[],
    )
    .unwrap_err();
    assert!(matches!(err, BacktestError::NoAlignedPriceData));
}

#[test]
fn calendar_never_rebalances_before_interval_elapses_even_with_huge_drift() {
    let daily_prices = vec![
        (date(1), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]),
        (date(2), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]),
        (date(3), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]),
        (date(4), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]),
        (date(5), vec![("XLM".to_string(), 1_000), ("USDC".to_string(), 100)]),
    ];

    let report = run_backtest(
        &Strategy::Calendar { interval_days: 30 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();

    assert!(
        report.rebalances.is_empty(),
        "5 days in with a 30-day interval, drift alone must not trigger anything"
    );
}

#[test]
fn calendar_rebalances_on_schedule_regardless_of_drift() {
    // Flat prices - zero drift on every day - but the calendar strategy
    // must still fire purely on elapsed time.
    let flat = |d| (date(d), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]);
    let daily_prices = vec![flat(1), flat(2), flat(3), flat(4), flat(5)];

    let report = run_backtest(
        &Strategy::Calendar { interval_days: 2 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();

    let dates: Vec<NaiveDate> = report.rebalances.iter().map(|r| r.date).collect();
    assert_eq!(dates, vec![date(3), date(5)]);
}

#[test]
fn volatility_band_narrows_for_a_calm_asset_and_catches_modest_drift() {
    // Both assets dead flat until the last day, so realized volatility
    // is 0 for both - the effective threshold floors at min_threshold_bps
    // (200), well below a typical fixed 500bps threshold.
    let flat = |d| (date(d), vec![("XLM".to_string(), 100), ("USDC".to_string(), 100)]);
    let mut daily_prices = vec![flat(1), flat(2), flat(3), flat(4)];
    // +10% on XLM only - a modest, sub-500bps-threshold-shaped move.
    daily_prices.push((date(5), vec![("XLM".to_string(), 110), ("USDC".to_string(), 100)]));

    let strategy = Strategy::VolatilityBand {
        vol_multiplier_bps: 10_000,
        min_threshold_bps: 200,
        max_threshold_bps: 2_000,
    };
    let report = run_backtest(&strategy, &targets(), 10_000, &daily_prices).unwrap();

    assert_eq!(
        report.rebalances.len(),
        1,
        "a calm asset's narrowed threshold should catch a drift a fixed 500bps threshold would miss"
    );
    assert_eq!(report.rebalances[0].date, date(5));

    // Confirm a fixed threshold really would have missed this drift, so
    // the narrowing is doing real work, not just coincidentally matching.
    let fixed_report = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();
    assert!(fixed_report.rebalances.is_empty());
}

#[test]
fn volatility_band_widens_for_a_volatile_asset_and_suppresses_a_drift_that_would_otherwise_trigger()
{
    // Both assets swing wildly in lockstep (same price at every step) for
    // several days - each individually has high realized volatility, but
    // since they move identically the *ratio* between them (and so the
    // allocation drift) stays exactly at target the whole time, meaning
    // no early trigger sneaks in before volatility has had time to build
    // up. Only on the final day does XLM move independently, creating
    // real drift.
    let swing = |d, xlm, usdc| (date(d), vec![("XLM".to_string(), xlm), ("USDC".to_string(), usdc)]);
    let mut daily_prices = vec![
        swing(1, 100, 100),
        swing(2, 180, 180),
        swing(3, 100, 100),
        swing(4, 180, 180),
        swing(5, 100, 100),
        swing(6, 180, 180),
    ];
    // Final day: XLM alone jumps further while USDC holds - a ~570bps
    // drift that would trip a fixed 500bps threshold, but both legs'
    // realized volatility (built up from the lockstep swings above) is
    // now large enough that the multiplier pins the effective threshold
    // at max_threshold_bps.
    daily_prices.push(swing(7, 230, 180));

    let strategy = Strategy::VolatilityBand {
        vol_multiplier_bps: 10_000,
        min_threshold_bps: 200,
        max_threshold_bps: 3_000,
    };
    let report = run_backtest(&strategy, &targets(), 10_000, &daily_prices).unwrap();

    let fixed_report = run_backtest(
        &Strategy::Threshold { threshold_bps: 500 },
        &targets(),
        10_000,
        &daily_prices,
    )
    .unwrap();
    assert!(
        !fixed_report.rebalances.is_empty(),
        "sanity check: a fixed 500bps threshold must actually trigger on this data"
    );
    assert!(
        report.rebalances.is_empty(),
        "high realized volatility should widen the band enough to suppress the same drift"
    );
}
