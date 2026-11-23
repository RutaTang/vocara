//! The audio analysis library (port of `lib/data/services/audio_dsp.dart`):
//! WAV encode/decode, the STFT spectrogram, the per-frame voice metrics
//! (YIN pitch, LPC formants, ring, tilt, CPP, H1−H2, loudness, input
//! quality), their streaming and live variants, and the tuner's smoother.
//!
//! Everything here is pure and synchronous. Dart ran the whole-take work
//! inside `Isolate.run`; in Rust those are plain functions and types the
//! caller runs on a worker thread (`spawn_blocking`): every type is `Send`.
//!
//! # API, by caller
//!
//! - **Decoding** (spectrogram_service, audio_edit_service, tests):
//!   [`PcmAudio`], [`read_wav_mono`], [`write_wav_mono`].
//! - **Whole-take analysis** (spectrogram_service, media_analysis_service):
//!   [`compute_spectrogram`], [`compute_voice_metrics`],
//!   [`compute_voice_metrics_and_spectrogram`] (one pass for both), and the
//!   windowed streaming forms fed decode chunk by decode chunk,
//!   [`StreamingSpectrogramBuilder`] and [`StreamingTakeAnalyzer`] — pinned
//!   bit-identical to the batch functions. [`stft_frame_count`] sizes them;
//!   [`MAX_ANALYSIS_SAMPLE_RATE`] is the admission ceiling.
//! - **Live analysis** (monitor_service): [`LiveVoiceAnalyzer`] (one
//!   [`LiveFrame`] per hop), [`StreamingStft`] (dB columns only) and
//!   [`TunerTracker`].
//! - **Pitch** (anyone with one frame): [`yin_pitch`].
//!
//! # Numerics
//!
//! Dart's `double` is `f64` here and its `Float32List` is `Vec<f32>`: a value
//! is narrowed exactly where Dart stored it into a Float32 list and widened
//! where Dart read it back, so the curves match the Flutter app's bit for
//! bit. Power-of-two FFTs ≥ 16 (every production size) run Dart's own
//! radix-2 transform, which is fftea's arithmetic operation for operation
//! (the private `fft` module); any other size goes to `rustfft`, as Dart
//! sent it to fftea's general FFT. Both are the unnormalized forward DFT
//! (`X[k] = Σ x[n]·e^{−2πikn/N}`), so magnitudes agree.
//!
//! Dart's named parameters with defaults are option structs with `Default`
//! ([`AnalysisParams`], [`YinParams`], [`TunerOptions`]); Dart's callbacks
//! are `FnMut` closures; a builder's `finish` consumes it.

mod fft;
mod filter;
mod live;
mod lpc;
mod metrics;
mod rolling;
mod spectrogram;
mod wav;
mod yin;

#[cfg(test)]
mod tests;

use std::f64::consts::LN_10;

pub use live::{LiveFrame, LiveVoiceAnalyzer, StreamingStft, TunerOptions, TunerTracker};
pub use metrics::{
    compute_voice_metrics, compute_voice_metrics_and_spectrogram, MetricsAndSpectrogram,
    StreamedTakeAnalysis, StreamingTakeAnalyzer,
};
pub use spectrogram::{compute_spectrogram, StreamingSpectrogramBuilder};
pub use wav::{read_wav_mono, write_wav_mono, PcmAudio};
pub use yin::{yin_pitch, yin_pitch_with, YinEstimate, YinParams};

use vocara_model::util::dart_num::{ceil_to_i64, min_f64};

/// The STFT framing and spectrogram range every analysis takes, with Dart's
/// defaults (its `fftSize = 2048, hop = 512, maxHz = 5000` named
/// parameters). The voice-metrics passes ignore [`AnalysisParams::max_hz`]
/// unless they also build a spectrogram.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalysisParams {
    /// Analysis window length in samples.
    pub fft_size: usize,
    /// Samples between frame starts.
    pub hop: usize,
    /// Top of the spectrogram's frequency range (Hz).
    pub max_hz: f64,
}

impl Default for AnalysisParams {
    fn default() -> Self {
        AnalysisParams {
            fft_size: 2048,
            hop: 512,
            max_hz: 5000.0,
        }
    }
}

/// Highest source sample rate the voice-metrics pass produces valid pitch
/// analysis for — the admission ceiling for file analysis
/// (`SpectrogramService.admitForAnalysis`).
///
/// Derivation: the YIN slice is one FFT frame of raw samples decimated by
/// yinFactor (≤ 2), so it always spans fftSize / sampleRate SECONDS —
/// 2048 samples at the production fftSize. [`yin_pitch`]'s widened fmin
/// search lag is a fixed 1 / (70 Hz · 0.9) ≈ 1/63 s; once the frame is
/// shorter than that (sampleRate > 63 · 2048 ≈ 129 kHz) the lag is capped to
/// the frame and the integration window collapses below 2 samples, so EVERY
/// frame returns unvoiced regardless of content — F0, formants, H1−H2,
/// register, vibrato and the voice profile all silently empty while the
/// (YIN-free) spectrogram renders normally. Rates approaching the bound leave
/// integration windows of a handful of samples, i.e. garbage periodicity.
/// 96 kHz is the highest standard rate with a usable window, and no standard
/// rate exists between it and the dead zone, so the gate refuses anything
/// above it instead of producing a healthy-looking take with empty analyses.
pub const MAX_ANALYSIS_SAMPLE_RATE: i64 = 96000;

