//! The engine's polyphase windowed-sinc kernel (64 taps, 256x oversampled table, Blackman
//! window, cutoff scaled for downsampling), and the input path's push-based rate converter
//! built on it. The output boundary's pull-based converter (the device module's
//! `OutResampler`) reads the same table. Dependency-free, so both build and test without the
//! `device` feature.

/// Kernel width in source frames. 64 taps at 256x table resolution yields ~70 dB stopband
/// with a passband flat well past 20 kHz at 64 MACs per output frame.
pub(crate) const SINC_TAPS: usize = 64;
pub(crate) const SINC_HALF: usize = SINC_TAPS / 2;
pub(crate) const SINC_OVERSAMPLE: usize = 256;

/// Builds the polyphase table for [step] (source frames per output frame): row `r` holds
/// the windowed-sinc weights for sub-phase `r / OVERSAMPLE`, cutoff scaled below the
/// output Nyquist when downsampling (step > 1), each row normalized to unity DC so the
/// converter has exactly 0 dB gain at every phase. Tap `k` of a row weighs the source frame
/// `floor(pos) - (HALF - 1) + k` for a read position `pos`.
pub(crate) fn build_sinc_table(step: f64) -> Vec<f32> {
    let fc = if step > 1.0 { 0.92 / step } else { 0.92 };
    let mut table = vec![0.0f32; (SINC_OVERSAMPLE + 1) * SINC_TAPS];
    for r in 0..=SINC_OVERSAMPLE {
        let frac = r as f64 / SINC_OVERSAMPLE as f64;
        let mut sum = 0.0f64;
        for k in 0..SINC_TAPS {
            // Contribution of content index floor(pos) - (HALF-1) + k at offset x from pos.
            let x = k as f64 - (SINC_HALF as f64 - 1.0) - frac;
            // At x = 0 the scaled sinc's limit is `fc`, not 1: the centre tap of the phase-0
            // row stood fc⁻¹ times too tall, and the row's unity-DC normalization then
            // squashed every other tap under it — that row all but passed its input through
            // unfiltered, and a downsampling device (a 16-32 kHz call-profile output) heard
            // the engine's top octave folded down, 8-14 dB under the signal.
            let sinc = if x.abs() < 1e-12 {
                fc
            } else {
                let px = std::f64::consts::PI * fc * x;
                px.sin() / px * fc
            };
            // Blackman window over the kernel span.
            let w = {
                let t = (x + SINC_HALF as f64) / (SINC_TAPS as f64);
                let t = t.clamp(0.0, 1.0);
                0.42 - 0.5 * (2.0 * std::f64::consts::PI * t).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * t).cos()
            };
            let v = sinc * w;
            table[r * SINC_TAPS + k] = v as f32;
            sum += v;
        }
        // Unity DC per row: without this the gain ripples with the sub-phase.
        if sum.abs() > 1e-12 {
            let inv = (1.0 / sum) as f32;
            for k in 0..SINC_TAPS {
                table[r * SINC_TAPS + k] *= inv;
            }
        }
    }
    table
}

/// The most engine-rate frames one input sample can become in [`InputRateConverter::convert`]:
/// six at the lowest rate a mic negotiates (8 kHz narrowband), one more for a drift ratio past
/// 1, and a spare. The bound is also what keeps the audio thread's loop finite on a rate no
/// device has (a zero rate would never advance).
pub const MAX_FRAMES_PER_INPUT: usize = 8;

/// How far the input converter's output trails its input while it converts a rate, in INPUT
/// frames: the kernel is centred on the read position, so a frame is emitted once the half of
/// the kernel ahead of it has arrived. 2 ms at 16 kHz, 1.33 ms at 24 kHz, 0.73 ms at 44.1 kHz;
/// the two-point interpolator it replaced trailed by under one frame. Nothing at the engine
/// rate passes through the kernel, so a 48 kHz mic carries none of it.
pub const INPUT_SINC_DELAY_FRAMES: usize = SINC_HALF;

/// The input path's push-based rate converter: one mono mic sample in, the engine-rate frames
/// it completes out (the mic is carried as one signal on both lanes, so one channel is enough).
///
/// At the engine rate it passes samples through (the identity fast path, bit for bit), and a
/// clock-drift ratio off 1 (tens of ppm) is followed with the two-point interpolator, which is
/// continuous with that fast path as the drift estimator's deadband switches between them; a
/// trim that small makes no images. Any other rate goes through the windowed-sinc kernel for
/// as long as the stream lasts: two-point interpolation of a 24 kHz mic left the take -5 dB at
/// 10 kHz with images near -20 dB (6 kHz content imaged at 18 kHz), heard as harsh sibilance in
/// the take and the monitor. The kernel costs [`INPUT_SINC_DELAY_FRAMES`] of latency, constant
/// for the stream, and its leading output is the near-silence of a zeroed history.
pub struct InputRateConverter {
    /// Input frames per engine frame at the device's nominal rate (`in_rate / ENGINE_SR`).
    step: f64,
    /// The next output's position past the interpolation's left sample, in input frames.
    pos: f64,
    /// The two-point interpolator's left sample, and whether one has arrived.
    prev: f32,
    primed: bool,
    /// The kernel and its history, for a rate other than the engine's.
    sinc: Option<SincHistory>,
}

