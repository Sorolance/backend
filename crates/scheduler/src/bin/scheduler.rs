use std::time::Duration;

use rebalancer_db::{connect, upsert_portfolio, MIGRATOR};
use rebalancer_notify::WebhookClient;
use rebalancer_oracle::coingecko::CoinGeckoClient;
use rebalancer_oracle::on_chain::OnChainPriceReader;
use rebalancer_scheduler::chain::ChainClient;
use rebalancer_scheduler::pricing::observe_market_prices;
use rebalancer_scheduler::{config::Config, observe_risk_once, run_once, FeeAwareConfig, RebalanceParams};
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    };

    let pool = connect(&config.database_url)
        .await
        .expect("connect to database");
    MIGRATOR.run(&pool).await.expect("run migrations");

    // Upserted on every startup rather than provisioned separately - there's
    // no onboarding API yet (see PROJECT.md), and this is the only portfolio
    // this scheduler instance manages.
    let portfolio = upsert_portfolio(
        &pool,
        &config.vault_contract_id,
        &config.owner_address,
        &config.portfolio_name,
        config.threshold_bps,
    )
    .await
    .expect("upsert portfolio row");

    let chain = ChainClient {
        stellar_cli: config.stellar_cli.clone(),
        rpc_url: config.rpc_url.clone(),
        network_passphrase: config.network_passphrase.clone(),
        vault_contract_id: config.vault_contract_id.clone(),
        keeper_identity: config.keeper_identity.clone(),
        keeper_address: config.keeper_address.clone(),
        router_contract_id: config.router_contract_id.clone(),
    };
    let on_chain_prices = OnChainPriceReader {
        stellar_cli: config.stellar_cli.clone(),
        rpc_url: config.rpc_url.clone(),
        network_passphrase: config.network_passphrase.clone(),
        oracle_adapter_id: config.oracle_adapter_contract_id.clone(),
        // Read-only - reuses the keeper identity rather than requiring a
        // separate one just to source a funded --source-account.
        source_account: config.keeper_identity.clone(),
    };
    let coingecko = CoinGeckoClient::new();
    let webhook_client = WebhookClient::new();
    let fee_aware = FeeAwareConfig {
        max_cost_bps: config.max_rebalance_cost_bps,
        urgent_drift_multiplier: config.urgent_drift_multiplier,
        execution_slippage_buffer_bps: config.execution_slippage_buffer_bps,
    };

    info!(
        portfolio_id = %portfolio.id,
        vault = %config.vault_contract_id,
        interval_secs = config.poll_interval_secs,
        trigger_interval_secs = config.trigger_poll_interval_secs,
        "scheduler starting"
    );

    let mut interval = tokio::time::interval(Duration::from_secs(config.poll_interval_secs));
    let mut trigger_interval =
        tokio::time::interval(Duration::from_secs(config.trigger_poll_interval_secs));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = observe_market_prices(
                    &pool,
                    &on_chain_prices,
                    &coingecko,
                    config.price_divergence_warn_bps,
                )
                .await
                {
                    error!(error = %e, "price observation tick failed");
                }
                if let Err(e) = observe_risk_once(
                    &pool,
                    &chain,
                    &webhook_client,
                    portfolio.id,
                    &config.vault_contract_id,
                )
                .await
                {
                    error!(error = %e, "risk observation tick failed");
                }
                if let Err(e) = run_once(
                    &pool,
                    &chain,
                    &on_chain_prices,
                    &webhook_client,
                    portfolio.id,
                    RebalanceParams { threshold_bps: config.threshold_bps as u32, fee_aware, force_urgent: false },
                )
                .await
                {
                    error!(error = %e, "tick failed");
                }
            }
            _ = trigger_interval.tick() => {
                // Query-only unless something's actually pending - see
                // trigger_poll_interval_secs's doc comment for why this
                // runs on its own, much shorter interval than the full
                // tick above.
                match rebalancer_db::claim_pending_external_trigger(&pool, portfolio.id).await {
                    Ok(Some(trigger)) => {
                        info!(
                            trigger_id = %trigger.id,
                            reason = ?trigger.reason,
                            "processing external trigger"
                        );
                        if let Err(e) = run_once(
                            &pool,
                            &chain,
                            &on_chain_prices,
                            &webhook_client,
                            portfolio.id,
                            RebalanceParams { threshold_bps: config.threshold_bps as u32, fee_aware, force_urgent: true },
                        )
                        .await
                        {
                            error!(error = %e, "triggered tick failed");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => error!(error = %e, "external trigger poll failed"),
                }
            }
        }
    }
}
