use anyhow::Context;
use pos_domain::catalog::Product;
use pos_domain::sale::Sale;
use rust_decimal::Decimal;
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
    let tax_rate = tax_rate.parse::<Decimal>().context("invalid tax rate")?;
    if tax_rate < Decimal::ZERO || tax_rate > Decimal::ONE {
        anyhow::bail!("tax rate out of range: {tax_rate}");
    }
    let stock_qty = row
        .get::<Option<i64>, _>("stock_qty")
        .map(|q| u32::try_from(q).context("invalid stock_qty"))
        .transpose()?;
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
        stock_qty,
        active: row.get::<i64, _>("active") != 0,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockShortage {
    pub sku: String,
    pub requested: u32,
    pub available: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckoutCommit {
    Inserted,
    Duplicate(Uuid),
    StockShortage(StockShortage),
}

async fn sale_id_for_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    idempotency_key: Uuid,
) -> anyhow::Result<Option<Uuid>> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM sales WHERE idempotency_key = ? LIMIT 1")
            .bind(idempotency_key.to_string())
            .fetch_optional(&mut **tx)
            .await?;
    id.map(|raw| raw.parse::<Uuid>().context("invalid sale id"))
        .transpose()
}

/// Atomically deduct stock, persist the sale and enqueue it for sync.
/// A duplicate idempotency key short-circuits before any stock is deducted.
pub async fn commit_checkout(
    pool: &SqlitePool,
    sale: &Sale,
    payload_json: &str,
    stock_requests: &[(Uuid, u32)],
) -> anyhow::Result<CheckoutCommit> {
    let mut tx: sqlx::Transaction<'_, sqlx::Sqlite> = pool.begin().await?;

    if let Some(existing_id) = sale_id_for_key(&mut tx, sale.idempotency_key).await? {
        return Ok(CheckoutCommit::Duplicate(existing_id));
    }

    for (product_id, requested) in stock_requests {
        let row: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT sku, stock_qty FROM products_cache
             WHERE id = ? AND merchant_id = ? AND active = 1",
        )
        .bind(product_id.to_string())
        .bind(sale.merchant_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((sku, stock)) = row else {
            anyhow::bail!("product {product_id} is not in the active catalog");
        };
        let Some(stock) = stock else {
            // NULL means stock is not tracked for this product.
            continue;
        };
        let available = u32::try_from(stock).context("invalid stock_qty")?;
        if available < *requested {
            return Ok(CheckoutCommit::StockShortage(StockShortage {
                sku,
                requested: *requested,
                available,
            }));
        }
        let updated = sqlx::query(
            "UPDATE products_cache SET stock_qty = stock_qty - ?
             WHERE id = ? AND merchant_id = ? AND active = 1 AND stock_qty >= ?",
        )
        .bind(i64::from(*requested))
        .bind(product_id.to_string())
        .bind(sale.merchant_id.to_string())
        .bind(i64::from(*requested))
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT stock_qty FROM products_cache
                 WHERE id = ? AND merchant_id = ? AND active = 1",
            )
            .bind(product_id.to_string())
            .bind(sale.merchant_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            return Ok(CheckoutCommit::StockShortage(StockShortage {
                sku,
                requested: *requested,
                available: current.and_then(|q| u32::try_from(q).ok()).unwrap_or(0),
            }));
        }
    }

    let result = sqlx::query(
        "INSERT OR IGNORE INTO sales
            (id, merchant_id, idempotency_key, payload_json, status)
         VALUES (?, ?, ?, ?, 'pending_sync')",
    )
    .bind(sale.id.to_string())
    .bind(sale.merchant_id.to_string())
    .bind(sale.idempotency_key.to_string())
    .bind(payload_json)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        let existing_id = sale_id_for_key(&mut tx, sale.idempotency_key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("duplicate sale idempotency key has no row"))?;
        return Ok(CheckoutCommit::Duplicate(existing_id));
    }

    sqlx::query("INSERT INTO sync_outbox (kind, payload_json) VALUES ('sale', ?)")
        .bind(payload_json)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(CheckoutCommit::Inserted)
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
