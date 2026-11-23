//! Reading decoded JSON the way the Dart `fromJson` factories do.
//!
//! `json['key']` is null for a missing key and for an explicit `null` alike;
//! `as T` throws a `TypeError` on any other type (an [`Error::Type`] here),
//! `as T?` lets null through. `as int` accepts only a JSON integer (Dart's
//! decoder makes `1.0` a double), `as num` either kind.

use serde_json::{Map, Value};

use crate::{Error, Result};

pub(crate) type JsonMap = Map<String, Value>;

/// `json[key]`: `None` for a missing key and for `null`.
pub(crate) fn field<'a>(j: &'a JsonMap, key: &str) -> Option<&'a Value> {
    match j.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() => "int",
        Value::Number(_) => "double",
        Value::String(_) => "String",
        Value::Array(_) => "List",
        Value::Object(_) => "Map",
    }
}

fn mismatch(key: &str, expected: &str, got: Option<&Value>) -> Error {
    let got = got.map_or("Null", kind_of);
    Error::Type(format!("'{key}': {got} is not a subtype of {expected}"))
}

/// `value as Map<String, dynamic>`.
pub(crate) fn as_map<'a>(v: &'a Value, what: &str) -> Result<&'a JsonMap> {
    match v {
        Value::Object(m) => Ok(m),
        other => Err(mismatch(what, "Map<String, dynamic>", Some(other))),
    }
}

/// `value as int` for a JSON number value.
pub(crate) fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// `value as num` (then `toDouble()`).
pub(crate) fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `json[key] as String`.
pub(crate) fn req_str(j: &JsonMap, key: &str) -> Result<String> {
    match field(j, key) {
        Some(Value::String(s)) => Ok(s.clone()),
        other => Err(mismatch(key, "String", other)),
    }
}

/// `json[key] as String?`.
pub(crate) fn opt_str(j: &JsonMap, key: &str) -> Result<Option<String>> {
    match field(j, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        other => Err(mismatch(key, "String?", other)),
    }
}

/// `json[key] as int`.
pub(crate) fn req_int(j: &JsonMap, key: &str) -> Result<i64> {
    let v = field(j, key);
    v.and_then(int_of).ok_or_else(|| mismatch(key, "int", v))
}

/// `json[key] as int?`.
pub(crate) fn opt_int(j: &JsonMap, key: &str) -> Result<Option<i64>> {
    match field(j, key) {
        None => Ok(None),
        Some(v) => int_of(v)
            .map(Some)
            .ok_or_else(|| mismatch(key, "int?", Some(v))),
    }
}

/// `(json[key] as num?)?.toDouble()`.
pub(crate) fn opt_num(j: &JsonMap, key: &str) -> Result<Option<f64>> {
    match field(j, key) {
        None => Ok(None),
        Some(v) => num_of(v)
            .map(Some)
            .ok_or_else(|| mismatch(key, "num?", Some(v))),
    }
}

/// `json[key] as bool?`.
pub(crate) fn opt_bool(j: &JsonMap, key: &str) -> Result<Option<bool>> {
    match field(j, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        other => Err(mismatch(key, "bool?", other)),
    }
}

/// `json[key] as List<dynamic>?`.
pub(crate) fn opt_list<'a>(j: &'a JsonMap, key: &str) -> Result<Option<&'a Vec<Value>>> {
    match field(j, key) {
        None => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        other => Err(mismatch(key, "List<dynamic>?", other)),
    }
}

/// `(json[key] as List<dynamic>? ?? const []).map((e) => parse(e as Map))`.
pub(crate) fn list_of<T>(
    j: &JsonMap,
    key: &str,
    parse: impl Fn(&Value) -> Result<T>,
) -> Result<Vec<T>> {
    match opt_list(j, key)? {
        None => Ok(Vec::new()),
        Some(items) => items.iter().map(parse).collect(),
    }
}

/// `(json[key] as List? ?? []).map((e) => (e as num).toDouble())`.
pub(crate) fn doubles(j: &JsonMap, key: &str) -> Result<Vec<f64>> {
    list_of(j, key, |e| {
        num_of(e).ok_or_else(|| mismatch(key, "num", Some(e)))
    })
}

/// A double as `toJson` writes it (`Value::Null` for NaN/±infinity, which
/// Dart's `jsonEncode` would refuse; clamped model fields never hold one).
pub(crate) fn double(v: f64) -> Value {
    Value::from(v)
}

/// An optional value as `toJson` writes it (`null` when absent).
pub(crate) fn nullable<T: Into<Value>>(v: Option<T>) -> Value {
    v.map_or(Value::Null, Into::into)
}
