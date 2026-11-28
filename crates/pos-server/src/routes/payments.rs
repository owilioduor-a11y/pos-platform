use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::body::Bytes;
use axum::extract::{Path, State};
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

use crate::gateways::daraja;
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

fn validate_mpesa_amount(amount: &Money) -> ApiResult<()> {
    if amount.currency.code != "KES" {
        return Err(ApiError::BadRequest("M-Pesa only supports KES".into()));
    }
    daraja::whole_kes_amount(amount.amount_minor)
        .map(|_| ())
        .map_err(|error| ApiError::BadRequest(error.to_string()))
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

#[derive(Debug, PartialEq, Eq)]
struct DarajaCallback {
    checkout_request_id: String,
    result_code: i64,
    receipt_number: Option<String>,
    /// Whole KES, as reported by Daraja (we send `amount_minor / 100`).
    amount_kes: Option<i64>,
    phone: Option<String>,
}

fn mask_phone(phone: &str) -> String {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    if digits.len() <= 4 {
        "****".into()
    } else {
        format!("****{}", &digits[digits.len() - 4..])
    }
}

fn callback_metadata<'a>(stk: &'a Value, name: &str) -> Option<&'a Value> {
    stk.pointer("/CallbackMetadata/Item")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("Name").and_then(Value::as_str) == Some(name))
        })
        .and_then(|item| item.get("Value"))
}

fn parse_daraja_callback(payload: &Value) -> ApiResult<DarajaCallback> {
    let stk = payload
        .pointer("/Body/stkCallback")
        .ok_or_else(|| ApiError::BadRequest("missing Daraja stkCallback".into()))?;
    let checkout_request_id = stk
        .get("CheckoutRequestID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::BadRequest("missing Daraja CheckoutRequestID".into()))?
        .to_string();
    let result_code = stk
        .get("ResultCode")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::BadRequest("missing Daraja ResultCode".into()))?;
    let receipt_number = callback_metadata(stk, "MpesaReceiptNumber")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let amount_kes = callback_metadata(stk, "Amount").and_then(|value| match value {
        Value::Number(amount) => amount.as_i64(),
        Value::String(amount) => amount.trim().parse::<i64>().ok(),
        _ => None,
    });
    // Daraja has shipped this field as both a JSON string and a number.
    let phone = callback_metadata(stk, "PhoneNumber").and_then(|value| match value {
        Value::String(phone) if !phone.is_empty() => Some(phone.clone()),
        Value::Number(phone) => Some(phone.to_string()),
        _ => None,
    });
    Ok(DarajaCallback {
        checkout_request_id,
        result_code,
        receipt_number,
        amount_kes,
        phone,
    })
}

/// Daraja callbacks are unsigned. A callback is only trusted when it agrees
/// with the STK intent we created — including the amount and the payer for a
/// successful result — not merely because it carried a known request id.
fn callback_matches_intent(
    callback: &DarajaCallback,
    intent_amount_minor: i64,
    intent_phone_masked: &str,
) -> bool {
    if callback.result_code != 0 {
        return true;
    }
    if intent_amount_minor <= 0 || intent_amount_minor % 100 != 0 {
        return false;
    }
    if callback.receipt_number.is_none() {
        return false;
    }
    if callback.amount_kes != Some(intent_amount_minor / 100) {
        return false;
    }
    callback
        .phone
        .as_deref()
        .is_none_or(|phone| mask_phone(phone) == intent_phone_masked)
}

struct StkIntent<'a> {
    merchant_id: Uuid,
    idempotency_key: Uuid,
    account_ref: &'a str,
    amount_minor: i64,
    currency: &'a str,
    phone: &'a str,
    payload: &'a Value,
}

