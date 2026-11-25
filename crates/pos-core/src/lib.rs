uniffi::setup_scaffolding!();

pub mod db;
mod repository;
pub mod sync;

use pos_domain::catalog::Product;
use pos_domain::money::{Currency, Money};
use pos_domain::payment::{CardScheme, EmvCardMetadata, FundingType};
use pos_domain::sale::{finalize_sale, Cart, SplitPayment};
use pos_domain::tax::{TaxEngine, TaxMode};
use pos_domain::{DomainError, PaymentMethod};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::sync::{Mutex, PoisonError};
use uuid::Uuid;

/// Errors surfaced to the Kotlin/Android binding layer.
#[derive(Debug, uniffi::Error)]
pub enum PosCoreError {
    Database(String),
    Domain(String),
    Sync(String),
    NotFound,
    InvalidInput(String),
}

impl std::fmt::Display for PosCoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(m) => write!(f, "database error: {m}"),
            Self::Domain(m) => write!(f, "domain error: {m}"),
            Self::Sync(m) => write!(f, "sync error: {m}"),
            Self::NotFound => write!(f, "not found"),
            Self::InvalidInput(m) => write!(f, "invalid input: {m}"),
        }
    }
}

impl std::error::Error for PosCoreError {}

fn domain_err(e: DomainError) -> PosCoreError {
    PosCoreError::Domain(e.to_string())
}

fn db_err(e: anyhow::Error) -> PosCoreError {
    PosCoreError::Database(e.to_string())
}

fn lock_cart(cart: &Mutex<Cart>) -> std::sync::MutexGuard<'_, Cart> {
    cart.lock().unwrap_or_else(PoisonError::into_inner)
}

fn currency_for(code: &str) -> Result<Currency, PosCoreError> {
    match code.trim().to_ascii_uppercase().as_str() {
        "KES" => Ok(Currency::kes()),
        "USD" => Ok(Currency::usd()),
        "EUR" => Ok(Currency::eur()),
        "GBP" => Ok(Currency::gbp()),
        other => Currency::new(other, 2, other).map_err(domain_err),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Deserialize)]
struct PaymentLegIn {
    method: String,
    amount_minor: i64,
    reference: String,
}

fn parse_method(raw: &str) -> Result<PaymentMethod, PosCoreError> {
    match raw.to_ascii_lowercase().as_str() {
        "card_tap" | "cardtap" | "card" => Ok(PaymentMethod::CardTap),
        "mpesa" | "m_pesa" | "mobile_money" => Ok(PaymentMethod::Mpesa),
        "cash" => Ok(PaymentMethod::Cash),
        other => Err(PosCoreError::InvalidInput(format!(
            "unknown method: {other}"
        ))),
    }
}

fn parse_scheme(raw: &str) -> CardScheme {
    match raw.to_ascii_lowercase().as_str() {
        "visa" => CardScheme::Visa,
        "mastercard" | "master" => CardScheme::Mastercard,
        "amex" | "american_express" => CardScheme::Amex,
        "unionpay" => CardScheme::UnionPay,
        _ => CardScheme::Other,
    }
}

fn parse_funding(raw: &str) -> FundingType {
    match raw.to_ascii_lowercase().as_str() {
        "credit" => FundingType::Credit,
        "prepaid" => FundingType::Prepaid,
        _ => FundingType::Debit,
    }
}

/// Offline-first POS engine exposed to Android via UniFFI.
/// All money amounts are integer minor units (e.g. cents).
pub struct PosCore {
    pool: SqlitePool,
    runtime: tokio::runtime::Runtime,
    merchant_id: Uuid,
    server_url: String,
    api_token: String,
    cart: Mutex<Cart>,
}

