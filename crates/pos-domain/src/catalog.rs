use crate::money::Money;
use crate::{DomainError, DomainResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Business mode a merchant operates in.
/// - `Micro`: Quick Charge only — ad-hoc amounts, no catalog requirement.
/// - `Retail`: Full catalog, barcode scanning, stock, multi-cashier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MerchantMode {
    Micro,
    Retail,
}

/// A merchant profile. The POS app uses `mode` to pick the default screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerchantProfile {
    pub id: Uuid,
    pub business_name: String,
    pub mode: MerchantMode,
    /// ISO 4217 currency the merchant prices in.
    pub currency: crate::money::Currency,
    /// KRA PIN for eTIMS tax invoice submission (Phase 2).
    pub kra_pin: Option<String>,
}

/// A validated barcode (EAN-8, UPC-A, EAN-13, or Code-128 digit string).
/// Stored as the raw scanned string; `parse` enforces sane length/charset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Barcode(pub String);

impl Barcode {
    pub fn parse(raw: &str) -> DomainResult<Self> {
        let trimmed = raw.trim();
        let valid_len = matches!(trimmed.len(), 8 | 12 | 13 | 14);
        if !valid_len || !trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(DomainError::InvalidBarcode(trimmed.to_string()));
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl std::ops::Deref for Barcode {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// A sellable catalog item. `price` is VAT-inclusive when the merchant's
/// `TaxMode` is `Inclusive`. `tax_rate` is stored as a fraction (0.16 = 16%)
/// so zero-rated/exempt items can carry their own rate per KENYA_LAUNCH.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    pub id: Uuid,
    pub merchant_id: Uuid,
    pub sku: String,
    pub barcode: Option<String>,
    pub name: String,
    pub price: Money,
    pub tax_rate: Decimal,
    pub stock_qty: Option<u32>,
    pub active: bool,
}

impl Product {
    /// Create a product with Kenya's standard 16% VAT rate.
    pub fn new(
        merchant_id: Uuid,
        sku: impl Into<String>,
        name: impl Into<String>,
        price: Money,
    ) -> DomainResult<Self> {
        Self::new_with_tax_rate(merchant_id, sku, name, price, Decimal::new(16, 2))
    }

    /// Create a product with an explicit rate so zero-rated or exempt items
    /// are taxed correctly (KENYA_LAUNCH.md §3).
    pub fn new_with_tax_rate(
        merchant_id: Uuid,
        sku: impl Into<String>,
        name: impl Into<String>,
        price: Money,
        tax_rate: Decimal,
    ) -> DomainResult<Self> {
        let sku = sku.into().trim().to_string();
        let name = name.into().trim().to_string();
        if sku.is_empty() {
            return Err(DomainError::InvalidInput("sku cannot be empty".into()));
        }
        if name.is_empty() {
            return Err(DomainError::InvalidInput("name cannot be empty".into()));
        }
        if price.is_negative() {
            return Err(DomainError::InvalidAmount("negative price".into()));
        }
        if tax_rate < Decimal::ZERO || tax_rate > Decimal::ONE {
            return Err(DomainError::InvalidTaxRate(tax_rate.to_string()));
        }
        Ok(Self {
            id: Uuid::now_v7(),
            merchant_id,
            sku,
            barcode: None,
            name,
            price,
            tax_rate,
            stock_qty: None,
            active: true,
        })
    }

    /// Deduct sold quantity from stock. Fails if stock is tracked and
    /// insufficient — prevents overselling offline.
    pub fn deduct_stock(&mut self, qty: u32) -> DomainResult<()> {
        if let Some(stock) = self.stock_qty {
            if stock < qty {
                return Err(DomainError::InsufficientStock {
                    sku: self.sku.clone(),
                    requested: qty,
                    available: stock,
                });
            }
            self.stock_qty = Some(stock - qty);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barcode_lengths() {
        assert!(Barcode::parse("5901234123457").is_ok()); // EAN-13
        assert!(Barcode::parse("01234565").is_ok()); // EAN-8
        assert!(Barcode::parse("123456789012").is_ok()); // UPC-A
        assert!(Barcode::parse("abc").is_err());
        assert!(Barcode::parse("12345678901234567890").is_err());
    }

    #[test]
    fn product_requires_names() {
        let m = Money::new(100, crate::money::Currency::kes()).unwrap();
        let mid = Uuid::now_v7();
        assert!(Product::new(mid, "", "Name", m.clone()).is_err());
        assert!(Product::new(mid, "SKU", "", m).is_err());
    }

    #[test]
    fn tax_rate_is_validated() {
        let m = Money::new(100, crate::money::Currency::kes()).unwrap();
        let mid = Uuid::now_v7();
        assert!(Product::new_with_tax_rate(mid, "ZERO", "Bread", m.clone(), Decimal::ZERO).is_ok());
        assert!(Product::new_with_tax_rate(mid, "BAD", "Bad", m, Decimal::new(150, 2)).is_err());
    }

    #[test]
    fn stock_deduction_guarded() {
        let m = Money::new(100, crate::money::Currency::kes()).unwrap();
        let mut p = Product::new(Uuid::now_v7(), "SKU1", "Widget", m).unwrap();
        p.stock_qty = Some(5);
        p.deduct_stock(3).unwrap();
        assert_eq!(p.stock_qty, Some(2));
        assert!(p.deduct_stock(3).is_err());
        // Untracked stock never fails.
        p.stock_qty = None;
        assert!(p.deduct_stock(999).is_ok());
    }
}
