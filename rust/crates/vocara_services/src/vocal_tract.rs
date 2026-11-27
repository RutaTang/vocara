//! The "Vocal tract" view's estimates: the singer's articulation, read from the voice analysis
//! the app already runs, live from the mic and along a take.
//!
//! Both run `vocara_tract`'s inversion ([`Inverter`]) over the analysis' frames: F0, F1–F3 and
//! how surely the frame is voiced. Each frame's [`VocalTractState`] is the estimate (posture,
//! articulation, the model's formants, residual, confidence), its components in plain units,
//! and the vocal folds as the analysis measures them.
//!
//! - **Live** ([`VocalTractTracker`]): fed one [`LiveFrame`] per analysis hop; the monitor runs
//!   one while the view asks for it ([`crate::monitor_service::MonitorService::set_vocal_tract`])
//!   and keeps the latest state ([`crate::monitor_service::MonitorState::vocal_tract`]).
//! - **A take** ([`VocalTractTrack`]): computed once from the take's [`VoiceMetrics`], frame by
//!   frame as live, on a worker (a few seconds for a three-minute take); then
//!   [`VocalTractTrack::state_at`] answers any time of the take. The song screen's analysis
//!   cache computes and keeps it beside the take's other analyses.
//!
//! # The vocal folds
//!
//! Not fitted: [`VocalFolds`] passes F0 through, reads the register from H1−H2 as the app's
//! register map does ([`register_score`]: 0 chest, 1 head), and how breathy the voice is from the
//! cepstral peak prominence: on this app's CPP scale clean sung vowels read 2.3–3.3 (the
//! analysis' own synthetic voices, 110–440 Hz) and a pulse train with aspiration noise 0.6–0.9,
//! so breathiness runs from 0 at [`CPP_CLEAN`] to 1 at [`CPP_BREATHY`]. A display hint, not a
//! measurement of the glottis.

use std::sync::Arc;

use vocara_model::models::spectrogram::AnalysisFraming;
use vocara_model::models::voice_metrics::{register_score, RegisterOptions, VoiceMetrics};
use vocara_tract::anatomy::BodyMix;
use vocara_tract::codebook::Codebook;
use vocara_tract::components::{TractComponents, VocalFolds};
use vocara_tract::inversion::{suggest_body, FormantFrame, Inverter, TractEstimate};
use vocara_tract::posture::{Posture, N_FIT};

use crate::audio_dsp::LiveFrame;

/// CPP (this app's scale) of a clean, well-closed sung tone: breathiness 0.
pub const CPP_CLEAN: f64 = 2.5;
/// CPP of a breathy one: breathiness 1.
pub const CPP_BREATHY: f64 = 0.7;

/// The vocal folds of a frame (see the module docs); every field NaN when unvoiced.
pub fn folds_of(f0: f64, cpp: f64, h1h2: f64) -> VocalFolds {
    if f0.is_nan() || f0 <= 0.0 {
        return VocalFolds::SILENT;
    }
    let breathiness = if cpp.is_finite() {
        ((CPP_CLEAN - cpp) / (CPP_CLEAN - CPP_BREATHY)).clamp(0.0, 1.0)
    } else {
        f64::NAN
    };
    let register = if h1h2.is_finite() {
        register_score(h1h2, RegisterOptions::default())
    } else {
        f64::NAN
    };
    VocalFolds {
        f0_hz: f0,
        breathiness,
        register,
    }
}

/// What the view shows for one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VocalTractState {
    /// The inversion's estimate: posture, articulation (for the 2D and 3D geometry), the
    /// model's formants, residual, confidence, whether this moment was fitted or held.
    pub estimate: TractEstimate,
    /// The articulation in plain units.
    pub components: TractComponents,
    /// The vocal folds as measured.
    pub folds: VocalFolds,
}

impl VocalTractState {
    fn of(estimate: TractEstimate, body: BodyMix, folds: VocalFolds) -> VocalTractState {
        VocalTractState {
            components: TractComponents::of(&estimate.articulation, body),
            estimate,
            folds,
        }
    }
}

/// One frame of measures for [`VocalTractTracker::push`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameMeasures {
    /// When the frame is (s), increasing.
    pub time: f64,
    /// F0 (Hz), NaN when unvoiced.
    pub f0: f64,
    /// F1–F3 (Hz), NaN where not measured.
    pub formants: [f64; 3],
    /// How surely the frame is voiced, 0–1.
    pub confidence: f64,
    /// Cepstral peak prominence (this app's scale) and H1−H2 (dB), NaN when unknown.
    pub cpp: f64,
    pub h1h2: f64,
}

