CREATE TABLE portfolios (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Stellar contract address of the deployed vault instance for this
    -- portfolio, and the wallet address that owns it. Both are the
    -- authoritative identity - the chain, not this row, decides who can
    -- withdraw or reconfigure.
    vault_address TEXT NOT NULL UNIQUE,
    owner_address TEXT NOT NULL,
    name TEXT NOT NULL,
    -- Only 'threshold' exists today (see contracts/vault). 'calendar' and
    -- 'volatility_band' are Phase 3 work - a CHECK constraint keeps this
    -- easy to extend with a later migration, unlike a Postgres ENUM type.
    strategy_type TEXT NOT NULL DEFAULT 'threshold'
        CHECK (strategy_type IN ('threshold')),
    threshold_bps INTEGER NOT NULL
        CHECK (threshold_bps > 0 AND threshold_bps < 10000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_portfolios_owner_address ON portfolios (owner_address);
