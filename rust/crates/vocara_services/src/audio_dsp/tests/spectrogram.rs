//! The spectrogram builders: spectrogram_test.dart and
//! spectrogram_assembly_test.dart.

use std::f64::consts::PI;

use vocara_model::models::spectrogram::Spectrogram;

use super::super::{
    compute_spectrogram, compute_voice_metrics, compute_voice_metrics_and_spectrogram,
    stft_frame_count, AnalysisParams, LiveVoiceAnalyzer, StreamingSpectrogramBuilder,
    StreamingStft, StreamingTakeAnalyzer,
};
use super::support::{assert_bits_eq, fftea_stft_columns, DartRandom};

fn params(fft_size: usize, hop: usize, max_hz: f64) -> AnalysisParams {
    AnalysisParams {
        fft_size,
        hop,
        max_hz,
    }
}

fn with_max_hz(max_hz: f64) -> AnalysisParams {
    AnalysisParams {
        max_hz,
        ..AnalysisParams::default()
    }
}

/// [samples] fed to a fresh builder [step] samples at a time.
fn streamed(samples: &[f32], sr: i64, p: AnalysisParams, hint: usize, step: usize) -> Spectrogram {
    let mut b = StreamingSpectrogramBuilder::new(sr, p, hint);
    for chunk in samples.chunks(step) {
        b.add(chunk);
    }
    b.finish()
}

/// 0.4·sin(220 Hz) plus a little of Dart's `Random` noise.
fn tone_with_noise(n: usize, sr: i64, rng: &mut DartRandom) -> Vec<f32> {
    (0..n)
        .map(|i| {
            (0.4 * (2.0 * PI * 220.0 * i as f64 / sr as f64).sin()
                + 0.05 * (rng.next_double() - 0.5)) as f32
        })
        .collect()
}

#[test]
fn streaming_builder_equals_compute_spectrogram_exactly_over_awkward_chunk_boundaries() {
    // The builder must be COLUMN-EXACT with the batch path (the same frames
    // and column math) — including the trailing zero-padded partial frame
    // and the empty-input case.
    let n = 10000; // not a multiple of fftSize or hop
    let samples: Vec<f32> = (0..n)
        .map(|i| ((i as f64 / 9.0).sin() * 0.4 + (i as f64 / 51.0).sin() * 0.2) as f32)
        .collect();
    let p = AnalysisParams::default();
    let batch = compute_spectrogram(&samples, 44100, p);

    let mut uniform = vec![500; n / 500];
    uniform.push(n % 500);
    for chunks in [vec![n], vec![1, 999, 2048, n - 1 - 999 - 2048], uniform] {
        let mut b = StreamingSpectrogramBuilder::new(44100, p, 0);
        let mut at = 0;
        for c in chunks {
            if c == 0 {
                continue;
            }
            b.add(&samples[at..at + c]);
            at += c;
        }
        assert_eq!(at, n, "fixture sanity");
        let s = b.finish();
        assert_eq!(s.frames, batch.frames);
        assert_eq!(s.bins, batch.bins);
        assert_bits_eq(&s.db, &batch.db, "columns must be bit-identical");
        assert_eq!(s.min_db, batch.min_db);
        assert_eq!(s.max_db, batch.max_db);
    }

    // Empty input: zero frames on both paths, never a lone padded frame.
    assert_eq!(
        StreamingSpectrogramBuilder::new(44100, p, 0)
            .finish()
            .frames,
        0
    );
    assert_eq!(compute_spectrogram(&[], 44100, p).frames, 0);
}

