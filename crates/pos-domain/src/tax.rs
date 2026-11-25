use crate::money::Money;
use crate::{DomainError, DomainResult};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// Whether catalog prices are entered VAT-inclusive or VAT-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxMode {
    /// Shelf prices include tax (common for Kenyan retail).
    Inclusive,
    /// Shelf prices exclude tax; tax is added at checkout.
    Exclusive,
}

/// Result of applying a tax rate to an amount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxBreakdown {
    pub net: Money,
    pub tax: Money,
    pub gross: Money,
}

/// VAT engine. Uses `rust_decimal` for rate math with explicit
/// `MidpointAwayFromZero` rounding — never floats (AGENTS.md rule 1).
#[derive(Debug, Clone)]
pub struct TaxEngine {
    /// Default rate as a fraction, e.g. Decimal::new(16, 2) = 0.16.
    pub default_rate: Decimal,
    pub mode: TaxMode,
}

impl TaxEngine {
    /// Kenya standard VAT: 16%.
    pub fn kenya_vat(mode: TaxMode) -> Self {
        Self {
            default_rate: Decimal::new(16, 2),
            mode,
        }
    }

    pub fn new(rate: Decimal, mode: TaxMode) -> DomainResult<Self> {
        if rate < Decimal::ZERO || rate > Decimal::ONE {
            return Err(DomainError::InvalidTaxRate(rate.to_string()));
        }
        Ok(Self {
            default_rate: rate,
            mode,
        })
    }

    /// Compute net/tax/gross for `amount`.
    /// - Inclusive mode: `amount` is treated as gross.
    /// - Exclusive mode: `amount` is treated as net.
    pub fn compute(&self, amount: &Money) -> DomainResult<TaxBreakdown> {
        self.compute_with_rate(amount, self.default_rate)
    }

    /// Compute with an explicit per-line rate (e.g. zero-rated items).
    pub fn compute_with_rate(&self, amount: &Money, rate: Decimal) -> DomainResult<TaxBreakdown> {
        if rate < Decimal::ZERO || rate > Decimal::ONE {
            return Err(DomainError::InvalidTaxRate(rate.to_string()));
        }
        let minor = Decimal::from(amount.amount_minor);

        let (net_minor, tax_minor, gross_minor) = match self.mode {
            TaxMode::Inclusive => {
                // tax = gross * rate / (1 + rate)
                let tax = if rate.is_zero() {
                    Decimal::ZERO
                } else {
                    minor
                        .checked_mul(rate)
                        .and_then(|v| v.checked_div(Decimal::ONE + rate))
                        .ok_or(DomainError::MoneyOverflow)?
                };
                let tax_minor = round_minor(tax)?;
                let net_minor = amount.amount_minor - tax_minor;
                (net_minor, tax_minor, amount.amount_minor)
            }
            TaxMode::Exclusive => {
                // tax = net * rate; gross = net + tax
                let tax = minor.checked_mul(rate).ok_or(DomainError::MoneyOverflow)?;
                let tax_minor = round_minor(tax)?;
                let gross_minor = amount
                    .amount_minor
                    .checked_add(tax_minor)
                    .ok_or(DomainError::MoneyOverflow)?;
                (amount.amount_minor, tax_minor, gross_minor)
            }
        };

        let currency = amount.currency.clone();
        Ok(TaxBreakdown {
            net: Money::new(net_minor, currency.clone())?,
            tax: Money::new(tax_minor, currency.clone())?,
            gross: Money::new(gross_minor, currency)?,
        })
    }
}

fn round_minor(value: Decimal) -> DomainResult<i64> {
    value
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()
        .ok_or(DomainError::MoneyOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inclusive_vat_16() {
        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        // KSh 116.00 gross -> KSh 100.00 net + KSh 16.00 tax
        let m = Money::new(11600, crate::money::Currency::kes()).unwrap();
        let bd = engine.compute(&m).unwrap();
        assert_eq!(bd.net.amount_minor, 10000);
        assert_eq!(bd.tax.amount_minor, 1600);
        assert_eq!(bd.gross.amount_minor, 11600);
    }

    #[test]
    fn inclusive_rounds_half_away() {
        let engine = TaxEngine::kenya_vat(TaxMode::Inclusive);
        // gross = 1 minor unit -> tax = round(1 * 0.16/1.16) = 0
        let m = Money::new(1, crate::money::Currency::kes()).unwrap();
        let bd = engine.compute(&m).unwrap();
        assert_eq!(bd.tax.amount_minor, 0);
        assert_eq!(bd.net.amount_minor, 1);
    }

    #[test]
    fn exclusive_vat_16() {
        let engine = TaxEngine::kenya_vat(TaxMode::Exclusive);
        // KSh 100.00 net -> KSh 16.00 tax -> KSh 116.00 gross
        let m = Money::new(10000, crate::money::Currency::kes()).unwrap();
        let bd = engine.compute(&m).unwrap();
        assert_eq!(bd.net.amount_minor, 10000);
        assert_eq!(bd.tax.amount_minor, 1600);
        assert_eq!(bd.gross.amount_minor, 11600);
    }

    #[test]
    fn zero_rate() {
        let engine = TaxEngine::new(Decimal::ZERO, TaxMode::Inclusive).unwrap();
        let m = Money::new(5000, crate::money::Currency::kes()).unwrap();
        let bd = engine.compute(&m).unwrap();
        assert_eq!(bd.tax.amount_minor, 0);
        assert_eq!(bd.net.amount_minor, 5000);
        assert_eq!(bd.gross.amount_minor, 5000);
    }

    #[test]
    fn invalid_rate_rejected() {
        assert!(TaxEngine::new(Decimal::new(150, 2), TaxMode::Inclusive).is_err());
    }
}
