//! Vocal-take versions ("interpretations") of a song practice (port of
//! `ui/song_practice_detail/view_model/interpretations_controller.dart`): the
//! list/active getters and the four mutators — switch (pause → save →
//! resume), create-and-switch, rename, and delete-with-file-cleanup under the
//! shared busy gate.
//!
//! All state lives in the practice model (versions / active version id),
//! edited through the host ([`InterpretationsHost`], Dart's injected
//! closures); the engine is driven directly so the playing flag stays
//! stream-driven.
//!
//! # How Dart's awaits map here
//!
//! A mutator runs up to its first I/O and returns the effect the screen runs
//! ([`InterpretationsEffect`]), tagged with the operation it belongs to; the
//! result comes back through [`InterpretationsController::resynced`],
//! [`InterpretationsController::saved`] or
//! [`InterpretationsController::cleaned_up`], which run the Dart code after
//! that `await`. The engine's `pause` / `play` return at once here (see
//! `PlaybackEngine`), so the Dart awaits on them are plain calls; the live
//! practice is still re-read after the pause, as Dart re-read it.
//!
//! The delete runs under the screen's ONE busy gate from its first step to
//! its file cleanup: the gate is taken as the delete starts (Dart's
//! `busyGate.run`), held across the resync and the save, and its guarded
//! action — run on a worker by the screen, then handed to
//! [`InterpretationsController::cleaned_up`] — is the deletion of the
//! dropped takes' audio files.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_string;
use vocara_model::util::id::new_id;
use vocara_services::audio_file_service::AudioFiles;
use vocara_services::playback_engine::PlaybackEngine;

use super::analysis_cache::AnalysisCache;
use crate::util::busy_gate::{BusyGate, BusyOutcome, BusyRun};
use crate::util::ActionError;

#[cfg(test)]
mod tests;

/// What the controller needs of the analysis cache (Dart's tests faked the
/// cache for exactly these).
pub trait InterpretationsAnalysis {
    fn dock_open(&self) -> bool;
    fn dock_shows_take(&self) -> bool;
    fn drop_lane_state(&mut self, track_id: &str);
    fn evict_analysis(&mut self, path: &str, recompute: bool);
}

impl InterpretationsAnalysis for AnalysisCache {
    fn dock_open(&self) -> bool {
        AnalysisCache::dock_open(self)
    }

    fn dock_shows_take(&self) -> bool {
        AnalysisCache::dock_shows_take(self)
    }

    fn drop_lane_state(&mut self, track_id: &str) {
        AnalysisCache::drop_lane_state(self, track_id);
    }

    fn evict_analysis(&mut self, path: &str, recompute: bool) {
        AnalysisCache::evict_analysis(self, path, recompute);
    }
}

/// What the controller reads and edits on its host view model (Dart's
/// injected closures).
pub trait InterpretationsHost {
    /// The practice as the model holds it now.
    fn practice(&self) -> Option<&SongPractice>;
    /// Replaces the model's practice.
    fn set_practice(&mut self, practice: SongPractice);
    /// Re-points the selection at what the model now shows (and asks for its
    /// analysis).
    fn ensure_selection(&mut self);
    fn toast(&mut self, message: String);
    /// Whether a take is in flight: the whole take window, not just the
    /// armed span.
    fn is_recording(&self) -> bool;
    fn is_separating(&self) -> bool;
    /// What the take in flight ([`InterpretationsHost::is_recording`]) makes
    /// the user wait for when there is nothing left to stop (stopped and
    /// saving, or a start being cancelled); `None` while there is a take to
    /// stop.
    fn take_wait(&self) -> Option<String> {
        None
    }
    /// The user's transport presses so far (the playback controller's
    /// `transport_moves`).
    fn transport_moves(&self) -> u64 {
        0
    }
    /// The screen's ONE busy gate, lent to every controller.
    fn busy_gate(&mut self) -> &mut BusyGate;
    /// The analysis cache.
    fn analysis(&mut self) -> &mut dyn InterpretationsAnalysis;
}

/// An operation in flight, tagging its effects and their results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VersionsOp(u64);