#[test]
fn the_spectrogram_builders_are_ffteas_stft_bit_for_bit() {
    // What computeSpectrogram was: fftea's STFT over the samples, the first
    // maxBin magnitudes of each frame in dB, no frame for empty input.
    let mut rng = DartRandom::new(23);
    for (sr, fft_size, hop) in [
        (8000, 2048, 512),
        (22050, 2048, 512),
        (44100, 2048, 512),
        (48000, 2048, 512),
        (48000, 1024, 256),
        (44100, 1000, 300), // the general FFT, not radix-2
    ] {
        for n in [0, 1, 300, fft_size, fft_size + 1, 9001, sr as usize] {
            let samples = tone_with_noise(n, sr, &mut rng);
            for max_hz in [5000.0, 18000.0] {
                let (want, _, _) = fftea_stft_columns(&samples, sr, max_hz, fft_size, hop);
                let why = format!("sr={sr} fft={fft_size} n={n} maxHz={max_hz}");
                let p = params(fft_size, hop, max_hz);
                let batch = compute_spectrogram(&samples, sr, p);
                assert_bits_eq(&batch.db, &want, &why);
                let s = streamed(&samples, sr, p, 0, 777);
                assert_bits_eq(&s.db, &want, &format!("{why} (streamed)"));
            }
        }
    }
}

#[test]
fn the_live_analyzers_columns_are_ffteas_transform_bit_for_bit() {
    // What StreamingStft and LiveVoiceAnalyzer computed: every complete
    // window at a multiple of hop (no padded tail), Hann-windowed through
    // the transform, the first maxBin magnitudes in dB.
    let mut rng = DartRandom::new(29);
    for (sr, fft_size, hop) in [
        (16000, 2048, 512),
        (44100, 2048, 512),
        (48000, 2048, 512),
        (48000, 1024, 256),
        (44100, 1000, 300), // the general FFT, not radix-2
    ] {
        let n = sr as usize / 2 + 123;
        let samples = tone_with_noise(n, sr, &mut rng);
        for max_hz in [5000.0, 18000.0] {
            // The complete windows only: fftea's columns of the samples cut
            // at the last complete window's end.
            let complete = if n < fft_size {
                0
            } else {
                (n - fft_size) / hop * hop + fft_size
            };
            let (want, frames, bins) =
                fftea_stft_columns(&samples[..complete], sr, max_hz, fft_size, hop);
            let why = format!("sr={sr} fft={fft_size} maxHz={max_hz}");
            let p = params(fft_size, hop, max_hz);
            let mut stft = StreamingStft::new(sr, p);
            let mut live = LiveVoiceAnalyzer::new(sr, p);
            let (mut stft_cols, mut live_cols) = (Vec::new(), Vec::new());
            for chunk in samples.chunks(777) {
                stft.add_samples(chunk, |c| stft_cols.push(c));
                live.add_samples(chunk, |f| live_cols.push(f.column));
            }
            assert_eq!(stft_cols.len(), frames, "{why}");
            assert_eq!(live_cols.len(), frames, "{why}");
            for c in 0..frames {
                let w = &want[c * bins..(c + 1) * bins];
                assert_bits_eq(&stft_cols[c], w, &format!("{why} col {c}"));
                assert_bits_eq(&live_cols[c], w, &format!("{why} col {c} (live)"));
            }
        }
    }
}

#[test]
fn compute_spectrogram_concentrates_energy_at_the_input_tone_frequencies() {
    let sr = 44100;
    let n = sr as usize; // 1 second
    let tones = [500.0, 1500.0, 3000.0];
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let mut v = 0.0;
            for f in tones {
                v += (2.0 * PI * f * i as f64 / sr as f64).sin();
            }
            (v / tones.len() as f64 * 0.8) as f32
        })
        .collect();
    let spec = compute_spectrogram(&samples, sr, params(2048, 512, 5000.0));
    assert!(spec.frames > 10);
    assert!(spec.bins > 200);

    // Average dB per bin across all frames.
    let mut avg = vec![0.0f64; spec.bins];
    for f in 0..spec.frames {
        for (b, a) in avg.iter_mut().enumerate() {
            *a += f64::from(spec.db[f * spec.bins + b]);
        }
    }
    for a in &mut avg {
        *a /= spec.frames as f64;
    }
    let bin_of = |hz: f64| (hz / spec.bin_hz()).round() as usize;
    let control = avg[bin_of(4500.0)]; // a quiet region with no tone
    for f in tones {
        assert!(
            avg[bin_of(f)] > control + 20.0,
            "tone {f} Hz should dominate the ~4500 Hz control bin"
        );
    }
}

