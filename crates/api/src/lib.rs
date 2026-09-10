//! HTTP layer for the frontend - the `api` crate `PROJECT.md` and
//! `backend/README.md` have long listed as "planned but not yet built".
//!
//! Scoped narrowly to what sub-portfolios + audit log export need:
//! registering a portfolio the frontend has already deployed and
//! initialized on-chain via the owner's own wallet (this crate never
//! deploys a contract or holds a key that could - see the `contracts`
//! repo's `vault::initialize`, which requires `owner.require_auth()`),
//! listing a wallet's portfolios for the dashboard, and exporting one
//! portfolio's on-chain rebalance history as a CSV audit log. Also covers
//! external trigger webhooks and the what-if simulator, neither of which
//! needs chain access either - `/simulate` takes the caller's own live
//! balances/prices as input (the frontend already has them, reading the
//! contract directly for the dashboard - see `PROJECT.md` section 6)
//! rather than this crate fetching them itself, preserving the
//! never-touches-the-chain boundary above.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rebalancer_db::{
    get_portfolio, insert_external_trigger, insert_portfolio_with_targets, insert_webhook,
    list_active_webhooks_for_portfolio, list_portfolios_by_owner, list_rebalance_events,
    list_targets, NewTarget, PgPool, Portfolio, Target, Uuid, Webhook,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tower_http::cors::{Any, CorsLayer};

pub fn build_router(pool: PgPool) -> Router {
    Router::new()
        .route("/portfolios", get(list_portfolios).post(create_portfolio))
        .route("/portfolios/:id", get(get_one_portfolio))
        .route("/portfolios/:id/report.csv", get(report_csv))
        .route("/portfolios/:id/webhooks", post(create_webhook))
        .route("/portfolios/:id/trigger", post(trigger_portfolio))
        .route("/portfolios/:id/simulate", post(simulate_portfolio))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(pool)
}

#[derive(Debug, Serialize)]
struct TargetResponse {
    asset: String,
    price_asset_kind: String,
    price_asset_value: String,
    weight_bps: i32,
}

impl From<Target> for TargetResponse {
    fn from(t: Target) -> Self {
        Self {
            asset: t.asset,
            price_asset_kind: t.price_asset_kind,
            price_asset_value: t.price_asset_value,
            weight_bps: t.weight_bps,
        }
    }
}

#[derive(Debug, Serialize)]
struct PortfolioResponse {
    id: Uuid,
    vault_address: String,
    owner_address: String,
    name: String,
    strategy_type: String,
    threshold_bps: i32,
    targets: Vec<TargetResponse>,
}

/// A JSON error body plus the right status code - every handler below
/// returns `Result<_, ApiError>` rather than panicking or swallowing a DB
/// error into a generic 500 with no explanation.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // A unique-constraint violation is the only sqlx::Error this crate's
        // own inserts can realistically hit in normal use (re-registering an
        // already-registered vault_address) - surfaced as 409, not a bare 500.
        if let sqlx::Error::Database(db_err) = &e {
            if db_err.is_unique_violation() {
                return ApiError {
                    status: StatusCode::CONFLICT,
                    message: "a portfolio for this vault_address is already registered".into(),
                };
            }
        }
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("database error: {e}"),
        }
    }
}

async fn portfolio_response(pool: &PgPool, portfolio: Portfolio) -> Result<PortfolioResponse, ApiError> {
    let targets = list_targets(pool, portfolio.id).await?;
    Ok(PortfolioResponse {
        id: portfolio.id,
        vault_address: portfolio.vault_address,
        owner_address: portfolio.owner_address,
        name: portfolio.name,
        strategy_type: portfolio.strategy_type,
        threshold_bps: portfolio.threshold_bps,
        targets: targets.into_iter().map(TargetResponse::from).collect(),
    })
}

#[derive(Debug, Deserialize)]
struct ListPortfoliosQuery {
    owner_address: String,
}

async fn list_portfolios(
    State(pool): State<PgPool>,
    Query(q): Query<ListPortfoliosQuery>,
) -> Result<Json<Vec<PortfolioResponse>>, ApiError> {
    let rows = list_portfolios_by_owner(&pool, &q.owner_address).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(portfolio_response(&pool, row).await?);
    }
    Ok(Json(out))
}

async fn get_one_portfolio(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<PortfolioResponse>, ApiError> {
    let portfolio = get_portfolio(&pool, id).await?.ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "portfolio not found".into(),
    })?;
    Ok(Json(portfolio_response(&pool, portfolio).await?))
}

