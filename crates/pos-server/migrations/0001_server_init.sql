-- Cloud ledger schema for pos-server (PostgreSQL).
-- pos-core mirrors completed sales here via /api/v1/sync/sales;
-- idempotency_key is UNIQUE so retries never double-post.

CREATE TABLE IF NOT EXISTS merchants (
    id            UUID PRIMARY KEY,
    business_name TEXT NOT NULL,
    mode          TEXT NOT NULL CHECK (mode IN ('micro', 'retail')),
    currency_code TEXT NOT NULL DEFAULT 'KES',
    kra_pin       TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS users (
    id            UUID PRIMARY KEY,
    merchant_id   UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'cashier' CHECK (role IN ('owner', 'manager', 'cashier')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_users_merchant ON users (merchant_id);

CREATE TABLE IF NOT EXISTS products (
    id           UUID PRIMARY KEY,
    merchant_id  UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    sku          TEXT NOT NULL,
    barcode      TEXT,
    name         TEXT NOT NULL,
    price_minor  BIGINT NOT NULL CHECK (price_minor >= 0),
    currency     TEXT NOT NULL,
    tax_rate     NUMERIC(5,4) NOT NULL DEFAULT 0.16,
    stock_qty    INTEGER,
    active       BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (merchant_id, sku)
);

CREATE INDEX IF NOT EXISTS idx_products_barcode ON products (merchant_id, barcode);

CREATE TABLE IF NOT EXISTS sales (
    id              UUID PRIMARY KEY,
    merchant_id     UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    cashier_id      UUID REFERENCES users(id) ON DELETE SET NULL,
    idempotency_key UUID NOT NULL UNIQUE,
    payload_json    JSONB NOT NULL,
    status          TEXT NOT NULL DEFAULT 'completed'
                    CHECK (status IN ('pending', 'completed', 'voided')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sales_merchant ON sales (merchant_id, created_at);

-- Payment legs of each sale (split payments supported).
CREATE TABLE IF NOT EXISTS payments (
    id           BIGSERIAL PRIMARY KEY,
    sale_id      UUID NOT NULL REFERENCES sales(id) ON DELETE CASCADE,
    method       TEXT NOT NULL CHECK (method IN ('card_tap', 'mpesa', 'cash')),
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    currency     TEXT NOT NULL,
    reference    TEXT,
    provider     TEXT,
    status       TEXT NOT NULL DEFAULT 'approved',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_payments_sale ON payments (sale_id);
CREATE INDEX IF NOT EXISTS idx_payments_reference ON payments (reference);

-- Non-sale events synced from devices (recorded taps, inventory deltas).
CREATE TABLE IF NOT EXISTS sync_events (
    id           BIGSERIAL PRIMARY KEY,
    merchant_id  UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_sync_events_merchant ON sync_events (merchant_id, received_at);

-- Idempotent webhook dedupe for payment provider callbacks.
CREATE TABLE IF NOT EXISTS webhook_events (
    id           BIGSERIAL PRIMARY KEY,
    provider     TEXT NOT NULL,
    event_id     TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, event_id)
);
