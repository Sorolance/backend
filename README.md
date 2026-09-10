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
  history). Repository functions (`upsert_portfolio`,
  `insert_rebalance_event`, `insert_price_snapshot`) are added as real
  callers need them rather than guessed at ahead of time - `crates/scheduler`
  is the first, and so far only, consumer.

- `crates/oracle` (package `rebalancer-oracle`) — two independent price
  sources: `on_chain` reads the same `oracle_adapter` (Reflector,
  staleness-checked) the deployed `vault` itself trusts, via the `stellar`
  CLI; `coingecko` is a wholly separate off-chain source used to
  cross-check it. Deliberately kept as two separate clients, never merged
  into one "the price" abstraction - see the crate doc comment for the
  important scope note: CoinGecko is a fallback for *this backend's own
  observability*, never something the on-chain `rebalance` decision can
  fall back to (Soroban contracts can't reach an HTTP API).

- `crates/notify` (package `rebalancer-notify`) — webhook dispatch: signs
  the outgoing body with HMAC-SHA256 (`X-Rebalancer-Signature:
  sha256=<hex>`), the same model Stripe/GitHub webhooks use, and POSTs
  it. Deliberately just "sign and send" with no opinion about this
  project's own event types or which DB rows to notify - that shaping
  lives in `crates/scheduler/src/notifications.rs`. Email isn't built -
  it needs a real provider (SendGrid/Postmark/SES/...) and an API key,
  a decision only the project owner can make.

- `crates/scheduler` (package `rebalancer-scheduler`) — polls the deployed
  `vault` contract for drift on an interval and, once it's above
  threshold, computes the real trades needed to reach target, quotes each
  one against the deployed `router`, and submits a keeper-signed
  `rebalance` only if that's actually worth doing right now - fee-aware
  execution (Phase 4), see below - dispatching a `rebalance.completed`
  webhook on every genuinely new one recorded; also calls
  `vault::observe_risk` each tick regardless of whether a rebalance is
  imminent, dispatching `risk.circuit_breaker_tripped` if that call just
  tripped it; also polls both `rebalancer-oracle` sources for every
  configured asset each tick, records both into `price_snapshots`, and
  logs a warning if they've diverged past a configurable threshold
  (`PRICE_DIVERGENCE_WARN_BPS`). Shells out to the `stellar` CLI
  (`crates/scheduler/src/chain.rs`) rather than a hand-rolled Soroban
  RPC/XDR client, since there's no first-party Rust client for that and
  the CLI is exactly what this project's testnet deploys and smoke tests
  already use. Fixed-interval polling (`tokio::time::interval`), not the
  `tokio-cron-scheduler` originally sketched below - simpler, and a fixed
  interval is all Phase 1/2 actually needs; cron-style scheduling can come
  back if a real need for it (e.g. per-strategy schedules) shows up.

  **Fee-aware execution (Phase 4).** Before Phase 4, every `rebalance`
  call submitted `trades = []` regardless of drift - a real gap this item
  closed, not just an add-on: `run_once` now reads
  `vault::compute_allocation` for the live target weights + drift, fetches
  each target asset's real balance (`chain::token_balance`, SEP-41
  `balance`) and oracle price, and feeds them into
  `rebalancer_core::compute_rebalance_trades` to get the actual
  `TradeIntent`s needed to reach target - the same drift math `vault`
  itself enforces, generalized to emit real trades instead of just a
  boolean. Each trade is quoted for real against the router
  (`chain::quote_swap`), and its slippage vs. the oracle-implied fair
  price (`rebalancer_core::slippage_bps`) plus the live network fee
  (`chain::fee_stats`, Stellar's `p99` Soroban inclusion fee, converted to
  value terms via XLM's own price) combine into a total cost in bps of the
  trade's value (`rebalancer_core::total_cost_bps`). A rebalance whose
  worst-asset drift is far enough past threshold
  (`URGENT_DRIFT_MULTIPLIER`, default 2x) executes regardless of cost;
  otherwise it only executes if that cost is within
  `MAX_REBALANCE_COST_BPS` (default 50 bps), deferring to the next tick if
  not - the drift isn't lost, it's just re-evaluated next time, often
  cheaper (a calmer market) or urgent enough to override cost by then.
  "Batch small rebalances" (the other half of this PROJECT.md item) needs
  no separate logic: every trade `compute_rebalance_trades` returns for a
  tick already submits together in `vault::rebalance`'s one call, one
  network fee.

  Verified live against the deployed testnet vault: deposited a real,
  deliberately skewed 66/34 XLM/USDC split (630bps drift - past the 500bps
  threshold, but under the 1,000bps urgent line), and confirmed the
  computed single trade's real router quote showed ~50% slippage against
  the router's genuinely thin liquidity (its actual real-money constraint
  from the Phase 4 router work, not a fabricated test condition) -
  `total_cost_bps` came back ~5,038-5,052 across two runs, correctly
  deferred at the default 50bps budget (`deferring rebalance to next
  tick: not urgent and too costly right now`), then correctly executed
  once `MAX_REBALANCE_COST_BPS` was raised past that cost
  (`executing fee-aware rebalance ... urgent=false trades=1`) - a real
  signed `rebalance` landed on-chain, and the *next* tick's
  `compute_allocation` confirmed drift had actually dropped to ~194bps,
  under threshold (`drift below threshold, nothing to do`). Withdrew the
  test deposit back out afterward, confirmed the vault is empty again.

