-- External trigger webhooks (PROJECT.md differentiator #8): lets a power
-- user's own system POST a custom trigger condition instead of waiting on
-- this backend's drift/calendar checks alone. Authenticated inbound via
-- the same HMAC secret a portfolio's `webhooks` row already carries (see
-- rebalancer-api's POST /portfolios/:id/trigger) - one secret serves both
-- directions rather than a second credential type.
--
-- A trigger never bypasses `vault`'s own on-chain drift gate - it can't,
-- that's enforced in the contract itself. It only overrides this
-- backend's fee-aware cost deferral, treating the next check as urgent -
-- see rebalancer-scheduler::run_once's force_urgent parameter.
CREATE TABLE external_triggers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id UUID NOT NULL REFERENCES portfolios (id) ON DELETE CASCADE,
    reason TEXT,
    payload JSONB NOT NULL DEFAULT '{}',
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ
);

-- Serves the scheduler's "any pending trigger for this portfolio?" poll -
-- a partial index since processed_at IS NULL rows are always a small
-- minority once the scheduler is running.
CREATE INDEX idx_external_triggers_pending
    ON external_triggers (portfolio_id, requested_at)
    WHERE processed_at IS NULL;