#[uniffi::export]
impl PosCore {
    #[uniffi::constructor]
    pub fn new(
        db_path: String,
        merchant_id: String,
        server_url: String,
        api_token: String,
    ) -> Result<Self, PosCoreError> {
        let merchant_id = merchant_id
            .parse::<Uuid>()
            .map_err(|_| PosCoreError::InvalidInput("merchant_id must be a UUID".into()))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| PosCoreError::Database(format!("tokio runtime: {e}")))?;
        let pool = runtime
            .block_on(db::create_pool(&db_path))
            .map_err(|e| PosCoreError::Database(e.to_string()))?;
        Ok(Self {
            pool,
            runtime,
            merchant_id,
            server_url,
            api_token,
            cart: Mutex::new(Cart::new()),
        })
    }

    /// Register (upsert) a product into the offline catalog cache.
    /// Returns the product UUID as a string.
    pub fn register_product(
        &self,
        sku: String,
        name: String,
        price_minor: i64,
        currency_code: String,
        barcode: Option<String>,
        stock_qty: Option<u32>,
    ) -> Result<String, PosCoreError> {
        let currency = currency_for(&currency_code)?;
        let price = Money::new(price_minor, currency).map_err(domain_err)?;
        let mut product = Product::new(self.merchant_id, sku, name, price).map_err(domain_err)?;
        product.barcode = barcode
            .map(|b| pos_domain::catalog::Barcode::parse(&b).map(|x| x.0))
            .transpose()
            .map_err(domain_err)?;
        product.tax_rate = rust_decimal::Decimal::new(16, 2); // Kenya VAT 16%
        product.stock_qty = stock_qty;
        let id = product.id.to_string();
        self.runtime
            .block_on(repository::upsert_product(&self.pool, &product))
            .map_err(db_err)?;
        Ok(id)
    }

    /// Look up a cached product by scanned barcode and add it to the cart.
    /// Returns `true` if the barcode was found.
    pub fn scan_barcode(&self, barcode: String) -> Result<bool, PosCoreError> {
        let product = self
            .runtime
            .block_on(repository::find_product_by_barcode(
                &self.pool,
                self.merchant_id,
                &barcode,
            ))
            .map_err(db_err)?;
        match product {
            Some(p) => {
                lock_cart(&self.cart)
                    .add_product(p, 1)
                    .map_err(domain_err)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Add a catalog product to the in-memory cart by product UUID.
    pub fn add_product_to_cart(
        &self,
        product_id: String,
        quantity: u32,
    ) -> Result<(), PosCoreError> {
        let id = product_id
            .parse::<Uuid>()
            .map_err(|_| PosCoreError::InvalidInput("product_id must be a UUID".into()))?;
        let product = self
            .runtime
            .block_on(repository::get_product(&self.pool, self.merchant_id, id))
            .map_err(db_err)?
            .ok_or(PosCoreError::NotFound)?;
        lock_cart(&self.cart)
            .add_product(product, quantity)
            .map_err(domain_err)
    }

    /// Set an ad-hoc amount for Micro-merchant Quick Charge mode.
    pub fn set_adhoc_amount(&self, amount_minor: i64, currency_code: String) -> Result<(), PosCoreError> {
        let currency = currency_for(&currency_code)?;
        let amount = Money::new(amount_minor, currency).map_err(domain_err)?;
        lock_cart(&self.cart).set_adhoc(amount).map_err(domain_err)
    }

    /// Clear the current cart.
    pub fn clear_cart(&self) {
        *lock_cart(&self.cart) = Cart::new();
    }

    /// Gross total of the current cart in minor units (VAT-inclusive).
    pub fn cart_total_minor(&self) -> Result<i64, PosCoreError> {
        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        let cart = lock_cart(&self.cart);
        if cart.is_empty() {
            return Err(PosCoreError::InvalidInput("cart is empty".into()));
        }
        cart.totals(&engine)
            .map(|t| t.gross.amount_minor)
            .map_err(domain_err)
    }

    /// Summary string of the current cart contents.
    pub fn cart_summary(&self) -> String {
        let cart = lock_cart(&self.cart);
        format!(
            "items={}, adhoc={}",
            cart.items.len(),
            cart.ad_hoc_amount.is_some()
        )
    }

    /// Finalize the cart into a Sale and persist it offline.
    /// `payments_json` is a JSON array of legs:
    /// `[{"method":"card_tap","amount_minor":12500,"reference":"..."}, ...]`
    /// Returns the sale UUID. The sale is enqueued for server sync.
    pub fn checkout(&self, payments_json: String) -> Result<String, PosCoreError> {
        let legs: Vec<PaymentLegIn> = serde_json::from_str(&payments_json)
            .map_err(|e| PosCoreError::InvalidInput(format!("payments_json: {e}")))?;

        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        let cart = lock_cart(&self.cart).clone();
        if cart.is_empty() {
            return Err(PosCoreError::InvalidInput("cart is empty".into()));
        }

        let mut sale = finalize_sale(&cart, self.merchant_id, None, &engine, unix_now())
            .map_err(domain_err)?;

        let currency = sale.totals.gross.currency.clone();
        sale.payments = legs
            .into_iter()
            .map(|leg| {
                Ok(SplitPayment {
                    method: parse_method(&leg.method)?,
                    amount: Money::new(leg.amount_minor, currency.clone()).map_err(domain_err)?,
                    reference: leg.reference,
                })
            })
            .collect::<Result<Vec<_>, PosCoreError>>()?;
        sale.validate_payments().map_err(domain_err)?;

        let payload = serde_json::to_string(&sale)
            .map_err(|e| PosCoreError::Database(format!("serialize sale: {e}")))?;

        self.runtime
            .block_on(async {
                repository::insert_sale(&self.pool, &sale).await?;
                repository::enqueue_outbox(&self.pool, "sale", &payload).await?;
                Ok::<(), anyhow::Error>(())
            })
            .map_err(db_err)?;

        *lock_cart(&self.cart) = Cart::new();
        Ok(sale.id.to_string())
    }

    /// Record an approved NFC tap result for audit/sync. NEVER pass PAN,
    /// CVV, or track data — only masked EMV metadata.
    /// Returns the outbox row id.
    #[allow(clippy::too_many_arguments)]
    pub fn record_tap(
        &self,
        amount_minor: i64,
        currency_code: String,
        scheme: String,
        funding: String,
        pan_last4: String,
        auth_code: Option<String>,
        processor_ref: String,
    ) -> Result<i64, PosCoreError> {
        let currency = currency_for(&currency_code)?;
        let metadata = EmvCardMetadata {
            scheme: parse_scheme(&scheme),
            funding: parse_funding(&funding),
            pan_last4,
            aid_masked: None,
            auth_code,
            app_label: None,
        };
        let payload = serde_json::json!({
            "amount_minor": amount_minor,
            "currency": currency.code,
            "metadata": serde_json::to_value(&metadata).unwrap_or(serde_json::Value::Null),
            "processor_ref": processor_ref,
            "recorded_at_unix": unix_now(),
        });
        self.runtime
            .block_on(repository::enqueue_outbox(
                &self.pool,
                "tap",
                &payload.to_string(),
            ))
            .map_err(db_err)
    }

    /// Drain the sync outbox to pos-server. Returns rows synced.
    pub fn sync_pending(&self) -> Result<u32, PosCoreError> {
        let synced = self
            .runtime
            .block_on(sync::push_pending(
                &self.pool,
                &self.server_url,
                &self.api_token,
            ))
            .map_err(|e| PosCoreError::Sync(e.to_string()))?;
        Ok(synced as u32)
    }
}
