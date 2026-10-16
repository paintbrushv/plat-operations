//! `inspect_export`: describe the shape of a delimited export, and show a few
//! redacted sample values, before anyone proposes a mapping for it.
//!
//! The function is stateless and deterministic. It reads the file's text
//! (sent inline, like `import_period`'s files) and returns only facts about
//! its shape: the dialect, the header row, the banner and footer rows around
//! the table, and per column a kind, a fill rate, a distinct count and up to
//! five sample values. It never returns the whole file, every list in the
//! result has a fixed cap, and a free-text column (names, addresses, emails,
//! or anything that is nearly unique per row) is redacted by default, so what
//! reaches a model vendor through this function is bounded and mostly
//! non-personal.
//!
//! Rows are the records the CSV reader yields: a blank line is not a row, and
//! a quoted field with a line break inside is one row. Row indexes are
//! 0-based positions in that sequence.
//!
//! Everything echoed from the file is bounded by bytes, not only by characters:
//! a control, invisible or non-space whitespace character is written as
//! `U+XXXX` text before any length cap, and the whole result is held to
//! [`MAX_RESULT_BYTES`] by dropping samples, then banner rows, then shortening
//! names (`truncated: true`).
//!
//! The function is CPU-bound but short (the input is at most 2 MiB), and no
//! other catalog handler moves work to `spawn_blocking`, so it runs inline.
//!
//! The footer rule is shared with the standardized-CSV validator
//! ([`crate::ontology::is_footer_cell`]): `total` as a whole cell, and
//! `grand total`, `subtotal`, `summary` or `all properties` anywhere in a
//! cell, case-insensitively.
use super::{error, protocol::MAX_REQUEST_BYTES, store::is_hidden_character, Result};
use crate::ontology::is_footer_cell;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

/// Columns reported; a wider table is refused with `INPUT_LIMIT`.
pub const MAX_COLUMNS: usize = 256;
/// The serialized result is at most this many bytes.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
const MAX_NAME_CHARS: usize = 120;
const MAX_BANNER_ROWS: usize = 10;
const BANNER_CHARS: usize = 200;
const MAX_FOOTER_ROWS: usize = 20;
const FOOTER_CHARS: usize = 80;
const MAX_SAMPLES: usize = 5;
const SAMPLE_CHARS: usize = 80;
const HEADER_CHARS: usize = 200;
const DISTINCT_CAP: usize = 1000;
const DIALECT_RECORDS: usize = 50;
const DELIMITERS: [u8; 4] = *b",\t;|";
const REDACTED_SAMPLE: &str = "<redacted>";
/// A column whose header contains one of these (any case, anywhere in it) is
/// redacted when redaction is on, whatever its kind.
const PERSON_SUBSTRINGS: [&str; 24] = [
    "resident",
    "tenant",
    "lessee",
    "occupant",
    "applicant",
    "guarantor",
    "email",
    "phone",
    "ssn",
    "contact",
    "address",
    "social",
    "birth",
    "card",
    "bank",
    "routing",
    "license",
    "licence",
    "mobile",
    "customer",
    "payee",
    "household",
    "owner",
    "holder",
];
/// Header words that redact the column only as a whole word (any case, and
/// camel-case humps count as words, so `CellPhone` and `Cell` both match, but
/// `Hotel` does not match `tel` and `Tinted` does not match `tin`). `ss` and `mail` cover `SS No` and `E-Mail`; `tax`
/// followed by `id` (or `taxid`) covers `Tax ID`.
const PERSON_WORDS: [&str; 10] = [
    "ss", "tin", "dob", "tel", "cell", "first", "last", "full", "mail", "taxid",
];
/// A header with `name` in it (as a substring, so `FirstName`, `FullName` and
/// `Surname` match) redacts the column unless every `name` directly follows one
/// of these (`(account|gl|vendor|property|unit|column|report)[ _-]*name`, any
/// case, camel case allowed) and no person word is present: `account_name`,
/// `Vendor Name` and `Property Name` name a thing in the books, not a person,
/// while `Name / Unit` and `Name` do.
const NAME_QUALIFIERS: [&str; 7] = [
    "account", "gl", "vendor", "property", "unit", "column", "report",
];
/// Header words that exempt a text column from the distinct-ratio rule: an
/// identifier or label a mapping needs to read (unit ids, account codes,
/// account descriptions, categories) is unique per row by nature. A qualified
/// name header (`Vendor Name`, `account_name`) has it too. The exemption never
/// applies when a person word (or an unqualified `name`) is present.
const RATIO_EXEMPT_WORDS: [&str; 10] = [
    "code",
    "id",
    "number",
    "num",
    "no",
    "ref",
    "account",
    "unit",
    "description",
    "category",
];
/// Header words that mark an integer column as an identifier (account code,
/// unit number), which is a label rather than a quantity.
const IDENTIFIER_WORDS: [&str; 8] = [
    "code", "id", "number", "num", "no", "ref", "account", "unit",
];
/// Fewer non-empty values than this and the distinct-ratio rule does not apply.
const RATIO_MIN_VALUES: usize = 8;
/// An integer column is ratio-checked when a value has this many digits.
const LONG_INTEGER_DIGITS: usize = 7;
/// A header cell longer than this is replaced when redaction is on: a real
/// column label is short, a data row is not.
const HEADER_REDACT_CHARS: usize = 40;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn default_redact() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    name: String,
    text: String,
    #[serde(default = "default_redact")]
    redact: bool,
}

type Rows = Vec<Vec<String>>;

