//! Loading, saving and freshness: the load (with its landing grace and the
//! catch-up behind a stuck save), the save chain, the remote-applied reload,
//! and the adoption rules every read and write goes through.

use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use vocara_model::models::audio_track::AudioClip;
use vocara_model::models::song_practice::SongPractice;
use vocara_services::graph_audio_playback_service::SyncError;
use vocara_services::recording_finalizer::{Hold, Landing};
use vocara_store::sync::sync_hashes::Loaded;

use super::super::interpretations_controller::VersionsOp;
use super::super::recording_controller::RecordingOp;
use super::edits::{EditStep, EmptyTake, TakeBack};
use super::jobs::{Failure, Output};
use super::lyrics::LyricsThen;
use super::{Call, JobKind, SongPracticeDetailViewModel};

/// How long a reopen waits for what the last screen on this practice left
/// landing before it opens on the store as it is (see `start_load`).
const LANDING_GRACE: Duration = Duration::from_millis(300);

/// What a job's result resumes.
pub(in super::super) enum Pending {
    Probe,
    LoadGrace,
    LoadRead,
    CatchUpSettled,
    CatchUpRead,
    Reload {
        epoch: u64,
        before: Arc<SongPractice>,
        then: ReloadThen,
    },
    /// The running save's current stage.
    Save,
    Resync(ResyncThen),
    Edit(Box<EditStep>),
    Export,
    Techniques(Call),
    LyricsFile {
        call: Call,
        lrc: bool,
    },
}

/// What a remote-applied reload's end resumes.
pub(in super::super) enum ReloadThen {
    Nothing,
    /// The catch-up that fell back to it (the gate was taken) is done.
    CatchUpDone,
}

/// What a resync's end resumes (Dart's `await mixer.resyncPlayback()`).
pub(in super::super) enum ResyncThen {
    Recording(RecordingOp),
    Versions(VersionsOp),
    Load,
    CatchUp,
    Reload(ReloadThen),
    Edit(Box<EditStep>),
}

/// The catch-up behind a stuck save (see `start_catch_up`).
pub(in super::super) struct CatchUp {
    /// Whether it holds the busy gate (unlisted).
    guarded: bool,
    settled: bool,
    adopted: bool,
}

/// What a save persists and how (the three bodies Dart queued).
pub(in super::super) enum SaveKind {
    /// `_doSave`.
    Practice,
    /// `_writeLyrics() && _reverts == reverts`.
    Lyrics { reverts: u64 },
    /// The separation's `_doSave`, taken back inside the save when it did
    /// not land.
    Separation(Box<TakeBack>),
}

/// What a save's caller does with its answer (the code after Dart's
/// `await _save()`).
pub(in super::super) enum SaveThen {
    Nothing,
    Recording(RecordingOp),
    Versions(VersionsOp),
    Lyrics(LyricsThen),
    EmptyTake(Box<EmptyTake>),
    Edit(Box<EditStep>),
    CommitMove,
}

/// A save queued behind the one running, holding the quit and listed as
/// landing from the moment it was queued: an edit already made in the UI
/// must not be lost to a Cmd+Q that exits between the edit and its commit,
/// and a screen reopened on the practice meanwhile must wait for it.
pub(in super::super) struct QueuedSave {
    kind: SaveKind,
    then: SaveThen,
    landing: Landing,
    hold: Hold,
}

/// Where the running save waits.
enum Stage {
    Writing,
    /// `_catchUpOnStore`'s read, for the model as adopted.
    CatchingUp {
        adopted: Option<Arc<SongPractice>>,
        sent_hashes: Option<IndexMap<String, String>>,
    },
    /// The failed write's re-read of the disk.
    Reverting,
    /// `_doSave`'s closing resync.
    Tail,
    /// `_writeLyrics`'s resync after a merge, before its end.
    LyricsResync,
    /// `_writeLyrics`'s resync after a revert, before its toast and end.
    LyricsRevertResync,
}

/// The save in flight.
pub(in super::super) struct RunningSave {
    kind: SaveKind,
    then: SaveThen,
    landing: Landing,
    hold: Hold,
    sent: Arc<SongPractice>,
    stage: Stage,
    ok: bool,
}

fn loaded_of(output: Output) -> Result<Loaded<SongPractice>, String> {
    match output {
        Output::Loaded(r) | Output::Saved(r) => r,
        Output::Panicked(m) => Err(m),
        _ => Err("unexpected job output".into()),
    }
}

fn synced_of(output: Output) -> Result<(), SyncError> {
    match output {
        Output::Synced(r) => r,
        Output::Panicked(m) => Err(SyncError::Panicked(m)),
        _ => Err(SyncError::Panicked("unexpected job output".into())),
    }
}

