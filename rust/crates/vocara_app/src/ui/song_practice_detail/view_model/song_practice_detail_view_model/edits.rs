//! The model's structure: audio imports, source separation, export, the
//! track and clip edits under the busy gate, empty takes, notes and
//! technique markers.
//!
//! A guarded edit takes the busy gate as Dart's `_busyGate.run` did, runs
//! its steps (each file or engine wait a job, each `await _save()` a save on
//! the chain) and hands the gate back at its end, its failure said as the
//! gate's `onError` said it. Its state moves along with it from step to step
//! ([`EditStep`]).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path;
use vocara_model::util::id::new_id;
use vocara_services::audio_exporter::AudioExporter;
use vocara_services::source_separation_service::SeparationError;
use vocara_services::waveform_service::{exact_duration_ms, WaveformResult};

use super::super::playback_controller::PlaybackController;
use super::super::recording_controller::RecordingOp;
use super::jobs::{self, Failure, Output, ProgressCell};
use super::persist::{Pending, SaveKind, SaveThen};
use super::{
    clamp_marker_end, clamp_note_end, normalize_marker_range, uncovered_spans, Call, ClipRiders,
    Effect, JobKind, Reply, SelectionBefore, SongPracticeDetailViewModel, Started,
};

// --- Step state -------------------------------------------------------------------

/// Who asked for an empty take.
pub(super) enum EmptyTakeFor {
    /// REC's take setup, which awaits the take's id.
    Recording(RecordingOp),
    /// The "+ New take" button, which gives the selection back on a failed
    /// save only with no REC arming.
    NewTake,
}

/// An empty take waiting for its save.
pub(super) struct EmptyTake {
    asked: EmptyTakeFor,
    track_id: String,
    before: SelectionBefore,
}

/// Where an import lands.
pub(super) enum ImportKind {
    Accompaniment,
    Reference,
    /// A new take, filed under the interpretation it was dropped into.
    Vocal {
        version_id: Option<String>,
    },
    /// A clip on take `target_id` at `start_ms`.
    Clip {
        target_id: String,
        start_ms: i64,
    },
}

/// An import in flight (`_withImportedAudio` and its body).
pub(super) struct Import {
    kind: ImportKind,
    source: String,
    call: Call,
    imported: String,
    dest: String,
    /// The analysis's duration (a clip's end is its start plus it).
    duration_ms: i64,
    end_ms: Option<i64>,
    /// A replaced backing's files and the kept original it drops.
    replaced: Vec<String>,
}

/// What a failed separation save takes back of what it did, and only where
/// the model still holds it: putting back the whole model from before it
/// dropped what was edited while the save ran, the Original lane's own name,
/// notes and mix included.
pub(super) struct TakeBack {
    instrumental_id: String,
    live_original: Arc<AudioTrack>,
    cur_accompaniment: Option<Arc<AudioTrack>>,
    cur_reference: Option<Arc<AudioTrack>>,
    cur_separation_source: Option<String>,
}

impl TakeBack {
    /// The live model with the separation taken back; `None` when the model
    /// no longer holds it (it is left as it is).
    pub(super) fn apply(&self, live: &SongPractice) -> Option<SongPractice> {
        if live.accompaniment.as_ref().map(|a| a.id.as_str()) != Some(self.instrumental_id.as_str())
        {
            return None;
        }
        let kept = live.reference.clone();
        let (backing, reference) = if self.live_original.is_reference() {
            // The Original stays where it was.
            (self.cur_accompaniment.clone(), kept)
        } else {
            // The Original goes back to the backing slot it came from.
            let backing = match &kept {
                Some(k) if k.id == self.live_original.id => {
                    Arc::new(rekinded(k, AudioTrackKind::Accompaniment))
                }
                _ => Arc::clone(&self.live_original),
            };
            (Some(backing), self.cur_reference.clone())
        };
        Some(SongPractice {
            accompaniment: backing,
            reference,
            separation_source: self.cur_separation_source.clone(),
            ..live.clone()
        })
    }
}

/// `t` as a lane of `kind`: the same lane, every field carried over (Dart's
/// `copyWith` could not change kind, so the constructor was spelled out —
/// which is how pan, mute, solo and the insert chain once came to be dropped
/// while volume and the colour tint were kept).
fn rekinded(t: &AudioTrack, kind: AudioTrackKind) -> AudioTrack {
    AudioTrack { kind, ..t.clone() }
}

/// A separation in flight.
pub(super) struct Separation {
    original_id: String,
    original_path: String,
    imported: String,
    dest: String,
    old_accompaniment: Option<Arc<AudioTrack>>,
    old_reference: Option<Arc<AudioTrack>>,
    prev_play_reference: bool,
    prev_selection: (Option<String>, Option<String>, Option<String>, u64),
}

/// A split in flight.
pub(super) struct Split {
    track_id: String,
    clip: AudioClip,
    at_ms: i64,
    first: String,
    second: String,
    first_clip_id: String,
    follow_selection: bool,
    moved_keys: Vec<(String, String)>,
}

/// A merge in flight.
pub(super) struct Merge {
    track_id: String,
    top_id: String,
    bottom_id: String,
    top_clip: AudioClip,
    bottom_clip: AudioClip,
    merged: String,
    merged_clip_id: String,
    moved_track: bool,
    moved_clip: bool,
    prior_clip: Option<String>,
    moved_keys: Vec<(String, String)>,
}

/// How a run of deletes treats a failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeleteMode {
    /// It throws out of the edit (Dart's bare `await deleteAudio`).
    Propagate,
    /// It ends the run, swallowed (one `try` around the run).
    StopQuietly,
    /// It is skipped (a `try` per file: best-effort).
    Continue,
}

/// One delete of a run, with the checks Dart made right before it.
pub(super) struct Doomed {
    path: String,
    /// Not while the model still plays it.
    skip_if_held: bool,
    /// Drop its analysis first, once no lane plays it.
    evict_unused: bool,
}

impl Doomed {
    fn plain(path: impl Into<String>) -> Doomed {
        Doomed {
            path: path.into(),
            skip_if_held: false,
            evict_unused: false,
        }
    }

    fn unless_held(path: impl Into<String>) -> Doomed {
        Doomed {
            skip_if_held: true,
            ..Doomed::plain(path)
        }
    }

    fn evicting(path: impl Into<String>) -> Doomed {
        Doomed {
            evict_unused: true,
            ..Doomed::plain(path)
        }
    }
}

/// Where an edit waits.
pub(super) enum EditStep {
    // Imports.
    ImportPrepared(Box<Import>),
    ImportSaved(Box<Import>),
    ImportReplacedDeleted(Box<Import>),
    ImportCleanedUp(Box<Import>, Result<(), Failure>),
    // Separation.
    Separated(Box<Separation>),
    SeparationImported(Box<Separation>),
    SeparationSaved(Box<Separation>),
    SeparationRevertResynced(Box<Separation>),
    SeparationOldDeleted,
    SeparationCleanedUp(Result<(), Failure>),
    // Structural edits.
    DeleteTrackSaved {
        track_id: String,
        doomed: Vec<String>,
        stale: Option<String>,
    },
    DeleteTrackFilesDeleted,
    DeleteClipSaved {
        track_id: String,
        path: String,
    },
    DeleteClipFileDeleted {
        track_id: String,
    },
    DuplicateCopied {
        track_id: String,
        clips: Vec<AudioClip>,
    },
    DuplicateSaved {
        copies: Vec<String>,
    },
    DuplicateDiscarded(Result<(), Failure>),
    SplitCut(Box<Split>),
    SplitAnalyzed(Box<Split>),
    SplitSaved(Box<Split>),
    SplitOriginalDeleted(Box<Split>),
    SplitDiscarded(Box<Split>, Result<(), Failure>),
    MergeJoined(Box<Merge>),
    MergeSaved(Box<Merge>),
    MergeSourcesDeleted(Box<Merge>),
    MergeDiscarded(Box<Merge>, Result<(), Failure>),
    ReanalyzeAnalyzed {
        track_id: String,
        clip_id: String,
        path: String,
        queue: VecDeque<(String, String)>,
        analyzed: HashMap<String, WaveformResult>,
    },
    ReanalyzeSaved {
        track_id: String,
    },
    DiscardEmptySaved {
        op: RecordingOp,
        track_id: String,
        before: Option<SelectionBefore>,
    },
    /// A run of deletes, then `after` resumes with its outcome.
    Deleting {
        queue: VecDeque<Doomed>,
        mode: DeleteMode,
        after: Box<EditStep>,
    },
}

fn failure_of(output: &Output) -> Option<Failure> {
    match output {
        Output::Panicked(m) => Some(Failure::Other(m.clone())),
        _ => None,
    }
}

impl SongPracticeDetailViewModel {
    // --- Deletes -----------------------------------------------------------------------

    /// Runs `queue`'s deletes one after another (each with its checks made
    /// as its turn comes, as Dart's loop made them between its awaits), then
    /// resumes `after` with the run's outcome.
    fn run_deletes(&mut self, mut queue: VecDeque<Doomed>, mode: DeleteMode, after: EditStep) {
        while let Some(item) = queue.pop_front() {
            if item.skip_if_held && self.c.holds_audio(&item.path) {
                continue;
            }
            if item.evict_unused {
                self.evict_unused(&item.path);
            }
            let files = Arc::clone(&self.c.audio_files);
            let path = item.path;
            self.c.job(
                JobKind::Delete,
                Pending::Edit(Box::new(EditStep::Deleting {
                    queue,
                    mode,
                    after: Box::new(after),
                })),
                move || Output::Deleted(jobs::delete(&files, &path)),
            );
            return;
        }
        self.edit_resume(after, Ok(()));
    }

    /// Deletes working files an abandoned edit left behind; a lingering one
    /// is harmless (the boot sweep reclaims `edits/`), so failures are
    /// swallowed. Never a file the model still plays: a failed save does not
    /// revert a model an edit moved on since, so a split's halves or a
    /// merge's output can stay in it — deleted, the lane pointed at nothing
    /// and the next save failed on it too (as an import is kept).
    fn discard_quietly(&mut self, paths: Vec<String>, after: EditStep) {
        let queue = paths.into_iter().map(Doomed::unless_held).collect();
        self.run_deletes(queue, DeleteMode::Continue, after);
    }

    // --- Dispatch ----------------------------------------------------------------------

    pub(super) fn edit_job_done(&mut self, step: EditStep, output: Output) {
        match step {
            EditStep::Deleting { queue, mode, after } => {
                let result = match output {
                    Output::Deleted(r) => r,
                    other => Err(failure_of(&other)
                        .unwrap_or_else(|| Failure::Other("unexpected job output".into()))),
                };
                match result {
                    Err(f) if mode == DeleteMode::Propagate => self.edit_resume(*after, Err(f)),
                    Err(_) if mode == DeleteMode::StopQuietly => self.edit_resume(*after, Ok(())),
                    _ => self.run_deletes(queue, mode, *after),
                }
            }
            EditStep::ImportPrepared(import) => self.import_prepared(*import, output),
            EditStep::Separated(sep) => self.separated(*sep, output),
            EditStep::SeparationImported(sep) => self.separation_imported(*sep, output),
            EditStep::DuplicateCopied { track_id, clips } => {
                self.duplicate_copied(&track_id, &clips, output);
            }
            EditStep::SplitCut(split) => self.split_cut(*split, output),
            EditStep::SplitAnalyzed(split) => self.split_analyzed(*split, output),
            EditStep::MergeJoined(merge) => self.merge_joined(*merge, output),
            EditStep::ReanalyzeAnalyzed {
                track_id,
                clip_id,
                path,
                queue,
                mut analyzed,
            } => match output {
                Output::Analyzed(Ok(w)) => {
                    analyzed.insert(clip_id, w);
                    // Same path, possibly new bytes: redo a compute still in
                    // flight.
                    self.analysis.evict_analysis(&path, true);
                    self.reanalyze_next(track_id, queue, analyzed);
                }
                Output::Analyzed(Err(f)) => self.reanalyze_finished(&track_id, Err(f)),
                other => {
                    let f = failure_of(&other)
                        .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                    self.reanalyze_finished(&track_id, Err(f));
                }
            },
            // Steps resumed by a save, a resync or deletes: not by a job.
            other => self.edit_resume(other, Err(Failure::Other("unexpected job".into()))),
        }
    }

