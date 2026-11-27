-- Pending M-Pesa STK Push intents. The Daraja callback arrives after the
-- initial API response, so the final result is reconciled against this row.

CREATE TABLE IF NOT EXISTS mpesa_stk_intents (
    id                   BIGSERIAL PRIMARY KEY,
    merchant_id          UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    idempotency_key      UUID NOT NULL UNIQUE,
    checkout_request_id  TEXT NOT NULL UNIQUE,
    merchant_request_id  TEXT,
    account_ref          TEXT NOT NULL,
    amount_minor         BIGINT NOT NULL CHECK (amount_minor > 0),
    currency             TEXT NOT NULL,
    phone_masked         TEXT NOT NULL,
    status               TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending', 'completed', 'failed')),
    result_code          BIGINT,
    receipt_number       TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_mpesa_intents_merchant
    ON mpesa_stk_intents (merchant_id, created_at);
