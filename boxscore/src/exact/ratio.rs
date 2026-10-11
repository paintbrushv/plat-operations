//! Exact ratios of integers, reported at a fixed scale of four decimal places.
//!
//! A ratio is never a floating-point number. It carries the integer numerator
//! and denominator it was built from, so a consumer can recompute it, and a
//! decimal string rounded half away from zero in integer arithmetic. Every
//! value is a JSON string except `scale`, so no consumer can parse it as a
//! float by accident.
use super::{error, Result};
use serde::{Serialize, Serializer};
use serde_json::{json, Value};

/// Decimal places in [`Ratio::decimal`].
pub const SCALE: u32 = 4;
const FACTOR: u128 = 10_u128.pow(SCALE);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio {
    numerator: i128,
    denominator: i128,
    /// The ratio times 10^SCALE, rounded half away from zero. Computed once
    /// in `new`, which is what makes `decimal` infallible.
    scaled: i128,
}

impl Ratio {
    pub fn new(numerator: i128, denominator: i128) -> Result<Self> {
        if denominator == 0 {
            return Err(error("ZERO_DENOMINATOR", "Ratio denominator is zero"));
        }
        let overflow = || error("RATIO_OVERFLOW", "Ratio exceeds the supported range");
        let negative = (numerator < 0) != (denominator < 0);
        let n = numerator.unsigned_abs();
        let d = denominator.unsigned_abs();
        let scaled_n = n.checked_mul(FACTOR).ok_or_else(overflow)?;
        let mut quotient = scaled_n / d;
        // Round the magnitude up when the remainder is at least half of the
        // divisor, which is half away from zero once the sign is reapplied.
        // `rem >= d - rem` avoids doubling `rem`, so it cannot overflow.
        let remainder = scaled_n % d;
        if remainder >= d - remainder {
            quotient = quotient.checked_add(1).ok_or_else(overflow)?;
        }
        let magnitude = i128::try_from(quotient).map_err(|_| overflow())?;
        Ok(Self {
            numerator,
            denominator,
            scaled: if negative { -magnitude } else { magnitude },
        })
    }

    pub fn numerator(&self) -> i128 {
        self.numerator
    }

    pub fn denominator(&self) -> i128 {
        self.denominator
    }

    /// Fixed four-place decimal, for example `0.8895` or `-0.3333`. A value
    /// that rounds to zero is `0.0000`, never `-0.0000`.
    pub fn decimal(&self) -> String {
        let magnitude = self.scaled.unsigned_abs();
        format!(
            "{}{}.{:04}",
            if self.scaled < 0 { "-" } else { "" },
            magnitude / FACTOR,
            magnitude % FACTOR
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "numerator": self.numerator.to_string(),
            "denominator": self.denominator.to_string(),
            "decimal": self.decimal(),
            "scale": SCALE,
        })
    }
}

impl Serialize for Ratio {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}
