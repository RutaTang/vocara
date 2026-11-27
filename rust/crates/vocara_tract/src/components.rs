//! What an estimate means, in plain units: how open the jaw is, where the tongue is, how round
//! the lips are, where the larynx sits, whether the velum is open, and the vocal folds.
//!
//! [`TractComponents::of`] reads an articulation (an estimate's, or any) for a body. Each
//! component, what it means and which formants move it (the inversion fits them to F1–F3, and
//! three formants cannot fix seven articulators: the less a component moves the formants, the
//! more of it is the prior's guess, see [`crate::inversion`]):
//!
//! | Component | Meaning | Formants |
//! |---|---|---|
//! | [`jaw_mm`](TractComponents::jaw_mm), [`jaw`](TractComponents::jaw) | the gap between the incisors (mm at the body's size); and as a share of the fit's range, 0.5–23 mm on the adult male | F1 up as it opens (with the tongue's height) |
//! | [`tongue_height`](TractComponents::tongue_height) | the tongue body's height, 0 as for /a ɑ æ/, 1 as for /i u y/ | F1 down as it rises |
//! | [`tongue_frontness`](TractComponents::tongue_frontness) | the tongue body's place, 0 as for /u o ɔ ɑ/, 1 as for /i e ɛ æ/ | F2 up as it fronts (F2 − F1 the clearest cue) |
//! | [`tongue_root_mm`](TractComponents::tongue_root_mm) | the tongue root's advance (+) or retraction (−) from rest | F1 (a wider pharynx lowers it); weakly determined |
//! | [`lip_aperture`](TractComponents::lip_aperture), [`lip_aperture_mm`](TractComponents::lip_aperture_mm) | the lips' opening, 0 closed, 1 at 20 mm on the adult male | all formants down as it narrows, F2 most |
//! | [`lip_rounding`](TractComponents::lip_rounding) | 0 spread, 1 rounded and protruded: half protrusion (of 14 mm), half narrowing | F2 and F3 down (a longer, narrower front tube) |
//! | [`larynx_mm`](TractComponents::larynx_mm) | the larynx's height from rest (+ raised) | every formant up as it rises (a shorter tract), F3 and F4 most |
//! | [`velum_open`](TractComponents::velum_open), [`velum_port_cm2`](TractComponents::velum_port_cm2) | the velopharyngeal port | none fitted: the inversion holds it closed |
//!
//! The tongue's 0–1 scales are anchored on the body's own calibrated vowels (CHIMERA fitted
//! each body's, and a child's open vowels hold the tongue higher in the template than a man's;
//! blended by weight for a mixed body), the others on fixed ranges; all are clamped, so a
//! posture past an anchor reads 0 or 1. Millimetres are at the body's size (template
//! centimetres times its scale).
//!
//! The vocal folds are not part of the articulation: [`VocalFolds`] carries what the voice
//! analysis measures of them (F0, how breathy, how head-like the register), passed through.

use std::sync::OnceLock;

use crate::anatomy::{BodyMix, BodyName, HumanArtic};
use crate::math::clamp;
use crate::vowels::vowel;

/// The vowels whose tongue body anchors [`TractComponents::tongue_height`] (open: 0, close: 1)
/// and [`TractComponents::tongue_frontness`] (back: 0, front: 1), averaged.
pub const OPEN_VOWELS: [&str; 3] = ["a", "ɑ", "æ"];
pub const CLOSE_VOWELS: [&str; 5] = ["i", "u", "y", "ɨ", "ɯ"];
pub const BACK_VOWELS: [&str; 5] = ["u", "o", "ɔ", "ɒ", "ɑ"];
pub const FRONT_VOWELS: [&str; 4] = ["i", "e", "ɛ", "æ"];

/// A body's tongue anchors (template cm): open and close heights (tbY), back and front places
/// (tbX).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TongueAnchors {
    pub open: f64,
    pub close: f64,
    pub back: f64,
    pub front: f64,
}

impl TongueAnchors {
    /// `body`'s anchors: its presets' calibrated vowels, blended by weight.
    pub fn of(body: BodyMix) -> TongueAnchors {
        static PRESETS: OnceLock<[TongueAnchors; 3]> = OnceLock::new();
        let presets = PRESETS.get_or_init(|| {
            [BodyName::Male, BodyName::Female, BodyName::Child].map(|b| {
                let mean = |ipas: &[&str], f: fn(&HumanArtic) -> f64| {
                    let found: Vec<f64> = ipas
                        .iter()
                        .filter_map(|i| vowel(b, i).map(|v| f(&v.artic)))
                        .collect();
                    found.iter().sum::<f64>() / found.len().max(1) as f64
                };
                TongueAnchors {
                    open: mean(&OPEN_VOWELS, |a| a.tb_y),
                    close: mean(&CLOSE_VOWELS, |a| a.tb_y),
                    back: mean(&BACK_VOWELS, |a| a.tb_x),
                    front: mean(&FRONT_VOWELS, |a| a.tb_x),
                }
            })
        });
        let mut out = TongueAnchors {
            open: 0.0,
            close: 0.0,
            back: 0.0,
            front: 0.0,
        };
        for (i, (_, w)) in body.weights().iter().enumerate() {
            let p = presets[i];
            out.open += w * p.open;
            out.close += w * p.close;
            out.back += w * p.back;
            out.front += w * p.front;
        }
        out
    }
}

/// The jaw's range (template cm at the incisors): the fit's search box.
pub const JAW_RANGE: (f64, f64) = (0.05, 2.3);
/// The lip opening read as wide open (template cm).
pub const LIP_OPEN: f64 = 2.0;
/// Full protrusion (template cm): the fit's bound.
pub const PROTRUSION_FULL: f64 = 1.4;

