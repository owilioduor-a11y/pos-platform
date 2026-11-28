use crate::config::Config;
use crate::gateways::payment_http_client;
use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use pos_domain::money::Money;
use pos_domain::payment::{PaymentGateway, PaymentReceipt, PaymentRequest};
use pos_domain::{DomainError, DomainResult, PaymentMethod};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Safaricom Daraja M-Pesa Express (STK Push) client.
pub struct DarajaClient {
    http: reqwest::Client,
    base_url: String,
    consumer_key: String,
    consumer_secret: String,
    passkey: String,
    shortcode: String,
    callback_url: String,
    callback_secret: String,
}

/// Daraja's `Amount` field is whole KES. Reject sub-shilling amounts instead
/// of silently truncating them, which would make the callback disagree with
/// the stored intent.
pub fn whole_kes_amount(amount_minor: i64) -> DomainResult<i64> {
    if amount_minor <= 0 {
        return Err(DomainError::PaymentDeclined(
            "amount must be positive".into(),
        ));
    }
    if amount_minor % 100 != 0 {
        return Err(DomainError::PaymentDeclined(
            "M-Pesa amounts must be whole KES".into(),
        ));
    }
    Ok(amount_minor / 100)
}

/// Daraja callbacks are unsigned. We bind each per-request callback URL to the
/// idempotency key so a spoofed callback needs the server secret, not just a
/// leaked CheckoutRequestID.
fn callback_token(secret: &str, idempotency_key: Uuid) -> DomainResult<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| DomainError::PaymentDeclined("bad Daraja callback secret".into()))?;
    mac.update(idempotency_key.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn callback_token_matches(
    secret: &str,
    idempotency_key: Uuid,
    provided: &str,
) -> DomainResult<bool> {
    let Ok(provided) = hex::decode(provided.trim()) else {
        return Ok(false);
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| DomainError::PaymentDeclined("bad Daraja callback secret".into()))?;
    mac.update(idempotency_key.as_bytes());
    Ok(mac.verify_slice(&provided).is_ok())
}

fn callback_url_with_token(url: &str, token: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}stk_token={token}")
}

impl DarajaClient {
    pub fn new(config: &Config) -> DomainResult<Self> {
        let base_url = if config.daraja_env.eq_ignore_ascii_case("production") {
            "https://api.safaricom.co.ke"
        } else {
            "https://sandbox.safaricom.co.ke"
        }
        .to_string();
        Ok(Self {
            http: payment_http_client()?,
            base_url,
            consumer_key: config.daraja_consumer_key.clone(),
            consumer_secret: config.daraja_consumer_secret.clone(),
            passkey: config.daraja_passkey.clone(),
            shortcode: config.daraja_shortcode.clone(),
            callback_url: config.daraja_callback_url.clone(),
            callback_secret: config.daraja_callback_secret.clone(),
        })
    }

    pub fn callback_token_required(&self) -> bool {
        !self.callback_secret.is_empty()
    }

    /// Per-request callback URL carrying a token bound to this idempotency key.
    pub fn tokenized_callback_url(&self, idempotency_key: Uuid) -> DomainResult<String> {
        if self.callback_secret.is_empty() {
            // Development/sandbox only; production validation fails closed.
            return Ok(self.callback_url.clone());
        }
        let token = callback_token(&self.callback_secret, idempotency_key)?;
        Ok(callback_url_with_token(&self.callback_url, &token))
    }

    /// Constant-time verification against the token embedded in the callback URL.
    pub fn verify_callback_token(
        &self,
        idempotency_key: Uuid,
        provided: &str,
    ) -> DomainResult<bool> {
        if self.callback_secret.is_empty() {
            // Development/sandbox only; production validation fails closed.
            return Ok(true);
        }
        callback_token_matches(&self.callback_secret, idempotency_key, provided)
    }