#[test]
fn compute_voice_metrics_and_spectrogram_equals_the_two_separate_passes_bit_for_bit() {
    // Its spectrogram is the metrics pass's own FFT.
    let mut rng = DartRandom::new(5);
    let p = AnalysisParams::default();
    for sr in [8000, 16000, 22050, 44100, 48000] {
        // Sub-frame, exact-frame, hop-ragged and longer lengths: the trailing
        // zero-padded frame and the empty input included.
        for n in [0, 300, 2048, 2049, 9001, sr as usize] {
            let samples = tone_with_noise(n, sr, &mut rng);
            let both = compute_voice_metrics_and_spectrogram(&samples, sr, p);
            let spec = compute_spectrogram(&samples, sr, p);
            let metrics = compute_voice_metrics(&samples, sr, p);
            let s = &both.spectrogram;
            let why = format!("sr={sr} n={n}");
            assert_eq!(s.frames, spec.frames, "{why}");
            assert_eq!(s.bins, spec.bins, "{why}");
            assert_eq!(s.max_hz, spec.max_hz, "{why}");
            assert_bits_eq(&s.db, &spec.db, &why);
            assert_eq!(s.min_db, spec.min_db, "{why}");
            assert_eq!(s.max_db, spec.max_db, "{why}");
            assert_eq!(both.metrics.frames, metrics.frames, "{why}");
            assert_bits_eq(&both.metrics.f0, &metrics.f0, &why);
            assert_bits_eq(&both.metrics.cpp, &metrics.cpp, &why);
            assert_bits_eq(&both.metrics.f1, &metrics.f1, &why);
            assert_eq!(both.metrics.active, metrics.active, "{why}");
        }
    }
}

#[test]
fn a_low_rate_source_caps_max_hz_at_the_highest_bin_both_builders_emit() {
    // At 8 kHz the bins stop at Nyquist (4 kHz); a maxHz left at 5 kHz
    // stretched the 0–4 kHz image over a 0–5 kHz axis, grid and formant
    // overlay. Rates whose bins reach 5 kHz keep it unchanged.
    for sr in [8000i64, 11025, 44100, 48000] {
        let samples: Vec<f32> = (0..sr as usize / 4)
            .map(|i| ((2.0 * PI * 1000.0 * i as f64 / sr as f64).sin() * 0.5) as f32)
            .collect();
        let p = AnalysisParams::default();
        let batch = compute_spectrogram(&samples, sr, p);
        let mut b = StreamingSpectrogramBuilder::new(sr, p, 0);
        b.add(&samples);
        let s = b.finish();
        let top = batch.freq_of_bin(batch.bins - 1);
        let want = if sr < 10000 { top } else { 5000.0 };
        assert_eq!(batch.max_hz, want, "{sr} Hz batch");
        assert_eq!(s.max_hz, want, "{sr} Hz streamed");
    }
}

#[test]
fn streaming_stft_emits_columns_with_energy_at_the_tone_frequency() {
    let sr = 44100;
    let mut stft = StreamingStft::new(sr, params(2048, 512, 5000.0));
    let mut cols = Vec::new();
    let tone = 2000.0;
    let mut phase = 0.0f64;
    for _ in 0..11 {
        let s: Vec<f32> = (0..4096)
            .map(|_| {
                let v = phase.sin() as f32;
                phase += 2.0 * PI * tone / sr as f64;
                v
            })
            .collect();
        stft.add_samples(&s, |c| cols.push(c));
    }
    assert!(cols.len() > 10);
    let bin_of = |hz: f64| (hz / stft.bin_hz()).round() as usize;
    let col = cols.last().unwrap();
    assert!(col[bin_of(tone)] > col[bin_of(4500.0)] + 20.0);
}

