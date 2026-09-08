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

use chain::{ChainClient, ChainError};
use chrono::Utc;
use rebalancer_db::{PgPool, Uuid};
use rebalancer_notify::WebhookClient;
use tracing::{error, info, warn};

/// One poll-and-maybe-rebalance cycle for a single portfolio. Blocking
/// (the `stellar` CLI calls inside `chain` are synchronous subprocess
/// calls) - callers on an async runtime should run this via
/// `tokio::task::spawn_blocking` for the chain half; the DB write after
/// it is genuinely async.
pub async fn run_once(
    pool: &PgPool,
    chain: &ChainClient,
    webhook_client: &WebhookClient,
    portfolio_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let c = chain.clone();
    let needs = tokio::task::spawn_blocking(move || c.needs_rebalance()).await??;
    if !needs {
        info!("drift below threshold, nothing to do");
        return Ok(());
    }

    info!("drift at or above threshold, submitting rebalance");
    let c = chain.clone();
    match tokio::task::spawn_blocking(move || c.submit_rebalance()).await? {
        Ok(outcome) => {
            let inserted = rebalancer_db::insert_rebalance_event(
                pool,
                portfolio_id,
                &outcome.tx_hash,
                Utc::now(),
                serde_json::json!([]),
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
                "vault rejected the rebalance attempt (expected: RouterNotConfigured until Phase 4 wires a router)"
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