- `crates/backtest` (package `rebalancer-backtest`) — replays the
  threshold strategy against historical daily prices, reusing
  `rebalancer_core::{compute_allocation, needs_rebalance}` unchanged so a
  backtest reflects the same decision logic the live scheduler and
  on-chain `vault` use, not a parallel reimplementation. Sources history
  from `rebalancer_oracle::coingecko::CoinGeckoClient::market_chart`
  (added alongside this crate). Frictionless - no fees/slippage (Phase
  4) or cost-basis tracking (a separate Phase 3 item) modeled yet. Ships
  a `backtest` CLI bin since there's no `api` crate yet to expose
  `POST /portfolios/:id/backtest` for real.

More crates (`api`, `chain`) land as later build phases reach them - see
`../PROJECT.md` section 5.

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

Now that `ROUTER_CONTRACT_ID` is required and fee-aware execution
computes and submits real trades (see above), a rebalance only fails
closed with `RouterNotConfigured` if `set_router` was never called for
this vault - genuinely misconfigured, not the expected steady state
anymore. See `crates/scheduler/src/lib.rs` for a known on-chain quirk
still worth fixing in `contracts`, unrelated to fee-aware execution:
`needs_rebalance` currently reads a *totally empty* vault (zero balance in
every asset) as needing a rebalance too, so an unfunded vault will show up
here as constantly "due" until it's ever deposited into.

Each tick also polls Reflector (via `ORACLE_ADAPTER_CONTRACT_ID`) and
CoinGecko for every asset in `crates/scheduler/src/pricing.rs`'s fixed
asset list, logging `price cross-check ok`/`... have diverged` at
`INFO`/`WARN`. CoinGecko's public API rejects requests without a
descriptive User-Agent outright (not just harsher rate limiting) -
`CoinGeckoClient::new` sets one; if you see every `coingecko price read
failed` in a row, that's usually why. CoinGecko's rate limit is real
too (confirmed live during development) - occasional `coingecko price
read failed` warnings mentioning "Rate Limit" are expected under any
kind of rapid manual re-testing, not a bug; they're logged and the loop
carries on.

No onboarding API/UI exists yet to register a webhook, so there's
nothing to actually deliver in a normal local run - insert a row into
`webhooks` by hand (or see `crates/scheduler/src/notifications/test.rs`'s
`sends_a_real_signed_webhook`, an `#[ignore]`d test that exercises the
real dispatch path against a local HTTP listener, with the exact steps
to run it in its own doc comment) to see one for real.

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
(6 migrations + 10 integration tests, run against a real local Postgres).

