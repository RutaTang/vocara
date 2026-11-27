//! The inversion's codebook: postures sampled over the plausible part of the search box, each
//! with the F1–F3 the forward model gives it, for one body.
//!
//! An inversion starts from the entries whose formants are nearest the measured ones (in
//! log-frequency, weighted as the fit weighs them): a good start is what lets a local refinement
//! land on the right posture after a jump (a new vowel after a breath) instead of the nearest
//! local minimum.
//!
//! The entries are, in this order: CHIMERA's calibrated sung vowels of the three bodies (each
//! projected onto the fitted articulators, [`Posture::of`]), the neutral posture, then
//! [`SAMPLED`] postures of a Halton sequence over the search box ([`LO`]..[`HI`]), keeping those
//! the prior finds plausible (squared Mahalanobis distance at most [`MAX_PRIOR_D2`]). An entry
//! whose airway has fewer than three formants is dropped. The sequence is fixed, so a body's
//! codebook is the same on every run and every machine.
//!
//! Building one takes a few tenths of a second (some 1,600 forward evaluations), so
//! [`Codebook::shared`] builds each body's once per process and keeps it; call it off the UI
//! thread before the first frame if a hitch matters ([`Codebook::prewarm`]).

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::anatomy::{BodyMix, BodyName};
use crate::forward::TractModel;
use crate::math::ln;
use crate::posture::{sung_vowels, Posture, Prior, HI, LO, N_FIT};

/// How many Halton postures a codebook keeps (besides its vowels).
pub const SAMPLED: usize = 1500;
/// The prior distance a sampled posture may lie at (χ² with 7 degrees of freedom passes this
/// with probability 1 in 10,000).
pub const MAX_PRIOR_D2: f64 = 27.0;
/// Halton draws before giving up on filling [`SAMPLED`].
const MAX_DRAWS: u32 = 400_000;
/// The first seven primes: one Halton base per fitted articulator.
const BASES: [u32; N_FIT] = [2, 3, 5, 7, 11, 13, 17];
/// How many bodies' codebooks the process keeps.
const CACHED_BODIES: usize = 6;

/// One body's codebook (see the module docs).
#[derive(Clone, Debug)]
pub struct Codebook {
    body: BodyMix,
    postures: Vec<Posture>,
    /// ln F1, ln F2, ln F3 of each posture
    log_formants: Vec<[f64; 3]>,
}

impl Codebook {
    /// Builds `body`'s codebook (see the module docs).
    pub fn build(body: BodyMix) -> Codebook {
        let mut model = TractModel::new(body);
        let prior = Prior::of_vowels();
        let mut postures: Vec<Posture> = [BodyName::Male, BodyName::Female, BodyName::Child]
            .into_iter()
            .flat_map(sung_vowels)
            .map(|v| Posture::of(&v.artic))
            .collect();
        postures.push(Posture::NEUTRAL);
        let mut kept = 0;
        let mut n = 1;
        while kept < SAMPLED && n <= MAX_DRAWS {
            let mut p = [0.0; N_FIT];
            for (i, value) in p.iter_mut().enumerate() {
                *value = LO[i] + halton(n, BASES[i]) * (HI[i] - LO[i]);
            }
            n += 1;
            if prior.distance2(&p) <= MAX_PRIOR_D2 {
                postures.push(Posture(p));
                kept += 1;
            }
        }
        let mut out = Codebook {
            body,
            postures: Vec::with_capacity(postures.len()),
            log_formants: Vec::with_capacity(postures.len()),
        };
        for p in postures {
            let f = model.lowest_formants(&p.articulation(), 3);
            if f.count == 3 {
                out.postures.push(p);
                out.log_formants
                    .push([ln(f.freq[0]), ln(f.freq[1]), ln(f.freq[2])]);
            }
        }
        out
    }

