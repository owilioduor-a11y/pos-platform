use crate::auth::AuthUser;
use crate::error::ApiResult;
use axum::extract::State;
use axum::Json;
use pos_domain::sale::Sale;
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

        match insert_sale(&pool, &sale, user.user_id).await {
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

/// Insert a sale + its payment legs atomically.
/// Returns false on idempotency conflict, with no partial rows written.
async fn insert_sale(pool: &PgPool, sale: &Sale, cashier_id: Uuid) -> ApiResult<bool> {
    let payload = serde_json::to_value(sale)?;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await?;

    let result = sqlx::query(
        "INSERT INTO sales (id, merchant_id, cashier_id, idempotency_key, payload_json, status)
         VALUES ($1, $2, $3, $4, $5, 'completed')
         ON CONFLICT (idempotency_key) DO NOTHING",
    )
    .bind(sale.id)
    .bind(sale.merchant_id)
    .bind(cashier_id)
    .bind(sale.idempotency_key)
    .bind(payload)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(false);
    }

    for payment in &sale.payments {
        sqlx::query(
            "INSERT INTO payments (sale_id, method, amount_minor, currency, reference, status)
             VALUES ($1, $2, $3, $4, $5, 'approved')",
        )
        .bind(sale.id)
        .bind(method_str(payment.method))
        .bind(payment.amount.amount_minor)
        .bind(&payment.amount.currency.code)
        .bind(&payment.reference)
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
