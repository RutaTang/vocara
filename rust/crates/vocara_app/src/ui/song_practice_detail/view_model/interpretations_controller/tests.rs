//! Port of `test/interpretations_controller_test.dart`.
//!
//! Dart's host harness edited the practice through the controller's closures
//! and awaited each mutator; here [`Host`] implements the host trait and
//! [`drive`] plays the screen, answering each effect (the resync, the save,
//! the guarded clean-up) until the operation is done. A take committed
//! "during the pause await" is the practice the host reports once the
//! engine has paused ([`Host::on_pause`]): the engine's pause returns at
//! once here, and the controller must still read the live practice after
//! it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioClip, AudioTrackKind};
use vocara_services::playback_engine::{Broadcast, BusPlayback, Subscription, TrackPlayback};

use super::*;

/// Transport fake: records the pause/play order so the switch flow (pause →
/// save → resume-only-if-playing) is observable.
#[derive(Default)]
struct FakeEngine {
    transport_running: AtomicBool,
    pauses: AtomicUsize,
    log: Mutex<Vec<&'static str>>,
    position: Broadcast<Duration>,
    playing: Broadcast<bool>,
}

impl FakeEngine {
    fn log(&self) -> Vec<&'static str> {
        self.log.lock().unwrap().clone()
    }

    fn paused(&self) -> bool {
        self.pauses.load(Ordering::SeqCst) > 0
    }
}

impl PlaybackEngine for FakeEngine {
    fn position(&self) -> Subscription<Duration> {
        self.position.subscribe()
    }
    fn playing(&self) -> Subscription<bool> {
        self.playing.subscribe()
    }
    fn current_position(&self) -> Duration {
        Duration::from_secs(5)
    }
    fn is_playing(&self) -> bool {
        self.transport_running.load(Ordering::SeqCst)
    }
    fn set_loop(&self, _: bool, _: i64, _: i64) {}
    fn sync(&self, _: Vec<TrackPlayback>, _: Vec<BusPlayback>) {}
    fn play(&self) {
        self.log.lock().unwrap().push("play");
        self.transport_running.store(true, Ordering::SeqCst);
    }
    fn pause(&self) {
        self.log.lock().unwrap().push("pause");
        self.transport_running.store(false, Ordering::SeqCst);
        self.pauses.fetch_add(1, Ordering::SeqCst);
    }
    fn seek(&self, _: Duration) {}
    fn set_volume(&self, _: &str, _: f64, _: bool) {}
    fn set_track_pan(&self, _: &str, _: f64) {}
    fn set_master_volume(&self, _: f64) {}
    fn supports_effects(&self) -> bool {
        true
    }
    fn set_track_fx(&self, _: &str, _: &TrackFx) {}
    fn set_bus_fx(&self, _: &str, _: &TrackFx) {}
    fn set_bus_volume(&self, _: &str, _: f64, _: bool) {}
    fn set_bus_pan(&self, _: &str, _: f64) {}
    fn bus_gain_reduction_db(&self, _: &str) -> f64 {
        0.0
    }
    fn set_track_send_level(&self, _: &str, _: usize, _: f64) {}
    fn set_bus_send_level(&self, _: &str, _: usize, _: f64) {}
    fn set_reverb(&self, _: &ReverbSettings) {}
    fn track_gain_reduction_db(&self, _: &str) -> f64 {
        0.0
    }
    fn dispose(&self) {}
}

#[derive(Default)]
struct FakeAnalysis {
    open: bool,
    ensured: bool,
    dropped_lanes: Vec<String>,
    evicted: Vec<String>,
}

impl InterpretationsAnalysis for FakeAnalysis {
    fn dock_open(&self) -> bool {
        self.open
    }
    fn dock_shows_take(&self) -> bool {
        true
    }
    fn drop_lane_state(&mut self, track_id: &str) {
        self.dropped_lanes.push(track_id.to_string());
    }
    fn evict_analysis(&mut self, path: &str, _recompute: bool) {
        self.evicted.push(path.to_string());
    }
}

#[derive(Default)]
struct FakeFiles {
    deleted: Mutex<Vec<String>>,
}

