use std::time::Duration;

use rebalancer_db::{connect, upsert_portfolio, MIGRATOR};
use rebalancer_scheduler::chain::ChainClient;
use rebalancer_scheduler::{config::Config, run_once};
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
    };

    info!(
        portfolio_id = %portfolio.id,
        vault = %config.vault_contract_id,
        interval_secs = config.poll_interval_secs,
        "scheduler starting"
    );

    let mut interval = tokio::time::interval(Duration::from_secs(config.poll_interval_secs));
    loop {
        interval.tick().await;
        if let Err(e) = run_once(&pool, &chain, portfolio.id).await {
            error!(error = %e, "tick failed");
        }
    }
}
