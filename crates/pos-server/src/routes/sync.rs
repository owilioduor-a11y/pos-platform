use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::extract::State;
use axum::Json;
use pos_domain::sale::{Sale, SplitPayment};
use pos_domain::PaymentMethod;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// Batch item pushed by pos-core's sync outbox.
#[derive(Debug, Deserialize)]
pub struct SyncItem {
    /// Outbox row id from the device — used for tracing only; dedupe is
    /// handled by the sales.idempotency_key UNIQUE constraint.
    #[allow(dead_code)]
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

    for item in req.items {
        if item.kind != "sale" {
            rejected += 1;
            continue;
        }
        let sale: Sale = match serde_json::from_value(item.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "unparseable sale payload");
                rejected += 1;
                continue;
            }
        };
        // Never trust client-supplied merchant/cashier ids over the token.
        if sale.merchant_id != user.merchant_id {
            rejected += 1;
            continue;
        }
        if sale.idempotency_key.get_version_num() != 7 {
            tracing::warn!(sale_id = %sale.id, "sale idempotency key is not UUIDv7");
            rejected += 1;
            continue;
        }
        if let Err(e) = sale.validate_payments() {
            tracing::warn!(sale_id = %sale.id, error = %e, "invalid sale payments");
            rejected += 1;
            continue;
        }

        match insert_sale(&pool, &sale, user.merchant_id, user.user_id).await {
            Ok(true) => accepted += 1,
            Ok(false) => duplicates += 1,
            Err(e) => {
                tracing::warn!(error = %e, "sale insert failed");
                rejected += 1;
            }
        }
    }

    Ok(Json(SyncResponse {
        accepted,
        duplicates,
        rejected,
    }))
}

#[derive(Debug, PartialEq, Eq)]
struct StkIntentStatus {
    status: String,
    amount_minor: i64,
    currency: String,
}

/// A client-supplied checkout id only counts as evidence of payment when the
/// intent belongs to the authenticated merchant and matches the leg exactly.
fn mpesa_payment_status(
    intent: Option<StkIntentStatus>,
    leg: &SplitPayment,
) -> ApiResult<&'static str> {
    let Some(intent) = intent else {
        return Err(ApiError::BadRequest(
            "M-Pesa reference has no STK intent for this merchant".into(),
        ));
    };
    if intent.amount_minor != leg.amount.amount_minor || intent.currency != leg.amount.currency.code
    {
        return Err(ApiError::BadRequest(
            "M-Pesa intent does not match the payment leg".into(),
        ));
    }
    Ok(match intent.status.as_str() {
        "completed" => "approved",
        "failed" => "failed",
        _ => "pending",
    })
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

    for payment in &sale.payments {
        // M-Pesa STK Push is confirmed asynchronously; carry the reconciled
        // intent status forward if a callback already arrived.
        let status = if payment.method == PaymentMethod::Mpesa {
            let intent = sqlx::query_as::<_, (String, i64, String)>(
                "SELECT status, amount_minor, currency FROM mpesa_stk_intents
                 WHERE checkout_request_id = $1 AND merchant_id = $2",
            )
            .bind(&payment.reference)
            .bind(merchant_id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|(status, amount_minor, currency)| StkIntentStatus {
                status,
                amount_minor,
                currency,
            });
            mpesa_payment_status(intent, payment)?
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
) -> ApiResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for item in req.items {
        if !matches!(item.kind.as_str(), "tap" | "inventory") {
            // Sales must go through /sync/sales; other kinds are unknown.
            tracing::warn!(kind = %item.kind, "rejecting unsupported sync event");
            rejected += 1;
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
            Ok(_) => accepted += 1,
            Err(e) => {
                tracing::warn!(error = %e, kind = %item.kind, "event insert failed");
                rejected += 1;
            }
        }
    }
    Ok(Json(json!({ "accepted": accepted, "rejected": rejected })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::money::{Currency, Money};

    fn intent(status: &str, amount_minor: i64, currency: &str) -> StkIntentStatus {
        StkIntentStatus {
            status: status.into(),
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
    fn mpesa_leg_status_comes_from_matching_intent() {
        let leg = mpesa_leg(100_000, Currency::kes());
        assert_eq!(
            mpesa_payment_status(Some(intent("completed", 100_000, "KES")), &leg).unwrap(),
            "approved"
        );
        assert_eq!(
            mpesa_payment_status(Some(intent("failed", 100_000, "KES")), &leg).unwrap(),
            "failed"
        );
        assert_eq!(
            mpesa_payment_status(Some(intent("pending", 100_000, "KES")), &leg).unwrap(),
            "pending"
        );
    }

    #[test]
    fn mpesa_leg_rejects_unknown_or_mismatched_intent() {
        let leg = mpesa_leg(100_000, Currency::kes());
        assert!(mpesa_payment_status(None, &leg).is_err());
        assert!(mpesa_payment_status(Some(intent("completed", 10_000, "KES")), &leg).is_err());
        assert!(mpesa_payment_status(Some(intent("completed", 100_000, "USD")), &leg).is_err());
    }
}