/// Whether a clip `from` holds sits at another path in `to` — a re-read
/// re-keying its lane's analysis (see `adopt_loaded`). Only then are the
/// folded-out lanes refreshed: refreshing on every save recomputed lanes the
/// analysis budget had evicted, a full decode per fader release.
fn clip_paths_moved(from: Option<&SongPractice>, to: &SongPractice) -> bool {
    let Some(from) = from else {
        return false;
    };
    fn clips_of(p: &SongPractice) -> Vec<&AudioClip> {
        let mut out = Vec::new();
        if let Some(a) = &p.accompaniment {
            out.extend(a.clips.iter());
        }
        if let Some(r) = &p.reference {
            out.extend(r.clips.iter());
        }
        for t in &p.vocal_tracks {
            out.extend(t.clips.iter());
        }
        out
    }
    let paths: std::collections::HashMap<&str, &str> = clips_of(from)
        .into_iter()
        .map(|c| (c.id.as_str(), c.audio_file_path.as_str()))
        .collect();
    clips_of(to).into_iter().any(|c| {
        paths
            .get(c.id.as_str())
            .is_some_and(|was| *was != c.audio_file_path)
    })
}

impl SongPracticeDetailViewModel {
    // --- Jobs ------------------------------------------------------------------------

    /// A job's result: the rest of the Dart function that awaited it.
    #[must_use]
    pub fn job_done(&mut self, done: super::JobDone) -> Vec<super::Effect> {
        let Some(pending) = self.c.pending.remove(&done.id) else {
            return self.finish();
        };
        let output = done.output;
        match pending {
            Pending::Probe => {
                if let Output::Available(available) = output {
                    self.c.separation_available = available;
                }
                self.c.notify();
            }
            Pending::LoadGrace => {
                let settled = matches!(output, Output::Settled(true));
                if !settled {
                    self.start_catch_up();
                }
                self.load_read();
            }
            Pending::LoadRead => self.load_landed(loaded_of(output)),
            Pending::CatchUpSettled => {
                if let Some(cu) = self.c.catch_up.as_mut() {
                    cu.settled = true;
                }
                self.catch_up_ready();
            }
            Pending::CatchUpRead => match loaded_of(output) {
                Ok(loaded) if !self.c.disposed => {
                    self.adopt_loaded(loaded);
                    self.resync(ResyncThen::CatchUp);
                }
                _ => self.end_catch_up(Ok(())),
            },
            Pending::Reload {
                epoch,
                before,
                then,
            } => self.reload_landed(epoch, &before, loaded_of(output), then),
            Pending::Save => self.save_step(output),
            Pending::Resync(then) => self.resynced(then, synced_of(output)),
            Pending::Edit(step) => self.edit_job_done(*step, output),
            Pending::Export => {
                if let Output::Exported(Some(said)) = output {
                    self.c.toast(&said);
                }
            }
            Pending::Techniques(call) => {
                let techniques = match output {
                    Output::Techniques(list) => list,
                    _ => None,
                };
                self.c.reply(super::Reply::Techniques { call, techniques });
            }
            Pending::LyricsFile { call, lrc } => self.lyrics_file_read(call, lrc, output),
        }
        self.finish()
    }

    /// Queues the engine sync to the model now (`mixer.resyncPlayback()`)
    /// and waits for it off the UI thread.
    pub(super) fn resync(&mut self, then: ResyncThen) {
        let pending = self.mixer_call(|m, h| m.resync_playback(h));
        self.c.job(JobKind::Sync, Pending::Resync(then), move || {
            Output::Synced(pending.wait())
        });
    }

    fn resynced(&mut self, then: ResyncThen, result: Result<(), SyncError>) {
        match then {
            ResyncThen::Recording(op) => {
                self.recording_call(|r, h| r.resynced(op, result, h));
            }
            ResyncThen::Versions(op) => {
                // A failed sync threw out of Dart's await; the controller
                // carries on as its continuation would not have: it is told
                // the resync is done either way.
                self.versions_apply(|v, _| v.resynced(op));
            }
            ResyncThen::Load => {
                self.c.notify();
                self.load_finished(result.map_err(|e| e.to_string()));
            }
            ResyncThen::CatchUp => self.end_catch_up(result.map_err(|e| Failure::from_sync(&e))),
            ResyncThen::Reload(then) => {
                self.c.notify();
                self.reload_done(then);
            }
            ResyncThen::Edit(step) => {
                self.edit_resynced(*step, result.map_err(|e| Failure::from_sync(&e)));
            }
        }
    }

