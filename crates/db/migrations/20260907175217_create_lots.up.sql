-- Cost-basis tracking (PROJECT.md differentiator #3). One row per
-- acquisition; realized gain/loss on a rebalance is computed by matching
-- disposed quantity against these rows (FIFO by acquired_at), not stored
-- here directly.
CREATE TABLE lots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id UUID NOT NULL REFERENCES portfolios (id) ON DELETE CASCADE,
    asset TEXT NOT NULL,
    qty NUMERIC(39, 0) NOT NULL CHECK (qty > 0),
    price NUMERIC(39, 0) NOT NULL CHECK (price > 0),
    acquired_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_lots_portfolio_id_asset_acquired_at
    ON lots (portfolio_id, asset, acquired_at);
