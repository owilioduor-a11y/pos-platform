//! pos-domain — pure Rust domain models, money/tax math, and payment
//! provider traits. Zero platform/IO dependencies (see AGENTS.md).

pub mod catalog;
pub mod money;
pub mod payment;
pub mod sale;
pub mod tax;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type DomainResult<T> = Result<T, DomainError>;

/// Payment rails supported across all regions.
/// Crypto-backed cards (Coinbase, Crypto.com, Binance, etc.) ride the
/// standard `CardTap` rail — they are ordinary EMV debit/prepaid cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    /// Contactless NFC tap via certified SoftPOS kernel.
    CardTap,
    /// M-Pesa mobile money (Daraja STK Push / C2B).
    Mpesa,
    /// Physical cash tender.
    Cash,
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid amount: {0}")]
    InvalidAmount(String),

    #[error("invalid quantity")]
    InvalidQuantity,

    #[error("cart is empty")]
    EmptyCart,

    #[error("no payments recorded")]
    NoPayments,

    #[error("payment exceeds sale total by {0} minor units")]
    Overpayment(i64),

    #[error("sale payments do not cover the gross total")]
    SaleNotFullyPaid,

    #[error("currency mismatch: expected {expected}, got {got}")]
    CurrencyMismatch { expected: String, got: String },

    #[error("money overflow")]
    MoneyOverflow,

    #[error("invalid currency code: {0}")]
    InvalidCurrency(String),

    #[error("invalid tax rate: {0}")]
    InvalidTaxRate(String),

    #[error("invalid barcode: {0}")]
    InvalidBarcode(String),

    #[error("invalid card metadata: {0}")]
    InvalidCardMetadata(String),

    #[error("insufficient stock for sku {sku}: requested {requested}, available {available}")]
    InsufficientStock {
        sku: String,
        requested: u32,
        available: u32,
    },

    #[error("payment failed: {0}")]
    PaymentDeclined(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("domain: {0}")]
    Other(String),
}