    /// `body`'s codebook, built on the first call for that body and shared after.
    pub fn shared(body: BodyMix) -> Arc<Codebook> {
        static CACHE: OnceLock<Mutex<Vec<Arc<Codebook>>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
        let find = |list: &[Arc<Codebook>]| list.iter().find(|c| same_body(c.body, body)).cloned();
        if let Some(c) = find(&cache.lock().unwrap_or_else(PoisonError::into_inner)) {
            return c;
        }
        // built outside the lock: another body's inversion need not wait for this one (two
        // threads asking for the same new body each build it once; the first stored is kept)
        let built = Arc::new(Codebook::build(body));
        let mut list = cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(c) = find(&list) {
            return c;
        }
        if list.len() >= CACHED_BODIES {
            list.remove(0);
        }
        list.push(Arc::clone(&built));
        built
    }

    /// Builds `body`'s shared codebook now (see the module docs).
    pub fn prewarm(body: BodyMix) {
        Codebook::shared(body);
    }

    /// The body this codebook is of.
    pub fn body(&self) -> BodyMix {
        self.body
    }

    pub fn len(&self) -> usize {
        self.postures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.postures.is_empty()
    }

    /// Entry `i`: its posture and its ln F1–F3.
    pub fn entry(&self, i: usize) -> (Posture, [f64; 3]) {
        (self.postures[i], self.log_formants[i])
    }

    /// The `out.len()` entries nearest the measured log-formants `y` under weights `w`
    /// (Σ wᵢ·(ln Fᵢ − yᵢ)²), nearest first, into `out`; returns how many were written.
    pub fn nearest(&self, y: &[f64; 3], w: &[f64; 3], out: &mut [usize]) -> usize {
        let k = out.len();
        let mut dist = [f64::INFINITY; MAX_NEAREST];
        let k = k.min(MAX_NEAREST);
        let mut n = 0;
        for (i, lf) in self.log_formants.iter().enumerate() {
            let mut d = 0.0;
            for j in 0..3 {
                let e = lf[j] - y[j];
                d += w[j] * e * e;
            }
            if n == k && d >= dist[k - 1] {
                continue;
            }
            // insert, keeping the list sorted (ties: the earlier entry first)
            let mut at = n.min(k - 1);
            if n < k {
                n += 1;
            }
            while at > 0 && dist[at - 1] > d {
                dist[at] = dist[at - 1];
                out[at] = out[at - 1];
                at -= 1;
            }
            dist[at] = d;
            out[at] = i;
        }
        n
    }
}

/// The most entries [`Codebook::nearest`] returns.
pub const MAX_NEAREST: usize = 16;

fn same_body(a: BodyMix, b: BodyMix) -> bool {
    a.female.to_bits() == b.female.to_bits() && a.child.to_bits() == b.child.to_bits()
}

/// The radical inverse of `i` in base `b` (the Halton sequence's `i`th value in [0, 1)).
fn halton(mut i: u32, b: u32) -> f64 {
    let mut f = 1.0;
    let mut r = 0.0;
    while i > 0 {
        f /= b as f64;
        r += f * (i % b) as f64;
        i /= b;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halton_is_the_radical_inverse() {
        assert_eq!(halton(1, 2), 0.5);
        assert_eq!(halton(2, 2), 0.25);
        assert_eq!(halton(3, 2), 0.75);
        assert!((halton(5, 3) - (2.0 / 3.0 + 1.0 / 9.0)).abs() < 1e-15);
    }

    #[test]
    fn nearest_returns_the_closest_in_order() {
        let cb = Codebook {
            body: BodyMix::MALE,
            postures: vec![Posture::NEUTRAL; 5],
            log_formants: vec![[0.0; 3], [3.0; 3], [1.0; 3], [2.0; 3], [1.0; 3]],
        };
        let mut out = [0usize; 3];
        assert_eq!(cb.nearest(&[1.1; 3], &[1.0; 3], &mut out), 3);
        assert_eq!(out, [2, 4, 3]);
        let mut all = [0usize; 8];
        assert_eq!(cb.nearest(&[2.9; 3], &[1.0; 3], &mut all), 5);
        assert_eq!(all[..5], [1, 3, 2, 4, 0]);
    }
}
