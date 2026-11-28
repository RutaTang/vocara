//! The record round trip a take is aligned by: how far behind the playhead the singer's voice
//! lands in the take. Each leg of the mic unit's IO (the output the singer hears, the input that
//! records them) adds its device's IO buffer, safety offset and hardware latency and its
//! stream's latency (a USB interface reports its converters' delay there); the unit adds its own
//! processing latency, the echo canceller one block, the speaker-to-mic flight a little, and a
//! mic element at another rate than the engine's the input converter's kernel.
//!
//! The figures are read once before the voice unit starts (an estimate, which also sizes the
//! echo reference's ring) and again once its input has delivered: before that a Bluetooth
//! headset is still in its playback profile, at another rate and latency than the call profile
//! it records in, and the IO buffer read back may not be the one the unit runs at. Platform-
//! free arithmetic, so it is tested without hardware; the HAL reads are the voice unit's.

use crate::sample::{frames_at_engine_rate, ENGINE_SR};
use crate::sinc::INPUT_SINC_DELAY_FRAMES;

/// One leg's figures as the HAL reports them: frames at the device's nominal `rate`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Leg {
    pub buffer: u32,
    pub safety: u32,
    pub device: u32,
    pub stream: u32,
    pub rate: f64,
}

/// What else the round trip holds besides the legs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extras {
    /// Speaker-to-mic flight, in engine frames.
    pub acoustic: u32,
    /// The echo canceller's block, in engine frames (0 with the canceller bypassed).
    pub aec_block: u32,
    /// The voice unit's own latency (`kAudioUnitProperty_Latency`), in seconds.
    pub unit_seconds: f64,
    /// The rate the mic element delivers at: another than the engine's passes through the input
    /// converter's kernel (see [`INPUT_SINC_DELAY_FRAMES`]).
    pub input_rate: f64,
}

/// The record round trip in engine frames: each leg scaled from its device's rate, then the
/// extras.
pub fn round_trip_frames(legs: &[Leg], extras: Extras) -> u32 {
    let mut frames = extras.acoustic + extras.aec_block;
    for leg in legs {
        let sum = leg
            .buffer
            .saturating_add(leg.safety)
            .saturating_add(leg.device)
            .saturating_add(leg.stream);
        frames = frames.saturating_add(frames_at_engine_rate(sum, leg.rate));
    }
    if extras.unit_seconds.is_finite() && extras.unit_seconds > 0.0 {
        frames = frames.saturating_add((extras.unit_seconds * ENGINE_SR).round() as u32);
    }
    let rate = extras.input_rate;
    if rate.is_finite() && rate > 0.0 && (rate - ENGINE_SR).abs() >= 1.0 {
        frames = frames.saturating_add(frames_at_engine_rate(INPUT_SINC_DELAY_FRAMES as u32, rate));
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_term_of_the_round_trip_counts_at_the_engine_rate() {
        // A USB interface at 44.1 kHz: 256-frame buffers, its converters' delay as stream
        // latency, and the voice unit's own latency.
        let leg = Leg {
            buffer: 256,
            safety: 24,
            device: 10,
            stream: 151,
            rate: 44_100.0,
        };
        let extras = Extras {
            acoustic: 64,
            aec_block: 256,
            unit_seconds: 0.002,
            input_rate: 48_000.0,
        };
        let got = round_trip_frames(&[leg, leg], extras);
        let per_leg = ((256.0 + 24.0 + 10.0 + 151.0) * 48_000.0 / 44_100.0_f64).round() as u32;
        assert_eq!(got, 64 + 256 + 2 * per_leg + 96);
        // Without the stream latency each leg read 151 frames short: 7 ms on the round trip.
        let short = Leg { stream: 0, ..leg };
        assert_eq!(got - round_trip_frames(&[short, short], extras), 2 * 164);
    }

    #[test]
    fn a_headset_in_its_call_profile_counts_its_own_rate_and_the_input_kernel() {
        // A Bluetooth headset recording at 16 kHz in its call profile: its legs scale from 16
        // kHz, and the mic's frames pass through the input converter's kernel.
        let leg = Leg {
            buffer: 128,
            safety: 0,
            device: 400,
            stream: 0,
            rate: 16_000.0,
        };
        let extras = Extras {
            input_rate: 16_000.0,
            ..Extras::default()
        };
        let got = round_trip_frames(&[leg, leg], extras);
        assert_eq!(got, 2 * 528 * 3 + INPUT_SINC_DELAY_FRAMES as u32 * 3);
        // The same figures read as if at the engine rate (the playback profile's, before the
        // unit started) were a third of it.
        let early = Leg {
            rate: 48_000.0,
            ..leg
        };
        assert!(round_trip_frames(&[early, early], Extras::default()) * 3 <= got);
    }

    #[test]
    fn unreadable_figures_add_nothing() {
        let leg = Leg {
            rate: f64::NAN,
            ..Leg::default()
        };
        let extras = Extras {
            unit_seconds: f64::NAN,
            input_rate: 0.0,
            ..Extras::default()
        };
        assert_eq!(round_trip_frames(&[leg], extras), 0);
    }
}
