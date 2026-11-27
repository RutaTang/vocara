//! The articulations the inversion fits: seven articulators acoustics can tell apart, the rest
//! following them, and the prior that says which of those postures a singer plausibly holds.
//!
//! # The fitted subset
//!
//! [`FIT_KEYS`]: the jaw, the tongue body's centre (front/back, high/low), the tongue root, the
//! lips' aperture and protrusion, and the larynx. They shape the area function most, and their
//! effects on F1–F3 differ enough to be told apart (with the prior's help; three formants
//! cannot pin seven numbers). Everything else is held at a sung vowel's posture
//! ([`Posture::articulation`]):
//!
//! - the tongue tip rests behind the lower incisors, carried by the jaw and dragged by the
//!   tongue body (CHIMERA's [`rest_tip`], as its articulator bank sets a free tip), uncurled;
//! - the lips' spreading follows their protrusion: rounding protrudes and narrows them together
//!   (orbicularis oris), so width = 1 − 1.3·protrusion, clamped to 0–1. Over CHIMERA's
//!   calibrated vowels this is within 0.1 of the fitted width for the spread and the schwa-like
//!   ones, within 0.3 for the rounded ones;
//! - the velum is closed (CHIMERA's neutral −0.15 cm²: no port), no labiodental contact, no
//!   epilaryngeal or ventricular narrowing, no lateral channel, no dorsum dip.
//!
//! # The priors
//!
//! What "plausible" means to the inversion, in two parts, both over the seven fitted values in
//! template coordinates:
//!
//! - [`Prior`]: a Gaussian, its mean and covariance those of CHIMERA's calibrated sung vowels
//!   (all three bodies; the apical and rhotic vowels left out, as they curl the tip), widened by
//!   a floor on each articulator's spread. It keeps the fit near the space of vowels and its
//!   correlations (the jaw opening with the tongue lowering, protrusion with a narrower
//!   aperture), but alone it pulls every posture towards the middle, a schwa: an /i/ came out
//!   with the tongue at half height, made up for by the tongue root, the larynx and the lips.
//! - [`VowelPrior`]: the distance to the nearest calibrated vowel of the body (its presets'
//!   postures blended by weight), each articulator in units of its own spread. Near the edges
//!   of the vowel space the nearest vowel is an edge vowel, so an /i/ keeps its high tongue.

// Small fixed-size linear algebra (seven articulators, three formants) reads clearer indexed.
#![allow(clippy::needless_range_loop)]

use crate::anatomy::{rest_tip, ArticKey, BodyMix, BodyName, HumanArtic, NEUTRAL};
use crate::math::clamp;
use crate::vowels::calibrated;

/// How many articulators the inversion fits.
pub const N_FIT: usize = 7;

/// The fitted articulators, in the order of a [`Posture`]'s values.
pub const FIT_KEYS: [ArticKey; N_FIT] = [
    ArticKey::Jaw,
    ArticKey::TbX,
    ArticKey::TbY,
    ArticKey::Root,
    ArticKey::LipAp,
    ArticKey::LipProt,
    ArticKey::Larynx,
];

/// Indices into a [`Posture`]'s values.
pub const JAW: usize = 0;
pub const TB_X: usize = 1;
pub const TB_Y: usize = 2;
pub const ROOT: usize = 3;
pub const LIP_AP: usize = 4;
pub const LIP_PROT: usize = 5;
pub const LARYNX: usize = 6;

/// The search box (template cm): CHIMERA's vowel calibration's bounds.
pub const LO: [f64; N_FIT] = [0.05, 1.2, 5.0, -0.9, 0.12, 0.0, -1.3];
pub const HI: [f64; N_FIT] = [2.3, 4.6, 8.8, 0.9, 2.0, 1.4, 1.3];

/// The lips' spreading for a protrusion (see the module docs).
pub fn lip_width_for(protrusion: f64) -> f64 {
    clamp(1.0 - 1.3 * protrusion, 0.0, 1.0)
}

/// Seven fitted articulator values (template cm), in [`FIT_KEYS`] order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Posture(pub [f64; N_FIT]);

impl Posture {
    /// CHIMERA's neutral (schwa-like) posture's values.
    pub const NEUTRAL: Posture = Posture([
        NEUTRAL.jaw,
        NEUTRAL.tb_x,
        NEUTRAL.tb_y,
        NEUTRAL.root,
        NEUTRAL.lip_ap,
        NEUTRAL.lip_prot,
        NEUTRAL.larynx,
    ]);

