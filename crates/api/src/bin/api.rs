use std::env;

use rebalancer_api::build_router;
use rebalancer_db::{connect, MIGRATOR};
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let bind_addr = env::var("API_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let pool = connect(&database_url).await.expect("connect to database");
    MIGRATOR.run(&pool).await.expect("run migrations");

    let app = build_router(pool);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("bind {bind_addr}: {e}"));
    info!(%bind_addr, "rebalancer-api listening");
    axum::serve(listener, app).await.expect("serve api");
}
