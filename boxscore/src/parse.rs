//! Shared parsing helpers for money cells, count cells, and CSV readers.
//!
//! Yardi-derived exports format numbers many ways: `$1,234.56`, `(500.00)`,
//! `-`, `()`, and blank cells. These helpers normalize all of them so every
//! connector agrees on what a cell means.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

/// Parse a currency/numeric cell. Accounting-style blanks (``, `-`, `()`)
/// mean zero; parentheses mean negative; `$`, `,`, and `%` are stripped.
pub fn parse_money(value: &str) -> Result<f64> {
    let trimmed = value.trim();
    let negated = trimmed.starts_with('(') && trimmed.ends_with(')');
    let inner = trimmed.trim_start_matches('(').trim_end_matches(')');
    let cleaned = inner.replace(['$', ',', '%'], "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "-" {
        return Ok(0.0);
    }
    let parsed = cleaned
        .parse::<f64>()
        .with_context(|| format!("invalid numeric value: {value}"))?;
    Ok(if negated { -parsed.abs() } else { parsed })
}

/// Lenient variant for aggregate columns: unparseable cells count as zero.
pub fn parse_money_lenient(value: &str) -> f64 {
    parse_money(value).unwrap_or_default()
}

/// Parse an integer count cell, tolerating currency formatting.
pub fn parse_count(value: &str) -> Result<i64> {
    Ok(parse_money(value)?.round() as i64)
}

/// Open a CSV file, stripping a UTF-8 BOM if present so the first header
/// matches struct field names and header lookups.
pub fn csv_reader_from_path(path: &Path) -> Result<csv::Reader<Cursor<Vec<u8>>>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to open CSV: {}", path.display()))?;
    let bytes = match bytes.strip_prefix(b"\xef\xbb\xbf") {
        Some(stripped) => stripped.to_vec(),
        None => bytes,
    };
    Ok(csv::Reader::from_reader(Cursor::new(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_currency_formats() {
        assert_eq!(parse_money("$1,234.56").unwrap(), 1234.56);
        assert_eq!(parse_money("(500.00)").unwrap(), -500.0);
        assert_eq!(parse_money("($1,500)").unwrap(), -1500.0);
        assert_eq!(parse_money("-42.5").unwrap(), -42.5);
        assert_eq!(parse_money("0").unwrap(), 0.0);
    }

    #[test]
    fn accounting_blanks_mean_zero() {
        assert_eq!(parse_money("").unwrap(), 0.0);
        assert_eq!(parse_money("  ").unwrap(), 0.0);
        assert_eq!(parse_money("-").unwrap(), 0.0);
        assert_eq!(parse_money("()").unwrap(), 0.0);
        assert_eq!(parse_money("( )").unwrap(), 0.0);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_money("abc").is_err());
        assert!(parse_money("12abc").is_err());
        assert_eq!(parse_money_lenient("abc"), 0.0);
    }

    #[test]
    fn strips_utf8_bom_from_csv_headers() {
        let dir = std::env::temp_dir().join("boxscore_parse_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bom.csv");
        std::fs::write(&path, b"\xef\xbb\xbfaccount_code,amount\n4000,10\n").unwrap();
        let mut reader = csv_reader_from_path(&path).unwrap();
        let headers = reader.headers().unwrap().clone();
        assert_eq!(headers.get(0), Some("account_code"));
    }
}