#[derive(Debug, Deserialize)]
struct NewTargetRequest {
    asset: String,
    price_asset_kind: String,
    price_asset_value: String,
    weight_bps: i32,
}

#[derive(Debug, Deserialize)]
struct CreatePortfolioRequest {
    vault_address: String,
    owner_address: String,
    name: String,
    threshold_bps: i32,
    targets: Vec<NewTargetRequest>,
}

/// Registers a portfolio whose vault the frontend has *already* deployed,
/// `initialize`d, and authorized the shared keeper on via the owner's own
/// wallet (see `frontend/src/hooks/use-portfolios.ts`'s `useCreatePortfolio`
/// — deploy -> initialize -> set_keeper -> this call). This endpoint never
/// touches the chain itself.
async fn create_portfolio(
    State(pool): State<PgPool>,
    Json(req): Json<CreatePortfolioRequest>,
) -> Result<(StatusCode, Json<PortfolioResponse>), ApiError> {
    if req.targets.is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: "targets must not be empty".into(),
        });
    }
    let targets: Vec<NewTarget> = req
        .targets
        .into_iter()
        .map(|t| NewTarget {
            asset: t.asset,
            price_asset_kind: t.price_asset_kind,
            price_asset_value: t.price_asset_value,
            weight_bps: t.weight_bps,
        })
        .collect();

    let portfolio = insert_portfolio_with_targets(
        &pool,
        &req.vault_address,
        &req.owner_address,
        &req.name,
        req.threshold_bps,
        &targets,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(portfolio_response(&pool, portfolio).await?)))
}

/// One CSV field, quoted and with embedded quotes doubled per RFC 4180 -
/// portfolio names and trade JSON can both contain commas.
fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Every recorded rebalance for one portfolio as a downloadable CSV -
/// "downloadable CSV of every rebalance action with on-chain tx hashes"
/// from `PROJECT.md`'s audit log export item. `fee_paid`/`slippage_bps`
/// are included but stay blank today: the scheduler never populates those
/// columns yet (see `PROJECT.md`) - a pre-existing gap this export
/// surfaces rather than hides.
async fn report_csv(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let portfolio = get_portfolio(&pool, id).await?.ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "portfolio not found".into(),
    })?;
    let events = list_rebalance_events(&pool, id).await?;

    let mut csv = String::from("tx_hash,executed_at,trades,fee_paid,slippage_bps\n");
    for e in events {
        csv.push_str(&csv_field(&e.tx_hash));
        csv.push(',');
        csv.push_str(&e.executed_at.to_rfc3339());
        csv.push(',');
        csv.push_str(&csv_field(&e.trades.to_string()));
        csv.push(',');
        if let Some(fee) = e.fee_paid {
            csv.push_str(&fee.to_string());
        }
        csv.push(',');
        if let Some(slippage) = e.slippage_bps {
            csv.push_str(&slippage.to_string());
        }
        csv.push('\n');
    }

    let filename = format!("{}-rebalance-history.csv", portfolio.name.replace(' ', "-"));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/csv".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        csv,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct CreateWebhookRequest {
    url: String,
    /// e.g. `rebalance.completed`, `risk.circuit_breaker_tripped` - the
    /// values `rebalancer-scheduler::notifications` dispatches on. Not
    /// validated against a fixed list here (see `list_active_webhooks`'s
    /// own doc comment: that list lives in `rebalancer-notify`/
    /// `rebalancer-scheduler`, not a DB or API constraint).
    #[serde(default)]
    event_types: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WebhookResponse {
    id: Uuid,
    url: String,
    event_types: Vec<String>,
    /// Only ever present in this one response - the create response is
    /// the sole place a caller can read it back. It also authenticates
    /// this portfolio's `/trigger` endpoint (see that handler below), so
    /// losing it means registering a new webhook, not recovering the old
    /// secret.
    secret: String,
}

impl From<Webhook> for WebhookResponse {
    fn from(w: Webhook) -> Self {
        Self { id: w.id, url: w.url, event_types: w.event_types, secret: w.secret }
    }
}

