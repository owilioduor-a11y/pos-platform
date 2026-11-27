use crate::money::Money;
use crate::DomainResult;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Card network / scheme detected from the EMV application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardScheme {
    Visa,
    Mastercard,
    Amex,
    UnionPay,
    Other,
}

/// Funding type reported by the card's EMV data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FundingType {
    Debit,
    Credit,
    Prepaid,
}

/// Safe EMV receipt metadata. Contains NO PAN, CVV, or track data —
/// only masked/opaque fields safe to store and display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmvCardMetadata {
    pub scheme: CardScheme,
    pub funding: FundingType,
    /// Last 4 digits only, e.g. "4242".
    pub pan_last4: String,
    /// Masked AID, e.g. "A0000000031010" truncated for display.
    pub aid_masked: Option<String>,
    /// Issuer auth code (ARPC/TC auth response code).
    pub auth_code: Option<String>,
    /// EMV application label (e.g. "VISA CREDIT").
    pub app_label: Option<String>,
}

impl EmvCardMetadata {
    /// Build receipt metadata, rejecting anything that looks like a PAN.
    /// Only the last four digits may be retained (AGENTS.md rule 2).
    pub fn new(
        scheme: CardScheme,
        funding: FundingType,
        pan_last4: impl Into<String>,
        aid_masked: Option<String>,
        auth_code: Option<String>,
        app_label: Option<String>,
    ) -> DomainResult<Self> {
        let pan_last4 = pan_last4.into().trim().to_string();
        if pan_last4.len() != 4 || !pan_last4.bytes().all(|b| b.is_ascii_digit()) {
            return Err(crate::DomainError::InvalidCardMetadata(
                "pan_last4 must be exactly four digits".into(),
            ));
        }
        Ok(Self {
            scheme,
            funding,
            pan_last4,
            aid_masked,
            auth_code,
            app_label,
        })
    }
}

/// Result of a contactless tap through a certified SoftPOS kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapResult {
    pub approved: bool,
    pub amount: Money,
    pub metadata: EmvCardMetadata,
    /// Processor transaction reference — opaque, never a PAN.
    pub processor_ref: String,
    pub decline_reason: Option<String>,
}

/// Request to charge a card via the client-side SoftPOS kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapChargeRequest {
    pub amount: Money,
    pub merchant_id: Uuid,
    pub idempotency_key: Uuid,
    /// Human-readable descriptor shown on cardholder statement.
    pub statement_descriptor: String,
}

/// Server-side payment request routed to a regional gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentRequest {
    pub amount: Money,
    pub merchant_id: Uuid,
    pub idempotency_key: Uuid,
    pub method: crate::PaymentMethod,
    /// For M-Pesa: the payer phone number (MSISDN, e.g. 2547XXXXXXXX).
    pub mpesa_phone: Option<String>,
    /// For card: opaque token from the SoftPOS SDK — never raw PAN.
    pub card_token: Option<String>,
}

/// Normalized receipt returned by any PaymentGateway implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentReceipt {
    pub approved: bool,
    pub reference: String,
    pub amount: Money,
    pub metadata: Option<EmvCardMetadata>,
    pub decline_reason: Option<String>,
    pub raw_provider_payload: serde_json::Value,
}

impl PaymentReceipt {
    /// Build a receipt, redacting card secrets from the provider payload
    /// before it can be logged or persisted.
    pub fn new(
        approved: bool,
        reference: impl Into<String>,
        amount: Money,
        metadata: Option<EmvCardMetadata>,
        decline_reason: Option<String>,
        mut raw_provider_payload: serde_json::Value,
    ) -> Self {
        redact_sensitive(&mut raw_provider_payload);
        Self {
            approved,
            reference: reference.into(),
            amount,
            metadata,
            decline_reason,
            raw_provider_payload,
        }
    }
}

/// Recursively remove PAN/CVV/track fields from provider JSON. Keys are
/// compared case-insensitively after stripping separators.
pub fn redact_sensitive(value: &mut serde_json::Value) {
    const REDACTED_KEYS: [&str; 7] = [
        "pan",
        "cardnumber",
        "cvv",
        "cvc",
        "track1",
        "track2",
        "trackdata",
    ];
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let normalized: String = key
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                if REDACTED_KEYS.contains(&normalized.as_str()) {
                    *child = serde_json::Value::String("[REDACTED]".into());
                } else {
                    redact_sensitive(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_sensitive(item);
            }
        }
        _ => {}
    }
}

/// Server-side abstraction over regional payment providers
/// (Daraja M-Pesa, Kenyan acquirers, Stripe, Adyen, NMI).
#[async_trait::async_trait]
pub trait PaymentGateway: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn charge(&self, req: &PaymentRequest) -> DomainResult<PaymentReceipt>;
    async fn refund(&self, reference: &str, amount: &Money) -> DomainResult<PaymentReceipt>;
    fn verify_webhook(&self, headers: &[(String, String)], body: &[u8]) -> DomainResult<bool>;
}

/// Client-side abstraction over certified SoftPOS SDKs.
/// Implementations wrap vendor AARs. NEVER implement raw NFC/EMV APDU logic.
#[async_trait::async_trait]
pub trait TapToPayKernel: Send + Sync {
    fn kernel_name(&self) -> &'static str;
    async fn is_available(&self) -> bool;
    async fn tap_to_pay(&self, req: &TapChargeRequest) -> DomainResult<TapResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Currency;

    #[test]
    fn emv_metadata_accepts_last4_only() {
        let metadata = EmvCardMetadata::new(
            CardScheme::Visa,
            FundingType::Credit,
            "4242",
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(metadata.pan_last4, "4242");
    }

    #[test]
    fn emv_metadata_rejects_full_pan() {
        assert!(EmvCardMetadata::new(
            CardScheme::Visa,
            FundingType::Credit,
            "4242424242424242",
            None,
            None,
            None,
        )
        .is_err());
        assert!(EmvCardMetadata::new(
            CardScheme::Visa,
            FundingType::Credit,
            "42a2",
            None,
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn receipt_redacts_nested_card_secrets() {
        let payload = serde_json::json!({
            "approved": true,
            "card": { "pan": "4242424242424242", "CVV": "123" },
            "items": [{ "track2": "12345678901234567890" }],
        });
        let receipt = PaymentReceipt::new(
            true,
            "ref_1",
            Money::new(100, Currency::kes()).unwrap(),
            None,
            None,
            payload,
        );
        let card = receipt.raw_provider_payload.get("card").unwrap();
        assert_eq!(card["pan"], "[REDACTED]");
        assert_eq!(card["CVV"], "[REDACTED]");
        assert_eq!(
            receipt.raw_provider_payload["items"][0]["track2"],
            "[REDACTED]"
        );
        assert_eq!(receipt.raw_provider_payload["approved"], true);
    }
}
