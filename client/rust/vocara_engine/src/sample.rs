//! Core sample types and helpers.
//!
//! The whole engine works on **interleaved stereo** `f32` buffers: one buffer holds
//! `frames * CHANNELS` samples laid out `L0, R0, L1, R1, ...`. A graph "port" is one such
//! buffer. This keeps the buffer pool uniform (every buffer is `max_block * CHANNELS`) and
//! lets pan/width operate without a separate per-channel layout.

/// The engine's working sample type. The RT graph is f32; offline analysis (Dart side)
/// keeps its own f64 path.
pub type Smp = f32;

/// Channel count. Fixed stereo — vocals are panned, the reverb has width.
pub const CHANNELS: usize = 2;

/// The engine's fixed internal sample rate. The DSP graph always runs at this rate,
/// independent of any audio device. The device boundary (output now, input later)
/// resamples between `ENGINE_SR` and each device's native rate, so switching devices —
/// or running different input/output devices — never re-prepares the graph.
pub const ENGINE_SR: f64 = 48_000.0;

/// Smallest level we treat as audible when converting to dB (avoids `log10(0)`).
const DB_FLOOR: f32 = 1.0e-9;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    // 10^(db/20)
    (10.0f32).powf(db * 0.05)
}

#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    20.0 * (lin.abs() + DB_FLOOR).log10()
}

/// Equal-power pan law. `pan` in [-1, 1]; returns (left_gain, right_gain). Center keeps
/// each channel at -3 dB so a centered mono image sums to unity power.
#[inline]
pub fn equal_power_pan(pan: f32) -> (f32, f32) {
    // Map [-1,1] -> [0, pi/2]; left = cos, right = sin.
    let p = pan.clamp(-1.0, 1.0);
    let angle = (p + 1.0) * (core::f32::consts::FRAC_PI_4); // 0..pi/2
    (angle.cos(), angle.sin())
}

/// Zero an interleaved stereo buffer (only the live `frames` region).
#[inline]
pub fn clear(buf: &mut [Smp], frames: usize) {
    for s in &mut buf[..frames * CHANNELS] {
        *s = 0.0;
    }
}

/// Accumulate `src` into `dst` (interleaved, `frames` region). The fan-in sum primitive.
#[inline]
pub fn add_into(dst: &mut [Smp], src: &[Smp], frames: usize) {
    for i in 0..frames * CHANNELS {
        dst[i] += src[i];
    }
}
