//! Runs a real backtest against real CoinGecko historical data for this
//! project's own 60/40 XLM/USDC, 5% threshold configuration (see
//! PROJECT.md's deployed vault). No `api` crate exists yet to expose
//! `POST /portfolios/:id/backtest` for real, so this CLI is the only way
//! to actually run one today - same situation `rebalancer-scheduler` was
//! in before any API existed.
//!
//! Usage: `cargo run -p rebalancer-backtest --bin backtest [days]`
//! (defaults to 90 days).

use chrono::NaiveDate;
use rebalancer_backtest::{align_daily, run_backtest, AssetPriceSeries};
use rebalancer_core::TargetWeight;
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

#[tokio::main]
async fn main() {
    let days: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);

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

    let report = match run_backtest(&targets, THRESHOLD_BPS, initial_value, &daily_prices) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("backtest failed: {e}");
            std::process::exit(1);
        }
    };

    let scale = 10f64.powi(DECIMALS as i32);
    println!(
        "Backtest: {} days, {} aligned trading days, 60/40 XLM/USDC, {}bps threshold",
        days,
        daily_prices.len(),
        THRESHOLD_BPS
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
    println!("Rebalances: {}", report.rebalances.len());
    for rebalance in &report.rebalances {
        let drift: Vec<String> = rebalance
            .allocation_before
            .iter()
            .map(|e| format!("{}: {:+.2}%", e.asset, e.drift_bps as f64 / 100.0))
            .collect();
        println!("  {} - drift at trigger: {}", rebalance.date, drift.join(", "));
    }
}
