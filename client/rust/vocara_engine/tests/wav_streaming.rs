//! Streaming WAV header patching: a hard-killed recording must stay parseable.
//!
//! `WavStream` patches the RIFF + data sizes in place about once per second of audio
//! (`sync_header_if_due`, driven by the recording writer thread), so a force quit /
//! crash / `kill -9` — where `finalize` never runs and the `BufWriter` tail is lost —
//! leaves a valid WAV covering everything up to the last patch. These tests simulate
//! the kill with `std::mem::forget` (no destructor, no flush, no finalize), which is
//! the closest in-process equivalent of the process dying mid-record.

use std::fs;
use std::path::PathBuf;

use vocara_engine::wav::WavStream;

fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn le_i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

/// A unique temp path for one test (the leaked-on-purpose stream keeps its fd open,
/// which is fine on unix: the parse reads the path independently).
fn temp_wav(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vocara_wav_stream_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Asserts the canonical 44-byte header written by `WavStream` and returns the data size.
fn assert_valid_header(bytes: &[u8], channels: u16, sample_rate: u32) -> u32 {
    assert!(bytes.len() >= 44, "file shorter than a WAV header");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(le_u32(bytes, 16), 16, "fmt chunk size");
    assert_eq!(le_u16(bytes, 20), 1, "PCM format tag");
    assert_eq!(le_u16(bytes, 22), channels);
    assert_eq!(le_u32(bytes, 24), sample_rate);
    let block_align = channels * 2;
    assert_eq!(le_u32(bytes, 28), sample_rate * u32::from(block_align));
    assert_eq!(le_u16(bytes, 32), block_align);
    assert_eq!(le_u16(bytes, 34), 16, "bits per sample");
    assert_eq!(&bytes[36..40], b"data");
    let data_size = le_u32(bytes, 40);
    assert_eq!(le_u32(bytes, 4), 36 + data_size, "RIFF size tracks data size");
    assert!(
        bytes.len() as u64 >= 44 + u64::from(data_size),
        "header must never claim more bytes than the file holds"
    );
    data_size
}

/// The same 16-bit quantization `WavStream::write_sample` applies.
fn pcm16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[test]
fn hard_kill_leaves_valid_wav_covering_samples_up_to_last_patch() {
    // A tiny sample rate keeps "1 second of audio" (= the patch cadence) small.
    const SR: u32 = 1000;
    let path = temp_wav("hard_kill.wav");
    let mut w = WavStream::create(&path, 1, SR).unwrap();

    // Drive the stream exactly like writer_run: drain batches of samples, then give the
    // periodic patch a chance after each batch. 25 batches x 100 samples = 2500 samples,
    // so the patch fires at 1000 and again at 2000 samples.
    for batch in 0..25u32 {
        for i in 0..100u32 {
            let n = batch * 100 + i;
            w.write_sample((n % 100) as f32 / 200.0).unwrap();
        }
        w.sync_header_if_due().unwrap();
    }
    // A post-batch tail that never reaches another patch (and stays inside the BufWriter).
    for _ in 0..30 {
        w.write_sample(0.25).unwrap();
    }
    // The hard kill: no finalize, no drop glue, the buffered tail is lost.
    std::mem::forget(w);

    let bytes = fs::read(&path).unwrap();
    let data_size = assert_valid_header(&bytes, 1, SR);
    // Everything up to the LAST patch (2000 samples) is covered by the header...
    assert_eq!(data_size, 2000 * 2);
    // ...and was actually flushed: the patch seeks flush the BufWriter, and nothing after
    // the last patch was flushed, so the file ends exactly at the patched payload.
    assert_eq!(bytes.len() as u64, 44 + u64::from(data_size));
    // The covered payload decodes to the samples that were written (spot-check the ramp).
    for &n in &[0usize, 50, 199, 1500, 1999] {
        let expect = pcm16((n % 100) as f32 / 200.0);
        assert_eq!(le_i16(&bytes, 44 + n * 2), expect, "sample {n}");
    }
}

#[test]
fn kill_before_first_patch_leaves_a_valid_empty_wav() {
    const SR: u32 = 1000;
    let path = temp_wav("early_kill.wav");
    let mut w = WavStream::create(&path, 1, SR).unwrap();
    // Under one second of audio: the cadence check must NOT patch yet.
    for _ in 0..10 {
        w.write_sample(0.5).unwrap();
    }
    w.sync_header_if_due().unwrap();
    std::mem::forget(w);

    let bytes = fs::read(&path).unwrap();
    // Still a parseable WAV — just an empty one (the boot sweep deletes empty orphans).
    let data_size = assert_valid_header(&bytes, 1, SR);
    assert_eq!(data_size, 0);
}

#[test]
fn finalize_still_stamps_exact_sizes_after_periodic_patches() {
    const SR: u32 = 1000;
    let path = temp_wav("finalized.wav");
    let mut w = WavStream::create(&path, 1, SR).unwrap();
    // 2530 samples with the same batch/patch rhythm as the writer thread, then a
    // graceful stop: finalize must cover the FULL payload, not just the last patch.
    for batch in 0..23u32 {
        for _ in 0..110 {
            w.write_sample(-0.125).unwrap();
        }
        let _ = batch;
        w.sync_header_if_due().unwrap();
    }
    w.finalize().unwrap();

    let bytes = fs::read(&path).unwrap();
    let data_size = assert_valid_header(&bytes, 1, SR);
    assert_eq!(data_size, 23 * 110 * 2);
    assert_eq!(bytes.len() as u64, 44 + u64::from(data_size));
    assert_eq!(le_i16(&bytes, 44), pcm16(-0.125));
    assert_eq!(le_i16(&bytes, 44 + (23 * 110 - 1) * 2), pcm16(-0.125));
}
