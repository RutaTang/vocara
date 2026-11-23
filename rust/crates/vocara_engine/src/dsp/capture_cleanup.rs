//! Capture-path conditioning for the mic input: a 40 Hz second-order high-pass ([`HighPass`]).
//! Runs after the AEC, before the linear record gain. Platform-free pure DSP (lifted out of the
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