    // --- Loading ---------------------------------------------------------------------

    /// Dart's `_load`. A take stopped, or an edit made, just before the last
    /// screen on this practice closed may still be landing (its finalize or
    /// save outlives that screen). A short grace covers an ordinary save;
    /// past it the screen opens on what the store holds now, but catches up
    /// before it takes an edit (see `start_catch_up`) — waiting it all out
    /// held every reopen behind a slow save on a loading spinner.
    pub(super) fn start_load(&mut self) {
        self.c.load_running = true;
        self.c.load_error = None;
        if self.c.finalizer.is_landing(&self.c.practice_id) {
            let finalizer = Arc::clone(&self.c.finalizer);
            let id = self.c.practice_id.clone();
            self.c.job(JobKind::Landing, Pending::LoadGrace, move || {
                Output::Settled(finalizer.settled_within(&id, LANDING_GRACE))
            });
        } else {
            self.load_read();
        }
    }

    /// Load through the base-hash path: the hashes captured here are what
    /// every later save three-way merges against.
    fn load_read(&mut self) {
        let repo = Arc::clone(&self.c.repository);
        let id = self.c.practice_id.clone();
        self.c.job(JobKind::Load, Pending::LoadRead, move || {
            Output::Loaded(repo.load_practice(&id).map_err(|e| e.to_string()))
        });
    }

