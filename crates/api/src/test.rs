//! Integration tests against a real local Postgres, same convention as
//! `rebalancer-db`'s own test module - drives `build_router` through real
//! HTTP request/response types (`tower::ServiceExt::oneshot`) rather than
//! calling handler functions directly, so a routing or (de)serialization
//! mistake fails here instead of only showing up against a live server.

use super::*;
use http_body_util::BodyExt;
use rebalancer_db::{connect, MIGRATOR};
use tower::ServiceExt;

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://yahia008@%2Fvar%2Frun%2Fpostgresql/rebalancer_dev".into());
    let pool = connect(&url).await.expect("connect to test database");
    MIGRATOR.run(&pool).await.expect("run migrations");
    pool
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

fn create_request(owner: &str, vault_address: &str) -> axum::http::Request<axum::body::Body> {
    let body = serde_json::json!({
        "vault_address": vault_address,
        "owner_address": owner,
        "name": "api test portfolio",
        "threshold_bps": 500,
        "targets": [
            {"asset": "CASSET_XLM", "price_asset_kind": "other", "price_asset_value": "XLM", "weight_bps": 6000},
            {"asset": "CASSET_USDC", "price_asset_kind": "other", "price_asset_value": "USDC", "weight_bps": 4000},
        ],
    });
    axum::http::Request::builder()
        .method("POST")
        .uri("/portfolios")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn create_list_get_round_trip() {
    let pool = test_pool().await;
    let owner = format!("GTEST_{}", Uuid::new_v4().simple());
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let response = build_router(pool.clone())
        .oneshot(create_request(&owner, &vault_address))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert_eq!(created["vault_address"], vault_address);
    assert_eq!(created["targets"].as_array().unwrap().len(), 2);
    let id = created["id"].as_str().unwrap().to_string();

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/portfolios?owner_address={owner}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body_json(response).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id);

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/portfolios/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = body_json(response).await;
    assert_eq!(fetched["vault_address"], vault_address);

    sqlx::query("DELETE FROM portfolios WHERE id = $1::uuid")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_rejects_duplicate_vault_address_with_409() {
    let pool = test_pool().await;
    let owner = format!("GTEST_{}", Uuid::new_v4().simple());
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let response = build_router(pool.clone())
        .oneshot(create_request(&owner, &vault_address))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let id = body_json(response).await["id"].as_str().unwrap().to_string();

    let response = build_router(pool.clone())
        .oneshot(create_request(&owner, &vault_address))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);

    sqlx::query("DELETE FROM portfolios WHERE id = $1::uuid")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_rejects_empty_targets_with_400() {
    let pool = test_pool().await;
    let body = serde_json::json!({
        "vault_address": format!("CTEST{}", Uuid::new_v4().simple()),
        "owner_address": "GTEST_OWNER",
        "name": "no targets",
        "threshold_bps": 500,
        "targets": [],
    });
    let response = build_router(pool)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/portfolios")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn report_csv_includes_header_and_recorded_events() {
    let pool = test_pool().await;
    let owner = format!("GTEST_{}", Uuid::new_v4().simple());
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let response = build_router(pool.clone())
        .oneshot(create_request(&owner, &vault_address))
        .await
        .unwrap();
    let id = body_json(response).await["id"].as_str().unwrap().to_string();
    let portfolio_id: Uuid = id.parse().unwrap();

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/portfolios/{id}/report.csv"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/csv"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let csv_before = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(csv_before, "tx_hash,executed_at,trades,fee_paid,slippage_bps\n");

    let tx_hash = format!("{}", Uuid::new_v4().simple());
    rebalancer_db::insert_rebalance_event(
        &pool,
        portfolio_id,
        &tx_hash,
        chrono::Utc::now(),
        serde_json::json!([]),
    )
    .await
    .expect("insert rebalance event")
    .expect("new row inserted");

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/portfolios/{id}/report.csv"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let csv_after = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(csv_after.contains(&tx_hash));

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

async fn register_portfolio(pool: &PgPool) -> (String, Uuid) {
    let owner = format!("GTEST_{}", Uuid::new_v4().simple());
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let response = build_router(pool.clone())
        .oneshot(create_request(&owner, &vault_address))
        .await
        .unwrap();
    let id = body_json(response).await["id"].as_str().unwrap().to_string();
    (id.clone(), id.parse().unwrap())
}

#[tokio::test]
async fn create_webhook_returns_a_usable_secret() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/webhooks"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({"url": "https://example.com/hook", "event_types": ["rebalance.completed"]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    assert_eq!(created["url"], "https://example.com/hook");
    assert!(created["secret"].as_str().unwrap().len() >= 32);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

async fn register_webhook(pool: &PgPool, portfolio_path_id: &str) -> String {
    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{portfolio_path_id}/webhooks"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({"url": "https://example.com/hook", "event_types": []}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(response).await["secret"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn trigger_with_a_correctly_signed_body_is_accepted_and_recorded() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;
    let secret = register_webhook(&pool, &id).await;

    let body = serde_json::json!({"reason": "custom_price_shock"}).to_string();
    let signature = sign(&secret, body.as_bytes());

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/trigger"))
                .header("content-type", "application/json")
                .header("X-Rebalancer-Signature", format!("sha256={signature}"))
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted = body_json(response).await;
    assert!(accepted["id"].as_str().is_some());

    let claimed = rebalancer_db::claim_pending_external_trigger(&pool, portfolio_id)
        .await
        .expect("claim")
        .expect("a trigger was recorded");
    assert_eq!(claimed.reason.as_deref(), Some("custom_price_shock"));

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn trigger_with_wrong_signature_is_rejected_with_401() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;
    register_webhook(&pool, &id).await;

    let body = serde_json::json!({}).to_string();
    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/trigger"))
                .header("content-type", "application/json")
                .header("X-Rebalancer-Signature", format!("sha256={}", sign("wrong-secret", body.as_bytes())))
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn trigger_with_no_registered_webhook_is_rejected_with_400() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;

    let body = serde_json::json!({}).to_string();
    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/trigger"))
                .header("content-type", "application/json")
                .header("X-Rebalancer-Signature", format!("sha256={}", sign("anything", body.as_bytes())))
                .body(axum::body::Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// The `create_request` fixture's targets: 60% `CASSET_XLM` / 40%
/// `CASSET_USDC`, threshold 500 bps - kept here rather than duplicated in
/// each test below.
fn on_target_simulate_body(shocks: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "balances": {"CASSET_XLM": "6000000000000", "CASSET_USDC": "4000000000000"},
        "prices": {"CASSET_XLM": "1000000", "CASSET_USDC": "1000000"},
        "shocks": shocks,
    })
}

#[tokio::test]
async fn simulate_on_target_with_no_shock_needs_no_rebalance() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/simulate"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(on_target_simulate_body(serde_json::json!({})).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body_json(response).await;
    assert_eq!(result["needs_rebalance"], false);
    assert_eq!(result["trades"].as_array().unwrap().len(), 0);
    assert_eq!(result["allocation"].as_array().unwrap().len(), 2);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn simulate_with_a_large_price_shock_projects_a_rebalance_and_trades() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;

    // XLM's price drops 30% - its value share falls well past the 500bps
    // threshold, so USDC becomes overweight and a trade should be
    // projected from USDC into XLM to bring the split back to 60/40.
    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/simulate"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    on_target_simulate_body(serde_json::json!({"CASSET_XLM": -3000})).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body_json(response).await;
    assert_eq!(result["needs_rebalance"], true);
    let trades = result["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0]["asset_in"], "CASSET_USDC");
    assert_eq!(trades[0]["asset_out"], "CASSET_XLM");

    let xlm_entry = result["allocation"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["asset"] == "CASSET_XLM")
        .unwrap();
    assert!(xlm_entry["drift_bps"].as_i64().unwrap() < 0, "shocked XLM should be underweight");

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn simulate_missing_an_assets_balance_is_rejected_with_400() {
    let pool = test_pool().await;
    let (id, portfolio_id) = register_portfolio(&pool).await;

    let response = build_router(pool.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/portfolios/{id}/simulate"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({
                        "balances": {"CASSET_XLM": "6000000000000"},
                        "prices": {"CASSET_XLM": "1000000", "CASSET_USDC": "1000000"},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn get_unknown_portfolio_returns_404() {
    let pool = test_pool().await;
    let response = build_router(pool)
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/portfolios/{}", Uuid::new_v4()))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
