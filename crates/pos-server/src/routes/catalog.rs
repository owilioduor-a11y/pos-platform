use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::extract::{Path, State};
use axum::Json;
use pos_domain::money::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct UpsertProductRequest {
    pub sku: String,
    pub barcode: Option<String>,
    pub name: String,
    pub price_minor: i64,
    pub currency_code: Option<String>,
    pub tax_rate: Option<String>,
    pub stock_qty: Option<i64>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStockRequest {
    /// New absolute count from the caller's physical count.
    pub stock_qty: i64,
    /// Count the caller saw before editing. Prevents stale writes from
    /// overwriting sales that synced in the meantime.
    pub expected_stock_qty: i64,
}

#[derive(Debug, Serialize)]
pub struct ProductResponse {
    pub id: String,
    pub sku: String,
    pub barcode: Option<String>,
    pub name: String,
    pub price_minor: i64,
    pub currency: String,
    pub tax_rate: String,
    pub stock_qty: Option<i64>,
    pub active: bool,
}

fn validate_tax_rate(rate: Decimal) -> ApiResult<Decimal> {
    if rate < Decimal::ZERO || rate > Decimal::ONE {
        return Err(ApiError::BadRequest(format!(
            "tax_rate must be between 0 and 1; got {rate}"
        )));
    }
    Ok(rate)
}

fn validate_stock_qty(qty: Option<i64>) -> ApiResult<Option<i64>> {
    if let Some(q) = qty {
        if q < 0 || q > i64::from(u32::MAX) {
            return Err(ApiError::BadRequest(format!(
                "stock_qty must be between 0 and {}; got {q}",
                u32::MAX
            )));
        }
    }
    Ok(qty)
}

fn row_to_product(row: &sqlx::postgres::PgRow) -> ApiResult<ProductResponse> {
    Ok(ProductResponse {
        id: row.get::<Uuid, _>("id").to_string(),
        sku: row.get("sku"),
        barcode: row.get("barcode"),
        name: row.get("name"),
        price_minor: row.get("price_minor"),
        currency: row.get("currency"),
        tax_rate: row.get::<String, _>("tax_rate"),
        stock_qty: row.get("stock_qty"),
        active: row.get("active"),
    })
}

/// GET /api/v1/catalog/products — list the merchant's catalog.
pub async fn list_products(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ProductResponse>>> {
    let pool = state.pool.clone();
    let rows = sqlx::query(
        "SELECT id, sku, barcode, name, price_minor, currency, tax_rate::text AS tax_rate, stock_qty, active
         FROM products WHERE merchant_id = $1 AND active = TRUE
         ORDER BY name",
    )
    .bind(user.merchant_id)
    .fetch_all(&pool)
    .await?;
    Ok(Json(
        rows.iter().map(row_to_product).collect::<ApiResult<_>>()?,
    ))
}

/// POST /api/v1/catalog/products — upsert a product (catalog push from HQ
/// or device-initiated creation). Idempotent on (merchant_id, sku).
pub async fn upsert_product(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<UpsertProductRequest>,
) -> ApiResult<Json<ProductResponse>> {
    let pool = state.pool.clone();
    if req.sku.trim().is_empty() || req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("sku and name are required".into()));
    }
    if req.price_minor < 0 {
        return Err(ApiError::BadRequest("price_minor must be >= 0".into()));
    }
    let currency = req
        .currency_code
        .clone()
        .unwrap_or_else(|| "KES".into())
        .to_ascii_uppercase();
    Currency::new(&currency, 2, "").map_err(ApiError::from)?;
    let tax_rate = match &req.tax_rate {
        Some(raw) => validate_tax_rate(
            raw.parse::<Decimal>()
                .map_err(|_| ApiError::BadRequest(format!("invalid tax_rate '{raw}'")))?,
        )?,
        None => Decimal::new(16, 2),
    };
    let stock_qty = validate_stock_qty(req.stock_qty)?;

    let row = sqlx::query(
        "INSERT INTO products
            (id, merchant_id, sku, barcode, name, price_minor, currency, tax_rate, stock_qty, active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::numeric, $9, $10)
         ON CONFLICT (merchant_id, sku) DO UPDATE SET
            barcode    = excluded.barcode,
            name       = excluded.name,
            price_minor = excluded.price_minor,
            currency   = excluded.currency,
            tax_rate   = excluded.tax_rate,
            stock_qty  = excluded.stock_qty,
            active     = excluded.active,
            updated_at = now()
         RETURNING id, sku, barcode, name, price_minor, currency, tax_rate::text AS tax_rate, stock_qty, active",
    )
    .bind(Uuid::now_v7())
    .bind(user.merchant_id)
    .bind(req.sku.trim())
    .bind(req.barcode.as_deref().map(str::trim).filter(|b| !b.is_empty()))
    .bind(req.name.trim())
    .bind(req.price_minor)
    .bind(&currency)
    .bind(tax_rate.to_string())
    .bind(stock_qty)
    .bind(req.active.unwrap_or(true))
    .fetch_one(&pool)
    .await?;

    Ok(Json(row_to_product(&row)?))
}

/// PATCH /api/v1/catalog/products/:id/stock — manual count with optimistic
/// concurrency control. A stale absolute count cannot overwrite stock changes
/// already applied by synced sales.
pub async fn update_stock(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateStockRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    validate_stock_qty(Some(req.stock_qty))?;
    validate_stock_qty(Some(req.expected_stock_qty))?;

    let current: Option<i64> =
        sqlx::query_scalar("SELECT stock_qty FROM products WHERE id = $1 AND merchant_id = $2")
            .bind(id)
            .bind(user.merchant_id)
            .fetch_optional(&pool)
            .await?
            .flatten();
    if current.is_none() {
        return Err(ApiError::NotFound);
    }
    if current != Some(req.expected_stock_qty) {
        return Err(ApiError::Conflict(
            "stock changed since it was read; refresh and retry".into(),
        ));
    }

    let result = sqlx::query(
        "UPDATE products SET stock_qty = $1, updated_at = now()
         WHERE id = $2 AND merchant_id = $3 AND stock_qty = $4",
    )
    .bind(req.stock_qty)
    .bind(id)
    .bind(user.merchant_id)
    .bind(req.expected_stock_qty)
    .execute(&pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "stock changed since it was read; refresh and retry".into(),
        ));
    }
    Ok(Json(json!({ "updated": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tax_rate_must_be_a_fraction() {
        assert!(validate_tax_rate(Decimal::ZERO).is_ok());
        assert!(validate_tax_rate(Decimal::new(16, 2)).is_ok());
        assert_eq!(validate_tax_rate(Decimal::ONE).unwrap(), Decimal::ONE);
        assert!(validate_tax_rate(Decimal::new(-1, 2)).is_err());
        assert!(validate_tax_rate(Decimal::new(150, 2)).is_err());
    }

    #[test]
    fn stock_qty_must_fit_domain_u32() {
        assert!(validate_stock_qty(None).is_ok());
        assert_eq!(validate_stock_qty(Some(0)).unwrap(), Some(0));
        assert_eq!(
            validate_stock_qty(Some(i64::from(u32::MAX))).unwrap(),
            Some(i64::from(u32::MAX))
        );
        assert!(validate_stock_qty(Some(-1)).is_err());
        assert!(validate_stock_qty(Some(i64::from(u32::MAX) + 1)).is_err());
    }
}
