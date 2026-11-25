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
            existing.quantity += quantity;
        } else {
            self.items.push(CartItem { product, quantity });
        }
        Ok(())
    }

    pub fn set_adhoc(&mut self, amount: Money) -> DomainResult<()> {
        if amount.is_zero() {
            return Err(DomainError::InvalidAmount("zero amount".into()));
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
            let bd = engine.compute(&line)?;
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
    /// Sum of payment legs.
    pub fn paid_total(&self) -> DomainResult<Money> {
        let mut total: Option<Money> = None;
        for p in &self.payments {
            total = Some(match total {
                Some(t) => t.checked_add(&p.amount)?,
                None => p.amount.clone(),
            });
        }
        total.ok_or(DomainError::EmptyCart)
    }

    pub fn balance_due(&self) -> DomainResult<Money> {
        let paid = self.paid_total()?;
        match self.totals.gross.checked_sub(&paid) {
            Ok(diff) => Ok(diff),
            Err(_) => Ok(Money::zero(self.totals.gross.currency.clone())),
        }
    }

    pub fn is_fully_paid(&self) -> bool {
        matches!(self.balance_due(), Ok(b) if b.is_zero())
    }

    /// Validate that payments exactly cover the gross total.
    pub fn validate_payments(&self) -> DomainResult<()> {
        let paid = self.paid_total()?;
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

/// Build a Sale from a Cart. Caller assigns payments afterwards.
pub fn finalize_sale(
    cart: &Cart,
    merchant_id: Uuid,
    cashier_id: Option<Uuid>,
    engine: &TaxEngine,
    created_at_unix: i64,
) -> DomainResult<Sale> {
    let totals = cart.totals(engine)?;
    Ok(Sale {
        id: Uuid::now_v7(),
        merchant_id,
        cashier_id,
        idempotency_key: Uuid::now_v7(),
        items: cart.items.clone(),
        ad_hoc_amount: cart.ad_hoc_amount.clone(),
        totals,
        payments: Vec::new(),
        created_at_unix,
    })
}
