//! Port of `client/test/audio_edit_service_test.dart`, and of the
//! `trimWavHeadInPlace` half of `client/test/wav_malformed_input_test.dart`
//! (its `probeWav` and `admitForAnalysis` halves belong to the audio file
//! and spectrogram services).
//!
//! Not repeated here: the WAV-parsing assertions of the Dart file that check
//! `readWavMono` itself (it accepts an EXTENSIBLE PCM16 file, and walks a
//! word-aligned odd-size fmt chunk), which `audio_dsp`'s own tests port
//! (`read_wav_mono_accepts_extensible_pcm16`,
//! `read_wav_mono_walks_a_word_aligned_odd_fmt_chunk`). The edit outputs here
//! are still read back through `read_wav_mono`, as the Dart tests did.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vocara_services::audio_dsp::{read_wav_mono, write_wav_mono};
use vocara_services::audio_edit_service::*;
use vocara_services::audio_file_service::{self, AudioFiles};

const PRACTICE: &str = "P0000000000000000000000000";

/// An [`AudioFiles`] that hands out temp-dir paths so the edit service can
/// be exercised without the sandbox (Dart's `_TempFiles`).
struct TempFiles {
    dir: PathBuf,
    n: AtomicUsize,
}

impl TempFiles {
    fn new(dir: &Path) -> Arc<TempFiles> {
        Arc::new(TempFiles {
            dir: dir.to_path_buf(),
            n: AtomicUsize::new(0),
        })
    }
}

impl AudioFiles for TempFiles {
    fn import_audio(&self, _: &str, _: &str) -> audio_file_service::Result<String> {
        unimplemented!()
    }
    fn new_recording_path(
        &self,
        _practice_id: &str,
        extension: &str,
        _take_id: Option<&str>,
        _start_ms: Option<i64>,
    ) -> audio_file_service::Result<String> {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{}/clip_{n}.{extension}", self.dir.display()))
    }
    fn write_aligned_start(&self, _: &str, _: i64) -> bool {
        unimplemented!()
    }
    fn is_app_storage(&self, _: &str) -> bool {
        unimplemented!()
    }
    fn delete_audio(&self, _: &str) -> audio_file_service::Result<()> {
        unimplemented!()
    }
}

fn service(dir: &Path) -> AudioEditService {
    AudioEditService::new(TempFiles::new(dir))
}

fn path_of(dir: &Path, name: &str) -> String {
    dir.join(name).to_string_lossy().into_owned()
}

/// A little-endian byte builder.
#[derive(Default)]
struct Bytes(Vec<u8>);

