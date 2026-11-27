//! The adapter between the articulatory engine (`vocara_tract`, read through
//! `vocara_services::vocal_tract`) and the Vocal tract tab's [`TractPose`]:
//! the one place the tab meets the engine's types (see the
//! `vocal_tract_view` module docs for the seam).
//!
//! # Geometry
//!
//! The engine's articulation is in its template frame: centimetres, x
//! forward, y up, the glottis at the origin, the same frame the tab's figure
//! is drawn in. The figure is an adult male's head; the tongue body's centre
//! goes through the template-to-body transform ([`anatomy::tp`]) as the
//! engine's own drawing does, and the jaw's gap, the lips' opening and the
//! larynx's height are template lengths the figure takes as they are. The
//! tongue keeps the adult male's radius, less where it would cross the hard
//! palate: the engine's walls hold it off the palate, so the figure leaves
//! the same thin airway there.
//!
//! # Readouts
//!
//! The panel shows the engine's own components, in the singer's body's
//! units (the jaw's gap in millimetres, the tongue's height and frontness,
//! the lips' rounding, the larynx's height), the vocal folds as the voice
//! analysis measured them, the formants heard, and a fit from how well the
//! fitted shape's formants match them.

use vocara_services::vocal_tract::VocalTractState;
use vocara_tract::components::{TractComponents, VocalFolds};
use vocara_tract::inversion::TractEstimate;

use crate::ui::song_practice_detail::view_model::analysis_cache::DEFAULT_TRACT_BODY;

use super::vocal_tract_view::anatomy::{self, tp};
use super::vocal_tract_view::demo::{articulate, Articulation};
use super::vocal_tract_view::geometry::nearest;
use super::vocal_tract_view::pose::{Cm, Fit, TractPose};

/// The adult male's tongue body radius (cm), the engine's.
pub const TONGUE_R: f32 = 2.25;
/// The least the tongue's body comes to the hard palate (cm).
pub const PALATE_GAP: f32 = 0.12;
/// The tongue body's smallest drawn radius (cm).
pub const MIN_TONGUE_R: f32 = 1.8;
/// The jaw's range at the incisors (cm), the engine's.
pub const JAW_RANGE: (f32, f32) = (0.05, 2.3);
/// The velopharyngeal port (cm²) drawn as a fully lowered velum.
pub const VELUM_FULL_CM2: f32 = 1.0;
/// The fitted formants' RMS relative error up to which the fit reads good,
/// and up to which it reads fair.
pub const FIT_GOOD: f64 = 0.05;
pub const FIT_FAIR: f64 = 0.10;

/// The pose the tab draws for the engine's `state`.
pub fn pose_of(state: &VocalTractState) -> TractPose {
    let e = &state.estimate;
    let a = &e.articulation;
    let c = &state.components;
    let folds = &state.folds;
    let voiced = folds.voiced();
    let tongue = tp(a.tb_x as f32, a.tb_y as f32);
    let art = Articulation {
        jaw_open: finite_or(a.jaw as f32, 0.6).clamp(JAW_RANGE.0, JAW_RANGE.1),
        tongue,
        tongue_r: tongue_radius(tongue),
        lip_round: finite_or(c.lip_rounding as f32, 0.3).clamp(0.0, 1.0),
        lip_open: finite_or(a.lip_ap as f32, 0.7).clamp(0.0, 2.5),
        velum_open: velum_open(c.velum_port_cm2),
        larynx: finite_or(a.larynx as f32, 0.0).clamp(-1.5, 1.5),
        f0: if voiced { folds.f0_hz as f32 } else { 0.0 },
        quality: voice_quality(folds.breathiness),
        formants: formants_of(e.measured, e.model_formants),
    };
    let mut pose = articulate(&art, voiced, fit_of(e.residual, e.fitted, voiced));
    // The readouts in the singer's body's units, the engine's.
    let k = &mut pose.components;
    k.jaw_mm = finite_or(c.jaw_mm as f32, k.jaw_mm).max(0.0);
    k.tongue_height = finite_or(c.tongue_height as f32, k.tongue_height).clamp(0.0, 1.0);
    k.tongue_front = finite_or(c.tongue_frontness as f32, k.tongue_front).clamp(0.0, 1.0);
    k.lip_aperture_mm = finite_or(c.lip_aperture_mm as f32, k.lip_aperture_mm).max(0.0);
    k.larynx_mm = finite_or(c.larynx_mm as f32, k.larynx_mm);
    pose
}

