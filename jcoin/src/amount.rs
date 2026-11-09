//! Coin amounts: the base unit, and conversion to and from decimal JEF strings.
//!
//! All amounts on the wire, in transactions, and in state are `u64` base units.
//! One JEF is `COIN` base units (8 decimal places). Decimal strings are only a
//! user-interface format: the wallet parses them, and anything that shows an
//! amount to a person formats it.

use std::fmt;

/// Base units per JEF.
pub const COIN: u64 = 100_000_000;

/// Number of decimal places in a JEF amount.
pub const DECIMALS: usize = 8;

/// Reasons a decimal JEF string is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountError {
    Empty,
    /// A character other than a digit or a single `.`.
    Invalid,
    /// More than `DECIMALS` digits after the `.`.
    TooPrecise,
    /// The value does not fit in a `u64` of base units.
    Overflow,
}

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AmountError::Empty => write!(f, "amount is empty"),
            AmountError::Invalid => write!(f, "amount must be a decimal number such as 12.5"),
            AmountError::TooPrecise => write!(f, "amount has more than {DECIMALS} decimal places"),
            AmountError::Overflow => write!(f, "amount is too large"),
        }
    }
}

impl std::error::Error for AmountError {}

/// Parse a decimal JEF string ("12", "12.5", "0.00000001", ".5") into base
/// units. Rejects signs, exponents, separators, and sub-base-unit precision.
pub fn parse_jef(input: &str) -> Result<u64, AmountError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(AmountError::Empty);
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(AmountError::Invalid);
    }
    let all_digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(whole) || !all_digits(frac) {
        return Err(AmountError::Invalid);
    }
    if frac.len() > DECIMALS {
        return Err(AmountError::TooPrecise);
    }

    let whole_units = if whole.is_empty() {
        0
    } else {
        whole
            .parse::<u64>()
            .map_err(|_| AmountError::Overflow)?
            .checked_mul(COIN)
            .ok_or(AmountError::Overflow)?
    };
    // Right-pad the fraction to DECIMALS digits: "5" -> "50000000".
    let frac_units = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<DECIMALS$}")
            .parse::<u64>()
            .map_err(|_| AmountError::Invalid)?
    };
    whole_units
        .checked_add(frac_units)
        .ok_or(AmountError::Overflow)
}

/// Format base units as a decimal JEF string with trailing zeros removed
/// ("50", "12.5", "0.00000001").
pub fn format_jef(units: u64) -> String {
    let whole = units / COIN;
    let frac = units % COIN;
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:0>DECIMALS$}");
    format!("{whole}.{}", frac.trim_end_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whole_and_fractional_amounts() {
        assert_eq!(parse_jef("0"), Ok(0));
        assert_eq!(parse_jef("50"), Ok(50 * COIN));
        assert_eq!(parse_jef("12.5"), Ok(1_250_000_000));
        assert_eq!(parse_jef(".5"), Ok(COIN / 2));
        assert_eq!(parse_jef("3."), Ok(3 * COIN));
        assert_eq!(parse_jef("0.00000001"), Ok(1));
        assert_eq!(parse_jef(" 1.25 "), Ok(125_000_000));
    }

    #[test]
    fn rejects_malformed_amounts() {
        assert_eq!(parse_jef(""), Err(AmountError::Empty));
        assert_eq!(parse_jef("."), Err(AmountError::Invalid));
        assert_eq!(parse_jef("-1"), Err(AmountError::Invalid));
        assert_eq!(parse_jef("+1"), Err(AmountError::Invalid));
        assert_eq!(parse_jef("1e3"), Err(AmountError::Invalid));
        assert_eq!(parse_jef("1,000"), Err(AmountError::Invalid));
        assert_eq!(parse_jef("1.2.3"), Err(AmountError::Invalid));
        assert_eq!(parse_jef("0.000000001"), Err(AmountError::TooPrecise));
    }

    #[test]
    fn rejects_overflow() {
        // u64::MAX base units is about 184467440737.09551615 JEF.
        assert_eq!(parse_jef("184467440737.09551615"), Ok(u64::MAX));
        assert_eq!(
            parse_jef("184467440737.09551616"),
            Err(AmountError::Overflow)
        );
        assert_eq!(parse_jef("184467440738"), Err(AmountError::Overflow));
        assert_eq!(
            parse_jef("99999999999999999999999"),
            Err(AmountError::Overflow)
        );
    }

    #[test]
    fn formats_without_trailing_zeros() {
        assert_eq!(format_jef(0), "0");
        assert_eq!(format_jef(50 * COIN), "50");
        assert_eq!(format_jef(1_250_000_000), "12.5");
        assert_eq!(format_jef(1), "0.00000001");
        assert_eq!(format_jef(u64::MAX), "184467440737.09551615");
    }

    #[test]
    fn format_then_parse_round_trips() {
        for units in [0, 1, 7, COIN - 1, COIN, COIN + 1, 123_456_789_012, u64::MAX] {
            assert_eq!(parse_jef(&format_jef(units)), Ok(units));
        }
    }
}
