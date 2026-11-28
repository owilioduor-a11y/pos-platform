-- An approved SoftPOS tap audit event may back exactly one card sale. The
-- server cannot see acquirer approval directly, so sales sync must correlate
-- with a stored tap event and consume it once.

ALTER TABLE sync_events
    ADD COLUMN IF NOT EXISTS consumed_by_sale_id UUID REFERENCES sales(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_sync_events_tap_consumption
    ON sync_events (merchant_id, kind, consumed_by_sale_id, id);