/// The inversion over a stream of analysis frames (see the module docs).
pub struct VocalTractTracker {
    inverter: Inverter,
    state: VocalTractState,
    /// live frames taken (their clock)
    live_frames: u64,
}

impl VocalTractTracker {
    /// A tracker for `body`. Builds the body's codebook if this process has not built it yet
    /// (a few tenths of a second): call [`Codebook::prewarm`] on a worker first, or
    /// [`VocalTractTracker::with_codebook`] with [`Codebook::cached`].
    pub fn new(body: BodyMix) -> VocalTractTracker {
        VocalTractTracker::with_codebook(Codebook::shared(body))
    }

    /// A tracker over a codebook at hand (its body's).
    pub fn with_codebook(codebook: Arc<Codebook>) -> VocalTractTracker {
        let body = codebook.body();
        let inverter = Inverter::with_codebook(codebook, Default::default());
        let state = VocalTractState::of(*inverter.estimate(), body, VocalFolds::SILENT);
        VocalTractTracker {
            inverter,
            state,
            live_frames: 0,
        }
    }

    pub fn body(&self) -> BodyMix {
        self.inverter.body()
    }

    /// The latest state.
    pub fn state(&self) -> &VocalTractState {
        &self.state
    }

    /// Forgets the frames so far (a new session, or a seek).
    pub fn reset(&mut self) {
        self.inverter.reset();
        self.live_frames = 0;
        self.state =
            VocalTractState::of(*self.inverter.estimate(), self.body(), VocalFolds::SILENT);
    }

    /// Takes one frame.
    pub fn push(&mut self, m: &FrameMeasures) -> &VocalTractState {
        let estimate = *self.inverter.update(&FormantFrame {
            time: m.time,
            f0: m.f0,
            formants: m.formants,
            confidence: m.confidence,
        });
        self.state = VocalTractState::of(estimate, self.body(), folds_of(m.f0, m.cpp, m.h1h2));
        &self.state
    }

    /// Takes the live analysis' next frame (`frame_rate`: its frames a second). Only sustained
    /// voice is fitted (the frame's `active`, which the live overlays wait for too), weighted
    /// by its YIN periodicity.
    pub fn push_live(&mut self, f: &LiveFrame, frame_rate: f64) -> &VocalTractState {
        let time = self.live_frames as f64 / frame_rate;
        self.live_frames += 1;
        self.push(&FrameMeasures {
            time,
            f0: f.f0,
            formants: [f.f1, f.f2, f.f3],
            confidence: if f.active { f.f0_confidence } else { 0.0 },
            cpp: f.cpp,
            h1h2: f.h1h2,
        })
    }
}

/// How many analysis frames a take's track steps by: half the analysis rate (about 47 a
/// second at 48 kHz) is smooth enough to watch, and halves the compute.
pub const TAKE_STRIDE: usize = 2;

/// One stored frame of a [`VocalTractTrack`] (single precision: it is for display).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TrackFrame {
    posture: [f32; N_FIT],
    model_formants: [f32; 3],
    measured: [f32; 3],
    f0: f32,
    residual: f32,
    confidence: f32,
    breathiness: f32,
    register: f32,
    fitted: bool,
}

/// A take's vocal-tract estimates, one per [`TAKE_STRIDE`] analysis frames of its
/// [`VoiceMetrics`] (see the module docs).
#[derive(Clone, Debug)]
pub struct VocalTractTrack {
    body: BodyMix,
    hop: i64,
    fft_size: i64,
    sample_rate: i64,
    frames: Vec<TrackFrame>,
}

impl AnalysisFraming for VocalTractTrack {
    /// The take's analysis frames the track spans.
    fn frames(&self) -> usize {
        self.frames.len() * TAKE_STRIDE
    }
    fn hop(&self) -> i64 {
        self.hop
    }
    fn fft_size(&self) -> i64 {
        self.fft_size
    }
    fn sample_rate(&self) -> i64 {
        self.sample_rate
    }
}

