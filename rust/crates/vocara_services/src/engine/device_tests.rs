//! The native paths against the engine's own media fixtures (`device`
//! builds). No engine instance is created: that opens the audio hardware.

use std::time::Duration;

use super::media::{self, MediaError};
use super::*;
use crate::waveform_service;

fn fixture(name: &str) -> String {
    format!(
        "{}/../../../client/rust/vocara_engine/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// 1 s at 44.1 kHz, silent until a 0.5 s tone onset (LAME delay and padding
/// trimmed).
const ONSET_MP3: &str = "onset_0500ms.mp3";

#[test]
fn a_decode_owns_its_engine_buffer() {
    let pcm = decode(&fixture(ONSET_MP3), 44_100, 0, 0).unwrap();
    assert_eq!(pcm.layout(), Layout::InterleavedStereo);
    assert_eq!((pcm.frames(), pcm.rate()), (44_100, 44_100));
    let s = pcm.samples();
    assert_eq!(s.len(), 2 * 44_100);
    let onset = s.chunks(2).position(|f| f[0].abs() > 0.09).unwrap();
    assert!(onset.abs_diff(22_050) < 44, "the tone starts at {onset}");
    // Copied out before the engine buffer is freed (once, on drop).
    let v = pcm.into_vec();
    assert_eq!(v.len(), 2 * 44_100);
}

#[test]
fn a_mono_decode_is_the_stereo_average_freed_by_its_own_call() {
    let stereo = decode(&fixture(ONSET_MP3), 44_100, 0, 0).unwrap();
    let mut mono = decode_mono(&fixture(ONSET_MP3), 44_100, 0, 0).unwrap();
    assert_eq!(mono.layout(), Layout::Mono);
    assert_eq!(mono.frames(), stereo.frames());
    for (m, lr) in mono.samples().iter().zip(stereo.samples().chunks(2)) {
        assert_eq!(*m, (lr[0] + lr[1]) * 0.5);
    }
    // Writable in place, as the analysis zeroes non-finite samples.
    mono.samples_mut()[0] = 0.25;
    assert_eq!(mono.samples()[0], 0.25);
}

#[test]
fn a_decode_resamples_to_the_rate_asked() {
    let pcm = decode(&fixture(ONSET_MP3), ENGINE_RATE, 0, 0).unwrap();
    assert_eq!(pcm.rate(), ENGINE_RATE);
    assert!(pcm.frames().abs_diff(48_000) <= 2, "{}", pcm.frames());
}

#[test]
fn refusals_come_back_typed() {
    let mp3 = fixture(ONSET_MP3);
    // Over the duration budget: refused before the track allocates.
    assert_eq!(
        decode(&mp3, 44_100, 4_410, 0).unwrap_err(),
        DecodeError::TooLong
    );
    // Over the absolute source-frame budget without being over-duration.
    assert_eq!(
        decode_mono(&mp3, 44_100, 0, 1_000).unwrap_err(),
        DecodeError::TooBig
    );
    // The disk, not the format.
    assert_eq!(
        decode(&fixture("no_such_file.mp3"), 44_100, 0, 0).unwrap_err(),
        DecodeError::Io
    );
    // No output rate: the generic failure.
    assert_eq!(decode(&mp3, 0, 0, 0).unwrap_err(), DecodeError::Failed);
    let tmp = tempfile::tempdir().unwrap();
    let junk = tmp.path().join("junk.mp3");
    std::fs::write(&junk, b"not audio at all").unwrap();
    assert_eq!(
        decode(&junk.to_string_lossy(), 44_100, 0, 0).unwrap_err(),
        DecodeError::Failed
    );
}

#[test]
fn the_probe_reads_declared_lengths_only() {
    assert_eq!(probe_duration_ms(&fixture(ONSET_MP3)), Ok(Some(1000)));
    // A WAV written to a pipe declares no length.
    assert_eq!(probe_duration_ms(&fixture("pipe_100ms.wav")), Ok(None));
    assert_eq!(
        probe_duration_ms(&fixture("no_such_file.wav")),
        Err(ProbeError::Unreadable)
    );
    assert_eq!(
        media::probe_media_duration(&fixture("unclosed_100ms.caf")),
        Ok(Some(Duration::from_millis(100)))
    );
    assert_eq!(
        media::probe_media_duration(&fixture("no_such_file.wav")),
        Ok(None)
    );
}

#[test]
fn a_punch_splices_old_before_new_and_never_writes_over_an_input() {
    let mp3 = fixture(ONSET_MP3);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("punched.wav");
    let out = out.to_string_lossy();
    let new_frames = decode(&mp3, ENGINE_RATE, 0, 0).unwrap().frames() as u64;
    let n = punch_wav(&mp3, &mp3, 250, ENGINE_RATE, &out).unwrap();
    assert_eq!(n, 12_000 + new_frames);
    let back = decode(&out, ENGINE_RATE, 0, 0).unwrap();
    assert_eq!(back.frames() as u64, n);
    // The output is never one of the inputs.
    assert_eq!(
        punch_wav(&out, &mp3, 250, ENGINE_RATE, &out),
        Err(DecodeError::Failed)
    );
    // A missing input names its cause.
    assert_eq!(
        punch_wav(
            &mp3,
            &fixture("no_such_file.mp3"),
            0,
            ENGINE_RATE,
            &tmp.path().join("x.wav").to_string_lossy()
        ),
        Err(DecodeError::Io)
    );
}

#[test]
fn the_media_helpers_apply_their_caps() {
    let mp3 = fixture(ONSET_MP3);
    let pcm = media::decode_media_to_mono(&mp3, media::ANALYSIS_DECODE_RATE, None)
        .unwrap()
        .unwrap();
    assert_eq!((pcm.frames(), pcm.layout()), (44_100, Layout::Mono));
    let cap = Duration::from_millis(500);
    assert_eq!(
        media::decode_media_to_mono(&mp3, 44_100, Some(cap)).unwrap_err(),
        MediaError::TooLong { max_duration: cap }
    );
    let staged = media::decode_for_playback(&mp3, ENGINE_RATE)
        .unwrap()
        .unwrap();
    assert_eq!(staged.layout(), Layout::InterleavedStereo);
    assert!(
        media::decode_for_playback(&fixture("no_such_file.mp3"), ENGINE_RATE)
            .is_err_and(|e| e == MediaError::Read)
    );
}

#[test]
fn the_waveform_decodes_through_the_engine() {
    // Resampled from 44.1 kHz to the engine rate: a second, give or take the
    // resampler's last frame.
    let r = waveform_service::analyze(&fixture(ONSET_MP3), 100).unwrap();
    assert!(r.duration_ms.abs_diff(1000) <= 1, "{} ms", r.duration_ms);
    assert_eq!(r.peaks.len(), 100);
    assert!(
        r.peaks[..45].iter().all(|&p| p < 0.01),
        "silent before the onset"
    );
    assert!(
        r.peaks[55..95].iter().all(|&p| p > 0.05),
        "the tone after it"
    );
}

#[test]
fn host_queries_answer_without_an_engine() {
    assert!(enumerate_output_devices().len() <= DEVICE_ENUM_CAP);
    assert!(enumerate_input_devices().iter().all(|d| d.is_input));
    assert_eq!(input_device_channel_count("no-such-device-uid"), 0);
}
