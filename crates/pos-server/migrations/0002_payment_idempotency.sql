-- Server-side idempotency ledger for payment attempts.
-- The row is reserved BEFORE the provider call so simultaneous retries cannot
-- both reach the gateway. A retry reads the stored outcome instead.

CREATE TABLE IF NOT EXISTS payment_attempts (
    id               BIGSERIAL PRIMARY KEY,
    merchant_id      UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    idempotency_key  UUID NOT NULL,
    method           TEXT NOT NULL CHECK (method IN ('card_tap', 'mpesa')),
    request_hash     TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'in_progress'
                     CHECK (status IN ('in_progress', 'completed', 'failed')),
    response_json    JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (merchant_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_payment_attempts_merchant
    ON payment_attempts (merchant_id, status, created_at);