    /// The fitted values of an articulation (the others are dropped).
    pub fn of(a: &HumanArtic) -> Posture {
        Posture(FIT_KEYS.map(|k| a.get(k)))
    }

    /// The whole articulation: these values, the rest following them (see the module docs).
    pub fn articulation(&self) -> HumanArtic {
        let p = &self.0;
        let tip = rest_tip(p[JAW], p[TB_X], p[TB_Y]);
        HumanArtic {
            jaw: p[JAW],
            tb_x: p[TB_X],
            tb_y: p[TB_Y],
            tt_x: tip.x,
            tt_y: tip.y,
            tt_curl: 0.0,
            root: p[ROOT],
            lip_ap: p[LIP_AP],
            lip_prot: p[LIP_PROT],
            lip_width: lip_width_for(p[LIP_PROT]),
            larynx: p[LARYNX],
            ..NEUTRAL
        }
    }

    /// Clamped into the search box.
    pub fn clamped(mut self) -> Posture {
        for i in 0..N_FIT {
            self.0[i] = clamp(self.0[i], LO[i], HI[i]);
        }
        self
    }
}

/// The calibrated vowels the prior is drawn from: CHIMERA's, without the apical vowels of
/// Mandarin (ɿ ʅ, the consonant's tongue held open) and the rhotic ɝ (the tip curled), which a
/// posture with a resting tip cannot hold.
pub fn sung_vowels(
    body: BodyName,
) -> impl Iterator<Item = &'static crate::vowels::CalibratedVowel> {
    calibrated(body)
        .iter()
        .filter(|v| !matches!(v.ipa, "ɿ" | "ʅ" | "ɝ"))
}

/// The smallest spread (template cm) the prior allows each articulator, so that no direction
/// the vowels happen not to vary along is forbidden.
const SPREAD_FLOOR: [f64; N_FIT] = [0.12, 0.15, 0.15, 0.15, 0.12, 0.12, 0.15];

/// A Gaussian over postures (see the module docs), kept as its mean and the inverse of its
/// covariance's Cholesky factor.
#[derive(Clone, Debug, PartialEq)]
pub struct Prior {
    pub mean: Posture,
    /// L⁻¹ for Σ = L·Lᵀ (lower triangular).
    l_inv: [[f64; N_FIT]; N_FIT],
}

impl Prior {
    /// The prior of the calibrated sung vowels of all three bodies.
    pub fn of_vowels() -> Prior {
        let samples: Vec<Posture> = [BodyName::Male, BodyName::Female, BodyName::Child]
            .into_iter()
            .flat_map(sung_vowels)
            .map(|v| Posture::of(&v.artic))
            .collect();
        Prior::of(&samples)
    }

    /// The Gaussian of `samples` (their mean and covariance, the spread floored).
    pub fn of(samples: &[Posture]) -> Prior {
        let n = samples.len().max(1) as f64;
        let mut mean = [0.0; N_FIT];
        for s in samples {
            for i in 0..N_FIT {
                mean[i] += s.0[i] / n;
            }
        }
        let mut cov = [[0.0; N_FIT]; N_FIT];
        for s in samples {
            for i in 0..N_FIT {
                for j in 0..N_FIT {
                    cov[i][j] += (s.0[i] - mean[i]) * (s.0[j] - mean[j]) / n;
                }
            }
        }
        for i in 0..N_FIT {
            cov[i][i] += SPREAD_FLOOR[i] * SPREAD_FLOOR[i];
        }
        let l = cholesky(&cov);
        Prior {
            mean: Posture(mean),
            l_inv: lower_inverse(&l),
        }
    }

    /// The squared Mahalanobis distance of `p` from the mean.
    pub fn distance2(&self, p: &[f64]) -> f64 {
        let mut d = [0.0; N_FIT];
        for i in 0..N_FIT {
            d[i] = p[i] - self.mean.0[i];
        }
        let mut sum = 0.0;
        for i in 0..N_FIT {
            let mut z = 0.0;
            for j in 0..=i {
                z += self.l_inv[i][j] * d[j];
            }
            sum += z * z;
        }
        sum
    }
}

/// How far (template cm) a posture may stray from a calibrated vowel per unit of
/// [`VowelPrior::distance2`], per fitted articulator.
pub const VOWEL_SPREAD: [f64; N_FIT] = [0.25, 0.3, 0.3, 0.3, 0.3, 0.25, 0.3];

