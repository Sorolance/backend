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

- `crates/scheduler` (package `rebalancer-scheduler`) — polls the deployed
  `vault` contract for drift on an interval and submits a keeper-signed
  `rebalance` when it's above threshold. Shells out to the `stellar` CLI
  (`crates/scheduler/src/chain.rs`) rather than a hand-rolled Soroban
  RPC/XDR client, since there's no first-party Rust client for that and
  the CLI is exactly what this project's testnet deploys and smoke tests
  already use. Fixed-interval polling (`tokio::time::interval`), not the
  `tokio-cron-scheduler` originally sketched below - simpler, and a fixed
  interval is all Phase 1 actually needs; cron-style scheduling can come
  back if a real need for it (e.g. per-strategy schedules) shows up.

More crates (`api`, `oracle`, `chain`, `notify`) land as later build
phases reach them - see `../PROJECT.md` section 5.

### Running the scheduler locally

```sh
stellar keys generate rebalancer-keeper --network testnet --fund
stellar keys address rebalancer-keeper   # -> KEEPER_ADDRESS

# One-time: the vault owner must authorize the keeper before the
# scheduler can call `rebalance`. rebalancer-owner is the deployer
# identity from PROJECT.md's testnet deployment.
stellar contract invoke --id <VAULT_CONTRACT_ID> --source-account rebalancer-owner \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015" --send=yes \
  -- set_keeper --keeper <KEEPER_ADDRESS>

cp ../.env.example ../.env   # fill in KEEPER_IDENTITY/KEEPER_ADDRESS above
cargo run -p rebalancer-scheduler --bin rebalancer-scheduler
```

Until Phase 4 wires a router, every attempted `rebalance` fails closed
with `RouterNotConfigured` - that's expected, logged as a warning, and
does not crash the loop. See `crates/scheduler/src/lib.rs` for a known
on-chain quirk this surfaced: `needs_rebalance` currently reads an empty
vault as needing a rebalance too, so an empty/unfunded vault will show up
here as constantly "due" - harmless today, worth fixing in `contracts`
before Phase 4.

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
(6 migrations + 8 integration tests, run against a real local Postgres).

Phase 1: `rebalancer-scheduler` done and verified live against the
deployed testnet vault (keeper authorized via `set_keeper`, drift
detection and rebalance submission both confirmed working end-to-end;
every submission currently fails closed with `RouterNotConfigured` since
no router exists yet - see above). No API yet - the frontend still reads
the contract directly.
