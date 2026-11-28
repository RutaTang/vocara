//! The live-input peak meter: a lock-free hand-off from the input callback to the one reader
//! that polls it (the mixer's MIC meter and the forming take waveform, about every 50 ms).
//!
//! The callback used to STORE each buffer's peak, so a poll saw only the last buffer before it:
//! at 256-frame voice-unit callbacks (5.3 ms) that is one buffer in nine, and most plosives and
//! belted onsets never reached the meter or the live waveform (the singer set the gain from a
//! meter that under-read, and at Stop the waveform jumped to the spikier one drawn from the
//! file). The callback now RAISES a held peak and the reader TAKES it, clearing it, so the next
//! read sees the loudest sample of every buffer since the last one.
//!
//! One reader at a time: each read clears what the other would have seen. The song page's take
//! tick and its monitor meter take turns (the monitor meter yields while a take records).

use std::sync::atomic::{AtomicU32, Ordering};

/// Audio thread: raise the held peak (f32 bits in `held`) to at least `peak`, a linear level.
/// One wait-free atomic max: a non-negative float's bits order as its value does. A negative or
/// NaN `peak` raises nothing (`-0.0`'s sign bit would otherwise read as the loudest of all).
#[inline]
pub fn raise_peak(held: &AtomicU32, peak: f32) {
    if peak > 0.0 {
        held.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
}

/// Reader: the highest peak raised since the previous take (0 when none was), clearing it.
#[inline]
pub fn take_peak(held: &AtomicU32) -> f32 {
    f32::from_bits(held.swap(0, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spike_in_one_callback_reaches_the_next_read() {
        // Ten 5.3 ms callbacks between two 50 ms reads; only the third carries the onset.
        let held = AtomicU32::new(0);
        for cb in 0..10 {
            raise_peak(&held, if cb == 2 { 0.9 } else { 0.1 });
        }
        assert_eq!(take_peak(&held), 0.9, "the read misses the spike");
        // Taken: the next read reports only what came after it.
        assert_eq!(take_peak(&held), 0.0);
        raise_peak(&held, 0.25);
        assert_eq!(take_peak(&held), 0.25);
    }

    #[test]
    fn a_negative_zero_or_nan_peak_raises_nothing() {
        let held = AtomicU32::new(0);
        raise_peak(&held, -0.0);
        raise_peak(&held, f32::NAN);
        assert_eq!(take_peak(&held).to_bits(), 0);
        raise_peak(&held, 0.5);
        raise_peak(&held, -0.0);
        assert_eq!(take_peak(&held), 0.5);
    }
}
