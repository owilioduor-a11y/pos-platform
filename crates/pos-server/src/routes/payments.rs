use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use pos_domain::money::{Currency, Money};
use pos_domain::payment::{PaymentGateway, PaymentReceipt, PaymentRequest};
use pos_domain::PaymentMethod;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ChargeRequest {
    /// "card_tap" | "mpesa" | "cash".
    pub method: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    /// Required for mpesa.
    pub mpesa_phone: Option<String>,
    /// Required for card_tap — opaque SoftPOS token, never a PAN.
    pub card_token: Option<String>,
    /// Client-generated UUIDv7 key. Required so retries never double-charge.
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
pub struct MpesaPushRequest {
    pub phone: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    /// Till vs paybill account reference (max 12 chars).
    pub account_ref: Option<String>,
    /// Client-generated UUIDv7 key for this STK Push attempt.
    pub idempotency_key: String,
}

fn parse_method(raw: &str) -> ApiResult<PaymentMethod> {
    match raw.to_ascii_lowercase().as_str() {
        "card_tap" | "cardtap" | "card" => Ok(PaymentMethod::CardTap),
        "mpesa" | "m_pesa" | "mobile_money" => Ok(PaymentMethod::Mpesa),
        "cash" => Ok(PaymentMethod::Cash),
        other => Err(ApiError::BadRequest(format!("unknown method '{other}'"))),
    }
}

fn money_from(req_currency: &Option<String>, amount_minor: i64) -> ApiResult<Money> {
    let code = req_currency
        .clone()
        .unwrap_or_else(|| "KES".into())
        .to_ascii_uppercase();
    let currency = Currency::new(&code, 2, "").map_err(ApiError::from)?;
    Money::new(amount_minor, currency).map_err(ApiError::from)
}

fn parse_idempotency_key(raw: &str) -> ApiResult<Uuid> {
    let key = raw
        .trim()
        .parse::<Uuid>()
        .map_err(|_| ApiError::BadRequest("idempotency_key must be a UUIDv7".into()))?;
    if key.get_version_num() != 7 {
        return Err(ApiError::BadRequest(
            "idempotency_key must be a UUIDv7".into(),
        ));
    }
    Ok(key)
}

fn method_str(method: PaymentMethod) -> &'static str {
    match method {
        PaymentMethod::CardTap => "card_tap",
        PaymentMethod::Mpesa => "mpesa",
        PaymentMethod::Cash => "cash",
    }
}

fn charge_response(receipt: &PaymentReceipt, provider: &'static str) -> Value {
    json!({
        "approved": receipt.approved,
        "reference": receipt.reference,
        "amount_minor": receipt.amount.amount_minor,
        "currency": receipt.amount.currency.code,
        "provider": provider,
        "decline_reason": receipt.decline_reason,
    })
}

/// Hash the request inputs that affect the provider call. Stored alongside the
/// attempt so a reused UUIDv7 cannot silently return an unrelated payment.
fn request_fingerprint(
    method: PaymentMethod,
    amount: &Money,
    mpesa_phone: Option<&str>,
    card_token: Option<&str>,
    account_ref: Option<&str>,
) -> ApiResult<String> {
    let canonical = serde_json::to_vec(&json!({
        "method": method_str(method),
        "amount_minor": amount.amount_minor,
        "currency": amount.currency.code,
        "mpesa_phone": mpesa_phone,
        "card_token": card_token,
        "account_ref": account_ref,
    }))?;
    Ok(hex::encode(Sha256::digest(canonical)))
}

enum AttemptReservation {
    Owner,
    Completed(Value),
    InProgress,
    Failed,
    Mismatch,
}

