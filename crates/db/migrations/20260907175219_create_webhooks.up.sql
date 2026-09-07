-- External trigger / notification webhooks (PROJECT.md differentiators
-- #7 audit log and #8 external triggers both read this). `secret` signs
-- our own outgoing payloads (HMAC) so the receiving endpoint can verify
-- authenticity - this is the same model Stripe/GitHub webhooks use, not
-- a credential we're protecting on the caller's behalf.
CREATE TABLE webhooks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    portfolio_id UUID NOT NULL REFERENCES portfolios (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    secret TEXT NOT NULL,
    event_types TEXT[] NOT NULL DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_webhooks_portfolio_id ON webhooks (portfolio_id);
