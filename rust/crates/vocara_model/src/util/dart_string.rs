//! Dart's `String` semantics where they differ from Rust's.
//!
//! Dart strings are UTF-16: `length` counts code units, and `trim` strips the
//! Unicode `White_Space` characters plus the byte order mark (U+FEFF), which
//! Rust's `str::trim` keeps. A lyrics file that starts with a BOM, trimmed
//! the Rust way, would keep it and read differently.

/// Whether Dart's `trim` family strips [c]: Unicode `White_Space` and U+FEFF.
pub fn is_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// Dart's `String.trim()`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_whitespace)
}

/// Dart's `String.trimRight()`.
pub fn trim_right(s: &str) -> &str {
    s.trim_end_matches(is_whitespace)
}

/// Dart's `String.trimLeft()`.
pub fn trim_left(s: &str) -> &str {
    s.trim_start_matches(is_whitespace)
}

/// Dart's `String.length`: UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Dart's `s.substring(0, units)` for a string longer than [units] UTF-16
/// code units. A cut inside a surrogate pair leaves Dart a lone high
/// surrogate, which its UTF-8 encoder writes as U+FFFD; the same character
/// stands in for it here.
pub fn truncate_utf16(s: &str, units: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > units {
            if used < units {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_strips_the_bom_too() {
        assert_eq!(trim("\u{FEFF} la \t"), "la");
        assert_eq!(trim_right("la \u{3000}"), "la");
        assert_eq!(trim_left("\u{85}la"), "la");
    }

    #[test]
    fn lengths_and_cuts_count_utf16_units() {
        assert_eq!(utf16_len("a😀"), 3);
        assert_eq!(truncate_utf16("a😀b", 2), "a\u{FFFD}");
        assert_eq!(truncate_utf16("a😀b", 3), "a😀");
        assert_eq!(truncate_utf16("abc", 2), "ab");
    }
}
