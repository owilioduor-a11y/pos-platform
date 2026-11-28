-- A completed M-Pesa STK intent may fund exactly one sale. Sales sync claims
-- the intent inside the sale transaction; a retry or a second sale reusing the
-- checkout id cannot claim it again.

ALTER TABLE mpesa_stk_intents
    ADD COLUMN IF NOT EXISTS consumed_by_sale_id UUID REFERENCES sales(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS consumed_at TIMESTAMPTZ;

CREATE UNIQUE INDEX IF NOT EXISTS idx_mpesa_intents_consumed
    ON mpesa_stk_intents (checkout_request_id)
    WHERE consumed_by_sale_id IS NOT NULL;
