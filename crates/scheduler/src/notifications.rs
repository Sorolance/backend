//! Dispatches webhooks for the two event types PROJECT.md calls out:
//! rebalance completion and risk triggers. Event shaping (what goes in
//! the JSON body, which DB rows to look up) lives here rather than in
//! `rebalancer-notify`, which stays a generic "sign and POST" utility
//! with no opinion about this project's own event types.

use chrono::{DateTime, Utc};
use rebalancer_db::{PgPool, Uuid};
use rebalancer_notify::WebhookClient;
use tracing::{error, info};

pub async fn notify_rebalance_completed(
    pool: &PgPool,
    client: &WebhookClient,
    portfolio_id: Uuid,
    tx_hash: &str,
    executed_at: DateTime<Utc>,
) {
    dispatch(
        pool,
        client,
        portfolio_id,
        "rebalance.completed",
        serde_json::json!({
            "portfolio_id": portfolio_id,
            "tx_hash": tx_hash,
            "executed_at": executed_at,
        }),
    )
    .await;
}

pub async fn notify_circuit_breaker_tripped(
    pool: &PgPool,
    client: &WebhookClient,
    portfolio_id: Uuid,
    vault_contract_id: &str,
) {
    dispatch(
        pool,
        client,
        portfolio_id,
        "risk.circuit_breaker_tripped",
        serde_json::json!({
            "portfolio_id": portfolio_id,
            "vault_contract_id": vault_contract_id,
            "tripped_at": Utc::now(),
        }),
    )
    .await;
}

async fn dispatch(
    pool: &PgPool,
    client: &WebhookClient,
    portfolio_id: Uuid,
    event_type: &str,
    data: serde_json::Value,
) {
    let webhooks = match rebalancer_db::list_active_webhooks(pool, portfolio_id, event_type).await
    {
        Ok(webhooks) => webhooks,
        Err(e) => {
            error!(error = %e, event_type, "failed to list webhook subscribers");
            return;
        }
    };
    if webhooks.is_empty() {
        // The common case today - no onboarding API/UI exists yet to
        // register a webhook at all (see PROJECT.md) - not worth a log
        // line every tick.
        return;
    }

    let body = serde_json::json!({ "event": event_type, "data": data });
    let body_bytes = serde_json::to_vec(&body).expect("serde_json::Value always serializes");

    for webhook in webhooks {
        match client.send(&webhook.url, &webhook.secret, &body_bytes).await {
            Ok(()) => info!(webhook_id = %webhook.id, event_type, "webhook delivered"),
            Err(e) => {
                error!(webhook_id = %webhook.id, event_type, error = %e, "webhook delivery failed")
            }
        }
    }
}

#[cfg(test)]
mod test;
