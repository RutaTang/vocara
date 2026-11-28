//! Capture-path conditioning for the mic input: a 40 Hz second-order high-pass ([`HighPass`]),
//! and the [`VoicePath`] that splits the mic into the echo-cancelled take and the monitor. The
//! take's high-pass runs after the AEC, the monitor's on the mic itself; both before the linear
//! record gain. Platform-free pure DSP (lifted out of the
//! macOS VPIO glue) so the simulation tests can drive it.
//!
//! # The capture path is deliberately STATIC and LINEAR
//!
//! Everything between the AEC and the record/monitor/analysis fan-out is time-invariant and
//! linear: this fixed high-pass and a fixed (user-set) gain. There is intentionally NO adaptive
//! stage — no gate, no expander, no AGC — and NO nonlinearity — no soft-knee ceiling. History:
//!
//! * A downward expander used to live here; its noise-floor estimate chased the envelope of
//!   HELD notes, which is the recorded "pumping" bug (onsets loud, sustains sagging by over
//!   10 dB, recovering after every breath). Singing is made of held notes; any dynamics in the
//!   capture path modulates the recorded voice sooner or later. Removed.
//! * A tanh soft-knee ceiling used to cap the boosted signal at +-1 for the 16-bit file; it
//!   baked irreversible harmonic distortion into every loud note. Recordings are now 32-bit
//!   float (levels past 0 dBFS are stored losslessly; the master bus's safety limiter protects
//!   playback), so the ceiling is gone and the gain is a pure scale. Removed.
//!
//! The audible hiss floor between phrases (the record gain also amplifies mic hiss) is the
//! accepted tradeoff — the user pulls the mic fader down or uses a quieter mic. Do not
//! reintroduce adaptive gain or a baked-in ceiling here; the regression tests pin both the
//! time-invariance and the linearity.
//!
//! # Why the high-pass corner sits at 40 Hz
//!
//! The filter removes DC offset and sub-vocal rumble (desk thumps, footfall, room/fan LF) that
//! would otherwise eat headroom and pollute the spectral analyses. It is SECOND-order
//! Butterworth at 40 Hz rather than the classic tracking low-cut at 75-80 Hz because the
//! analyses read this same signal: a one-pole 80 Hz corner attenuates a low male fundamental
//! measurably (about -1.8 dB at 110 Hz, -0.9 dB at E2 = 82 Hz) and biases H1-H2 by over a dB
//! in that range. At 40 Hz / 12 dB-per-octave the voice band is essentially untouched
//! (-0.25 dB at 82 Hz, well under -0.1 dB from 110 Hz up) while 30 Hz rumble is cut MORE than
//! the old one-pole managed (-6 dB vs -4 dB) and DC is blocked completely.

use std::f64::consts::PI;

use crate::aec::Aec;

/// Engine capture rate the coefficients are derived for.
const HP_RATE: f64 = 48_000.0;
/// High-pass corner (Hz). See the module docs for why 40 and not 75-80.
const HP_FC: f64 = 40.0;

/// 40 Hz second-order Butterworth high-pass @ 48 kHz (RBJ biquad, Q = 1/sqrt(2)), transposed
/// direct form II. Passes the sung range essentially untouched; kills DC and rumble. RT-safe,
/// O(1) per sample. (If the capture runs at a lower negotiated rate — the AEC-bypass duplex
/// path — the corner only moves DOWN in Hz, which errs in the transparent direction.)
pub struct HighPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for HighPass {
    fn default() -> Self {
        Self::new()
    }
}

