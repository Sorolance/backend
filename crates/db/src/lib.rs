//! Postgres connection + row models for the rebalancer backend.
//!
//! This crate holds only the schema and typed rows - no query/repository
//! methods yet, since nothing consumes them until the `api`/`scheduler`
//! crates exist (Phase 1). Building those out now would be guessing at an
//! interface no real caller has asked for yet.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

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

#[cfg(test)]
mod test;
