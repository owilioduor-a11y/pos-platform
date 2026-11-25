use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use pos_domain::money::{Currency, Money};
use pos_domain::payment::PaymentRequest;
use pos_domain::PaymentMethod;
use serde::Deserialize;
use serde_json::json;
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
    /// Client idempotency key; generated server-side if absent.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MpesaPushRequest {
    pub phone: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    /// Till vs paybill account reference (max 12 chars).
    pub account_ref: Option<String>,
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
    let idempotency_key = match &req.idempotency_key {
        Some(raw) => raw
            .parse::<Uuid>()
            .map_err(|_| ApiError::BadRequest("idempotency_key must be a UUID".into()))?,
        None => Uuid::now_v7(),
    };

    let request = PaymentRequest {
        amount,
        merchant_id: user.merchant_id,
        idempotency_key,
        method,
        mpesa_phone: req.mpesa_phone,
        card_token: req.card_token,
    };

    let receipt = state.gateways.charge(&request).await?;
    Ok(Json(json!({
        "approved": receipt.approved,
        "reference": receipt.reference,
        "amount_minor": receipt.amount.amount_minor,
        "currency": receipt.amount.currency.code,
        "provider": state.gateways.provider_for(method),
        "decline_reason": receipt.decline_reason,
    })))
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
    let account_ref = req.account_ref.unwrap_or_else(|| user.merchant_id.to_string());
    let result = state
        .gateways
        .mpesa
        .stk_push(&req.phone, &amount, &account_ref, "POS sale")
        .await
        .map_err(|e| ApiError::PaymentFailed(e.to_string()))?;

    Ok(Json(json!({
        "merchant_request_id": result.merchant_request_id,
        "checkout_request_id": result.checkout_request_id,
        "response_code": result.response_code,
        "response_description": result.response_description,
    })))
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
        .unwrap_or("unknown")
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
        return Ok(Json(json!({ "ResultCode": 0, "ResultDesc": "duplicate ignored" })));
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
            arr.iter().find(|i| i.get("Name").and_then(|n| n.as_str()) == Some("MpesaReceiptNumber"))
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
    let gateways = &state.gateways;
    let header_pairs: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.as_str().to_string(), s.to_string())))
        .collect();
    let verified = gateways
        .card
        .verify_webhook(&header_pairs, &body)
        .map_err(|e| ApiError::Unauthorized)?;
    if !verified {
        return Err(ApiError::Unauthorized);
    }

    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("invalid JSON body".into()))?;
    let event_id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
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