Phase 1: `rebalancer-scheduler` done and verified live against the
deployed testnet vault (keeper authorized via `set_keeper`, drift
detection and rebalance submission both confirmed working end-to-end). No
API yet - the frontend still reads the contract directly.

Phase 2, all three items done:
- Pricing: `rebalancer-oracle` (10 tests), wired into the scheduler's
  tick, verified live - both sources agree closely for real (XLM 0 bps
  apart, USDC 3 bps apart, in one captured run).
- Circuit breaker + concentration limits (`risk_guard`) - see the
  `contracts` repo; this backend doesn't interact with `risk_guard`
  directly, the scheduler's `chain.rs` only ever calls `vault`, which
  calls `risk_guard` on its own (`vault::observe_risk`, called each tick
  from `crates/scheduler/src/lib.rs`'s `observe_risk_once`).
- Notifications: `rebalancer-notify` (3 tests) + `rebalancer-db`'s
  `list_active_webhooks` (1 test), wired into both the rebalance-complete
  and circuit-breaker-tripped paths, verified live against a real local
  HTTP listener with an independently-computed HMAC-SHA256 signature
  (Python's own `hmac` module, not this crate's code) confirmed valid.
  Email not built - needs a real provider/API key, a decision only the
  project owner can make.

Phase 3, all three off-chain items done:
- Backtesting engine: `rebalancer-backtest` + `rebalancer-oracle`'s
  `market_chart`. Verified live - `cargo run -p rebalancer-backtest
  --bin backtest -- 90` against real CoinGecko history for this
  project's 60/40 XLM/USDC, 500bps config produced 2 rebalances, both
  triggered right at the threshold (e.g. XLM -5.22% / USDC +5.21%),
  confirming the replay matches `rebalancer-core`'s actual gate rather
  than drifting from it.
- Calendar + volatility-band strategy templates: `rebalancer_core::Strategy`
  (`calendar_due`, `realized_volatility_bps`,
  `volatility_adjusted_threshold_bps`, `needs_rebalance_per_asset`),
  wired into `rebalancer-backtest`'s replay loop and selectable via the
  CLI's second argument (`threshold` (default) | `calendar` |
  `vol-band`). Verified live over 180 days of real XLM/USDC history:
  threshold triggered 4 times at ~5% drift, calendar triggered 5 times
  on a fixed 30-day cadence regardless of drift size, vol-band
  triggered 10 times at a narrower ~2-3% effective threshold consistent
  with the pair's realized volatility over the period. Both templates
  exist only here so far - no on-chain representation yet, see
  `contracts/contracts/strategy_registry`'s "not yet built" status in
  PROJECT.md.
- Cost-basis / tax-lot tracking: `rebalancer_core::dispose_fifo` (FIFO
  lot matching, mirrors `crates/db`'s `lots` table convention), wired
  into the backtester so every rebalance opens a lot for each
  net-bought asset and disposes FIFO for each net-sold one, reporting
  realized gain/loss per rebalance and as a report total. Verified live
  over the same 180-day threshold run: rebalances that trimmed
  appreciated XLM realized real gains (+$113.74, +$282.95), while ones
  disposing of USDC (barely moves in price) realized near-zero
  gain/loss (-$0.03, -$0.11) - matches the economics of what actually
  happened. Not yet wired into `rebalancer-db`'s `lots` table for real
  recording - no real caller wires it up yet (real trades do execute now,
  see the Phase 4 note below, but `run_once` doesn't record cost-basis
  lots for them - that's separate, still-unstarted work), consistent with
  this backend's convention of adding repository functions only once
  something actually needs them.

Phase 4 (backend half): fee-aware execution done - see the
`crates/scheduler` bullet above for what it does and its live
verification (real deposit, real quote, real deferral at the default cost
budget, real execution once raised, real on-chain drift drop confirmed on
the following tick, real withdrawal back out). Sub-portfolios and audit
log export remain, along with the `contracts` repo's still-open
empty-vault `needs_rebalance` quirk noted above.
