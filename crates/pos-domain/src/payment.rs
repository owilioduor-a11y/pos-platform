use crate::money::Money;
use crate::{DomainError, DomainResult};
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