/// Atomically claim the idempotency key. Only the owner may call the provider;
/// every other caller gets the stored outcome or a safe conflict.
async fn reserve_attempt(
    pool: &PgPool,
    merchant_id: Uuid,
    idempotency_key: Uuid,
    method: PaymentMethod,
    fingerprint: &str,
) -> ApiResult<AttemptReservation> {
    let inserted = sqlx::query(
        "INSERT INTO payment_attempts
            (merchant_id, idempotency_key, method, request_hash, status)
         VALUES ($1, $2, $3, $4, 'in_progress')
         ON CONFLICT (merchant_id, idempotency_key) DO NOTHING",
    )
    .bind(merchant_id)
    .bind(idempotency_key)
    .bind(method_str(method))
    .bind(fingerprint)
    .execute(pool)
    .await?;
    if inserted.rows_affected() > 0 {
        return Ok(AttemptReservation::Owner);
    }

    let row: Option<(String, String, Option<Value>)> = sqlx::query_as(
        "SELECT status, request_hash, response_json FROM payment_attempts
         WHERE merchant_id = $1 AND idempotency_key = $2",
    )
    .bind(merchant_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;
    let Some((status, stored_hash, response)) = row else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "payment attempt conflict has no row"
        )));
    };
    if stored_hash != fingerprint {
        return Ok(AttemptReservation::Mismatch);
    }
    match status.as_str() {
        "completed" => response.map(AttemptReservation::Completed).ok_or_else(|| {
            ApiError::Internal(anyhow::anyhow!("completed payment attempt has no response"))
        }),
        "failed" => Ok(AttemptReservation::Failed),
        "in_progress" => Ok(AttemptReservation::InProgress),
        other => Err(ApiError::Internal(anyhow::anyhow!(
            "unexpected payment attempt status '{other}'"
        ))),
    }
}

fn reservation_error(reservation: &AttemptReservation) -> ApiError {
    match reservation {
        AttemptReservation::InProgress => ApiError::Conflict(
            "payment with this idempotency key is already in progress; retry shortly".into(),
        ),
        AttemptReservation::Failed => ApiError::PaymentFailed(
            "previous attempt with this idempotency key failed; use a new key".into(),
        ),
        AttemptReservation::Mismatch => ApiError::Conflict(
            "idempotency key was already used for a different payment request".into(),
        ),
        AttemptReservation::Completed(_) => {
            ApiError::Internal(anyhow::anyhow!("completed attempt was not returned"))
        }
        AttemptReservation::Owner => {
            ApiError::Internal(anyhow::anyhow!("owned attempt was not processed"))
        }
    }
}

async fn complete_attempt(
    pool: &PgPool,
    merchant_id: Uuid,
    idempotency_key: Uuid,
    response: &Value,
) -> ApiResult<Value> {
    let result = sqlx::query(
        "UPDATE payment_attempts
         SET status = 'completed', response_json = $3, updated_at = now()
         WHERE merchant_id = $1 AND idempotency_key = $2 AND status = 'in_progress'",
    )
    .bind(merchant_id)
    .bind(idempotency_key)
    .bind(response)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "payment attempt is no longer in progress".into(),
        ));
    }
    Ok(response.clone())
}

async fn fail_attempt(pool: &PgPool, merchant_id: Uuid, idempotency_key: Uuid) -> ApiResult<()> {
    let result = sqlx::query(
        "UPDATE payment_attempts
         SET status = 'failed', response_json = NULL, updated_at = now()
         WHERE merchant_id = $1 AND idempotency_key = $2 AND status = 'in_progress'",
    )
    .bind(merchant_id)
    .bind(idempotency_key)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict(
            "payment attempt is no longer in progress".into(),
        ));
    }
    Ok(())
}

