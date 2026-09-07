-- Not tied to a portfolio - prices are shared across every portfolio that
-- happens to hold the same asset. `asset_kind`/`asset_value` mirror
-- oracle_common::Asset (Stellar(Address) | Other(Symbol)); `price` is the
-- raw oracle-native fixed-point value (e.g. 14 decimals for Reflector),
-- not normalized here.
CREATE TABLE price_snapshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_kind TEXT NOT NULL CHECK (asset_kind IN ('stellar', 'other')),
    asset_value TEXT NOT NULL,
    price NUMERIC(39, 0) NOT NULL,
    source TEXT NOT NULL,
    -- NULL for off-chain fallback sources (e.g. CoinGecko) that have no
    -- Stellar ledger sequence to attach.
    ledger_seq BIGINT,
    observed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Serves "latest price for this asset" lookups - the query the scheduler
-- and dashboard both run most often.
CREATE INDEX idx_price_snapshots_asset_observed_at
    ON price_snapshots (asset_kind, asset_value, observed_at DESC);
