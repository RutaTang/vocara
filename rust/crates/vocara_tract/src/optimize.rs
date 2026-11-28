//! Nelder–Mead simplex minimiser with box bounds (port of CHIMERA's `util/optimize.rs`).
//!
//! The algorithm is CHIMERA's step for step (the same stable ordering, centroid sums, reflect /
//! expand / contract / shrink decisions and clamping), so it takes the same iterations and
//! function evaluations on CHIMERA's fixtures. Where CHIMERA's version allocated a vector per
//! vertex and per step, this one keeps the simplex in fixed arrays of up to [`MAX_DIM`]
//! dimensions: a minimisation allocates nothing, which the per-frame inversion relies on.

use crate::math::{js_max, js_min, num_order};

/// The most dimensions [`nelder_mead`] minimises over.
pub const MAX_DIM: usize = 8;

/// Options of [`nelder_mead`]; `None` takes the default noted on each field.
#[derive(Clone, Copy, Debug, Default)]
pub struct NelderMeadOpts<'a> {
    /// initial simplex step per coordinate, either sign (default 0.1·|x0| or 0.05 near zero).
    /// The step's vertex is clamped into the box like every other point, so a step out of the
    /// box from an `x0` on its bound collapses onto `x0` and that coordinate never moves: step
    /// into the box (negative from an upper bound)
    pub step: Option<&'a [f64]>,
    /// lower bounds (default −∞)
    pub lo: Option<&'a [f64]>,
    /// upper bounds (default +∞)
    pub hi: Option<&'a [f64]>,
    /// default 400·n
    pub max_iter: Option<usize>,
    /// relative spread of the simplex values at which to stop (default 1e-7)
    pub tol: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NelderMeadResult {
    x: [f64; MAX_DIM],
    n: usize,
    pub fx: f64,
    pub iters: usize,
}

impl NelderMeadResult {
    /// The minimiser found.
    pub fn x(&self) -> &[f64] {
        &self.x[..self.n]
    }
}

type Point = [f64; MAX_DIM];

/// Nelder–Mead simplex minimiser with box bounds (clamped). `x0` has at most [`MAX_DIM`]
/// coordinates.
pub fn nelder_mead(
    mut f: impl FnMut(&[f64]) -> f64,
    x0: &[f64],
    opts: NelderMeadOpts,
) -> NelderMeadResult {
    let n = x0.len();
    assert!(
        (1..=MAX_DIM).contains(&n),
        "nelder_mead: 1 to {MAX_DIM} dimensions"
    );
    let mut lo = [f64::NEG_INFINITY; MAX_DIM];
    let mut hi = [f64::INFINITY; MAX_DIM];
    if let Some(l) = opts.lo {
        lo[..n].copy_from_slice(&l[..n]);
    }
    if let Some(h) = opts.hi {
        hi[..n].copy_from_slice(&h[..n]);
    }
    let clamp_x = |x: &mut Point| {
        for i in 0..n {
            x[i] = js_min(hi[i], js_max(lo[i], x[i]));
        }
    };
    let mut step = [0.0; MAX_DIM];
    match opts.step {
        Some(s) => step[..n].copy_from_slice(&s[..n]),
        None => {
            for i in 0..n {
                step[i] = if x0[i].abs() > 1e-3 {
                    0.1 * x0[i].abs()
                } else {
                    0.05
                };
            }
        }
    }
    let max_iter = opts.max_iter.unwrap_or(400 * n);
    let tol = opts.tol.unwrap_or(1e-7);
    let mut start = [0.0; MAX_DIM];
    start[..n].copy_from_slice(x0);
    let mut simplex = [[0.0; MAX_DIM]; MAX_DIM + 1];
    simplex[0] = start;
    clamp_x(&mut simplex[0]);
    for i in 0..n {
        let mut p = start;
        p[i] += step[i];
        clamp_x(&mut p);
        simplex[i + 1] = p;
    }
    let mut values = [0.0; MAX_DIM + 1];
    for i in 0..=n {
        values[i] = f(&simplex[i][..n]);
    }
    let mut it = 0;
    let mut idx = [0usize; MAX_DIM + 1];
    while it < max_iter {
        // stable sort by value, as `sort((a, b) => values[a] - values[b])`
        for (i, slot) in idx[..=n].iter_mut().enumerate() {
            *slot = i;
        }
        for i in 1..=n {
            let cur = idx[i];
            let mut j = i;
            while j > 0 && num_order(values[idx[j - 1]], values[cur]).is_gt() {
                idx[j] = idx[j - 1];
                j -= 1;
            }
            idx[j] = cur;
        }
        let (old_s, old_v) = (simplex, values);
        for i in 0..=n {
            simplex[i] = old_s[idx[i]];
            values[i] = old_v[idx[i]];
        }
        if (values[n] - values[0]).abs() < tol * (values[0].abs() + 1e-12) {
            break;
        }
        let mut centroid = [0.0; MAX_DIM];
        for s in &simplex[..n] {
            for j in 0..n {
                centroid[j] += s[j] / n as f64;
            }
        }
        let worst = simplex[n];
        let mut xr = [0.0; MAX_DIM];
        for j in 0..n {
            xr[j] = centroid[j] + (centroid[j] - worst[j]);
        }
        clamp_x(&mut xr);
        let fr = f(&xr[..n]);
        if fr < values[0] {
            let mut xe = [0.0; MAX_DIM];
            for j in 0..n {
                xe[j] = centroid[j] + 2.0 * (centroid[j] - worst[j]);
            }
            clamp_x(&mut xe);
            let fe = f(&xe[..n]);
            if fe < fr {
                simplex[n] = xe;
                values[n] = fe;
            } else {
                simplex[n] = xr;
                values[n] = fr;
            }
        } else if fr < values[n - 1] {
            simplex[n] = xr;
            values[n] = fr;
        } else {
            let mut xc = [0.0; MAX_DIM];
            for j in 0..n {
                xc[j] = centroid[j] + 0.5 * (worst[j] - centroid[j]);
            }
            clamp_x(&mut xc);
            let fc = f(&xc[..n]);
            if fc < values[n] {
                simplex[n] = xc;
                values[n] = fc;
            } else {
                let s0 = simplex[0];
                for i in 1..=n {
                    let mut shrunk = [0.0; MAX_DIM];
                    for j in 0..n {
                        shrunk[j] = s0[j] + 0.5 * (simplex[i][j] - s0[j]);
                    }
                    clamp_x(&mut shrunk);
                    simplex[i] = shrunk;
                    values[i] = f(&simplex[i][..n]);
                }
            }
        }
        it += 1;
    }
    let mut best = 0;
    for i in 1..=n {
        if values[i] < values[best] {
            best = i;
        }
    }
    NelderMeadResult {
        x: simplex[best],
        n,
        fx: values[best],
        iters: it,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_negative_step_from_the_upper_bound_moves_off_it() {
        // x0 on the upper bound, the minimum inside the box
        let bowl = |p: &[f64]| (p[0] - 0.4).powi(2) + (p[1] - 0.3).powi(2);
        let r = nelder_mead(
            bowl,
            &[1.0, 0.5],
            NelderMeadOpts {
                step: Some(&[-0.5, 0.25]),
                lo: Some(&[0.0, 0.0]),
                hi: Some(&[1.0, 1.0]),
                max_iter: None,
                tol: Some(1e-12),
            },
        );
        assert!((r.x()[0] - 0.4).abs() < 1e-4, "x = {:?}", r.x());
        assert!((r.x()[1] - 0.3).abs() < 1e-4, "x = {:?}", r.x());
    }
}