/// POST /api/v1/payments/charge — route a payment to the regional gateway.
pub async fn charge(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<ChargeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let method = parse_method(&req.method)?;
    if method == PaymentMethod::Cash {
        return Err(ApiError::BadRequest(
            "cash payments settle at the till; no gateway call".into(),
        ));
    }
    let amount = money_from(&req.currency_code, req.amount_minor)?;
    if amount.amount_minor <= 0 {
        return Err(ApiError::BadRequest("amount must be positive".into()));
    }
    let idempotency_key = parse_idempotency_key(&req.idempotency_key)?;
    let fingerprint = request_fingerprint(
        method,
        &amount,
        req.mpesa_phone.as_deref(),
        req.card_token.as_deref(),
        None,
    )?;
    match reserve_attempt(
        &state.pool,
        user.merchant_id,
        idempotency_key,
        method,
        &fingerprint,
    )
    .await?
    {
        AttemptReservation::Owner => {}
        AttemptReservation::Completed(response) => return Ok(Json(response)),
        other => return Err(reservation_error(&other)),
    }

    let request = PaymentRequest {
        amount,
        merchant_id: user.merchant_id,
        idempotency_key,
        method,
        mpesa_phone: req.mpesa_phone,
        card_token: req.card_token,
    };

    let receipt = match state.gateways.charge(&request).await {
        Ok(receipt) => receipt,
        Err(e) => {
            // Never let a retry reach the provider after a gateway error:
            // the first call may have reached the acquirer before failing.
            fail_attempt(&state.pool, user.merchant_id, idempotency_key).await?;
            return Err(e.into());
        }
    };
    let response = charge_response(&receipt, state.gateways.provider_for(method));
    let response =
        complete_attempt(&state.pool, user.merchant_id, idempotency_key, &response).await?;
    Ok(Json(response))
}

/// POST /api/v1/payments/mpesa/stkpush — direct Daraja STK Push trigger.
pub async fn mpesa_stkpush(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Json(req): Json<MpesaPushRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let amount = money_from(&req.currency_code, req.amount_minor)?;
    if amount.amount_minor <= 0 {
        return Err(ApiError::BadRequest("amount must be positive".into()));
    }
    let idempotency_key = parse_idempotency_key(&req.idempotency_key)?;
    let account_ref = req
        .account_ref
        .unwrap_or_else(|| user.merchant_id.to_string());
    let fingerprint = request_fingerprint(
        PaymentMethod::Mpesa,
        &amount,
        Some(&req.phone),
        None,
        Some(&account_ref),
    )?;
    match reserve_attempt(
        &state.pool,
        user.merchant_id,
        idempotency_key,
        PaymentMethod::Mpesa,
        &fingerprint,
    )
    .await?
    {
        AttemptReservation::Owner => {}
        AttemptReservation::Completed(response) => return Ok(Json(response)),
        other => return Err(reservation_error(&other)),
    }

    // Reuse the standard charge path — DarajaClient::stk_push is invoked
    // through the PaymentGateway impl for PaymentMethod::Mpesa.
    let request = PaymentRequest {
        amount,
        merchant_id: user.merchant_id,
        idempotency_key,
        method: PaymentMethod::Mpesa,
        mpesa_phone: Some(req.phone),
        card_token: None,
    };
    let receipt = match state.gateways.charge(&request).await {
        Ok(receipt) => receipt,
        Err(e) => {
            fail_attempt(&state.pool, user.merchant_id, idempotency_key).await?;
            return Err(e.into());
        }
    };
    let payload = receipt.raw_provider_payload;
    let response = json!({
        "approved": receipt.approved,
        "reference": receipt.reference,
        "merchant_request_id": payload.get("MerchantRequestID"),
        "checkout_request_id": payload.get("CheckoutRequestID"),
        "response_code": payload.get("ResponseCode"),
        "response_description": payload.get("ResponseDescription"),
        "account_ref": account_ref,
    });
    let response =
        complete_attempt(&state.pool, user.merchant_id, idempotency_key, &response).await?;
    Ok(Json(response))
}

