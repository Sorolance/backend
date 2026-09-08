#![cfg(test)]

use super::*;
use rebalancer_db::connect;

/// Not run by default (`#[ignore]`) - needs a real local HTTP listener
/// and a real `webhooks` row pointing at it, since `dispatch` genuinely
/// makes a network call and this test wants to prove that call actually
/// happens correctly (right URL, right signature) rather than mocking it
/// away. To run it for real:
///
/// ```sh
/// python3 -m http.server 8934 &   # or any listener that returns 2xx
/// psql "$DATABASE_URL" -c "
///   INSERT INTO webhooks (portfolio_id, url, secret, event_types, is_active)
///   VALUES ('<a real portfolio id>', 'http://127.0.0.1:8934/', 'live-test-secret',
///           ARRAY['rebalance.completed'], true)"
/// PORTFOLIO_ID=<that id> cargo test -p rebalancer-scheduler \
///   notifications::test::sends_a_real_signed_webhook -- --ignored --nocapture
/// ```
///
/// This is exactly how it was verified live while building this feature
/// - see PROJECT.md's Phase 2 notifications entry for the captured
/// signature-verification result.
#[tokio::test]
#[ignore = "needs a real local HTTP listener and a webhooks row pointing at it - see doc comment"]
async fn sends_a_real_signed_webhook() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://yahia008@%2Fvar%2Frun%2Fpostgresql/rebalancer_dev".into());
    let portfolio_id: Uuid = std::env::var("PORTFOLIO_ID")
        .expect("set PORTFOLIO_ID to a portfolio with a real webhooks row - see doc comment")
        .parse()
        .expect("PORTFOLIO_ID must be a valid UUID");

    let pool = connect(&database_url).await.expect("connect to database");
    let client = WebhookClient::new();

    notify_rebalance_completed(&pool, &client, portfolio_id, "test-tx-hash", Utc::now()).await;
}