impl HighPass {
    pub fn new() -> Self {
        // RBJ audio-EQ-cookbook high-pass, derived in f64 once at construction (not RT).
        let w0 = 2.0 * PI * HP_FC / HP_RATE;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        HighPass {
            b0: ((1.0 + cos_w0) / 2.0 / a0) as f32,
            b1: (-(1.0 + cos_w0) / a0) as f32,
            b2: ((1.0 + cos_w0) / 2.0 / a0) as f32,
            a1: (-2.0 * cos_w0 / a0) as f32,
            a2: ((1.0 - alpha) / a0) as f32,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// The mono mic's two signals after the channel mix, each high-passed by its own [`HighPass`]:
///
/// * the TAKE — echo-cancelled by the [`Aec`] — that the capture (analysis) and recording rings
///   carry, so the backing track does not bleed into what is kept;
/// * the MONITOR — the mic before the canceller — that the hear-yourself path plays. The
///   canceller works in blocks and emits one block late even in pass-through (headphones, the
///   only route the monitor is audible on), and that block (~5.3 ms at 48 kHz) was heard as
///   extra monitor latency for nothing: what the monitor plays never enters the canceller's
///   reference, and on headphones there is no echo to cancel.
///
/// Platform-free so the simulation tests drive it; the VoiceProcessingIO callback runs it once
/// per capture. RT-safe: no alloc / lock / syscall.
pub struct VoicePath {
    aec: Aec,
    take_hp: HighPass,
    monitor_hp: HighPass,
}

impl VoicePath {
    pub fn new(aec: Aec) -> VoicePath {
        VoicePath {
            aec,
            take_hp: HighPass::new(),
            monitor_hp: HighPass::new(),
        }
    }

    /// The canceller, for its per-callback route state (`set_echo_free`).
    pub fn aec_mut(&mut self) -> &mut Aec {
        &mut self.aec
    }

    /// One capture: `mic` (the mixed mono mic) and `far` (the far-end reference for the same
    /// samples; `None` when the canceller is bypassed) into `take` and `monitor`. Processes the
    /// shortest of the slices; `scratch` holds the canceller's output and is at least as long.
    pub fn process(
        &mut self,
        mic: &[f32],
        far: Option<&[f32]>,
        scratch: &mut [f32],
        take: &mut [f32],
        monitor: &mut [f32],
    ) {
        let n = mic
            .len()
            .min(scratch.len())
            .min(take.len())
            .min(monitor.len());
        for i in 0..n {
            monitor[i] = self.monitor_hp.process(mic[i]);
        }
        match far {
            Some(far) => {
                let n = n.min(far.len());
                self.aec.process(&mic[..n], &far[..n], &mut scratch[..n]);
            }
            None => scratch[..n].copy_from_slice(&mic[..n]),
        }
        for i in 0..n {
            take[i] = self.take_hp.process(scratch[i]);
        }
    }
}

#[cfg(test)]
mod voice_path_tests {
    use super::*;

    /// Index of the largest magnitude.
    fn peak_at(x: &[f32]) -> usize {
        (0..x.len())
            .max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs()))
            .unwrap()
    }

    /// An impulse through the path in the canceller's pass-through (headphones, where the monitor
    /// is audible): the take keeps the canceller's one-block latency, the monitor has none.
    #[test]
    fn the_monitor_skips_the_cancellers_block_of_latency() {
        let mut path = VoicePath::new(Aec::new(48_000.0, 250.0));
        path.aec_mut().set_echo_free(true);
        let n = 1024;
        let mut mic = vec![0.0f32; n];
        mic[10] = 0.5;
        let far = vec![0.0f32; n];
        let (mut scratch, mut take, mut monitor) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
        // Callback-sized pieces, as the unit delivers them.
        for c in (0..n).step_by(128) {
            path.process(
                &mic[c..c + 128],
                Some(&far[c..c + 128]),
                &mut scratch[c..c + 128],
                &mut take[c..c + 128],
                &mut monitor[c..c + 128],
            );
        }
        assert_eq!(
            peak_at(&monitor),
            10,
            "the monitor hears the impulse at once"
        );
        assert_eq!(
            peak_at(&take) - peak_at(&monitor),
            256,
            "the take is one canceller block behind"
        );
        // Both lanes are the same high-passed voice, only shifted.
        assert!((monitor[10] - take[266]).abs() < 1e-6);
    }

    /// With the canceller bypassed (mismatched element rates) both lanes are the high-passed mic.
    #[test]
    fn a_bypassed_canceller_leaves_both_lanes_the_same_voice() {
        let mut path = VoicePath::new(Aec::new(48_000.0, 250.0));
        let mic: Vec<f32> = (0..256).map(|i| ((i as f32) * 0.05).sin() * 0.3).collect();
        let (mut scratch, mut take, mut monitor) = (vec![0.0; 256], vec![0.0; 256], vec![0.0; 256]);
        path.process(&mic, None, &mut scratch, &mut take, &mut monitor);
        assert_eq!(take, monitor);
        assert!(take.iter().any(|&s| s.abs() > 0.1));
    }
}