/// The windowed-sinc kernel's table and the input history it reads.
struct SincHistory {
    table: Vec<f32>,
    /// The last [`SINC_TAPS`] input samples, each written twice (at `i` and `i + TAPS`), so the
    /// newest `TAPS` are one contiguous slice, oldest first, wherever the write index stands.
    hist: [f32; 2 * SINC_TAPS],
    /// Where the next sample goes; also the start of the newest `TAPS`.
    at: usize,
}

impl SincHistory {
    #[inline]
    fn push(&mut self, x: f32) {
        self.hist[self.at] = x;
        self.hist[self.at + SINC_TAPS] = x;
        self.at = (self.at + 1) % SINC_TAPS;
    }

    /// The kernel at `frac` past the history's sample `HALF` back from the newest: its taps
    /// span exactly the newest `TAPS` samples.
    #[inline]
    fn read(&self, frac: f64) -> f32 {
        let rf = frac * SINC_OVERSAMPLE as f64;
        let r0 = (rf.floor() as usize).min(SINC_OVERSAMPLE - 1);
        let rw = (rf - r0 as f64) as f32;
        let row0 = &self.table[r0 * SINC_TAPS..(r0 + 1) * SINC_TAPS];
        let row1 = &self.table[(r0 + 1) * SINC_TAPS..(r0 + 2) * SINC_TAPS];
        let win = &self.hist[self.at..self.at + SINC_TAPS];
        let mut y = 0.0f32;
        for t in 0..SINC_TAPS {
            y += win[t] * (row0[t] + (row1[t] - row0[t]) * rw);
        }
        y
    }
}

impl InputRateConverter {
    /// A converter for a device delivering `in_rate`, into `engine_rate`. Allocates the
    /// kernel's table (once per input stream, on the host thread) when the rates differ.
    pub fn new(in_rate: f64, engine_rate: f64) -> InputRateConverter {
        let step = in_rate / engine_rate;
        let converts = (step - 1.0).abs() >= 1e-9;
        InputRateConverter {
            step,
            pos: 0.0,
            prev: 0.0,
            primed: false,
            sinc: converts.then(|| SincHistory {
                table: build_sinc_table(step),
                hist: [0.0; 2 * SINC_TAPS],
                at: 0,
            }),
        }
    }

    /// The nominal step (input frames per engine frame).
    pub fn step(&self) -> f64 {
        self.step
    }

    /// Takes one input sample and writes the engine-rate frames it completes to `out`,
    /// returning how many. `ratio` is the drift estimator's correction (exactly 1.0 inside its
    /// deadband). RT-safe: no allocation, at most [`MAX_FRAMES_PER_INPUT`] frames.
    #[inline]
    pub fn convert(&mut self, x: f32, ratio: f64, out: &mut [f32; MAX_FRAMES_PER_INPUT]) -> usize {
        let step = self.step * ratio;
        if let Some(sinc) = self.sinc.as_mut() {
            sinc.push(x);
            let mut n = 0;
            while self.pos < 1.0 && n < MAX_FRAMES_PER_INPUT {
                out[n] = sinc.read(self.pos.max(0.0));
                n += 1;
                self.pos += step;
            }
            self.pos -= 1.0;
            return n;
        }
        if (step - 1.0).abs() < 1e-9 {
            // Keep the interpolator's state coherent while the fast path owns the stream. The
            // drift estimator publishes EXACTLY 1.0 inside its ±2 ppm deadband, so a chain
            // hovering near that edge crosses back and forth; leaving `prev` behind meant the
            // first interpolation after a crossing blended the current sample with one from an
            // arbitrarily older callback — a single-sample step out of nowhere, i.e. a click.
            //
            // The phase left behind is 1.0, not 0.0: positions in the interpolating branch are
            // measured from `prev`, this sample has just been emitted in full, and the next
            // output is due one whole input later — exactly `prev + 1`. Leaving 0.0 would
            // re-emit the sample identity just sent (a duplicate).
            self.prev = x;
            self.primed = true;
            self.pos = 1.0;
            out[0] = x;
            return 1;
        }
        if !self.primed {
            self.prev = x;
            self.primed = true;
            return 0;
        }
        let mut n = 0;
        while self.pos < 1.0 && n < MAX_FRAMES_PER_INPUT {
            let t = self.pos as f32;
            out[n] = self.prev + (x - self.prev) * t;
            n += 1;
            self.pos += step;
        }
        self.pos -= 1.0;
        self.prev = x;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::ENGINE_SR;

    /// Every engine-rate frame `samples` (at `in_rate`) becomes.
    fn convert_all(in_rate: f64, ratio: f64, samples: &[f32]) -> Vec<f32> {
        let mut c = InputRateConverter::new(in_rate, ENGINE_SR);
        let mut out = Vec::new();
        let mut buf = [0.0f32; MAX_FRAMES_PER_INPUT];
        for &x in samples {
            let n = c.convert(x, ratio, &mut buf);
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    fn tone(rate: f64, hz: f64, amp: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (std::f64::consts::TAU * hz * i as f64 / rate).sin()) as f32)
            .collect()
    }

    /// The amplitude of `hz` in `x` (at `rate`), Hann-windowed so leakage from the other
    /// component cannot pose as it.
    fn amplitude(x: &[f32], rate: f64, hz: f64) -> f64 {
        let n = x.len();
        let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            let ph = std::f64::consts::TAU * hz * i as f64 / rate;
            re += v as f64 * w * ph.cos();
            im += v as f64 * w * ph.sin();
            wsum += w;
        }
        2.0 * (re * re + im * im).sqrt() / wsum
    }

