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

/// One target weight to write alongside a new portfolio - mirrors
/// `contracts::vault::TargetWeight` plus the DB's separate
/// `price_asset_kind`/`price_asset_value` split (see `targets`'s migration
/// doc comment).
#[derive(Debug, Clone)]
pub struct NewTarget {
    pub asset: String,
    pub price_asset_kind: String,
    pub price_asset_value: String,
    pub weight_bps: i32,
}

/// Registers a brand-new sub-portfolio: the vault has already been
/// deployed, `initialize`d, and had `set_keeper` called on it by the
/// owner's own wallet (see `crates/api`'s `POST /portfolios`) - this just
/// records that fact so the scheduler and dashboard pick it up. Unlike
/// `upsert_portfolio` (used by the scheduler's single-instance startup
/// seeding), this is a one-shot insert: a second call with the same
/// `vault_address` fails on the `UNIQUE` constraint rather than silently
/// updating, since re-registering an existing portfolio is never a valid
/// client action.
pub async fn insert_portfolio_with_targets(
    pool: &PgPool,
    vault_address: &str,
    owner_address: &str,
    name: &str,
    threshold_bps: i32,
    targets: &[NewTarget],
) -> Result<Portfolio, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let portfolio: Portfolio = sqlx::query_as(
        "INSERT INTO portfolios (vault_address, owner_address, name, threshold_bps)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(vault_address)
    .bind(owner_address)
    .bind(name)
    .bind(threshold_bps)
    .fetch_one(&mut *tx)
    .await?;

    for t in targets {
        sqlx::query(
            "INSERT INTO targets (portfolio_id, asset, price_asset_kind, price_asset_value, weight_bps)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(portfolio.id)
        .bind(&t.asset)
        .bind(&t.price_asset_kind)
        .bind(&t.price_asset_value)
        .bind(t.weight_bps)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(portfolio)
}

/// Every registered portfolio, across all owners - the scheduler's own
/// polling loop uses this to tick every sub-portfolio's vault on the same
/// interval, rather than the single env-configured one it was limited to
/// before sub-portfolios existed.
pub async fn list_portfolios(pool: &PgPool) -> Result<Vec<Portfolio>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM portfolios ORDER BY created_at")
        .fetch_all(pool)
        .await
}

/// Every portfolio owned by one wallet address - what the dashboard's
/// portfolio list queries by.
pub async fn list_portfolios_by_owner(
    pool: &PgPool,
    owner_address: &str,
) -> Result<Vec<Portfolio>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM portfolios WHERE owner_address = $1 ORDER BY created_at")
        .bind(owner_address)
        .fetch_all(pool)
        .await
}

pub async fn get_portfolio(pool: &PgPool, id: Uuid) -> Result<Option<Portfolio>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM portfolios WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn list_targets(pool: &PgPool, portfolio_id: Uuid) -> Result<Vec<Target>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM targets WHERE portfolio_id = $1 ORDER BY created_at")
        .bind(portfolio_id)
        .fetch_all(pool)
        .await
}

/// Every recorded rebalance for one portfolio, newest first - the source
/// rows for the audit log CSV export (`crates/api`'s
/// `GET /portfolios/:id/report.csv`).
pub async fn list_rebalance_events(
    pool: &PgPool,
    portfolio_id: Uuid,
) -> Result<Vec<RebalanceEvent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM rebalance_events WHERE portfolio_id = $1 ORDER BY executed_at DESC",
    )
    .bind(portfolio_id)
    .fetch_all(pool)
    .await
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

/// Records one price observation. Append-only (no uniqueness constraint,
/// unlike `rebalance_events.tx_hash`) - every poll is its own row, since
/// this is a time series, not current-state. `price` is already scaled
/// to whatever fixed-point base the caller's source uses (Reflector's
/// raw `i128`, or CoinGecko's USD float scaled to match it for direct
/// comparison) - this function doesn't know or care which source it is
/// beyond the `source` label, by design (see `rebalancer-oracle`'s crate
/// doc comment for why the two sources are kept source-agnostic here but
/// never blurred into one "the" price upstream).
pub async fn insert_price_snapshot(
    pool: &PgPool,
    asset_kind: &str,
    asset_value: &str,
    price: BigDecimal,
    source: &str,
    ledger_seq: Option<i64>,
    observed_at: DateTime<Utc>,
) -> Result<PriceSnapshot, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO price_snapshots (asset_kind, asset_value, price, source, ledger_seq, observed_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING *",
    )
    .bind(asset_kind)
    .bind(asset_value)
    .bind(price)
    .bind(source)
    .bind(ledger_seq)
    .bind(observed_at)
    .fetch_one(pool)
    .await
}

/// Active webhook subscribers for one portfolio and event type -
/// `event_type` must be present in a row's `event_types` array (an
/// unrestricted `TEXT[]`, so this list of valid values lives in
/// `rebalancer-notify`/`rebalancer-scheduler`, not a DB constraint) and
/// `is_active` must be true. Empty results are the common case (no
/// onboarding API/UI exists yet to register a webhook at all - see
/// PROJECT.md) and callers should treat that as "nothing to do", not an
/// error.
pub async fn list_active_webhooks(
    pool: &PgPool,
    portfolio_id: Uuid,
    event_type: &str,
) -> Result<Vec<Webhook>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM webhooks
         WHERE portfolio_id = $1 AND is_active = true AND $2 = ANY(event_types)",
    )
    .bind(portfolio_id)
    .bind(event_type)
    .fetch_all(pool)
    .await
}