fn unreadable() -> super::ExactError {
    error(
        "INVALID_INPUT",
        "text could not be read as delimited rows; send the file's text as it is",
    )
}

/// Read `text` as records split on `delimiter`. Ragged rows are kept as they
/// are; cells are trimmed. A row of more than [`MAX_COLUMNS`] cells is refused
/// with `INPUT_LIMIT` as it is read, before the rest of the table is held in
/// memory.
fn records(text: &str, delimiter: u8) -> Result<Rows> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|_| unreadable())?;
        if record.len() > MAX_COLUMNS {
            return Err(error(
                "INPUT_LIMIT",
                format!(
                    "row {index} has {} cells; at most {MAX_COLUMNS} are reported",
                    record.len()
                ),
            ));
        }
        rows.push(record.iter().map(str::to_string).collect());
    }
    Ok(rows)
}

/// The cell counts of the first `limit` records split on `delimiter`. Only the
/// counts are kept, so a candidate delimiter that splits a line into thousands
/// of cells costs no memory and is not refused: it simply scores badly.
fn record_lengths(text: &str, delimiter: u8, limit: usize) -> Result<Vec<usize>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut lengths = Vec::new();
    for record in reader.records().take(limit) {
        lengths.push(record.map_err(|_| unreadable())?.len());
    }
    Ok(lengths)
}

/// The delimiter under which the first records have the most consistent cell
/// count: the number of records with the modal count, counting only a modal
/// count of two or more. Ties go to the larger modal count, then to the
/// order `,` tab `;` `|`.
fn detect_delimiter(text: &str) -> Result<u8> {
    let mut best = (0_usize, 0_usize, b',');
    for delimiter in DELIMITERS {
        let lengths = record_lengths(text, delimiter, DIALECT_RECORDS)?;
        let (count, matching) = modal(lengths.into_iter());
        let score = if count >= 2 { matching } else { 0 };
        if (score, count) > (best.0, best.1) {
            best = (score, count, delimiter);
        }
    }
    Ok(best.2)
}

/// The most common value and how many times it occurs; the larger value wins
/// a tie. `(0, 0)` for no values.
fn modal(values: impl Iterator<Item = usize>) -> (usize, usize) {
    let mut histogram: BTreeMap<usize, usize> = BTreeMap::new();
    for value in values {
        *histogram.entry(value).or_default() += 1;
    }
    histogram_mode(&histogram)
}

fn histogram_mode(histogram: &BTreeMap<usize, usize>) -> (usize, usize) {
    histogram
        .iter()
        .map(|(value, times)| (*times, *value))
        .max()
        .map_or((0, 0), |(times, value)| (value, times))
}

fn line_ending(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    let (mut lf, mut crlf, mut cr) = (0_usize, 0_usize, 0_usize);
    for (i, byte) in bytes.iter().enumerate() {
        match byte {
            b'\n' if i > 0 && bytes[i - 1] == b'\r' => crlf += 1,
            b'\n' => lf += 1,
            b'\r' if bytes.get(i + 1) != Some(&b'\n') => cr += 1,
            _ => {}
        }
    }
    match (lf, crlf, cr) {
        (_, 0, 0) => "lf",
        (0, _, 0) => "crlf",
        (0, 0, _) => "cr",
        _ => "mixed",
    }
}

/// Whether any field starts with a double quote.
fn uses_quotes(text: &str, delimiter: u8) -> bool {
    let after_delimiter = format!("{}\"", delimiter as char);
    text.lines()
        .any(|line| line.trim_start().starts_with('"') || line.contains(&after_delimiter))
}

/// How a cell reads as a number.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Number {
    /// Digits with an optional leading sign, nothing else.
    Whole,
    /// A decimal with optional sign, thousands separators, parentheses or a
    /// trailing minus.
    Decimal,
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Digits, or digits grouped by thousands separators (`1,234,567`).
fn whole_part(s: &str) -> bool {
    if !s.contains(',') {
        return digits(s);
    }
    let mut groups = s.split(',');
    let first_ok = groups.next().is_some_and(|g| digits(g) && g.len() <= 3);
    first_ok && groups.all(|g| digits(g) && g.len() == 3)
}

/// `s` without one leading or trailing `$` and the whitespace around it, or
/// `None` when it has no currency symbol.
fn strip_dollar(s: &str) -> Option<&str> {
    let s = s.trim();
    s.strip_prefix('$')
        .map(str::trim_start)
        .or_else(|| s.strip_suffix('$').map(str::trim_end))
}

fn number(cell: &str) -> Option<Number> {
    let mut rest = cell.trim();
    // A currency symbol (outside or inside parentheses, or after a sign) makes
    // a decimal of a whole number, so it counts as a mark like a parenthesis.
    let mut marked = false;
    if let Some(bare) = strip_dollar(rest) {
        rest = bare;
        marked = true;
    }
    if let Some(inner) = rest
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
    {
        rest = inner.trim();
        marked = true;
        if let Some(bare) = strip_dollar(rest) {
            rest = bare;
        }
    } else if let Some(unsigned) = rest.strip_prefix(['+', '-']) {
        // A leading sign alone keeps a whole number whole.
        rest = unsigned;
        if let Some(bare) = strip_dollar(rest) {
            rest = bare;
            marked = true;
        }
    } else if let Some(unsigned) = rest.strip_suffix('-') {
        rest = unsigned;
        marked = true;
        if let Some(bare) = strip_dollar(rest) {
            rest = bare;
        }
    }
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    if fraction.is_some_and(|f| !digits(f)) {
        return None;
    }
    if whole.is_empty() {
        fraction?;
    } else if !whole_part(whole) {
        return None;
    }
    let plain = fraction.is_none() && !whole.contains(',') && !marked && !whole.is_empty();
    Some(if plain {
        Number::Whole
    } else {
        Number::Decimal
    })
}

