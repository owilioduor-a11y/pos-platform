uniffi::setup_scaffolding!();

pub mod db;
mod repository;
pub mod sync;

use pos_domain::catalog::Product;
use pos_domain::money::{Currency, Money};
use pos_domain::payment::{CardScheme, EmvCardMetadata, FundingType};
use pos_domain::sale::{finalize_sale, Cart, SplitPayment};
use pos_domain::tax::{TaxEngine, TaxMode};
use pos_domain::{DomainError, DomainResult, PaymentMethod};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
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

fn parse_idempotency_key(raw: &str) -> Result<Uuid, PosCoreError> {
    let key = raw
        .trim()
        .parse::<Uuid>()
        .map_err(|_| PosCoreError::InvalidInput("idempotency_key must be a UUID".into()))?;
    if key.get_version_num() != 7 {
        return Err(PosCoreError::InvalidInput(
            "idempotency_key must be UUIDv7".into(),
        ));
    }
    Ok(key)
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

/// Collapse cart lines into one stock deduction per product. The repository
/// treats a NULL `stock_qty` as untracked, so the cart snapshot must not decide
/// which lines are deducted.
fn stock_requests(cart: &Cart) -> DomainResult<Vec<(Uuid, u32)>> {
    let mut totals: HashMap<Uuid, u32> = HashMap::new();
    for item in &cart.items {
        let total = totals.entry(item.product.id).or_insert(0);
        *total = total
            .checked_add(item.quantity)
            .ok_or(DomainError::InvalidQuantity)?;
    }
    Ok(totals.into_iter().collect())
}

/// Offline-first POS engine exposed to Android via UniFFI.
/// All money amounts are integer minor units (e.g. cents).
#[derive(uniffi::Object)]
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
    ) -> Result<Arc<Self>, PosCoreError> {
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
        Ok(Arc::new(Self {
            pool,
            runtime,
            merchant_id,
            server_url,
            api_token,
            cart: Mutex::new(Cart::new()),
        }))
    }

    /// Register (upsert) a product into the offline catalog cache.
    /// Returns the product UUID as a string.
    #[allow(clippy::too_many_arguments)]
    pub fn register_product(
        &self,
        sku: String,
        name: String,
        price_minor: i64,
        currency_code: String,
        barcode: Option<String>,
        stock_qty: Option<u32>,
        tax_rate: Option<String>,
    ) -> Result<String, PosCoreError> {
        let currency = currency_for(&currency_code)?;
        let price = Money::new(price_minor, currency).map_err(domain_err)?;
        let tax_rate = match tax_rate {
            Some(raw) => raw
                .trim()
                .parse::<rust_decimal::Decimal>()
                .map_err(|_| PosCoreError::InvalidInput(format!("invalid tax_rate: {raw}")))?,
            None => rust_decimal::Decimal::new(16, 2), // Kenya standard VAT
        };
        if tax_rate < rust_decimal::Decimal::ZERO || tax_rate > rust_decimal::Decimal::ONE {
            return Err(PosCoreError::InvalidInput(format!(
                "tax_rate out of range: {tax_rate}"
            )));
        }
        let mut product = Product::new_with_tax_rate(self.merchant_id, sku, name, price, tax_rate)
            .map_err(domain_err)?;
        product.barcode = barcode
            .map(|b| pos_domain::catalog::Barcode::parse(&b).map(|x| x.0))
            .transpose()
            .map_err(domain_err)?;
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
    pub fn set_adhoc_amount(
        &self,
        amount_minor: i64,
        currency_code: String,
    ) -> Result<(), PosCoreError> {
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

    /// Generate a client-side UUIDv7 key for the next checkout attempt.
    /// Reusing this key on a retry deduplicates the sale on the server.
    pub fn new_idempotency_key(&self) -> String {
        Uuid::now_v7().to_string()
    }

    /// Finalize the cart into a Sale and persist it offline.
    /// `payments_json` is a JSON array of legs:
    /// `[{"method":"card_tap","amount_minor":12500,"reference":"..."}, ...]`
    /// `idempotency_key` must come from `new_idempotency_key()`.
    /// Returns the sale UUID. The sale is enqueued for server sync.
    pub fn checkout(
        &self,
        payments_json: String,
        idempotency_key: String,
    ) -> Result<String, PosCoreError> {
        let legs: Vec<PaymentLegIn> = serde_json::from_str(&payments_json)
            .map_err(|e| PosCoreError::InvalidInput(format!("payments_json: {e}")))?;
        let idempotency_key = parse_idempotency_key(&idempotency_key)?;

        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        let cart = lock_cart(&self.cart).clone();
        if cart.is_empty() {
            return Err(PosCoreError::InvalidInput("cart is empty".into()));
        }

        let mut sale = finalize_sale(
            &cart,
            self.merchant_id,
            None,
            idempotency_key,
            &engine,
            unix_now(),
        )
        .map_err(domain_err)?;

        let currency = sale.totals.gross.currency.clone();
        sale.payments = legs
            .into_iter()
            .map(|leg| {
                SplitPayment::new(
                    parse_method(&leg.method)?,
                    Money::new(leg.amount_minor, currency.clone()).map_err(domain_err)?,
                    leg.reference,
                )
                .map_err(domain_err)
            })
            .collect::<Result<Vec<_>, PosCoreError>>()?;
        sale.validate_payments().map_err(domain_err)?;

        let payload = serde_json::to_string(&sale)
            .map_err(|e| PosCoreError::Database(format!("serialize sale: {e}")))?;

        let requests = stock_requests(&cart).map_err(domain_err)?;
        let outcome = self
            .runtime
            .block_on(repository::commit_checkout(
                &self.pool, &sale, &payload, &requests,
            ))
            .map_err(db_err)?;

        match outcome {
            repository::CheckoutCommit::Inserted => {
                *lock_cart(&self.cart) = Cart::new();
                Ok(sale.id.to_string())
            }
            repository::CheckoutCommit::Duplicate(existing_id) => {
                // Retry with the same key: keep the original sale and do not
                // deduct stock or enqueue a second outbox row.
                *lock_cart(&self.cart) = Cart::new();
                Ok(existing_id.to_string())
            }
            repository::CheckoutCommit::StockShortage(shortage) => {
                // Keep the cart so the cashier can adjust the quantity.
                Err(domain_err(DomainError::InsufficientStock {
                    sku: shortage.sku,
                    requested: shortage.requested,
                    available: shortage.available,
                }))
            }
        }
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
        let metadata = EmvCardMetadata::new(
            parse_scheme(&scheme),
            parse_funding(&funding),
            pan_last4,
            None,
            auth_code,
            None,
        )
        .map_err(domain_err)?;
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
        self.runtime
            .block_on(sync::push_pending(
                &self.pool,
                &self.server_url,
                &self.api_token,
            ))
            .map_err(|e| PosCoreError::Sync(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn test_core() -> Arc<PosCore> {
        PosCore::new(
            ":memory:".into(),
            Uuid::now_v7().to_string(),
            "http://127.0.0.1:1".into(),
            "test-token".into(),
        )
        .unwrap()
    }

    fn count_rows(core: &PosCore, table: &str) -> i64 {
        let query = format!("SELECT COUNT(*) FROM {table}");
        core.runtime
            .block_on(sqlx::query_scalar::<_, i64>(&query).fetch_one(&core.pool))
            .unwrap()
    }

    #[test]
    fn checkout_retry_returns_original_sale_without_duplicate_outbox() {
        let core = test_core();
        let key = core.new_idempotency_key();
        let legs = r#"[{"method":"cash","amount_minor":1000,"reference":"cash_1"}]"#;

        core.set_adhoc_amount(1000, "KES".into()).unwrap();
        let first = core.checkout(legs.to_string(), key.clone()).unwrap();
        assert_eq!(count_rows(&core, "sync_outbox"), 1);

        // A retry with the same key must not create a second sale or event.
        core.set_adhoc_amount(1000, "KES".into()).unwrap();
        let second = core.checkout(legs.to_string(), key).unwrap();
        assert_eq!(first, second);
        assert_eq!(count_rows(&core, "sales"), 1);
        assert_eq!(count_rows(&core, "sync_outbox"), 1);
    }

    #[test]
    fn register_product_honours_zero_rated_tax_rate() {
        let core = test_core();
        let id = core
            .register_product(
                "BREAD".into(),
                "Bread".into(),
                10000,
                "KES".into(),
                Some("5901234123457".into()),
                Some(5),
                Some("0".into()),
            )
            .unwrap();

        let product = core
            .runtime
            .block_on(repository::get_product(
                &core.pool,
                core.merchant_id,
                id.parse::<Uuid>().unwrap(),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(product.tax_rate, Decimal::ZERO);

        core.add_product_to_cart(id, 1).unwrap();
        assert_eq!(core.cart_total_minor().unwrap(), 10000);
    }

    #[test]
    fn register_product_rejects_invalid_tax_rate() {
        let core = test_core();
        assert!(core
            .register_product(
                "BAD".into(),
                "Bad".into(),
                100,
                "KES".into(),
                None,
                None,
                Some("1.5".into()),
            )
            .is_err());
    }

    #[test]
    fn record_tap_rejects_full_pan() {
        let core = test_core();
        assert!(core
            .record_tap(
                1000,
                "KES".into(),
                "visa".into(),
                "credit".into(),
                "4242424242424242".into(),
                None,
                "ref_1".into(),
            )
            .is_err());
        assert_eq!(count_rows(&core, "sync_outbox"), 0);
    }

    fn stock_for(core: &PosCore, product_id: &str) -> Option<i64> {
        core.runtime
            .block_on(
                sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT stock_qty FROM products_cache WHERE id = ?",
                )
                .bind(product_id)
                .fetch_one(&core.pool),
            )
            .unwrap()
    }

    #[test]
    fn checkout_deducts_tracked_stock_once_per_idempotency_key() {
        let core = test_core();
        let id = core
            .register_product(
                "WIDGET".into(),
                "Widget".into(),
                1000,
                "KES".into(),
                None,
                Some(5),
                None,
            )
            .unwrap();
        let legs = r#"[{"method":"cash","amount_minor":3000,"reference":"cash_1"}]"#;
        let key = core.new_idempotency_key();

        core.add_product_to_cart(id.clone(), 3).unwrap();
        core.checkout(legs.to_string(), key.clone()).unwrap();
        assert_eq!(stock_for(&core, &id), Some(2));

        // Retry must not deduct stock a second time.
        core.add_product_to_cart(id.clone(), 3).unwrap();
        core.checkout(legs.to_string(), key).unwrap();
        assert_eq!(stock_for(&core, &id), Some(2));
    }

    #[test]
    fn checkout_rejects_overselling_and_keeps_cart() {
        let core = test_core();
        let id = core
            .register_product(
                "LIMITED".into(),
                "Limited".into(),
                1000,
                "KES".into(),
                None,
                Some(2),
                None,
            )
            .unwrap();
        let legs = r#"[{"method":"cash","amount_minor":3000,"reference":"cash_1"}]"#;

        core.add_product_to_cart(id.clone(), 3).unwrap();
        let result = core.checkout(legs.to_string(), core.new_idempotency_key());
        assert!(matches!(
            result,
            Err(PosCoreError::Domain(message)) if message.contains("insufficient stock")
        ));
        assert_eq!(stock_for(&core, &id), Some(2));
        assert_eq!(count_rows(&core, "sync_outbox"), 0);
        assert_eq!(core.cart_summary(), "items=1, adhoc=false");
    }

    #[test]
    fn checkout_uses_database_stock_even_if_cart_snapshot_was_untracked() {
        let core = test_core();
        let id = core
            .register_product(
                "LATER".into(),
                "Later".into(),
                1000,
                "KES".into(),
                None,
                None,
                None,
            )
            .unwrap();
        let legs = r#"[{"method":"cash","amount_minor":3000,"reference":"cash_1"}]"#;

        // Cart copies the product while stock is untracked.
        core.add_product_to_cart(id.clone(), 3).unwrap();
        core.runtime
            .block_on(
                sqlx::query("UPDATE products_cache SET stock_qty = ? WHERE id = ?")
                    .bind(5_i64)
                    .bind(&id)
                    .execute(&core.pool),
            )
            .unwrap();

        core.checkout(legs.to_string(), core.new_idempotency_key())
            .unwrap();
        assert_eq!(stock_for(&core, &id), Some(2));
    }
}