/// Registers an outbound webhook for a portfolio, generating its secret
/// server-side (two concatenated UUIDv4s - 122 bits of randomness each,
/// far more than an HMAC-SHA256 key needs - rather than pulling in a
/// dedicated `rand` dependency for this one call site). The same secret
/// then authenticates inbound requests to `/trigger` below - see
/// `insert_webhook`'s doc comment in `rebalancer-db`.
async fn create_webhook(
    State(pool): State<PgPool>,
    Path(portfolio_id): Path<Uuid>,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<(StatusCode, Json<WebhookResponse>), ApiError> {
    get_portfolio(&pool, portfolio_id).await?.ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "portfolio not found".into(),
    })?;
    if req.url.is_empty() {
        return Err(ApiError { status: StatusCode::BAD_REQUEST, message: "url must not be empty".into() });
    }

    let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let webhook = insert_webhook(&pool, portfolio_id, &req.url, &secret, &req.event_types).await?;
    Ok((StatusCode::CREATED, Json(webhook.into())))
}

#[derive(Debug, Serialize)]
struct TriggerResponse {
    id: Uuid,
    requested_at: chrono::DateTime<chrono::Utc>,
}

/// External trigger webhooks (PROJECT.md differentiator #8): lets a power
/// user's own system tell this backend "check this portfolio now"
/// instead of waiting on the scheduler's own drift/calendar polling.
/// Authenticated the same way outbound webhooks are, just in reverse -
/// the caller signs the raw request body with `X-Rebalancer-Signature:
/// sha256=<hmac>` using any secret from one of this portfolio's
/// registered webhooks (`rebalancer_notify::verify_signature`, the
/// receiving side of the exact scheme `WebhookClient::send` produces).
/// A body is optional; when present it must be a JSON object, and its
/// optional `reason` string field is pulled out for the trigger row's
/// own `reason` column while the whole object is kept as `payload`.
///
/// This never bypasses `vault`'s on-chain drift gate - it can't, that's
/// enforced in the contract. `rebalancer-scheduler::run_once` only uses a
/// claimed trigger to skip its own fee-aware cost deferral, so the
/// backend still won't submit a rebalance the chain would reject anyway.
async fn trigger_portfolio(
    State(pool): State<PgPool>,
    Path(portfolio_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<TriggerResponse>), ApiError> {
    get_portfolio(&pool, portfolio_id).await?.ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "portfolio not found".into(),
    })?;

    let webhooks = list_active_webhooks_for_portfolio(&pool, portfolio_id).await?;
    if webhooks.is_empty() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: "no webhook registered for this portfolio yet - register one via POST /portfolios/:id/webhooks first".into(),
        });
    }

    let signature = headers
        .get("X-Rebalancer-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError { status: StatusCode::UNAUTHORIZED, message: "missing X-Rebalancer-Signature header".into() })?;
    let authenticated = webhooks
        .iter()
        .any(|w| rebalancer_notify::verify_signature(&w.secret, &body, signature));
    if !authenticated {
        return Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "signature did not match any registered webhook secret for this portfolio".into(),
        });
    }

    let payload: serde_json::Value = if body.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: "body must be a JSON object".into(),
        })?
    };
    let reason = payload.get("reason").and_then(|v| v.as_str()).map(str::to_owned);

    let trigger = insert_external_trigger(&pool, portfolio_id, reason.as_deref(), payload).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(TriggerResponse { id: trigger.id, requested_at: trigger.requested_at }),
    ))
}

#[derive(Debug, Deserialize)]
struct SimulateRequest {
    /// Live balance for each target asset, keyed by `targets.asset` (the
    /// vault-held token's contract id - the same identifier
    /// `GET /portfolios/:id`'s `targets[].asset` already returns), as an
    /// `i128` string - JSON numbers can't carry that range safely,
    /// matching how `rebalance_events.trades` already serializes amounts
    /// elsewhere in this crate.
    balances: HashMap<String, String>,
    /// Live oracle price for each target asset, same keying and
    /// stringified-`i128` convention as `balances`, in whatever
    /// consistent fixed-point base the caller used to fetch both -
    /// `rebalancer_core::compute_allocation` only ever compares
    /// `balance * price` ratios, so the base itself doesn't matter here
    /// as long as it's the same one across every asset in one request.
    prices: HashMap<String, String>,
    /// Per-asset price shock, in bps of that asset's `prices` entry -
    /// negative for a drop (e.g. -3000 for "this asset's price falls
    /// 30%"), positive for a spike. Assets not present here are left at
    /// their live price. Balances are never shocked - a price move
    /// doesn't change what's actually held.
    #[serde(default)]
    shocks: HashMap<String, i32>,
}

#[derive(Debug, Serialize)]
struct SimulatedAllocationEntry {
    asset: String,
    target_weight_bps: u32,
    current_weight_bps: u32,
    drift_bps: i32,
}

