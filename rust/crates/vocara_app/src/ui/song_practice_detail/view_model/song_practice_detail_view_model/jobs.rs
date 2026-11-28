//! The view model's own blocking work: each Dart `await` on a repository,
//! file, edit, separation or engine call is a [`Job`] the screen runs on a
//! worker thread, whose [`JobDone`] comes back to
//! [`SongPracticeDetailViewModel::job_done`](super::SongPracticeDetailViewModel::job_done).

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique::Technique;
use vocara_services::audio_edit_service::{AudioEditError, AudioEdits};
use vocara_services::audio_file_service::{AudioFileError, AudioFiles};
use vocara_services::engine::media::{MediaError, MAX_PLAYBACK_DURATION};
use vocara_services::graph_audio_playback_service::SyncError;
use vocara_services::source_separation_service::{SeparationError, SeparationResult};
use vocara_services::waveform_service::WaveformResult;
use vocara_store::sync::sync_hashes::Loaded;

use crate::util::panic_message;

/// What a failed step threw, sorted as Dart's `_friendlyError` sorted it:
/// a refusal that carries user-presentable copy (a `FormatException`, a
/// `MediaTooLongException` / `MediaUnsupportedException`), or anything else
/// (said as "Something went wrong.").
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Said(String),
    Other(String),
}

impl Failure {
    /// Dart's `_friendlyError(e)`.
    pub(super) fn friendly(&self) -> String {
        match self {
            Failure::Said(message) => message.clone(),
            Failure::Other(_) => SOMETHING_WENT_WRONG.to_string(),
        }
    }

    /// Dart's `e.toString()`, which the busy gate keeps as its error
    /// message.
    pub(super) fn raw(&self) -> String {
        match self {
            Failure::Said(m) | Failure::Other(m) => m.clone(),
        }
    }

    pub(super) fn from_media(e: &MediaError) -> Failure {
        match e {
            // Dart's MediaTooLongException / MediaUnsupportedException (and
            // its MediaReadException subclass) carry their copy.
            MediaError::TooLong { .. } | MediaError::Unsupported(_) | MediaError::Read => {
                Failure::Said(e.to_string())
            }
            MediaError::EngineUnavailable => Failure::Other(e.to_string()),
        }
    }

    pub(super) fn from_files(e: &AudioFileError) -> Failure {
        match e {
            AudioFileError::Format(m) => Failure::Said(m.clone()),
            other => Failure::Other(other.to_string()),
        }
    }

    pub(super) fn from_edit(e: &AudioEditError) -> Failure {
        match e {
            AudioEditError::Format(m) => Failure::Said(m.clone()),
            AudioEditError::Media(m) => Failure::from_media(m),
            other => Failure::Other(other.to_string()),
        }
    }

    pub(super) fn from_separation(e: &SeparationError) -> Failure {
        match e {
            // The sidecar's FormatException messages, verbatim.
            SeparationError::Failed(m) => Failure::Said(m.clone()),
            other => Failure::Other(other.to_string()),
        }
    }

    pub(super) fn from_sync(e: &SyncError) -> Failure {
        Failure::Other(e.to_string())
    }
}

/// Dart's catch-all friendly error.
pub(super) const SOMETHING_WENT_WRONG: &str = "Something went wrong.";

/// What kind of work a [`Job`] does (for the screen's diagnostics and the
/// tests, which hold some kinds back as Dart's tests parked their fakes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// `SongPracticeRepository::load_practice`.
    Load,
    /// `SongPracticeRepository::save_practice_with_base`.
    Save,
    /// A queued engine sync (`MixerController::resync_playback`'s wait).
    Sync,
    /// A wait on what is landing on this practice (the finalizer).
    Landing,
    /// The separation engine's availability probe.
    Probe,
    /// An import's copy, transcode and waveform analysis.
    Import,
    /// Waveform analyses.
    Analyze,
    /// A split's byte cut.
    Split,
    /// A merge's join and its analysis.
    Merge,
    /// A duplicate's file copies.
    Duplicate,
    /// A stem separation.
    Separate,
    /// An export's save panel and copy.
    Export,
    /// Audio file deletes.
    Delete,
    /// A dropped lyrics file's read.
    ReadLyrics,
    /// The technique library's list.
    Techniques,
}

