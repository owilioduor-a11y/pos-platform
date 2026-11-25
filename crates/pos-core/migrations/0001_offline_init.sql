-- Offline-first local SQLite schema for pos-core.
-- Stores the catalog cache, completed sales, and the sync outbox.

CREATE TABLE IF NOT EXISTS products_cache (
    id            TEXT PRIMARY KEY,
    merchant_id   TEXT NOT NULL,
    sku           TEXT NOT NULL,
    barcode       TEXT,
    name          TEXT NOT NULL,
    price_minor   INTEGER NOT NULL CHECK (price_minor >= 0),
    currency      TEXT NOT NULL,
    tax_rate      TEXT NOT NULL DEFAULT '0.16',
    stock_qty     INTEGER,
    active        INTEGER NOT NULL DEFAULT 1,
    updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (merchant_id, sku)
);

CREATE INDEX IF NOT EXISTS idx_products_barcode
    ON products_cache (merchant_id, barcode);

-- Completed sales. `idempotency_key` is UNIQUE so retries never
-- create duplicate local records.
CREATE TABLE IF NOT EXISTS sales (
    id               TEXT PRIMARY KEY,
    merchant_id      TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL UNIQUE,
    payload_json     TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending_sync'
                     CHECK (status IN ('pending_sync', 'synced', 'failed')),
    attempts         INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Generic outbox for anything that must reach pos-server:
-- completed sales and recorded tap events.
CREATE TABLE IF NOT EXISTS sync_outbox (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT NOT NULL CHECK (kind IN ('sale', 'tap', 'inventory')),
    payload_json TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_outbox_created
    ON sync_outbox (created_at);
