use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::extract::State;
use axum::Json;
use pos_domain::sale::{Sale, SplitPayment};
use pos_domain::PaymentMethod;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// Batch item pushed by pos-core's sync outbox.
#[derive(Debug, Deserialize)]
pub struct SyncItem {
    /// Outbox row id from the device, echoed back so the client can clear only
    /// rows the server actually accepted.
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct SyncRequest {
    pub items: Vec<SyncItem>,
}

#[derive(Debug, Serialize)]
pub struct SyncResponse {
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    /// Per-row outcome, enabling the device to drop accepted rows without
    /// leaving a rejected row wedged at the head of the outbox forever.
    pub results: Vec<SyncItemResult>,
}

#[derive(Debug, Serialize)]
pub struct SyncItemResult {
    pub id: i64,
    pub status: &'static str,
}

fn record_result(
    results: &mut Vec<SyncItemResult>,
    counter: &mut usize,
    id: i64,
    status: &'static str,
) {
    *counter += 1;
    results.push(SyncItemResult { id, status });
}

/// POST /api/v1/sync/sales — idempotent batch ingestion of completed sales.
/// `idempotency_key` UNIQUE constraint makes client retries safe.
pub async fn sync_sales(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<SyncRequest>,
) -> ApiResult<Json<SyncResponse>> {
    let pool = state.pool.clone();
    let mut accepted = 0usize;
    let mut duplicates = 0usize;
    let mut rejected = 0usize;
    let mut results = Vec::with_capacity(req.items.len());

    for item in req.items {
        if item.kind != "sale" {
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }
        let sale: Sale = match serde_json::from_value(item.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "unparseable sale payload");
                record_result(&mut results, &mut rejected, item.id, "rejected");
                continue;
            }
        };
        // Never trust client-supplied merchant/cashier ids over the token.
        if sale.merchant_id != user.merchant_id {
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }
        if sale.idempotency_key.get_version_num() != 7 {
            tracing::warn!(sale_id = %sale.id, "sale idempotency key is not UUIDv7");
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }
        if let Err(e) = sale.validate_payments() {
            tracing::warn!(sale_id = %sale.id, error = %e, "invalid sale payments");
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }
        if sale.items.iter().any(|line| line.quantity == 0) {
            tracing::warn!(sale_id = %sale.id, "sale contains a zero-quantity line");
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }

        match insert_sale(&pool, &sale, user.merchant_id, user.user_id).await {
            Ok(true) => record_result(&mut results, &mut accepted, item.id, "accepted"),
            Ok(false) => record_result(&mut results, &mut duplicates, item.id, "duplicate"),
            Err(e) => {
                tracing::warn!(error = %e, "sale insert failed");
                record_result(&mut results, &mut rejected, item.id, "rejected");
            }
        }
    }

    Ok(Json(SyncResponse {
        accepted,
        duplicates,
        rejected,
        results,
    }))
}

#[derive(Debug, PartialEq, Eq)]
struct StkIntentStatus {
    status: String,
    idempotency_key: Uuid,
    amount_minor: i64,
    currency: String,
}

/// A client-supplied checkout id only counts as evidence of payment when the
/// intent belongs to this sale attempt, matches the leg exactly, and has
/// settled. Unsettled or already-consumed intents are not sale evidence.
fn mpesa_payment_status(
    intent: Option<StkIntentStatus>,
    sale_idempotency_key: Uuid,
    leg: &SplitPayment,
) -> ApiResult<&'static str> {
    let Some(intent) = intent else {
        return Err(ApiError::BadRequest(
            "M-Pesa reference has no STK intent for this merchant".into(),
        ));
    };
    if intent.idempotency_key != sale_idempotency_key {
        return Err(ApiError::BadRequest(
            "M-Pesa intent belongs to a different payment attempt".into(),
        ));
    }
    if intent.amount_minor != leg.amount.amount_minor || intent.currency != leg.amount.currency.code
    {
        return Err(ApiError::BadRequest(
            "M-Pesa intent does not match the payment leg".into(),
        ));
    }
    match intent.status.as_str() {
        "completed" => Ok("approved"),
        "failed" => Err(ApiError::BadRequest("M-Pesa intent failed".into())),
        _ => Err(ApiError::BadRequest(
            "M-Pesa intent has not completed".into(),
        )),
    }
}

