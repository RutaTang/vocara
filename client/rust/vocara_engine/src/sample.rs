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

/// Convert a frame count measured at `device_rate` into the equivalent frame count at
/// `ENGINE_SR`. CoreAudio reports device buffer sizes / latencies / safety offsets in frames
/// at the DEVICE's nominal rate, while the engine publishes latencies in engine-rate frames
/// (the Dart side divides by 48 000) — summing them unscaled skews every non-48 kHz device's
/// latency by its rate ratio. A non-finite or non-positive rate passes the count through.
#[inline]
pub fn frames_at_engine_rate(frames: u32, device_rate: f64) -> u32 {
    if device_rate.is_finite() && device_rate > 0.0 {
        ((frames as f64) * (ENGINE_SR / device_rate)).round() as u32
    } else {
        frames
    }
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
    let n = frames * CHANNELS;
    for (d, &s) in dst[..n].iter_mut().zip(&src[..n]) {
        *d += s;
    }
}

/// `dst = 0.0 + src` (interleaved, `frames` region): exactly [`clear`] followed by
/// [`add_into`], in one pass. Not a copy — the add maps a `-0.0` sample to `+0.0`, as summing
/// into a cleared buffer always has.
#[inline]
pub fn sum_first(dst: &mut [Smp], src: &[Smp], frames: usize) {
    let n = frames * CHANNELS;
    for (d, &s) in dst[..n].iter_mut().zip(&src[..n]) {
        *d = 0.0 + s;
    }
}

#[cfg(test)]
mod tests {
    use super::{add_into, clear, frames_at_engine_rate, sum_first, CHANNELS};

    /// The fan-in gather's fused first edge is exactly clear-then-add — including the `-0.0`
    /// it maps to `+0.0`, NaN payloads, infinities and subnormals.
    #[test]
    fn sum_first_is_clear_then_add_bit_for_bit() {
        let specials = [
            0.0f32,
            -0.0,
            1.0,
            -1.0e-40,
            f32::MIN_POSITIVE,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::from_bits(0x7FA0_0001), // a signalling NaN
            f32::from_bits(0xFFC0_1234), // a negative quiet NaN with a payload
            3.0e38,
        ];
        let src: Vec<f32> = specials
            .iter()
            .cycle()
            .take(64 * CHANNELS)
            .copied()
            .collect();
        let frames = src.len() / CHANNELS;
        let mut fused = vec![7.0f32; src.len()];
        let mut two_pass = vec![7.0f32; src.len()];
        sum_first(&mut fused, &src, frames);
        clear(&mut two_pass, frames);
        add_into(&mut two_pass, &src, frames);
        for (a, b) in fused.iter().zip(&two_pass) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn frames_scale_to_engine_rate() {
        // Identity at the engine rate.
        assert_eq!(frames_at_engine_rate(512, 48_000.0), 512);
        // 44.1 kHz device frames grow by 48/44.1 (e.g. a 44.1 kHz USB interface).
        assert_eq!(frames_at_engine_rate(441, 44_100.0), 480);
        assert_eq!(frames_at_engine_rate(512, 44_100.0), 557); // 512 * 48/44.1 = 557.28 → 557
                                                               // High-rate device frames shrink.
        assert_eq!(frames_at_engine_rate(960, 96_000.0), 480);
        assert_eq!(frames_at_engine_rate(882, 88_200.0), 480);
        // Degenerate rates pass the count through unchanged.
        assert_eq!(frames_at_engine_rate(512, 0.0), 512);
        assert_eq!(frames_at_engine_rate(512, -1.0), 512);
        assert_eq!(frames_at_engine_rate(512, f64::NAN), 512);
    }
}
