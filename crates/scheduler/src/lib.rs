//! Polls the deployed `vault` contract for drift on an interval and, when
//! `needs_rebalance` is true, submits `rebalance` signed by the backend's
//! keeper key - the last piece of Phase 1's "threshold-based rebalancing
//! end-to-end" (see `PROJECT.md`). Before this crate existed, `rebalance`
//! had to be triggered manually.
//!
//! Known limitation carried over from the on-chain contract (see
//! `contracts/contracts/vault/src/lib.rs` in the sibling `contracts`
//! repo): `needs_rebalance` currently returns `true` for an *empty* vault
//! too - `compute_allocation` treats a zero balance as 0% actual against
//! a nonzero target, which reads as maximum drift, rather than the "an
//! empty vault never needs rebalancing" behavior its own doc comment
//! claims. Confirmed live against the deployed testnet vault. Harmless
//! today (`rebalance` fails closed with `RouterNotConfigured` until
//! Phase 4 wires a router - see `run_once`'s handling of
//! `ChainError::Contract`), but worth fixing on-chain before Phase 4,
//! since a real, funded vault sitting exactly on-target would otherwise
//! never look "done" to this scheduler.

pub mod chain;
pub mod config;
pub mod notifications;
pub mod pricing;

use chain::{ChainClient, ChainError, SignedTrade};
use chrono::Utc;
use rebalancer_core::{AssetState, TargetWeight};
use rebalancer_db::{PgPool, Uuid};
use rebalancer_notify::WebhookClient;
use rebalancer_oracle::on_chain::OnChainPriceReader;
use std::collections::HashMap;
use tracing::{error, info, warn};

/// The fee-aware tunables PROJECT.md's "fee-aware execution" item calls
/// for - grouped separately from `config::Config` mainly so `run_once`'s
/// signature doesn't grow yet another handful of loose `u32`s.
#[derive(Debug, Clone, Copy)]
pub struct FeeAwareConfig {
    pub max_cost_bps: u32,
    pub urgent_drift_multiplier: u32,
    /// Extra headroom subtracted from a fresh `quote_swap` result before
    /// it's sent on-chain as `min_amount_out` - protects against reserves
    /// moving between this read-only quote and the write transaction
    /// actually landing (same purpose `swap_exact_in`'s own
    /// `min_amount_out` gate serves at the contract level; this just picks
    /// a real number for it instead of a caller-guessed one).
    pub execution_slippage_buffer_bps: u32,
}

/// Per-call parameters for `run_once`, grouped into one struct purely to
/// keep its argument count sane (clippy's `too_many_arguments`) - `pool`,
/// `chain`, `on_chain_prices`, and `webhook_client` stay as their own
/// params since the body borrows each of them independently and often
/// more than once (`chain.clone()` inside every `spawn_blocking` call in
/// particular), where bundling would just move the noise rather than cut
/// it.
#[derive(Debug, Clone, Copy)]
pub struct RebalanceParams {
    pub threshold_bps: u32,
    pub fee_aware: FeeAwareConfig,
    /// See `run_once`'s own doc comment for what this does and doesn't
    /// override.
    pub force_urgent: bool,
}