/// Work the screen runs for the controller.
pub enum InterpretationsEffect {
    /// Load the engine with the takes the model now shows (the mixer's
    /// `resync_playback`), then call [`InterpretationsController::resynced`].
    Resync(VersionsOp),
    /// Persist the practice (the view model's save, which reports whether it
    /// landed), then call [`InterpretationsController::saved`].
    Save(VersionsOp),
    /// Ask for the selected take's analysis again
    /// (`AnalysisCache::ensure_selected_spectrogram`); nothing waits for it.
    EnsureSelectedAnalysis,
    /// Run the busy gate's guarded action on a worker
    /// ([`BusyRun::into_task`]) and hand its outcome to
    /// [`InterpretationsController::cleaned_up`].
    CleanUp(VersionsOp, BusyRun<String>),
}

impl std::fmt::Debug for InterpretationsEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterpretationsEffect::Resync(op) => f.debug_tuple("Resync").field(op).finish(),
            InterpretationsEffect::Save(op) => f.debug_tuple("Save").field(op).finish(),
            InterpretationsEffect::EnsureSelectedAnalysis => f.write_str("EnsureSelectedAnalysis"),
            InterpretationsEffect::CleanUp(op, _) => f.debug_tuple("CleanUp").field(op).finish(),
        }
    }
}

/// Where an operation waits.
enum Op {
    SetActive {
        stage: Stage,
        was_playing: bool,
        moves: u64,
    },
    Create {
        stage: Stage,
    },
    Rename,
    Delete {
        stage: Stage,
        doomed: Vec<Arc<AudioTrack>>,
        /// The gate's run, taken as the delete started; its action deletes
        /// what `paths` holds once it runs.
        run: BusyRun<String>,
        paths: Arc<Mutex<Vec<String>>>,
    },
    /// A delete's guarded action is running; the gate is handed back as it
    /// lands.
    CleanUp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Resync,
    Save,
}

/// Vocal-take versions ("interpretations") of a song practice (see the
/// module docs).
pub struct InterpretationsController {
    playback: Arc<dyn PlaybackEngine>,
    audio_files: Arc<dyn AudioFiles>,
    /// Set in [`InterpretationsController::dispose`]; guards notifications
    /// racing the tear-down.
    disposed: bool,
    revision: u64,
    next_op: u64,
    ops: HashMap<VersionsOp, Op>,
}

impl InterpretationsController {
    pub fn new(engine: Arc<dyn PlaybackEngine>, audio_files: Arc<dyn AudioFiles>) -> Self {
        InterpretationsController {
            playback: engine,
            audio_files,
            disposed: false,
            revision: 0,
            next_op: 0,
            ops: HashMap::new(),
        }
    }

