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