impl VocalTractTrack {
    /// The track of a take's metrics for `body`: the inversion run over every
    /// [`TAKE_STRIDE`]th frame, in order. A frame is voiced (confidence 1) where the analysis
    /// has its F0, F1 and F2, unvoiced (0) elsewhere. `wanted` is asked between stretches of
    /// frames; `None` when it says no.
    pub fn compute(
        metrics: &VoiceMetrics,
        body: BodyMix,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> Option<VocalTractTrack> {
        let mut tracker = VocalTractTracker::new(body);
        let n = metrics.frames;
        let mut frames = Vec::with_capacity(n / TAKE_STRIDE + 1);
        for i in (0..n).step_by(TAKE_STRIDE) {
            if i % 2048 == 0 && wanted.is_some_and(|w| !w()) {
                return None;
            }
            let at = |c: &[f32]| c.get(i).map_or(f64::NAN, |x| f64::from(*x));
            let (f0, f1, f2) = (at(&metrics.f0), at(&metrics.f1), at(&metrics.f2));
            let voiced = f0 > 0.0 && f1 > 0.0 && f2 > 0.0;
            let s = tracker.push(&FrameMeasures {
                time: metrics.time_of_frame(i as i64),
                f0,
                formants: [f1, f2, at(&metrics.f3)],
                confidence: if voiced { 1.0 } else { 0.0 },
                cpp: at(&metrics.cpp),
                h1h2: at(&metrics.h1h2),
            });
            let e = &s.estimate;
            frames.push(TrackFrame {
                posture: e.posture.0.map(|x| x as f32),
                model_formants: e.model_formants.map(|x| x as f32),
                measured: e.measured.map(|x| x as f32),
                f0: e.f0 as f32,
                residual: e.residual as f32,
                confidence: e.confidence as f32,
                breathiness: s.folds.breathiness as f32,
                register: s.folds.register as f32,
                fitted: e.fitted,
            });
        }
        Some(VocalTractTrack {
            body,
            hop: metrics.hop,
            fft_size: metrics.fft_size,
            sample_rate: metrics.sample_rate,
            frames,
        })
    }

    /// The body the track was computed for.
    pub fn body(&self) -> BodyMix {
        self.body
    }

    /// How many estimates it holds.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The memory its estimates take (for a cache's budget).
    pub fn bytes(&self) -> usize {
        self.frames.len() * std::mem::size_of::<TrackFrame>()
    }

    /// When estimate `i` is (s from the analysed audio's start).
    pub fn time_of(&self, i: usize) -> f64 {
        self.time_of_frame((i * TAKE_STRIDE) as i64)
    }

    /// The state at `seconds` into the analysed audio: the posture interpolated between the two
    /// estimates around it (so a view following the playhead moves smoothly), the rest from
    /// the nearer one; clamped to the track's ends. `None` for an empty track.
    pub fn state_at(&self, seconds: f64) -> Option<VocalTractState> {
        let last = self.frames.len().checked_sub(1)?;
        let u = (self.frame_position(seconds) / TAKE_STRIDE as f64).clamp(0.0, last as f64);
        let i = (u.floor() as usize).min(last);
        let j = (i + 1).min(last);
        let t = u - i as f64;
        let (a, b) = (&self.frames[i], &self.frames[j]);
        let near = if t < 0.5 { a } else { b };
        let mut posture = [0.0; N_FIT];
        for (k, p) in posture.iter_mut().enumerate() {
            *p = f64::from(a.posture[k]) + (f64::from(b.posture[k]) - f64::from(a.posture[k])) * t;
        }
        let posture = Posture(posture);
        let lerp = |x: f32, y: f32| f64::from(x) + (f64::from(y) - f64::from(x)) * t;
        let estimate = TractEstimate {
            time: seconds,
            posture,
            articulation: posture.articulation(),
            model_formants: near.model_formants.map(f64::from),
            measured: near.measured.map(f64::from),
            f0: f64::from(near.f0),
            residual: f64::from(near.residual),
            confidence: lerp(a.confidence, b.confidence),
            fitted: near.fitted,
        };
        let folds = if near.f0 > 0.0 {
            VocalFolds {
                f0_hz: f64::from(near.f0),
                breathiness: f64::from(near.breathiness),
                register: f64::from(near.register),
            }
        } else {
            VocalFolds::SILENT
        };
        Some(VocalTractState::of(estimate, self.body, folds))
    }
}

/// A body to start a take's track with: [`suggest_body`] over its voiced frames' F3, `None`
/// when too few have one.
pub fn suggested_body(metrics: &VoiceMetrics) -> Option<BodyMix> {
    suggest_body(
        metrics
            .f3
            .iter()
            .zip(&metrics.f0)
            .filter(|(_, f0)| **f0 > 0.0)
            .map(|(f3, _)| f64::from(*f3)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_dsp::tests::support::vowel;
    use crate::audio_dsp::{compute_voice_metrics, AnalysisParams, LiveVoiceAnalyzer};

    #[test]
    fn the_folds_read_the_analysis() {
        assert!(!folds_of(f64::NAN, 2.0, 0.0).voiced());
        let clean = folds_of(220.0, 3.0, -2.0);
        let breathy = folds_of(220.0, 0.7, 12.0);
        assert_eq!(clean.breathiness, 0.0);
        assert_eq!(breathy.breathiness, 1.0);
        assert!(clean.register < 0.4 && breathy.register > 0.6);
        assert!(folds_of(220.0, f64::NAN, f64::NAN).breathiness.is_nan());
    }

    /// A take of two vowels (the analysis' own source-filter test vowels: an open /a/, a pause,
    /// a close front /i/): the track follows them, answers any time, and is the same computed
    /// twice.
    #[test]
    fn a_take_track_follows_its_vowels() {
        let sr = 48000;
        let mut x = vowel(750.0, 1250.0, 2700.0, 150.0, 1.2);
        x.extend(vec![0.0f32; 24000]);
        x.extend(vowel(320.0, 2300.0, 3000.0, 150.0, 1.2));
        let m = compute_voice_metrics(&x, sr, AnalysisParams::default());
        let body = BodyMix::new(0.5, 0.0);
        let track = VocalTractTrack::compute(&m, body, None).expect("wanted");
        assert_eq!(track.len(), m.frames.div_ceil(TAKE_STRIDE));
        let a = track.state_at(0.8).unwrap();
        let i = track.state_at(2.5).unwrap();
        assert!(a.estimate.fitted && i.estimate.fitted);
        assert!(
            a.components.tongue_height < i.components.tongue_height - 0.3,
            "{:?} {:?}",
            a.components,
            i.components
        );
        assert!(i.components.tongue_frontness > a.components.tongue_frontness);
        assert!(a.components.jaw_mm > i.components.jaw_mm);
        // the silence between: held, fading
        let gap = track.state_at(1.45).unwrap();
        assert!(!gap.estimate.fitted && gap.estimate.confidence < a.estimate.confidence);
        assert!(!gap.folds.voiced());
        // the ends clamp
        assert!(track.state_at(-1.0).is_some() && track.state_at(99.0).is_some());
        // deterministic
        let again = VocalTractTrack::compute(&m, body, None).unwrap();
        assert_eq!(format!("{:?}", again.frames), format!("{:?}", track.frames));
        // unwanted: nothing
        assert!(VocalTractTrack::compute(&m, body, Some(&|| false)).is_none());
    }

    /// The live analysis of the analysis' own live test vowel (/a/-like: 700, 1200, 2600 Hz
    /// at 200 Hz), fed as the mic feeds it: fitted once the voice is sustained, an open vowel.
    #[test]
    fn the_live_tracker_follows_the_mic() {
        let mut analyzer = LiveVoiceAnalyzer::default();
        let mut tracker = VocalTractTracker::new(BodyMix::new(0.5, 0.0));
        let x = vowel(700.0, 1200.0, 2600.0, 200.0, 1.0);
        let rate = analyzer.frame_rate();
        let mut fitted = 0;
        for chunk in x.chunks(1000) {
            analyzer.add_samples(chunk, |f| {
                fitted += usize::from(tracker.push_live(&f, rate).estimate.fitted);
            });
        }
        assert!(fitted > 30, "{fitted}");
        let s = tracker.state();
        assert!(s.estimate.fitted && s.folds.voiced());
        assert!(s.estimate.residual < 0.1, "{}", s.estimate.residual);
        assert!(s.components.tongue_height < 0.6, "{:?}", s.components);
        tracker.reset();
        assert!(!tracker.state().estimate.fitted);
    }

    #[test]
    fn a_body_is_suggested_from_the_take() {
        let x = vowel(750.0, 1250.0, 2700.0, 150.0, 1.0);
        let m = compute_voice_metrics(&x, 48000, AnalysisParams::default());
        assert!(suggested_body(&m).is_some());
        let silence = compute_voice_metrics(&vec![0.0f32; 48000], 48000, AnalysisParams::default());
        assert!(suggested_body(&silence).is_none());
    }
}