    /// Bumped on every Dart `notifyListeners` (never after dispose).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision += 1;
    }

    fn op(&mut self, op: Op) -> VersionsOp {
        self.next_op += 1;
        let id = VersionsOp(self.next_op);
        self.ops.insert(id, op);
        id
    }

    /// Refuses `action` while a take is in flight, saying what to wait for.
    fn refused_for_take(host: &mut dyn InterpretationsHost, action: &str) -> bool {
        if !host.is_recording() {
            return false;
        }
        let message = host
            .take_wait()
            .unwrap_or_else(|| format!("Stop recording to {action}."));
        host.toast(message);
        true
    }

    /// All interpretations of this song, in creation order.
    pub fn all(practice: Option<&SongPractice>) -> Vec<Arc<TakeVersion>> {
        practice.map(|p| p.versions.clone()).unwrap_or_default()
    }

    /// The interpretation currently shown and played.
    pub fn active(practice: Option<&SongPractice>) -> Option<Arc<TakeVersion>> {
        practice.and_then(|p| p.active_version().cloned())
    }

    /// Vocal takes belonging to the active interpretation — the only takes
    /// shown in the lane list and loaded into playback.
    pub fn vocal_takes(practice: Option<&SongPractice>) -> Vec<Arc<AudioTrack>> {
        practice
            .map(SongPractice::active_vocal_tracks)
            .unwrap_or_default()
    }

    /// The name a new interpretation gets when it is given none:
    /// "Interpretation N" numbered on from how many there are, past any name
    /// already taken. Numbered by the count alone, a second "Interpretation
    /// 2" followed the first of two being deleted.
    pub fn next_default_name(practice: Option<&SongPractice>) -> String {
        let all = Self::all(practice);
        let mut n = all.len() + 1;
        while all.iter().any(|v| v.name == format!("Interpretation {n}")) {
            n += 1;
        }
        format!("Interpretation {n}")
    }

    /// Switches which interpretation is shown/played; the other
    /// interpretations' takes stop playing. Because the loaded track set
    /// changes, this is a pause → reload → resume (playback continues from
    /// the same spot if it was playing).
    #[must_use]
    pub fn set_active(
        &mut self,
        version_id: &str,
        host: &mut dyn InterpretationsHost,
    ) -> Vec<InterpretationsEffect> {
        let Some(practice) = host.practice() else {
            return Vec::new();
        };
        // The interpretation ON SCREEN, which a stored id that resolves to
        // none falls back from (see SongPractice::active_version): picking it
        // is no switch, and must not pause, save and resume playback for
        // nothing.
        if practice.active_version().map(|v| v.id.as_str()) == Some(version_id) {
            return Vec::new();
        }
        if Self::refused_for_take(host, "switch interpretation") {
            return Vec::new();
        }
        let Some(practice) = host.practice() else {
            return Vec::new();
        };
        if !practice.versions.iter().any(|v| v.id == version_id) {
            return Vec::new();
        }
        let was_playing = self.playback.is_playing();
        self.playback.pause();
        let moves = host.transport_moves();
        // Re-read the LIVE practice after the pause (the rule `delete`
        // follows): a take recorded/imported during it must survive the
        // rebuild, and the target interpretation may have been deleted
        // meanwhile. An early return here must hand playback back (it was
        // paused above but nothing changed), not strand the transport
        // silently paused.
        let cur = match host.practice() {
            Some(cur)
                if cur.active_version_id.as_deref() != Some(version_id)
                    && cur.versions.iter().any(|v| v.id == version_id) =>
            {
                cur.clone()
            }
            _ => {
                if was_playing && !self.disposed {
                    self.playback.play();
                }
                return Vec::new();
            }
        };
        host.set_practice(SongPractice {
            active_version_id: Some(version_id.to_string()),
            ..cur
        });
        host.ensure_selection();
        // Shown now, not once the save lands: it queues behind any save
        // already in flight, and until then the screen showed the old
        // interpretation while the selection and REC already acted on the
        // new one. The engine too, or Play meanwhile played the old takes
        // under the new lanes.
        self.notify();
        let op = self.op(Op::SetActive {
            stage: Stage::Resync,
            was_playing,
            moves,
        });
        vec![InterpretationsEffect::Resync(op)]
    }

    /// Whether [`InterpretationsController::create`] would be refused right
    /// now, saying why when it would. Asked before the name dialog opens: the
    /// name typed into it was refused only once the dialog closed, and lost.
    pub fn refuses_create(&self, host: &mut dyn InterpretationsHost) -> bool {
        Self::refused_for_take(host, "add an interpretation")
    }

    /// Creates a new (empty) interpretation and switches to it, ready to
    /// record a fresh emotional/technical reading of the song.
    #[must_use]
    pub fn create(
        &mut self,
        name: &str,
        host: &mut dyn InterpretationsHost,
    ) -> Vec<InterpretationsEffect> {
        if host.practice().is_none() {
            return Vec::new();
        }
        if Self::refused_for_take(host, "add an interpretation") {
            return Vec::new();
        }
        let trimmed = dart_string::trim(name).to_string();
        self.playback.pause();
        // Re-read the LIVE practice after the pause (see set_active): a take
        // recorded/imported during it must survive the rebuild, and the
        // default name numbers against the current version count.
        let Some(cur) = host.practice().cloned() else {
            return Vec::new();
        };
        let version = Arc::new(TakeVersion::new(
            new_id(),
            if trimmed.is_empty() {
                Self::next_default_name(Some(&cur))
            } else {
                trimmed
            },
            DateTime::now(),
        ));
        let mut versions = cur.versions.clone();
        versions.push(Arc::clone(&version));
        host.set_practice(SongPractice {
            versions,
            active_version_id: Some(version.id.clone()),
            ..cur
        });
        host.ensure_selection();
        self.notify(); // shown and loaded now (see set_active)
        let op = self.op(Op::Create {
            stage: Stage::Resync,
        });
        vec![InterpretationsEffect::Resync(op)]
    }

    #[must_use]
    pub fn rename(
        &mut self,
        version_id: &str,
        name: &str,
        host: &mut dyn InterpretationsHost,
    ) -> Vec<InterpretationsEffect> {
        let trimmed = dart_string::trim(name);
        let Some(practice) = host.practice() else {
            return Vec::new();
        };
        if trimmed.is_empty() {
            return Vec::new();
        }
        let versions = practice
            .versions
            .iter()
            .map(|v| {
                if v.id == version_id {
                    Arc::new(TakeVersion {
                        name: trimmed.to_string(),
                        ..(**v).clone()
                    })
                } else {
                    Arc::clone(v)
                }
            })
            .collect();
        let next = SongPractice {
            versions,
            ..practice.clone()
        };
        host.set_practice(next);
        self.notify(); // shown now (see set_active)
        let op = self.op(Op::Rename);
        vec![InterpretationsEffect::Save(op)]
    }

    /// Deletes an interpretation and every take in it (audio files
    /// included). Refuses to remove the last one — a practice always keeps
    /// one.
    #[must_use]
    pub fn delete(
        &mut self,
        version_id: &str,
        host: &mut dyn InterpretationsHost,
    ) -> Vec<InterpretationsEffect> {
        let Some(versions) = host.practice().map(|p| p.versions.len()) else {
            return Vec::new();
        };
        if Self::refused_for_take(host, "delete an interpretation") {
            return Vec::new();
        }
        // No concurrent structural edit; said, not silently ignored, and in
        // the words the take menu's refusals use.
        if host.is_separating() {
            host.toast("Wait for the separation to finish.".to_string());
            return Vec::new();
        }
        if host.busy_gate().busy() {
            host.toast("Wait for the current edit to finish.".to_string());
            return Vec::new();
        }
        if versions <= 1 {
            host.toast("Keep at least one interpretation.".to_string());
            return Vec::new();
        }
        // The guarded action: the dropped takes' files, once the save landed
        // (filled in by `saved`; empty when the delete ends early).
        let paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let run = {
            let paths = Arc::clone(&paths);
            let files = Arc::clone(&self.audio_files);
            host.busy_gate().run(move || {
                let paths =
                    std::mem::take(&mut *paths.lock().unwrap_or_else(PoisonError::into_inner));
                for path in paths {
                    files.delete_audio(&path).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        };
        let Some(run) = run else {
            return Vec::new();
        };
        self.playback.pause();
        // Re-read the LIVE practice after the pause so a take recorded into a
        // DIFFERENT interpretation meanwhile isn't dropped by a
        // stale-snapshot rebuild; re-check the guard too (it may have
        // changed meanwhile).
        let cur = match host.practice() {
            Some(cur) if cur.versions.len() > 1 => cur.clone(),
            _ => return self.end_delete(run),
        };
        let doomed: Vec<Arc<AudioTrack>> = cur
            .vocal_tracks
            .iter()
            .filter(|t| t.version_id.as_deref() == Some(version_id))
            .cloned()
            .collect();
        let remaining: Vec<Arc<TakeVersion>> = cur
            .versions
            .iter()
            .filter(|v| v.id != version_id)
            .cloned()
            .collect();
        let Some(first) = remaining.first() else {
            // The target was the only one left.
            return self.end_delete(run);
        };
        let active_version_id = if cur.active_version_id.as_deref() == Some(version_id) {
            Some(first.id.clone())
        } else {
            cur.active_version_id.clone()
        };
        let vocal_tracks = cur
            .vocal_tracks
            .iter()
            .filter(|t| t.version_id.as_deref() != Some(version_id))
            .cloned()
            .collect();
        host.set_practice(SongPractice {
            versions: remaining,
            active_version_id,
            vocal_tracks,
            ..cur
        });
        host.ensure_selection();
        self.notify(); // shown and loaded now (see set_active)
        let op = self.op(Op::Delete {
            stage: Stage::Resync,
            doomed,
            run,
            paths,
        });
        vec![InterpretationsEffect::Resync(op)]
    }

    /// A delete that ends before its edit: the gate's run, with nothing to
    /// delete, so the gate is handed back once it lands.
    fn end_delete(&mut self, run: BusyRun<String>) -> Vec<InterpretationsEffect> {
        let op = self.op(Op::CleanUp);
        vec![InterpretationsEffect::CleanUp(op, run)]
    }

    /// The engine was loaded with the takes the model shows: persist.
    #[must_use]
    pub fn resynced(&mut self, op: VersionsOp) -> Vec<InterpretationsEffect> {
        let Some(entry) = self.ops.get_mut(&op) else {
            return Vec::new();
        };
        let stage = match entry {
            Op::SetActive { stage, .. } | Op::Create { stage } | Op::Delete { stage, .. } => stage,
            Op::Rename | Op::CleanUp => return Vec::new(),
        };
        if *stage != Stage::Resync {
            return Vec::new();
        }
        *stage = Stage::Save;
        vec![InterpretationsEffect::Save(op)]
    }

    /// The save landed (`ok`: it persisted).
    #[must_use]
    pub fn saved(
        &mut self,
        op: VersionsOp,
        ok: bool,
        host: &mut dyn InterpretationsHost,
    ) -> Vec<InterpretationsEffect> {
        let Some(entry) = self.ops.remove(&op) else {
            return Vec::new();
        };
        match entry {
            Op::SetActive {
                stage: Stage::Save,
                was_playing,
                moves,
            } => {
                let mut effects = Vec::new();
                let analysis = host.analysis();
                if analysis.dock_open() && analysis.dock_shows_take() {
                    effects.push(InterpretationsEffect::EnsureSelectedAnalysis);
                }
                // The route can pop while the save is in flight; don't drive
                // the disposed engine wiring back into play.
                if self.disposed {
                    return effects;
                }
                // Nor over what the user did while the save queued: a Stop, a
                // seek, a Play or Pause of their own, or a take now arming.
                // (Not judged by the playhead: a render under way at the
                // pause still moved it one buffer, and the switch then stayed
                // paused for no reason.)
                if was_playing
                    && !host.is_recording()
                    && !self.playback.is_playing()
                    && host.transport_moves() == moves
                {
                    self.playback.play();
                }
                effects
            }
            Op::Create { stage: Stage::Save } | Op::Rename => Vec::new(),
            Op::Delete {
                stage: Stage::Save,
                doomed,
                run,
                paths,
            } => {
                // Persist the dropped state BEFORE deleting files, and ONLY
                // delete when the save succeeded — a failed save reverts the
                // model back to referencing these takes, so their audio must
                // survive (delete_audio already refuses pooled files; this
                // gate also covers any not-yet-pooled working file).
                if ok {
                    let mut doomed_paths = Vec::new();
                    for t in &doomed {
                        host.analysis().drop_lane_state(&t.id);
                        // Delete EVERY clip's file, not just the primary — a
                        // multi-clip take would otherwise orphan its trailing
                        // clips' audio.
                        for c in &t.clips {
                            // Another interpretation's take may play the same
                            // pooled bytes: its analysis stays (see
                            // SongPractice::references_audio).
                            let referenced = host
                                .practice()
                                .is_some_and(|p| p.references_audio(&c.audio_file_path));
                            if !referenced {
                                host.analysis().evict_analysis(&c.audio_file_path, false);
                            }
                            doomed_paths.push(c.audio_file_path.clone());
                        }
                    }
                    *paths.lock().unwrap_or_else(PoisonError::into_inner) = doomed_paths;
                }
                self.end_delete(run)
            }
            other => {
                // Not waiting on a save: keep it where it was.
                self.ops.insert(op, other);
                Vec::new()
            }
        }
    }

    /// The delete's guarded action landed: the gate is handed back. Returns
    /// the gate's error (Dart's `onError`), for the screen to surface.
    pub fn cleaned_up(
        &mut self,
        op: VersionsOp,
        outcome: BusyOutcome<String>,
        host: &mut dyn InterpretationsHost,
    ) -> Option<ActionError<String>> {
        match self.ops.remove(&op) {
            Some(Op::CleanUp) => host.busy_gate().finish(outcome),
            Some(other) => {
                self.ops.insert(op, other);
                None
            }
            None => None,
        }
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
    }
}
