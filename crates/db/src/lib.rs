//! Postgres connection + row models for the rebalancer backend.
//!
//! Query/repository methods are added as real callers need them, rather
//! than guessed at ahead of time. `upsert_portfolio` and
//! `insert_rebalance_event` exist because `crates/scheduler` is now a real
//! consumer; everything else still goes through ad hoc `sqlx::query_as`
//! (see `crates/db/src/test.rs`) until an `api` crate needs it too.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
pub use sqlx::PgPool;
pub use uuid::Uuid;

/// Migrations embedded at compile time from `./migrations` - run them with
/// `MIGRATOR.run(&pool).await` on startup, or via `sqlx-cli` in dev/CI.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Portfolio {
    pub id: Uuid,
    pub vault_address: String,
    pub owner_address: String,
    pub name: String,
    pub strategy_type: String,
    pub threshold_bps: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Target {
    pub id: Uuid,
    pub portfolio_id: Uuid,
    pub asset: String,
    pub price_asset_kind: String,
    pub price_asset_value: String,
    pub weight_bps: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RebalanceEvent {
    pub id: Uuid,
    pub portfolio_id: Uuid,
    pub tx_hash: String,
    pub executed_at: DateTime<Utc>,
    pub trades: serde_json::Value,
    pub fee_paid: Option<BigDecimal>,
    pub slippage_bps: Option<i32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Lot {
    pub id: Uuid,
    pub portfolio_id: Uuid,
    pub asset: String,
    pub qty: BigDecimal,
    pub price: BigDecimal,
    pub acquired_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PriceSnapshot {
    pub id: Uuid,
    pub asset_kind: String,
    pub asset_value: String,
    pub price: BigDecimal,
    pub source: String,
    pub ledger_seq: Option<i64>,
    pub observed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub portfolio_id: Uuid,
    pub url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// Inserts a portfolio row on first sight of a vault address, or updates
/// the mutable metadata (owner, name, threshold) on every call after -
/// safe to call on every scheduler startup rather than needing a separate
/// one-time provisioning step. `strategy_type` is left at its `threshold`
/// default since that's the only strategy that exists on-chain today.
pub async fn upsert_portfolio(
    pool: &PgPool,
    vault_address: &str,
    owner_address: &str,
    name: &str,
    threshold_bps: i32,
) -> Result<Portfolio, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO portfolios (vault_address, owner_address, name, threshold_bps)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (vault_address) DO UPDATE
         SET owner_address = EXCLUDED.owner_address,
             name = EXCLUDED.name,
             threshold_bps = EXCLUDED.threshold_bps
         RETURNING *",
    )
    .bind(vault_address)
    .bind(owner_address)
    .bind(name)
    .bind(threshold_bps)
    .fetch_one(pool)
    .await
}

/// Records a successful `vault::rebalance` call. `tx_hash` is unique, so a
/// scheduler retrying after a crash between submission and this insert
/// will hit the `ON CONFLICT` branch and get back `None` rather than a
/// duplicate row or an error - callers should treat `None` as "already
/// recorded", not a failure.
pub async fn insert_rebalance_event(
    pool: &PgPool,
    portfolio_id: Uuid,
    tx_hash: &str,
    executed_at: DateTime<Utc>,
    trades: serde_json::Value,
) -> Result<Option<RebalanceEvent>, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO rebalance_events (portfolio_id, tx_hash, executed_at, trades)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (tx_hash) DO NOTHING
         RETURNING *",
    )
    .bind(portfolio_id)
    .bind(tx_hash)
    .bind(executed_at)
    .bind(trades)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod test;