/// The articulation's components (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TractComponents {
    /// The incisors' gap (mm).
    pub jaw_mm: f64,
    /// The jaw's opening as a share of its range, 0–1.
    pub jaw: f64,
    /// 0 low (/a/) – 1 high (/i u/).
    pub tongue_height: f64,
    /// 0 back (/u o/) – 1 front (/i e/).
    pub tongue_frontness: f64,
    /// The tongue root's advance from rest (mm, + forward).
    pub tongue_root_mm: f64,
    /// The lips' opening (mm), 0 when they touch.
    pub lip_aperture_mm: f64,
    /// The lips' opening, 0–1.
    pub lip_aperture: f64,
    /// 0 spread – 1 rounded and protruded.
    pub lip_rounding: f64,
    /// The larynx from rest (mm, + raised).
    pub larynx_mm: f64,
    /// The velopharyngeal port's area (cm²).
    pub velum_port_cm2: f64,
    /// Whether the velum is open (the port's area above zero).
    pub velum_open: bool,
}

impl TractComponents {
    /// The components of `a` for `body`.
    pub fn of(a: &HumanArtic, body: BodyMix) -> TractComponents {
        let k = body.body().scale;
        let unit = |lo: f64, hi: f64, x: f64| clamp((x - lo) / (hi - lo), 0.0, 1.0);
        let port = a.velum.max(0.0) * k * k;
        let t = TongueAnchors::of(body);
        TractComponents {
            jaw_mm: a.jaw.max(0.0) * k * 10.0,
            jaw: unit(JAW_RANGE.0, JAW_RANGE.1, a.jaw),
            tongue_height: unit(t.open, t.close, a.tb_y),
            tongue_frontness: unit(t.back, t.front, a.tb_x),
            tongue_root_mm: a.root * k * 10.0,
            lip_aperture_mm: a.lip_ap.max(0.0) * k * 10.0,
            lip_aperture: unit(0.0, LIP_OPEN, a.lip_ap),
            lip_rounding: 0.5 * unit(0.0, PROTRUSION_FULL, a.lip_prot)
                + 0.5 * (1.0 - clamp(a.lip_width, 0.0, 1.0)),
            larynx_mm: a.larynx * k * 10.0,
            velum_port_cm2: port,
            velum_open: port > 0.0,
        }
    }
}

/// The vocal folds as the voice analysis measures them (not fitted: passed through).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VocalFolds {
    /// Fundamental frequency (Hz): how fast they vibrate. NaN when unvoiced.
    pub f0_hz: f64,
    /// How breathy the voice is, 0 (firm closure) – 1 (very breathy): how far apart they stay;
    /// NaN when unknown.
    pub breathiness: f64,
    /// The register, 0 chest (thick folds) – 1 head (thin, stretched); NaN when unknown.
    pub register: f64,
}

impl VocalFolds {
    /// No voice.
    pub const SILENT: VocalFolds = VocalFolds {
        f0_hz: f64::NAN,
        breathiness: f64::NAN,
        register: f64::NAN,
    };

    /// Whether they vibrate.
    pub fn voiced(&self) -> bool {
        self.f0_hz > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::NEUTRAL;

    #[test]
    fn the_anchors_order_the_vowels() {
        for b in [BodyName::Male, BodyName::Female, BodyName::Child] {
            let t = TongueAnchors::of(b.into());
            assert!(
                t.close - t.open > 1.0 && t.front - t.back > 1.0,
                "{b:?} {t:?}"
            );
        }
        // a mix lies between its presets
        let (m, f) = (
            TongueAnchors::of(BodyMix::MALE),
            TongueAnchors::of(BodyMix::FEMALE),
        );
        let half = TongueAnchors::of(BodyMix::new(0.5, 0.0));
        assert!((half.open - 0.5 * (m.open + f.open)).abs() < 1e-12);
    }

    #[test]
    fn the_corner_vowels_read_as_they_should() {
        for b in [BodyName::Male, BodyName::Female, BodyName::Child] {
            let c = |ipa: &str| TractComponents::of(&vowel(b, ipa).unwrap().artic, b.into());
            let (i, a, u) = (c("i"), c("ɑ"), c("u"));
            assert!(
                i.tongue_height > 0.8 && i.tongue_frontness > 0.8,
                "{b:?} {i:?}"
            );
            assert!(i.lip_rounding < 0.25, "{b:?} {i:?}");
            assert!(
                a.tongue_height < 0.2 && a.jaw > 0.6 && a.lip_aperture > 0.9,
                "{b:?} {a:?}"
            );
            assert!(
                u.tongue_height > 0.6 && u.tongue_frontness < 0.5,
                "{b:?} {u:?}"
            );
            assert!(
                u.lip_rounding > 0.6 && u.lip_rounding > i.lip_rounding + 0.4,
                "{b:?}"
            );
            assert!(a.jaw_mm > 12.0 && i.jaw_mm < 6.0, "{b:?}");
            assert!(!i.velum_open);
        }
        // a smaller body: the same posture, fewer millimetres
        let a = vowel(BodyName::Male, "ɑ").unwrap().artic;
        let man = TractComponents::of(&a, BodyMix::MALE);
        let child = TractComponents::of(&a, BodyMix::CHILD);
        assert!((child.jaw_mm / man.jaw_mm - 0.74).abs() < 1e-9);
        let nasal = HumanArtic {
            velum: 0.8,
            ..NEUTRAL
        };
        assert!(TractComponents::of(&nasal, BodyMix::MALE).velum_open);
    }
}
