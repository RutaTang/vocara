//! Shared helpers for the golden tests (port of CHIMERA's `tests/common/mod.rs`, the part they
//! use): the fixtures in `tests/golden/` are CHIMERA's, copied from its `crates/chimera/tests/
//! golden/` at the commit `src/lib.rs` names (`analysis.json` trimmed to the cases the port
//! covers: the human airway, the simple networks, the peak finder and Nelder–Mead).
#![allow(dead_code)]

use serde_json::Value;
use vocara_tract::anatomy::{ArticKey, HumanArtic, NEUTRAL};

pub fn load(name: &str) -> Value {
    let path = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

pub fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap(),
        Value::String(s) => match s.as_str() {
            "NaN" => f64::NAN,
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            _ => panic!("not a number: {s}"),
        },
        Value::Null => f64::NAN,
        _ => panic!("not a number: {v}"),
    }
}

pub fn nums(v: &Value) -> Vec<f64> {
    v.as_array()
        .unwrap_or_else(|| panic!("not an array: {v}"))
        .iter()
        .map(num)
        .collect()
}

/// Relative difference with an absolute floor (values near zero compare absolutely).
pub fn rel(a: f64, b: f64, floor: f64) -> f64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        return 0.0;
    }
    (a - b).abs() / a.abs().max(b.abs()).max(floor)
}

/// Worst relative difference between two equal-length sequences (panics on a length mismatch).
pub fn worst(label: &str, a: &[f64], b: &[f64], floor: f64) -> f64 {
    assert_eq!(
        a.len(),
        b.len(),
        "{label}: length {} vs {}",
        a.len(),
        b.len()
    );
    a.iter()
        .zip(b)
        .map(|(x, y)| rel(*x, *y, floor))
        .fold(0.0, f64::max)
}

/// An articulation as CHIMERA's fixtures write it (`{"jaw": 0.6, "tbX": 2.9, …}`), missing keys
/// neutral.
pub fn artic(v: &Value) -> HumanArtic {
    let mut a = NEUTRAL;
    for (key, x) in v.as_object().expect("an articulation object") {
        let k = ArticKey::from_name(key).unwrap_or_else(|| panic!("unknown articulator {key}"));
        a.set(k, num(x));
    }
    a
}