impl From<rebalancer_core::AllocationEntry<String>> for SimulatedAllocationEntry {
    fn from(e: rebalancer_core::AllocationEntry<String>) -> Self {
        Self {
            asset: e.asset,
            target_weight_bps: e.target_weight_bps,
            current_weight_bps: e.current_weight_bps,
            drift_bps: e.drift_bps,
        }
    }
}

#[derive(Debug, Serialize)]
struct SimulatedTrade {
    asset_in: String,
    asset_out: String,
    amount_in: String,
}

impl From<rebalancer_core::TradeIntent<String>> for SimulatedTrade {
    fn from(t: rebalancer_core::TradeIntent<String>) -> Self {
        Self { asset_in: t.asset_in, asset_out: t.asset_out, amount_in: t.amount_in.to_string() }
    }
}

#[derive(Debug, Serialize)]
struct SimulateResponse {
    needs_rebalance: bool,
    allocation: Vec<SimulatedAllocationEntry>,
    /// The trades `rebalancer_core::compute_rebalance_trades` says would
    /// bring the portfolio back to target under the shocked prices -
    /// always empty when `needs_rebalance` is false. Nothing here ever
    /// gets submitted; this crate has no keeper key to submit it with in
    /// the first place (see this module's own doc comment).
    trades: Vec<SimulatedTrade>,
}

/// Shocks `price` by `shock_bps` (bps of `price`, signed) - floors at 0
/// rather than going negative, since a shock past -10000 bps ("more than
/// -100%") has no real-world meaning for a price.
fn apply_shock(price: i128, shock_bps: i32) -> i128 {
    let factor = 10_000i128 + shock_bps as i128;
    if factor <= 0 {
        return 0;
    }
    price.saturating_mul(factor) / 10_000
}

/// What-if stress simulator (PROJECT.md differentiator #9): "BTC drops
/// 30% tomorrow" - given the portfolio's real current balances/prices
/// (supplied by the caller, not fetched here - see this module's doc
/// comment) plus a per-asset price shock, projects the resulting drift
/// and the trades that would fire, without executing anything. Reuses
/// `rebalancer_core::{compute_allocation, needs_rebalance,
/// compute_rebalance_trades}` unchanged - the same functions
/// `rebalancer-scheduler` runs against real on-chain data every tick -
/// so a projection here reflects the live decision logic exactly, the
/// same fidelity guarantee `rebalancer-backtest` makes for historical
/// replay.
async fn simulate_portfolio(
    State(pool): State<PgPool>,
    Path(portfolio_id): Path<Uuid>,
    Json(req): Json<SimulateRequest>,
) -> Result<Json<SimulateResponse>, ApiError> {
    let portfolio = get_portfolio(&pool, portfolio_id).await?.ok_or_else(|| ApiError {
        status: StatusCode::NOT_FOUND,
        message: "portfolio not found".into(),
    })?;
    let targets = list_targets(&pool, portfolio_id).await?;

    let mut target_weights = Vec::with_capacity(targets.len());
    let mut states = Vec::with_capacity(targets.len());
    for t in &targets {
        let balance_str = req.balances.get(&t.asset).ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("missing balances[\"{}\"]", t.asset),
        })?;
        let balance: i128 = balance_str.parse().map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("balances[\"{}\"] is not a valid integer", t.asset),
        })?;
        let price_str = req.prices.get(&t.asset).ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("missing prices[\"{}\"]", t.asset),
        })?;
        let price: i128 = price_str.parse().map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("prices[\"{}\"] is not a valid integer", t.asset),
        })?;
        let shocked_price = apply_shock(price, req.shocks.get(&t.asset).copied().unwrap_or(0));

        target_weights.push(rebalancer_core::TargetWeight {
            asset: t.asset.clone(),
            weight_bps: t.weight_bps as u32,
        });
        states.push(rebalancer_core::AssetState { asset: t.asset.clone(), balance, price: shocked_price });
    }

    let allocation = rebalancer_core::compute_allocation(&target_weights, &states);
    let needs = rebalancer_core::needs_rebalance(&allocation, portfolio.threshold_bps as u32);
    let trades = if needs {
        rebalancer_core::compute_rebalance_trades(&target_weights, &states)
    } else {
        Vec::new()
    };

    Ok(Json(SimulateResponse {
        needs_rebalance: needs,
        allocation: allocation.into_iter().map(Into::into).collect(),
        trades: trades.into_iter().map(Into::into).collect(),
    }))
}

#[cfg(test)]
mod test;
