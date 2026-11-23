//! The analysis FFT (spectrogram_test.dart: "the analysis FFT is fftea's
//! realFft bit for bit"), plus the convention check against `rustfft`.

use rustfft::num_complex::Complex;

use super::super::fft::{hanning, Radix2RealFft, RealFft};
use super::support::{fftea_hanning, fftea_real_fft, DartRandom};

/// Signed zeros (a Hann window's ends, digital silence), denormal-scale and
/// full-scale values, and an all-zero frame.
fn trial_frame(rng: &mut DartRandom, n: usize, t: usize) -> Vec<f64> {
    (0..n)
        .map(|_| {
            if t == 0 {
                0.0
            } else {
                match rng.next_int(12) {
                    0 => 0.0,
                    1 => -0.0,
                    2 => 1e-310 * (rng.next_double() - 0.5),
                    _ => rng.next_double() * 2.0 - 1.0,
                }
            }
        })
        .collect()
}

#[test]
fn the_analysis_fft_is_ffteas_real_fft_bit_for_bit() {
    let mut rng = DartRandom::new(17);
    for n in [16, 32, 64, 256, 512, 1024, 2048, 4096, 8192] {
        let mut fft = Radix2RealFft::new(n);
        for t in 0..30 {
            let x = trial_frame(&mut rng, n, t);
            fft.transform(&x);
            let got = fft.spectrum();
            let want = fftea_real_fft(&x);
            for (k, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_eq!(
                    (g.0.to_bits(), g.1.to_bits()),
                    (w.0.to_bits(), w.1.to_bits()),
                    "n={n} trial {t} bin {k}"
                );
            }
        }
    }
}

#[test]
fn the_analysis_fft_is_the_unnormalized_forward_dft_rustfft_computes() {
    // fftea and rustfft share the convention X[k] = Σ x[n]·e^{−2πikn/N}, no
    // scaling: the radix-2 path and the general path agree to rounding.
    let mut rng = DartRandom::new(3);
    for n in [16, 64, 2048] {
        let x: Vec<f64> = (0..n).map(|_| rng.next_double() * 2.0 - 1.0).collect();
        let mut fft = Radix2RealFft::new(n);
        fft.transform(&x);
        let mut buf: Vec<Complex<f64>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(n)
            .process(&mut buf);
        for (k, (g, w)) in fft.spectrum().iter().zip(&buf).enumerate() {
            assert!(
                (g.0 - w.re).abs() < 1e-9 && (g.1 - w.im).abs() < 1e-9,
                "n={n} bin {k}: {g:?} vs {w}"
            );
        }
    }
}

#[test]
fn magnitudes_and_real_parts_read_the_transform() {
    // A non-power-of-two size takes the general path; both paths report
    // |X[k]| and Re X[k] of the same spectrum.
    for n in [12, 1000, 1024] {
        let x: Vec<f64> = (0..n).map(|i| ((i * 7 % 13) as f64 - 6.0) / 6.0).collect();
        let mut fft = RealFft::new(n);
        fft.transform(&x);
        let mut mags = vec![0.0; n / 2 + 1];
        fft.magnitudes_into(&mut mags, n / 2 + 1);
        let mut re = vec![0.0; 5];
        fft.real_parts_into(&mut re, 3, 8);
        let mut buf: Vec<Complex<f64>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(n)
            .process(&mut buf);
        for k in 0..=n / 2 {
            assert!((mags[k] - buf[k].norm()).abs() < 1e-9, "n={n} |X[{k}]|");
        }
        for k in 3..8 {
            assert!((re[k - 3] - buf[k].re).abs() < 1e-9, "n={n} Re X[{k}]");
        }
    }
}

#[test]
fn the_hann_window_is_ffteas() {
    for n in [1, 2, 3, 16, 1000, 2048] {
        let got = hanning(n);
        let want = fftea_hanning(n);
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.to_bits(), w.to_bits(), "n={n} w[{i}]");
        }
    }
}