impl AudioFiles for FakeFiles {
    fn import_audio(
        &self,
        _: &str,
        _: &str,
    ) -> vocara_services::audio_file_service::Result<String> {
        unreachable!("not called")
    }
    fn new_recording_path(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<i64>,
    ) -> vocara_services::audio_file_service::Result<String> {
        unreachable!("not called")
    }
    fn write_aligned_start(&self, _: &str, _: i64) -> bool {
        unreachable!("not called")
    }
    fn is_app_storage(&self, _: &str) -> bool {
        unreachable!("not called")
    }
    fn delete_audio(&self, path: &str) -> vocara_services::audio_file_service::Result<()> {
        self.deleted.lock().unwrap().push(path.to_string());
        Ok(())
    }
}

/// A live host harness: the practice model and what the controller edits it
/// through, mirroring the view model's wiring.
struct Host {
    practice: Option<SongPractice>,
    /// The practice the model holds once the engine paused (a take landed
    /// meanwhile), until the controller replaces it.
    on_pause: Option<SongPractice>,
    engine: Arc<FakeEngine>,
    files: Arc<FakeFiles>,
    analysis: FakeAnalysis,
    gate: BusyGate,
    toasts: Vec<String>,
    save_result: bool,
    saves: usize,
    ensure_selection_calls: usize,
    /// The active interpretation at each engine resync, before its save.
    resynced_on: Vec<Option<String>>,
    recording: bool,
    separating: bool,
    /// The user's transport presses (Play/Pause, seek, Stop).
    transport_moves: u64,
    /// Runs as the save lands, so a test can e.g. dispose the controller
    /// mid-save (route popped while a switch persists).
    on_save: Option<fn(&mut Host, &mut InterpretationsController)>,
    resynced_before_save: Option<Option<String>>,
}

impl Host {
    fn new(practice: SongPractice) -> (Host, InterpretationsController) {
        let engine = Arc::new(FakeEngine::default());
        let files = Arc::new(FakeFiles::default());
        let controller = InterpretationsController::new(
            Arc::clone(&engine) as Arc<dyn PlaybackEngine>,
            Arc::clone(&files) as Arc<dyn AudioFiles>,
        );
        let host = Host {
            practice: Some(practice),
            on_pause: None,
            engine,
            files,
            analysis: FakeAnalysis::default(),
            gate: BusyGate::new(),
            toasts: Vec::new(),
            save_result: true,
            saves: 0,
            ensure_selection_calls: 0,
            resynced_on: Vec::new(),
            recording: false,
            separating: false,
            transport_moves: 0,
            on_save: None,
            resynced_before_save: None,
        };
        (host, controller)
    }

    fn live(&self) -> &SongPractice {
        self.practice().expect("a practice")
    }

    fn deleted(&self) -> Vec<String> {
        self.files.deleted.lock().unwrap().clone()
    }
}

impl InterpretationsHost for Host {
    fn practice(&self) -> Option<&SongPractice> {
        if self.engine.paused() {
            if let Some(p) = &self.on_pause {
                return Some(p);
            }
        }
        self.practice.as_ref()
    }

    fn set_practice(&mut self, practice: SongPractice) {
        self.on_pause = None;
        self.practice = Some(practice);
    }

    fn ensure_selection(&mut self) {
        self.ensure_selection_calls += 1;
    }

