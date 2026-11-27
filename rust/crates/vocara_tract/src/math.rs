//! Deterministic math (port of CHIMERA's `math.rs`, the part the model uses). Every
//! transcendental function goes through the pure-Rust `libm` crate, so every target computes
//! bit-identical geometry and formants, and CHIMERA's fixtures match to the last bit. `sqrt`,
//! `abs`, `floor` and the basic operations are IEEE-exact everywhere and come from `core`. Rust
//! never fuses multiply–adds on its own, so the operation order is CHIMERA's.
//!
//! `js_*` reproduce the JavaScript semantics the model was first written in (NaN-propagating
//! min/max, `Math.round` rounding halves up).

pub const PI: f64 = core::f64::consts::PI;

#[inline(always)]
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}
#[inline(always)]
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}
#[inline(always)]
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}
#[inline(always)]
pub fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}
#[inline(always)]
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}
#[inline(always)]
pub fn acos(x: f64) -> f64 {
    libm::acos(x)
}
#[inline(always)]
pub fn cbrt(x: f64) -> f64 {
    libm::cbrt(x)
}
#[inline(always)]
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}
#[inline(always)]
pub fn sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// `Math.min(a, b)`: NaN if either is NaN; −0 is below +0.
#[inline(always)]
pub fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else if b < a {
        b
    } else if a.is_sign_negative() {
        a
    } else {
        b
    }
}

/// `Math.max(a, b)`: NaN if either is NaN; +0 is above −0.
#[inline(always)]
pub fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else if b > a {
        b
    } else if a.is_sign_positive() {
        a
    } else {
        b
    }
}

/// `Math.round`: nearest integer, halves toward +∞.
#[inline(always)]
pub fn js_round(x: f64) -> f64 {
    let r = x.floor();
    if x - r >= 0.5 {
        r + 1.0
    } else {
        r
    }
}

#[inline(always)]
pub fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

#[inline(always)]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[inline(always)]
pub fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A total order on floats for sorting, NaN last (CHIMERA's `num_order`, JavaScript's
/// `sort((a, b) => a - b)` on the values it sorts).
pub(crate) fn num_order(a: f64, b: f64) -> core::cmp::Ordering {
    match a.partial_cmp(&b) {
        Some(o) => o,
        None => a.is_nan().cmp(&b.is_nan()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_min_max_follow_javascript() {
        assert!(js_min(f64::NAN, 1.0).is_nan() && js_min(1.0, f64::NAN).is_nan());
        assert!(js_max(f64::NAN, 1.0).is_nan() && js_max(1.0, f64::NAN).is_nan());
        assert!(js_min(0.0, -0.0).is_sign_negative() && js_min(-0.0, 0.0).is_sign_negative());
        assert!(js_max(0.0, -0.0).is_sign_positive() && js_max(-0.0, 0.0).is_sign_positive());
        assert_eq!(js_min(2.0, 3.0), 2.0);
        assert_eq!(js_max(2.0, 3.0), 3.0);
    }

    #[test]
    fn js_round_halves_up() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
        assert_eq!(js_round(-0.6), -1.0);
    }
}
