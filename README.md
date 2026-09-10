# Stellar Portfolio Rebalancer — Backend

Rust/axum backend for a non-custodial DeFi portfolio rebalancer on
Stellar. Watches prices, computes drift against the deployed
[`vault` contract](../contracts), and submits fee-aware rebalances signed
by a keeper key that can never withdraw funds or change targets.

See [`../PROJECT.md`](../PROJECT.md) for the full project plan.

## Architecture

| Crate | Package | Responsibility |
|---|---|---|
| `crates/core` | `rebalancer-core` | Pure drift/rebalance/risk math — target validation, drift calculation, trade computation, fee-aware cost/urgency evaluation, FIFO cost-basis lots. No I/O, mirrors the on-chain `vault`'s math exactly. |
| `crates/db` | `rebalancer-db` | Postgres schema (sqlx migrations) and repository functions. A read cache for dashboards/history — the chain remains the source of truth. |
| `crates/oracle` | `rebalancer-oracle` | Two independent price sources: on-chain Reflector (the same feed the deployed `vault` trusts) and CoinGecko (an off-chain cross-check only — never a fallback for the on-chain rebalance decision, since a Soroban contract can't reach an HTTP API). |
| `crates/notify` | `rebalancer-notify` | HMAC-SHA256-signed webhook dispatch (`X-Rebalancer-Signature`, the Stripe/GitHub model). |
| `crates/scheduler` | `rebalancer-scheduler` | Polls the vault for drift, computes and fee-gates rebalances, submits them, and dispatches notifications. The main service — see [Fee-aware execution](#fee-aware-execution) below. |
| `crates/backtest` | `rebalancer-backtest` | Replays a strategy (threshold, calendar, or volatility-band) against historical daily prices, reusing `rebalancer-core`'s live decision logic, with FIFO cost-basis tracking. Ships as a CLI (`backtest`) pending the `api` crate. |

`api` (an HTTP layer for the frontend, currently reading contracts
directly) is a planned but not-yet-built crate — see
[`../PROJECT.md`](../PROJECT.md).

## Fee-aware execution

Before submitting a rebalance, `rebalancer-scheduler`:

1. Computes the real trades needed to reach target, from each asset's
   live balance and oracle price (`rebalancer_core::compute_rebalance_trades`).
2. Quotes every trade against the deployed router and measures its
   slippage against the oracle-implied fair price
   (`rebalancer_core::slippage_bps`).
3. Combines that slippage with the live Stellar network fee
   (`stellar fees stats`) into a total cost, in bps of the trade's own
   value (`rebalancer_core::total_cost_bps`).
4. Executes immediately if drift is far enough past threshold to count
   as urgent; otherwise only executes if that cost is within budget,
   deferring to the next tick if not
   (`rebalancer_core::evaluate_fee_aware_execution`).

Every trade computed for a tick submits together in one `vault.rebalance`
call — batching falls out of this for free, no separate logic needed.
Tunable via `MAX_REBALANCE_COST_BPS`, `URGENT_DRIFT_MULTIPLIER`, and
`EXECUTION_SLIPPAGE_BUFFER_BPS` (see [Configuration](#configuration)).

## Getting Started

### Prerequisites

- Rust (stable)
- PostgreSQL
- [`stellar` CLI](https://developers.stellar.org/docs/tools/developer-tools/cli/stellar-cli)

### Configuration

Copy `.env.example` to `.env` and fill in the required values:

| Variable | Required | Description |
|---|:---:|---|
| `DATABASE_URL` | ✅ | Postgres connection string. |
| `RPC_URL`, `NETWORK_PASSPHRASE` | | Default to Stellar testnet. |
| `VAULT_CONTRACT_ID`, `ORACLE_ADAPTER_CONTRACT_ID`, `ROUTER_CONTRACT_ID` | ✅ | Deployed contract addresses — see [`../contracts/README.md`](../contracts/README.md). |
| `OWNER_ADDRESS` | ✅ | The vault owner's address (mirrored into `portfolios` for display; the contract remains the source of truth for authorization). |
| `KEEPER_IDENTITY`, `KEEPER_ADDRESS` | ✅ | A `stellar keys` identity name (not a raw secret) authorized via `set_keeper`, and its address. |
| `THRESHOLD_BPS`, `POLL_INTERVAL_SECS`, `PRICE_DIVERGENCE_WARN_BPS` | | Optional, sensible defaults — see `crates/scheduler/src/config.rs`. |
| `MAX_REBALANCE_COST_BPS`, `URGENT_DRIFT_MULTIPLIER`, `EXECUTION_SLIPPAGE_BUFFER_BPS` | | Fee-aware execution tuning — default to 50 bps, 2x, and 50 bps respectively. |

### Database

Needs a Postgres role matching your OS user (peer auth over the local
Unix socket — no password):

```sh
sudo -u postgres createuser -s "$(whoami)"
sudo -u postgres createdb -O "$(whoami)" rebalancer_dev
cd crates/db && sqlx migrate run     # or: sqlx migrate revert
```

### Running the scheduler

```sh
stellar keys generate rebalancer-keeper --network testnet --fund
stellar keys address rebalancer-keeper   # -> KEEPER_ADDRESS

# One-time: the vault owner must authorize the keeper before it can call
# `rebalance`.
stellar contract invoke --id <VAULT_CONTRACT_ID> --source-account <owner-identity> \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015" --send=yes \
  -- set_keeper --keeper <KEEPER_ADDRESS>

cargo run -p rebalancer-scheduler --bin rebalancer-scheduler
```

A rebalance fails closed with `RouterNotConfigured` only if `set_router`
was never called for the vault. A totally empty (zero-balance) vault will
show up as constantly "due" — a known on-chain quirk in
`vault::needs_rebalance`, tracked in [`../PROJECT.md`](../PROJECT.md).

## Testing

```sh
cargo test --workspace       # crates/db's tests need the Postgres setup above
cargo clippy --workspace --all-targets -- -D warnings
```

## Notes

- CoinGecko's public API rejects requests without a descriptive
  `User-Agent` outright, and rate-limits real usage — both handled and
  logged, not bugs if you see them.
- No onboarding API/UI exists yet to register a webhook; insert a row
  into `webhooks` by hand, or see
  `crates/scheduler/src/notifications/test.rs`'s `sends_a_real_signed_webhook`
  (an `#[ignore]`d test with the exact steps to run it against a local
  listener).

## Status

| Phase | Scope | Status |
|---|---|---|
| 0 | `rebalancer-core`, `rebalancer-db` | Done |
| 1 | `rebalancer-scheduler` (drift detection + submission) | Done — live-verified |
| 2 | Pricing cross-check, circuit breaker, webhook notifications | Done — live-verified |
| 3 | Backtesting engine, calendar/volatility-band strategies, cost-basis tracking | Done — live-verified over real historical data |
| 4 | Fee-aware execution | Done — live-verified (real deposit, real deferral, real execution, real drift drop confirmed on-chain) |
| 4 | Sub-portfolios, audit log export | Not started |

See [`../PROJECT.md`](../PROJECT.md) for the full build log and every
live-network verification behind these results.
