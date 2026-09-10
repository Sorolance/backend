#![cfg(test)]

use super::*;
use bigdecimal::BigDecimal;
use std::str::FromStr;

/// Integration test against a real local Postgres - needs `DATABASE_URL`
/// set, or falls back to the local dev database created for this project
/// (see backend/README.md). Verifies the row structs actually match the
/// schema (a FromRow mismatch fails here, not silently at runtime later)
/// and that the constraints mirrored from the on-chain vault contract
/// (weight_bps range, no duplicate asset per portfolio) hold for real.
async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://yahia008@%2Fvar%2Frun%2Fpostgresql/rebalancer_dev".into());
    let pool = connect(&url).await.expect("connect to test database");
    MIGRATOR.run(&pool).await.expect("run migrations");
    pool
}

#[tokio::test]
async fn portfolio_and_targets_round_trip() {
    let pool = test_pool().await;
    let mut tx = pool.begin().await.unwrap();

    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio: Portfolio = sqlx::query_as(
        "INSERT INTO portfolios (vault_address, owner_address, name, threshold_bps)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(&vault_address)
    .bind("GTEST_OWNER")
    .bind("round trip test")
    .bind(500i32)
    .fetch_one(&mut *tx)
    .await
    .expect("insert portfolio");

    assert_eq!(portfolio.vault_address, vault_address);
    assert_eq!(portfolio.strategy_type, "threshold");
    assert_eq!(portfolio.threshold_bps, 500);

    let target: Target = sqlx::query_as(
        "INSERT INTO targets (portfolio_id, asset, price_asset_kind, price_asset_value, weight_bps)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING *",
    )
    .bind(portfolio.id)
    .bind("CASSET_XLM")
    .bind("other")
    .bind("XLM")
    .bind(6_000i32)
    .fetch_one(&mut *tx)
    .await
    .expect("insert target");

    assert_eq!(target.portfolio_id, portfolio.id);
    assert_eq!(target.weight_bps, 6_000);

    // Rolled back, not committed - this test never leaves data behind.
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn duplicate_asset_per_portfolio_is_rejected() {
    let pool = test_pool().await;
    let mut tx = pool.begin().await.unwrap();

    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio: Portfolio = sqlx::query_as(
        "INSERT INTO portfolios (vault_address, owner_address, name, threshold_bps)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(&vault_address)
    .bind("GTEST_OWNER")
    .bind("duplicate asset test")
    .bind(500i32)
    .fetch_one(&mut *tx)
    .await
    .expect("insert portfolio");

    let insert_target = |asset: &'static str, weight: i32| {
        sqlx::query(
            "INSERT INTO targets (portfolio_id, asset, price_asset_kind, price_asset_value, weight_bps)
             VALUES ($1, $2, 'other', $3, $4)",
        )
        .bind(portfolio.id)
        .bind(asset)
        .bind(asset)
        .bind(weight)
    };

    insert_target("CASSET_XLM", 6_000)
        .execute(&mut *tx)
        .await
        .expect("first insert for this asset succeeds");

    let err = insert_target("CASSET_XLM", 1_000)
        .execute(&mut *tx)
        .await
        .expect_err("duplicate asset for the same portfolio must be rejected");
    assert!(matches!(err, sqlx::Error::Database(_)));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn weight_bps_out_of_range_is_rejected() {
    let pool = test_pool().await;
    let mut tx = pool.begin().await.unwrap();

    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio: Portfolio = sqlx::query_as(
        "INSERT INTO portfolios (vault_address, owner_address, name, threshold_bps)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(&vault_address)
    .bind("GTEST_OWNER")
    .bind("bad weight test")
    .bind(500i32)
    .fetch_one(&mut *tx)
    .await
    .expect("insert portfolio");

    let err = sqlx::query(
        "INSERT INTO targets (portfolio_id, asset, price_asset_kind, price_asset_value, weight_bps)
         VALUES ($1, 'CASSET_XLM', 'other', 'XLM', $2)",
    )
    .bind(portfolio.id)
    .bind(10_001i32) // > 10_000
    .execute(&mut *tx)
    .await
    .expect_err("weight_bps above 10_000 must be rejected");
    assert!(matches!(err, sqlx::Error::Database(_)));

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn price_snapshot_numeric_round_trips_full_i128_range() {
    let pool = test_pool().await;
    let mut tx = pool.begin().await.unwrap();

    // Larger than i64/BIGINT can hold, well within i128 range - this is
    // exactly why price_snapshots.price is NUMERIC(39,0), not BIGINT.
    let big_price = BigDecimal::from_str("99999999999999999999999999999999999999").unwrap();

    let snapshot: PriceSnapshot = sqlx::query_as(
        "INSERT INTO price_snapshots (asset_kind, asset_value, price, source, observed_at)
         VALUES ('other', 'XLM', $1, 'reflector', now())
         RETURNING *",
    )
    .bind(&big_price)
    .fetch_one(&mut *tx)
    .await
    .expect("insert price snapshot");

    assert_eq!(snapshot.price, big_price);

    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn upsert_portfolio_inserts_then_updates_in_place() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let first = upsert_portfolio(&pool, &vault_address, "GOWNER_OLD", "first name", 500)
        .await
        .expect("first upsert inserts");
    assert_eq!(first.owner_address, "GOWNER_OLD");
    assert_eq!(first.name, "first name");
    assert_eq!(first.threshold_bps, 500);

    let second = upsert_portfolio(&pool, &vault_address, "GOWNER_NEW", "second name", 800)
        .await
        .expect("second upsert updates the same row");
    assert_eq!(second.id, first.id, "same vault_address must be the same row");
    assert_eq!(second.owner_address, "GOWNER_NEW");
    assert_eq!(second.name, "second name");
    assert_eq!(second.threshold_bps, 800);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_rebalance_event_is_idempotent_on_tx_hash() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio = upsert_portfolio(&pool, &vault_address, "GOWNER", "idempotency test", 500)
        .await
        .expect("upsert portfolio");

    let tx_hash = format!("{}", Uuid::new_v4().simple());
    let trades = serde_json::json!([]);

    let first = insert_rebalance_event(&pool, portfolio.id, &tx_hash, Utc::now(), trades.clone())
        .await
        .expect("first insert succeeds")
        .expect("first insert returns the new row");
    assert_eq!(first.tx_hash, tx_hash);
    assert_eq!(first.portfolio_id, portfolio.id);

    let second = insert_rebalance_event(&pool, portfolio.id, &tx_hash, Utc::now(), trades)
        .await
        .expect("retry does not error");
    assert!(
        second.is_none(),
        "duplicate tx_hash must be a no-op, not a second row"
    );

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_price_snapshot_allows_multiple_rows_per_asset() {
    let pool = test_pool().await;
    let asset_value = format!("TEST{}", Uuid::new_v4().simple());
    let now = Utc::now();

    let reflector = insert_price_snapshot(
        &pool,
        "other",
        &asset_value,
        BigDecimal::from_str("18808876553202").unwrap(),
        "reflector",
        Some(123_456),
        now,
    )
    .await
    .expect("insert reflector snapshot");
    assert_eq!(reflector.source, "reflector");
    assert_eq!(reflector.ledger_seq, Some(123_456));

    // No uniqueness constraint - a second source's observation for the
    // same asset at the same moment must not collide with the first.
    let coingecko = insert_price_snapshot(
        &pool,
        "other",
        &asset_value,
        BigDecimal::from_str("18849700000000").unwrap(),
        "coingecko",
        None,
        now,
    )
    .await
    .expect("insert coingecko snapshot");
    assert_eq!(coingecko.source, "coingecko");
    assert!(coingecko.ledger_seq.is_none());
    assert_ne!(reflector.id, coingecko.id);

    sqlx::query("DELETE FROM price_snapshots WHERE id IN ($1, $2)")
        .bind(reflector.id)
        .bind(coingecko.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_portfolio_with_targets_writes_both_in_one_call() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let portfolio = insert_portfolio_with_targets(
        &pool,
        &vault_address,
        "GOWNER",
        "sub-portfolio test",
        500,
        &[
            NewTarget {
                asset: "CASSET_XLM".into(),
                price_asset_kind: "other".into(),
                price_asset_value: "XLM".into(),
                weight_bps: 6_000,
            },
            NewTarget {
                asset: "CASSET_USDC".into(),
                price_asset_kind: "other".into(),
                price_asset_value: "USDC".into(),
                weight_bps: 4_000,
            },
        ],
    )
    .await
    .expect("insert portfolio with targets");

    let targets = list_targets(&pool, portfolio.id)
        .await
        .expect("list targets");
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|t| t.asset == "CASSET_XLM" && t.weight_bps == 6_000));
    assert!(targets.iter().any(|t| t.asset == "CASSET_USDC" && t.weight_bps == 4_000));

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_portfolio_with_targets_rejects_duplicate_vault_address() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let targets = [NewTarget {
        asset: "CASSET_XLM".into(),
        price_asset_kind: "other".into(),
        price_asset_value: "XLM".into(),
        weight_bps: 10_000,
    }];

    let first = insert_portfolio_with_targets(&pool, &vault_address, "GOWNER", "first", 500, &targets)
        .await
        .expect("first registration succeeds");

    let err = insert_portfolio_with_targets(&pool, &vault_address, "GOWNER", "second", 500, &targets)
        .await
        .expect_err("re-registering the same vault_address must fail");
    assert!(matches!(err, sqlx::Error::Database(_)));

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn list_portfolios_by_owner_and_list_rebalance_events() {
    let pool = test_pool().await;
    let owner = format!("GTEST_{}", Uuid::new_v4().simple());
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());

    let portfolio = upsert_portfolio(&pool, &vault_address, &owner, "list test", 500)
        .await
        .expect("upsert portfolio");

    let mine = list_portfolios_by_owner(&pool, &owner)
        .await
        .expect("list portfolios by owner");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, portfolio.id);

    assert!(get_portfolio(&pool, portfolio.id)
        .await
        .expect("get portfolio")
        .is_some());

    let tx_hash = format!("{}", Uuid::new_v4().simple());
    insert_rebalance_event(&pool, portfolio.id, &tx_hash, Utc::now(), serde_json::json!([]))
        .await
        .expect("insert rebalance event")
        .expect("new row inserted");

    let events = list_rebalance_events(&pool, portfolio.id)
        .await
        .expect("list rebalance events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].tx_hash, tx_hash);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_webhook_then_list_active_webhooks_for_portfolio_finds_it() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio = upsert_portfolio(&pool, &vault_address, "GOWNER", "trigger test", 500)
        .await
        .expect("upsert portfolio");

    let webhook = insert_webhook(
        &pool,
        portfolio.id,
        "https://example.com/hook",
        "shh-its-a-secret",
        &["rebalance.completed".to_string()],
    )
    .await
    .expect("insert webhook");
    assert_eq!(webhook.secret, "shh-its-a-secret");

    let active = list_active_webhooks_for_portfolio(&pool, portfolio.id)
        .await
        .expect("list active webhooks");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, webhook.id);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn claim_pending_external_trigger_claims_oldest_unprocessed_then_stops() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio = upsert_portfolio(&pool, &vault_address, "GOWNER", "trigger test", 500)
        .await
        .expect("upsert portfolio");

    // Nothing pending yet.
    assert!(claim_pending_external_trigger(&pool, portfolio.id)
        .await
        .expect("claim with nothing pending")
        .is_none());

    let first = insert_external_trigger(
        &pool,
        portfolio.id,
        Some("price_shock"),
        serde_json::json!({"reason": "price_shock"}),
    )
    .await
    .expect("insert first trigger");
    let _second = insert_external_trigger(&pool, portfolio.id, None, serde_json::json!({}))
        .await
        .expect("insert second trigger");

    // Claims the oldest one first, marks it processed.
    let claimed = claim_pending_external_trigger(&pool, portfolio.id)
        .await
        .expect("claim first trigger")
        .expect("a trigger was pending");
    assert_eq!(claimed.id, first.id);
    assert_eq!(claimed.reason.as_deref(), Some("price_shock"));
    assert!(claimed.processed_at.is_some());

    // A second call gets the other one, not the same row again.
    let claimed_again = claim_pending_external_trigger(&pool, portfolio.id)
        .await
        .expect("claim second trigger")
        .expect("a second trigger was pending");
    assert_ne!(claimed_again.id, first.id);

    // Both now processed - nothing left to claim.
    assert!(claim_pending_external_trigger(&pool, portfolio.id)
        .await
        .expect("claim with nothing left pending")
        .is_none());

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn insert_strategy_template_then_list_and_get_round_trip() {
    let pool = test_pool().await;
    let targets = serde_json::json!([
        {"asset": "CASSET_XLM", "price_asset_kind": "other", "price_asset_value": "XLM", "weight_bps": 6000},
        {"asset": "CASSET_USDC", "price_asset_kind": "other", "price_asset_value": "USDC", "weight_bps": 4000},
    ]);
    let template = insert_strategy_template(&pool, "60/40 XLM-USDC", 500, targets.clone())
        .await
        .expect("insert strategy template");
    assert_eq!(template.name, "60/40 XLM-USDC");
    assert_eq!(template.targets, targets);

    let listed = list_strategy_templates(&pool).await.expect("list strategy templates");
    assert!(listed.iter().any(|t| t.id == template.id));

    let fetched = get_strategy_template(&pool, template.id)
        .await
        .expect("get strategy template")
        .expect("template exists");
    assert_eq!(fetched.id, template.id);

    assert!(get_strategy_template(&pool, Uuid::new_v4())
        .await
        .expect("get unknown template")
        .is_none());

    sqlx::query("DELETE FROM strategy_templates WHERE id = $1")
        .bind(template.id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn list_active_webhooks_filters_by_event_type_and_active_flag() {
    let pool = test_pool().await;
    let vault_address = format!("CTEST{}", Uuid::new_v4().simple());
    let portfolio = upsert_portfolio(&pool, &vault_address, "GOWNER", "webhook test", 500)
        .await
        .expect("upsert portfolio");

    let matching: Uuid = sqlx::query_scalar(
        "INSERT INTO webhooks (portfolio_id, url, secret, event_types, is_active)
         VALUES ($1, 'https://example.com/a', 'secret-a', ARRAY['rebalance.completed'], true)
         RETURNING id",
    )
    .bind(portfolio.id)
    .fetch_one(&pool)
    .await
    .expect("insert matching webhook");

    // Wrong event type - must not match.
    sqlx::query(
        "INSERT INTO webhooks (portfolio_id, url, secret, event_types, is_active)
         VALUES ($1, 'https://example.com/b', 'secret-b', ARRAY['risk.circuit_breaker_tripped'], true)",
    )
    .bind(portfolio.id)
    .execute(&pool)
    .await
    .expect("insert non-matching event type webhook");

    // Right event type, but inactive - must not match.
    sqlx::query(
        "INSERT INTO webhooks (portfolio_id, url, secret, event_types, is_active)
         VALUES ($1, 'https://example.com/c', 'secret-c', ARRAY['rebalance.completed'], false)",
    )
    .bind(portfolio.id)
    .execute(&pool)
    .await
    .expect("insert inactive webhook");

    let results = list_active_webhooks(&pool, portfolio.id, "rebalance.completed")
        .await
        .expect("list webhooks");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, matching);

    sqlx::query("DELETE FROM portfolios WHERE id = $1")
        .bind(portfolio.id)
        .execute(&pool)
        .await
        .unwrap();
}