    fn toast(&mut self, message: String) {
        self.toasts.push(message);
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn is_separating(&self) -> bool {
        self.separating
    }

    fn transport_moves(&self) -> u64 {
        self.transport_moves
    }

    fn busy_gate(&mut self) -> &mut BusyGate {
        &mut self.gate
    }

    fn analysis(&mut self) -> &mut dyn InterpretationsAnalysis {
        &mut self.analysis
    }
}

/// Plays the screen until the operation is done (Dart's `await`).
fn drive(
    controller: &mut InterpretationsController,
    host: &mut Host,
    effects: Vec<InterpretationsEffect>,
) {
    let mut queue: std::collections::VecDeque<_> = effects.into();
    while let Some(effect) = queue.pop_front() {
        let next = match effect {
            InterpretationsEffect::Resync(op) => {
                let active = host.practice().and_then(|p| p.active_version_id.clone());
                host.resynced_on.push(active);
                controller.resynced(op)
            }
            InterpretationsEffect::Save(op) => {
                host.saves += 1;
                host.resynced_before_save = Some(host.resynced_on.last().cloned().flatten());
                if let Some(hook) = host.on_save {
                    hook(host, controller);
                }
                let ok = host.save_result;
                controller.saved(op, ok, host)
            }
            InterpretationsEffect::EnsureSelectedAnalysis => {
                host.analysis.ensured = true;
                Vec::new()
            }
            InterpretationsEffect::CleanUp(op, run) => {
                let outcome = run.run();
                if let Some(error) = controller.cleaned_up(op, outcome, host) {
                    host.toasts.push(error.to_string());
                }
                Vec::new()
            }
        };
        queue.extend(next);
    }
}

fn version(id: &str, name: &str) -> Arc<TakeVersion> {
    Arc::new(TakeVersion::new(id, name, DateTime::local_ymd(2026, 1, 1)))
}

fn take(id: &str, version_id: &str, files: &[&str]) -> Arc<AudioTrack> {
    Arc::new(AudioTrack {
        version_id: Some(version_id.to_string()),
        clips: files
            .iter()
            .enumerate()
            .map(|(i, f)| AudioClip::new(format!("{id}-c{i}"), *f, 1000))
            .collect(),
        ..AudioTrack::new(
            id,
            AudioTrackKind::Vocal,
            format!("Take {id}"),
            DateTime::local_ymd(2026, 1, 1),
        )
    })
}

fn practice(
    versions: Vec<Arc<TakeVersion>>,
    active_version_id: Option<&str>,
    vocal_tracks: Vec<Arc<AudioTrack>>,
) -> SongPractice {
    SongPractice {
        versions,
        active_version_id: active_version_id.map(str::to_string),
        vocal_tracks,
        ..SongPractice::new(
            "p1",
            "Song",
            DateTime::local_ymd(2026, 1, 1),
            DateTime::local_ymd(2026, 1, 1),
        )
    }
}

fn two(active: &str) -> SongPractice {
    practice(
        vec![version("v1", "One"), version("v2", "Two")],
        Some(active),
        Vec::new(),
    )
}

fn one() -> SongPractice {
    practice(vec![version("v1", "One")], Some("v1"), Vec::new())
}

fn ids<T>(items: &[Arc<T>], id: impl Fn(&T) -> &str) -> Vec<String> {
    items.iter().map(|t| id(t).to_string()).collect()
}

// --- vocal_takes ----------------------------------------------------------------

#[test]
fn vocal_takes_returns_only_the_active_interpretations_takes() {
    let (host, _) = Host::new(practice(
        vec![version("v1", "One"), version("v2", "Two")],
        Some("v2"),
        vec![take("t1", "v1", &[]), take("t2", "v2", &[])],
    ));
    let p = host.practice();
    assert_eq!(
        ids(&InterpretationsController::vocal_takes(p), |t| &t.id),
        ["t2"]
    );
    assert_eq!(
        InterpretationsController::active(p).map(|v| v.id.clone()),
        Some("v2".to_string())
    );
    assert_eq!(
        ids(&InterpretationsController::all(p), |v| &v.id),
        ["v1", "v2"]
    );
}

// --- set_active -----------------------------------------------------------------

#[test]
fn set_active_pauses_switches_saves_and_resumes_only_if_it_was_playing() {
    let (mut host, mut c) = Host::new(two("v1"));
    host.engine.transport_running.store(true, Ordering::SeqCst);
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.live().active_version_id.as_deref(), Some("v2"));
    assert_eq!(host.engine.log(), ["pause", "play"]);
    assert_eq!(host.saves, 1);
    assert_eq!(host.ensure_selection_calls, 1);
    assert!(host.toasts.is_empty());
}

#[test]
fn set_active_loads_the_new_interpretation_into_the_engine_before_its_save() {
    // Shown at once, the new lanes sat over an engine still holding the old
    // takes until the save landed: Play meanwhile played those.
    let (mut host, mut c) = Host::new(two("v1"));
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.resynced_before_save, Some(Some("v2".to_string())));
}

#[test]
fn set_active_shows_the_switch_before_the_save_lands() {
    let (mut host, mut c) = Host::new(two("v1"));
    let before = c.revision();
    let e = c.set_active("v2", &mut host);
    assert_eq!(host.live().active_version_id.as_deref(), Some("v2"));
    assert_eq!(c.revision(), before + 1);
    assert!(matches!(e[..], [InterpretationsEffect::Resync(_)]));
    drive(&mut c, &mut host, e);
}

#[test]
fn set_active_does_not_resume_when_the_transport_was_paused() {
    let (mut host, mut c) = Host::new(two("v1"));
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.engine.log(), ["pause"]);
}

