# Stellar Portfolio Rebalancer — Backend

Rust/axum backend. See `../PROJECT.md` for the full project plan.

## Layout

- `crates/core` (package `rebalancer-core`) — pure drift-calculation logic:
  target validation, current-vs-target weight, drift, and the threshold
  gate. Mirrors `contracts/contracts/vault`'s on-chain math exactly, so the
  scheduler and backtester (once built) reason about drift identically to
  what the contract actually enforces. No I/O, no async - just the math,
  so it's cheap to test exhaustively and safe to reuse from a backtest
  running over historical data.
- `crates/db` (package `rebalancer-db`) — Postgres schema (sqlx migrations)
  and typed row structs. `targets` mirrors `vault::TargetWeight` including
  the `asset`/`price_asset_*` split (the token actually held isn't always
  the same identifier the oracle prices it under - see the contracts repo
  history). No repository/query methods yet - nothing consumes them until
  `api`/`scheduler` exist in Phase 1, so there's no real interface to build
  against yet.

More crates (`api`, `oracle`, `chain`, `scheduler`, `notify`) land as later
build phases reach them - see `../PROJECT.md` section 5.

## Build & test

```sh
cargo test --workspace       # crates/db tests need a running Postgres, see below
cargo clippy --workspace --all-targets -- -D warnings
```

### Local Postgres for `crates/db`

Needs a database and a Postgres role matching your OS user (peer auth over
the local unix socket - no password):

```sh
sudo -u postgres createuser -s "$(whoami)"
sudo -u postgres createdb -O "$(whoami)" rebalancer_dev
cd crates/db
export DATABASE_URL="postgres://$(whoami)@%2Fvar%2Frun%2Fpostgresql/rebalancer_dev"
sqlx migrate run     # or: sqlx migrate revert, to roll one back
```

Copy `.env.example` to `.env` and adjust if your setup differs (a
docker-compose Postgres, a different user, etc.) - `crates/db`'s tests
default to the URL above if `DATABASE_URL` isn't set.

## Status

Phase 0 (foundations) done: `rebalancer-core` (13 tests) and `rebalancer-db`
(6 migrations + 4 integration tests, run against a real local Postgres).
Nothing else yet - no API, no chain client, no scheduler.
