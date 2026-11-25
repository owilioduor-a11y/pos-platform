use crate::{DomainError, DomainResult};
use serde::{Deserialize, Serialize};
use std::fmt;

/// ISO 4217 currency with its minor-unit exponent.
/// All monetary math in this crate uses integer minor units only —
/// never f32/f64 (see AGENTS.md financial math rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency {
    /// ISO 4217 alpha-3 code, e.g. "KES".
    pub code: String,
    /// Number of decimal digits in the minor unit (e.g. 2 for cents).
    pub exponent: u32,
    /// Display symbol, e.g. "KSh" or "$". Empty means code-only display.
    pub symbol: String,
}

impl Currency {
    pub fn new(code: &str, exponent: u32, symbol: &str) -> DomainResult<Self> {
        let code = code.trim().to_ascii_uppercase();
        if code.len() != 3 || !code.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(DomainError::InvalidCurrency(code));
        }
        Ok(Self {
            code,
            exponent,
            symbol: symbol.to_string(),
        })
    }

    pub fn kes() -> Self {
        Self {
            code: "KES".into(),
            exponent: 2,
            symbol: "KSh".into(),
        }
    }

    pub fn usd() -> Self {
        Self {
            code: "USD".into(),
            exponent: 2,
            symbol: "$".into(),
        }
    }

    pub fn eur() -> Self {
        Self {
            code: "EUR".into(),
            exponent: 2,
            symbol: "€".into(),
        }
    }

    pub fn gbp() -> Self {
        Self {
            code: "GBP".into(),
            exponent: 2,
            symbol: "£".into(),
        }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)
    }
}

/// An amount of money in integer minor units (e.g. cents / pesewa).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub amount_minor: i64,
    pub currency: Currency,
}

impl Money {
    pub fn new(amount_minor: i64, currency: Currency) -> DomainResult<Self> {
        Ok(Self {
            amount_minor,
            currency,
        })
    }

    pub fn zero(currency: Currency) -> Self {
        Self {
            amount_minor: 0,
            currency,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.amount_minor == 0
    }

    pub fn is_positive(&self) -> bool {
        self.amount_minor > 0
    }

    /// Multiply by a whole-number quantity with overflow checking.
    pub fn mul_quantity(&self, qty: u32) -> DomainResult<Self> {
        let amount = self
            .amount_minor
            .checked_mul(i64::from(qty))
            .ok_or(DomainError::MoneyOverflow)?;
        Self::new(amount, self.currency.clone())
    }

    fn ensure_same_currency(&self, other: &Money) -> DomainResult<()> {
        if self.currency.code != other.currency.code {
            return Err(DomainError::CurrencyMismatch {
                expected: self.currency.code.clone(),
                got: other.currency.code.clone(),
            });
        }
        Ok(())
    }

    pub fn checked_add(&self, other: &Money) -> DomainResult<Self> {
        self.ensure_same_currency(other)?;
        let amount = self
            .amount_minor
            .checked_add(other.amount_minor)
            .ok_or(DomainError::MoneyOverflow)?;
        Self::new(amount, self.currency.clone())
    }

    /// Subtract `other`. Fails on underflow (negative result) or overflow.
    pub fn checked_sub(&self, other: &Money) -> DomainResult<Self> {
        self.ensure_same_currency(other)?;
        let amount = self
            .amount_minor
            .checked_sub(other.amount_minor)
            .ok_or(DomainError::MoneyOverflow)?;
        if amount < 0 {
            return Err(DomainError::MoneyOverflow);
        }
        Self::new(amount, self.currency.clone())
    }

    /// Format for receipts, e.g. "KSh 1,250.00". Minor units only —
    /// formatting never touches floats.
    pub fn format(&self) -> String {
        let exp = self.currency.exponent as usize;
        let minor = self.amount_minor.unsigned_abs();
        let divisor = 10i64.pow(exp as u32);
        let major = minor / divisor as u64;
        let frac = minor % divisor as u64;
        let sign = if self.amount_minor < 0 { "-" } else { "" };
        let symbol = if self.currency.symbol.is_empty() {
            self.currency.code.as_str()
        } else {
            self.currency.symbol.as_str()
        };
        if exp == 0 {
            format!("{sign}{symbol} {major}")
        } else {
            format!("{sign}{symbol} {major}.{frac:0>width$}", width = exp)
        }
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_same_currency() {
        let a = Money::new(1000, Currency::kes()).unwrap();
        let b = Money::new(250, Currency::kes()).unwrap();
        assert_eq!(a.checked_add(&b).unwrap().amount_minor, 1250);
    }

    #[test]
    fn add_rejects_mismatch() {
        let a = Money::new(1000, Currency::kes()).unwrap();
        let b = Money::new(250, Currency::usd()).unwrap();
        assert!(matches!(
            a.checked_add(&b),
            Err(DomainError::CurrencyMismatch { .. })
        ));
    }

    #[test]
    fn sub_rejects_underflow() {
        let a = Money::new(100, Currency::kes()).unwrap();
        let b = Money::new(250, Currency::kes()).unwrap();
        assert!(a.checked_sub(&b).is_err());
    }

    #[test]
    fn mul_quantity_overflow_safe() {
        let a = Money::new(i64::MAX, Currency::kes()).unwrap();
        assert!(matches!(
            a.mul_quantity(2),
            Err(DomainError::MoneyOverflow)
        ));
    }

    #[test]
    fn format_kes() {
        let m = Money::new(125000, Currency::kes()).unwrap();
        assert_eq!(m.format(), "KSh 1250.00");
    }

    #[test]
    fn currency_validation() {
        assert!(Currency::new("KE", 2, "").is_err());
        assert!(Currency::new("kes", 2, "").is_ok());
    }
}
