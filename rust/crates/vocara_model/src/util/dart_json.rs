//! JSON text exactly as Dart's `jsonEncode` writes it.
//!
//! The Flutter app stores JSON in library columns (FX chains, sends, reverb,
//! lyrics, aliases, tags), in settings and in sync payloads, and hashes
//! canonical JSON for sync change detection (`SyncWriter.contentHashOf`). A
//! Rust write of the same value must produce the same bytes, or an untouched
//! row reads as changed on the next save and every Flutter device sees a
//! phantom edit. `serde_json` agrees with Dart on strings and integers but not
//! on doubles: Dart writes `1.0` for a whole double, plain digits up to 21
//! of them, and `1e+21` / `1.5e-7` past that; this module writes doubles the
//! Dart way. Map keys keep insertion order (serde_json's `preserve_order`), as
//! Dart's map literals do.

use serde_json::{Map, Number, Value};

/// Why a value has no Dart JSON text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// NaN or ±Infinity: Dart's `jsonEncode` throws on them too.
    #[error("non-finite double is not representable in JSON")]
    NonFinite,
}

/// What [`double`] puts in a JSON tree for NaN or ±infinity. A
/// `serde_json::Value` cannot hold a non-finite number (`Value::from` turns
/// one into `null`), but Dart's `toJson` kept the double and its `jsonEncode`
/// then threw, so a model holding one could not be saved. [`encode`] refuses
/// this marker the same way; nothing else turns a model's JSON into text.
const NON_FINITE_MARKER: &str = "\u{0}vocara:non-finite-double\u{0}";

/// A double as a Dart `toJson` writes it: a JSON number, or, for NaN and
/// ±infinity, a value [`encode`] refuses with [`EncodeError::NonFinite`]
/// (where Dart's `jsonEncode` threw).
pub fn double(v: f64) -> Value {
    if v.is_finite() {
        Value::from(v)
    } else {
        Value::String(NON_FINITE_MARKER.to_string())
    }
}

/// Encodes [value] as Dart's `jsonEncode` would.
pub fn encode(value: &Value) -> Result<String, EncodeError> {
    let mut out = String::new();
    write_value(&mut out, value)?;
    Ok(out)
}

fn write_value(out: &mut String, value: &Value) -> Result<(), EncodeError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n)?,
        Value::String(s) if s == NON_FINITE_MARKER => return Err(EncodeError::NonFinite),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item)?;
            }
            out.push(']');
        }
        Value::Object(map) => write_map(out, map)?,
    }
    Ok(())
}

fn write_map(out: &mut String, map: &Map<String, Value>) -> Result<(), EncodeError> {
    out.push('{');
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(out, k);
        out.push(':');
        write_value(out, v)?;
    }
    out.push('}');
    Ok(())
}

fn write_number(out: &mut String, n: &Number) -> Result<(), EncodeError> {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else if let Some(f) = n.as_f64() {
        out.push_str(&dart_double(f).ok_or(EncodeError::NonFinite)?);
    }
    Ok(())
}

/// Dart's string escaping: `"` and `\`, the short forms `\b \f \n \r \t`, and
/// `\u00XX` (lowercase hex) for the other control characters below U+0020.
/// Everything else, DEL and non-ASCII included, is written as is.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A double as Dart's `double.toString` writes it, or `None` for NaN and
/// ±Infinity.
///
/// The digits are the shortest that round-trip (Rust's `{:e}` and Dart's
/// converter both produce them); the layout follows ECMAScript's
/// Number::toString with Dart's one difference, a `.0` after a whole number:
/// with `k` digits and the decimal point `n` places after the first,
/// - `k <= n <= 21`: the digits, `n - k` zeros, `.0`;
/// - `0 < n <= 21`: the digits with the point inside them;
/// - `-6 < n <= 0`: `0.`, `-n` zeros, the digits;
/// - otherwise `d[.ddd]e±x`.
pub fn dart_double(v: f64) -> Option<String> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some(if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string());
    }
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e')?;
    let exp: i32 = exp.parse().ok()?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
        out.push_str(".0");
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.abs().to_string());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whole_doubles_keep_their_point_and_ints_do_not() {
        assert_eq!(
            encode(&json!([1.0, 1, -0.0, 100.0])).unwrap(),
            "[1.0,1,-0.0,100.0]"
        );
    }

    #[test]
    fn non_finite_doubles_are_refused() {
        assert_eq!(dart_double(f64::NAN), None);
        assert_eq!(dart_double(f64::INFINITY), None);
        let v = Value::Number(Number::from_f64(1.0).unwrap());
        assert!(encode(&v).is_ok());
    }

    #[test]
    fn a_non_finite_double_in_a_model_tree_fails_the_encode() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let v = json!({"a": [1, {"b": double(bad)}]});
            assert!(matches!(encode(&v), Err(EncodeError::NonFinite)));
        }
        assert_eq!(encode(&json!([double(-0.5)])).unwrap(), "[-0.5]");
    }

    #[test]
    fn map_keys_keep_insertion_order() {
        let v = json!({"b": 1, "a": [1.0, {"z": null, "y": "x"}], "c": -0.5});
        assert_eq!(
            encode(&v).unwrap(),
            r#"{"b":1,"a":[1.0,{"z":null,"y":"x"}],"c":-0.5}"#
        );
    }
}