/// The distance to the nearest of a body's calibrated sung vowels (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct VowelPrior {
    vowels: Vec<Posture>,
}

impl VowelPrior {
    /// The prior of `body`'s vowels: each sung vowel's posture blended over the presets by the
    /// body's weights, as CHIMERA's UI blends its vowel space.
    pub fn of(body: BodyMix) -> VowelPrior {
        let weights = body.weights();
        let vowels = sung_vowels(BodyName::Male)
            .map(|v| {
                let mut p = [0.0; N_FIT];
                let mut total = 0.0;
                for (b, w) in weights {
                    let Some(own) = sung_vowels(b).find(|x| x.ipa == v.ipa) else {
                        continue;
                    };
                    let q = Posture::of(&own.artic);
                    for j in 0..N_FIT {
                        p[j] += w * q.0[j];
                    }
                    total += w;
                }
                for x in &mut p {
                    *x /= total;
                }
                Posture(p)
            })
            .collect();
        VowelPrior { vowels }
    }

    /// The vowels' postures.
    pub fn vowels(&self) -> &[Posture] {
        &self.vowels
    }

    /// The squared distance of `p` to the nearest vowel, each articulator in units of
    /// [`VOWEL_SPREAD`].
    pub fn distance2(&self, p: &[f64]) -> f64 {
        let inv = VOWEL_SPREAD.map(|s| 1.0 / s);
        let mut best = f64::INFINITY;
        for v in &self.vowels {
            let mut d = 0.0;
            for j in 0..N_FIT {
                let e = (p[j] - v.0[j]) * inv[j];
                d += e * e;
            }
            best = best.min(d);
        }
        best
    }
}

/// Cholesky factor of a symmetric positive-definite matrix.
fn cholesky(a: &[[f64; N_FIT]; N_FIT]) -> [[f64; N_FIT]; N_FIT] {
    let mut l = [[0.0; N_FIT]; N_FIT];
    for i in 0..N_FIT {
        for j in 0..=i {
            let mut s = a[i][j];
            for k in 0..j {
                s -= l[i][k] * l[j][k];
            }
            l[i][j] = if i == j {
                s.max(1e-12).sqrt()
            } else {
                s / l[j][j]
            };
        }
    }
    l
}

/// The inverse of a lower-triangular matrix (lower triangular).
fn lower_inverse(l: &[[f64; N_FIT]; N_FIT]) -> [[f64; N_FIT]; N_FIT] {
    let mut inv = [[0.0; N_FIT]; N_FIT];
    for i in 0..N_FIT {
        inv[i][i] = 1.0 / l[i][i];
        for j in 0..i {
            let mut s = 0.0;
            for k in j..i {
                s += l[i][k] * inv[k][j];
            }
            inv[i][j] = -s / l[i][i];
        }
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posture_round_trips_through_its_articulation() {
        let p = Posture([1.0, 3.2, 6.6, 0.1, 0.9, 0.3, -0.2]);
        let a = p.articulation();
        assert_eq!(Posture::of(&a), p);
        let tip = rest_tip(1.0, 3.2, 6.6);
        assert_eq!((a.tt_x, a.tt_y), (tip.x, tip.y));
        assert!((a.lip_width - 0.61).abs() < 1e-12);
        assert_eq!(a.velum, NEUTRAL.velum);
    }

    #[test]
    fn the_prior_centres_on_the_vowels() {
        let prior = Prior::of_vowels();
        // the mean is a mid, central posture
        let m = prior.mean.0;
        assert!(
            m[TB_Y] > 6.2 && m[TB_Y] < 7.3 && m[TB_X] > 2.2 && m[TB_X] < 3.3,
            "{m:?}"
        );
        // the vowels sit at about the dimension's distance (χ² with 7 degrees of freedom)
        let d: Vec<f64> = sung_vowels(BodyName::Male)
            .map(|v| prior.distance2(&Posture::of(&v.artic).0))
            .collect();
        let mean = d.iter().sum::<f64>() / d.len() as f64;
        assert!(mean > 3.0 && mean < 10.0, "{mean}");
        // an implausible posture (jaw wide open, tongue high and front) is far
        let odd = Posture([2.2, 4.4, 8.6, 0.8, 0.2, 1.3, 1.2]);
        assert!(prior.distance2(&odd.0) > 60.0);
        assert!(prior.distance2(&prior.mean.0) < 1e-20);
    }
}