/// YIN periodicity (1 − d′ at the chosen lag) below which a frame is treated
/// as unvoiced. Clearly-sung vowels read ≈ 0.8–0.95; room noise / breath read
/// well under this. Gating F0 *and* the formants on periodicity (not just
/// loudness) is what keeps formant dots off non-periodic noise that clears
/// the RMS floor.
const VOICED_CONFIDENCE: f64 = 0.55;

/// Samples taken in between frame emissions by every streaming consumer.
/// Emitting only once a whole chunk was in (the file reader's are 4 MiB: a
/// million samples and more) kept the chunk in the rolling buffers, which
/// then copied what was left of it on every compaction — every 32 frames —
/// so a 10-minute take spent about as long compacting as transforming.
/// Frames read the same samples whenever they are emitted.
const EMIT_BLOCK: usize = 4096;

/// How many STFT frames [samples] samples make — fftea's STFT framing, which
/// every analysis here keeps: frames start every [hop], the trailing partial
/// frame is zero-padded and emitted, and empty input gives none.
pub fn stft_frame_count(samples: usize, fft_size: usize, hop: usize) -> usize {
    if samples == 0 {
        0
    } else if samples <= fft_size {
        1
    } else {
        (samples - fft_size).div_ceil(hop) + 1
    }
}

/// How many FFT bins (DC up) a spectrogram keeps to cover [max_hz].
fn spectrogram_bins(max_hz: f64, fft_size: usize, sample_rate: i64) -> usize {
    let covering = ceil_to_i64(max_hz * fft_size as f64 / sample_rate as f64) + 1;
    // Dart sized a Float32List with this and threw on a negative length (a
    // negative maxHz); no bins at all is the harmless reading.
    (fft_size / 2 + 1).min(covering.max(0) as usize)
}

/// The top of the range a [max_bin]-bin spectrogram actually covers — what
/// both builders record as `Spectrogram::max_hz`. Normally the requested
/// [max_hz] (the last bin sits at or just past it), but below 10 kHz the bins
/// stop at Nyquist first, and a max_hz left above that stretched the image's
/// rows over a taller axis, grid and formant overlay than they hold.
fn covered_max_hz(max_hz: f64, max_bin: usize, sample_rate: i64, fft_size: usize) -> f64 {
    min_f64(
        max_hz,
        ((max_bin as i64 - 1) * sample_rate) as f64 / fft_size as f64,
    )
}

/// A magnitude in dB: `20·log10(mag + 1e-9)`, computed as Dart did (a
/// natural log over ln 10, not `log10`).
fn db20(mag: f64) -> f64 {
    20.0 * ((mag + 1e-9).ln() / LN_10)
}

/// A zero-padded window copy into [out] (the window is its length, reused
/// frame after frame): values from [src] starting at absolute index [start];
/// indices outside `[0, src_len)` read as 0 (the exact edge semantics the
/// batch metrics loop always had).
fn pad_slice_into(out: &mut [f32], src: &[f32], start: i64, src_len: usize) {
    let count = out.len() as i64;
    let src_len = src_len as i64;
    let from = start.clamp(0, src_len);
    let to = (start + count).clamp(0, src_len);
    if to > from {
        let lead = (from - start) as usize;
        let tail = (to - start) as usize;
        out[..lead].fill(0.0);
        out[lead..tail].copy_from_slice(&src[from as usize..to as usize]);
        out[tail..].fill(0.0);
    } else {
        out.fill(0.0);
    }
}

/// [`pad_slice_into`] into a fresh window of [count] samples.
fn pad_slice(src: &[f32], start: i64, count: usize, src_len: usize) -> Vec<f32> {
    let mut out = vec![0.0; count];
    pad_slice_into(&mut out, src, start, src_len);
    out
}

/// Dart's `int.clamp`, which throws an `ArgumentError` when `lower > upper`:
/// the analyzers' derived sizes can only meet that with an FFT too small to
/// hold the cepstral pitch window (a programming error, as in Dart).
fn clamp_int(v: i64, lower: i64, upper: i64) -> i64 {
    assert!(
        lower <= upper,
        "clamp range {lower}..{upper} is empty (analysis window too small)"
    );
    v.clamp(lower, upper)
}
