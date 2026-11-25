use anyhow::Context;
use pos_domain::catalog::Product;
use pos_domain::sale::Sale;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

/// Upsert a product into the local catalog cache.
pub async fn upsert_product(pool: &SqlitePool, product: &Product) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO products_cache
            (id, merchant_id, sku, barcode, name, price_minor, currency,
             tax_rate, stock_qty, active, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT (merchant_id, sku) DO UPDATE SET
            barcode   = excluded.barcode,
            name      = excluded.name,
            price_minor = excluded.price_minor,
            currency  = excluded.currency,
            tax_rate  = excluded.tax_rate,
            stock_qty = excluded.stock_qty,
            active    = excluded.active,
            updated_at = datetime('now')",
    )
    .bind(product.id.to_string())
    .bind(product.merchant_id.to_string())
    .bind(&product.sku)
    .bind(&product.barcode)
    .bind(&product.name)
    .bind(product.price.amount_minor)
    .bind(&product.price.currency.code)
    .bind(product.tax_rate.to_string())
    .bind(product.stock_qty.map(i64::from))
    .bind(i64::from(product.active))
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch a single cached product by id.
pub async fn get_product(
    pool: &SqlitePool,
    merchant_id: Uuid,
    product_id: Uuid,
) -> anyhow::Result<Option<Product>> {
    let row = sqlx::query(
        "SELECT id, merchant_id, sku, barcode, name, price_minor, currency,
                tax_rate, stock_qty, active
         FROM products_cache
         WHERE merchant_id = ? AND id = ? AND active = 1
         LIMIT 1",
    )
    .bind(merchant_id.to_string())
    .bind(product_id.to_string())
    .fetch_optional(pool)
    .await?;

    row.map(product_from_row).transpose()
}

/// Find a cached product by barcode for scanner-driven sales.
pub async fn find_product_by_barcode(
    pool: &SqlitePool,
    merchant_id: Uuid,
    barcode: &str,
) -> anyhow::Result<Option<Product>> {
    let row = sqlx::query(
        "SELECT id, merchant_id, sku, barcode, name, price_minor, currency,
                tax_rate, stock_qty, active
         FROM products_cache
         WHERE merchant_id = ? AND barcode = ? AND active = 1
         LIMIT 1",
    )
    .bind(merchant_id.to_string())
    .bind(barcode)
    .fetch_optional(pool)
    .await?;

    row.map(product_from_row).transpose()
}

fn product_from_row(row: SqliteRow) -> anyhow::Result<Product> {
    let price_minor: i64 = row.get("price_minor");
    let currency_code: String = row.get("currency");
    let currency =
        pos_domain::money::Currency::new(&currency_code, 2, "").context("invalid currency")?;
    let price = pos_domain::money::Money::new(price_minor, currency).context("invalid price")?;
    let tax_rate: String = row.get("tax_rate");
    let tax_rate = tax_rate
        .parse::<rust_decimal::Decimal>()
        .context("invalid tax rate")?;
    Ok(Product {
        id: row
            .get::<String, _>("id")
            .parse::<Uuid>()
            .context("invalid product id")?,
        merchant_id: row
            .get::<String, _>("merchant_id")
            .parse::<Uuid>()
            .context("invalid merchant id")?,
        sku: row.get("sku"),
        barcode: row.get("barcode"),
        name: row.get("name"),
        price,
        tax_rate,
        stock_qty: row.get::<Option<i64>, _>("stock_qty").map(|q| q as u32),
        active: row.get::<i64, _>("active") != 0,
    })
}

/// Persist a completed sale. Returns `true` if inserted, `false` if the
/// idempotency key already existed (retry-safe dedupe).
pub async fn insert_sale(pool: &SqlitePool, sale: &Sale) -> anyhow::Result<bool> {
    let payload = serde_json::to_string(sale).context("serialize sale")?;
    let result = sqlx::query(
        "INSERT OR IGNORE INTO sales
            (id, merchant_id, idempotency_key, payload_json, status)
         VALUES (?, ?, ?, ?, 'pending_sync')",
    )
    .bind(sale.id.to_string())
    .bind(sale.merchant_id.to_string())
    .bind(sale.idempotency_key.to_string())
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Enqueue an arbitrary payload (sale JSON or tap event JSON) for sync.
pub async fn enqueue_outbox(
    pool: &SqlitePool,
    kind: &str,
    payload_json: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query("INSERT INTO sync_outbox (kind, payload_json) VALUES (?, ?)")
        .bind(kind)
        .bind(payload_json)
        .execute(pool)
        .await?;
    Ok(result.last_insert_rowid())
}