/// A job's result (private: only the view model reads it).
pub(super) enum Output {
    Loaded(Result<Loaded<SongPractice>, String>),
    Saved(Result<Loaded<SongPractice>, String>),
    Synced(Result<(), SyncError>),
    /// Whether nothing was landing by the time the wait ended.
    Settled(bool),
    Available(bool),
    Imported(Result<Imported, Failure>),
    Analyzed(Result<WaveformResult, Failure>),
    Halves(Result<(WaveformResult, WaveformResult), Failure>),
    Split(Result<(String, String), Failure>),
    /// A merge's joined file, and (when it was made) its analysis and the
    /// top clip's exact length.
    Merged(
        Result<String, Failure>,
        Option<Result<(WaveformResult, i64), Failure>>,
    ),
    Duplicated(Duplicated),
    Separated(Result<SeparationResult, SeparationError>),
    Exported(Option<String>),
    Deleted(Result<(), Failure>),
    Lyrics(LyricsRead),
    Techniques(Option<Vec<Technique>>),
    /// The work panicked (Dart's throw of something unexpected).
    Panicked(String),
}

/// An import's prologue (`_withImportedAudio` up to its body's first
/// analysis): the copy in the sandbox, the playable WAV, and that WAV's
/// waveform (or why it failed).
pub(super) struct Imported {
    pub imported: String,
    pub dest: String,
    pub analysis: Result<WaveformResult, Failure>,
}

/// A duplicate's copies: the ones made, by source clip, and the failure
/// that stopped them (if any).
pub(super) struct Duplicated {
    pub copies: Vec<(String, String)>,
    pub failure: Option<Failure>,
}

/// A dropped lyrics file, as read.
pub(super) enum LyricsRead {
    Text(String),
    /// Not a plain file (a device, a pipe, a folder) or unreadable.
    Unreadable,
    TooLarge,
}

/// Blocking work for a worker thread (see the module doc).
pub struct Job {
    pub(super) id: u64,
    kind: JobKind,
    work: Box<dyn FnOnce() -> Output + Send>,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish()
    }
}

impl Job {
    pub(super) fn new(
        id: u64,
        kind: JobKind,
        work: impl FnOnce() -> Output + Send + 'static,
    ) -> Job {
        Job {
            id,
            kind,
            work: Box::new(work),
        }
    }

    pub fn kind(&self) -> JobKind {
        self.kind
    }

    /// Runs the work on the current thread (blocking: the screen runs it on
    /// a worker) and returns its result to hand back. A panic comes back as
    /// a failure of the step, as a Dart throw did.
    pub fn run(self) -> JobDone {
        let output = match catch_unwind(AssertUnwindSafe(self.work)) {
            Ok(output) => output,
            Err(panic) => Output::Panicked(panic_message(panic.as_ref())),
        };
        JobDone {
            id: self.id,
            output,
        }
    }
}

/// A [`Job`]'s result.
pub struct JobDone {
    pub(super) id: u64,
    pub(super) output: Output,
}

impl fmt::Debug for JobDone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobDone").field("id", &self.id).finish()
    }
}

/// A stem separation's best-effort progress, written by the worker and read
/// on the UI thread (see `separation_progress_tick`).
#[derive(Default)]
pub(super) struct ProgressCell(Mutex<Option<f64>>);

impl ProgressCell {
    pub(super) fn set(&self, v: f64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(v);
    }