    fn load_landed(&mut self, loaded: Result<Loaded<SongPractice>, String>) {
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(e) => {
                self.catch_up_adopted();
                self.load_finished(Err(e));
                return;
            }
        };
        let vocal_tracks = loaded.aggregate.vocal_tracks.clone();
        self.adopt_loaded(loaded);
        self.catch_up_adopted();
        // Render the workspace as soon as the project DATA is ready (tracks +
        // cached waveforms), so there is no blank loading screen; the audio
        // engine sync below continues in the background.
        self.c.notify();
        // Compile-time demo/screenshot hooks (see AppConfig).
        self.analysis_call(|a, h| a.apply_demo_hooks(&vocal_tracks, h));
        self.resync(ResyncThen::Load);
    }

    /// The load command's end: a remote change that landed while it read is
    /// owed now (the command notified as it finished, its running flag
    /// already clear).
    fn load_finished(&mut self, outcome: Result<(), String>) {
        self.c.load_running = false;
        self.c.load_error = outcome.err();
        self.c.notify();
        if self.c.missed_remote {
            self.c.missed_remote = false;
            self.reload_if_unchanged(ReloadThen::Nothing);
        }
    }

    /// Holds the busy gate from now until what is landing on this practice
    /// has settled — however long that takes — then re-reads and adopts the
    /// store, once the first read was adopted. Every save merges against
    /// what the screen read, and the merge settles a row both sides changed
    /// wholesale: an edit made on the read from before the landing save lost
    /// either itself or the landed change.
    fn start_catch_up(&mut self) {
        self.c.catching_up = true;
        // A fresh screen's gate is free; if anything held it, the guarded
        // reload stays out of that work's way. Unlisted: it waits for
        // everything landing on this practice, itself included if it were
        // listed.
        let guarded = self.c.gate.run_unwrapped(|| Ok(())).is_some();
        if guarded {
            self.c.gate_unlisted = true;
            self.c.notify();
        }
        self.c.catch_up = Some(CatchUp {
            guarded,
            settled: false,
            adopted: false,
        });
        let finalizer = Arc::clone(&self.c.finalizer);
        let id = self.c.practice_id.clone();
        self.c
            .job(JobKind::Landing, Pending::CatchUpSettled, move || {
                finalizer.settled(&id);
                Output::Settled(true)
            });
    }

    /// The first read was adopted (or failed): the catch-up may go on.
    fn catch_up_adopted(&mut self) {
        let Some(cu) = self.c.catch_up.as_mut() else {
            return;
        };
        cu.adopted = true;
        self.catch_up_ready();
    }

    fn catch_up_ready(&mut self) {
        let Some(cu) = self.c.catch_up.as_ref() else {
            return;
        };
        if !(cu.settled && cu.adopted) {
            return;
        }
        if cu.guarded {
            if self.c.disposed || self.c.practice.is_none() {
                self.end_catch_up(Ok(()));
                return;
            }
            let repo = Arc::clone(&self.c.repository);
            let id = self.c.practice_id.clone();
            self.c.job(JobKind::Load, Pending::CatchUpRead, move || {
                Output::Loaded(repo.load_practice(&id).map_err(|e| e.to_string()))
            });
        } else {
            self.reload_if_unchanged(ReloadThen::CatchUpDone);
        }
    }

    fn end_catch_up(&mut self, outcome: Result<(), Failure>) {
        let guarded = self.c.catch_up.as_ref().is_some_and(|cu| cu.guarded);
        if guarded {
            self.c.finish_gate(outcome);
        }
        self.c.catch_up = None;
        self.c.catching_up = false;
        self.c.notify();
    }

    // --- Remote-applied reload ---------------------------------------------------------

    /// Re-reads the practice and adopts it unless anything is in flight or
    /// the model changed meanwhile — the freshness reload shared by a remote
    /// apply and a save that landed after this screen opened (see the load).
    pub(super) fn reload_if_unchanged(&mut self, then: ReloadThen) {
        if self.c.disposed {
            return self.reload_done(then);
        }
        if self.c.practice.is_none() || self.c.load_running {
            // Owed after the load (see load_finished): dropped, a change
            // landing after the first read stayed invisible until the next
            // save re-read.
            self.c.missed_remote = true;
            return self.reload_done(then);
        }
        if self.busy() || self.is_recording() || self.is_separating() || self.c.saving {
            return self.reload_done(then);
        }
        // A change not saved yet (a fader held still, a save still queued):
        // read over with the store, the control snapped back and its release
        // saved the old value. Skip — that change's own save merges the
        // remote one and re-reads.
        let (Some(current), Some(persisted)) = (&self.c.practice, &self.c.persisted) else {
            return self.reload_done(then);
        };
        if !Arc::ptr_eq(current, persisted) {
            return self.reload_done(then);
        }
        self.c.reload_epoch += 1;
        let epoch = self.c.reload_epoch;
        let before = Arc::clone(current);
        let repo = Arc::clone(&self.c.repository);
        let id = self.c.practice_id.clone();
        self.c.job(
            JobKind::Load,
            Pending::Reload {
                epoch,
                before,
                then,
            },
            move || Output::Loaded(repo.load_practice(&id).map_err(|e| e.to_string())),
        );
    }

    fn reload_landed(
        &mut self,
        epoch: u64,
        before: &Arc<SongPractice>,
        loaded: Result<Loaded<SongPractice>, String>,
        then: ReloadThen,
    ) {
        if self.c.disposed || epoch != self.c.reload_epoch {
            return self.reload_done(then);
        }
        if self.busy() || self.is_recording() || self.is_separating() || self.c.saving {
            return self.reload_done(then);
        }
        // The same for a live edit (fader/pan ticks, clip drags, FX knobs)
        // made during the read: it changes the model without saving or
        // bumping the epoch.
        if !self.c.is_current(before) {
            return self.reload_done(then);
        }
        let Ok(loaded) = loaded else {
            // Deleted remotely (or unreadable): keep showing the current
            // state. A later save aborts safely on the tombstoned parent;
            // navigation-on-remote-delete is deliberately out of scope here.
            return self.reload_done(then);
        };
        self.adopt_loaded(loaded);
        self.resync(ResyncThen::Reload(then));
    }

    fn reload_done(&mut self, then: ReloadThen) {
        match then {
            ReloadThen::Nothing => {}
            ReloadThen::CatchUpDone => {
                self.c.catch_up = None;
                self.c.catching_up = false;
                self.c.notify();
            }
        }
    }

    // --- Adoption --------------------------------------------------------------------

    /// Adopts a freshly-read aggregate as the live model: the base hashes
    /// the next save merges against, plus every normalization the model
    /// needs before anything reads it.
    ///
    /// Shared by the load and the remote-applied reload precisely so the two
    /// can't drift. They HAD drifted: the reload skipped the timeline (a
    /// take that arrived from another device could sit past the end of the
    /// local timeline — invisible and unplayable until the next save re-grew
    /// it) and skipped the analysis refresh (the dock is push-based, so it
    /// kept painting the previous subject's spectrogram, or nothing at all
    /// when the selection moved because the selected take was deleted
    /// remotely).
    pub(super) fn adopt_loaded(&mut self, loaded: Loaded<SongPractice>) {
        let before = self.c.practice.clone();
        let moved = clip_paths_moved(before.as_deref(), &loaded.aggregate);
        let next = match before.as_deref() {
            None => loaded.aggregate,
            Some(held) => loaded.aggregate.sharing(held),
        };
        self.c.set_practice(next);
        self.c.base_hashes = loaded.base_hashes;
        let play_reference = self.mixer.play_reference();
        self.c.ensure_selection(play_reference);
        self.c.ensure_timeline();
        self.c.persisted = self.c.practice.clone();
        self.ensure_loop_range();
        if self.shows_selected_analysis() {
            self.ensure_selected_spectrogram();
        }
        // A re-read can move a clip to another path (a synced clip's bytes
        // landed: its `asset://` ref became the pool path), which re-keys its
        // lane's analysis; folded-out lanes compute only when pushed, and sat
        // blank.
        if moved {
            self.analysis_call(|a, h| a.refresh_expanded_lanes(h));
        }
    }

    /// What a save landing after the screen closed still owes the saves
    /// queued behind it: the fresh base (and, for the model it held, the
    /// re-read). They run on — they hold the model the user left with — and
    /// against the stale base the store's skip rules read a change undone
    /// since (an unmute after a mute, a note added then deleted) as
    /// unchanged, and dropped it. Only the fields: nothing on screen is left
    /// to refresh.
    fn adopt_saved_base(&mut self, sent: &Arc<SongPractice>, saved: Loaded<SongPractice>) {
        if self.c.is_current(sent) {
            self.c.set_practice(saved.aggregate.sharing(sent));
            self.c.persisted = self.c.practice.clone();
            self.c.base_hashes = saved.base_hashes;
        } else {
            self.c.base_hashes = saved.sent_hashes.unwrap_or(saved.base_hashes);
        }
    }

    /// Takes in a successful save of `sent`. Adopts the re-read aggregate and
    /// its fresh base only if no caller mutated the model while the write was
    /// in flight. Otherwise the save queued behind this one carries the newer
    /// model — overwriting it would silently drop that edit — and its base
    /// must be the state that model derives from: what this save SENT, not
    /// the re-read store. The re-read includes rows the three-way rules kept
    /// from concurrent writers that the newer model has never seen, and
    /// merging against it would make the queued save tombstone a remote
    /// insert, revert a remote edit and resurrect a remote delete.
    fn adopt_saved(&mut self, sent: &Arc<SongPractice>, saved: Loaded<SongPractice>) {
        if self.c.is_current(sent) {
            let moved = clip_paths_moved(Some(sent), &saved.aggregate);
            // What the re-read reproduces keeps the objects it was sent as
            // (see SongPractice::sharing).
            self.c.set_practice(saved.aggregate.sharing(sent));
            self.c.base_hashes = saved.base_hashes;
            // The re-read can hold content past the end of the timeline (a
            // take from another device, merged in; see adopt_loaded).
            self.c.ensure_timeline();
            self.c.persisted = self.c.practice.clone();
            // The re-read may move clips to other paths (see adopt_loaded),
            // re-keying their analysis: the dock and inspector look it up by
            // path, and sat blank on a take whose clip moved after its own
            // save.
            if moved {
                if self.shows_selected_analysis() {
                    self.ensure_selected_spectrogram();
                }
                self.analysis_call(|a, h| a.refresh_expanded_lanes(h));
            }
        } else {
            self.c.base_hashes = saved.sent_hashes.unwrap_or(saved.base_hashes);
        }
    }

    /// Takes the disk's model back after a failed write. When that changes
    /// the lyrics, the sync cursor re-seeds on the disk's: on the first line
    /// still untimed there from `unsaved_from` on (a lost tap, or the first
    /// line a lost clear or import had made untimed), else back on that line
    /// itself (a lost re-time of a fully timed song).
    pub(super) fn revert_to(&mut self, reloaded: Loaded<SongPractice>) {
        let before = self.c.lyrics().to_vec();
        self.c.reverts += 1;
        let next = match self.c.practice.as_deref() {
            None => reloaded.aggregate,
            Some(held) => reloaded.aggregate.sharing(held),
        };
        self.c.set_practice(next);
        self.c.base_hashes = reloaded.base_hashes;
        // The stored timeline need not cover the content (a recovered or
        // synced take past it): grown on load, it shrank back on a failed
        // save's revert, the take cut off at the right edge.
        self.c.ensure_timeline();
        self.c.persisted = self.c.practice.clone();
        // Nothing unsaved after it, the lines unchanged or not: kept set, an
        // unchanged Save wrote again, and a failing write toasted a failure
        // over text already on disk.
        let unsaved = self.c.unsaved_from.take();
        if before == self.c.lyrics() {
            return;
        }
        let from = unsaved.unwrap_or(0).min(self.c.lyrics().len());
        let at = self
            .c
            .first_untimed_from(from)
            .unwrap_or_else(|| self.c.words_from(from));
        self.c.place_cursor(at);
    }

    // --- The save chain ---------------------------------------------------------------

    /// Persists the model (Dart's `_save()`).
    pub(super) fn save(&mut self, then: SaveThen) {
        self.enqueue_save(SaveKind::Practice, then);
    }

    /// Serializes every persist of the model (gated or not): two in-flight
    /// `save_practice_with_base` calls would otherwise share one base-hash
    /// snapshot (two rapid interpretation clicks, a lyric stamp racing a
    /// gated save), and whichever landed second would three-way merge
    /// against a base that no longer describes the store. Each save starts
    /// only after the previous one fully completed, then reads the LIVE
    /// model and base. Independent of the busy gate: saves never wait on the
    /// gate, so a gated save chaining after an ungated one cannot deadlock.
    ///
    /// Each queued persist holds the quit until it lands (its wait in the
    /// queue included), and is listed as landing on the practice too, like a
    /// stopped take's finalize: the save outlives the screen (an edit, then
    /// Back on a slow disk), and a screen reopened on the practice meanwhile
    /// read it from before the edit.
    pub(super) fn enqueue_save(&mut self, kind: SaveKind, then: SaveThen) {
        let landing = self.c.finalizer.begin_landing(&self.c.practice_id);
        let hold = self.c.finalizer.begin_hold();
        self.c.save_queue.push_back(QueuedSave {
            kind,
            then,
            landing,
            hold,
        });
        if self.c.running_save.is_none() {
            self.c.save_kick = true;
        }
    }

    pub(super) fn start_next_save(&mut self) {
        let Some(q) = self.c.save_queue.pop_front() else {
            return;
        };
        let lyrics = matches!(q.kind, SaveKind::Lyrics { .. });
        if !lyrics {
            // `_doSave`: the timeline first.
            if self.c.practice.is_some() {
                self.c.ensure_timeline();
            }
        }
        let Some(sent) = self.c.practice.clone() else {
            // `if (_practice == null) return false;`: nothing written. Its
            // guards go, the next save starts, then its caller hears.
            drop(q.hold);
            drop(q.landing);
            self.start_next_save();
            self.save_then(q.then, Ok(false));
            return;
        };
        // A starting save supersedes any in-flight remote-applied reload (its
        // read predates this write) and blocks new ones until the fresh base
        // is in.
        self.c.reload_epoch += 1;
        self.c.saving = true;
        let repo = Arc::clone(&self.c.repository);
        let base = self.c.base_hashes.clone();
        let practice = Arc::clone(&sent);
        self.c.running_save = Some(RunningSave {
            kind: q.kind,
            then: q.then,
            landing: q.landing,
            hold: q.hold,
            sent,
            stage: Stage::Writing,
            ok: false,
        });
        // Three-way save against the base captured at load / the previous
        // save, so a stale aggregate can't revert or tombstone concurrent
        // (synced-in) changes; the returned fresh base replaces ours for the
        // next save.
        self.c.job(JobKind::Save, Pending::Save, move || {
            Output::Saved(
                repo.save_practice_with_base(&practice, &base)
                    .map_err(|e| e.to_string()),
            )
        });
    }

    fn set_stage(&mut self, stage: Stage) {
        if let Some(r) = self.c.running_save.as_mut() {
            r.stage = stage;
        }
    }

    fn save_reread(&mut self, stage: Stage) {
        self.set_stage(stage);
        let repo = Arc::clone(&self.c.repository);
        let id = self
            .c
            .running_save
            .as_ref()
            .map(|r| r.sent.id.clone())
            .unwrap_or_default();
        self.c.job(JobKind::Load, Pending::Save, move || {
            Output::Loaded(repo.load_practice(&id).map_err(|e| e.to_string()))
        });
    }

    fn save_resync(&mut self, stage: Stage) {
        self.set_stage(stage);
        let pending = self.mixer_call(|m, h| m.resync_playback(h));
        self.c.job(JobKind::Sync, Pending::Save, move || {
            Output::Synced(pending.wait())
        });
    }

    /// The running save's job landed: the code after its await.
    fn save_step(&mut self, output: Output) {
        let Some(running) = self.c.running_save.as_mut() else {
            return;
        };
        let sent = Arc::clone(&running.sent);
        let lyrics = matches!(running.kind, SaveKind::Lyrics { .. });
        let stage = std::mem::replace(&mut running.stage, Stage::Writing);
        match stage {
            Stage::Writing => {
                let result = match output {
                    Output::Saved(r) => r,
                    // The repository threw: the save throws out of its
                    // `finally`, nothing adopted, nothing reverted.
                    Output::Panicked(m) => return self.save_finished(Err(Failure::Other(m))),
                    _ => Err("unexpected job output".into()),
                };
                self.save_written(&sent, result, lyrics);
            }
            Stage::CatchingUp {
                adopted,
                sent_hashes,
            } => {
                let again = loaded_of(output);
                // `_catchUpOnStore`: an edit made during this read stops it.
                let differed = match again {
                    Ok(again)
                        if !self.c.disposed
                            && adopted.as_ref().is_some_and(|a| self.c.is_current(a)) =>
                    {
                        let differed = Some(&again.base_hashes) != sent_hashes.as_ref();
                        self.adopt_loaded(again);
                        differed
                    }
                    _ => false,
                };
                let ok = self.c.running_save.as_ref().is_some_and(|r| r.ok);
                if lyrics {
                    if self.c.disposed {
                        // `return true` from inside the try: its finally ran,
                        // the notify after it did not.
                        self.c.saving = false;
                        return self.save_finished(Ok(true));
                    }
                    if differed {
                        return self.save_resync(Stage::LyricsResync);
                    }
                    self.lyrics_end();
                } else {
                    if self.c.disposed {
                        return self.save_finished(Ok(ok));
                    }
                    self.save_tail();
                }
            }
            Stage::Reverting => {
                let reloaded = loaded_of(output);
                // The identity guard protects a NEWER queued model from the
                // revert: a save queued behind this one carries it, and
                // reverting over it would make that save persist pre-edit
                // state (a silent lost update). Let the queued save retry the
                // combined state; if it also fails, its own revert (then
                // current) restores disk state consistently.
                let reverted = match reloaded {
                    Ok(reloaded) if self.c.is_current(&sent) => {
                        self.revert_to(reloaded);
                        true
                    }
                    _ => false,
                };
                if lyrics {
                    // And the engine and selection with it, as the practice
                    // save's revert does: the disk's model can differ in more
                    // than the lyrics (an import whose own save failed ahead
                    // of this one, unreverted as the stamp had moved the
                    // model on). Left, its lane was gone from the screen and
                    // still played.
                    if reverted && !self.c.disposed {
                        self.ensure_selection_analyzed();
                        self.ensure_loop_range();
                        return self.save_resync(Stage::LyricsRevertResync);
                    }
                    self.c.toast("Couldn't save your changes.");
                    self.lyrics_end();
                } else {
                    self.c.toast("Couldn't save your changes.");
                    self.save_tail();
                }
            }
            Stage::Tail => {
                let ok = self.c.running_save.as_ref().is_some_and(|r| r.ok);
                match synced_of(output) {
                    Ok(()) => {
                        self.c.notify();
                        self.save_finished(Ok(ok));
                    }
                    Err(e) => self.save_finished(Err(Failure::from_sync(&e))),
                }
            }
            Stage::LyricsResync => match synced_of(output) {
                Ok(()) => self.lyrics_end(),
                Err(e) => self.save_finished(Err(Failure::from_sync(&e))),
            },
            Stage::LyricsRevertResync => match synced_of(output) {
                Ok(()) => {
                    self.c.toast("Couldn't save your changes.");
                    self.lyrics_end();
                }
                Err(e) => self.save_finished(Err(Failure::from_sync(&e))),
            },
        }
    }

    /// The write landed: `_doSave` / `_writeLyrics` after their repository
    /// await.
    fn save_written(
        &mut self,
        sent: &Arc<SongPractice>,
        result: Result<Loaded<SongPractice>, String>,
        lyrics: bool,
    ) {
        let ok = result.is_ok();
        // If the route was popped mid-save (e.g. a second back while
        // finalizing a recording), the write above still committed — the take
        // is safe — but the playback/selection resync below must not touch
        // what the page owned.
        if self.c.disposed {
            if let Ok(saved) = result {
                self.adopt_saved_base(sent, saved);
            }
            return self.save_finished(Ok(ok && !lyrics));
        }
        let Ok(saved) = result else {
            // Revert the optimistic in-memory model to what's actually on
            // disk, so the UI never keeps showing edits a failed write didn't
            // persist. The base hashes are refreshed with it (they must
            // always describe the model).
            return self.save_reread(Stage::Reverting);
        };
        if let Some(r) = self.c.running_save.as_mut() {
            r.ok = true;
        }
        // Saved with nothing newer behind it: no stamp is pending any more.
        let current = self.c.is_current(sent);
        if current {
            self.c.unsaved_from = None;
        }
        let reread = saved.reread;
        if lyrics
            && current
            && saved
                .sent_hashes
                .as_ref()
                .is_some_and(|sent_hashes| *sent_hashes != saved.base_hashes)
        {
            // The store differs from what this write sent: the merge kept
            // rows a concurrent writer changed (remote reloads are skipped
            // while a save is in flight, so during tap-to-time this is how
            // they arrive). Adopt the re-read the way a load does and resync
            // the engine — but only then, so a plain lyric stamp still never
            // touches playback.
            self.adopt_loaded(saved);
            return self.save_resync(Stage::LyricsResync);
        }
        let sent_hashes = saved.sent_hashes.clone();
        self.adopt_saved(sent, saved);
        if current && !reread {
            // After a save that committed but could not re-read the store
            // (its result stands in what it sent): read the store once more
            // and take it in as a load does. Judged against the sent state
            // alone, rows the merge kept from another writer stayed off
            // screen, and the next edit of one rolled it back. Only for a
            // model the save held as it was sent — judged BEFORE adopting the
            // save: judged after it, an edit made while the save ran was read
            // over with the store, and its own queued save then wrote nothing
            // (a moved-on model's queued save re-reads anyway). An edit made
            // during this read stops it too.
            let adopted = self.c.practice.clone();
            if adopted.is_none() {
                return self.after_catch_up_skipped(lyrics);
            }
            return self.save_reread(Stage::CatchingUp {
                adopted,
                sent_hashes,
            });
        }
        if lyrics {
            self.lyrics_end();
        } else {
            self.save_tail();
        }
    }

    fn after_catch_up_skipped(&mut self, lyrics: bool) {
        if lyrics {
            self.lyrics_end();
        } else {
            self.save_tail();
        }
    }

    /// `_doSave`'s close: the selection, the loop and the engine follow the
    /// model it leaves.
    fn save_tail(&mut self) {
        self.ensure_selection_analyzed();
        self.ensure_loop_range();
        self.save_resync(Stage::Tail);
    }

    /// `_writeLyrics`'s close: its finally, the notify, its answer.
    fn lyrics_end(&mut self) {
        self.c.saving = false;
        self.c.notify();
        let ok = self.c.running_save.as_ref().is_some_and(|r| r.ok);
        self.save_finished(Ok(ok));
    }

    /// The running save is done (Dart's future completing): its `finally`,
    /// the separation's take-back inside the save, the next queued save's
    /// start, then its caller.
    fn save_finished(&mut self, result: Result<bool, Failure>) {
        let Some(running) = self.c.running_save.take() else {
            return;
        };
        self.c.saving = false;
        let result = match (&running.kind, result) {
            // `await _writeLyrics() && _reverts == reverts`: landed only if
            // no revert came first. A queued write sends the model as it is
            // when it runs, so a save failing ahead of it sent these lines
            // too, and its revert threw them away.
            (SaveKind::Lyrics { reverts }, Ok(ok)) => Ok(ok && self.c.reverts == *reverts),
            (SaveKind::Separation(take_back), Ok(false)) => {
                // The write stored nothing (one that committed reports
                // success, its re-read failing or not). Taken back here,
                // inside this save, before any queued behind it runs: one
                // queued (an edit made while this ran) sends the model as it
                // is when it starts, and taken back after, it saved the
                // separation the files below are about to be deleted from
                // under. After a revert, nothing is left to take back.
                if let Some(live) = self.c.practice.clone() {
                    let next = take_back.apply(&live);
                    if let Some(next) = next {
                        self.c.set_practice(next);
                    }
                }
                Ok(false)
            }
            (_, result) => result,
        };
        drop(running.hold);
        drop(running.landing);
        // The next queued save starts before this one's caller resumes, as
        // Dart's chained `then` ran it.
        self.start_next_save();
        self.save_then(running.then, result);
    }

    fn save_then(&mut self, then: SaveThen, result: Result<bool, Failure>) {
        match then {
            SaveThen::Nothing => {}
            SaveThen::Recording(op) => {
                let ok = result.unwrap_or(false);
                self.recording_call(|r, h| r.saved(op, ok, h));
            }
            SaveThen::Versions(op) => {
                let ok = result.unwrap_or(false);
                self.versions_apply(|v, h| v.saved(op, ok, h));
            }
            SaveThen::Lyrics(then) => self.lyrics_saved(then, result),
            SaveThen::EmptyTake(empty) => self.empty_take_saved(*empty, result.unwrap_or(false)),
            SaveThen::Edit(step) => self.edit_saved(*step, result),
            SaveThen::CommitMove => {
                // A drag whose lane unmounted commits after the frame, which
                // may be the page closing: the move is saved, and nothing
                // else is left to refresh.
                if self.c.disposed {
                    return;
                }
                if self.shows_selected_analysis() {
                    self.ensure_selected_spectrogram();
                }
                self.analysis_call(|a, h| a.refresh_expanded_lanes(h));
            }
        }
    }
}
