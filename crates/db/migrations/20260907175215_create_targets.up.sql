-- Mirrors contracts/vault::TargetWeight, including the price_asset split
-- (see contracts repo history: "vault: decouple custodied asset from
-- oracle pricing key") - the token actually held (`asset`) is not always
-- the same identifier the oracle prices it under (`price_asset_*`).
CREATE TABLE targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id UUID NOT NULL REFERENCES portfolios (id) ON DELETE CASCADE,
    asset TEXT NOT NULL,
    price_asset_kind TEXT NOT NULL CHECK (price_asset_kind IN ('stellar', 'other')),
    price_asset_value TEXT NOT NULL,
    weight_bps INTEGER NOT NULL CHECK (weight_bps > 0 AND weight_bps <= 10000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (portfolio_id, asset)
);

CREATE INDEX idx_targets_portfolio_id ON targets (portfolio_id);