    pub(super) fn take(&self) -> Option<f64> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The import's admission: refuses a file whose DECLARED duration exceeds
/// the playback cap before anything is copied (the decode-time budget alone
/// would only reject it at playback staging, after it was copied, analyzed
/// and persisted as an unplayable clip). Here, on the import's worker, not
/// on the UI thread: the probe opens and reads the file, which blocks on an
/// iCloud placeholder until it downloads, or on a slow network volume.
/// A probe that can't run (an unreadable file, no engine in unit tests)
/// admits: the decode budget still bounds the file at staging. Containers
/// that declare no length are admitted too.
fn too_long(
    probe: &(dyn Fn(&str) -> Result<Option<Duration>, MediaError> + Send + Sync),
    source_path: &str,
) -> Option<Failure> {
    let declared = catch_unwind(AssertUnwindSafe(|| probe(source_path)))
        .ok()
        .and_then(Result::ok)
        .flatten()?;
    if declared <= MAX_PLAYBACK_DURATION {
        return None;
    }
    // Rounded up, as the limit counts: rounded down, a file just over it
    // read as exactly the limit.
    let minutes = (i64::try_from(declared.as_millis()).unwrap_or(i64::MAX) + 59999) / 60000;
    Some(Failure::Said(format!(
        "That file is {minutes} minutes long. The limit is {} minutes.",
        MAX_PLAYBACK_DURATION.as_secs() / 60
    )))
}

/// `_audioFiles.importAudio`, `_edits.ensureStreamableWav`, the redundant
/// original's delete and the body's first `_waveforms.analyze` of
/// `_withImportedAudio`, in order. `Err` when the import itself failed
/// (nothing was created); otherwise what was created and the analysis.
pub(super) fn import_prepared(
    files: &dyn AudioFiles,
    edits: &dyn AudioEdits,
    analyze: &(dyn Fn(&str) -> Result<WaveformResult, String> + Send + Sync),
    probe: &(dyn Fn(&str) -> Result<Option<Duration>, MediaError> + Send + Sync),
    practice_id: &str,
    source_path: &str,
) -> Result<Imported, Failure> {
    if let Some(refused) = too_long(probe, source_path) {
        return Err(refused);
    }
    let imported = match files.import_audio(practice_id, source_path) {
        Ok(path) => path,
        Err(AudioFileError::Io(e)) => {
            // The source gone or unreadable mid-import (moved, a
            // disconnected drive) — only here: a write failing (a full
            // disk) is not the file's.
            if !Path::new(source_path).exists() {
                return Err(Failure::Said("The file couldn't be read.".into()));
            }
            return Err(Failure::Other(e.to_string()));
        }
        Err(e) => return Err(Failure::from_files(&e)),
    };
    let dest = match edits.ensure_streamable_wav(practice_id, &imported) {
        Ok(dest) => dest,
        Err(e) => {
            return Ok(Imported {
                dest: imported.clone(),
                imported,
                analysis: Err(Failure::from_edit(&e)),
            })
        }
    };
    if dest != imported {
        // The compressed original is now redundant: the WAV is the track.
        if let Err(e) = files.delete_audio(&imported) {
            return Ok(Imported {
                imported,
                dest,
                analysis: Err(Failure::from_files(&e)),
            });
        }
    }
    let analysis = analyze_said(analyze, &dest);
    Ok(Imported {
        imported,
        dest,
        analysis,
    })
}

/// A waveform analysis whose error is its user-presentable message (Dart's
/// `WaveformService.analyze` threw `FormatException`s and the media
/// refusals); a panic is anything else.
pub(super) fn analyze_said(
    analyze: &(dyn Fn(&str) -> Result<WaveformResult, String> + Send + Sync),
    path: &str,
) -> Result<WaveformResult, Failure> {
    match catch_unwind(AssertUnwindSafe(|| analyze(path))) {
        Ok(Ok(w)) => Ok(w),
        Ok(Err(message)) => Err(Failure::Said(message)),
        Err(panic) => Err(Failure::Other(panic_message(panic.as_ref()))),
    }
}

/// Reads a dropped lyrics file: a plain file, no further than `cap` bytes
/// (a link to a device reports no length and was read without end, and a
/// pipe held the import open for good), decoded as UTF-8.
pub(super) fn read_lyrics_file(path: &str, cap: u64) -> LyricsRead {
    use std::io::Read;
    // `FileSystemEntity.type` follows links, as `metadata` does.
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        _ => return LyricsRead::Unreadable,
    }
    let Ok(file) = std::fs::File::open(path) else {
        return LyricsRead::Unreadable;
    };
    let mut bytes = Vec::new();
    if file.take(cap + 1).read_to_end(&mut bytes).is_err() {
        return LyricsRead::Unreadable;
    }
    if bytes.len() as u64 > cap {
        return LyricsRead::TooLarge;
    }
    match String::from_utf8(bytes) {
        Ok(text) => LyricsRead::Text(text),
        Err(_) => LyricsRead::Unreadable,
    }
}

/// Copies every clip file of a lane (`_edits.duplicate`), stopping at the
/// first failure.
pub(super) fn duplicate_clips(
    edits: &dyn AudioEdits,
    practice_id: &str,
    clips: &[(String, String)],
) -> Duplicated {
    let mut copies = Vec::new();
    for (clip_id, path) in clips {
        match catch_unwind(AssertUnwindSafe(|| edits.duplicate(practice_id, path))) {
            Ok(Ok(dest)) => copies.push((clip_id.clone(), dest)),
            Ok(Err(e)) => {
                return Duplicated {
                    copies,
                    failure: Some(Failure::from_edit(&e)),
                }
            }
            Err(panic) => {
                return Duplicated {
                    copies,
                    failure: Some(Failure::Other(panic_message(panic.as_ref()))),
                }
            }
        }
    }
    Duplicated {
        copies,
        failure: None,
    }
}

/// `_audioFiles.deleteAudio(path)`.
pub(super) fn delete(files: &Arc<dyn AudioFiles>, path: &str) -> Result<(), Failure> {
    files
        .delete_audio(path)
        .map_err(|e| Failure::from_files(&e))
}