    pub(super) fn edit_saved(&mut self, step: EditStep, result: Result<bool, Failure>) {
        match step {
            EditStep::ImportSaved(import) => self.import_saved(*import, result),
            EditStep::SeparationSaved(sep) => self.separation_saved(*sep, result),
            EditStep::DeleteTrackSaved {
                track_id,
                doomed,
                stale,
            } => match result {
                Ok(true) => {
                    self.analysis.drop_lane_state(&track_id);
                    let mut queue: VecDeque<Doomed> =
                        doomed.into_iter().map(Doomed::evicting).collect();
                    if let Some(stale) = stale {
                        queue.push_back(Doomed::unless_held(stale));
                    }
                    self.run_deletes(
                        queue,
                        DeleteMode::Propagate,
                        EditStep::DeleteTrackFilesDeleted,
                    );
                }
                Ok(false) => self.c.finish_gate(Ok(())),
                Err(f) => self.c.finish_gate(Err(f)),
            },
            EditStep::DeleteClipSaved { track_id, path } => match result {
                Ok(true) => self.run_deletes(
                    VecDeque::from([Doomed::evicting(path)]),
                    DeleteMode::Propagate,
                    EditStep::DeleteClipFileDeleted { track_id },
                ),
                Ok(false) => {
                    self.analysis_call(|a, h| a.on_track_audio_changed(&track_id, h));
                    self.c.finish_gate(Ok(()));
                }
                Err(f) => self.c.finish_gate(Err(f)),
            },
            EditStep::DuplicateSaved { copies } => match result {
                Ok(true) => {
                    self.c.toast("Take duplicated.");
                    self.c.finish_gate(Ok(()));
                }
                Ok(false) => {
                    // The save reverted the model; the copied files would sit
                    // orphaned in edits/ until the boot sweep otherwise.
                    let queue = copies.into_iter().map(Doomed::unless_held).collect();
                    self.run_deletes(
                        queue,
                        DeleteMode::Propagate,
                        EditStep::DuplicateDiscarded(Ok(())),
                    );
                }
                Err(f) => self.c.finish_gate(Err(f)),
            },
            EditStep::SplitSaved(split) => self.split_saved(*split, result),
            EditStep::MergeSaved(merge) => self.merge_saved(*merge, result),
            EditStep::ReanalyzeSaved { track_id } => {
                self.reanalyze_finished(&track_id, result.map(|_| ()));
            }
            EditStep::DiscardEmptySaved {
                op,
                track_id,
                before,
            } => {
                let outcome = match result {
                    Ok(saved) => {
                        if saved {
                            self.analysis.drop_lane_state(&track_id);
                        }
                        Ok(())
                    }
                    Err(f) => Err(f),
                };
                self.c.finish_gate(outcome);
                self.empty_take_discard_done(op, &track_id, before);
            }
            other => self.edit_resume(other, Err(Failure::Other("unexpected save".into()))),
        }
    }

    pub(super) fn edit_resynced(&mut self, step: EditStep, result: Result<(), Failure>) {
        match step {
            EditStep::SeparationRevertResynced(sep) => {
                self.separation_revert_resynced(*sep, result)
            }
            other => self.edit_resume(other, result),
        }
    }

    /// A run of deletes (or another wait) ended: the step after it.
    fn edit_resume(&mut self, step: EditStep, result: Result<(), Failure>) {
        match step {
            EditStep::ImportReplacedDeleted(import) => match result {
                // Committed: nothing to clean up.
                Ok(()) => self.import_finished(*import, Ok(())),
                Err(f) => self.import_clean_up(*import, Err(f)),
            },
            EditStep::ImportCleanedUp(import, outcome) => self.import_finished(*import, outcome),
            EditStep::SeparationOldDeleted => self.separation_finished(Ok(())),
            EditStep::SeparationCleanedUp(outcome) => self.separation_finished(outcome),
            EditStep::DeleteTrackFilesDeleted | EditStep::DuplicateDiscarded(Ok(())) => {
                self.c.finish_gate(result);
            }
            EditStep::DuplicateDiscarded(Err(f)) => self.c.finish_gate(Err(f)),
            EditStep::DeleteClipFileDeleted { track_id } => match result {
                Ok(()) => {
                    self.analysis_call(|a, h| a.on_track_audio_changed(&track_id, h));
                    self.c.finish_gate(Ok(()));
                }
                Err(f) => self.c.finish_gate(Err(f)),
            },
            EditStep::SplitOriginalDeleted(split) => match result {
                Ok(()) => {
                    self.c.toast("Take split.");
                    self.split_finished(&split.track_id, Ok(()));
                }
                Err(f) => self.split_finished(&split.track_id, Err(f)),
            },
            EditStep::SplitDiscarded(split, outcome) => {
                self.split_finished(&split.track_id, outcome)
            }
            EditStep::MergeSourcesDeleted(merge) => match result {
                Ok(()) => {
                    self.c.toast("Takes merged.");
                    self.merge_finished(&merge.track_id, Ok(()));
                }
                Err(f) => self.merge_finished(&merge.track_id, Err(f)),
            },
            EditStep::MergeDiscarded(merge, outcome) => {
                self.merge_finished(&merge.track_id, outcome)
            }
            // Anything else ends the edit with what it got.
            _ => self.c.finish_gate(result),
        }
    }

    // --- Importing audio ---------------------------------------------------------------

    /// The pre-guard every import shares: `None` to go on, or the reason it
    /// stops (already said). Not for a screen already gone: a drop of
    /// several files carried on importing after Back, each file a write a
    /// reopened screen did not wait for. (One in flight still lands.) No
    /// concurrent structural edit: a take in flight is part of EVERY
    /// structural pre-guard: the gate must be free when the stop finalize
    /// needs it, or the finalize waits behind the edit — and the edit may
    /// delete or move the very lane the take is landing on.
    fn import_refused(&mut self) -> bool {
        if self.c.disposed || self.c.practice.is_none() {
            return true;
        }
        self.busy() || self.is_separating() || self.take_in_flight()
    }

    /// Imports `source_path` as the accompaniment (replacing any existing
    /// one).
    pub fn add_accompaniment(&mut self, source_path: &str) -> Started {
        self.import(ImportKind::Accompaniment, source_path)
    }

    /// Imports `source_path` as the reference track / original song
    /// (replacing any existing one).
    pub fn add_reference(&mut self, source_path: &str) -> Started {
        self.import(ImportKind::Reference, source_path)
    }

    /// Imports `source_path` as a new vocal take.
    pub fn add_vocal_take(&mut self, source_path: &str) -> Started {
        // The interpretation it was dropped into: a switch to another (or a
        // new one) while it imports must not carry the take along.
        let version_id = self
            .c
            .practice
            .as_deref()
            .and_then(|p| p.active_version().map(|v| v.id.clone()));
        self.import(ImportKind::Vocal { version_id }, source_path)
    }

    /// Imports `source_path` and adds it as a new CLIP on the existing vocal
    /// take `track_id` at the playhead — the drag-drop equivalent of
    /// punching a recording onto an armed take. Existing clips are left
    /// untouched (Logic / Cubase: clips never auto-merge).
    ///
    /// `at_ms` is where the clip goes when the caller took the playhead
    /// earlier than this call: at the tap that opened a file picker (the
    /// picker can stay up for seconds while playback runs on), or for the
    /// next file of a drop of several (each one's import waits for the one
    /// before). Answers where the placed clip ENDS, or `None` when nothing
    /// was added — so a multi-file drop can lay its files end to end.
    pub fn add_clip_to_take(
        &mut self,
        track_id: &str,
        source_path: &str,
        at_ms: Option<i64>,
    ) -> Started {
        let call = self.next_call();
        let target = self.c.track_by_id(track_id);
        let refused = self.c.disposed
            || self.c.practice.is_none()
            || target
                .as_ref()
                .is_none_or(|t| t.kind != AudioTrackKind::Vocal)
            || self.busy()
            || self.is_separating()
            || self.take_in_flight();
        if refused {
            self.c.reply(Reply::ClipAdded { call, end_ms: None });
            return self.started(call);
        }
        // The playhead at the DROP: copying, transcoding and analyzing take
        // seconds, and during playback (or a scrub meanwhile) a read after
        // them placed the clip wherever the playhead had moved on to.
        let start_ms = at_ms
            .unwrap_or_else(|| i64::try_from(self.position().as_millis()).unwrap_or(i64::MAX))
            .max(0);
        let target_id = target.map(|t| t.id.clone()).unwrap_or_default();
        self.start_import(
            ImportKind::Clip {
                target_id,
                start_ms,
            },
            source_path,
            call,
        );
        self.started(call)
    }

    fn import(&mut self, kind: ImportKind, source_path: &str) -> Started {
        let call = self.next_call();
        if self.import_refused() {
            self.c.reply(Reply::Done(call));
            return self.started(call);
        }
        self.start_import(kind, source_path, call);
        self.started(call)
    }

    /// Imports `source` into the sandbox, TRANSCODES compressed media to an
    /// engine-streamable WAV once (decode at import, never in full at every
    /// open), and runs the body with the playable copy. The body says
    /// whether the import was COMMITTED (its save landed): on false — a
    /// failed save reports through the save's revert+toast, not an error —
    /// and on any failure, every file this import created is deleted before
    /// the edit ends, rather than left for the next boot's staging sweep.
    /// (Imports stage under `imports/`, which boot recovery never adopts, so
    /// not even an interrupted one can come back as a phantom take.)
    fn start_import(&mut self, kind: ImportKind, source: &str, call: Call) {
        if !self.c.take_gate() {
            return self.import_reply(&kind, call, None);
        }
        let import = Import {
            kind,
            source: source.to_string(),
            call,
            imported: String::new(),
            dest: String::new(),
            duration_ms: 0,
            end_ms: None,
            replaced: Vec::new(),
        };
        self.import_job(
            source.to_string(),
            EditStep::ImportPrepared(Box::new(import)),
        );
    }

    fn import_job(&mut self, source: String, step: EditStep) {
        let files = Arc::clone(&self.c.audio_files);
        let edits = Arc::clone(&self.c.edits);
        let analyze = Arc::clone(&self.c.analyze);
        let probe = Arc::clone(&self.c.probe_duration);
        let practice_id = self.c.practice_id.clone();
        self.c
            .job(JobKind::Import, Pending::Edit(Box::new(step)), move || {
                Output::Imported(jobs::import_prepared(
                    &*files,
                    &*edits,
                    &*analyze,
                    &*probe,
                    &practice_id,
                    &source,
                ))
            });
    }

    fn import_reply(&mut self, kind: &ImportKind, call: Call, end_ms: Option<i64>) {
        match kind {
            ImportKind::Clip { .. } => self.c.reply(Reply::ClipAdded { call, end_ms }),
            _ => self.c.reply(Reply::Done(call)),
        }
    }