/// One poll-and-maybe-rebalance cycle for a single portfolio. Blocking
/// (the `stellar` CLI calls inside `chain` are synchronous subprocess
/// calls) - callers on an async runtime should run this via
/// `tokio::task::spawn_blocking` for the chain half; the DB write after
/// it is genuinely async.
///
/// Fee-aware execution (PROJECT.md section 3, item 4): once
/// `needs_rebalance` is true, this no longer submits blindly. It computes
/// the real `TradeIntent`s needed to reach target
/// (`rebalancer_core::compute_rebalance_trades`, using live balances via
/// `chain.token_balance` and live prices via `on_chain_prices`), quotes
/// each one for real against the router (`chain.quote_swap`) to measure
/// its actual slippage, and combines that with the live network fee
/// (`chain.fee_stats`) into a total cost in bps of the trade's own value
/// (`rebalancer_core::total_cost_bps`). If the portfolio's worst drift is
/// far enough past threshold to count as urgent, it executes regardless of
/// cost; otherwise it only executes if that cost is within
/// `fee_aware.max_cost_bps`, deferring to the next tick if not (the drift
/// doesn't go away - it'll be re-evaluated, likely cheaper or urgent by
/// then). All of `vault::rebalance`'s trades for a tick still submit in
/// one transaction, one network fee - that's the "batch small rebalances"
/// half of the same PROJECT.md item, and falls out of this for free since
/// `compute_rebalance_trades` already returns the whole batch at once.
///
/// `force_urgent` is set when this call is servicing a claimed external
/// trigger (PROJECT.md differentiator #8, `rebalancer-api`'s
/// `/portfolios/:id/trigger`) rather than a plain interval tick - it
/// overrides the fee-aware cost gate exactly as a genuinely urgent drift
/// would, so a power user's own trigger condition doesn't sit deferred
/// behind `fee_aware.max_cost_bps` waiting for cheaper network
/// conditions. It does *not* touch the `needs_rebalance` check above -
/// that mirrors an invariant enforced in the `vault` contract itself
/// (`rebalance` reverts as a no-op below threshold), which no amount of
/// backend-side urgency can or should bypass.
pub async fn run_once(
    pool: &PgPool,
    chain: &ChainClient,
    on_chain_prices: &OnChainPriceReader,
    webhook_client: &WebhookClient,
    portfolio_id: Uuid,
    params: RebalanceParams,
) -> Result<(), Box<dyn std::error::Error>> {
    let RebalanceParams { threshold_bps, fee_aware, force_urgent } = params;
    let c = chain.clone();
    let needs = tokio::task::spawn_blocking(move || c.needs_rebalance()).await??;
    if !needs {
        info!("drift below threshold, nothing to do");
        return Ok(());
    }

    let c = chain.clone();
    let allocation = match tokio::task::spawn_blocking(move || c.compute_allocation()).await? {
        Ok(a) => a,
        Err(ChainError::Contract(vault_err)) => {
            warn!(?vault_err, "vault rejected compute_allocation, skipping this tick");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let max_drift_bps = allocation.iter().map(|e| e.drift_bps.unsigned_abs()).max().unwrap_or(0);

    // Live balance + oracle price for every target asset, keyed by its
    // token address (matching `compute_allocation`'s own `asset` field) -
    // the absolute values `compute_rebalance_trades` needs that weight
    // ratios alone can't provide.
    let mut targets = Vec::with_capacity(allocation.len());
    let mut states = Vec::with_capacity(allocation.len());
    let mut prices: HashMap<String, i128> = HashMap::with_capacity(allocation.len());
    for entry in &allocation {
        targets.push(TargetWeight { asset: entry.asset.clone(), weight_bps: entry.target_weight_bps });
        let Some(asset_cfg) = pricing::asset_by_token_contract_id(&entry.asset) else {
            warn!(
                asset = %entry.asset,
                "compute_allocation returned an asset outside pricing::ASSETS, skipping fee-aware rebalance this tick"
            );
            return Ok(());
        };

        let c = chain.clone();
        let asset_addr = entry.asset.clone();
        let vault_id = chain.vault_contract_id.clone();
        let balance =
            tokio::task::spawn_blocking(move || c.token_balance(&asset_addr, &vault_id)).await??;

        let reader = on_chain_prices.clone();
        let kind = asset_cfg.price_asset_kind;
        let value = asset_cfg.price_asset_value.to_string();
        let price = tokio::task::spawn_blocking(move || reader.get_price(kind, &value)).await??;

        prices.insert(entry.asset.clone(), price);
        states.push(AssetState { asset: entry.asset.clone(), balance, price });
    }

    let trade_intents = rebalancer_core::compute_rebalance_trades(&targets, &states);
    if trade_intents.is_empty() {
        info!(max_drift_bps, "drift at/above threshold but no trade cleared rounding, skipping this tick");
        return Ok(());
    }

    // Quote every trade for real, tracking the batch's total value and
    // worst-case slippage.
    let mut signed_trades = Vec::with_capacity(trade_intents.len());
    let mut trade_value: i128 = 0;
    let mut worst_slippage_bps: u32 = 0;
    for intent in &trade_intents {
        let c = chain.clone();
        let asset_in = intent.asset_in.clone();
        let asset_out = intent.asset_out.clone();
        let amount_in = intent.amount_in;
        let quoted = match tokio::task::spawn_blocking(move || c.quote_swap(&asset_in, &asset_out, amount_in))
            .await?
        {
            Ok(q) => q,
            Err(ChainError::RouterContract(router_err)) => {
                warn!(
                    ?router_err,
                    asset_in = %intent.asset_in,
                    asset_out = %intent.asset_out,
                    "router rejected the quote for this trade, skipping this tick"
                );
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let price_in = prices[&intent.asset_in];
        let price_out = prices[&intent.asset_out];
        worst_slippage_bps =
            worst_slippage_bps.max(rebalancer_core::slippage_bps(amount_in, price_in, price_out, quoted));
        trade_value = trade_value.saturating_add(amount_in.saturating_mul(price_in));

        let min_amount_out = quoted.saturating_mul(10_000 - fee_aware.execution_slippage_buffer_bps as i128)
            / 10_000;
        signed_trades.push(SignedTrade {
            asset_in: intent.asset_in.clone(),
            asset_out: intent.asset_out.clone(),
            amount_in,
            min_amount_out,
        });
    }

    let c = chain.clone();
    let fee_stats = tokio::task::spawn_blocking(move || c.fee_stats()).await??;
    let xlm_price = pricing::ASSETS
        .iter()
        .find(|a| a.symbol == "XLM")
        .and_then(|a| prices.get(a.token_contract_id))
        .copied()
        .unwrap_or(0);
    let network_fee_value = fee_stats.soroban_inclusion_fee.p99.saturating_mul(xlm_price);

    let total_cost_bps = rebalancer_core::total_cost_bps(trade_value, network_fee_value, worst_slippage_bps);
    let decision = if force_urgent {
        rebalancer_core::FeeAwareDecision { execute: true, urgent: true, total_cost_bps }
    } else {
        rebalancer_core::evaluate_fee_aware_execution(
            max_drift_bps,
            threshold_bps,
            fee_aware.urgent_drift_multiplier,
            total_cost_bps,
            fee_aware.max_cost_bps,
        )
    };

    if !decision.execute {
        info!(
            total_cost_bps,
            max_drift_bps,
            max_cost_bps = fee_aware.max_cost_bps,
            "deferring rebalance to next tick: not urgent and too costly right now"
        );
        return Ok(());
    }
    info!(
        total_cost_bps,
        urgent = decision.urgent,
        trades = signed_trades.len(),
        "executing fee-aware rebalance"
    );

    let c = chain.clone();
    let trades_for_submit = signed_trades.clone();
    match tokio::task::spawn_blocking(move || c.submit_rebalance(&trades_for_submit)).await? {
        Ok(outcome) => {
            let trades_json = serde_json::json!(signed_trades
                .iter()
                .map(|t| serde_json::json!({
                    "asset_in": t.asset_in,
                    "asset_out": t.asset_out,
                    "amount_in": t.amount_in.to_string(),
                    "min_amount_out": t.min_amount_out.to_string(),
                }))
                .collect::<Vec<_>>());
            let inserted = rebalancer_db::insert_rebalance_event(
                pool,
                portfolio_id,
                &outcome.tx_hash,
                Utc::now(),
                trades_json,
            )
            .await?;
            match inserted {
                Some(event) => {
                    info!(tx_hash = %event.tx_hash, "rebalance submitted and recorded");
                    notifications::notify_rebalance_completed(
                        pool,
                        webhook_client,
                        portfolio_id,
                        &event.tx_hash,
                        event.executed_at,
                    )
                    .await;
                }
                None => warn!(
                    tx_hash = %outcome.tx_hash,
                    "rebalance tx already recorded, skipping duplicate insert"
                ),
            }
        }
        Err(ChainError::Contract(vault_err)) => {
            warn!(
                ?vault_err,
                "vault rejected the rebalance attempt (e.g. RouterNotConfigured if set_router was never called)"
            );
        }
        Err(e) => {
            error!(error = %e, "rebalance submission failed unexpectedly");
        }
    }
    Ok(())
}

/// Feeds current oracle prices into the configured `risk_guard` (via
/// `vault::observe_risk`) and dispatches a webhook if this call just
/// tripped the breaker. Independent of `run_once` - called every tick
/// regardless of whether a rebalance is imminent, matching `risk_guard`'s
/// own design intent (see the `contracts` repo: "called on the same
/// interval a keeper polls drift").
pub async fn observe_risk_once(
    pool: &PgPool,
    chain: &ChainClient,
    webhook_client: &WebhookClient,
    portfolio_id: Uuid,
    vault_contract_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let c = chain.clone();
    match tokio::task::spawn_blocking(move || c.observe_risk()).await? {
        Ok(true) => {
            warn!("circuit breaker just tripped");
            notifications::notify_circuit_breaker_tripped(
                pool,
                webhook_client,
                portfolio_id,
                vault_contract_id,
            )
            .await;
        }
        Ok(false) => {}
        Err(ChainError::Contract(vault_err)) => {
            warn!(?vault_err, "observe_risk rejected by vault");
        }
        Err(e) => {
            error!(error = %e, "observe_risk failed unexpectedly");
        }
    }
    Ok(())
}
