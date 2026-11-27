use crate::catalog::Product;
use crate::money::Money;
use crate::tax::TaxEngine;
use crate::PaymentMethod;
use crate::{DomainError, DomainResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A line in the cart. `quantity` is a whole-number count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartItem {
    pub product: Product,
    pub quantity: u32,
}

impl CartItem {
    pub fn line_total(&self) -> DomainResult<Money> {
        self.product.price.mul_quantity(self.quantity)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cart {
    pub items: Vec<CartItem>,
    /// Quick-charge ad-hoc amount for Micro merchants (no catalog item).
    pub ad_hoc_amount: Option<Money>,
}

impl Cart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_product(&mut self, product: Product, quantity: u32) -> DomainResult<()> {
        if quantity == 0 {
            return Err(DomainError::InvalidQuantity);
        }
        if let Some(existing) = self.items.iter_mut().find(|i| i.product.id == product.id) {
            existing.quantity = existing
                .quantity
                .checked_add(quantity)
                .ok_or(DomainError::InvalidQuantity)?;
        } else {
            self.items.push(CartItem { product, quantity });
        }
        Ok(())
    }

    pub fn set_adhoc(&mut self, amount: Money) -> DomainResult<()> {
        if !amount.is_positive() {
            return Err(DomainError::InvalidAmount(
                "ad-hoc amount must be positive".into(),
            ));
        }
        self.ad_hoc_amount = Some(amount);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.ad_hoc_amount.is_none()
    }

    /// Sum of all line totals (gross amounts as entered).
    pub fn subtotal(&self) -> DomainResult<Money> {
        let mut total: Option<Money> = None;
        for item in &self.items {
            let line = item.line_total()?;
            total = Some(match total {
                Some(t) => t.checked_add(&line)?,
                None => line,
            });
        }
        if let Some(adhoc) = &self.ad_hoc_amount {
            total = Some(match total {
                Some(t) => t.checked_add(adhoc)?,
                None => adhoc.clone(),
            });
        }
        total.ok_or(DomainError::EmptyCart)
    }

    /// Compute VAT-inclusive totals across all lines.
    pub fn totals(&self, engine: &TaxEngine) -> DomainResult<SaleTotals> {
        if self.is_empty() {
            return Err(DomainError::EmptyCart);
        }
        let mut net_total: Option<Money> = None;
        let mut tax_total: Option<Money> = None;
        let mut gross_total: Option<Money> = None;

        for item in &self.items {
            let line = item.line_total()?;
            // Per-line rate matters for zero-rated/exempt goods.
            let bd = engine.compute_with_rate(&line, item.product.tax_rate)?;
            accumulate(&mut net_total, &bd.net)?;
            accumulate(&mut tax_total, &bd.tax)?;
            accumulate(&mut gross_total, &bd.gross)?;
        }
        if let Some(adhoc) = &self.ad_hoc_amount {
            let bd = engine.compute(adhoc)?;
            accumulate(&mut net_total, &bd.net)?;
            accumulate(&mut tax_total, &bd.tax)?;
            accumulate(&mut gross_total, &bd.gross)?;
        }

        Ok(SaleTotals {
            net: net_total.ok_or(DomainError::EmptyCart)?,
            tax: tax_total.ok_or(DomainError::EmptyCart)?,
            gross: gross_total.ok_or(DomainError::EmptyCart)?,
        })
    }
}

fn accumulate(acc: &mut Option<Money>, next: &Money) -> DomainResult<()> {
    *acc = Some(match acc.take() {
        Some(existing) => existing.checked_add(next)?,
        None => next.clone(),
    });
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleTotals {
    pub net: Money,
    pub tax: Money,
    pub gross: Money,
}

/// One payment leg of a possibly split sale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitPayment {
    pub method: PaymentMethod,
    pub amount: Money,
    /// Processor reference / M-Pesa receipt / masked card auth code.
    /// NEVER a PAN, CVV, or track data.
    pub reference: String,
}

impl SplitPayment {
    pub fn new(
        method: PaymentMethod,
        amount: Money,
        reference: impl Into<String>,
    ) -> DomainResult<Self> {
        if !amount.is_positive() {
            return Err(DomainError::InvalidAmount(
                "payment leg must be positive".into(),
            ));
        }
        let reference = reference.into().trim().to_string();
        if reference.is_empty() {
            return Err(DomainError::InvalidInput(
                "payment reference cannot be empty".into(),
            ));
        }
        Ok(Self {
            method,
            amount,
            reference,
        })
    }
}

/// A completed sale. `id` is a UUIDv7 (time-ordered) and
/// `idempotency_key` is the client-generated key that dedupes retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sale {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub cashier_id: Option<Uuid>,
    pub idempotency_key: Uuid,
    pub items: Vec<CartItem>,
    pub ad_hoc_amount: Option<Money>,
    pub totals: SaleTotals,
    pub payments: Vec<SplitPayment>,
    pub created_at_unix: i64,
}

impl Sale {
    /// Sum of payment legs. No legs means zero paid, not an error.
    pub fn paid_total(&self) -> DomainResult<Money> {
        let mut total = Money::zero(self.totals.gross.currency.clone());
        for p in &self.payments {
            total = total.checked_add(&p.amount)?;
        }
        Ok(total)
    }

    /// Remaining amount to collect. Overpayment is clamped to zero so a sale
    /// never reports a negative balance.
    pub fn balance_due(&self) -> DomainResult<Money> {
        let paid = self.paid_total()?;
        self.totals.gross.clamped_sub_at_zero(&paid)
    }

    pub fn is_fully_paid(&self) -> bool {
        matches!(self.balance_due(), Ok(b) if b.is_zero())
    }

    /// Validate that payments exactly cover the gross total in the same
    /// currency and that every leg is a positive, referenced tender.
    pub fn validate_payments(&self) -> DomainResult<()> {
        if self.payments.is_empty() {
            return Err(DomainError::NoPayments);
        }
        let paid = self.paid_total()?;
        for leg in &self.payments {
            if !leg.amount.is_positive() {
                return Err(DomainError::InvalidAmount(
                    "payment leg must be positive".into(),
                ));
            }
            if leg.amount.currency.code != self.totals.gross.currency.code {
                return Err(DomainError::CurrencyMismatch {
                    expected: self.totals.gross.currency.code.clone(),
                    got: leg.amount.currency.code.clone(),
                });
            }
            if leg.reference.trim().is_empty() {
                return Err(DomainError::InvalidInput(
                    "payment reference cannot be empty".into(),
                ));
            }
        }
        if paid.amount_minor > self.totals.gross.amount_minor {
            return Err(DomainError::Overpayment(
                paid.amount_minor - self.totals.gross.amount_minor,
            ));
        }
        if paid.amount_minor < self.totals.gross.amount_minor {
            return Err(DomainError::SaleNotFullyPaid);
        }
        Ok(())
    }
}

/// Build a Sale from a Cart. `idempotency_key` must be a client-generated
/// UUIDv7 so offline retries never create duplicate sales (AGENTS.md rule 3).
/// Caller assigns payments afterwards.
pub fn finalize_sale(
    cart: &Cart,
    merchant_id: Uuid,
    cashier_id: Option<Uuid>,
    idempotency_key: Uuid,
    engine: &TaxEngine,
    created_at_unix: i64,
) -> DomainResult<Sale> {
    if idempotency_key.get_version_num() != 7 {
        return Err(DomainError::InvalidInput(
            "idempotency key must be a UUIDv7".into(),
        ));
    }
    let totals = cart.totals(engine)?;
    Ok(Sale {
        id: Uuid::now_v7(),
        merchant_id,
        cashier_id,
        idempotency_key,
        items: cart.items.clone(),
        ad_hoc_amount: cart.ad_hoc_amount.clone(),
        totals,
        payments: Vec::new(),
        created_at_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Currency;
    use crate::tax::TaxMode;
    use rust_decimal::Decimal;

    fn sale_with_total(minor: i64) -> Sale {
        let currency = Currency::kes();
        Sale {
            id: Uuid::now_v7(),
            merchant_id: Uuid::now_v7(),
            cashier_id: None,
            idempotency_key: Uuid::now_v7(),
            items: Vec::new(),
            ad_hoc_amount: None,
            totals: SaleTotals {
                net: Money::new(minor, currency.clone()).unwrap(),
                tax: Money::zero(currency.clone()),
                gross: Money::new(minor, currency).unwrap(),
            },
            payments: Vec::new(),
            created_at_unix: 0,
        }
    }

    #[test]
    fn cart_rejects_negative_adhoc() {
        let mut cart = Cart::new();
        let amount = Money::new(-100, Currency::kes()).unwrap();
        assert!(cart.set_adhoc(amount).is_err());
        assert!(cart.set_adhoc(Money::zero(Currency::kes())).is_err());
    }

    #[test]
    fn cart_quantity_addition_is_overflow_safe() {
        let product = Product::new(
            Uuid::now_v7(),
            "SKU",
            "Widget",
            Money::new(100, Currency::kes()).unwrap(),
        )
        .unwrap();
        let mut cart = Cart::new();
        cart.add_product(product.clone(), u32::MAX).unwrap();
        assert!(matches!(
            cart.add_product(product, 1),
            Err(DomainError::InvalidQuantity)
        ));
    }

    #[test]
    fn per_product_tax_rate_is_used() {
        let zero_rated = Product::new_with_tax_rate(
            Uuid::now_v7(),
            "BREAD",
            "Bread",
            Money::new(10000, Currency::kes()).unwrap(),
            Decimal::ZERO,
        )
        .unwrap();
        let mut cart = Cart::new();
        cart.add_product(zero_rated, 1).unwrap();

        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        let totals = cart.totals(&engine).unwrap();
        assert_eq!(totals.gross.amount_minor, 10000);
        assert_eq!(totals.tax.amount_minor, 0);
        assert_eq!(totals.net.amount_minor, 10000);
    }

    #[test]
    fn sale_without_payments_is_not_paid() {
        let sale = sale_with_total(1000);
        assert_eq!(sale.balance_due().unwrap().amount_minor, 1000);
        assert!(!sale.is_fully_paid());
        assert!(matches!(
            sale.validate_payments(),
            Err(DomainError::NoPayments)
        ));
    }

    #[test]
    fn validate_payments_rejects_bad_legs() {
        let mut sale = sale_with_total(1000);

        sale.payments.push(
            SplitPayment::new(
                PaymentMethod::Cash,
                Money::new(1000, Currency::usd()).unwrap(),
                "ref_usd",
            )
            .unwrap(),
        );
        assert!(matches!(
            sale.validate_payments(),
            Err(DomainError::CurrencyMismatch { .. })
        ));

        sale.payments.clear();
        sale.payments.push(
            SplitPayment::new(
                PaymentMethod::Cash,
                Money::new(1100, Currency::kes()).unwrap(),
                "ref_over",
            )
            .unwrap(),
        );
        assert!(matches!(
            sale.validate_payments(),
            Err(DomainError::Overpayment(100))
        ));

        sale.payments.clear();
        sale.payments.push(
            SplitPayment::new(
                PaymentMethod::Cash,
                Money::new(900, Currency::kes()).unwrap(),
                "ref_short",
            )
            .unwrap(),
        );
        assert!(matches!(
            sale.validate_payments(),
            Err(DomainError::SaleNotFullyPaid)
        ));
    }

    #[test]
    fn split_payment_requires_positive_amount_and_reference() {
        let amount = Money::new(100, Currency::kes()).unwrap();
        assert!(SplitPayment::new(PaymentMethod::Cash, amount.clone(), "").is_err());
        assert!(SplitPayment::new(
            PaymentMethod::Cash,
            Money::new(-100, Currency::kes()).unwrap(),
            "ref"
        )
        .is_err());
        assert!(SplitPayment::new(PaymentMethod::Cash, amount, " ref ").is_ok());
    }

    #[test]
    fn finalize_requires_uuid7_idempotency_key() {
        let mut cart = Cart::new();
        cart.set_adhoc(Money::new(1000, Currency::kes()).unwrap())
            .unwrap();
        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);

        assert!(finalize_sale(&cart, Uuid::now_v7(), None, Uuid::nil(), &engine, 0,).is_err());

        let key = Uuid::now_v7();
        let sale = finalize_sale(&cart, Uuid::now_v7(), None, key, &engine, 0).unwrap();
        assert_eq!(sale.idempotency_key, key);
        assert_eq!(sale.idempotency_key.get_version_num(), 7);
    }
}