    fn import_prepared(&mut self, mut import: Import, output: Output) {
        let prepared = match output {
            Output::Imported(Ok(prepared)) => prepared,
            Output::Imported(Err(f)) => return self.import_finished(import, Err(f)),
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.import_finished(import, Err(f));
            }
        };
        import.imported = prepared.imported;
        import.dest = prepared.dest;
        let analysis = match prepared.analysis {
            Ok(w) => w,
            Err(f) => return self.import_clean_up(import, Err(f)),
        };
        import.duration_ms = analysis.duration_ms;
        let base_name = dart_path::basename_without_extension(&import.source);
        match &import.kind {
            ImportKind::Accompaniment | ImportKind::Reference => {
                let accompaniment = matches!(import.kind, ImportKind::Accompaniment);
                let track = AudioTrack::new(
                    new_id(),
                    if accompaniment {
                        AudioTrackKind::Accompaniment
                    } else {
                        AudioTrackKind::Reference
                    },
                    base_name,
                    DateTime::now(),
                )
                .with_audio(
                    import.dest.clone(),
                    0,
                    analysis.duration_ms,
                    analysis.peaks.clone(),
                );
                // Re-read the LIVE practice after the import/analyze awaits: a
                // take recorded/imported during them must survive, so rebuild
                // from the current model rather than a stale pre-await
                // snapshot (lost-update). The backing slot is single-writer
                // (guarded above), so the replaced backing + kept original
                // are read from the fresh model too.
                let Some(cur) = self.c.practice.clone() else {
                    return self.import_clean_up(import, Ok(()));
                };
                let old = if accompaniment {
                    cur.accompaniment.clone()
                } else {
                    cur.reference.clone()
                };
                import.replaced = old
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| c.audio_file_path.clone()))
                    .chain(cur.separation_source.clone())
                    .collect();
                // A manual import replaces any separation-derived backing —
                // drop the kept original so "Re-separate" can't later target
                // a stale source.
                let mut next = (*cur).clone();
                if accompaniment {
                    next.accompaniment = Some(Arc::new(track));
                } else {
                    next.reference = Some(Arc::new(track));
                }
                next.separation_source = None;
                self.c.set_practice(next);
                // Persist-before-delete: the replaced files fall only after
                // the save landed. Deleting them first meant a failed save
                // reverted the model to rows whose audio was already gone.
                self.save(SaveThen::Edit(Box::new(EditStep::ImportSaved(Box::new(
                    import,
                )))));
            }
            ImportKind::Vocal { version_id } => {
                // Not selected, unlike a recorded take: the selection is
                // REC's target, and a guide vocal imported to sing along with
                // became the take the next recording landed on, muted for the
                // whole of it.
                let version_id = version_id.clone();
                let dest = import.dest.clone();
                self.c
                    .append_vocal(&dest, &base_name, &analysis, 0, version_id.as_deref());
                self.save(SaveThen::Edit(Box::new(EditStep::ImportSaved(Box::new(
                    import,
                )))));
            }
            ImportKind::Clip {
                target_id,
                start_ms,
            } => {
                let clip = AudioClip {
                    id: new_id(),
                    audio_file_path: import.dest.clone(),
                    duration_ms: analysis.duration_ms,
                    start_ms: *start_ms,
                    waveform_peaks: analysis.peaks.clone(),
                };
                let clip_id = clip.id.clone();
                let target_id = target_id.clone();
                self.c.update_track(&target_id, &|t| {
                    let mut clips = t.clips.clone();
                    clips.push(clip.clone());
                    AudioTrack { clips, ..t.clone() }
                });
                self.c.selected_track_id = Some(target_id);
                self.c.selected_clip_id = Some(clip_id);
                self.c.selected_bus_id = None; // track and bus selection are exclusive
                self.save(SaveThen::Edit(Box::new(EditStep::ImportSaved(Box::new(
                    import,
                )))));
            }
        }
    }

    fn import_saved(&mut self, mut import: Import, result: Result<bool, Failure>) {
        let saved = match result {
            Ok(saved) => saved,
            Err(f) => return self.import_clean_up(import, Err(f)),
        };
        match &import.kind {
            ImportKind::Accompaniment | ImportKind::Reference => {
                if !saved {
                    return self.import_clean_up(import, Ok(()));
                }
                // The audio a replaced backing left — unless the model still
                // holds it: the kept original is the very file the Original
                // lane plays, and a take can share pooled bytes with the old
                // backing.
                let queue = std::mem::take(&mut import.replaced)
                    .into_iter()
                    .map(|path| Doomed {
                        path,
                        skip_if_held: true,
                        evict_unused: true,
                    })
                    .collect();
                self.run_deletes(
                    queue,
                    DeleteMode::Propagate,
                    EditStep::ImportReplacedDeleted(Box::new(import)),
                );
            }
            ImportKind::Vocal { .. } => {
                if saved {
                    self.import_finished(import, Ok(()));
                } else {
                    self.import_clean_up(import, Ok(()));
                }
            }
            ImportKind::Clip {
                target_id,
                start_ms,
            } => {
                // After the save, which re-reads the clip at its pooled path —
                // the key the analysis caches use (see the recording's
                // finalize).
                let target_id = target_id.clone();
                let start_ms = *start_ms;
                self.analysis_call(|a, h| a.on_track_audio_changed(&target_id, h));
                if saved {
                    import.end_ms = Some(start_ms + import.duration_ms);
                    self.import_finished(import, Ok(()));
                } else {
                    self.import_clean_up(import, Ok(()));
                }
            }
        }
    }

    /// `_withImportedAudio`'s `finally` for an import that did not commit:
    /// kept while the model on screen still points at it (a failed save that
    /// could not revert leaves the lane there for the next save to retry,
    /// which deleted from under it stored a lane whose audio was gone; the
    /// boot sweep reclaims it should no save ever take it).
    fn import_clean_up(&mut self, import: Import, outcome: Result<(), Failure>) {
        if import.dest.is_empty() || self.c.holds_audio(&import.dest) {
            return self.import_finished(import, outcome);
        }
        let mut queue = VecDeque::from([Doomed::plain(import.dest.clone())]);
        if import.dest != import.imported {
            queue.push_back(Doomed::plain(import.imported.clone()));
        }
        // Best-effort: the orphan sweep remains the backstop.
        self.run_deletes(
            queue,
            DeleteMode::StopQuietly,
            EditStep::ImportCleanedUp(Box::new(import), outcome),
        );
    }

    fn import_finished(&mut self, import: Import, outcome: Result<(), Failure>) {
        self.c.finish_gate(outcome);
        self.import_reply(&import.kind, import.call, import.end_ms);
    }

    // --- Source separation -------------------------------------------------------------

    /// Whether there's already a separated backing present (so the action
    /// reads as "re-extract"). False once the produced accompaniment is
    /// deleted — then it's a fresh extraction again.
    pub fn can_reseparate(&self) -> bool {
        self.c
            .practice
            .as_deref()
            .is_some_and(|p| p.separation_source.is_some() && p.accompaniment.is_some())
    }

    /// Separates the original song and puts the **instrumental** in the
    /// Accompaniment slot (to sing over). The **Original is kept** in the
    /// Reference slot untouched; the isolated vocal is discarded for now.
    /// The active backing switches to the instrumental.
    #[must_use]
    pub fn separate_backing(&mut self, track_id: &str) -> Vec<Effect> {
        let practice = self.c.practice.clone();
        let source = self.c.track_by_id(track_id);
        let (Some(practice), Some(source)) = (practice, source) else {
            return self.finish();
        };
        if !source.is_backing() {
            return self.finish();
        }
        if self.refuses_structural_edit() {
            return self.finish(); // no concurrent / re-entrant runs
        }
        // Residual race only: the menu item is hidden while unavailable (the
        // availability probe resolves before any menu can be shown), so this
        // guard fires only if a click slipped in before the probe landed.
        if !self.c.separation_available {
            self.c
                .toast("Source separation is not available in this version.");
            return self.finish();
        }
        // The original to separate + keep: prefer the Reference slot (where
        // the original lives), else the right-clicked track. Backings are
        // single-clip lanes, so the primary clip's file IS the original
        // recording.
        let original = practice.reference.clone().unwrap_or(source);
        let original_path = original
            .primary_clip()
            .map(|c| c.audio_file_path.clone())
            .unwrap_or_default();
        // A lane with no clip (one an older build emptied) has nothing to
        // split.
        if original_path.is_empty() {
            self.c.toast("There is no audio to separate.");
            return self.finish();
        }
        self.c.separation_progress = Some(0.0);
        self.c.separation_cancelled = false;
        self.c.notify();
        if !self.c.take_gate() {
            return self.finish();
        }
        let cell = Arc::new(ProgressCell::default());
        self.c.separation_cell = Some(Arc::clone(&cell));
        let separation = Arc::clone(&self.c.separation);
        // Registered on this thread, so a cancel right after reaches it.
        let ticket = separation.begin();
        let path = original_path.clone();
        let sep = Separation {
            original_id: original.id.clone(),
            original_path,
            imported: String::new(),
            dest: String::new(),
            old_accompaniment: None,
            old_reference: None,
            prev_play_reference: false,
            prev_selection: (None, None, None, 0),
        };
        self.c.job(
            JobKind::Separate,
            Pending::Edit(Box::new(EditStep::Separated(Box::new(sep)))),
            move || {
                let progress = move |v: f64| cell.set(v);
                Output::Separated(separation.separate_with(
                    ticket,
                    std::path::Path::new(&path),
                    Some(&progress),
                ))
            },
        );
        self.finish()
    }

    fn separated(&mut self, sep: Separation, output: Output) {
        let stems = match output {
            Output::Separated(Ok(stems)) => stems,
            // User aborted before any file work — nothing to undo.
            Output::Separated(Err(SeparationError::Cancelled)) => {
                return self.separation_finished(Ok(()))
            }
            Output::Separated(Err(e)) => {
                return self.separation_finished(Err(Failure::from_separation(&e)))
            }
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.separation_finished(Err(f));
            }
        };
        // Cancel stays on screen until the backing is in (seconds of import
        // and analysis after the stems, the whole visible run on a cached
        // song), but the service has nothing left to cancel by then.
        if self.c.separation_cancelled {
            return self.separation_finished(Ok(()));
        }
        // Only the instrumental is used; the vocal stem is ignored for now.
        // Same import discipline as every other lane import: the sidecar
        // writes a 44.1 kHz PCM16 WAV, so the import transcodes it ONCE to an
        // engine-streamable WAV — otherwise the separated backing would
        // bypass the disk-streaming feeder and be fully re-decoded into RAM
        // at every project open.
        let source = stems.instrumental_path.to_string_lossy().into_owned();
        self.import_job(source, EditStep::SeparationImported(Box::new(sep)));
    }

    fn separation_imported(&mut self, mut sep: Separation, output: Output) {
        let prepared = match output {
            Output::Imported(Ok(prepared)) => prepared,
            Output::Imported(Err(f)) => return self.separation_finished(Err(f)),
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.separation_finished(Err(f));
            }
        };
        sep.imported = prepared.imported;
        sep.dest = prepared.dest;
        let analysis = match prepared.analysis {
            Ok(w) => w,
            Err(f) => return self.separation_clean_up(sep, Err(f)),
        };
        // Cancelled while it imported: the import is undone (see above).
        if self.c.separation_cancelled {
            return self.separation_clean_up(sep, Ok(()));
        }
        // Re-read the LIVE practice: notes / lyrics / interpretations may
        // have been edited (and persisted) during the minutes-long
        // separation. Build on the current state so those edits are not
        // clobbered by the pre-await snapshot.
        let Some(cur) = self.c.practice.clone() else {
            return self.separation_clean_up(sep, Ok(()));
        };
        // The ORIGINAL lane is re-read too, not just the practice around it.
        // Renames and note/marker edits are NOT busy-gated, and separation
        // is the longest operation in the app — folding the pre-await
        // snapshot back in would overwrite everything the user added to that
        // lane while it ran (and the save tombstones the notes).
        let live_original =
            if cur.reference.as_ref().map(|r| r.id.as_str()) == Some(sep.original_id.as_str()) {
                cur.reference.clone()
            } else if cur.accompaniment.as_ref().map(|a| a.id.as_str())
                == Some(sep.original_id.as_str())
            {
                cur.accompaniment.clone()
            } else {
                None
            };
        let Some(live_original) = live_original.filter(|o| {
            o.primary_clip().map(|c| c.audio_file_path.as_str()) == Some(sep.original_path.as_str())
        }) else {
            // The lane was replaced or removed while separating: the stems
            // describe audio this project no longer has.
            self.c
                .toast("The original changed while separating. Try again.");
            return self.separation_clean_up(sep, Ok(()));
        };
        let base_name = strip_instrumental(&live_original.display_name);
        let instrumental = AudioTrack::new(
            new_id(),
            AudioTrackKind::Accompaniment,
            format!("{base_name} (Instrumental)"),
            DateTime::now(),
        )
        // Where the audio it was separated from sits: at 0, under an
        // Original dragged right, it played early against it.
        .with_audio(
            sep.dest.clone(),
            live_original.primary_clip().map_or(0, |c| c.start_ms),
            analysis.duration_ms,
            analysis.peaks.clone(),
        );
        // Keep the original as the Reference track. If it isn't already the
        // reference (e.g. it was dropped into the accompaniment slot — the
        // ordinary first separation), recreate it with the reference kind so
        // the Original stays available. Only the KIND changes: this is the
        // same lane, so it carries every field over (see rekinded).
        let reference_track = if live_original.is_reference() {
            Arc::clone(&live_original)
        } else {
            Arc::new(rekinded(&live_original, AudioTrackKind::Reference))
        };
        sep.old_accompaniment = cur.accompaniment.clone();
        sep.old_reference = cur.reference.clone();
        sep.prev_play_reference = self.mixer.play_reference();
        // The selection goes back with the flag on a failed save: that
        // save's check ran with the flag flipped, judging the backing lane by
        // the instrumental's slot, and moved a selection off the Original on
        // screen.
        sep.prev_selection = (
            self.c.selected_track_id.clone(),
            self.c.selected_clip_id.clone(),
            self.c.selected_bus_id.clone(),
            self.c.user_selections,
        );
        self.mixer.set_play_reference_silently(false); // sing over the instrumental
        let take_back = TakeBack {
            instrumental_id: instrumental.id.clone(),
            live_original: Arc::clone(&live_original),
            cur_accompaniment: cur.accompaniment.clone(),
            cur_reference: cur.reference.clone(),
            cur_separation_source: cur.separation_source.clone(),
        };
        let next = SongPractice {
            accompaniment: Some(Arc::new(instrumental)),
            reference: Some(reference_track),
            separation_source: Some(sep.original_path.clone()),
            ..(*cur).clone()
        };
        self.c.set_practice(next);
        // Past here nothing reads Cancel: it is taken off the overlay, or
        // pressed during the save it was ignored and the backing replaced.
        self.c.separation_committing = true;
        self.c.notify();
        // Persist BEFORE deleting any old audio, so a failed save can't leave
        // the JSON pointing at files already gone. On failure the import is
        // rolled back.
        self.enqueue_save(
            SaveKind::Separation(Box::new(take_back)),
            SaveThen::Edit(Box::new(EditStep::SeparationSaved(Box::new(sep)))),
        );
    }

    fn separation_saved(&mut self, sep: Separation, result: Result<bool, Failure>) {
        match result {
            Err(f) => self.separation_clean_up(sep, Err(f)),
            Ok(false) => {
                // Restore the pre-separation backing flag AND push it: the
                // failed save already resynced the engine under the flipped
                // flag, so a silent restore left the toggle and the audible
                // backing apart. The take-back changed the lanes after that
                // resync, too.
                self.mixer
                    .set_play_reference_silently(sep.prev_play_reference);
                self.resync(super::persist::ResyncThen::Edit(Box::new(
                    EditStep::SeparationRevertResynced(Box::new(sep)),
                )));
            }
            Ok(true) => {
                // The instrumental is live + persisted. Delete any prior
                // backing files that are no longer referenced — never the kept
                // original or new instrumental. Errors here must NOT roll back
                // the live track (which is why this runs after the import, not
                // inside it).
                let mut queue = VecDeque::new();
                for old in [&sep.old_accompaniment, &sep.old_reference]
                    .into_iter()
                    .flatten()
                {
                    for c in &old.clips {
                        if c.audio_file_path == sep.original_path || c.audio_file_path == sep.dest {
                            continue;
                        }
                        queue.push_back(Doomed::evicting(c.audio_file_path.clone()));
                    }
                }
                // Best-effort: a lingering old file is harmless.
                self.run_deletes(queue, DeleteMode::Continue, EditStep::SeparationOldDeleted);
            }
        }
    }

    fn separation_revert_resynced(&mut self, sep: Separation, result: Result<(), Failure>) {
        if let Err(f) = result {
            return self.separation_clean_up(sep, Err(f));
        }
        // Unless the user picked a lane while the save ran.
        let (track, clip, bus, picks) = sep.prev_selection.clone();
        if self.c.user_selections == picks {
            self.c.selected_track_id = track;
            self.c.selected_clip_id = clip;
            self.c.selected_bus_id = bus;
        }
        self.ensure_selection_analyzed();
        self.c.notify();
        self.c.toast("Could not save the separated track.");
        self.separation_clean_up(sep, Ok(()));
    }

    /// The separation's import that did not commit is undone (see
    /// `import_clean_up`).
    fn separation_clean_up(&mut self, sep: Separation, outcome: Result<(), Failure>) {
        if sep.dest.is_empty() || self.c.holds_audio(&sep.dest) {
            return self.separation_finished(outcome);
        }
        let mut queue = VecDeque::from([Doomed::plain(sep.dest.clone())]);
        if sep.dest != sep.imported {
            queue.push_back(Doomed::plain(sep.imported.clone()));
        }
        self.run_deletes(
            queue,
            DeleteMode::StopQuietly,
            EditStep::SeparationCleanedUp(outcome),
        );
    }

    fn separation_finished(&mut self, outcome: Result<(), Failure>) {
        self.c.separation_progress = None;
        self.c.separation_cell = None;
        self.c.separation_committing = false;
        self.c.notify();
        self.c.finish_gate(outcome);
    }

    /// Aborts an in-flight separation (best-effort).
    pub fn cancel_separation(&mut self) {
        if !self.can_cancel_separation() {
            return;
        }
        self.c.separation_cancelled = true;
        self.c.separation.cancel();
    }

    // --- Exporting audio ---------------------------------------------------------------
    // The escape hatch that guarantees audio is never trapped in the app's
    // private container: any track's audio can be copied out to a
    // user-chosen location, byte-for-byte.

    /// Exports a track's audio to a user-chosen location. Single-clip lanes
    /// only (the file IS the lane); a multi-clip lane is refused with a
    /// pointer to the per-clip export — v1 does not render a lane's clips +
    /// gaps into one file.
    #[must_use]
    pub fn export_track_audio(&mut self, track_id: &str) -> Vec<Effect> {
        let practice = self.c.practice.clone();
        let track = self.c.track_by_id(track_id);
        let (Some(practice), Some(track)) = (practice, track) else {
            return self.finish();
        };
        if track.clips.is_empty() {
            self.c.toast("This take has no audio to export yet.");
            return self.finish();
        }
        if track.clips.len() > 1 {
            self.c.toast(
                "This take has multiple clips. Export each clip from its own right-click menu.",
            );
            return self.finish();
        }
        let clip = &track.clips[0];
        let name = AudioExporter::file_name(
            &practice.title,
            &track.display_name,
            &clip.audio_file_path,
            "",
        );
        let noun = if track.is_backing() { "track" } else { "take" };
        self.export_file(&clip.audio_file_path, &name, noun);
        self.finish()
    }

    /// Exports one clip's audio file — the per-clip path for multi-clip
    /// lanes.
    #[must_use]
    pub fn export_clip_audio(&mut self, track_id: &str, clip_id: &str) -> Vec<Effect> {
        let practice = self.c.practice.clone();
        let track = self.c.track_by_id(track_id);
        let (Some(practice), Some(track)) = (practice, track) else {
            return self.finish();
        };
        let Some(clip) = track.clip_by_id(clip_id) else {
            return self.finish();
        };
        // Number the clip when the lane holds several, so exporting two clips
        // of the same take doesn't suggest the same file name twice — by
        // where it sits on the timeline, as the user sees them (by storage
        // order, a clip dropped before the first was "clip 2").
        let mut by_start: Vec<&AudioClip> = track.clips.iter().collect();
        by_start.sort_by_key(|c| c.start_ms);
        let index = by_start.iter().position(|c| c.id == clip_id).unwrap_or(0);
        let suffix = if track.clips.len() > 1 {
            format!(" (clip {})", index + 1)
        } else {
            String::new()
        };
        let name = AudioExporter::file_name(
            &practice.title,
            &track.display_name,
            &clip.audio_file_path,
            &suffix,
        );
        self.export_file(&clip.audio_file_path, &name, "clip");
        self.finish()
    }

    /// Exports `source` (see [`AudioExporter`]); a cancelled save panel is
    /// silent, every other outcome is said.
    fn export_file(&mut self, source: &str, suggested_name: &str, noun: &str) {
        let exporter = Arc::clone(&self.c.exporter);
        let source = source.to_string();
        let suggested_name = suggested_name.to_string();
        let noun = noun.to_string();
        self.c.job(JobKind::Export, Pending::Export, move || {
            Output::Exported(exporter.export(&source, &suggested_name, &noun, "audio"))
        });
    }

    // --- Takes -------------------------------------------------------------------------

    /// Creates an empty vocal take (a lane with no clip yet) in the active
    /// interpretation and selects it, so the next REC records into it. Its
    /// id goes to whoever asked once its save landed, `None` otherwise (REC
    /// then records nothing).
    pub(super) fn create_empty_vocal_take(&mut self, asked: EmptyTakeFor) {
        let Some(practice) = self.c.practice.clone() else {
            return self.empty_take_answer(asked, None);
        };
        let mut track = AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            format!("Take {}", self.c.next_take_number()),
            DateTime::now(),
        );
        // The RESOLVED interpretation (what the lanes show), not the raw
        // field: a missing or dangling active id — the integrity repair
        // clears one whose interpretation was deleted on another device —
        // still shows the first interpretation, and a take stamped with the
        // raw value would be filed under no interpretation at all and never
        // appear.
        track.version_id = practice.active_version().map(|v| v.id.clone());
        let track_id = track.id.clone();
        let mut next = (*practice).clone();
        next.vocal_tracks.push(Arc::new(track));
        self.c.set_practice(next);
        let before = SelectionBefore {
            token: self.c.id(),
            track: self.c.selected_track_id.clone(),
            bus: self.c.selected_bus_id.clone(),
            picks: self.c.user_selections,
        };
        self.c.selection_before_empty_take = Some(before.clone());
        self.c.selected_track_id = Some(track_id.clone());
        self.c.selected_bus_id = None; // track and bus selection are exclusive
        self.save(SaveThen::EmptyTake(Box::new(EmptyTake {
            asked,
            track_id,
            before,
        })));
    }

    pub(super) fn empty_take_saved(&mut self, empty: EmptyTake, saved: bool) {
        if saved {
            return self.empty_take_answer(empty.asked, Some(empty.track_id));
        }
        // Not saved, so reverted out of the model: as when a REC drops its
        // empty take, the selection goes back (it had fallen to the first
        // lane).
        if self
            .c
            .selection_before_empty_take
            .as_ref()
            .is_some_and(|b| b.token == empty.before.token)
        {
            self.c.selection_before_empty_take = None;
        }
        // Its failure gives the selection back only with no REC arming (for
        // the "+ New take" button): that start reads the selection as the
        // take to record into, and given back, a take the user meant to be
        // new went into the one selected before.
        let give_back = match empty.asked {
            EmptyTakeFor::Recording(_) => true,
            EmptyTakeFor::NewTake => !self.recording.is_starting_take(),
        };
        if give_back {
            self.restore_selection(&empty.before, &empty.track_id);
        }
        self.empty_take_answer(empty.asked, None);
    }

    fn empty_take_answer(&mut self, asked: EmptyTakeFor, created: Option<String>) {
        match asked {
            EmptyTakeFor::Recording(op) => {
                self.recording_call(|r, h| r.empty_take_created(op, created, h));
            }
            EmptyTakeFor::NewTake => {}
        }
    }

    /// The "+ New take" button: an empty take, selected — the user's own
    /// pick, which a REC start dropping its empty take must not restore over.
    #[must_use]
    pub fn new_take(&mut self) -> Vec<Effect> {
        self.user_selected();
        self.create_empty_vocal_take(EmptyTakeFor::NewTake);
        self.finish()
    }

    /// Drops the empty take a REC created for itself when the start never
    /// armed (cancelled by Stop, or failing), and gives back the selection
    /// it took: the selection otherwise fell back to the first lane, losing
    /// a selected bus or the Original lane the user was on.
    pub(super) fn discard_empty_take(&mut self, op: RecordingOp, track_id: String) {
        let before = self.c.selection_before_empty_take.take();
        // Not delete_track: it refuses while a take is in flight, which the
        // recorder reports this drop as (a REC meanwhile would be refused by
        // the gate). An empty take has no audio to delete.
        if !self.c.take_gate() {
            return self.empty_take_discard_done(op, &track_id, before);
        }
        let Some(cur) = self.c.practice.clone() else {
            self.c.finish_gate(Ok(()));
            return self.empty_take_discard_done(op, &track_id, before);
        };
        let take = cur.vocal_tracks.iter().find(|t| t.id == track_id);
        if take.is_none_or(|t| !t.clips.is_empty()) {
            self.c.finish_gate(Ok(()));
            return self.empty_take_discard_done(op, &track_id, before);
        }
        let next = SongPractice {
            vocal_tracks: cur
                .vocal_tracks
                .iter()
                .filter(|t| t.id != track_id)
                .cloned()
                .collect(),
            ..(*cur).clone()
        };
        self.c.set_practice(next);
        self.save(SaveThen::Edit(Box::new(EditStep::DiscardEmptySaved {
            op,
            track_id,
            before,
        })));
    }

    fn empty_take_discard_done(
        &mut self,
        op: RecordingOp,
        track_id: &str,
        before: Option<SelectionBefore>,
    ) {
        if let Some(before) = before {
            self.restore_selection(&before, track_id);
        }
        let effects = self.recording.empty_take_discarded(op);
        self.apply_recording(effects);
    }

    /// Gives back the selection `before` an empty take `track_id` took, once
    /// that take is gone.
    fn restore_selection(&mut self, before: &SelectionBefore, track_id: &str) {
        if self.c.track_by_id(track_id).is_some() {
            return;
        }
        // Unless the user picked a lane during the arm window: restored over
        // it, their choice was undone (the separation's restore stands down
        // too).
        if self.c.user_selections != before.picks {
            return;
        }
        let bus_exists = before.bus.as_deref().is_some_and(|bus| {
            self.c
                .practice
                .as_deref()
                .is_some_and(|p| p.fx_buses.iter().any(|b| b.id == bus))
        });
        if bus_exists {
            let bus = before.bus.clone();
            self.select_bus(bus.as_deref());
        } else if let Some(track) = before
            .track
            .as_deref()
            .filter(|t| self.c.track_by_id(t).is_some())
        {
            let track = track.to_string();
            self.select_track_inner(&track);
        }
    }

    // --- Track mutations ---------------------------------------------------------------

    #[must_use]
    pub fn delete_track(&mut self, track_id: &str) -> Vec<Effect> {
        self.delete_track_inner(track_id);
        self.finish()
    }

    fn delete_track_inner(&mut self, track_id: &str) {
        if self.c.practice.is_none() {
            return;
        }
        if self.refuses_structural_edit() {
            return; // no concurrent structural edit
        }
        if !self.c.take_gate() {
            return;
        }
        // No awaits happened since the guard, so the model is still live
        // here.
        let Some(cur) = self.c.practice.clone() else {
            return self.c.finish_gate(Ok(()));
        };
        let mut stale = None;
        let mut next = (*cur).clone();
        let doomed: Vec<String> =
            if let Some(acc) = cur.accompaniment.as_ref().filter(|a| a.id == track_id) {
                next.accompaniment = None;
                acc.clips
                    .iter()
                    .map(|c| c.audio_file_path.clone())
                    .collect()
            } else if let Some(r) = cur.reference.as_ref().filter(|r| r.id == track_id) {
                // The original kept for re-extracting the backing goes with
                // it: only the Original lane offers that, so left set it held
                // that audio for good, though the dialog said the audio file is
                // removed.
                stale = cur.separation_source.clone();
                next.reference = None;
                next.separation_source = None;
                r.clips.iter().map(|c| c.audio_file_path.clone()).collect()
            } else {
                // Delete EVERY clip's file — a multi-clip take would otherwise
                // orphan its trailing clips' audio.
                next.vocal_tracks = cur
                    .vocal_tracks
                    .iter()
                    .filter(|t| t.id != track_id)
                    .cloned()
                    .collect();
                cur.vocal_tracks
                    .iter()
                    .filter(|t| t.id == track_id)
                    .flat_map(|t| t.clips.iter().map(|c| c.audio_file_path.clone()))
                    .collect()
            };
        self.c.set_practice(next);
        // Persist BEFORE deleting any audio, and ONLY delete when the save
        // succeeded — a failed save reverts the model back to referencing
        // these clips, so their files must survive.
        self.save(SaveThen::Edit(Box::new(EditStep::DeleteTrackSaved {
            track_id: track_id.to_string(),
            doomed,
            stale,
        })));
    }

    /// Deletes a single clip from a lane (Logic / Cubase style), keeping the
    /// lane and its other clips. Notes anchored inside the clip's span are
    /// dropped. Use [`SongPracticeDetailViewModel::delete_track`] to remove
    /// a whole lane.
    #[must_use]
    pub fn delete_clip(&mut self, track_id: &str, clip_id: &str) -> Vec<Effect> {
        let Some(track) = self.c.track_by_id(track_id) else {
            return self.finish();
        };
        let Some(clip) = track.clip_by_id(clip_id).cloned() else {
            return self.finish();
        };
        // No concurrent structural edit; said, not silently ignored (the menu
        // offers it either way).
        if self.refuses_structural_edit() {
            return self.finish();
        }
        // A backing lane is its one clip: its only clip gone, it stayed as an
        // empty lane nothing could fill (a dropped file went to the takes, as
        // the slot read as taken), with its kept original still set.
        if track.is_backing() {
            self.delete_track_inner(track_id);
            return self.finish();
        }
        if !self.c.take_gate() {
            return self.finish();
        }
        let remaining: Vec<AudioClip> = track
            .clips
            .iter()
            .filter(|c| c.id != clip_id)
            .cloned()
            .collect();
        // Drop markers anchored where the lane falls silent; for a range that
        // starts before such a gap, clamp its end to the gap's edge so it
        // doesn't overhang it. Only the gaps: what another clip of the lane
        // still covers keeps its notes.
        let gaps = uncovered_spans(&clip, &remaining);
        let silenced = |ms: i64| gaps.iter().any(|g| ms >= g.0 && ms < g.1);
        let next_gap = |ms: i64| gaps.iter().find(|g| g.0 > ms).map(|g| g.0);
        let kept_notes: Vec<PracticeNote> = track
            .notes
            .iter()
            .filter(|n| !silenced(n.position_ms))
            .map(|n| match next_gap(n.position_ms) {
                Some(edge) => clamp_note_end(n, edge),
                None => n.clone(),
            })
            .collect();
        let kept_markers: Vec<TechniqueMarker> = track
            .technique_markers
            .iter()
            .filter(|m| !silenced(m.position_ms))
            .map(|m| match next_gap(m.position_ms) {
                Some(edge) => clamp_marker_end(m, edge),
                None => m.clone(),
            })
            .collect();
        self.c.update_track(track_id, &|t| AudioTrack {
            clips: remaining.clone(),
            notes: kept_notes.clone(),
            technique_markers: kept_markers.clone(),
            ..t.clone()
        });
        if self.c.selected_clip_id.as_deref() == Some(clip_id) {
            self.c.selected_clip_id = None;
        }
        // Persist BEFORE deleting the audio, and ONLY delete when the save
        // succeeded — a failed save reverts the model back to referencing
        // this clip, so its file must survive.
        self.save(SaveThen::Edit(Box::new(EditStep::DeleteClipSaved {
            track_id: track_id.to_string(),
            path: clip.audio_file_path,
        })));
        self.finish()
    }

    // --- Clip editing (drag-move, reorder, split, merge, …) --------------------------

    /// Persists a finished drag-move and re-aligns offset-aware playback. A
    /// move changes the lane-relative offsets baked into a multi-clip take's
    /// aggregate analysis (its cache key embeds clip positions, so the old
    /// entry is simply stale) — recompute whatever analysis is on screen for
    /// the new layout. The ensure calls are cheap no-ops for single-clip
    /// takes and cached subjects.
    #[must_use]
    pub fn commit_track_move(&mut self) -> Vec<Effect> {
        self.c.moving_live.clear();
        self.c.clip_riders = None;
        self.save(SaveThen::CommitMove);
        self.finish()
    }

    /// Moves a single clip along the timeline live (model only) while
    /// dragging, and with it the notes and technique markers on its audio
    /// (see `ClipRiders`: what lay on the clip when the drag began, a range
    /// by where it starts). Persist once on release with
    /// [`SongPracticeDetailViewModel::commit_track_move`], which saves the
    /// clip and its notes together, as the lane shows them all along.
    pub fn move_clip_start_live(&mut self, track_id: &str, clip_id: &str, start_ms: i64) {
        let Some(track) = self.c.track_by_id(track_id) else {
            return;
        };
        let Some(clip) = track.clip_by_id(clip_id) else {
            return;
        };
        self.c.moving_live.insert(track_id.to_string());
        let timeline = self
            .c
            .practice
            .as_deref()
            .map_or(0, |p| p.timeline_duration_ms);
        let max_start =
            vocara_model::util::dart_num::clamp_i64(timeline - clip.duration_ms, 0, timeline);
        let clamped = vocara_model::util::dart_num::clamp_i64(start_ms, 0, max_start);
        if clip.start_ms == clamped {
            return;
        }
        // Taken on the drag's first move, while the clip is still where the
        // drag found it.
        let riders = match self.c.clip_riders.take() {
            Some(r) if r.track_id == track_id && r.clip_id == clip_id => r,
            _ => match ClipRiders::of(&track, clip_id) {
                Some(r) => r,
                None => return,
            },
        };
        let delta = clamped - riders.start_ms;
        let clip_id = clip_id.to_string();
        self.c.update_track(track_id, &|t| AudioTrack {
            clips: t
                .clips
                .iter()
                .map(|c| {
                    if c.id == clip_id {
                        AudioClip {
                            start_ms: clamped,
                            ..c.clone()
                        }
                    } else {
                        c.clone()
                    }
                })
                .collect(),
            notes: riders.moved_notes(&t.notes, delta, timeline),
            technique_markers: riders.moved_markers(&t.technique_markers, delta, timeline),
            ..t.clone()
        });
        self.c.clip_riders = Some(riders);
        self.c.notify();
    }

    /// Moves the dragged vocal take into the lane currently held by
    /// `target_id` (drag-and-drop reorder) — "move to a different track" in
    /// our one-clip-per-lane model.
    #[must_use]
    pub fn move_vocal_track_to_lane_of(
        &mut self,
        dragged_id: &str,
        target_id: &str,
    ) -> Vec<Effect> {
        let Some(practice) = self.c.practice.clone() else {
            return self.finish();
        };
        if dragged_id == target_id {
            return self.finish();
        }
        let mut list = practice.vocal_tracks.clone();
        let from = list.iter().position(|t| t.id == dragged_id);
        let to = list.iter().position(|t| t.id == target_id);
        let (Some(from), Some(to)) = (from, to) else {
            return self.finish();
        };
        let moved = list.remove(from);
        list.insert(to, moved);
        self.c.set_practice(SongPractice {
            vocal_tracks: list,
            ..(*practice).clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    #[must_use]
    pub fn rename_track(&mut self, track_id: &str, name: &str) -> Vec<Effect> {
        let trimmed = vocara_model::util::dart_string::trim(name).to_string();
        if trimmed.is_empty() {
            return self.finish();
        }
        self.c.update_track(track_id, &|t| AudioTrack {
            display_name: trimmed.clone(),
            ..t.clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    /// Sets a track's custom lane tint (ARGB). `None` resets to the default.
    #[must_use]
    pub fn set_track_color(&mut self, track_id: &str, color_value: Option<i64>) -> Vec<Effect> {
        self.c.update_track(track_id, &|t| AudioTrack {
            color_value,
            ..t.clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    /// Copies a vocal take (audio + notes) into a new lane right below it.
    #[must_use]
    pub fn duplicate_track(&mut self, track_id: &str) -> Vec<Effect> {
        let src = self.c.track_by_id(track_id);
        let Some(src) = src.filter(|s| self.c.practice.is_some() && !s.is_backing()) else {
            return self.finish();
        };
        // No concurrent structural edit; said, not silently ignored (the menu
        // offers it either way).
        if self.refuses_structural_edit() {
            return self.finish();
        }
        if !self.c.take_gate() {
            return self.finish();
        }
        // Copy EVERY clip's file (not just the primary) to a fresh path and
        // give it a fresh id, so the duplicate is fully independent: a later
        // delete of the source (or the copy) must never touch the other's
        // audio, and reused clip PKs would collide on insert and roll the
        // whole save back.
        let clips: Vec<AudioClip> = src.clips.clone();
        let pairs: Vec<(String, String)> = clips
            .iter()
            .map(|c| (c.id.clone(), c.audio_file_path.clone()))
            .collect();
        let edits = Arc::clone(&self.c.edits);
        let practice_id = self.c.practice_id.clone();
        self.c.job(
            JobKind::Duplicate,
            Pending::Edit(Box::new(EditStep::DuplicateCopied {
                track_id: track_id.to_string(),
                clips,
            })),
            move || Output::Duplicated(jobs::duplicate_clips(&*edits, &practice_id, &pairs)),
        );
        self.finish()
    }

    fn duplicate_copied(&mut self, track_id: &str, clips: &[AudioClip], output: Output) {
        let duplicated = match output {
            Output::Duplicated(d) => d,
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.c.finish_gate(Err(f));
            }
        };
        // Each copy by the source clip it was made from.
        let mut copy_of: HashMap<String, AudioClip> = HashMap::new();
        let mut copies = Vec::new();
        for (clip_id, dest) in &duplicated.copies {
            if let Some(c) = clips.iter().find(|c| &c.id == clip_id) {
                copy_of.insert(
                    clip_id.clone(),
                    AudioClip {
                        id: new_id(),
                        audio_file_path: dest.clone(),
                        duration_ms: c.duration_ms,
                        start_ms: c.start_ms,
                        waveform_peaks: c.waveform_peaks.clone(),
                    },
                );
            }
            copies.push(dest.clone());
        }
        // A multi-clip lane can fail part way through (disk full on the
        // second file), and the copies that already landed are then
        // orphaned in edits/ with nothing referencing them. Undo them on the
        // way out instead of leaving them for the next healthy boot's sweep.
        // Never let a cleanup failure replace the failure that caused it: the
        // copy error is what the user needs to see (disk full), a delete that
        // also failed only leaks a file the boot sweep collects.
        if let Some(f) = duplicated.failure {
            let queue = copies.into_iter().map(Doomed::unless_held).collect();
            return self.run_deletes(
                queue,
                DeleteMode::StopQuietly,
                EditStep::DuplicateDiscarded(Err(f)),
            );
        }
        let discard = |vm: &mut Self, copies: Vec<String>| {
            let queue = copies.into_iter().map(Doomed::unless_held).collect();
            vm.run_deletes(
                queue,
                DeleteMode::Propagate,
                EditStep::DuplicateDiscarded(Ok(())),
            );
        };
        // Re-read the LIVE practice after the file copies so a take
        // recorded/imported meanwhile isn't dropped by a stale-snapshot
        // rebuild.
        let Some(cur) = self.c.practice.clone() else {
            return discard(self, copies);
        };
        let mut list = cur.vocal_tracks.clone();
        let Some(idx) = list.iter().position(|t| t.id == track_id) else {
            return discard(self, copies); // source removed during the copy
        };
        // The copy is of the lane as it is now, as split and merge rebuild
        // theirs: built from the pre-copy snapshot, it missed a rename, a
        // note or a clip drag made while the files copied, and kept the bus
        // and sends of a bus deleted meanwhile.
        let live = Arc::clone(&list[idx]);
        let copy = AudioTrack {
            id: new_id(),
            display_name: format!("{} copy", live.display_name),
            created_at: DateTime::now(),
            clips: live
                .clips
                .iter()
                .filter_map(|c| {
                    copy_of.get(&c.id).map(|copied| AudioClip {
                        start_ms: c.start_ms,
                        ..copied.clone()
                    })
                })
                .collect(),
            // Fresh note/marker ids too: unlike split/merge the SOURCE take
            // survives, so its child rows stay LIVE holding these ids —
            // copying them verbatim onto the new track id would collide on
            // the notes/markers primary key and roll the whole save back.
            notes: live
                .notes
                .iter()
                .map(|n| PracticeNote {
                    id: new_id(),
                    ..n.clone()
                })
                .collect(),
            technique_markers: live
                .technique_markers
                .iter()
                .map(|m| TechniqueMarker {
                    id: new_id(),
                    ..m.clone()
                })
                .collect(),
            ..(*live).clone()
        };
        list.insert(idx + 1, Arc::new(copy));
        self.c.set_practice(SongPractice {
            vocal_tracks: list,
            ..(*cur).clone()
        });
        self.save(SaveThen::Edit(Box::new(EditStep::DuplicateSaved {
            copies,
        })));
    }

    /// Splits a vocal take at the playhead into two takes; the second is
    /// offset to the cut. Notes follow whichever side they fall on.
    #[must_use]
    pub fn split_track_at_playhead(&mut self, track_id: &str) -> Vec<Effect> {
        let src = self.c.track_by_id(track_id);
        let Some(src) = src.filter(|s| self.c.practice.is_some() && !s.is_backing()) else {
            return self.finish();
        };
        // No concurrent structural edit; said, not silently ignored (the menu
        // offers it either way).
        if self.refuses_structural_edit() {
            return self.finish();
        }
        // Byte-splitting is a ONE-FILE operation: a multi-clip lane has
        // several independent files, and its lane span covers gaps no file
        // byte maps to — refuse rather than silently drop the other clips
        // (delete/move a clip from its menu instead).
        if src.clips.len() > 1 {
            self.c.toast(
                "This take has multiple clips. Delete or move a clip from its menu instead of \
                 splitting the lane.",
            );
            return self.finish();
        }
        // Single-clip lane: the clip IS the lane (same span), so the cut
        // position maps 1:1 into the clip's file.
        let Some(clip) = src.primary_clip().cloned() else {
            // Told to move the playhead onto a region the empty take does not
            // have.
            self.c.toast("An empty take has no audio to split.");
            return self.finish();
        };
        let at_ms = i64::try_from(self.position().as_millis()).unwrap_or(i64::MAX);
        if at_ms <= clip.start_ms + PlaybackController::MIN_LOOP_MS
            || at_ms >= clip.end_ms() - PlaybackController::MIN_LOOP_MS
        {
            self.c.toast(
                "Move the playhead onto this take to split it. The take spans only its \
                 highlighted region.",
            );
            return self.finish();
        }
        if !self.c.take_gate() {
            return self.finish();
        }
        let edits = Arc::clone(&self.c.edits);
        let practice_id = self.c.practice_id.clone();
        let source = clip.audio_file_path.clone();
        let split_file_ms = at_ms - clip.start_ms;
        let split = Split {
            track_id: track_id.to_string(),
            clip,
            at_ms,
            first: String::new(),
            second: String::new(),
            first_clip_id: String::new(),
            follow_selection: false,
            moved_keys: Vec::new(),
        };
        self.c.job(
            JobKind::Split,
            Pending::Edit(Box::new(EditStep::SplitCut(Box::new(split)))),
            move || {
                Output::Split(
                    edits
                        .split_wav(&practice_id, &source, split_file_ms)
                        .map(|p| (p.first_path, p.second_path))
                        .map_err(|e| Failure::from_edit(&e)),
                )
            },
        );
        self.finish()
    }

    fn split_cut(&mut self, mut split: Split, output: Output) {
        let (first, second) = match output {
            Output::Split(Ok(parts)) => parts,
            Output::Split(Err(f)) => return self.split_finished(&split.track_id, Err(f)),
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.split_finished(&split.track_id, Err(f));
            }
        };
        split.first = first.clone();
        split.second = second.clone();
        // Until the model takes the new files, every way out (an analysis
        // that throws, the practice or take gone meanwhile) discards them
        // rather than leave them in edits/ until the next boot's sweep.
        let analyze = Arc::clone(&self.c.analyze);
        self.c.job(
            JobKind::Analyze,
            Pending::Edit(Box::new(EditStep::SplitAnalyzed(Box::new(split)))),
            move || {
                let halves = jobs::analyze_said(&*analyze, &first)
                    .and_then(|a| jobs::analyze_said(&*analyze, &second).map(|b| (a, b)));
                Output::Halves(halves)
            },
        );
    }

    fn split_discard(&mut self, split: Split, outcome: Result<(), Failure>) {
        let paths = vec![split.first.clone(), split.second.clone()];
        self.discard_quietly(paths, EditStep::SplitDiscarded(Box::new(split), outcome));
    }

    fn split_analyzed(&mut self, mut split: Split, output: Output) {
        let (first_analysis, second_analysis) = match output {
            Output::Halves(Ok(halves)) => halves,
            Output::Halves(Err(f)) => return self.split_discard(split, Err(f)),
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.split_discard(split, Err(f));
            }
        };
        // Re-read the LIVE practice after the split/analyze awaits so a take
        // recorded/imported meanwhile isn't dropped by a stale-snapshot
        // rebuild.
        let Some(cur) = self.c.practice.clone() else {
            return self.split_discard(split, Ok(()));
        };
        let mut list = cur.vocal_tracks.clone();
        let Some(idx) = list.iter().position(|t| t.id == split.track_id) else {
            return self.split_discard(split, Ok(())); // source removed during the split
        };
        // Rebuild both halves from the LIVE take, not the pre-await snapshot:
        // renames and note/marker edits are NOT busy-gated, so one landing
        // during those awaits would be overwritten here — and a note added
        // meanwhile would be missing from the desired list, which the save
        // tombstones.
        let live = Arc::clone(&list[idx]);
        if live.clips.len() != 1
            || live.primary_clip().map(|c| c.id.as_str()) != Some(split.clip.id.as_str())
        {
            // The lane's audio changed underneath (no gated path does this
            // today): abandon the split rather than write halves cut from a
            // stale file.
            return self.split_discard(split, Ok(()));
        }
        // The surviving take keeps its lane, but its clip is a NEW row. Never
        // re-point a clip id at a different file: the reconcile would
        // overwrite the row's asset in place, leaving nothing that references
        // the original recording, and the next boot's recovery would adopt
        // its staging WAV as a whole "(recovered)" take beside the two
        // halves. A fresh id tombstones the old row, which keeps its asset
        // referenced.
        let first_clip_id = new_id();
        // The CLIP is re-read live too: a drag is not busy-gated either, and
        // it moves the clip without touching its file. The file was cut at
        // an offset INTO the clip, so both halves (and the cut) follow the
        // clip to where it sits now — rebuilding from the snapshot snapped it
        // back.
        let live_clip = live.clips[0].clone();
        let cut_ms = split.at_ms + live_clip.start_ms - split.clip.start_ms;
        let first = AudioTrack {
            clips: vec![AudioClip {
                id: first_clip_id.clone(),
                audio_file_path: split.first.clone(),
                duration_ms: first_analysis.duration_ms,
                waveform_peaks: first_analysis.peaks.clone(),
                ..live_clip.clone()
            }],
            // Markers stay with the half their start falls in; a range
            // straddling the cut is clamped so it doesn't dangle past the
            // shortened take.
            notes: live
                .notes
                .iter()
                .filter(|n| n.position_ms < cut_ms)
                .map(|n| clamp_note_end(n, cut_ms))
                .collect(),
            technique_markers: live
                .technique_markers
                .iter()
                .filter(|m| m.position_ms < cut_ms)
                .map(|m| clamp_marker_end(m, cut_ms))
                .collect(),
            ..(*live).clone()
        };
        let second_id = new_id();
        let track_id = split.track_id.clone();
        let mut moved_keys = Vec::new();
        let mut move_to = |vm: &mut Self, old_id: &str| -> String {
            let fresh = new_id();
            moved_keys.push((track_id.clone(), old_id.to_string()));
            vm.c.moved_marks.insert(
                (track_id.clone(), old_id.to_string()),
                (second_id.clone(), fresh.clone()),
            );
            fresh
        };
        // Relocating notes/markers onto a NEW track id needs fresh ids: the
        // source track is tombstoned (its child rows soft-deleted) but still
        // holds these ids, so reusing them would collide on insert and roll
        // the whole save back.
        let mut second_notes = Vec::new();
        for n in live.notes.iter().filter(|n| n.position_ms >= cut_ms) {
            let id = move_to(self, &n.id);
            second_notes.push(PracticeNote { id, ..n.clone() });
        }
        let mut second_markers = Vec::new();
        for m in live
            .technique_markers
            .iter()
            .filter(|m| m.position_ms >= cut_ms)
        {
            let id = move_to(self, &m.id);
            second_markers.push(TechniqueMarker { id, ..m.clone() });
        }
        let second = AudioTrack {
            id: second_id.clone(),
            // A FRESH clip id — otherwise the new take and the surviving
            // `first` take would share one clip primary key and the save
            // transaction would roll back on the duplicate INSERT, silently
            // losing the split (and the original file is already deleted by
            // then).
            clips: vec![AudioClip {
                id: new_id(),
                audio_file_path: split.second.clone(),
                duration_ms: second_analysis.duration_ms,
                start_ms: cut_ms,
                waveform_peaks: second_analysis.peaks.clone(),
            }],
            notes: second_notes,
            technique_markers: second_markers,
            display_name: format!("{} (2)", live.display_name),
            created_at: DateTime::now(),
            ..(*live).clone()
        };
        list[idx] = Arc::new(first);
        list.insert(idx + 1, Arc::new(second));
        self.c.set_practice(SongPractice {
            vocal_tracks: list,
            ..(*cur).clone()
        });
        split.follow_selection = self.c.selected_clip_id.as_deref() == Some(split.clip.id.as_str());
        if split.follow_selection {
            self.c.selected_clip_id = Some(first_clip_id.clone());
        }
        split.first_clip_id = first_clip_id;
        split.moved_keys = moved_keys;
        // Persist BEFORE deleting any old audio, so a failed save can't leave
        // the model pointing at a file that's already gone.
        self.save(SaveThen::Edit(Box::new(EditStep::SplitSaved(Box::new(
            split,
        )))));
    }

    fn split_saved(&mut self, split: Split, result: Result<bool, Failure>) {
        match result {
            Err(f) => self.split_finished(&split.track_id, Err(f)),
            Ok(true) => {
                let path = split.clip.audio_file_path.clone();
                self.run_deletes(
                    VecDeque::from([Doomed::evicting(path)]),
                    DeleteMode::Propagate,
                    EditStep::SplitOriginalDeleted(Box::new(split)),
                );
            }
            Ok(false) => {
                // The failed save reverted the model to the original one-clip
                // take, so the split halves are orphans — clean them up
                // instead.
                if split.follow_selection
                    && self.c.selected_clip_id.as_deref() == Some(split.first_clip_id.as_str())
                {
                    self.c.selected_clip_id = Some(split.clip.id.clone());
                }
                for key in &split.moved_keys {
                    self.c.moved_marks.remove(key);
                }
                self.split_discard(split, Ok(()));
            }
        }
    }

    fn split_finished(&mut self, track_id: &str, outcome: Result<(), Failure>) {
        self.c.finish_gate(outcome);
        // The surviving take keeps its id but now plays a new file.
        self.analysis_call(|a, h| a.on_track_audio_changed(track_id, h));
    }

    /// Merges a vocal take with the take in the lane below it: their audio is
    /// concatenated (inverse of split) and the result sits at the upper
    /// clip's position. The lower take's notes shift to follow the joined
    /// audio.
    #[must_use]
    pub fn merge_track_down(&mut self, track_id: &str) -> Vec<Effect> {
        let Some(practice) = self.c.practice.clone() else {
            return self.finish();
        };
        // No concurrent structural edit; said, not silently ignored (the menu
        // offers it either way).
        if self.refuses_structural_edit() {
            return self.finish();
        }
        // Pick the partner from the VISIBLE (active-interpretation) order —
        // the same list the UI's "Merge down" affordance is gated on. Using
        // the next take in the FULL list could grab (and then delete) a take
        // belonging to a DIFFERENT interpretation.
        let visible = practice.active_vocal_tracks();
        let v_idx = visible.iter().position(|t| t.id == track_id);
        let Some(v_idx) = v_idx.filter(|i| i + 1 < visible.len()) else {
            self.c.toast("No take below to merge with.");
            return self.finish();
        };
        let top = Arc::clone(&visible[v_idx]);
        let bottom = Arc::clone(&visible[v_idx + 1]);
        // Byte-concatenation joins exactly TWO files into one clip: a
        // multi-clip lane has several independent files (and its lane span
        // covers gaps no file byte maps to) — refuse rather than orphan the
        // other clips' audio.
        if top.clips.len() > 1 || bottom.clips.len() > 1 {
            self.c
                .toast("These takes have multiple clips and can't be merged into one lane.");
            return self.finish();
        }
        // Single-clip lanes: each lane's clip IS the lane (same span).
        let (Some(top_clip), Some(bottom_clip)) =
            (top.primary_clip().cloned(), bottom.primary_clip().cloned())
        else {
            self.c.toast("An empty take has no audio to merge.");
            return self.finish();
        };
        if !self.c.take_gate() {
            return self.finish();
        }
        let edits = Arc::clone(&self.c.edits);
        let analyze = Arc::clone(&self.c.analyze);
        let practice_id = self.c.practice_id.clone();
        let first = top_clip.audio_file_path.clone();
        let second = bottom_clip.audio_file_path.clone();
        let top_duration = top_clip.duration_ms;
        let merge = Merge {
            track_id: track_id.to_string(),
            top_id: top.id.clone(),
            bottom_id: bottom.id.clone(),
            top_clip,
            bottom_clip,
            merged: String::new(),
            merged_clip_id: String::new(),
            moved_track: false,
            moved_clip: false,
            prior_clip: None,
            moved_keys: Vec::new(),
        };
        self.c.job(
            JobKind::Merge,
            Pending::Edit(Box::new(EditStep::MergeJoined(Box::new(merge)))),
            move || {
                let merged = edits
                    .concat_wav(&practice_id, &first, &second)
                    .map_err(|e| Failure::from_edit(&e));
                let analysis = merged.as_ref().ok().map(|path| {
                    // Where the bottom audio now starts in the joined file:
                    // the top's true length, which a clip measured by an
                    // earlier build stores up to 10 ms short (its notes then
                    // landed that much early).
                    jobs::analyze_said(&*analyze, path)
                        .map(|w| (w, exact_duration_ms(&first, top_duration)))
                });
                Output::Merged(merged, analysis)
            },
        );
        self.finish()
    }

    fn merge_discard(&mut self, merge: Merge, outcome: Result<(), Failure>) {
        let paths = vec![merge.merged.clone()];
        self.discard_quietly(paths, EditStep::MergeDiscarded(Box::new(merge), outcome));
    }

    fn merge_joined(&mut self, mut merge: Merge, output: Output) {
        let (merged, analysis) = match output {
            Output::Merged(Ok(path), analysis) => (path, analysis),
            Output::Merged(Err(f), _) => return self.merge_finished(&merge.track_id, Err(f)),
            other => {
                let f = failure_of(&other)
                    .unwrap_or_else(|| Failure::Other("unexpected job output".into()));
                return self.merge_finished(&merge.track_id, Err(f));
            }
        };
        merge.merged = merged;
        // Until the model takes the new file, every way out discards it.
        let (analysis, top_ms) = match analysis {
            Some(Ok(pair)) => pair,
            Some(Err(f)) => return self.merge_discard(merge, Err(f)),
            None => {
                return self.merge_discard(merge, Err(Failure::Other("no analysis".into())));
            }
        };
        // Re-read the LIVE practice and re-find the indices after the
        // concat/analyze awaits (they shift if a take was added meanwhile)
        // so the merge never rebuilds from a stale snapshot and drops a
        // concurrent take.
        let Some(cur) = self.c.practice.clone() else {
            return self.merge_discard(merge, Ok(()));
        };
        let mut new_list = cur.vocal_tracks.clone();
        let idx = new_list.iter().position(|t| t.id == merge.top_id);
        let bottom_idx = new_list.iter().position(|t| t.id == merge.bottom_id);
        let (Some(idx), Some(bottom_idx)) = (idx, bottom_idx) else {
            return self.merge_discard(merge, Ok(()));
        };
        // Both lanes are re-read LIVE for the rebuild below: renames and
        // note/marker edits are NOT busy-gated, so folding the pre-await
        // snapshots in would overwrite one that landed during those awaits
        // (and a note added meanwhile, absent from the desired list, is
        // tombstoned by the save).
        let live_top = Arc::clone(&new_list[idx]);
        let live_bottom = Arc::clone(&new_list[bottom_idx]);
        if live_top.primary_clip().map(|c| c.id.as_str()) != Some(merge.top_clip.id.as_str())
            || live_bottom.primary_clip().map(|c| c.id.as_str())
                != Some(merge.bottom_clip.id.as_str())
            || live_top.clips.len() != 1
            || live_bottom.clips.len() != 1
        {
            // A lane's audio changed underneath (no gated path does this
            // today): abandon the merge rather than publish a file cut from
            // stale inputs.
            return self.merge_discard(merge, Ok(()));
        }
        // The clips themselves are re-read live as well: a drag (not gated)
        // moves a clip without changing its file, and the merged clip must
        // sit where the top clip is NOW, not where it was before the awaits.
        let live_top_clip = live_top.clips[0].clone();
        let live_bottom_clip = live_bottom.clips[0].clone();
        // Shift the bottom take's markers onto the merged timeline; range
        // markers shift their end by the same delta so the span is preserved.
        let delta = live_top_clip.start_ms + top_ms - live_bottom_clip.start_ms;
        // Fresh ids for the bottom take's notes/markers: the bottom track is
        // tombstoned but its child rows keep these ids, so reusing them on the
        // merged (top) track would collide on insert and roll the save back.
        let mut moved_keys = Vec::new();
        let bottom_id = merge.bottom_id.clone();
        let top_id = merge.top_id.clone();
        let mut move_to = |vm: &mut Self, old_id: &str| -> String {
            let fresh = new_id();
            moved_keys.push((bottom_id.clone(), old_id.to_string()));
            vm.c.moved_marks.insert(
                (bottom_id.clone(), old_id.to_string()),
                (top_id.clone(), fresh.clone()),
            );
            fresh
        };
        let mut notes = live_top.notes.clone();
        for n in &live_bottom.notes {
            let id = move_to(self, &n.id);
            notes.push(PracticeNote {
                id,
                position_ms: n.position_ms + delta,
                end_ms: n.end_ms.map(|e| e + delta),
                ..n.clone()
            });
        }
        notes.sort_by_key(|n| n.position_ms);
        let mut markers = live_top.technique_markers.clone();
        for m in &live_bottom.technique_markers {
            let id = move_to(self, &m.id);
            markers.push(TechniqueMarker {
                id,
                position_ms: m.position_ms + delta,
                end_ms: m.end_ms.map(|e| e + delta),
                ..m.clone()
            });
        }
        markers.sort_by_key(|m| m.position_ms);
        // The merged take keeps the top lane, but its clip is a NEW row —
        // never the top clip's id re-pointed at the merged file (see split:
        // the original recording would lose its last reference and come back
        // at the next boot as a "(recovered)" take).
        let merged_clip_id = new_id();
        let merged_track = AudioTrack {
            clips: vec![AudioClip {
                id: merged_clip_id.clone(),
                audio_file_path: merge.merged.clone(),
                duration_ms: analysis.duration_ms,
                waveform_peaks: analysis.peaks.clone(),
                ..live_top_clip
            }],
            notes,
            technique_markers: markers,
            ..(*live_top).clone()
        };
        new_list[idx] = Arc::new(merged_track);
        new_list.remove(bottom_idx);
        self.c.set_practice(SongPractice {
            vocal_tracks: new_list,
            ..(*cur).clone()
        });
        // The selection follows the audio into the merged lane: a selected
        // bottom lane (the take armed to record into, say) is the top one
        // now. Left on the absorbed lane, it fell back to the first lane, the
        // backing, and disarmed recording.
        merge.moved_track = self.c.selected_track_id.as_deref() == Some(merge.bottom_id.as_str());
        if merge.moved_track {
            self.c.selected_track_id = Some(merge.top_id.clone());
        }
        merge.prior_clip = self.c.selected_clip_id.clone();
        merge.moved_clip = merge.prior_clip.as_deref() == Some(merge.top_clip.id.as_str())
            || merge.prior_clip.as_deref() == Some(merge.bottom_clip.id.as_str());
        if merge.moved_clip {
            self.c.selected_clip_id = Some(merged_clip_id.clone());
        }
        merge.merged_clip_id = merged_clip_id;
        merge.moved_keys = moved_keys;
        // Persist BEFORE deleting any old audio, so a failed save can't leave
        // the model pointing at files that are already gone.
        self.save(SaveThen::Edit(Box::new(EditStep::MergeSaved(Box::new(
            merge,
        )))));
    }

    fn merge_saved(&mut self, merge: Merge, result: Result<bool, Failure>) {
        match result {
            Err(f) => self.merge_finished(&merge.track_id, Err(f)),
            Ok(true) => {
                self.analysis.drop_lane_state(&merge.bottom_id); // the absorbed take is gone
                let top = merge.top_clip.audio_file_path.clone();
                let bottom = merge.bottom_clip.audio_file_path.clone();
                self.evict_unused(&top);
                self.evict_unused(&bottom);
                self.run_deletes(
                    VecDeque::from([Doomed::plain(top), Doomed::plain(bottom)]),
                    DeleteMode::Propagate,
                    EditStep::MergeSourcesDeleted(Box::new(merge)),
                );
            }
            Ok(false) => {
                // The failed save reverted the model to the two original
                // takes, so the merged output is an orphan — clean it up
                // instead. The selection goes back with the takes, unless the
                // user moved it meanwhile. The revert's repair has already let
                // go of the merged clip, which it no longer finds.
                let ours = self.c.selected_track_id.as_deref() == Some(merge.top_id.as_str())
                    && (self.c.selected_clip_id.as_deref() == Some(merge.merged_clip_id.as_str())
                        || self.c.selected_clip_id.is_none());
                if ours && (merge.moved_track || merge.moved_clip) {
                    if merge.moved_track {
                        self.c.selected_track_id = Some(merge.bottom_id.clone());
                    }
                    if merge.moved_clip {
                        self.c.selected_clip_id = merge.prior_clip.clone();
                    }
                }
                for key in &merge.moved_keys {
                    self.c.moved_marks.remove(key);
                }
                self.merge_discard(merge, Ok(()));
            }
        }
    }

    fn merge_finished(&mut self, track_id: &str, outcome: Result<(), Failure>) {
        self.c.finish_gate(outcome);
        // The merged take keeps the top take's id but now plays a new file.
        self.analysis_call(|a, h| a.on_track_audio_changed(track_id, h));
    }

    /// Recomputes the waveform (and invalidates the cached spectrogram) for
    /// every clip on a lane — useful after its audio files changed
    /// underneath.
    #[must_use]
    pub fn reanalyze_track(&mut self, track_id: &str) -> Vec<Effect> {
        let Some(track) = self.c.track_by_id(track_id) else {
            return self.finish();
        };
        // No concurrent structural edit; said, not silently ignored (the menu
        // offers it either way).
        if self.refuses_structural_edit() {
            return self.finish();
        }
        if !self.c.take_gate() {
            return self.finish();
        }
        // Analyze EVERY clip's file (a lane can hold several independent
        // files), then fold the results back in BY CLIP ID against the live
        // model — a clip added/removed during the analyze awaits must survive
        // untouched. Only what the analysis measured is folded in: a clip
        // dragged during the awaits keeps its new position (the pre-await
        // copy snapped it back).
        let queue: VecDeque<(String, String)> = track
            .clips
            .iter()
            .map(|c| (c.id.clone(), c.audio_file_path.clone()))
            .collect();
        self.reanalyze_next(track_id.to_string(), queue, HashMap::new());
        self.finish()
    }

    fn reanalyze_next(
        &mut self,
        track_id: String,
        mut queue: VecDeque<(String, String)>,
        analyzed: HashMap<String, WaveformResult>,
    ) {
        if let Some((clip_id, path)) = queue.pop_front() {
            let analyze = Arc::clone(&self.c.analyze);
            let target = path.clone();
            self.c.job(
                JobKind::Analyze,
                Pending::Edit(Box::new(EditStep::ReanalyzeAnalyzed {
                    track_id,
                    clip_id,
                    path,
                    queue,
                    analyzed,
                })),
                move || Output::Analyzed(jobs::analyze_said(&*analyze, &target)),
            );
            return;
        }
        self.c.update_track(&track_id, &|t| AudioTrack {
            clips: t
                .clips
                .iter()
                .map(|c| match analyzed.get(&c.id) {
                    Some(w) => AudioClip {
                        duration_ms: w.duration_ms,
                        waveform_peaks: w.peaks.clone(),
                        ..c.clone()
                    },
                    None => c.clone(),
                })
                .collect(),
            ..t.clone()
        });
        self.save(SaveThen::Edit(Box::new(EditStep::ReanalyzeSaved {
            track_id,
        })));
    }

    fn reanalyze_finished(&mut self, track_id: &str, outcome: Result<(), Failure>) {
        self.c.finish_gate(outcome);
        self.analysis_call(|a, h| a.on_track_audio_changed(track_id, h));
    }

    // --- Notes -------------------------------------------------------------------------

    /// Adds a note at `start_ms` (a point), or across `start_ms..end_ms` when
    /// `end_ms` is given and the span is long enough.
    #[must_use]
    pub fn add_note(
        &mut self,
        track_id: &str,
        start_ms: i64,
        end_ms: Option<i64>,
        content: &str,
    ) -> Vec<Effect> {
        let now = DateTime::now();
        let (s, e) = normalize_marker_range(start_ms, end_ms);
        let note = PracticeNote {
            id: new_id(),
            position_ms: s,
            end_ms: e,
            content: content.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.c.update_track(track_id, &|t| {
            let mut notes = t.notes.clone();
            notes.push(note.clone());
            notes.sort_by_key(|n| n.position_ms);
            AudioTrack { notes, ..t.clone() }
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    #[must_use]
    pub fn add_note_at_playhead(&mut self, track_id: &str, content: &str) -> Vec<Effect> {
        let at = i64::try_from(self.position().as_millis()).unwrap_or(i64::MAX);
        self.add_note(track_id, at, None, content)
    }

    /// Sets a note's text. The rest of the note is taken as it stands now,
    /// not from the copy the editor opened with: a split landing while the
    /// editor was up (or a sync) can have clamped its end since, and writing
    /// the copy back undid that.
    #[must_use]
    pub fn update_note_content(
        &mut self,
        track_id: &str,
        note_id: &str,
        content: &str,
    ) -> Vec<Effect> {
        let (track_id, note_id) = self.mark_now(track_id, note_id);
        let now = DateTime::now();
        self.c.update_track(&track_id, &|t| AudioTrack {
            notes: t
                .notes
                .iter()
                .map(|n| {
                    if n.id == note_id {
                        PracticeNote {
                            content: content.to_string(),
                            updated_at: now,
                            ..n.clone()
                        }
                    } else {
                        n.clone()
                    }
                })
                .collect(),
            ..t.clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    #[must_use]
    pub fn delete_note(&mut self, track_id: &str, note_id: &str) -> Vec<Effect> {
        let (track_id, note_id) = self.mark_now(track_id, note_id);
        self.c.update_track(&track_id, &|t| AudioTrack {
            notes: t
                .notes
                .iter()
                .filter(|n| n.id != note_id)
                .cloned()
                .collect(),
            ..t.clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    // --- Technique markers -------------------------------------------------------------

    /// The technique library, or `None` when it could not be read: taken as
    /// an empty list, the picker told the user to add techniques they had.
    pub fn available_techniques(&mut self) -> Started {
        let call = self.next_call();
        let repo = Arc::clone(&self.c.technique_repository);
        self.c
            .job(JobKind::Techniques, Pending::Techniques(call), move || {
                Output::Techniques(repo.list_techniques().ok())
            });
        self.started(call)
    }

    /// Adds a technique marker at `start_ms` (a point), or across
    /// `start_ms..end_ms` when `end_ms` is given and the span is long enough.
    #[must_use]
    pub fn add_technique(
        &mut self,
        track_id: &str,
        technique_id: &str,
        technique_name: &str,
        start_ms: i64,
        end_ms: Option<i64>,
    ) -> Vec<Effect> {
        let now = DateTime::now();
        let (s, e) = normalize_marker_range(start_ms, end_ms);
        let marker = TechniqueMarker {
            id: new_id(),
            position_ms: s,
            end_ms: e,
            technique_id: technique_id.to_string(),
            technique_name: technique_name.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.c.update_track(track_id, &|t| {
            let mut markers = t.technique_markers.clone();
            markers.push(marker.clone());
            markers.sort_by_key(|m| m.position_ms);
            AudioTrack {
                technique_markers: markers,
                ..t.clone()
            }
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    #[must_use]
    pub fn delete_technique_marker(&mut self, track_id: &str, marker_id: &str) -> Vec<Effect> {
        let (track_id, marker_id) = self.mark_now(track_id, marker_id);
        self.c.update_track(&track_id, &|t| AudioTrack {
            technique_markers: t
                .technique_markers
                .iter()
                .filter(|m| m.id != marker_id)
                .cloned()
                .collect(),
            ..t.clone()
        });
        self.save(SaveThen::Nothing);
        self.finish()
    }

    /// The (take, id) of a note or marker as the model holds it now: the pair
    /// given while it is still there, else wherever splits and merges moved
    /// it.
    fn mark_now(&self, track_id: &str, id: &str) -> (String, String) {
        let mut at = (track_id.to_string(), id.to_string());
        for _ in 0..16 {
            if self.holds_mark(&at) {
                break;
            }
            match self.c.moved_marks.get(&at) {
                Some(next) => at = next.clone(),
                None => break,
            }
        }
        at
    }

    fn holds_mark(&self, at: &(String, String)) -> bool {
        self.c.track_by_id(&at.0).is_some_and(|t| {
            t.notes.iter().any(|n| n.id == at.1) || t.technique_markers.iter().any(|m| m.id == at.1)
        })
    }
}

/// The display name without a trailing " (Instrumental)" (Dart's
/// `replaceAll(RegExp(r'\s*\(Instrumental\)$'), '')`).
fn strip_instrumental(name: &str) -> String {
    match name.strip_suffix("(Instrumental)") {
        Some(rest) => rest
            .trim_end_matches(vocara_model::util::dart_string::is_whitespace)
            .to_string(),
        None => name.to_string(),
    }
}
