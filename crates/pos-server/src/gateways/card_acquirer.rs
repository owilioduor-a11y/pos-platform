use crate::config::Config;
use crate::gateways::payment_http_client;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use pos_domain::money::Money;
use pos_domain::payment::{PaymentGateway, PaymentReceipt, PaymentRequest};
use pos_domain::{DomainError, DomainResult, PaymentMethod};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Constant-time HMAC verification. A malformed signature is simply invalid,
/// not a distinct error a caller could use to probe the comparison.
fn verify_hmac(secret: &str, headers: &[(String, String)], body: &[u8]) -> DomainResult<bool> {
    let signature = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-signature"))
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| DomainError::PaymentDeclined("missing x-signature".into()))?;
    let Ok(provided) = hex::decode(signature.trim()) else {
        return Ok(false);
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| DomainError::PaymentDeclined("bad hmac key".into()))?;
    mac.update(body);
    Ok(mac.verify_slice(&provided).is_ok())
}

/// Server-side adapter for Kenyan card acquirers / SoftPOS backends.
/// When `KENYA_ACQUIRER_PROVIDER=mock` (or no API URL is configured) it
/// returns synthetic approved receipts so the full sale flow can be
/// exercised in sandbox without a certified acquirer contract.
pub struct CardAcquirerClient {
    http: reqwest::Client,
    api_url: String,
    api_key: String,
    webhook_secret: String,
    provider_name: &'static str,
    mock: bool,
}

impl CardAcquirerClient {
    pub fn new(config: &Config) -> DomainResult<Self> {
        let mock = config.acquirer_provider == "mock" || config.acquirer_api_url.is_empty();
        Ok(Self {
            http: payment_http_client()?,
            api_url: config.acquirer_api_url.trim_end_matches('/').to_string(),
            api_key: config.acquirer_api_key.clone(),
            webhook_secret: config.acquirer_webhook_secret.clone(),
            provider_name: "kenya_acquirer",
            mock,
        })
    }

    async fn charge_remote(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt> {
        let body = json!({
            "amount_minor": req.amount.amount_minor,
            "currency": req.amount.currency.code,
            "idempotency_key": req.idempotency_key.to_string(),
            "merchant_id": req.merchant_id.to_string(),
            "card_token": req.card_token,
        });
        let resp = self
            .http
            .post(format!("{}/v1/charges", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("acquirer charge: {e}")))?
            .error_for_status()
            .map_err(|e| DomainError::PaymentDeclined(format!("acquirer charge status: {e}")))?;

        let payload: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("acquirer body: {e}")))?;

        let approved = payload
            .get("approved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reference = payload
            .get("reference")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        Ok(PaymentReceipt::new(
            approved,
            reference,
            req.amount.clone(),
            None,
            payload
                .get("decline_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            payload,
        ))
    }

    fn mock_receipt(req: &PaymentRequest) -> PaymentReceipt {
        PaymentReceipt::new(
            true,
            format!("mock_{}", Uuid::now_v7().simple()),
            req.amount.clone(),
            None,
            None,
            json!({"mock": true}),
        )
    }
}

#[async_trait]
impl PaymentGateway for CardAcquirerClient {
    fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    async fn charge(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt> {
        if req.method != PaymentMethod::CardTap {
            return Err(DomainError::PaymentDeclined(
                "acquirer only supports card_tap".into(),
            ));
        }
        if self.mock {
            return Ok(Self::mock_receipt(req));
        }
        self.charge_remote(req).await
    }

    async fn refund(&self, reference: &str, amount: &Money) -> DomainResult<PaymentReceipt> {
        if self.mock {
            return Ok(PaymentReceipt::new(
                true,
                format!("mock_refund_{reference}"),
                amount.clone(),
                None,
                None,
                json!({"mock": true}),
            ));
        }
        let body = json!({
            "reference": reference,
            "amount_minor": amount.amount_minor,
            "currency": amount.currency.code,
        });
        let resp = self
            .http
            .post(format!("{}/v1/refunds", self.api_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("acquirer refund: {e}")))?
            .error_for_status()
            .map_err(|e| DomainError::PaymentDeclined(format!("acquirer refund status: {e}")))?;
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| DomainError::PaymentDeclined(format!("refund body: {e}")))?;
        Ok(PaymentReceipt::new(
            payload
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            payload
                .get("reference")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            amount.clone(),
            None,
            payload
                .get("decline_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            payload,
        ))
    }

    fn verify_webhook(&self, headers: &[(String, String)], body: &[u8]) -> DomainResult<bool> {
        if self.webhook_secret.is_empty() {
            // No secret configured (dev/mock) — accept but log a warning.
            tracing::warn!("acquirer webhook accepted without signature (no secret configured)");
            return Ok(true);
        }
        verify_hmac(&self.webhook_secret, headers, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(signature: &str) -> Vec<(String, String)> {
        vec![("X-Signature".into(), signature.into())]
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn hmac_verify_accepts_valid_signature() {
        let body = br#"{"id":"evt_1"}"#;
        let signature = sign("secret", body);
        assert!(verify_hmac("secret", &headers(&signature), body).unwrap());
        assert!(verify_hmac("secret", &headers(&signature.to_ascii_uppercase()), body).unwrap());
    }

    #[test]
    fn hmac_verify_rejects_wrong_or_malformed_signature() {
        let body = br#"{"id":"evt_1"}"#;
        let mut signature = sign("secret", body);
        let wrong = if signature.starts_with('0') { '1' } else { '0' };
        signature.replace_range(0..1, &wrong.to_string());
        assert!(!verify_hmac("secret", &headers(&signature), body).unwrap());
        assert!(!verify_hmac("secret", &headers("not-hex"), body).unwrap());
        assert!(!verify_hmac("secret", &headers("00"), body).unwrap());
    }

    #[test]
    fn hmac_verify_requires_signature_header() {
        assert!(verify_hmac("secret", &[], br#"{"id":"evt_1"}"#).is_err());
    }
}
