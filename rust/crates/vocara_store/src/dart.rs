//! The few Dart core-library semantics the store's data formats depend on:
//! string order and length in UTF-16 code units, `int.parse`, `padLeft`.
//!
//! Dart strings are UTF-16. Where a ported rule compares, measures or sorts
//! strings (the HLC node tiebreak, the content-hash key order, drift's id
//! length check), Rust must do it the same way or two devices could disagree
//! about the same data. For ASCII text, which is everything the app itself
//! writes, these agree with Rust's own byte semantics.

use std::cmp::Ordering;

/// Dart's `String.compareTo`: lexicographic over UTF-16 code units.
pub(crate) fn compare_strings(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.as_bytes().cmp(b.as_bytes());
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Dart's `String.length`: the number of UTF-16 code units.
pub(crate) fn string_length(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// Dart's `String.padLeft(width, '0')` for the ASCII text of a number.
pub(crate) fn pad_left_zeros(s: &str, width: usize) -> String {
    let len = string_length(s);
    if len >= width {
        return s.to_string();
    }
    let mut out = "0".repeat(width - len);
    out.push_str(s);
    out
}

/// Whether Dart's `int.parse` (and `String.trim`) treats the UTF-16 code
/// unit [c] as whitespace.
fn is_dart_whitespace(c: u32) -> bool {
    if c <= 32 {
        return c == 32 || (9..=13).contains(&c);
    }
    if c < 0x85 {
        return false;
    }
    if c == 0x85 || c == 0xA0 {
        return true;
    }
    if c <= 0x200A {
        return c == 0x1680 || c >= 0x2000;
    }
    matches!(c, 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

/// Dart's `int.parse(source)` (no radix) on the VM: surrounding whitespace
/// is ignored, one leading `+` or `-` is allowed, a `0x`/`0X` prefix reads
/// hexadecimal (up to 64 unsigned bits, wrapping like Dart's int), and a
/// decimal value outside the 64-bit range is refused. `None` where Dart
/// throws a `FormatException`.
pub(crate) fn parse_int(source: &str) -> Option<i64> {
    let units: Vec<char> = source.chars().collect();
    let mut start = 0;
    let mut end = units.len();
    while start < end && is_dart_whitespace(units[start] as u32) {
        start += 1;
    }
    while end > start && is_dart_whitespace(units[end - 1] as u32) {
        end -= 1;
    }
    if start == end {
        return None;
    }
    let mut negative = false;
    if units[start] == '+' || units[start] == '-' {
        negative = units[start] == '-';
        start += 1;
        if start == end {
            return None;
        }
    }
    if units[start] == '0' {
        if start + 1 == end {
            return Some(0);
        }
        if units[start + 1] == 'x' || units[start + 1] == 'X' {
            let digits = &units[start + 2..end];
            if digits.is_empty() {
                return None;
            }
            let mut value: u64 = 0;
            for c in digits {
                let d = c.to_digit(16)?;
                value = value.checked_mul(16)?.checked_add(u64::from(d))?;
            }
            let signed = value as i64;
            return Some(if negative {
                signed.wrapping_neg()
            } else {
                signed
            });
        }
    }
    let mut value: i128 = 0;
    for c in &units[start..end] {
        if !c.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i128::from(c.to_digit(10)?);
        if value > i128::from(i64::MAX) + 1 {
            return None;
        }
    }
    let value = if negative { -value } else { value };
    i64::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_order_is_utf16_code_unit_order() {
        assert_eq!(compare_strings("a", "b"), Ordering::Less);
        assert_eq!(compare_strings("AB", "A"), Ordering::Greater);
        // U+FF61 is one code unit above every surrogate; U+1F600 is a
        // surrogate pair starting at 0xD83D. UTF-8 bytes order them the
        // other way round.
        assert_eq!(compare_strings("\u{FF61}", "\u{1F600}"), Ordering::Greater);
        assert!("\u{FF61}".as_bytes() < "\u{1F600}".as_bytes());
    }

    #[test]
    fn length_counts_utf16_code_units() {
        assert_eq!(string_length("abc"), 3);
        assert_eq!(string_length("é"), 1);
        assert_eq!(string_length("\u{1F600}"), 2);
    }

    #[test]
    fn pad_left_pads_like_dart_even_for_a_sign() {
        assert_eq!(pad_left_zeros("42", 6), "000042");
        assert_eq!(pad_left_zeros("-5", 4), "00-5");
        assert_eq!(pad_left_zeros("1234567", 6), "1234567");
    }

    #[test]
    fn parse_int_follows_dart() {
        assert_eq!(parse_int("0000000001000"), Some(1000));
        assert_eq!(parse_int(" 12\n"), Some(12));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("-0"), Some(0));
        assert_eq!(parse_int("0x1F"), Some(31));
        assert_eq!(parse_int("0xFFFFFFFFFFFFFFFF"), Some(-1));
        assert_eq!(parse_int("9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_int("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int("9223372036854775808"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("  "), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int("0x"), None);
        assert_eq!(parse_int("1_000"), None);
        assert_eq!(parse_int("12a"), None);
        assert_eq!(parse_int("１２"), None, "fullwidth digits are not digits");
    }
}