    #[test]
    fn a_24_khz_mic_reaches_the_take_without_images_or_droop() {
        // A 6 kHz tone from a 24 kHz mic element. Two-point interpolation left its image at
        // 18 kHz about 19 dB under the tone and the tone itself 1.8 dB down.
        let input = tone(24_000.0, 6_000.0, 0.5, 24_000);
        let out = convert_all(24_000.0, 1.0, &input);
        assert_eq!(out.len(), 48_000, "two engine frames per input frame");
        let window = &out[4_800..4_800 + 9_600];
        let tone_amp = amplitude(window, ENGINE_SR, 6_000.0);
        let image = amplitude(window, ENGINE_SR, 18_000.0);
        let image_db = 20.0 * (image / tone_amp).log10();
        assert!(image_db < -60.0, "the 18 kHz image is at {image_db:.1} dB");
        let gain_db = 20.0 * (tone_amp / 0.5).log10();
        assert!(
            gain_db.abs() < 0.1,
            "the tone comes through at {gain_db:+.2} dB"
        );
    }

    #[test]
    fn a_16_khz_mic_keeps_its_band_clean_under_a_drift_trim() {
        // A narrowband headset (16 kHz) with the drift estimator trimming 30 ppm: the kernel
        // still runs, at the trimmed step.
        let input = tone(16_000.0, 3_000.0, 0.5, 16_000);
        let out = convert_all(16_000.0, 1.0 + 30e-6, &input);
        let n = out.len() as i64;
        assert!((n - 48_000).abs() <= 3, "{n} engine frames for one second");
        let window = &out[4_800..4_800 + 9_600];
        let tone_amp = amplitude(window, ENGINE_SR, 3_000.0);
        for image_hz in [13_000.0, 19_000.0] {
            let db = 20.0 * (amplitude(window, ENGINE_SR, image_hz) / tone_amp).log10();
            assert!(db < -60.0, "the {image_hz} Hz image is at {db:.1} dB");
        }
    }

    #[test]
    fn the_kernel_delays_a_converted_stream_by_its_documented_latency() {
        // An impulse at input frame 100 of a 24 kHz stream peaks at engine frame 2 * (100 +
        // the delay): the kernel is centred, so its peak lands exactly on the delayed frame.
        let mut input = vec![0.0f32; 400];
        input[100] = 1.0;
        let out = convert_all(24_000.0, 1.0, &input);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak, 2 * (100 + INPUT_SINC_DELAY_FRAMES));
    }

    #[test]
    fn the_engine_rate_passes_through_bit_for_bit_and_a_drift_trim_interpolates() {
        let input = tone(48_000.0, 1_000.0, 0.5, 4_800);
        assert_eq!(convert_all(48_000.0, 1.0, &input), input);
        // A trim is the two-point interpolator, continuous across the deadband crossing.
        let mut c = InputRateConverter::new(48_000.0, ENGINE_SR);
        let mut buf = [0.0f32; MAX_FRAMES_PER_INPUT];
        assert_eq!(c.convert(0.5, 1.0, &mut buf), 1);
        assert_eq!(
            c.convert(-1.0, 1.0 + 1e-3, &mut buf),
            0,
            "phase 1.0: the next is due"
        );
        let n = c.convert(-1.0, 1.0 + 1e-3, &mut buf);
        assert_eq!((n, buf[0]), (1, -1.0));
    }

    #[test]
    fn no_rate_runs_the_audio_thread_past_its_bound() {
        let mut c = InputRateConverter::new(0.0, ENGINE_SR);
        let mut buf = [0.0f32; MAX_FRAMES_PER_INPUT];
        for _ in 0..4 {
            assert!(c.convert(0.1, 1.0, &mut buf) <= MAX_FRAMES_PER_INPUT);
        }
    }
}
