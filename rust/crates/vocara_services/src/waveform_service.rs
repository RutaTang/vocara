//! Renderable waveforms (port of `lib/data/services/waveform_service.dart`).
//!
//! The Dart service extracted per-pixel (min, max) pairs at 100 pixels a
//! second with `just_waveform` (AVFoundation on macOS) and down-sampled them
//! to a compact, normalized peak list stored on the track and drawn by the
//! waveform painter. Here the pixel pairs are computed from mono PCM the
//! engine decodes ([`analyze`]; [`analyze_pcm`] takes samples decoded
//! elsewhere), quantized to 16 bits as the `.wave` file held them, and
//! down-sampled by the same rule.

use std::fs;
use std::path::Path;

use crate::audio_file_service::probe_wav;
use crate::engine::media::{self, MediaError, MAX_PLAYBACK_DURATION, MAX_PLAYBACK_SRC_FRAMES};
use crate::engine::{Pcm, ENGINE_RATE};

/// Result of analyzing an audio file: normalized peaks plus its duration.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformResult {
    pub peaks: Vec<f64>,
    pub duration_ms: i64,
}

/// The waveform's pixel rate (`WaveformZoom.pixelsPerSecond(100)`).
pub const PIXELS_PER_SECOND: u32 = 100;

/// Peaks [`analyze`] callers ask for unless they say otherwise (Dart's
/// `targetBuckets` default).
pub const DEFAULT_TARGET_BUCKETS: usize = 1000;

/// The rate the engine decodes a waveform's audio at: its own graph rate,
/// which every take and every transcoded import is already written at, so
/// their decode is never resampled and the pixels hold the file's own sample
/// peaks, as `just_waveform` read them at the file's rate.
pub const DECODE_RATE: u32 = ENGINE_RATE;

/// The message for audio that gave no waveform: undecodable, empty, or too
/// short for a single pixel.
const UNREADABLE: &str =
    "Couldn't read this audio — it may be corrupt, too short, or an unsupported format.";

/// The app-support folder the Dart service wrote its throwaway `.wave`
/// intermediates to; [`sweep_scratch`] still clears what a Flutter build
/// left there.
pub const SCRATCH_DIR_NAME: &str = "waveform_cache";

/// Why an analysis failed (the message the UI toasts).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WaveformError(pub String);