/// The pose of the engine's neutral posture with no voice: what the tab
/// takes in where a take has no estimate (the playhead outside it).
pub fn resting_pose() -> TractPose {
    let estimate = TractEstimate::initial();
    pose_of(&VocalTractState {
        components: TractComponents::of(&estimate.articulation, DEFAULT_TRACT_BODY),
        estimate,
        folds: VocalFolds::SILENT,
    })
}

/// The tongue body's radius for a centre at `centre`: the engine's, less
/// where it would come closer than [`PALATE_GAP`] to the hard palate.
pub fn tongue_radius(centre: Cm) -> f32 {
    let (_, to_palate) = nearest(&anatomy::anatomy().palate_surface, centre);
    (to_palate - PALATE_GAP).clamp(MIN_TONGUE_R, TONGUE_R)
}

/// The velum's lowering, 0 closed to 1 open, from the port's area.
pub fn velum_open(port_cm2: f64) -> f32 {
    finite_or(port_cm2 as f32 / VELUM_FULL_CM2, 0.0).clamp(0.0, 1.0)
}

/// Breathy (0) to pressed (1) from the analysis' breathiness (0 a firm
/// closure, 1 very breathy): a clean voice reads clear, mid-scale, as the
/// analysis cannot tell pressed from clean.
pub fn voice_quality(breathiness: f64) -> f32 {
    let b = finite_or(breathiness as f32, 0.0).clamp(0.0, 1.0);
    0.55 - 0.45 * b
}

/// The formants heard, where measured; the fitted shape's elsewhere.
pub fn formants_of(measured: [f64; 3], model: [f64; 3]) -> [f32; 3] {
    let ok = |f: f64| f.is_finite() && f > 0.0;
    std::array::from_fn(|i| {
        if ok(measured[i]) {
            measured[i] as f32
        } else if ok(model[i]) {
            model[i] as f32
        } else {
            0.0
        }
    })
}

/// How well the fitted shape explains the formants heard: its formants' RMS
/// relative error `residual`; a frame not fitted (held from earlier) while
/// there is a voice is uncertain.
pub fn fit_of(residual: f64, fitted: bool, voiced: bool) -> Fit {
    if !fitted {
        return if voiced { Fit::Uncertain } else { Fit::Good };
    }
    match residual {
        r if r <= FIT_GOOD => Fit::Good,
        r if r <= FIT_FAIR => Fit::Fair,
        _ => Fit::Uncertain,
    }
}