impl Bytes {
    fn str(&mut self, s: &str) -> &mut Self {
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}

fn pcm16(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// A mono 16-bit WAV tagged WAVE_FORMAT_EXTENSIBLE (0xFFFE), the way
/// AVFoundation often writes recordings.
fn extensible_wav_pcm16(samples: &[i16], sr: u32) -> Vec<u8> {
    let data = pcm16(samples);
    let fmt_size = 40u32;
    let mut b = Bytes::default();
    b.str("RIFF")
        .u32(4 + (8 + fmt_size) + (8 + data.len() as u32))
        .str("WAVE")
        .str("fmt ")
        .u32(fmt_size)
        .u16(0xFFFE) // WAVE_FORMAT_EXTENSIBLE
        .u16(1) // channels
        .u32(sr) // sample rate
        .u32(sr * 2) // byte rate
        .u16(2) // block align
        .u16(16) // bits per sample
        .u16(22) // cbSize
        .u16(16) // valid bits
        .u32(0) // channel mask
        .u16(1) // SubFormat tag = PCM
        .bytes(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ])
        .str("data")
        .u32(data.len() as u32)
        .bytes(&data);
    b.0
}

/// A mono PCM16 WAV whose `fmt ` chunk declares an ODD size (17): the 16
/// canonical bytes plus one extension byte, followed by the spec-required
/// word-alignment pad byte before `data`. The old import gate accepted ANY
/// uncompressed WAV, so a legacy library can still hold such files.
fn odd_fmt_wav_pcm16(samples: &[i16], sr: u32) -> Vec<u8> {
    let data = pcm16(samples);
    let fmt_size = 17u32;
    let data_pad = (data.len() & 1) as u32;
    let mut b = Bytes::default();
    b.str("RIFF")
        .u32(4 + (8 + fmt_size + 1) + (8 + data.len() as u32 + data_pad))
        .str("WAVE")
        .str("fmt ")
        .u32(fmt_size)
        .u16(1) // PCM
        .u16(1) // channels
        .u32(sr)
        .u32(sr * 2)
        .u16(2)
        .u16(16)
        .bytes(&[0]) // 17th fmt byte (a lone extension byte — size is what matters)
        .bytes(&[0]) // word-alignment pad before the next chunk
        .str("data")
        .u32(data.len() as u32)
        .bytes(&data);
    if data_pad == 1 {
        b.bytes(&[0]);
    }
    b.0
}

/// A canonical RIFF/WAVE file with an arbitrary 16-byte `fmt ` chunk and raw
/// [data] — for exercising the header validation with compressed or lying
/// formats.
fn wav_with_fmt(
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits: u16,
    data: &[u8],
) -> Vec<u8> {
    let mut b = Bytes::default();
    b.str("RIFF")
        .u32(36 + data.len() as u32)
        .str("WAVE")
        .str("fmt ")
        .u32(16)
        .u16(format_tag)
        .u16(channels)
        .u32(sample_rate)
        .u32(sample_rate * u32::from(block_align))
        .u16(block_align)
        .u16(bits)
        .str("data")
        .u32(data.len() as u32)
        .bytes(data);
    b.0
}

/// The `data` chunk payload of a canonical (RIFF + fmt + data) WAV.
fn data_chunk(wav: &[u8]) -> Vec<u8> {
    let mut at = 12;
    while at + 8 <= wav.len() {
        let size = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
        if &wav[at..at + 4] == b"data" {
            return wav[at + 8..at + 8 + size].to_vec();
        }
        at += 8 + size + (size & 1);
    }
    panic!("no data chunk");
}

/// A 16-bit stereo PCM WAV of [frames] frames of a quiet ramp.
fn stereo_pcm16(frames: usize, sample_rate: u32) -> Vec<u8> {
    let data_len = (frames * 4) as u32;
    let mut b = Bytes::default();
    b.str("RIFF")
        .u32(36 + data_len)
        .str("WAVE")
        .str("fmt ")
        .u32(16)
        .u16(1) // PCM
        .u16(2) // channels
        .u32(sample_rate)
        .u32(sample_rate * 4)
        .u16(4) // block align
        .u16(16)
        .str("data")
        .u32(data_len);
    for i in 0..frames {
        b.u16((i % 100) as i16 as u16);
        b.u16((-((i % 100) as i16)) as u16);
    }
    b.0
}

fn is_format(r: &Result<impl std::fmt::Debug>) -> bool {
    matches!(r, Err(AudioEditError::Format(_)))
}

fn mono_samples(bytes: &[u8]) -> Vec<f32> {
    read_wav_mono(bytes).expect("parses").samples
}

// Regression guard: the split point was clamped against the RAW data length,
// so a data chunk that isn't a whole number of frames could put the cut
// INSIDE a frame — the second half then starts on the wrong channel and
// plays with the stereo image swapped for its whole length.
#[test]
fn split_wav_cuts_on_a_frame_boundary_even_when_the_data_chunk_is_a_partial_frame_long() {
    let dir = tempfile::tempdir().unwrap();
    let block_align = 4usize; // stereo, 16-bit
    let frames = 100usize;
    // 100 whole frames plus a stray half frame, as a write cut short leaves.
    let data: Vec<u8> = (0..frames * block_align + 2).map(|i| i as u8).collect();
    let src = path_of(dir.path(), "ragged.wav");
    fs::write(
        &src,
        wav_with_fmt(1, 2, 8000, block_align as u16, 16, &data),
    )
    .unwrap();

    let edit = service(dir.path());
    // Far past the end, so the cut clamps to the last legal frame boundary —
    // the case that used to land on the ragged tail.
    let parts = edit.split_wav(PRACTICE, &src, 60000).unwrap();

    let first = data_chunk(&fs::read(&parts.first_path).unwrap());
    let second = data_chunk(&fs::read(&parts.second_path).unwrap());
    assert_eq!(
        first.len() % block_align,
        0,
        "the cut must not split a frame"
    );
    assert_eq!(second.len() % block_align, 0);
    assert_eq!(
        first.len() + second.len(),
        frames * block_align,
        "the trailing partial frame is dropped, never duplicated"
    );
    // Byte-for-byte continuous: the second half resumes exactly where the
    // first ended (a mid-frame cut would offset it by two bytes).
    assert_eq!(first, data[..first.len()]);
    assert_eq!(second, data[first.len()..first.len() + second.len()]);
}

#[test]
fn split_wav_cuts_a_take_into_two_parts_that_rejoin_to_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000usize;
    // A 1s ramp from -1..1 so the two halves are clearly distinguishable and
    // a continuous cut is easy to assert.
    let samples: Vec<f64> = (0..sr)
        .map(|i| (i as f64 / sr as f64) * 2.0 - 1.0)
        .collect();
    let src = path_of(dir.path(), "src.wav");
    fs::write(&src, write_wav_mono(&samples, sr as i64)).unwrap();

    let edit = service(dir.path());
    let parts = edit.split_wav(PRACTICE, &src, 400).unwrap();

    let first = read_wav_mono(&fs::read(&parts.first_path).unwrap()).unwrap();
    let second = read_wav_mono(&fs::read(&parts.second_path).unwrap()).unwrap();
    assert_eq!(first.sample_rate, sr as i64);
    // 400 ms at 8 kHz = 3200 samples in the first part.
    assert_eq!(first.samples.len(), 3200);
    assert_eq!(second.samples.len(), sr - 3200);
    // The cut is seamless: the two slices bracket the original ramp.
    assert!((f64::from(*first.samples.last().unwrap()) - samples[3199]).abs() < 2e-4);
    assert!((f64::from(second.samples[0]) - samples[3200]).abs() < 2e-4);

    let merged = edit
        .concat_wav(PRACTICE, &parts.first_path, &parts.second_path)
        .unwrap();
    let round = mono_samples(&fs::read(&merged).unwrap());
    assert_eq!(round.len(), sr);
    for i in (0..sr).step_by(137) {
        assert!(
            (f64::from(round[i]) - samples[i]).abs() < 2e-4,
            "sample {i}"
        );
    }
}