/// Insert a sale + its payment legs atomically.
/// Returns false on idempotency conflict, with no partial rows written.
async fn insert_sale(
    pool: &PgPool,
    sale: &Sale,
    merchant_id: Uuid,
    cashier_id: Uuid,
) -> ApiResult<bool> {
    let payload = serde_json::to_value(sale)?;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await?;

    let result = sqlx::query(
        "INSERT INTO sales (id, merchant_id, cashier_id, idempotency_key, payload_json, status)
         VALUES ($1, $2, $3, $4, $5, 'completed')
         ON CONFLICT (idempotency_key) DO NOTHING",
    )
    .bind(sale.id)
    .bind(merchant_id)
    .bind(cashier_id)
    .bind(sale.idempotency_key)
    .bind(payload)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(false);
    }

    // The server is the stock authority. A sale cannot make cloud stock
    // negative, and the UNIQUE sale key prevents a retry from deducting twice.
    for item in &sale.items {
        let quantity = i64::from(item.quantity);
        let tracked_stock = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT stock_qty FROM products
             WHERE id = $1 AND merchant_id = $2 AND active = TRUE",
        )
        .bind(item.product.id)
        .bind(merchant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(format!("unknown product for SKU {}", item.product.sku))
        })?;
        // NULL means stock is not tracked for this product.
        if let Some(available) = tracked_stock {
            if available < quantity {
                return Err(ApiError::BadRequest(format!(
                    "insufficient stock for SKU {}",
                    item.product.sku
                )));
            }
            let updated = sqlx::query(
                "UPDATE products
                 SET stock_qty = stock_qty - $1, updated_at = now()
                 WHERE id = $2 AND merchant_id = $3 AND active = TRUE
                   AND stock_qty IS NOT NULL AND stock_qty >= $1",
            )
            .bind(quantity)
            .bind(item.product.id)
            .bind(merchant_id)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 0 {
                return Err(ApiError::BadRequest(format!(
                    "stock changed while syncing SKU {}",
                    item.product.sku
                )));
            }
            sqlx::query(
                "INSERT INTO stock_movements (merchant_id, sale_id, product_id, quantity)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(merchant_id)
            .bind(sale.id)
            .bind(item.product.id)
            .bind(quantity)
            .execute(&mut *tx)
            .await?;
        }
    }

    for payment in &sale.payments {
        // Claim the STK intent inside the sale transaction. Only a settled
        // intent with an unconsumed checkout id can back a completed sale.
        let status = if payment.method == PaymentMethod::Mpesa {
            let intent = sqlx::query_as::<_, (String, Uuid, i64, String)>(
                "UPDATE mpesa_stk_intents
                 SET consumed_by_sale_id = $1, consumed_at = now()
                 WHERE checkout_request_id = $2 AND merchant_id = $3
                   AND consumed_by_sale_id IS NULL
                 RETURNING status, idempotency_key, amount_minor, currency",
            )
            .bind(sale.id)
            .bind(&payment.reference)
            .bind(merchant_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(
                |(status, idempotency_key, amount_minor, currency)| StkIntentStatus {
                    status,
                    idempotency_key,
                    amount_minor,
                    currency,
                },
            );
            mpesa_payment_status(intent, sale.idempotency_key, payment)?
        } else {
            "approved"
        };
        sqlx::query(
            "INSERT INTO payments (sale_id, method, amount_minor, currency, reference, status)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(sale.id)
        .bind(method_str(payment.method))
        .bind(payment.amount.amount_minor)
        .bind(&payment.amount.currency.code)
        .bind(&payment.reference)
        .bind(status)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

fn method_str(method: PaymentMethod) -> &'static str {
    match method {
        PaymentMethod::CardTap => "card_tap",
        PaymentMethod::Mpesa => "mpesa",
        PaymentMethod::Cash => "cash",
    }
}

/// POST /api/v1/sync/events — non-sale events (tap audit records,
/// inventory deltas). Stored verbatim for downstream processing.
pub async fn sync_events(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<SyncRequest>,
) -> ApiResult<Json<SyncResponse>> {
    let pool = state.pool.clone();
    let mut accepted = 0usize;
    let duplicates = 0usize;
    let mut rejected = 0usize;
    let mut results = Vec::with_capacity(req.items.len());
    for item in req.items {
        if !matches!(item.kind.as_str(), "tap" | "inventory") {
            // Sales must go through /sync/sales; other kinds are unknown.
            tracing::warn!(kind = %item.kind, "rejecting unsupported sync event");
            record_result(&mut results, &mut rejected, item.id, "rejected");
            continue;
        }
        let result = sqlx::query(
            "INSERT INTO sync_events (merchant_id, kind, payload_json) VALUES ($1, $2, $3)",
        )
        .bind(user.merchant_id)
        .bind(&item.kind)
        .bind(item.payload)
        .execute(&pool)
        .await;
        match result {
            Ok(_) => record_result(&mut results, &mut accepted, item.id, "accepted"),
            Err(e) => {
                tracing::warn!(error = %e, kind = %item.kind, "event insert failed");
                record_result(&mut results, &mut rejected, item.id, "rejected");
            }
        }
    }
    Ok(Json(SyncResponse {
        accepted,
        duplicates,
        rejected,
        results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::money::{Currency, Money};

    fn intent(
        status: &str,
        idempotency_key: Uuid,
        amount_minor: i64,
        currency: &str,
    ) -> StkIntentStatus {
        StkIntentStatus {
            status: status.into(),
            idempotency_key,
            amount_minor,
            currency: currency.into(),
        }
    }

    fn mpesa_leg(amount_minor: i64, currency: Currency) -> SplitPayment {
        SplitPayment::new(
            PaymentMethod::Mpesa,
            Money::new(amount_minor, currency).unwrap(),
            "ws_CO_1",
        )
        .unwrap()
    }

    #[test]
    fn mpesa_leg_requires_a_settled_matching_intent() {
        let leg = mpesa_leg(100_000, Currency::kes());
        let key = Uuid::now_v7();
        assert_eq!(
            mpesa_payment_status(Some(intent("completed", key, 100_000, "KES")), key, &leg)
                .unwrap(),
            "approved"
        );
        assert!(
            mpesa_payment_status(Some(intent("failed", key, 100_000, "KES")), key, &leg).is_err()
        );
        assert!(
            mpesa_payment_status(Some(intent("pending", key, 100_000, "KES")), key, &leg).is_err()
        );
    }

    #[test]
    fn sync_results_track_each_outbox_row() {
        let mut results = Vec::new();
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        record_result(&mut results, &mut accepted, 7, "accepted");
        record_result(&mut results, &mut rejected, 9, "rejected");
        assert_eq!(accepted, 1);
        assert_eq!(rejected, 1);
        assert_eq!(results[0].id, 7);
        assert_eq!(results[1].status, "rejected");
    }

    #[test]
    fn mpesa_leg_rejects_unknown_or_mismatched_intent() {
        let leg = mpesa_leg(100_000, Currency::kes());
        let key = Uuid::now_v7();
        assert!(mpesa_payment_status(None, key, &leg).is_err());
        assert!(
            mpesa_payment_status(Some(intent("completed", key, 10_000, "KES")), key, &leg).is_err()
        );
        assert!(
            mpesa_payment_status(Some(intent("completed", key, 100_000, "USD")), key, &leg)
                .is_err()
        );
        assert!(mpesa_payment_status(
            Some(intent("completed", Uuid::now_v7(), 100_000, "KES")),
            key,
            &leg
        )
        .is_err());
    }
}
