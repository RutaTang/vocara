//! Port of `client/test/waveform_peaks_test.dart`.
//!
//! just_waveform 0.0.7 viewed a .wave file's pixels at byte 10 instead of
//! 20, so its pixel getters read header fields as the first pixels and every
//! pixel after them late. The service reads the pixels itself
//! (`wave_file_pairs`, for the intermediates a Flutter build left), and a WAV
//! clip's length comes from its header, not its 10 ms pixels.

use std::fs;

use vocara_services::audio_dsp::write_wav_mono;
use vocara_services::waveform_service::{downsample, exact_duration_ms, wave_file_pairs};

/// A .wave file as the package's darwin writer lays it out: a 20-byte header
/// (version, flags, sample rate, samples per pixel, pixel count) and 16-bit
/// (min, max) pairs, in native byte order.
fn wave(peak_per_pixel: &[i16]) -> Vec<u8> {
    let mut b = Vec::with_capacity(20 + peak_per_pixel.len() * 4);
    for h in [1u32, 0, 44100, 441, peak_per_pixel.len() as u32] {
        b.extend_from_slice(&h.to_ne_bytes());
    }
    for &p in peak_per_pixel {
        b.extend_from_slice(&(-p).to_ne_bytes());
        b.extend_from_slice(&p.to_ne_bytes());
    }
    b
}

#[test]
fn peaks_start_at_the_first_pixel_no_header_spike_no_lag() {
    let dir = tempfile::tempdir().unwrap();
    // Three minutes of silence at 100 px/s with one loud pixel at 50.
    let mut pixels = vec![0i16; 18000];
    pixels[50] = 16384;
    let file = dir.path().join("take.wave");
    fs::write(&file, wave(&pixels)).unwrap();

    let peaks = downsample(&wave_file_pairs(&fs::read(&file).unwrap()), pixels.len());

    assert_eq!(peaks.len(), 18000);
    assert!(peaks[..3].iter().all(|&p| p == 0.0), "no header spike");
    assert_eq!(peaks[50], 0.5);
    assert_eq!(peaks[51], 0.0);
    assert_eq!(peaks[52], 0.0, "not drawn late");
}

#[test]
fn a_file_shorter_than_its_header_says_reads_only_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = wave(&[3276i16; 100]);
    // Claim more pixels than the file holds.
    bytes[16..20].copy_from_slice(&500u32.to_ne_bytes());
    let file = dir.path().join("take.wave");
    fs::write(&file, &bytes).unwrap();

    let peaks = downsample(&wave_file_pairs(&fs::read(&file).unwrap()), 1000);

    assert_eq!(peaks.len(), 100);
    assert!(peaks.iter().all(|p| (p - 0.1).abs() < 1e-4), "{peaks:?}");
}

#[test]
fn a_wav_clip_is_as_long_as_its_audio_not_its_10_ms_pixels() {
    // 1234 ms at 48 kHz, which the waveform counts as 123 pixels: 1230 ms.
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("take.wav");
    fs::write(&wav, write_wav_mono(&vec![0.0f64; 59232], 48000)).unwrap();
    let wav = wav.to_string_lossy();
    assert_eq!(exact_duration_ms(&wav, 1230), 1234);
    // A header that disagrees with what decoded keeps the decoded length.
    assert_eq!(exact_duration_ms(&wav, 900), 900);
    // Not a WAV: the waveform's length.
    let mp3 = dir.path().join("song.mp3");
    fs::write(&mp3, [1u8, 2, 3]).unwrap();
    assert_eq!(exact_duration_ms(&mp3.to_string_lossy(), 1230), 1230);
}
