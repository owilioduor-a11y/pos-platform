-- Cloud stock ledger. Sales sync decrements products.stock_qty and records an
-- immutable movement so every inventory delta can be explained by a sale.

CREATE TABLE IF NOT EXISTS stock_movements (
    id          BIGSERIAL PRIMARY KEY,
    merchant_id UUID NOT NULL REFERENCES merchants(id) ON DELETE CASCADE,
    sale_id     UUID NOT NULL REFERENCES sales(id) ON DELETE CASCADE,
    product_id  UUID NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    quantity    BIGINT NOT NULL CHECK (quantity > 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (sale_id, product_id)
);

CREATE INDEX IF NOT EXISTS idx_stock_movements_merchant
    ON stock_movements (merchant_id, created_at);
