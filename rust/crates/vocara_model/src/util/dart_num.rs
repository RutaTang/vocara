//! Dart's number semantics where they differ from Rust's.
//!
//! The ported code clamps, rounds and compares exactly as the Dart code did,
//! so stored values and derived numbers agree bit for bit between the two
//! apps:
//! - `num.clamp` compares with `compareTo`, which orders `-0.0` below `0.0`
//!   and NaN above everything: `(-0.0).clamp(0.0, 1.0)` is `0.0` (Rust's
//!   `f64::clamp` keeps `-0.0`), and `nan.clamp(a, b)` is `b`;
//! - `double.round()` rounds half away from zero (as `f64::round`) and
//!   returns an `int`; the Dart VM saturates out-of-range values, as Rust's
//!   `as i64` does (NaN and ±infinity throw in Dart: they map to the
//!   saturated value here, see [`round_to_i64`]);
//! - `~/` truncates toward zero (Rust's `/` on integers) and `%` on ints is
//!   never negative for a positive divisor (`rem_euclid`).

use std::cmp::Ordering;

/// Dart's `double.compareTo`: `-0.0 < 0.0`, and NaN is equal to itself and
/// greater than every other value (whatever its sign bit).
pub fn compare_f64(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            if a < b {
                Ordering::Less
            } else if a > b {
                Ordering::Greater
            } else if a == 0.0 {
                // Both zero: a negative zero sorts first.
                match (a.is_sign_negative(), b.is_sign_negative()) {
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    _ => Ordering::Equal,
                }
            } else {
                Ordering::Equal
            }
        }
    }
}

/// Dart's `num.clamp(lower, upper)` on doubles, compared with
/// [`compare_f64`]. Dart throws when `lower > upper`; every call site passes
/// constant bounds in order.
pub fn clamp_f64(v: f64, lower: f64, upper: f64) -> f64 {
    debug_assert!(compare_f64(lower, upper) != Ordering::Greater);
    if lower.is_nan() {
        return lower;
    }
    if compare_f64(v, lower) == Ordering::Less {
        return lower;
    }
    if compare_f64(v, upper) == Ordering::Greater {
        return upper;
    }
    v
}

/// Dart's `int.clamp(lower, upper)`: below `lower` gives `lower`, above
/// `upper` gives `upper`, tested in that order. Dart throws an
/// `ArgumentError` when `lower > upper`; the callers that can meet such a
/// range document what that means for them (they all bail out right after,
/// so the clamped value is never used).
pub fn clamp_i64(v: i64, lower: i64, upper: i64) -> i64 {
    if v < lower {
        lower
    } else if v > upper {
        upper
    } else {
        v
    }
}

/// Dart's `double.round()`: half away from zero, saturated to the 64-bit
/// range like the Dart VM's double-to-int conversion. Dart throws an
/// `UnsupportedError` on NaN and ±infinity (a programming error there); this
/// maps NaN to 0 and the infinities to the saturated bounds.
pub fn round_to_i64(v: f64) -> i64 {
    v.round() as i64
}

/// Dart's `double.floor()` returning an `int` (see [`round_to_i64`] for the
/// non-finite cases).
pub fn floor_to_i64(v: f64) -> i64 {
    v.floor() as i64
}

/// Dart's `double.ceil()` returning an `int` (see [`round_to_i64`] for the
/// non-finite cases).
pub fn ceil_to_i64(v: f64) -> i64 {
    v.ceil() as i64
}

/// Dart's `math.pow` on doubles: the VM's special cases (`x^0` is 1, `1^y`
/// is 1, NaN in gives NaN, `x^0.5` is `sqrt(x)` except for -∞), then the C
/// library's `pow`.
pub fn pow_f64(base: f64, exponent: f64) -> f64 {
    if exponent == 0.0 {
        return 1.0;
    }
    if base == 1.0 {
        return 1.0;
    }
    if exponent.is_nan() || base.is_nan() {
        return f64::NAN;
    }
    if base != f64::NEG_INFINITY && exponent == 0.5 {
        if base == 0.0 {
            return 0.0;
        }
        return base.sqrt();
    }
    base.powf(exponent)
}

/// Dart's `math.min` on doubles: NaN if either is NaN, and `-0.0` below
/// `0.0`.
pub fn min_f64(a: f64, b: f64) -> f64 {
    if a > b {
        return b;
    }
    if a < b {
        return a;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    // Equal: both zero (of either sign) or the same value.
    if a == 0.0 && (a.is_sign_negative() || b.is_sign_negative()) {
        return -0.0;
    }
    a
}

/// Dart's floored integer division helper used by `DateTime`
/// (`(a - (a < 0 ? b - 1 : 0)) ~/ b` for a positive `b`).
pub fn floored_division(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_orders_negative_zero_below_zero_and_nan_above_all() {
        assert!(clamp_f64(-0.0, 0.0, 1.0).is_sign_positive());
        assert!(clamp_f64(0.0, -1.0, -0.0).is_sign_negative());
        assert_eq!(clamp_f64(f64::NAN, 0.0, 1.0), 1.0);
        assert_eq!(clamp_f64(2.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp_f64(-2.0, 0.0, 1.0), 0.0);
        assert_eq!(clamp_f64(0.5, 0.0, 1.0), 0.5);
    }

    #[test]
    fn round_is_half_away_from_zero() {
        assert_eq!(round_to_i64(0.5), 1);
        assert_eq!(round_to_i64(-0.5), -1);
        assert_eq!(round_to_i64(2.5), 3);
        assert_eq!(round_to_i64(-2.5), -3);
        assert_eq!(round_to_i64(1e300), i64::MAX);
    }

    #[test]
    fn compare_matches_dart() {
        assert_eq!(compare_f64(-0.0, 0.0), Ordering::Less);
        assert_eq!(compare_f64(f64::NAN, f64::INFINITY), Ordering::Greater);
        assert_eq!(compare_f64(-f64::NAN, f64::NAN), Ordering::Equal);
        assert_eq!(floored_division(-1, 1000), -1);
        assert_eq!(floored_division(1999, 1000), 1);
    }
}