fn parse_part(s: &str, width: std::ops::RangeInclusive<usize>) -> Option<u32> {
    (digits(s) && width.contains(&s.len()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `YYYY-MM-DD` or `MM/DD/YYYY`, and a real calendar date.
fn is_date(cell: &str) -> bool {
    let ymd = |y: Option<u32>, m: Option<u32>, d: Option<u32>| match (y, m, d) {
        (Some(y), Some(m), Some(d)) => NaiveDate::from_ymd_opt(y as i32, m, d).is_some(),
        _ => false,
    };
    let dashed: Vec<&str> = cell.split('-').collect();
    if let [y, m, d] = dashed[..] {
        return m.len() == 2
            && d.len() == 2
            && ymd(
                parse_part(y, 4..=4),
                parse_part(m, 2..=2),
                parse_part(d, 2..=2),
            );
    }
    let slashed: Vec<&str> = cell.split('/').collect();
    if let [m, d, y] = slashed[..] {
        return ymd(
            parse_part(y, 4..=4),
            parse_part(m, 1..=2),
            parse_part(d, 1..=2),
        );
    }
    false
}

/// `YYYY-MM` or `Mon YYYY`.
fn is_period(cell: &str) -> bool {
    if let Some((year, month)) = cell.split_once('-') {
        return parse_part(year, 4..=4).is_some()
            && month.len() == 2
            && parse_part(month, 2..=2).is_some_and(|m| (1..=12).contains(&m));
    }
    if let Some((month, year)) = cell.split_once(' ') {
        return parse_part(year, 4..=4).is_some()
            && MONTHS.contains(&month.to_ascii_lowercase().as_str());
    }
    false
}

fn cap(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// `text` as it is echoed: every control, invisible or non-space whitespace
/// character is written as `U+XXXX` text first, then the result is cut to
/// `chars` characters. What reaches the result is printable, and bounded by
/// bytes (at most 4 per character) as well as by characters.
fn visible(text: &str, chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0_usize;
    for c in text.chars() {
        if count >= chars {
            break;
        }
        if is_hidden_character(c) {
            for digit in format!("U+{:04X}", c as u32).chars() {
                if count >= chars {
                    break;
                }
                out.push(digit);
                count += 1;
            }
        } else {
            out.push(c);
            count += 1;
        }
    }
    out
}

/// The words of a header, lowercased: split on anything that is not a letter
/// or digit and at each lower-to-upper camel-case hump (`FirstName` is `first`
/// and `name`).
fn header_words(header: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for c in header.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
        previous_lower = c.is_lowercase();
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Whether the header names a person, their contact details or an account
/// that identifies them: a [`PERSON_SUBSTRINGS`] entry anywhere in it, a
/// [`PERSON_WORDS`] word, `tax` then `id`, or `name` anywhere in it unless each
/// one is qualified (see [`NAME_QUALIFIERS`]).
fn names_a_person(header: &str) -> bool {
    // A padded name (`<unnamed 3>`) says nothing and contains `name`.
    if is_unnamed(header) {
        return false;
    }
    let lowered = header.to_lowercase();
    if PERSON_SUBSTRINGS.iter().any(|word| lowered.contains(word)) {
        return true;
    }
    let words = header_words(header);
    if words.iter().any(|w| PERSON_WORDS.contains(&w.as_str()))
        || words.windows(2).any(|pair| pair == ["tax", "id"])
    {
        return true;
    }
    has_name(header) && !names_are_qualified(header)
}

fn unnamed(index: usize) -> String {
    format!("<unnamed {index}>")
}

fn is_unnamed(label: &str) -> bool {
    label.strip_prefix("<unnamed ").is_some_and(|rest| {
        rest.strip_suffix('>')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn has_name(header: &str) -> bool {
    header.to_ascii_lowercase().contains("name")
}

/// Whether `name` occurs in the header and every occurrence directly follows a
/// [`NAME_QUALIFIERS`] word, with only spaces, underscores or hyphens between
/// (`Vendor Name`, `account_name`, `AccountName`, `MyAccountName`).
fn names_are_qualified(header: &str) -> bool {
    let lowered = header.to_ascii_lowercase();
    let original = header.as_bytes();
    let mut any = false;
    for (position, _) in lowered.match_indices("name") {
        any = true;
        let before = lowered.get(..position).unwrap_or_default();
        let before = before.trim_end_matches([' ', '_', '-']);
        let qualified = NAME_QUALIFIERS.iter().any(|qualifier| {
            let Some(start) = before.len().checked_sub(qualifier.len()) else {
                return false;
            };
            if !before.ends_with(qualifier) {
                return false;
            }
            // The qualifier is a word of its own: at the start, after a
            // non-letter, or at a camel-case hump.
            let hump = original.get(start).is_some_and(u8::is_ascii_uppercase);
            let previous_letter = start
                .checked_sub(1)
                .and_then(|i| original.get(i))
                .is_some_and(u8::is_ascii_alphabetic);
            hump || !previous_letter
        });
        if !qualified {
            return false;
        }
    }
    any
}

/// Whether the distinct-ratio rule is skipped for this header: an identifier
/// or label word, or a qualified `name`. A person word (or an unqualified
/// `name`) never earns the exemption.
fn exempt_from_ratio(header: &str) -> bool {
    if names_a_person(header) {
        return false;
    }
    header.contains('#')
        || names_are_qualified(header)
        || header_words(header)
            .iter()
            .any(|word| RATIO_EXEMPT_WORDS.contains(&word.as_str()))
}

fn is_identifier_header(header: &str) -> bool {
    header.contains('#')
        || header_words(header)
            .iter()
            .any(|word| IDENTIFIER_WORDS.contains(&word.as_str()))
}

/// `\d{3}-\d{2}-\d{4}` anywhere in the value.
fn has_ssn_shape(value: &str) -> bool {
    value.as_bytes().windows(11).any(|w| {
        matches!(w, [a, b, c, b'-', d, e, b'-', f, g, h, i]
            if [a, b, c, d, e, f, g, h, i].iter().all(|x| x.is_ascii_digit()))
    })
}

/// A payment card number: 13 to 19 digits, possibly grouped by spaces or
/// hyphens.
fn is_card_number(value: &str) -> bool {
    let mut digits = 0_usize;
    for c in value.chars() {
        match c {
            ' ' | '-' => {}
            c if c.is_ascii_digit() => digits += 1,
            _ => return false,
        }
    }
    (13..=19).contains(&digits)
}

/// A value that is personal data whatever its column is called: an email
/// address (anything with `@`), a social security number or a card number.
fn is_personal_value(value: &str) -> bool {
    value.contains('@') || has_ssn_shape(value) || is_card_number(value)
}

/// A header cell that cannot be a column label: it holds an `@`, is mostly
/// digits (a year or a `YYYY-MM` period is a label) or is longer than
/// [`HEADER_REDACT_CHARS`] characters.
fn unlikely_label(cell: &str) -> bool {
    if cell.contains('@') || cell.chars().count() > HEADER_REDACT_CHARS {
        return true;
    }
    if is_year(cell) || is_period(cell) {
        return false;
    }
    let (digits, others) =
        cell.chars()
            .filter(|c| !c.is_whitespace())
            .fold((0_usize, 0_usize), |(d, o), c| {
                if c.is_ascii_digit() {
                    (d + 1, o)
                } else {
                    (d, o + 1)
                }
            });
    digits > 0 && digits >= others
}

fn is_year(cell: &str) -> bool {
    digits(cell)
        && cell.len() == 4
        && cell
            .parse::<u32>()
            .is_ok_and(|y| (1900..=2100).contains(&y))
}

/// Following values of a column that a year header is checked against.
const YEAR_LOOKAHEAD: usize = 50;

/// Whether the values below a cell, in column `column`, are numeric: at least
/// one, and every one of the next [`YEAR_LOOKAHEAD`] non-empty values.
fn numbers_below(rest: &[Vec<String>], column: usize) -> bool {
    let mut seen = 0_usize;
    for value in rest
        .iter()
        .filter_map(|row| row.get(column))
        .filter(|cell| !cell.is_empty())
        .take(YEAR_LOOKAHEAD)
    {
        if number(value).is_none() {
            return false;
        }
        seen += 1;
    }
    seen > 0
}

/// A `Key:,value` line or a labelled date (`Run Date,10/16/2026,,`) above a
/// table: a cell ending in a colon, or a date in a row with blank cells.
fn is_banner_like(row: &[String]) -> bool {
    row.iter().any(|cell| cell.ends_with(':'))
        || (row.iter().any(String::is_empty) && row.iter().any(|cell| is_date(cell)))
}

/// Whether `row` can be a header: at least two cells, at least two of them
/// non-empty, every non-empty cell a non-numeric label (a four-digit year is a
/// label when the values below it are numbers), and at least half of all the
/// cells labels, so a header may have blank cells.
fn is_label_row(row: &[String], rest: &[Vec<String>]) -> bool {
    if row.len() < 2 || is_banner_like(row) {
        return false;
    }
    let mut labels = 0_usize;
    for (column, cell) in row.iter().enumerate() {
        if cell.is_empty() {
            continue;
        }
        if number(cell).is_none() || (is_year(cell) && numbers_below(rest, column)) {
            labels += 1;
        } else {
            return false;
        }
    }
    labels >= 2 && labels * 2 >= row.len()
}

/// The first row that is a label row and whose cell count is the modal cell
/// count of the rows after it (a tie goes to the header's own count), or, when
/// no row follows it, the first label row. A header has at least two cells.
fn find_header(rows: &Rows) -> Option<usize> {
    let mut following: BTreeMap<usize, usize> = BTreeMap::new();
    for row in rows {
        *following.entry(row.len()).or_default() += 1;
    }
    for (index, row) in rows.iter().enumerate() {
        if let Some(times) = following.get_mut(&row.len()) {
            *times -= 1;
        }
        following.retain(|_, times| *times > 0);
        let rest = rows.get(index + 1..).unwrap_or_default();
        let usual = match following.values().max() {
            None => true,
            Some(most) => following.get(&row.len()) == Some(most),
        };
        if usual && is_label_row(row, rest) {
            return Some(index);
        }
    }
    None
}

/// Whether a cell below `header_row` is a number or a date: the values under a
/// header differ in kind from it, which a row of data under data does not.
fn typed_below(rows: &[Vec<String>], header_row: usize) -> bool {
    rows.get(header_row + 1..)
        .unwrap_or_default()
        .iter()
        .take(YEAR_LOOKAHEAD)
        .flatten()
        .any(|cell| number(cell).is_some() || is_date(cell))
}

/// A real header shorter than its data rows: the earliest row in `range` that
/// is a label row (blank cells allowed), is not banner-like, has no date cell,
/// has more than half as many cells as the usual count of the rows after it
/// but fewer, and has at least two such rows after it. Returns the row and the
/// usual cell count, the width the missing names are padded to.
fn short_header(rows: &Rows, range: std::ops::Range<usize>) -> Option<(usize, usize)> {
    for index in range {
        let row = rows.get(index)?;
        let rest = rows.get(index + 1..).unwrap_or_default();
        let (usual, times) = modal(rest.iter().map(Vec::len));
        if row.len() >= 2
            && row.len() < usual
            && row.len() * 2 > usual
            && times >= 2
            && !row.iter().any(|cell| is_date(cell))
            && is_label_row(row, rest)
        {
            return Some((index, usual));
        }
    }
    None
}

fn banner_text(row: &[String], delimiter: u8, redact: bool) -> String {
    let used = row.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
    let cells: Vec<String> = row
        .iter()
        .take(used)
        .map(|cell| {
            if redact && is_personal_value(cell) {
                REDACTED_SAMPLE.to_string()
            } else {
                visible(cell, BANNER_CHARS)
            }
        })
        .collect();
    cap(&cells.join(&(delimiter as char).to_string()), BANNER_CHARS)
}

/// `numerator / denominator` as a two-place decimal string, rounded half up.
fn two_places(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        return "0.00".into();
    }
    let hundredths = (numerator * 200 + denominator) / (denominator * 2);
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

struct Profile {
    kind: &'static str,
    non_empty: usize,
    distinct: usize,
    samples: Vec<String>,
    /// Some value is a social security number, an email address or a card
    /// number, whatever the header says.
    personal_values: bool,
    /// Some value has [`LONG_INTEGER_DIGITS`] or more digits.
    long_digits: bool,
}

fn profile(header: &str, values: &[&str]) -> Profile {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut samples = Vec::new();
    let (mut dates, mut periods, mut wholes, mut numbers) = (0_usize, 0_usize, 0_usize, 0_usize);
    let mut leading_zero = false;
    let (mut personal_values, mut long_digits) = (false, false);
    for &value in values {
        if seen.insert(value) && samples.len() < MAX_SAMPLES {
            samples.push(visible(value, SAMPLE_CHARS));
        }
        personal_values |= is_personal_value(value);
        if is_date(value) {
            dates += 1;
        } else if is_period(value) {
            periods += 1;
        }
        match number(value) {
            Some(Number::Whole) => {
                wholes += 1;
                numbers += 1;
                let digits = value.trim_start_matches(['+', '-']);
                leading_zero |= digits.len() > 1 && digits.starts_with('0');
                long_digits |= digits.len() >= LONG_INTEGER_DIGITS;
            }
            Some(Number::Decimal) => numbers += 1,
            None => {}
        }
    }
    let total = values.len();
    let most = |count: usize| total > 0 && count * 10 >= total * 9;
    let kind = if total == 0 {
        "empty"
    } else if most(dates) {
        "date"
    } else if most(periods) {
        "period"
    } else if most(wholes) {
        if leading_zero || is_identifier_header(header) {
            "text"
        } else {
            "integer"
        }
    } else if most(numbers) {
        "money"
    } else {
        "text"
    };
    Profile {
        kind,
        non_empty: total,
        distinct: seen.len(),
        samples,
        personal_values,
        long_digits,
    }
}

/// Whether the column is redacted: a header naming a person (`flagged_header`)
/// redacts it whatever its kind; a text or integer column is redacted on sight of a
/// personal value, whatever its header; and the distinct-ratio rule redacts a
/// text column (or an integer column of long numbers) of at least
/// [`RATIO_MIN_VALUES`] values that are mostly different, or that are at least
/// ten different values each repeated at most three times (a rent roll lists a
/// resident once per charge), unless the header is exempt.
fn is_redacted(label: &str, flagged_header: bool, profile: &Profile) -> bool {
    if profile.non_empty == 0 {
        return false;
    }
    if flagged_header {
        return true;
    }
    let textual = profile.kind == "text" || profile.kind == "integer";
    if textual && profile.personal_values {
        return true;
    }
    let ratio_kind = profile.kind == "text" || (profile.kind == "integer" && profile.long_digits);
    let (n, distinct) = (profile.non_empty, profile.distinct);
    let mostly_unique = distinct * 2 > n || (distinct >= 10 && distinct * 3 >= n);
    ratio_kind && n >= RATIO_MIN_VALUES && mostly_unique && !exempt_from_ratio(label)
}

fn redacted_header(index: usize) -> String {
    format!("<redacted header {index}>")
}

/// Whatever one rendering of the result may hold, tightened in stages until
/// the serialized result fits [`MAX_RESULT_BYTES`]: samples first, then banner
/// rows, then the length of names.
struct Limits {
    samples: usize,
    banners: usize,
    name_chars: usize,
}

const STAGES: [Limits; 6] = [
    Limits {
        samples: MAX_SAMPLES,
        banners: MAX_BANNER_ROWS,
        name_chars: HEADER_CHARS,
    },
    Limits {
        samples: 1,
        banners: MAX_BANNER_ROWS,
        name_chars: HEADER_CHARS,
    },
    Limits {
        samples: 0,
        banners: MAX_BANNER_ROWS,
        name_chars: HEADER_CHARS,
    },
    Limits {
        samples: 0,
        banners: 0,
        name_chars: HEADER_CHARS,
    },
    Limits {
        samples: 0,
        banners: 0,
        name_chars: 40,
    },
    Limits {
        samples: 0,
        banners: 0,
        name_chars: 10,
    },
];

struct ColumnOut {
    name: String,
    index: usize,
    kind: &'static str,
    fill_rate: String,
    distinct: usize,
    samples: Vec<String>,
    redacted: bool,
}

/// Everything the result reports, before the byte bound is applied.
struct Parts {
    name: String,
    delimiter: u8,
    quote: bool,
    line_ending: &'static str,
    bom: bool,
    header_row: usize,
    header: Vec<String>,
    header_redacted: bool,
    banners: Vec<String>,
    footer_rows: Vec<Value>,
    footer_row_count: usize,
    data_row_count: usize,
    columns: Vec<ColumnOut>,
    redact: bool,
}

impl Parts {
    fn render(&self, limits: &Limits, truncated: bool) -> Value {
        let columns: Vec<Value> = self
            .columns
            .iter()
            .map(|c| {
                json!({
                    "name": cap(&c.name, limits.name_chars),
                    "index": c.index,
                    "kind": c.kind,
                    "fill_rate": c.fill_rate,
                    "distinct_count": c.distinct.min(DISTINCT_CAP),
                    "distinct_capped": c.distinct > DISTINCT_CAP,
                    "sample": if c.redacted {
                        vec![REDACTED_SAMPLE.to_string()]
                    } else {
                        c.samples.iter().take(limits.samples).cloned().collect()
                    },
                    "redacted": c.redacted,
                })
            })
            .collect();
        let redacted_columns: Vec<Value> = self
            .columns
            .iter()
            .filter(|c| c.redacted)
            .map(|c| json!({"index": c.index}))
            .collect();
        json!({
            "name": self.name,
            "dialect": {
                "delimiter": (self.delimiter as char).to_string(),
                "quote": if self.quote { "\"" } else { "none" },
                "line_ending": self.line_ending,
            },
            "bom": self.bom,
            "encoding": "utf-8",
            "encoding_note": self.bom.then_some("The text is already decoded; a leading byte-order mark (U+FEFF) was present and was ignored."),
            "header_row": self.header_row,
            "header": self.header.iter().map(|c| cap(c, limits.name_chars)).collect::<Vec<_>>(),
            "header_redacted": self.header_redacted,
            "banner_rows": self.banners.iter().take(limits.banners).collect::<Vec<_>>(),
            "footer_rows": self.footer_rows,
            "footer_row_count": self.footer_row_count,
            "data_row_count": self.data_row_count,
            "columns": columns,
            "redaction": {"applied": self.redact, "columns": redacted_columns},
            "truncated": truncated,
        })
    }

    /// The first rendering that serializes to at most [`MAX_RESULT_BYTES`].
    fn bounded(&self) -> Value {
        let mut last = Value::Null;
        for (stage, limits) in STAGES.iter().enumerate() {
            last = self.render(limits, stage > 0);
            if serde_json::to_string(&last).map_or(0, |text| text.len()) <= MAX_RESULT_BYTES {
                break;
            }
        }
        last
    }
}

/// Run `inspect_export`.
pub fn run(input: Input) -> Result<Value> {
    let Input { name, text, redact } = input;
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(error(
            "INVALID_INPUT",
            format!("name must be a label of 1-{MAX_NAME_CHARS} characters"),
        ));
    }
    if text.len() > MAX_REQUEST_BYTES {
        return Err(error("INPUT_LIMIT", "text exceeds 2 MiB"));
    }
    let bom = text.starts_with('\u{FEFF}');
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    if body.starts_with('\u{FEFF}') {
        return Err(error(
            "INVALID_INPUT",
            "text starts with more than one byte-order mark (U+FEFF); remove the extra mark and send the file again",
        ));
    }
    let delimiter = detect_delimiter(body)?;
    let rows = records(body, delimiter)?;
    if !rows.iter().any(|row| row.len() >= 2) {
        return Err(error("INVALID_INPUT", "no table found"));
    }
    // The header is the first label row with the usual cell count of the rows
    // below it. A shorter label row above it wins when the values below the
    // chosen row are no different in kind from it (all text, so the chosen row
    // is probably the first data row of a table whose header has fewer
    // cells), and also when no row has the usual cell count at all.
    let found = find_header(&rows);
    let short = match found {
        Some(index) if typed_below(&rows, index) => None,
        Some(index) => short_header(&rows, 0..index),
        None => short_header(&rows, 0..rows.len()),
    };
    let (header_row, width) = match (short, found) {
        (Some((index, usual)), _) => (index, usual),
        (None, Some(index)) => (
            index,
            rows.get(index).map_or(0, Vec::len),
        ),
        (None, None) => {
            return Err(error(
                "INVALID_INPUT",
                "no header row found: no row has the usual cell count of the rows below it with at least two non-empty cells, every non-empty cell a non-numeric label and at least half the cells labels",
            ))
        }
    };
    // Names the header lacks are numbered, so every column has one.
    let labels: Vec<String> = rows
        .get(header_row)
        .ok_or_else(unreadable)?
        .iter()
        .cloned()
        .chain((rows.get(header_row).map_or(0, Vec::len)..width).map(unnamed))
        .collect();
    let before_header = rows.get(..header_row).unwrap_or_default();
    let banner_rows: Vec<String> = before_header
        .iter()
        .filter(|row| row.iter().any(|c| !c.is_empty()))
        .take(MAX_BANNER_ROWS)
        .map(|row| banner_text(row, delimiter, redact))
        .collect();
    // A row above the chosen header that has two or more filled cells could
    // still be the real header, so a person word in it flags the column under
    // it (a `Run Date,10/16/2026` banner flags nothing).
    let skipped_rows: Vec<&Vec<String>> = before_header
        .iter()
        .filter(|row| row.iter().filter(|c| !c.is_empty()).count() >= 2)
        .collect();

    let mut footer_rows = Vec::new();
    let mut footer_row_count = 0_usize;
    let mut data: Vec<&Vec<String>> = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(header_row + 1) {
        let Some(first) = row.iter().find(|c| !c.is_empty()) else {
            continue;
        };
        if is_footer_cell(first) {
            footer_row_count += 1;
            if footer_rows.len() < MAX_FOOTER_ROWS {
                let shown = if redact && is_personal_value(first) {
                    REDACTED_SAMPLE.to_string()
                } else {
                    visible(first, FOOTER_CHARS)
                };
                footer_rows.push(json!({"row": index, "first_cell": shown}));
            }
        } else {
            data.push(row);
        }
    }

    let mut columns = Vec::with_capacity(labels.len());
    let mut header_shown = Vec::with_capacity(labels.len());
    let mut header_redacted = false;
    for (index, label) in labels.iter().enumerate() {
        let values: Vec<&str> = data
            .iter()
            .filter_map(|row| row.get(index))
            .map(String::as_str)
            .filter(|cell| !cell.is_empty())
            .collect();
        let profile = profile(label, &values);
        // A skipped row may be the real header, so a person word in its cell
        // for this column flags the column as well.
        let flagged_header = names_a_person(label)
            || skipped_rows
                .iter()
                .any(|row| row.get(index).is_some_and(|cell| names_a_person(cell)));
        let redacted = redact && is_redacted(label, flagged_header, &profile);
        // The header is read from the file like everything else: a cell that
        // looks like data is not echoed.
        let hide_label = redact && unlikely_label(label);
        let shown = if hide_label {
            header_redacted = true;
            redacted_header(index)
        } else {
            visible(label, HEADER_CHARS)
        };
        header_shown.push(shown.clone());
        columns.push(ColumnOut {
            name: shown,
            index,
            kind: profile.kind,
            fill_rate: two_places(profile.non_empty, data.len()),
            distinct: profile.distinct,
            samples: profile.samples,
            redacted,
        });
    }

    let parts = Parts {
        name: visible(&name, MAX_NAME_CHARS),
        delimiter,
        quote: uses_quotes(body, delimiter),
        line_ending: line_ending(body),
        bom,
        header_row,
        header: header_shown,
        header_redacted,
        banners: banner_rows,
        footer_rows,
        footer_row_count,
        data_row_count: data.len(),
        columns,
        redact,
    };
    Ok(parts.bounded())
}

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

pub fn input_schema() -> Value {
    json!({
        "$schema": DRAFT, "type": "object", "additionalProperties": false,
        "required": ["name", "text"],
        "properties": {
            "name": {"type": "string", "minLength": 1, "maxLength": MAX_NAME_CHARS,
                     "description": "A label for the export, 1-120 characters, such as the file name; echoed in the result"},
            "text": {"type": "string", "maxLength": MAX_REQUEST_BYTES,
                     "description": "The export's full text, as a JSON string. At most 2 MiB (the request cap); more is refused with INPUT_LIMIT. Comma, tab, semicolon or pipe separated; a leading byte-order mark is allowed and ignored."},
            "redact": {"type": "boolean", "default": true,
                       "description": "Default true. A column whose header names a person, contact or account (resident, tenant, lessee, occupant, applicant, guarantor, email, mail, phone, ssn, ss, social, tax id, tin, dob, birth, card, bank, routing, license, licence, mobile, cell, tel, contact, address, first, last, full, customer, payee, owner, holder or household (the short ones, ss, tin, dob, tel, cell, mail, first, last and full, only as a whole word, so hotel does not match), or name anywhere in it unless every name directly follows account, gl, vendor, property, unit, column or report, as in Vendor Name or account_name) has its sample replaced by <redacted> whatever its kind. So does a text or integer column in which any value is an email address (contains @), a social security number shape (ddd-dd-dddd) or a 13 to 19 digit card number, whatever its header; and so does a text column (or an integer column with a value of 7 or more digits) of at least 8 values that are mostly different (more than half distinct, or at least 10 distinct values each repeated at most 3 times) unless its header has an identifier or label word (code, id, number, num, no, ref, #, account, unit, description, category; a qualified name header has it too; never with a person word or an unqualified name). Header cells are echoed too: a single cell that contains @, is mostly digits (a year or YYYY-MM is a label) or is longer than 40 characters is replaced by <redacted header N>; header_redacted says so. A banner above the header does not redact it. false returns real sample values and header cells for every column, so use it only when the file holds no personal data."}
        }
    })
}

pub fn output_schema() -> Value {
    let text = |description: &str| json!({"type": "string", "description": description});
    json!({
        "$schema": DRAFT, "type": "object",
        "required": ["name", "dialect", "bom", "encoding", "encoding_note", "header_row",
                     "header", "header_redacted", "banner_rows", "footer_rows",
                     "footer_row_count", "data_row_count", "columns", "redaction",
                     "truncated"],
        "properties": {
            "name": {"type": "string"},
            "dialect": {
                "type": "object",
                "required": ["delimiter", "quote", "line_ending"],
                "properties": {
                    "delimiter": {"enum": [",", "\t", ";", "|"],
                                  "description": "The delimiter under which the first 50 records have the most consistent cell count"},
                    "quote": {"enum": ["\"", "none"]},
                    "line_ending": {"enum": ["lf", "crlf", "cr", "mixed"]}
                }
            },
            "bom": {"type": "boolean", "description": "The text started with a byte-order mark (U+FEFF)"},
            "encoding": {"const": "utf-8", "description": "The input is already text, so this is always utf-8"},
            "encoding_note": {"type": ["string", "null"], "description": "Notes the byte-order mark when there was one; otherwise null"},
            "header_row": {"type": "integer", "minimum": 0,
                           "description": "0-based index of the first row whose cell count is the usual cell count of the rows after it (a tie goes to the row's own count) and that has at least two non-empty cells, every non-empty cell a non-numeric label (a four-digit year counts when the values below it are numbers) and at least half of all its cells labels; a row with no row after it qualifies on the label test alone. A shorter label row above it (more than half as many cells) is preferred when the values below are all text, or when no row has the usual count, and its missing names are <unnamed N>; a Key: line or a labelled date is a banner. Blank lines are not rows."},
            "header": {"type": "array", "maxItems": MAX_COLUMNS,
                       "items": text("A trimmed header cell, at most 200 characters, with a control, invisible or non-space whitespace character written as U+XXXX; <redacted header N> when header_redacted replaced it")},
            "header_redacted": {"type": "boolean",
                                "description": "true when redaction replaced at least one header cell with <redacted header N> (a cell looks like data); always false with redact false"},
            "banner_rows": {"type": "array", "maxItems": MAX_BANNER_ROWS,
                            "items": text("A non-empty row before the header, cells joined by the delimiter, at most 200 characters; at most 10 rows are listed"),
                            "description": "Rows before the header (a property name, a report title, a run date), as they are, with a control, invisible or non-space whitespace character written as U+XXXX; with redaction on, a cell that is an email address, a social security number or a card number shows <redacted>"},
            "footer_rows": {"type": "array", "maxItems": MAX_FOOTER_ROWS,
                            "description": "Rows after the header whose first non-empty cell is a total, grand total, subtotal, summary or all properties token (total as the whole cell; the others anywhere in the cell; any case). At most 20 are listed; footer_row_count counts them all.",
                            "items": {
                                "type": "object", "required": ["row", "first_cell"],
                                "properties": {
                                    "row": {"type": "integer", "minimum": 0},
                                    "first_cell": text("The matching cell, at most 80 characters")
                                }
                            }},
            "footer_row_count": {"type": "integer", "minimum": 0},
            "data_row_count": {"type": "integer", "minimum": 0,
                               "description": "Rows after the header, not counting footer rows or rows with every cell empty"},
            "columns": {
                "type": "array", "maxItems": MAX_COLUMNS,
                "description": "One entry per header cell, at most 256 (a wider table is refused with INPUT_LIMIT)",
                "items": {
                    "type": "object",
                    "required": ["name", "index", "kind", "fill_rate", "distinct_count",
                                 "distinct_capped", "sample", "redacted"],
                    "properties": {
                        "name": text("The header cell as header shows it: at most 200 characters, U+XXXX for a control, invisible or non-space whitespace character, <redacted header N> when header_redacted replaced it"),
                        "index": {"type": "integer", "minimum": 0},
                        "kind": {"enum": ["money", "integer", "date", "period", "empty", "text"],
                                 "description": "At least 90% of the non-empty values decide it. date: YYYY-MM-DD or MM/DD/YYYY. period: YYYY-MM or Mon YYYY. integer: whole numbers with no separators or decimals (not an identifier such as an account code or one with leading zeros, which is text). money: a decimal with optional sign, thousands separators, parentheses or a trailing minus. empty: no values. Otherwise text."},
                        "fill_rate": {"type": "string", "pattern": "^[0-9]+\\.[0-9]{2}$",
                                      "description": "Non-empty values over data rows, as a two-place decimal string, rounded half up"},
                        "distinct_count": {"type": "integer", "minimum": 0, "maximum": DISTINCT_CAP,
                                           "description": "Distinct non-empty values, capped at 1000"},
                        "distinct_capped": {"type": "boolean", "description": "true when there are more than 1000 distinct values"},
                        "sample": {"type": "array", "maxItems": MAX_SAMPLES, "items": {"type": "string"},
                                   "description": "Up to 5 distinct non-empty values in first-seen order, each at most 80 characters, with a control, invisible or non-space whitespace character written as U+XXXX; [\"<redacted>\"] when the column is redacted; shortened when the result would exceed 256 KiB (see truncated)"},
                        "redacted": {"type": "boolean"}
                    }
                }
            },
            "redaction": {
                "type": "object", "required": ["applied", "columns"],
                "description": "Redaction applies to a column whose header names a person, contact or account (see the redact input) whatever its kind, to a text or integer column holding an email address, a social security number or a card number, and to text columns (or integer columns of 7 or more digit values) of at least 8 values that are mostly different unless the header has an identifier or label word",
                "properties": {
                    "applied": {"type": "boolean", "description": "The request's redact flag: true when the redaction rule was in force"},
                    "columns": {"type": "array", "maxItems": MAX_COLUMNS,
                                "description": "The redacted columns, by index (a name is not repeated here: the header may itself be a data row)",
                                "items": {"type": "object", "required": ["index"],
                                          "properties": {"index": {"type": "integer", "minimum": 0}}}}
                }
            },
            "truncated": {"type": "boolean",
                          "description": "true when the serialized result would have exceeded 256 KiB and was shortened, first by dropping samples, then banner rows, then shortening names"}
        }
    })
}
