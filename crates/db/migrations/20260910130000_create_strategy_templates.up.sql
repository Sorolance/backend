-- Copy strategies (PROJECT.md differentiator #10): a portfolio owner can
-- opt in to publish their portfolio's target allocation as a public,
-- anonymized template other users can browse and clone. Deliberately
-- carries no link back to the source portfolio, vault, or owner - just a
-- point-in-time snapshot of targets + threshold - so "anonymized" holds
-- even under a full-table read, not just at the API response layer.
CREATE TABLE strategy_templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    threshold_bps INTEGER NOT NULL
        CHECK (threshold_bps > 0 AND threshold_bps < 10000),
    -- Same shape as a NewTarget array ([{asset, price_asset_kind,
    -- price_asset_value, weight_bps}, ...]) - a snapshot, not a live
    -- reference, so publishing never breaks if the source portfolio is
    -- later changed or its targets updated.
    targets JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Serves the "browse" list, newest first - the only query pattern this
-- table needs today.
CREATE INDEX idx_strategy_templates_created_at ON strategy_templates (created_at DESC);