    async fn access_token(&self) -> DomainResult<String> {
        let url = format!(
            "{}/oauth/v1/generate?grant_type=client_credentials",
            self.base_url
        );
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.consumer_key, Some(&self.consumer_secret))
            .send()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("daraja oauth: {e}")))?
            .error_for_status()
            .map_err(|e| DomainError::PaymentDeclined(format!("daraja oauth status: {e}")))?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("daraja oauth body: {e}")))?;
        body.get("access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| DomainError::PaymentDeclined(format!("daraja oauth failed: {body}")))
    }

    /// M-Pesa Express STK Push. Returns a pending receipt; final status
    /// arrives via the callback webhook.
    pub async fn stk_push(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt> {
        let phone = req
            .mpesa_phone
            .clone()
            .ok_or_else(|| DomainError::PaymentDeclined("mpesa_phone required".into()))?;
        if req.amount.currency.code != "KES" {
            return Err(DomainError::PaymentDeclined(
                "M-Pesa only supports KES".into(),
            ));
        }

        let token = self.access_token().await?;
        let timestamp = Utc::now().format("%Y%m%d%H%M%S").to_string();
        let password = base64::engine::general_purpose::STANDARD
            .encode(format!("{}{}{}", self.shortcode, self.passkey, timestamp));
        let amount_kes = whole_kes_amount(req.amount.amount_minor)?;

        let body = json!({
            "BusinessShortCode": self.shortcode,
            "Password": password,
            "Timestamp": timestamp,
            "TransactionType": "CustomerPayBillOnline",
            "Amount": amount_kes,
            "PartyA": phone,
            "PartyB": self.shortcode,
            "PhoneNumber": phone,
            "CallBackURL": self.tokenized_callback_url(req.idempotency_key)?,
            "AccountReference": req.idempotency_key.to_string(),
            "TransactionDesc": "POS Sale",
        });

        let resp = self
            .http
            .post(format!("{}/mpesa/stkpush/v1/processrequest", self.base_url))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("stk push: {e}")))?
            .error_for_status()
            .map_err(|e| DomainError::PaymentDeclined(format!("stk push status: {e}")))?;

        let payload: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("stk push body: {e}")))?;

        let accepted = payload
            .get("ResponseCode")
            .and_then(Value::as_str)
            .map(|c| c == "0")
            .unwrap_or(false);

        let reference = payload
            .get("CheckoutRequestID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        Ok(PaymentReceipt::new(
            accepted,
            reference,
            req.amount.clone(),
            None,
            if accepted {
                None
            } else {
                Some(payload.to_string())
            },
            payload,
        ))
    }
}

#[async_trait]
impl PaymentGateway for DarajaClient {
    fn provider_name(&self) -> &'static str {
        "daraja"
    }

    async fn charge(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt> {
        match req.method {
            PaymentMethod::Mpesa => self.stk_push(req).await,
            other => Err(DomainError::PaymentDeclined(format!(
                "daraja does not support {other:?}"
            ))),
        }
    }

    async fn refund(&self, _reference: &str, _amount: &Money) -> DomainResult<PaymentReceipt> {
        // M-Pesa refunds go through the B2C/reversal APIs and require
        // additional security credentials — handled manually for now.
        Err(DomainError::PaymentDeclined(
            "M-Pesa refunds are not yet automated".into(),
        ))
    }

    fn verify_webhook(&self, _headers: &[(String, String)], body: &[u8]) -> DomainResult<bool> {
        // Daraja callbacks are not signed. Authenticity comes from the
        // per-request callback URL token plus the CheckoutRequestID/intent
        // correlation checked by the webhook handler.
        let parsed: Result<Value, _> = serde_json::from_slice(body);
        Ok(parsed.is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daraja_amounts_must_be_whole_kes() {
        assert_eq!(whole_kes_amount(100_000).unwrap(), 1000);
        assert!(whole_kes_amount(100_050).is_err());
        assert!(whole_kes_amount(0).is_err());
        assert!(whole_kes_amount(-100).is_err());
    }

    #[test]
    fn callback_token_is_bound_to_key_and_idempotency_key() {
        let secret = "callback-secret";
        let key = Uuid::now_v7();
        let token = callback_token(secret, key).unwrap();
        assert!(callback_token_matches(secret, key, &token).unwrap());
        assert!(!callback_token_matches(secret, Uuid::now_v7(), &token).unwrap());
        assert!(!callback_token_matches(secret, key, "not-hex").unwrap());
    }

    #[test]
    fn callback_token_url_uses_the_existing_query_separator() {
        assert_eq!(
            callback_url_with_token("https://pos.example/callback", "abc"),
            "https://pos.example/callback?stk_token=abc"
        );
        assert_eq!(
            callback_url_with_token("https://pos.example/callback?source=daraja", "abc"),
            "https://pos.example/callback?source=daraja&stk_token=abc"
        );
    }
}
