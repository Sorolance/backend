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

More crates (`api`, `oracle`, `chain`, `scheduler`, `notify`, `db`) land as
later build phases reach them - see `../PROJECT.md` section 5.

## Build & test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Status

Phase 0 (foundations): `rebalancer-core` implemented and unit-tested (13
tests). Nothing else yet - no API, no chain client, no database.
