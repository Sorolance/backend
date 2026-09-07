-- One row per on-chain rebalance() call. `trades` is the same shape as
-- contracts/vault::TradeInstruction (asset_in, asset_out, amount_in,
-- min_amount_out) plus whatever the actual fill was, once Phase 4 wires a
-- router - kept as JSONB rather than a child table since it's small,
-- fixed-per-event, and never queried independently of its parent event.
CREATE TABLE rebalance_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id UUID NOT NULL REFERENCES portfolios (id) ON DELETE CASCADE,
    tx_hash TEXT NOT NULL,
    executed_at TIMESTAMPTZ NOT NULL,
    trades JSONB NOT NULL DEFAULT '[]',
    -- Stroop-denominated amounts stored as NUMERIC rather than BIGINT:
    -- the contracts use i128, and NUMERIC(39,0) covers that full range
    -- without risking silent truncation on an unusually large amount.
    fee_paid NUMERIC(39, 0),
    slippage_bps INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tx_hash)
);

CREATE INDEX idx_rebalance_events_portfolio_id_executed_at
    ON rebalance_events (portfolio_id, executed_at DESC);