fn finite_or(x: f32, or: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        or
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vocara_services::vocal_tract::{folds_of, FrameMeasures, VocalTractTracker};
    use vocara_tract::anatomy::BodyMix;

    use crate::ui::song_practice_detail::widgets::vocal_tract_view::geometry::bounds;

    const MALE: BodyMix = BodyMix {
        female: 0.0,
        child: 0.0,
    };

    fn state_of(estimate: TractEstimate, folds: VocalFolds) -> VocalTractState {
        VocalTractState {
            components: TractComponents::of(&estimate.articulation, MALE),
            estimate,
            folds,
        }
    }

    fn sung(f0: f64) -> VocalFolds {
        folds_of(f0, 2.8, 4.0)
    }

    /// The tracker's state after holding `formants` for half a second.
    fn held(formants: [f64; 3]) -> VocalTractState {
        let mut t = VocalTractTracker::new(MALE);
        let mut s = *t.state();
        for i in 0..50 {
            s = *t.push(&FrameMeasures {
                time: f64::from(i) * 0.01,
                f0: 220.0,
                formants,
                confidence: 0.95,
                cpp: 2.8,
                h1h2: 4.0,
            });
        }
        s
    }

    #[test]
    fn the_neutral_estimate_draws_a_resting_figure_in_the_figures_frame() {
        let p = resting_pose();
        assert!(!p.voiced);
        assert_eq!(p.components.f0_hz, 0.0);
        // The neutral tongue sits in the mouth, under the palate.
        let (lo, hi) = bounds(&p.outlines.tongue);
        assert!(lo.x > -1.5 && hi.x < 7.0, "{lo:?} {hi:?}");
        assert!(hi.y < 10.3, "{hi:?}");
        assert!(p.outlines.all().iter().all(|(o, _)| o.len() >= 3));
    }

    #[test]
    fn open_and_close_vowels_move_the_jaw_and_tongue_the_right_way() {
        let a = held([730.0, 1090.0, 2440.0]);
        let i = held([270.0, 2290.0, 3010.0]);
        let u = held([300.0, 870.0, 2240.0]);
        let (pa, pi, pu) = (pose_of(&a), pose_of(&i), pose_of(&u));
        for p in [&pa, &pi, &pu] {
            assert!(p.voiced);
            assert_eq!(p.components.f0_hz, 220.0);
        }
        // /a/ opens the jaw more than /i/; /i/'s tongue is higher and more
        // front than /a/'s; /u/ rounds the lips more than /i/.
        assert!(pa.components.jaw_mm > pi.components.jaw_mm + 3.0);
        assert!(pi.components.tongue_height > pa.components.tongue_height + 0.3);
        assert!(pi.components.tongue_front > pu.components.tongue_front + 0.2);
        assert!(pu.components.lip_round > pi.components.lip_round + 0.2);
        // The fit of a vowel the model can make is good, and the formants
        // shown are the ones heard.
        assert_eq!(pa.fit, Fit::Good, "{:?}", a.estimate.residual);
        assert_eq!(pa.formants, [730.0, 1090.0, 2440.0]);
    }

    #[test]
    fn the_tongue_stays_off_the_hard_palate() {
        let palate = &anatomy::anatomy().palate_surface;
        for f in [
            [270.0, 2290.0, 3010.0],
            [300.0, 870.0, 2240.0],
            [400.0, 2000.0, 2600.0],
        ] {
            let p = pose_of(&held(f));
            for q in &p.outlines.tongue {
                let (_, d) = nearest(palate, *q);
                let below = palate.iter().all(|s| (s.x - q.x).abs() > 0.4 || q.y < s.y);
                assert!(below || d > 0.05, "{f:?}: {q:?} {d}");
            }
        }
        assert_eq!(tongue_radius(Cm::new(2.0, 5.0)), TONGUE_R);
        assert!(tongue_radius(Cm::new(3.2, 8.5)) < TONGUE_R);
        assert_eq!(tongue_radius(Cm::new(3.2, 9.9)), MIN_TONGUE_R);
    }

    #[test]
    fn the_folds_read_breathy_clear_from_the_analysis() {
        assert!(voice_quality(1.0) < 0.35, "breathy");
        let clean = voice_quality(0.0);
        assert!((0.35..0.7).contains(&clean), "clear");
        assert_eq!(voice_quality(f64::NAN), clean);
        let p = pose_of(&state_of(TractEstimate::initial(), sung(392.0)));
        assert!(p.voiced);
        assert_eq!(p.components.f0_hz, 392.0);
    }

    #[test]
    fn the_fit_and_formants_fall_back_sensibly() {
        assert_eq!(fit_of(0.01, true, true), Fit::Good);
        assert_eq!(fit_of(0.08, true, true), Fit::Fair);
        assert_eq!(fit_of(0.3, true, true), Fit::Uncertain);
        assert_eq!(fit_of(f64::NAN, true, true), Fit::Uncertain);
        assert_eq!(fit_of(f64::NAN, false, true), Fit::Uncertain);
        assert_eq!(
            formants_of([500.0, f64::NAN, 0.0], [510.0, 1500.0, f64::NAN]),
            [500.0, 1500.0, 0.0]
        );
        assert_eq!(velum_open(-0.15), 0.0);
        assert_eq!(velum_open(0.5), 0.5);
        assert_eq!(velum_open(f64::NAN), 0.0);
    }
}