#[test]
fn set_active_refreshes_the_open_docks_analysis_after_the_switch() {
    let (mut host, mut c) = Host::new(two("v1"));
    host.analysis.open = true;
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);
    assert!(host.analysis.ensured);
}

#[test]
fn set_active_is_guarded_while_recording() {
    let (mut host, mut c) = Host::new(two("v1"));
    host.recording = true;
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.live().active_version_id.as_deref(), Some("v1"));
    assert_eq!(host.toasts, ["Stop recording to switch interpretation."]);
    assert_eq!(host.saves, 0);
    assert!(host.engine.log().is_empty());
}

#[test]
fn a_refusal_for_a_take_says_what_to_wait_for_when_there_is_nothing_to_stop() {
    struct Waiting(Host);
    impl InterpretationsHost for Waiting {
        fn practice(&self) -> Option<&SongPractice> {
            self.0.practice()
        }
        fn set_practice(&mut self, p: SongPractice) {
            self.0.set_practice(p);
        }
        fn ensure_selection(&mut self) {}
        fn toast(&mut self, message: String) {
            self.0.toasts.push(message);
        }
        fn is_recording(&self) -> bool {
            true
        }
        fn is_separating(&self) -> bool {
            false
        }
        fn take_wait(&self) -> Option<String> {
            Some("Wait for the take to finish saving.".to_string())
        }
        fn busy_gate(&mut self) -> &mut BusyGate {
            &mut self.0.gate
        }
        fn analysis(&mut self) -> &mut dyn InterpretationsAnalysis {
            &mut self.0.analysis
        }
    }
    let (host, mut c) = Host::new(two("v1"));
    let mut host = Waiting(host);
    let e = c.set_active("v2", &mut host);
    assert!(e.is_empty());
    assert_eq!(host.0.toasts, ["Wait for the take to finish saving."]);
}

#[test]
fn set_active_ignores_an_unknown_id_and_the_already_active_id() {
    let (mut host, mut c) = Host::new(one());
    let e = c.set_active("nope", &mut host);
    drive(&mut c, &mut host, e);
    let e = c.set_active("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.saves, 0);
    assert!(host.engine.log().is_empty());
}

#[test]
fn picking_the_interpretation_shown_when_the_stored_id_resolves_to_none_is_no_switch() {
    // Sync can leave the stored active id null or pointing at a deleted
    // interpretation; the first one is then what is shown.
    let (mut host, mut c) = Host::new(two("gone"));
    let e = c.set_active("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.saves, 0);
    assert!(host.engine.log().is_empty());
}

#[test]
fn a_take_committed_during_the_pause_survives_the_switch() {
    // Rebuilds from the LIVE practice, not the pre-pause snapshot: a
    // recording finalize lands a new take while pause() runs.
    let (mut host, mut c) = Host::new(two("v1"));
    let mut landed = host.live().clone();
    landed.vocal_tracks.push(take("t-new", "v1", &[]));
    host.on_pause = Some(landed);
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.live().active_version_id.as_deref(), Some("v2"));
    assert!(
        host.live().vocal_tracks.iter().any(|t| t.id == "t-new"),
        "the concurrently-committed take must not be clobbered"
    );
}

#[test]
fn an_early_return_after_the_pause_resumes_playback_instead_of_stranding_the_transport() {
    // The target becomes active DURING the pause (a racing switch or a
    // remote apply): the post-pause re-read bails out — but playback was
    // already paused and must be handed back.
    let (mut host, mut c) = Host::new(two("v1"));
    host.engine.transport_running.store(true, Ordering::SeqCst);
    host.on_pause = Some(SongPractice {
        active_version_id: Some("v2".to_string()),
        ..host.live().clone()
    });
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.engine.log(), ["pause", "play"]);
    assert_eq!(host.saves, 0, "nothing changed; no save");
}

#[test]
fn set_active_does_not_resume_over_a_stop_or_a_take_started_while_the_save_queued() {
    // The switch played on from where it paused whatever the user had done
    // meanwhile: Stop pressed, or REC armed.
    for meanwhile in ["stop", "rec"] {
        let (mut host, mut c) = Host::new(two("v1"));
        host.engine.transport_running.store(true, Ordering::SeqCst);
        host.on_save = Some(if meanwhile == "stop" {
            |host: &mut Host, _: &mut InterpretationsController| host.transport_moves += 1
        } else {
            |host: &mut Host, _: &mut InterpretationsController| host.recording = true
        });
        let e = c.set_active("v2", &mut host);
        drive(&mut c, &mut host, e);
        assert_eq!(host.engine.log(), ["pause"], "{meanwhile}");
    }
}