/// spectrogram_assembly_test's `_signal`.
fn assembly_signal(n: usize, sr: i64) -> Vec<f32> {
    let mut rnd = DartRandom::new(3);
    (0..n)
        .map(|i| {
            let t = i as f64;
            (0.4 * (2.0 * PI * 220.0 * t / sr as f64).sin()
                + 0.2 * (2.0 * PI * 3100.0 * t / sr as f64).sin()
                + (rnd.next_double() - 0.5) * 0.05) as f32
        })
        .collect()
}

/// The old assembly's scan of the dB range over the stored values.
fn old_range(db: &[f32], frames: usize) -> (f64, f64) {
    if frames == 0 {
        return (-90.0, 0.0);
    }
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in db {
        let v = f64::from(v);
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    (lo, hi)
}

fn expect_same(s: &Spectrogram, want: &[f32], frames: usize, bins: usize, what: &str) {
    let (min_db, max_db) = old_range(want, frames);
    assert_eq!(s.frames, frames, "{what} frames");
    assert_eq!(s.bins, bins, "{what} bins");
    assert_eq!(s.db.len(), frames * bins, "{what} db length");
    assert_bits_eq(&s.db, want, &format!("{what} db must be bit-identical"));
    assert_eq!(s.min_db, min_db, "{what} minDb");
    assert_eq!(s.max_db, max_db, "{what} maxDb");
}

#[test]
fn every_spectrogram_builder_still_assembles_exactly_the_old_columns() {
    // spectrogram_assembly_test: one column per STFT frame, then a flat copy
    // with the dB range scanned over the stored values, kept as the oracle.
    for (sr, n, max_hz) in [
        (48000, 0, 5000.0),
        (48000, 1000, 5000.0), // shorter than one window
        (48000, 2048, 5000.0),
        (48000, 30011, 5000.0), // not a multiple of the hop
        (44100, 22050, 5000.0),
        (48000, 30011, 18000.0), // the EQ display's wide range
        (8000, 9001, 5000.0),    // bins stop at Nyquist first
    ] {
        let samples = assembly_signal(n, sr);
        let (want, frames, bins) = fftea_stft_columns(&samples, sr, max_hz, 2048, 512);
        let what = format!("sr={sr} n={n} maxHz={max_hz}");
        let p = with_max_hz(max_hz);

        expect_same(
            &compute_spectrogram(&samples, sr, p),
            &want,
            frames,
            bins,
            &format!("{what} batch"),
        );

        // Sized from the frame count (what computeFromFile passes), and also
        // told too few frames (it regrows) or too many (a file cut short on
        // disk): the image is the same either way, and exactly sized.
        let exact = stft_frame_count(n, 2048, 512);
        let mut hints = vec![0, exact, exact / 2, exact + 7];
        hints.dedup();
        for hint in hints {
            let s = streamed(&samples, sr, p, hint, 4097);
            expect_same(
                &s,
                &want,
                frames,
                bins,
                &format!("{what} streamed (hint {hint})"),
            );
            assert_eq!(
                s.db.capacity(),
                s.db.len(),
                "{what} exactly sized (hint {hint})"
            );
        }

        let both = compute_voice_metrics_and_spectrogram(&samples, sr, p);
        expect_same(
            &both.spectrogram,
            &want,
            frames,
            bins,
            &format!("{what} metrics pass"),
        );

        let expected = stft_frame_count(n, 2048, 512);
        assert_eq!(expected, frames, "{what} frame count");
        let mut take = StreamingTakeAnalyzer::new(sr, p, expected, true);
        for chunk in samples.chunks(3001) {
            take.add(chunk, None);
        }
        let s = take.finish().spectrogram.expect("asked for a spectrogram");
        expect_same(&s, &want, frames, bins, &format!("{what} streamed take"));
    }
}