async fn store_stk_intent(pool: &PgPool, intent: &StkIntent<'_>) -> ApiResult<()> {
    let (Some(checkout_request_id), Some(merchant_request_id)) = (
        intent
            .payload
            .get("CheckoutRequestID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty()),
        intent
            .payload
            .get("MerchantRequestID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty()),
    ) else {
        tracing::error!("Daraja accepted STK push without request ids; cannot reconcile callback");
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO mpesa_stk_intents
            (merchant_id, idempotency_key, checkout_request_id, merchant_request_id,
             account_ref, amount_minor, currency, phone_masked)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (idempotency_key) DO NOTHING",
    )
    .bind(intent.merchant_id)
    .bind(intent.idempotency_key)
    .bind(checkout_request_id)
    .bind(merchant_request_id)
    .bind(intent.account_ref)
    .bind(intent.amount_minor)
    .bind(intent.currency)
    .bind(mask_phone(intent.phone))
    .execute(pool)
    .await?;
    Ok(())
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
    if method == PaymentMethod::Mpesa {
        validate_mpesa_amount(&amount)?;
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
    validate_mpesa_amount(&amount)?;
    let idempotency_key = parse_idempotency_key(&req.idempotency_key)?;
    let account_ref = req
        .account_ref
        .unwrap_or_else(|| user.merchant_id.to_string());
    let phone = req.phone;
    let fingerprint = request_fingerprint(
        PaymentMethod::Mpesa,
        &amount,
        Some(&phone),
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
        amount: amount.clone(),
        merchant_id: user.merchant_id,
        idempotency_key,
        method: PaymentMethod::Mpesa,
        mpesa_phone: Some(phone.clone()),
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
    if receipt.approved {
        store_stk_intent(
            &state.pool,
            &StkIntent {
                merchant_id: user.merchant_id,
                idempotency_key,
                account_ref: &account_ref,
                amount_minor: amount.amount_minor,
                currency: &amount.currency.code,
                phone: &phone,
                payload: &payload,
            },
        )
        .await?;
    }
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

fn mpesa_status_response(
    status: &str,
    result_code: Option<i64>,
    receipt_number: Option<&str>,
    amount_minor: i64,
) -> Value {
    json!({
        "status": status,
        "approved": status == "completed",
        "result_code": result_code,
        "receipt_number": receipt_number,
        "amount_minor": amount_minor,
    })
}

/// GET /api/v1/payments/mpesa/status/:checkout_request_id — poll the
/// reconciled callback result for an STK Push belonging to this merchant.
pub async fn mpesa_status(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(checkout_request_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if checkout_request_id.trim().is_empty() {
        return Err(ApiError::BadRequest("checkout request id required".into()));
    }
    let row: Option<(String, Option<i64>, Option<String>, i64)> = sqlx::query_as(
        "SELECT status, result_code, receipt_number, amount_minor
         FROM mpesa_stk_intents
         WHERE merchant_id = $1 AND checkout_request_id = $2",
    )
    .bind(user.merchant_id)
    .bind(&checkout_request_id)
    .fetch_optional(&state.pool)
    .await?;
    let (status, result_code, receipt_number, amount_minor) = row.ok_or(ApiError::NotFound)?;
    Ok(Json(mpesa_status_response(
        &status,
        result_code,
        receipt_number.as_deref(),
        amount_minor,
    )))
}

/// POST /api/v1/webhooks/daraja — Safaricom STK callback.
/// Public route: Safaricom callbacks are unsigned, so the result is accepted
/// only after it matches the stored STK intent (id, amount and payer). Replays
/// are deduped by CheckoutRequestID, but only validated callbacks are recorded
/// so a spoofed event cannot block the genuine one.
pub async fn daraja_webhook(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("invalid JSON body".into()))?;
    let callback = parse_daraja_callback(&parsed)?;

    let intent = sqlx::query_as::<_, (Uuid, i64, String, String)>(
        "SELECT merchant_id, amount_minor, currency, phone_masked FROM mpesa_stk_intents
         WHERE checkout_request_id = $1",
    )
    .bind(&callback.checkout_request_id)
    .fetch_optional(&pool)
    .await?;
    let Some((intent_merchant_id, intent_amount_minor, intent_currency, intent_phone_masked)) =
        intent
    else {
        tracing::warn!(
            checkout_request_id = %callback.checkout_request_id,
            "ignoring Daraja callback without a matching STK intent"
        );
        return Ok(Json(json!({ "ResultCode": 0, "ResultDesc": "ignored" })));
    };
    if !callback_matches_intent(&callback, intent_amount_minor, &intent_phone_masked) {
        tracing::warn!(
            checkout_request_id = %callback.checkout_request_id,
            "rejecting Daraja callback that disagrees with the STK intent"
        );
        return Ok(Json(json!({ "ResultCode": 0, "ResultDesc": "rejected" })));
    }

    let result = sqlx::query(
        "INSERT INTO webhook_events (provider, event_id, payload_json) VALUES ('daraja', $1, $2)
         ON CONFLICT (provider, event_id) DO NOTHING",
    )
    .bind(&callback.checkout_request_id)
    .bind(&parsed)
    .execute(&pool)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(Json(
            json!({ "ResultCode": 0, "ResultDesc": "duplicate ignored" }),
        ));
    }

    let (intent_status, payment_status) = if callback.result_code == 0 {
        ("completed", "approved")
    } else {
        ("failed", "failed")
    };
    let updated = sqlx::query(
        "UPDATE mpesa_stk_intents
         SET status = $2, result_code = $3, receipt_number = $4, updated_at = now()
         WHERE checkout_request_id = $1",
    )
    .bind(&callback.checkout_request_id)
    .bind(intent_status)
    .bind(callback.result_code)
    .bind(&callback.receipt_number)
    .execute(&pool)
    .await?;
    // The sale may sync before or after the callback, so reconcile both sides.
    let payment_updates = sqlx::query(
        "UPDATE payments SET status = $2
         WHERE method = 'mpesa' AND reference = $1
           AND amount_minor = $4 AND currency = $5
           AND EXISTS (
               SELECT 1 FROM sales
               WHERE sales.id = payments.sale_id AND sales.merchant_id = $3
           )",
    )
    .bind(&callback.checkout_request_id)
    .bind(payment_status)
    .bind(intent_merchant_id)
    .bind(intent_amount_minor)
    .bind(intent_currency)
    .execute(&pool)
    .await?;
    if updated.rows_affected() == 0 {
        tracing::warn!(
            checkout_request_id = %callback.checkout_request_id,
            payments_updated = payment_updates.rows_affected(),
            "Daraja callback has no matching STK intent"
        );
    } else {
        tracing::info!(
            result_code = callback.result_code,
            status = intent_status,
            payments_updated = payment_updates.rows_affected(),
            "daraja callback reconciled"
        );
    }
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
    fn mpesa_amount_must_be_whole_kes() {
        assert!(validate_mpesa_amount(&Money::new(100_000, Currency::kes()).unwrap()).is_ok());
        assert!(validate_mpesa_amount(&Money::new(100_050, Currency::kes()).unwrap()).is_err());
        assert!(validate_mpesa_amount(&Money::new(100_000, Currency::usd()).unwrap()).is_err());
    }

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

    #[test]
    fn daraja_callback_requires_checkout_id_and_result_code() {
        let valid = json!({
            "Body": {
                "stkCallback": {
                    "CheckoutRequestID": "ws_CO_123",
                    "ResultCode": 0,
                    "CallbackMetadata": {
                        "Item": [
                            { "Name": "MpesaReceiptNumber", "Value": "QX7ABC1" },
                            { "Name": "Amount", "Value": 1000 },
                            { "Name": "PhoneNumber", "Value": 254712345678_i64 }
                        ]
                    }
                }
            }
        });
        assert_eq!(
            parse_daraja_callback(&valid).unwrap(),
            DarajaCallback {
                checkout_request_id: "ws_CO_123".into(),
                result_code: 0,
                receipt_number: Some("QX7ABC1".into()),
                amount_kes: Some(1000),
                phone: Some("254712345678".into()),
            }
        );
        assert!(
            parse_daraja_callback(&json!({"Body": {"stkCallback": {"ResultCode": 0}}})).is_err()
        );
        assert!(parse_daraja_callback(
            &json!({"Body": {"stkCallback": {"CheckoutRequestID": "ws_CO_123"}}})
        )
        .is_err());
    }

    fn success_callback(
        amount_kes: Option<i64>,
        phone: Option<&str>,
        with_receipt: bool,
    ) -> DarajaCallback {
        DarajaCallback {
            checkout_request_id: "ws_CO_123".into(),
            result_code: 0,
            receipt_number: with_receipt.then(|| "QX7ABC1".into()),
            amount_kes,
            phone: phone.map(str::to_string),
        }
    }

    #[test]
    fn daraja_success_callback_must_match_intent_amount_and_payer() {
        assert!(callback_matches_intent(
            &success_callback(Some(1000), Some("254712345678"), true),
            100_000,
            "****5678"
        ));
        // A missing phone in CallbackMetadata is tolerated, but a wrong one is not.
        assert!(callback_matches_intent(
            &success_callback(Some(1000), None, true),
            100_000,
            "****5678"
        ));
        assert!(!callback_matches_intent(
            &success_callback(Some(999), Some("254712345678"), true),
            100_000,
            "****5678"
        ));
        assert!(!callback_matches_intent(
            &success_callback(Some(1000), Some("254700000000"), true),
            100_000,
            "****5678"
        ));
        assert!(!callback_matches_intent(
            &success_callback(Some(1000), Some("254712345678"), false),
            100_000,
            "****5678"
        ));
        assert!(!callback_matches_intent(
            &success_callback(None, Some("254712345678"), true),
            100_000,
            "****5678"
        ));
        assert!(!callback_matches_intent(
            &success_callback(Some(1000), Some("254712345678"), true),
            100_050,
            "****5678"
        ));
    }

    #[test]
    fn daraja_failure_callback_needs_only_the_matching_request_id() {
        let callback = DarajaCallback {
            checkout_request_id: "ws_CO_123".into(),
            result_code: 1,
            receipt_number: None,
            amount_kes: None,
            phone: None,
        };
        assert!(callback_matches_intent(&callback, 100_000, "****5678"));
    }

    #[test]
    fn daraja_accepts_string_or_numeric_amount() {
        let fixture = |value: Value| {
            json!({
                "Body": { "stkCallback": {
                    "CheckoutRequestID": "ws_CO_123",
                    "ResultCode": 0,
                    "CallbackMetadata": { "Item": [
                        { "Name": "MpesaReceiptNumber", "Value": "QX7ABC1" },
                        { "Name": "Amount", "Value": value }
                    ]}
                }}
            })
        };
        assert_eq!(
            parse_daraja_callback(&fixture(json!(1000)))
                .unwrap()
                .amount_kes,
            Some(1000)
        );
        assert_eq!(
            parse_daraja_callback(&fixture(json!("1000")))
                .unwrap()
                .amount_kes,
            Some(1000)
        );
    }

    #[test]
    fn phone_mask_keeps_only_last_four_digits() {
        assert_eq!(mask_phone("+254712345678"), "****5678");
        assert_eq!(mask_phone("123"), "****");
    }

    #[test]
    fn mpesa_status_marks_only_completed_as_approved() {
        let pending = mpesa_status_response("pending", None, None, 1000);
        assert_eq!(pending["status"], "pending");
        assert_eq!(pending["approved"], false);

        let completed = mpesa_status_response("completed", Some(0), Some("QX7ABC1"), 1000);
        assert_eq!(completed["approved"], true);
        assert_eq!(completed["receipt_number"], "QX7ABC1");
    }
}