/// Registers a webhook subscriber for one portfolio. `secret` is generated
/// by the caller (`crates/api`) and returned to the registering user
/// exactly once, at registration time - this function just persists it,
/// the same secret then doubles as the credential
/// `insert_external_trigger`'s caller checks inbound requests against, so
/// a power user manages one credential per portfolio, not two.
pub async fn insert_webhook(
    pool: &PgPool,
    portfolio_id: Uuid,
    url: &str,
    secret: &str,
    event_types: &[String],
) -> Result<Webhook, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO webhooks (portfolio_id, url, secret, event_types)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(portfolio_id)
    .bind(url)
    .bind(secret)
    .bind(event_types)
    .fetch_one(pool)
    .await
}

/// Every active webhook for a portfolio, regardless of `event_types` -
/// unlike `list_active_webhooks`, which filters to subscribers of one
/// outbound event. Used to authenticate an inbound external-trigger
/// request: any of a portfolio's registered secrets is a valid credential
/// for that portfolio's `/trigger` endpoint, independent of which events
/// that webhook happens to be subscribed to.
pub async fn list_active_webhooks_for_portfolio(
    pool: &PgPool,
    portfolio_id: Uuid,
) -> Result<Vec<Webhook>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM webhooks WHERE portfolio_id = $1 AND is_active = true")
        .bind(portfolio_id)
        .fetch_all(pool)
        .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExternalTrigger {
    pub id: Uuid,
    pub portfolio_id: Uuid,
    pub reason: Option<String>,
    pub payload: serde_json::Value,
    pub requested_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}

/// Records an authenticated external trigger request (PROJECT.md
/// differentiator #8) - `crates/api`'s `/trigger` handler calls this only
/// after verifying the request's HMAC signature against one of the
/// portfolio's registered webhook secrets. Unprocessed until
/// `claim_pending_external_trigger` picks it up.
pub async fn insert_external_trigger(
    pool: &PgPool,
    portfolio_id: Uuid,
    reason: Option<&str>,
    payload: serde_json::Value,
) -> Result<ExternalTrigger, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO external_triggers (portfolio_id, reason, payload)
         VALUES ($1, $2, $3)
         RETURNING *",
    )
    .bind(portfolio_id)
    .bind(reason)
    .bind(payload)
    .fetch_one(pool)
    .await
}

/// Atomically claims the oldest unprocessed trigger for a portfolio, if
/// any - `FOR UPDATE SKIP LOCKED` so a second scheduler instance polling
/// the same portfolio (there's only ever one today, but this makes the
/// query safe if that changes) can't double-claim the same row. Returns
/// `None` when there's nothing pending, the common case on most polls.
pub async fn claim_pending_external_trigger(
    pool: &PgPool,
    portfolio_id: Uuid,
) -> Result<Option<ExternalTrigger>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let pending: Option<ExternalTrigger> = sqlx::query_as(
        "SELECT * FROM external_triggers
         WHERE portfolio_id = $1 AND processed_at IS NULL
         ORDER BY requested_at
         LIMIT 1
         FOR UPDATE SKIP LOCKED",
    )
    .bind(portfolio_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(trigger) = pending else {
        tx.commit().await?;
        return Ok(None);
    };

    let claimed: ExternalTrigger =
        sqlx::query_as("UPDATE external_triggers SET processed_at = now() WHERE id = $1 RETURNING *")
            .bind(trigger.id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(Some(claimed))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StrategyTemplate {
    pub id: Uuid,
    pub name: String,
    pub threshold_bps: i32,
    /// Same shape as a `NewTarget` array - see the `strategy_templates`
    /// migration's doc comment for why this stays JSONB rather than its
    /// own row-per-target table.
    pub targets: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Publishes a snapshot of `targets` as a public strategy template
/// (PROJECT.md differentiator #10) - opt-in only, never automatic, and
/// deliberately anonymized: no portfolio/vault/owner reference is stored
/// anywhere in this row, just the target weights and threshold
/// themselves, as of the moment of publishing.
pub async fn insert_strategy_template(
    pool: &PgPool,
    name: &str,
    threshold_bps: i32,
    targets: serde_json::Value,
) -> Result<StrategyTemplate, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO strategy_templates (name, threshold_bps, targets)
         VALUES ($1, $2, $3)
         RETURNING *",
    )
    .bind(name)
    .bind(threshold_bps)
    .bind(targets)
    .fetch_one(pool)
    .await
}

/// Every published template, newest first - what the "browse" list
/// queries by. Unfiltered and unpaginated for now, same scope as
/// `list_portfolios`.
pub async fn list_strategy_templates(pool: &PgPool) -> Result<Vec<StrategyTemplate>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM strategy_templates ORDER BY created_at DESC")
        .fetch_all(pool)
        .await
}

pub async fn get_strategy_template(pool: &PgPool, id: Uuid) -> Result<Option<StrategyTemplate>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM strategy_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

#[cfg(test)]
mod test;
