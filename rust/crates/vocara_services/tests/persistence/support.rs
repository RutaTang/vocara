//! Helpers shared by the Persistence suites.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vocara_model::models::song_practice::SongPractice;
use vocara_services::audio_dsp::write_wav_mono;
use vocara_services::persistence::{OpenError, OpenOptions, Persistence, WavAnalyzer};
use vocara_services::waveform_service::WaveformResult;
use vocara_store::db::library_lock::LibraryGuard;
use vocara_store::repositories::SongPracticeRepository;

/// The analyzer a real recovery would run through the waveform service;
/// tests stand in a fixed result (the engine's decoder isn't available
/// here).
pub fn fake_analyze() -> WavAnalyzer {
    Arc::new(|_| {
        Ok(WaveformResult {
            peaks: vec![0.1, 0.2],
            duration_ms: 500,
        })
    })
}

/// An analyzer that always fails (a systemic failure: a broken decoder or
/// disk), not per-file junk.
pub fn broken_analyze() -> WavAnalyzer {
    Arc::new(|_| {
        Err(vocara_services::waveform_service::WaveformError(
            "decoder down".into(),
        ))
    })
}

/// Mono 16-bit PCM WAV bytes of [samples] ramp samples at [gain] — the exact
/// shape the recording engine writes.
pub fn mono_wav_gain(samples: usize, gain: f64) -> Vec<u8> {
    let ramp: Vec<f64> = (0..samples)
        .map(|i| gain * (i % 100) as f64 / 100.0)
        .collect();
    write_wav_mono(&ramp, 48000)
}

/// [`mono_wav_gain`] at the default gain 0.5.
pub fn mono_wav(samples: usize) -> Vec<u8> {
    mono_wav_gain(samples, 0.5)
}

/// Writes [bytes] to [path], creating its directory.
pub fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// A path as the text the store keeps.
pub fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Opens the on-disk library in [root] with [analyze] and, when given, a
/// fixed guard (Dart's `boot()` helpers).
pub fn boot_with(
    root: &Path,
    analyze: WavAnalyzer,
    guard: Option<Arc<dyn LibraryGuard>>,
) -> Result<Persistence, OpenError> {
    let mut options = OpenOptions::new(root).analyzer(analyze);
    if let Some(guard) = guard {
        options = options.guard(guard);
    }
    Persistence::open(options)
}

/// Opens the on-disk library in [root] with the fake analyzer.
pub fn boot(root: &Path) -> Persistence {
    boot_with(root, fake_analyze(), None).unwrap()
}

/// Creates a practice titled 'Song'.
pub fn create_practice(p: &Persistence) -> SongPractice {
    p.song_repo().create_practice("Song").unwrap()
}

/// The practice [id] as stored.
pub fn reload(p: &Persistence, id: &str) -> SongPractice {
    p.song_repo().get_practice(id).unwrap()
}

/// An in-memory Persistence whose temp root is removed when it drops
/// (Dart's `Persistence.memory()` plus its tearDown).
pub struct Memory {
    p: Option<Persistence>,
    pub root: PathBuf,
}

impl Memory {
    pub fn new() -> Memory {
        let p = Persistence::memory().unwrap();
        let root = p.assets().root().to_path_buf();
        Memory { p: Some(p), root }
    }
}

impl std::ops::Deref for Memory {
    type Target = Persistence;
    fn deref(&self) -> &Persistence {
        self.p.as_ref().unwrap()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        drop(self.p.take());
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `SELECT COUNT(*)` of [sql] with string args.
pub fn count(p: &Persistence, sql: &str, args: &[&str]) -> i64 {
    p.db()
        .query_row(sql, rusqlite::params_from_iter(args.iter()), |r| r.get(0))
        .unwrap()
}

/// Whether this process runs as root, which reads a file whatever its
/// permission bits: a `chmod 000` makes nothing unreadable then.
pub fn runs_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}
