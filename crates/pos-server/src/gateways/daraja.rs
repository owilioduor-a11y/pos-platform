use crate::config::Config;
use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use pos_domain::money::Money;
use pos_domain::payment::{PaymentGateway, PaymentReceipt, PaymentRequest};
use pos_domain::{DomainError, DomainResult, PaymentMethod};
use serde_json::{json, Value};

/// Safaricom Daraja M-Pesa Express (STK Push) client.
pub struct DarajaClient {
    http: reqwest::Client,
    base_url: String,
    consumer_key: String,
    consumer_secret: String,
    passkey: String,
    shortcode: String,
    callback_url: String,
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

impl DarajaClient {
    pub fn new(config: &Config) -> Self {
        let base_url = if config.daraja_env.eq_ignore_ascii_case("production") {
            "https://api.safaricom.co.ke"
        } else {
            "https://sandbox.safaricom.co.ke"
        }
        .to_string();
        Self {
            http: reqwest::Client::new(),
            base_url,
            consumer_key: config.daraja_consumer_key.clone(),
            consumer_secret: config.daraja_consumer_secret.clone(),
            passkey: config.daraja_passkey.clone(),
            shortcode: config.daraja_shortcode.clone(),
            callback_url: config.daraja_callback_url.clone(),
        }
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
            .map_err(|e| DomainError::PaymentDeclined(format!("daraja oauth: {e}")))?;
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
            "CallBackURL": self.callback_url,
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
            .map_err(|e| DomainError::PaymentDeclined(format!("stk push: {e}")))?;

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
        // Daraja callbacks are not signed. We accept the payload and rely on
        // the CheckoutRequestID correlation recorded at STK-push time.
        // Parse check ensures we only ack well-formed callbacks.
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
}