#[test]
fn a_split_or_merge_that_fails_to_publish_leaves_nothing_in_edits() {
    // A split whose second rename failed left its first half at its final
    // path, a finished edit; a merge whose rename failed left the whole
    // merged temp behind.
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000;
    let src = path_of(dir.path(), "src.wav");
    fs::write(&src, write_wav_mono(&vec![0.25f64; sr], sr as i64)).unwrap();
    let edits = dir.path().join("edits");
    let left = || -> Vec<String> {
        fs::read_dir(&edits)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    };

    // Splits write clip_0 and clip_1: a directory where the second goes fails
    // its rename.
    fs::create_dir_all(edits.join("clip_1.wav")).unwrap();
    let splitter = service(dir.path());
    assert!(matches!(
        splitter.split_wav(PRACTICE, &src, 400),
        Err(AudioEditError::Io(_))
    ));
    assert!(left().is_empty(), "{:?}", left());

    // A merge writes clip_0.
    fs::create_dir(edits.join("clip_0.wav")).unwrap();
    let merger = service(dir.path());
    assert!(matches!(
        merger.concat_wav(PRACTICE, &src, &src),
        Err(AudioEditError::Io(_))
    ));
    assert!(left().is_empty(), "{:?}", left());
}

#[test]
fn reads_and_splits_a_wave_format_extensible_recording() {
    // AVFoundation (the `record` plugin's macOS backend) can write WAVs
    // tagged 0xFFFE rather than plain PCM. Both the decoder and the
    // byte-level split must handle it.
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000u32;
    let samples: Vec<i16> = (0..sr).map(|i| ((i % 200) * 100) as i16 - 10000).collect();
    let src = path_of(dir.path(), "rec.wav");
    fs::write(&src, extensible_wav_pcm16(&samples, sr)).unwrap();

    let pcm = read_wav_mono(&fs::read(&src).unwrap());
    assert_eq!(
        pcm.expect("readWavMono must accept EXTENSIBLE PCM16")
            .samples
            .len(),
        sr as usize
    );

    let edit = service(dir.path());
    let parts = edit.split_wav(PRACTICE, &src, 250).unwrap();
    let first = mono_samples(&fs::read(&parts.first_path).unwrap());
    let second = mono_samples(&fs::read(&parts.second_path).unwrap());
    assert_eq!(first.len(), 2000); // 250 ms at 8 kHz
    assert_eq!(second.len(), sr as usize - 2000);
}

/// One punch-in the stand-in engine was asked for.
#[derive(Debug, Clone, PartialEq)]
struct Punch {
    old: String,
    neu: String,
    split_ms: i64,
}

// A rate or channel mismatch used to be refused outright, though the engine's
// re-encoding merge handles both: Merge down failed for a mono recording
// under a stereo import.
#[test]
fn concat_wav_re_encodes_takes_whose_rate_or_channels_differ_instead_of_joining_or_refusing_them() {
    let dir = tempfile::tempdir().unwrap();
    let tone: Vec<f64> = (0..441).map(|i| f64::from(i % 50) / 50.0 - 0.5).collect();
    let a = path_of(dir.path(), "a.wav"); // mono, 44.1 kHz: 441 frames = 10 ms
    let rate = path_of(dir.path(), "rate.wav"); // mono, 48 kHz
    let stereo = path_of(dir.path(), "stereo.wav"); // stereo, 44.1 kHz
    fs::write(&a, write_wav_mono(&tone, 44100)).unwrap();
    fs::write(&rate, write_wav_mono(&tone, 48000)).unwrap();
    fs::write(&stereo, stereo_pcm16(441, 44100)).unwrap();

    // Stands in for the engine's re-encoding merge (it does not splice in
    // unit tests), recording what it was asked.
    let punches: Arc<Mutex<Vec<Punch>>> = Arc::default();
    let seen = punches.clone();
    let edit = service(dir.path()).with_punch_in(Arc::new(
        move |_practice: &str, old: &str, neu: &str, split_ms: i64| {
            seen.lock().unwrap().push(Punch {
                old: old.into(),
                neu: neu.into(),
                split_ms,
            });
            Ok(Some(format!("punched:{neu}")))
        },
    ));
    for second in [&rate, &stereo] {
        let out = edit.concat_wav(PRACTICE, &a, second).unwrap();
        assert_eq!(out, format!("punched:{second}"));
    }
    assert_eq!(
        *punches.lock().unwrap(),
        vec![
            Punch {
                old: a.clone(),
                neu: rate.clone(),
                split_ms: 10
            },
            Punch {
                old: a.clone(),
                neu: stereo.clone(),
                split_ms: 10
            },
        ],
        "split at all of the first take"
    );

    // Same format still joins byte for byte, without the engine.
    let c = path_of(dir.path(), "c.wav");
    fs::write(&c, write_wav_mono(&tone, 44100)).unwrap();
    let out = edit.concat_wav(PRACTICE, &a, &c).unwrap();
    assert_eq!(mono_samples(&fs::read(&out).unwrap()).len(), 882);
    assert_eq!(punches.lock().unwrap().len(), 2);
}