#[test]
fn set_active_resumes_though_the_playhead_moved_one_buffer_after_the_pause() {
    // A render already under way at the pause lands after it: judged by the
    // playhead, the switch stayed paused though the user did nothing. (The
    // controller reads only the transport moves, never the playhead.)
    let (mut host, mut c) = Host::new(two("v1"));
    host.engine.transport_running.store(true, Ordering::SeqCst);
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.engine.log(), ["pause", "play"]);
}

#[test]
fn set_active_does_not_resume_playback_when_disposed_during_the_save() {
    let (mut host, mut c) = Host::new(two("v1"));
    host.engine.transport_running.store(true, Ordering::SeqCst);
    // Route popped mid-save.
    host.on_save = Some(|_: &mut Host, c: &mut InterpretationsController| c.dispose());
    let e = c.set_active("v2", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.engine.log(), ["pause"]); // no play() on the disposed wiring
}

// --- create ---------------------------------------------------------------------

#[test]
fn create_appends_a_default_named_interpretation_and_switches_to_it() {
    let (mut host, mut c) = Host::new(one());
    let e = c.create("   ", &mut host);
    drive(&mut c, &mut host, e);

    let versions = &host.live().versions;
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[1].name, "Interpretation 2");
    assert_eq!(
        host.live().active_version_id.as_deref(),
        Some(versions[1].id.as_str())
    );
    assert_eq!(host.saves, 1);
    assert_eq!(host.engine.log(), ["pause"]);
}

#[test]
fn create_uses_the_trimmed_custom_name() {
    let (mut host, mut c) = Host::new(one());
    let e = c.create("  Soft ballad  ", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions[1].name, "Soft ballad");
}

#[test]
fn create_is_guarded_while_recording() {
    let (mut host, mut c) = Host::new(one());
    host.recording = true;
    let e = c.create("X", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions.len(), 1);
    assert_eq!(host.toasts, ["Stop recording to add an interpretation."]);
}

#[test]
fn create_says_before_the_name_is_asked_for_whether_it_would_be_refused() {
    let (mut host, c) = Host::new(one());
    assert!(!c.refuses_create(&mut host));
    assert!(host.toasts.is_empty());
    host.recording = true;
    assert!(c.refuses_create(&mut host));
    assert_eq!(host.toasts, ["Stop recording to add an interpretation."]);
}

#[test]
fn a_take_committed_during_the_pause_survives_the_create() {
    let (mut host, mut c) = Host::new(one());
    let mut landed = host.live().clone();
    landed.vocal_tracks.push(take("t-new", "v1", &[]));
    host.on_pause = Some(landed);
    let e = c.create("Soft ballad", &mut host);
    drive(&mut c, &mut host, e);

    assert_eq!(host.live().versions.len(), 2);
    assert!(host.live().vocal_tracks.iter().any(|t| t.id == "t-new"));
}

#[test]
fn the_default_name_numbers_past_any_name_already_taken() {
    // Numbered by the count alone, a second "Interpretation 2" followed the
    // first of two being deleted.
    let p = practice(
        vec![
            version("v2", "Interpretation 2"),
            version("v3", "Interpretation 3"),
        ],
        Some("v2"),
        Vec::new(),
    );
    assert_eq!(
        InterpretationsController::next_default_name(Some(&p)),
        "Interpretation 4"
    );
    let p = practice(
        vec![version("v2", "Interpretation 2")],
        Some("v2"),
        Vec::new(),
    );
    assert_eq!(
        InterpretationsController::next_default_name(Some(&p)),
        "Interpretation 3"
    );
}

// --- rename ---------------------------------------------------------------------