/// Deletes the `.wave` intermediates left under [root]'s
/// [`SCRATCH_DIR_NAME`] by analyses that never reached their own cleanup
/// (the app quit or was killed mid-analysis). They are derived scratch, never
/// the only copy of anything, but nothing else ever looked in the folder
/// again. Boot-only, before any screen can start an analysis. Best-effort;
/// never fails. Returns how many it removed.
pub fn sweep_scratch(root: &Path) -> usize {
    let dir = root.join(SCRATCH_DIR_NAME);
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_file() || path.extension().and_then(|x| x.to_str()) != Some("wave") {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The per-pixel (min, max) pairs of [samples] at [`PIXELS_PER_SECOND`],
/// quantized to 16 bits as the `.wave` file stored them: pixel `p` covers
/// samples `[p * spp, (p + 1) * spp)` with `spp = sample_rate / 100`, and a
/// trailing partial pixel is kept.
pub fn pixel_pairs(samples: &[f32], sample_rate: u32) -> Vec<i16> {
    let spp = (sample_rate / PIXELS_PER_SECOND).max(1) as usize;
    let mut pairs = Vec::with_capacity(samples.len() / spp * 2 + 2);
    for chunk in samples.chunks(spp) {
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &s in chunk {
            lo = lo.min(s);
            hi = hi.max(s);
        }
        pairs.push(quantize(lo));
        pairs.push(quantize(hi));
    }
    pairs
}

fn quantize(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Collapses the per-pixel (min, max) [pairs] into [target] normalized
/// peaks, taking the largest absolute amplitude in each bucket.
pub fn downsample(pairs: &[i16], target: usize) -> Vec<f64> {
    let pixels = pairs.len() / 2;
    if pixels == 0 {
        return Vec::new();
    }
    let buckets = target.min(pixels);
    let mut peaks = vec![0.0; buckets];
    for (b, peak_out) in peaks.iter_mut().enumerate() {
        let start = (b as f64 * pixels as f64 / buckets as f64).floor() as usize;
        let end = (((b + 1) as f64 * pixels as f64 / buckets as f64).floor() as usize)
            .clamp(start + 1, pixels);
        let mut peak = 0.0_f64;
        for i in start..end {
            let min = i32::from(pairs[2 * i]).abs();
            let max = i32::from(pairs[2 * i + 1]).abs();
            let amplitude = f64::from(min.max(max)) / 32768.0;
            if amplitude > peak {
                peak = amplitude;
            }
        }
        *peak_out = peak.min(1.0);
    }
    peaks
}

/// Reads the (min, max) pairs of a `.wave` file's bytes where the file holds
/// them: after its 20-byte header (version, flags, sample rate, samples per
/// pixel, pixel count). `just_waveform` 0.0.7 viewed them at byte 10, so its
/// getters read the header's last ten bytes as the first pixels and every
/// pixel after them two and a half pixels late: each take began with a spike
/// and was drawn 25 ms late. Native byte order, as the darwin writer wrote.
pub fn wave_file_pairs(bytes: &[u8]) -> Vec<i16> {
    const HEADER: usize = 20;
    if bytes.len() < HEADER {
        return Vec::new();
    }
    let count = u32::from_ne_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    let held = (bytes.len() - HEADER) / 4;
    let pixels = count.min(held);
    bytes[HEADER..HEADER + pixels * 4]
        .chunks_exact(2)
        .map(|c| i16::from_ne_bytes([c[0], c[1]]))
        .collect()
}

/// Analyzes `audio_file_path`: decodes it to mono with the engine, then
/// returns up to `target_buckets` peaks in [0, 1] and the audio duration in
/// milliseconds ([`analyze_pcm`]). Fails if decoding fails. Blocking: the
/// decode is the whole file's, so run it off the UI thread.
///
/// The decode is bounded as playback staging is (the clips analyzed here are
/// the ones staged): [`MAX_PLAYBACK_DURATION`] and
/// [`MAX_PLAYBACK_SRC_FRAMES`]. `just_waveform` streamed the file and had no
/// such caps; a file over them fails with the engine's refusal (the import
/// gate refuses an over-long file before it gets here).
pub fn analyze(
    audio_file_path: &str,
    target_buckets: usize,
) -> Result<WaveformResult, WaveformError> {
    analyze_with(audio_file_path, target_buckets, |path| {
        media::decode_mono_capped(
            path,
            DECODE_RATE,
            MAX_PLAYBACK_DURATION,
            MAX_PLAYBACK_SRC_FRAMES,
        )
    })
}

/// [`analyze`] over the mono decode `decode` stands for (the engine's in
/// production; a test's own where there is no engine). A refusal the decode
/// names (too long, too dense, unreadable) fails with its message; any other
/// failure, an empty decode or no engine at all, with the generic one, as
/// `just_waveform` completing with no waveform did.
pub fn analyze_with(
    audio_file_path: &str,
    target_buckets: usize,
    decode: impl FnOnce(&str) -> Result<Option<Pcm>, MediaError>,
) -> Result<WaveformResult, WaveformError> {
    match decode(audio_file_path) {
        Ok(Some(pcm)) => analyze_pcm(audio_file_path, pcm.samples(), pcm.rate(), target_buckets),
        Ok(None) | Err(MediaError::EngineUnavailable) => Err(WaveformError(UNREADABLE.into())),
        Err(e) => Err(WaveformError(e.to_string())),
    }
}

/// Analyzes decoded mono [samples] at [sample_rate] read from
/// [audio_file_path]: up to [target_buckets] peaks in [0, 1] and the audio
/// duration in milliseconds. Fails, as Dart's did, on audio too short to
/// give a single pixel, so callers abort and toast instead of persisting a
/// 0-length, waveform-less clip that renders as an invisible lane.
pub fn analyze_pcm(
    audio_file_path: &str,
    samples: &[f32],
    sample_rate: u32,
    target_buckets: usize,
) -> Result<WaveformResult, WaveformError> {
    if samples.is_empty() || sample_rate == 0 {
        return Err(WaveformError(UNREADABLE.into()));
    }
    let pairs = pixel_pairs(samples, sample_rate);
    // The waveform's own duration counted whole 10 ms pixels.
    let pixel_ms = (pairs.len() / 2) as i64 * (1000 / i64::from(PIXELS_PER_SECOND));
    let waveform_ms = pixel_ms.min((samples.len() as i64 * 1000) / i64::from(sample_rate));
    Ok(WaveformResult {
        peaks: downsample(&pairs, target_buckets),
        duration_ms: exact_duration_ms(audio_file_path, waveform_ms),
    })
}

/// [path]'s length to the millisecond when it is a WAV (every clip is, once
/// imported), else [waveform_ms]. The waveform counts whole 10 ms pixels, up
/// to 10 ms short of the audio: a merge shifted the bottom take's notes that
/// much early, a split left a gap over audio that plays, and a loop over the
/// whole song dropped the tail. The header's count is used only where it
/// agrees with the decoded one to within that grid.
pub fn exact_duration_ms(path: &str, waveform_ms: i64) -> i64 {
    let Ok(Some(wav)) = probe_wav(Path::new(path)) else {
        return waveform_ms;
    };
    let frame_bytes = u64::from(wav.channels * wav.bits_per_sample / 8);
    if frame_bytes == 0 {
        return waveform_ms;
    }
    let ms = ((wav.data_bytes / frame_bytes) as f64 * 1000.0 / f64::from(wav.sample_rate)).round()
        as i64;
    if ms >= waveform_ms && ms - waveform_ms <= 20 {
        ms
    } else {
        waveform_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A .wave file as the package's darwin writer laid it out: a 20-byte
    /// header and 16-bit (min, max) pairs.
    fn wave(peak_per_pixel: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
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
        let mut pixels = vec![0i16; 18000];
        pixels[50] = 16384;
        let peaks = downsample(&wave_file_pairs(&wave(&pixels)), 18000);
        assert_eq!(peaks.len(), 18000);
        assert!(peaks[..3].iter().all(|&p| p == 0.0), "no header spike");
        assert_eq!(peaks[50], 0.5);
        assert_eq!(peaks[51], 0.0);
        assert_eq!(peaks[52], 0.0, "not drawn late");
    }

    #[test]
    fn a_long_take_downsamples_to_its_buckets() {
        let pixels = vec![3277i16; 18000];
        let peaks = downsample(&wave_file_pairs(&wave(&pixels)), 100);
        assert_eq!(peaks.len(), 100);
        assert!(peaks.iter().all(|p| (p - 0.1).abs() < 1e-4));
    }

    fn wav_bytes(frames: usize, rate: u32) -> Vec<u8> {
        let data = (frames * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        b.resize(44 + frames * 2, 0);
        b
    }

    #[test]
    fn a_wav_clip_is_as_long_as_its_audio_not_its_10_ms_pixels() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("take.wav");
        fs::write(&wav, wav_bytes(48_000 * 1234 / 1000, 48_000)).unwrap();
        let path = wav.to_string_lossy();
        assert_eq!(exact_duration_ms(&path, 1230), 1234);
        // A header that disagrees with the decoded length by more than the
        // pixel grid is not trusted.
        assert_eq!(exact_duration_ms(&path, 900), 900);
        let mp3 = tmp.path().join("song.mp3");
        fs::write(&mp3, [0u8; 100]).unwrap();
        assert_eq!(exact_duration_ms(&mp3.to_string_lossy(), 1230), 1230);
    }

    #[test]
    fn pcm_pixels_are_10_ms_min_max_pairs() {
        let rate = 48_000;
        let mut samples = vec![0.0f32; rate as usize / 10]; // 100 ms
        samples[480] = 0.5; // pixel 1
        samples[960] = -1.0; // pixel 2
        let pairs = pixel_pairs(&samples, rate);
        assert_eq!(pairs.len(), 20);
        assert_eq!(pairs[2..4], [0, 16384]);
        assert_eq!(pairs[4..6], [-32767, 0]);
        let r = analyze_pcm("/no/such.mp3", &samples, rate, 1000).unwrap();
        assert_eq!(r.peaks.len(), 10);
        assert_eq!(r.duration_ms, 100);
        assert!(analyze_pcm("/x", &[], rate, 10).is_err());
    }

    #[test]
    fn analyze_decodes_through_the_engine_seam() {
        let rate = 48_000;
        let mut samples = vec![0.0f32; rate as usize / 10]; // 100 ms
        samples[480] = 0.5;
        let seen = std::cell::Cell::new(String::new());
        let r = analyze_with("/takes/a.mp3", 1000, |path| {
            seen.set(path.to_string());
            Ok(Some(Pcm::mono(samples.clone(), rate)))
        })
        .unwrap();
        assert_eq!(seen.take(), "/takes/a.mp3");
        assert_eq!(r.peaks.len(), 10);
        assert_eq!(r.peaks[1], 16384.0 / 32768.0);
        assert_eq!(r.duration_ms, 100);
    }

    #[test]
    fn a_failed_decode_fails_the_analysis() {
        let generic = analyze_with("/x.mp3", 10, |_| Ok(None)).unwrap_err();
        assert_eq!(generic.0, UNREADABLE);
        let no_engine =
            analyze_with("/x.mp3", 10, |_| Err(MediaError::EngineUnavailable)).unwrap_err();
        assert_eq!(no_engine.0, UNREADABLE);
        // A refusal the decode names keeps its own message.
        let too_long = analyze_with("/x.mp3", 10, |_| {
            Err(MediaError::TooLong {
                max_duration: MAX_PLAYBACK_DURATION,
            })
        })
        .unwrap_err();
        assert_eq!(
            too_long.0,
            "This file is too long. The limit is 60 minutes."
        );
        let empty = analyze_with("/x.mp3", 10, |_| {
            Ok(Some(Pcm::mono(Vec::new(), DECODE_RATE)))
        });
        assert_eq!(empty.unwrap_err().0, UNREADABLE);
    }

    #[cfg(not(feature = "device"))]
    #[test]
    fn without_an_engine_nothing_analyzes() {
        let tmp = tempfile::tempdir().unwrap();
        let wav = tmp.path().join("take.wav");
        fs::write(&wav, wav_bytes(4800, 48_000)).unwrap();
        let err = analyze(&wav.to_string_lossy(), DEFAULT_TARGET_BUCKETS).unwrap_err();
        assert_eq!(err.0, UNREADABLE);
    }

    #[test]
    fn the_scratch_sweep_clears_only_wave_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(SCRATCH_DIR_NAME);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.wave"), b"x").unwrap();
        fs::write(dir.join("b.wave"), b"x").unwrap();
        fs::write(dir.join("keep.txt"), b"x").unwrap();
        assert_eq!(sweep_scratch(tmp.path()), 2);
        assert!(dir.join("keep.txt").exists());
        assert_eq!(sweep_scratch(&tmp.path().join("missing")), 0);
    }
}