#[test]
fn a_re_encoded_merge_over_the_cap_names_the_merge_and_a_failed_one_says_so() {
    // The engine's refusal named one file ("This file is too long"); the
    // merge says the two together are. A splice that produced nothing is a
    // re-encoding failure.
    let dir = tempfile::tempdir().unwrap();
    let tone: Vec<f64> = vec![0.1; 441];
    let a = path_of(dir.path(), "a.wav");
    let b = path_of(dir.path(), "b.wav");
    fs::write(&a, write_wav_mono(&tone, 44100)).unwrap();
    fs::write(&b, write_wav_mono(&tone, 48000)).unwrap();
    let too_long = service(dir.path()).with_punch_in(Arc::new(|_: &str, _: &str, _: &str, _| {
        Err(AudioEditError::Media(
            vocara_services::engine::media::MediaError::TooLong {
                max_duration: vocara_services::engine::media::MAX_PLAYBACK_DURATION,
            },
        ))
    }));
    match too_long.concat_wav(PRACTICE, &a, &b) {
        Err(AudioEditError::Format(m)) => {
            assert_eq!(m, "The merged take would be longer than 60 minutes.")
        }
        other => panic!("{other:?}"),
    }
    let failed =
        service(dir.path()).with_punch_in(Arc::new(|_: &str, _: &str, _: &str, _| Ok(None)));
    match failed.concat_wav(PRACTICE, &a, &b) {
        Err(AudioEditError::Format(m)) => assert_eq!(
            m,
            "These takes have different audio formats and merging them by re-encoding failed."
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn concat_wav_merges_a_plain_pcm_take_with_an_extensible_take_of_the_same_actual_format() {
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000u32;
    let pcm = path_of(dir.path(), "pcm.wav"); // 16-byte PCM fmt
    let ext = path_of(dir.path(), "ext.wav"); // 40-byte EXTENSIBLE fmt
    fs::write(&pcm, write_wav_mono(&[0.1f64; 100], i64::from(sr))).unwrap();
    fs::write(&ext, extensible_wav_pcm16(&[1000i16; 100], sr)).unwrap();

    let edit = service(dir.path());
    let out = edit.concat_wav(PRACTICE, &pcm, &ext).unwrap();
    assert_eq!(mono_samples(&fs::read(&out).unwrap()).len(), 200);
}

#[test]
fn a_valid_bits_or_channel_mask_mismatch_is_not_byte_spliceable() {
    // The byte-splice keeps the FIRST header: joining a full-16 take with a
    // valid-bits-12 EXTENSIBLE take would reinterpret the second half's
    // samples under the wrong significance (24-in-32 style reinterpretation
    // is corruption). Container bits match, so only the validBits comparison
    // can catch it — the incompatibility routes concat through the decode ->
    // re-encode path instead.
    let sr = 8000;
    let plain = write_wav_mono(&[0.1f64; 100], sr);
    let ext = extensible_wav_pcm16(&[1000i16; 100], sr as u32);
    assert_eq!(
        wav_compatible_for_merge(&plain, &ext),
        Some(true),
        "baseline"
    );

    // Patch validBitsPerSample (fmt offset 18; fmt body starts at byte 20).
    let mut vb12 = ext.clone();
    vb12[38] = 12;
    assert_eq!(wav_compatible_for_merge(&plain, &vb12), Some(false));

    // Two EXTENSIBLE takes with different channel masks must not byte-splice
    // under the first's mask either (fmt offset 20).
    let mut mask_front = ext.clone();
    mask_front[40] = 1;
    let mut mask_back = ext.clone();
    mask_back[40] = 4;
    assert_eq!(
        wav_compatible_for_merge(&mask_front, &mask_back),
        Some(false)
    );
    assert_eq!(
        wav_compatible_for_merge(&mask_front, &mask_front),
        Some(true)
    );

    // A plain fmt makes no speaker-position claim (mask 0), so it does not
    // match one that does — in EITHER order. The splice keeps the FIRST
    // file's header, so letting this through would stamp one half's channel
    // layout over the other's. The `ext` baseline above still passes because
    // its mask is 0 too.
    assert_eq!(wav_compatible_for_merge(&plain, &mask_front), Some(false));
    assert_eq!(wav_compatible_for_merge(&mask_front, &plain), Some(false));

    // Not an uncompressed WAV on either side: no answer.
    assert_eq!(wav_compatible_for_merge(&plain, b"not a wav"), None);
}

#[test]
fn ensure_streamable_wav_passes_an_engine_rate_canonical_wav_through_untouched() {
    // No re-encode, no copy.
    let dir = tempfile::tempdir().unwrap();
    let tone: Vec<f64> = (0..400).map(|i| f64::from(i % 50) / 50.0 - 0.5).collect();
    let src = path_of(dir.path(), "take.wav");
    fs::write(&src, write_wav_mono(&tone, 48000)).unwrap();
    let before = fs::metadata(&src).unwrap().len();

    let edit = service(dir.path());
    let out = edit.ensure_streamable_wav(PRACTICE, &src).unwrap();
    assert_eq!(out, src, "a canonical engine-rate WAV is streamable");
    assert_eq!(fs::metadata(&src).unwrap().len(), before, "byte-identical");
    // The transcode branch itself needs the native engine (integration
    // coverage); the WAV passthrough is the decision this test pins.
}

#[test]
fn ensure_streamable_wav_transcodes_anything_else_and_says_so_when_that_fails() {
    // The transcode is the engine's split-0 splice with the source as `new`.
    let dir = tempfile::tempdir().unwrap();
    let src = path_of(dir.path(), "cd.wav");
    fs::write(&src, write_wav_mono(&[0.1f64; 64], 44100)).unwrap();
    let asked: Arc<Mutex<Vec<(String, String, i64)>>> = Arc::default();
    let seen = asked.clone();
    let edit = service(dir.path()).with_punch_in(Arc::new(
        move |_: &str, old: &str, neu: &str, split: i64| {
            seen.lock().unwrap().push((old.into(), neu.into(), split));
            Ok(Some("transcoded.wav".into()))
        },
    ));
    assert_eq!(
        edit.ensure_streamable_wav(PRACTICE, &src).unwrap(),
        "transcoded.wav"
    );
    assert_eq!(*asked.lock().unwrap(), vec![(src.clone(), src.clone(), 0)]);

    let failing =
        service(dir.path()).with_punch_in(Arc::new(|_: &str, _: &str, _: &str, _| Ok(None)));
    match failing.ensure_streamable_wav(PRACTICE, &src) {
        Err(AudioEditError::Format(m)) => {
            assert_eq!(m, "This file could not be converted for playback.")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn is_engine_streamable_wav_mirrors_the_native_feeder_admission() {
    // Vectors matched by the engine's feed tests (WavTrackReader::open) — the
    // two rules must not drift: only a canonical 44-byte header at the engine
    // rate with 1-2 PCM16/f32 channels streams from disk; everything else
    // must be transcoded at import or it silently falls back to the
    // full-decode RAM path.
    let dir = tempfile::tempdir().unwrap();
    let tone: Vec<f64> = (0..64).map(|i| f64::from(i % 16) / 16.0 - 0.5).collect();
    let judge = |name: &str, bytes: &[u8]| {
        let path = path_of(dir.path(), name);
        fs::write(&path, bytes).unwrap();
        AudioEditService::is_engine_streamable_wav(&path)
    };

    assert!(judge("ok48k.wav", &write_wav_mono(&tone, 48000)));
    assert!(
        !judge("cd44k.wav", &write_wav_mono(&tone, 44100)),
        "44.1 kHz is not the engine rate"
    );
    assert!(
        !judge("ext.wav", &extensible_wav_pcm16(&[100i16; 64], 48000)),
        "EXTENSIBLE fmt is not the canonical 44-byte layout"
    );
    // PCM24: canonical layout but a sample format the feeder refuses.
    let mut pcm24 = write_wav_mono(&tone, 48000);
    pcm24[34] = 24; // bitsPerSample 16 -> 24
    assert!(!judge("pcm24.wav", &pcm24), "PCM24 is not PCM16/float32");
    // Unreadable or missing answers no (the transcode reports its own
    // failure).
    assert!(!AudioEditService::is_engine_streamable_wav(&path_of(
        dir.path(),
        "missing.wav"
    )));
}

#[test]
fn split_and_concat_reject_a_compressed_adpcm_wav_instead_of_byte_splicing_it() {
    // A structurally valid RIFF/WAVE whose fmt tag is MS ADPCM (2): its
    // blockAlign is a codec block, not a PCM frame, so byte-splicing would
    // produce silent nonsense. It must hit the "compressed audio" path.
    let dir = tempfile::tempdir().unwrap();
    let adpcm = wav_with_fmt(
        2, // WAVE_FORMAT_ADPCM
        1,
        8000,
        256, // a typical ADPCM codec-block size
        4,
        &[0x55; 4096],
    );
    let path = path_of(dir.path(), "adpcm.wav");
    fs::write(&path, adpcm).unwrap();

    let edit = service(dir.path());
    assert!(is_format(&edit.split_wav(PRACTICE, &path, 100)));
    assert!(is_format(&edit.concat_wav(PRACTICE, &path, &path)));
}

#[test]
fn split_wav_rejects_an_extensible_wav_whose_sub_format_is_not_pcm_or_float() {
    // WAVE_FORMAT_EXTENSIBLE wrapping a compressed SubFormat (ADPCM = 2) must
    // be rejected just like a plainly-tagged compressed WAV.
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = extensible_wav_pcm16(&[1000i16; 4000], 8000);
    // Patch the SubFormat's leading format code (fmt body offset 24; fmt body
    // starts at byte 20 in this fixed-layout file) from PCM (1) to ADPCM (2).
    bytes[20 + 24] = 2;
    let path = path_of(dir.path(), "ext_adpcm.wav");
    fs::write(&path, bytes).unwrap();

    let edit = service(dir.path());
    assert!(is_format(&edit.split_wav(PRACTICE, &path, 100)));
}

#[test]
fn split_wav_rejects_a_header_whose_block_align_lies_about_the_frame_size() {
    // Tag says PCM, but blockAlign != channels * bytes/sample: splitting on
    // such a header would shear every frame after the cut.
    let dir = tempfile::tempdir().unwrap();
    let bad = wav_with_fmt(
        1, 1, 8000, 3, /* PCM16 mono must be 2 */
        16, &[0; 4000],
    );
    let path = path_of(dir.path(), "bad.wav");
    fs::write(&path, bad).unwrap();

    let edit = service(dir.path());
    assert!(is_format(&edit.split_wav(PRACTICE, &path, 100)));
}

#[test]
fn split_wav_still_accepts_an_ieee_float_wav() {
    // The compressed-format rejection must not over-tighten: 32-bit float
    // WAVs (format 3) are uncompressed and stay byte-spliceable.
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000usize;
    let data: Vec<u8> = (0..sr)
        .flat_map(|i| (((i as f64 / sr as f64) * 2.0 - 1.0) as f32).to_le_bytes())
        .collect();
    let float = wav_with_fmt(3, 1, sr as u32, 4, 32, &data);
    let path = path_of(dir.path(), "float.wav");
    fs::write(&path, float).unwrap();

    let edit = service(dir.path());
    let parts = edit.split_wav(PRACTICE, &path, 250).unwrap();
    // 250 ms at 8 kHz = 2000 frames * 4 bytes. Data chunk = bytes 44..end for
    // this fixed canonical layout.
    let first = fs::read(&parts.first_path).unwrap();
    let second = fs::read(&parts.second_path).unwrap();
    assert_eq!(first.len() - 44, 2000 * 4);
    assert_eq!(second.len() - 44, (sr - 2000) * 4);
    // The float samples survive the byte splice untouched.
    let f32_at = |i: usize| f32::from_le_bytes(first[44 + i * 4..48 + i * 4].try_into().unwrap());
    assert!((f32_at(0) - -1.0).abs() < 1e-6);
    assert!((f64::from(f32_at(1999)) - ((1999.0 / sr as f64) * 2.0 - 1.0)).abs() < 1e-6);
}

#[test]
fn split_outputs_of_an_odd_fmt_size_wav_keep_riff_word_alignment_and_round_trip() {
    // The re-emit paths preserve the source's fmt body verbatim. When that
    // body has an odd length, the RIFF spec requires a pad byte before the
    // next chunk — without it every later chunk sits at an odd offset, so
    // the service's own walkers (and symphonia's) never find `data` and the
    // split silently turns a playable clip into two unreadable files.
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000;
    let samples: Vec<i16> = (0..sr).map(|i| (i - 4000) as i16).collect();
    let src = path_of(dir.path(), "odd.wav");
    fs::write(&src, odd_fmt_wav_pcm16(&samples, sr as u32)).unwrap();

    let edit = service(dir.path());
    let parts = edit.split_wav(PRACTICE, &src, 250).unwrap();
    for path in [&parts.first_path, &parts.second_path] {
        let bytes = fs::read(path).unwrap();
        // RIFF(12) + fmt header(8) + fmt body(17) + pad(1) = 38.
        assert_eq!(
            &bytes[38..42],
            b"data",
            "the data id must sit at the word-aligned offset"
        );
        assert!(
            read_wav_mono(&bytes).is_some(),
            "an emitted half must re-parse"
        );
    }
    assert_eq!(
        mono_samples(&fs::read(&parts.first_path).unwrap()).len(),
        2000
    );

    // Full round-trip: merging the halves re-parses them and the merged file
    // re-parses in turn, with the audio intact.
    let merged = edit
        .concat_wav(PRACTICE, &parts.first_path, &parts.second_path)
        .unwrap();
    let round = read_wav_mono(&fs::read(&merged).unwrap()).expect("the merged file must re-parse");
    assert_eq!(round.samples.len(), sr as usize);
    for i in (0..sr as usize).step_by(137) {
        assert_eq!(
            (f64::from(round.samples[i]) * 32768.0).round() as i16,
            samples[i]
        );
    }
}

#[test]
fn trim_wav_head_in_place_keeps_an_odd_fmt_size_wav_word_aligned_so_it_stays_trimmable() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<i16> = (0..1000).collect();
    let path = path_of(dir.path(), "odd.wav");
    fs::write(&path, odd_fmt_wav_pcm16(&samples, 1000)).unwrap();

    trim_wav_head_in_place(&path, 250).unwrap();
    let pcm =
        read_wav_mono(&fs::read(&path).unwrap()).expect("the re-emitted header must re-parse");
    assert_eq!(pcm.samples.len(), 750);
    assert_eq!((f64::from(pcm.samples[0]) * 32768.0).round() as i64, 250);

    // A second trim parses the first trim's own output.
    trim_wav_head_in_place(&path, 250).unwrap();
    let again = read_wav_mono(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(again.samples.len(), 500);
    assert_eq!((f64::from(again.samples[0]) * 32768.0).round() as i64, 500);
}

#[test]
fn edit_outputs_land_under_an_edits_subdirectory_never_in_the_recovery_scanned_root() {
    // Boot recovery adopts orphan recording-shaped WAVs lying DIRECTLY under
    // audio/<pid>/; edit outputs written there would be resurrected as
    // phantom "(recovered)" takes after a kill between the file write and the
    // save commit. Split/concat/duplicate must therefore stage one level
    // down, under edits/ (still importable, still swept at boot).
    let dir = tempfile::tempdir().unwrap();
    let sr = 8000;
    let samples: Vec<f64> = (0..sr).map(|i| f64::from(i % 100) / 100.0 - 0.5).collect();
    let src = path_of(dir.path(), "src.wav");
    fs::write(&src, write_wav_mono(&samples, i64::from(sr))).unwrap();

    let edit = service(dir.path());
    let parts = edit.split_wav(PRACTICE, &src, 500).unwrap();
    let merged = edit
        .concat_wav(PRACTICE, &parts.first_path, &parts.second_path)
        .unwrap();
    let copy = edit.duplicate(PRACTICE, &src).unwrap();

    for path in [&parts.first_path, &parts.second_path, &merged, &copy] {
        assert_eq!(
            Path::new(path).parent().unwrap(),
            dir.path().join("edits"),
            "edit outputs must stage under edits/, not the staging root"
        );
        assert!(Path::new(path).exists());
    }
    // A duplicate keeps its source's extension (a bare name gets .wav).
    let song = path_of(dir.path(), "song.m4a");
    fs::write(&song, b"m4a bytes").unwrap();
    let dup = edit.duplicate(PRACTICE, &song).unwrap();
    assert!(dup.ends_with(".m4a"), "{dup}");
    assert_eq!(fs::read(&dup).unwrap(), b"m4a bytes");
    let bare = path_of(dir.path(), "noext");
    fs::write(&bare, b"x").unwrap();
    assert!(edit.duplicate(PRACTICE, &bare).unwrap().ends_with(".wav"));
}

#[test]
fn split_wav_rejects_non_wav_audio() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = path_of(dir.path(), "song.m4a");
    fs::write(&bogus, [7u8; 2048]).unwrap();

    let edit = service(dir.path());
    assert!(is_format(&edit.split_wav(PRACTICE, &bogus, 100)));
}

#[test]
fn split_wav_refuses_a_take_too_short_to_split() {
    // Under two frames there is no frame for each side.
    let dir = tempfile::tempdir().unwrap();
    let path = path_of(dir.path(), "one.wav");
    fs::write(&path, write_wav_mono(&[0.1f64; 1], 8000)).unwrap();
    match service(dir.path()).split_wav(PRACTICE, &path, 0) {
        Err(AudioEditError::Format(m)) => assert_eq!(m, "This take is too short to split."),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_take_whose_file_is_not_on_this_device_is_called_missing_not_compressed() {
    let dir = tempfile::tempdir().unwrap();
    let wav = path_of(dir.path(), "take.wav");
    fs::write(&wav, write_wav_mono(&[0.1f64; 100], 8000)).unwrap();
    // A synced clip whose bytes haven't arrived is an asset:// reference.
    let missing = "asset://0123456789abcdef";
    let edit = service(dir.path());
    let says_missing = |r: Result<String>| match r {
        Err(AudioEditError::Format(m)) => assert!(m.contains("isn't on this device"), "{m}"),
        other => panic!("{other:?}"),
    };

    says_missing(edit.split_wav(PRACTICE, missing, 100).map(|p| p.first_path));
    for (first, second) in [(missing, wav.as_str()), (wav.as_str(), missing)] {
        says_missing(edit.concat_wav(PRACTICE, first, second));
    }
    // A directory is no file either.
    says_missing(
        edit.split_wav(PRACTICE, &dir.path().to_string_lossy(), 100)
            .map(|p| p.first_path),
    );
}

fn take_with_ramp(dir: &Path, n: i16) -> (String, Vec<u8>) {
    // n frames @ 1 kHz mono PCM16: sample i has value i, so the content after
    // a trim must start at the trimmed frame (placement correctness is
    // exactly "which frame is now file offset 0").
    let samples: Vec<i16> = (0..n).collect();
    let bytes = wav_with_fmt(1, 1, 1000, 2, 16, &pcm16(&samples));
    let path = path_of(dir, "take.wav");
    fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

#[test]
fn trim_cuts_exactly_the_head_frames_and_preserves_the_sample_format() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = take_with_ramp(dir.path(), 1000);

    trim_wav_head_in_place(&path, 250).unwrap();

    let pcm = read_wav_mono(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(pcm.samples.len(), 750);
    assert_eq!((f64::from(pcm.samples[0]) * 32768.0).round() as i64, 250);
    assert_eq!(
        (f64::from(*pcm.samples.last().unwrap()) * 32768.0).round() as i64,
        999
    );
    // Nothing is left beside it.
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("take.wav")]);
}

#[test]
fn a_trim_past_the_end_leaves_an_empty_but_valid_wav() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = take_with_ramp(dir.path(), 100);

    trim_wav_head_in_place(&path, 5000).unwrap();

    let pcm = read_wav_mono(&fs::read(&path).unwrap()).expect("valid");
    assert_eq!(pcm.samples.len(), 0);
}

#[test]
fn zero_and_negative_trims_are_no_ops() {
    let dir = tempfile::tempdir().unwrap();
    let (path, bytes) = take_with_ramp(dir.path(), 100);

    trim_wav_head_in_place(&path, 0).unwrap();
    trim_wav_head_in_place(&path, -10).unwrap();

    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn trim_rejects_non_wav_audio_instead_of_corrupting_it() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = path_of(dir.path(), "song.m4a");
    let junk = [7u8; 2048];
    fs::write(&bogus, junk).unwrap();

    assert!(is_format(&trim_wav_head_in_place(&bogus, 100)));
    assert_eq!(fs::read(&bogus).unwrap(), junk, "left untouched");
    // And a missing take is a file system failure, not a format.
    assert!(matches!(
        trim_wav_head_in_place(&path_of(dir.path(), "gone.wav"), 100),
        Err(AudioEditError::Io(_))
    ));
}

// --- wav_malformed_input_test.dart: the head trim's half ----------------------
//
// Every WAV byte the app parses came from somewhere it does not control. The
// contract this pins: a malformed file is REJECTED (a format refusal), never
// a crash, a hang, or an out-of-range read. A header that lies about its
// sizes is the interesting case — the declared length must never be trusted
// past the real end of the file.

/// A canonical mono float32 WAV, the exact shape the recorder writes.
fn good_wav(frames: u32, rate: u32) -> Vec<u8> {
    let (bits, channels) = (32u16, 1u16);
    let block_align = channels * bits / 8;
    let data_bytes = frames * u32::from(block_align);
    let mut b = Bytes::default();
    b.str("RIFF")
        .u32(36 + data_bytes)
        .str("WAVE")
        .str("fmt ")
        .u32(16)
        .u16(3) // IEEE float
        .u16(channels)
        .u32(rate)
        .u32(rate * u32::from(block_align))
        .u16(block_align)
        .u16(bits)
        .str("data")
        .u32(data_bytes)
        .bytes(&vec![0; data_bytes as usize]);
    b.0
}

/// The head trim on one file: it may reject, but never fail with anything
/// other than a format refusal, and never hang.
fn trim_rejects_or_trims(dir: &Path, name: &str, bytes: &[u8]) {
    let path = path_of(dir, name);
    fs::write(&path, bytes).unwrap();
    match trim_wav_head_in_place(&path, 5) {
        Ok(()) | Err(AudioEditError::Format(_)) => {}
        Err(other) => panic!("{name}: {other:?}"),
    }
}

#[test]
fn the_trim_takes_a_canonical_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = path_of(dir.path(), "good.wav");
    fs::write(&path, good_wav(240, 48000)).unwrap();
    trim_wav_head_in_place(&path, 5).unwrap();
    // 5 ms at 48 kHz = 240 frames: all of it.
    assert_eq!(fs::metadata(&path).unwrap().len(), 44);
}

#[test]
fn truncation_at_every_prefix_length_is_rejected_never_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let good = good_wav(240, 48000);
    // Every boundary the chunk walk can land on: inside RIFF, inside fmt, at
    // the data header, and part way through the payload.
    for n in [0, 1, 4, 11, 12, 15, 20, 36, 40, 43, 44, 45, 100] {
        trim_rejects_or_trims(dir.path(), &format!("trunc{n}.wav"), &good[..n]);
    }
}

#[test]
fn a_header_that_lies_about_its_sizes_cannot_read_past_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let put32 =
        |b: &mut Vec<u8>, off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());

    // data size far beyond the file
    let mut huge = good_wav(240, 48000);
    put32(&mut huge, 40, 0xFFFF_FFF0);
    trim_rejects_or_trims(dir.path(), "huge-data.wav", &huge);
    // RIFF size far beyond the file
    let mut huge_riff = good_wav(240, 48000);
    put32(&mut huge_riff, 4, 0xFFFF_FFF0);
    trim_rejects_or_trims(dir.path(), "huge-riff.wav", &huge_riff);
    // fmt chunk claiming a gigantic size (the walk must not seek off the end)
    let mut huge_fmt = good_wav(240, 48000);
    put32(&mut huge_fmt, 16, 0xFFFF_FFF0);
    trim_rejects_or_trims(dir.path(), "huge-fmt.wav", &huge_fmt);
    // zero-size chunks: a naive walk advances by 8 forever
    let mut zero_fmt = good_wav(240, 48000);
    put32(&mut zero_fmt, 16, 0);
    trim_rejects_or_trims(dir.path(), "zero-fmt.wav", &zero_fmt);
}

#[test]
fn impossible_format_fields_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let put16 =
        |b: &mut Vec<u8>, off: usize, v: u16| b[off..off + 2].copy_from_slice(&v.to_le_bytes());
    let put32 =
        |b: &mut Vec<u8>, off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("zero-ch.wav", {
            let mut b = good_wav(240, 48000);
            put16(&mut b, 22, 0);
            b
        }),
        ("zero-rate.wav", {
            let mut b = good_wav(240, 48000);
            put32(&mut b, 24, 0);
            b
        }),
        ("zero-bits.wav", {
            let mut b = good_wav(240, 48000);
            put16(&mut b, 34, 0);
            b
        }),
        // blockAlign inconsistent with channels * bits (the PCM-frame check)
        ("bad-align.wav", {
            let mut b = good_wav(240, 48000);
            put16(&mut b, 32, 7);
            b
        }),
        // a compressed codec tag
        ("mp3-tag.wav", {
            let mut b = good_wav(240, 48000);
            put16(&mut b, 20, 0x0055); // MPEG layer 3
            b
        }),
    ];
    for (name, bytes) in cases {
        let path = path_of(dir.path(), name);
        fs::write(&path, &bytes).unwrap();
        assert!(is_format(&trim_wav_head_in_place(&path, 5)), "{name}");
        assert_eq!(fs::read(&path).unwrap(), bytes, "{name} left untouched");
    }
}

#[test]
fn a_wall_of_chunks_does_not_make_the_walk_run_away() {
    // 4000 empty 'JUNK' chunks before 'data': every parser bounds its walk,
    // so this must terminate (rejecting is fine) rather than scan forever.
    let dir = tempfile::tempdir().unwrap();
    let mut b = Bytes::default();
    b.str("RIFF").u32(0).str("WAVE");
    for _ in 0..4000 {
        b.str("JUNK").u32(0);
    }
    trim_rejects_or_trims(dir.path(), "junk-wall.wav", &b.0);
}