#[test]
fn rename_renames_exactly_the_target_trimmed_and_saves() {
    let (mut host, mut c) = Host::new(two("v1"));
    let e = c.rename("v2", "  Belted  ", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(
        host.live()
            .versions
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["One", "Belted"]
    );
    assert_eq!(host.saves, 1);
}

#[test]
fn rename_with_a_blank_name_is_a_no_op() {
    let (mut host, mut c) = Host::new(one());
    let e = c.rename("v1", "   ", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions[0].name, "One");
    assert_eq!(host.saves, 0);
}

// --- delete ---------------------------------------------------------------------

fn two_versions() -> (Host, InterpretationsController) {
    Host::new(practice(
        vec![version("v1", "One"), version("v2", "Two")],
        Some("v1"),
        vec![
            take("t1", "v1", &["a1.wav", "a2.wav"]),
            take("t2", "v2", &["b.wav"]),
        ],
    ))
}

#[test]
fn delete_removes_the_version_and_its_takes_falls_back_the_active_id_and_cleans_up_every_clip_file()
{
    let (mut host, mut c) = two_versions();
    let e = c.delete("v1", &mut host);
    assert!(host.gate.busy(), "held from the start of the delete");
    drive(&mut c, &mut host, e);

    assert_eq!(ids(&host.live().versions, |v| &v.id), ["v2"]);
    assert_eq!(host.live().active_version_id.as_deref(), Some("v2"));
    assert_eq!(ids(&host.live().vocal_tracks, |t| &t.id), ["t2"]);
    // Every clip of the doomed take — a multi-clip take must not orphan its
    // trailing clip's audio.
    assert_eq!(host.deleted(), ["a1.wav", "a2.wav"]);
    assert_eq!(host.analysis.evicted, ["a1.wav", "a2.wav"]);
    assert_eq!(host.analysis.dropped_lanes, ["t1"]);
    assert_eq!(host.saves, 1);
    assert!(host.toasts.is_empty());
    assert!(!host.gate.busy(), "handed back");
}

#[test]
fn deleting_a_non_active_version_keeps_the_active_id() {
    let (mut host, mut c) = two_versions();
    let e = c.delete("v2", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().active_version_id.as_deref(), Some("v1"));
    assert_eq!(host.deleted(), ["b.wav"]);
}

#[test]
fn a_failed_save_keeps_the_audio_files() {
    let (mut host, mut c) = two_versions();
    host.save_result = false;
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert!(host.deleted().is_empty());
    assert!(host.analysis.dropped_lanes.is_empty());
    assert!(!host.gate.busy());
}

#[test]
fn delete_keeps_the_analysis_of_audio_another_take_still_plays() {
    // Two lanes over identical bytes share one pooled path.
    let (mut host, mut c) = Host::new(practice(
        vec![version("v1", "One"), version("v2", "Two")],
        Some("v1"),
        vec![
            take("t1", "v1", &["shared.wav", "a2.wav"]),
            take("t2", "v2", &["shared.wav"]),
        ],
    ));
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.analysis.evicted, ["a2.wav"]);
    assert_eq!(host.deleted(), ["shared.wav", "a2.wav"]);
}

#[test]
fn delete_refuses_to_remove_the_last_interpretation() {
    let (mut host, mut c) = Host::new(practice(
        vec![version("v1", "One")],
        Some("v1"),
        vec![take("t1", "v1", &["a.wav"])],
    ));
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions.len(), 1);
    assert_eq!(host.toasts, ["Keep at least one interpretation."]);
    assert_eq!(host.saves, 0);
}

#[test]
fn delete_is_blocked_while_separating_and_says_so() {
    // Silent, the confirm dialog's "Delete" did nothing at all.
    let (mut host, mut c) = two_versions();
    host.separating = true;
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions.len(), 2);
    assert_eq!(host.saves, 0);
    assert_eq!(host.toasts, ["Wait for the separation to finish."]);
}

#[test]
fn delete_is_blocked_while_another_edit_holds_the_gate_and_says_so() {
    let (mut host, mut c) = two_versions();
    let _held = host.gate.run(|| Ok(())).expect("taken");
    let e = c.delete("v1", &mut host);
    assert!(e.is_empty());
    assert_eq!(host.toasts, ["Wait for the current edit to finish."]);
}

#[test]
fn delete_is_guarded_while_recording() {
    let (mut host, mut c) = two_versions();
    host.recording = true;
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.live().versions.len(), 2);
    assert_eq!(host.toasts, ["Stop recording to delete an interpretation."]);
}

#[test]
fn a_delete_that_finds_one_interpretation_left_after_the_pause_hands_the_gate_back() {
    let (mut host, mut c) = two_versions();
    host.on_pause = Some(SongPractice {
        versions: vec![version("v2", "Two")],
        ..host.live().clone()
    });
    let e = c.delete("v1", &mut host);
    drive(&mut c, &mut host, e);
    assert_eq!(host.saves, 0);
    assert!(host.deleted().is_empty());
    assert!(!host.gate.busy());
}