/// POST /api/v1/webhooks/daraja — Safaricom STK callback.
/// Public route (no JWT): authenticity comes from TLS + payload shape.
pub async fn daraja_webhook(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("invalid JSON body".into()))?;

    // Extract dedupe key: CheckoutRequestID or MerchantRequestID.
    let event_id = parsed
        .pointer("/Body/stkCallback/CheckoutRequestID")
        .and_then(|v| v.as_str())
        .or_else(|| {
            parsed
                .pointer("/Body/stkCallback/MerchantRequestID")
                .and_then(|v| v.as_str())
        })
        .ok_or_else(|| ApiError::BadRequest("missing Daraja callback request id".into()))?
        .to_string();

    let result = sqlx::query(
        "INSERT INTO webhook_events (provider, event_id, payload_json) VALUES ('daraja', $1, $2)
         ON CONFLICT (provider, event_id) DO NOTHING",
    )
    .bind(&event_id)
    .bind(&parsed)
    .execute(&pool)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(Json(
            json!({ "ResultCode": 0, "ResultDesc": "duplicate ignored" }),
        ));
    }

    // Extract payment outcome for ledger reconciliation.
    let stk = parsed.pointer("/Body/stkCallback");
    let result_code = stk
        .and_then(|s| s.get("ResultCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    let receipt = stk
        .and_then(|s| s.pointer("/CallbackMetadata/Item"))
        .and_then(|items| items.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|i| i.get("Name").and_then(|n| n.as_str()) == Some("MpesaReceiptNumber"))
        })
        .and_then(|i| i.get("Value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    tracing::info!(result_code, receipt, "daraja callback received");
    Ok(Json(json!({ "ResultCode": 0, "ResultDesc": "accepted" })))
}

/// POST /api/v1/webhooks/acquirer — card acquirer settlement callback.
/// Authenticated by HMAC-SHA256 signature in `x-signature` header.
pub async fn acquirer_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    let acquirer = state.gateways.card.as_ref().ok_or(ApiError::PaymentFailed(
        "card acquirer not configured".into(),
    ))?;
    let header_pairs: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect();
    let verified = acquirer
        .verify_webhook(&header_pairs, &body)
        .map_err(|_| ApiError::Unauthorized)?;
    if !verified {
        return Err(ApiError::Unauthorized);
    }

    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("invalid JSON body".into()))?;
    let event_id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("missing acquirer event id".into()))?
        .to_string();

    sqlx::query(
        "INSERT INTO webhook_events (provider, event_id, payload_json) VALUES ('acquirer', $1, $2)
         ON CONFLICT (provider, event_id) DO NOTHING",
    )
    .bind(&event_id)
    .bind(&parsed)
    .execute(&pool)
    .await?;

    Ok(Json(json!({ "received": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::money::Currency;

    #[test]
    fn idempotency_key_must_be_uuid7() {
        let v7 = Uuid::now_v7().to_string();
        assert!(parse_idempotency_key(&v7).is_ok());
        assert!(parse_idempotency_key(&Uuid::nil().to_string()).is_err());
        assert!(parse_idempotency_key("not-a-uuid").is_err());
    }

    #[test]
    fn charge_response_omits_raw_provider_payload() {
        let receipt = PaymentReceipt::new(
            true,
            "ref_1",
            Money::new(1000, Currency::kes()).unwrap(),
            None,
            None,
            json!({"card": {"pan": "4242424242424242"}}),
        );
        let response = charge_response(&receipt, "test_acquirer");
        assert_eq!(response["reference"], "ref_1");
        assert!(response.get("raw_provider_payload").is_none());
        assert!(response.get("pan").is_none());
    }

    #[test]
    fn request_fingerprint_is_stable_and_sensitive() {
        let amount = Money::new(1000, Currency::kes()).unwrap();
        let first = request_fingerprint(
            PaymentMethod::CardTap,
            &amount,
            None,
            Some("tok_test"),
            None,
        )
        .unwrap();
        let same = request_fingerprint(
            PaymentMethod::CardTap,
            &amount,
            None,
            Some("tok_test"),
            None,
        )
        .unwrap();
        let changed = request_fingerprint(
            PaymentMethod::CardTap,
            &amount,
            None,
            Some("tok_other"),
            None,
        )
        .unwrap();
        assert_eq!(first, same);
        assert_ne!(first, changed);
    }
}
