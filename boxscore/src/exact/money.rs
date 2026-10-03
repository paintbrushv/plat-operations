use super::{error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

/// Signed USD cents. The symmetric range makes reversal safe for every value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);
    pub fn from_cents(value: i64) -> Result<Self> {
        if value == i64::MIN {
            return Err(error("MONEY_OVERFLOW", "Amount exceeds signed-cent range"));
        }
        Ok(Self(value))
    }
    pub fn cents(self) -> i64 {
        self.0
    }
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .ok_or_else(|| error("MONEY_OVERFLOW", "Cent addition overflow"))
            .and_then(Self::from_cents)
    }
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .ok_or_else(|| error("MONEY_OVERFLOW", "Cent subtraction overflow"))
            .and_then(Self::from_cents)
    }
}

impl FromStr for Money {
    type Err = super::ExactError;
    fn from_str(value: &str) -> Result<Self> {
        let negative = value.starts_with('-');
        let unsigned = if negative { &value[1..] } else { value };
        let mut parts = unsigned.split('.');
        let whole = parts.next().unwrap_or("");
        let fraction = parts.next();
        let valid = !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && whole.len() <= 17
            && parts.next().is_none()
            && fraction.is_none_or(|f| {
                !f.is_empty() && f.len() <= 2 && f.bytes().all(|b| b.is_ascii_digit())
            });
        if !valid {
            return Err(error(
                "INVALID_MONEY",
                "Money must be a decimal string with at most two places",
            ));
        }
        let dollars: i128 = whole
            .parse()
            .map_err(|_| error("INVALID_MONEY", "Invalid decimal money"))?;
        let cents = match fraction {
            None => 0,
            Some(f) => f.parse::<i128>().unwrap() * if f.len() == 1 { 10 } else { 1 },
        };
        let magnitude = dollars * 100 + cents;
        if magnitude > i64::MAX as i128 {
            return Err(error("MONEY_OVERFLOW", "Amount exceeds signed-cent range"));
        }
        Self::from_cents(if negative {
            -(magnitude as i64)
        } else {
            magnitude as i64
        })
    }
}
impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let amount = self.0.unsigned_abs();
        write!(
            f,
            "{}{}.{:02}",
            if self.0 < 0 { "-" } else { "" },
            amount / 100,
            amount % 100
        )
    }
}
impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
