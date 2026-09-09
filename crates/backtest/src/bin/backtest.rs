//! Runs a real backtest against real CoinGecko historical data for this
//! project's own 60/40 XLM/USDC configuration (see PROJECT.md's deployed
//! vault), against any of `rebalancer_core::Strategy`'s templates. No
//! `api` crate exists yet to expose `POST /portfolios/:id/backtest` for
//! real, so this CLI is the only way to actually run one today - same
//! situation `rebalancer-scheduler` was in before any API existed.
//!
//! Usage: `cargo run -p rebalancer-backtest --bin backtest [days] [strategy]`
//! (`days` defaults to 90; `strategy` is one of `threshold` (default),
//! `calendar`, `vol-band` - each strategy's own parameters are fixed
//! constants below rather than further CLI flags, since this is a
//! one-off exploration tool, not the real `POST /portfolios/:id/backtest`
//! API this will become).

use chrono::NaiveDate;
use rebalancer_backtest::{align_daily, run_backtest, AssetPriceSeries};
use rebalancer_core::{Strategy, TargetWeight};
use rebalancer_oracle::coingecko::CoinGeckoClient;

/// Matches the fixed-point convention `rebalancer_oracle::divergence_bps`
/// and `rebalancer_scheduler::pricing` already use for CoinGecko-sourced
/// prices, so a backtest run reasons about prices in the same units the
/// live scheduler's own price_snapshots do.
const DECIMALS: u32 = 14;

const INITIAL_VALUE_USD: i128 = 10_000;

struct Asset {
    symbol: &'static str,
    coingecko_id: &'static str,
    weight_bps: u32,
}

const ASSETS: &[Asset] = &[
    Asset {
        symbol: "XLM",
        coingecko_id: "stellar",
        weight_bps: 6_000,
    },
    Asset {
        symbol: "USDC",
        coingecko_id: "usd-coin",
        weight_bps: 4_000,
    },
];
const THRESHOLD_BPS: u32 = 500;
const CALENDAR_INTERVAL_DAYS: u32 = 30;
const VOL_MULTIPLIER_BPS: u32 = 10_000; // 1:1 - threshold tracks realized volatility directly
const VOL_MIN_THRESHOLD_BPS: u32 = 200;
const VOL_MAX_THRESHOLD_BPS: u32 = 2_000;

fn parse_strategy(name: &str) -> Strategy {
    match name {
        "calendar" => Strategy::Calendar {
            interval_days: CALENDAR_INTERVAL_DAYS,
        },
        "vol-band" => Strategy::VolatilityBand {
            vol_multiplier_bps: VOL_MULTIPLIER_BPS,
            min_threshold_bps: VOL_MIN_THRESHOLD_BPS,
            max_threshold_bps: VOL_MAX_THRESHOLD_BPS,
        },
        "threshold" => Strategy::Threshold {
            threshold_bps: THRESHOLD_BPS,
        },
        other => {
            eprintln!("unknown strategy '{other}' - expected threshold, calendar, or vol-band");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let days: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(90);
    let strategy_name = args.next().unwrap_or_else(|| "threshold".to_string());
    let strategy = parse_strategy(&strategy_name);

    let client = CoinGeckoClient::new();
    let mut series = Vec::new();
    for asset in ASSETS {
        let points = match client.market_chart(asset.coingecko_id, days).await {
            Ok(points) => points,
            Err(e) => {
                eprintln!("failed to fetch history for {}: {e}", asset.symbol);
                std::process::exit(1);
            }
        };
        // Daily interval can still return more than one point for the
        // current (incomplete) day - last-write-wins keeps the most
        // recent, which is what a live backtest run wants.
        let mut by_date: std::collections::BTreeMap<NaiveDate, i128> = Default::default();
        for point in points {
            let scaled = (point.price_usd * 10f64.powi(DECIMALS as i32)).round() as i128;
            by_date.insert(point.timestamp.date_naive(), scaled);
        }
        series.push(AssetPriceSeries {
            symbol: asset.symbol.to_string(),
            prices: by_date.into_iter().collect(),
        });
    }

    let daily_prices = align_daily(&series);
    let targets: Vec<TargetWeight<String>> = ASSETS
        .iter()
        .map(|a| TargetWeight {
            asset: a.symbol.to_string(),
            weight_bps: a.weight_bps,
        })
        .collect();
    let initial_value = INITIAL_VALUE_USD * 10i128.pow(DECIMALS);

    let report = match run_backtest(&strategy, &targets, initial_value, &daily_prices) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("backtest failed: {e}");
            std::process::exit(1);
        }
    };

    let scale = 10f64.powi(DECIMALS as i32);
    println!(
        "Backtest: {} days, {} aligned trading days, 60/40 XLM/USDC, strategy: {}",
        days,
        daily_prices.len(),
        strategy_name
    );
    println!(
        "Initial value: ${:.2}",
        initial_value as f64 / scale
    );
    println!(
        "Final value:   ${:.2} ({:+.2}%)",
        report.final_value as f64 / scale,
        report.total_return_bps as f64 / 100.0
    );
    println!(
        "Rebalances: {} (total realized gain/loss: ${:+.2})",
        report.rebalances.len(),
        report.total_realized_gain_loss as f64 / scale
    );
    for rebalance in &report.rebalances {
        let drift: Vec<String> = rebalance
            .allocation_before
            .iter()
            .map(|e| format!("{}: {:+.2}%", e.asset, e.drift_bps as f64 / 100.0))
            .collect();
        println!(
            "  {} - drift at trigger: {} - realized gain/loss: ${:+.2}",
            rebalance.date,
            drift.join(", "),
            rebalance.realized_gain_loss as f64 / scale
        );
    }
}
