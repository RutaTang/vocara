//! Ports of `test/mixer_controller_test.dart`, `test/solo_resolution_test.dart`
//! and `test/audibility_test.dart`.
//!
//! The Dart host's `save` closure counted the save and resynced the engine
//! (as the view model's save tail does); here a mixer edit hands back a
//! [`SaveRequest`] and [`Host::edit`] runs that save. Where a Dart test parked
//! a save on a completer to look at the screen before it landed, the port
//! looks before running it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use vocara_model::models::audio_fx::{FxKind, ReverbSettings, TrackFx, TrackSend, SEND_OFF_DB};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::playback_engine::{BusPlayback, PlaybackEngine, Subscription, TrackPlayback};

use super::*;

/// The Dart VM's `Random(seed)` (sdk/lib/_internal/vm/lib/math_patch.dart),
/// so each random walk is the Flutter test's own: a multiply-with-carry
/// generator over a Thomas Wang-mixed seed, cranked four times. (The same
/// port as the audio DSP tests'.)
struct DartRandom {
    state: u64,
}

impl DartRandom {
    fn new(seed: i64) -> Self {
        let mut n = seed as u64;
        n = (!n).wrapping_add(n << 21);
        n ^= n >> 24;
        n = n.wrapping_mul(265);
        n ^= n >> 14;
        n = n.wrapping_mul(21);
        n ^= n >> 28;
        n = n.wrapping_add(n << 31);
        if n == 0 {
            n = 0x5a17;
        }
        let mut r = DartRandom { state: n };
        for _ in 0..4 {
            r.next_state();
        }
        r
    }

    fn next_state(&mut self) {
        const A: u64 = 0xffff_da61;
        let lo = self.state & 0xFFFF_FFFF;
        let hi = self.state >> 32;
        self.state = A.wrapping_mul(lo).wrapping_add(hi);
    }

    fn next_int(&mut self, max: usize) -> usize {
        let max = u32::try_from(max).unwrap();
        assert!(max > 0);
        if max.is_power_of_two() {
            self.next_state();
            return ((self.state & 0xFFFF_FFFF) as u32 & (max - 1)) as usize;
        }
        loop {
            self.next_state();
            let rnd32 = self.state & 0xFFFF_FFFF;
            let result = rnd32 % u64::from(max);
            if rnd32 - result + u64::from(max) <= 1 << 32 {
                return result as usize;
            }
        }
    }

    fn next_double(&mut self) -> f64 {
        let hi = self.next_int(1 << 26) as f64;
        let lo = self.next_int(1 << 27) as f64;
        (hi * (1u64 << 27) as f64 + lo) / (1u64 << 53) as f64
    }

    fn next_bool(&mut self) -> bool {
        self.next_int(2) == 0
    }

    /// `rnd.nextInt(n)` as an `i64`.
    fn next_i64(&mut self, max: usize) -> i64 {
        self.next_int(max) as i64
    }
}

/// What the fake engine was handed.
#[derive(Default)]
struct Pushed {
    volumes: HashMap<String, (f64, bool)>,
    bus_volumes: HashMap<String, (f64, bool)>,
    master_gain: Option<f64>,
    reverb: Option<ReverbSettings>,
    last_tracks: Option<Vec<TrackPlayback>>,
    last_buses: Option<Vec<BusPlayback>>,
}

/// Engine fake that records what the mixer pushes: per-source levels/mutes,
/// per-bus levels/mutes, the master gain, and the last full graph sync. What
/// the mixer has no business calling panics, as Dart's `Fake` threw.
#[derive(Default)]
struct FakeEngine {
    pushed: Mutex<Pushed>,
}

impl FakeEngine {
    fn pushed(&self) -> MutexGuard<'_, Pushed> {
        self.pushed.lock().unwrap()
    }

    /// Whether the engine last heard source `id` muted.
    fn muted(&self, id: &str) -> Option<bool> {
        self.pushed().volumes.get(id).map(|v| v.1)
    }

    fn bus_muted(&self, id: &str) -> Option<bool> {
        self.pushed().bus_volumes.get(id).map(|v| v.1)
    }
}

impl PlaybackEngine for FakeEngine {
    fn position(&self) -> Subscription<Duration> {
        panic!("unexpected call: position")
    }

    fn playing(&self) -> Subscription<bool> {
        panic!("unexpected call: playing")
    }

    fn current_position(&self) -> Duration {
        panic!("unexpected call: current_position")
    }

    fn is_playing(&self) -> bool {
        panic!("unexpected call: is_playing")
    }

    fn set_loop(&self, _enabled: bool, _start_ms: i64, _end_ms: i64) {
        panic!("unexpected call: set_loop")
    }

    fn sync(&self, tracks: Vec<TrackPlayback>, buses: Vec<BusPlayback>) {
        let mut p = self.pushed();
        p.last_tracks = Some(tracks);
        p.last_buses = Some(buses);
    }

    fn play(&self) {
        panic!("unexpected call: play")
    }

    fn pause(&self) {
        panic!("unexpected call: pause")
    }

    fn seek(&self, _position: Duration) {
        panic!("unexpected call: seek")
    }

    fn set_volume(&self, id: &str, volume: f64, muted: bool) {
        self.pushed()
            .volumes
            .insert(id.to_string(), (volume, muted));
    }

    fn set_track_pan(&self, _id: &str, _pan: f64) {}

    fn set_master_volume(&self, gain: f64) {
        self.pushed().master_gain = Some(gain);
    }

    fn supports_effects(&self) -> bool {
        true
    }

    fn set_track_fx(&self, _track_id: &str, _fx: &TrackFx) {}

    fn set_bus_fx(&self, _bus_id: &str, _fx: &TrackFx) {}

    fn set_bus_volume(&self, bus_id: &str, volume: f64, muted: bool) {
        self.pushed()
            .bus_volumes
            .insert(bus_id.to_string(), (volume, muted));
    }

    fn set_bus_pan(&self, _bus_id: &str, _pan: f64) {}

    fn bus_gain_reduction_db(&self, _bus_id: &str) -> f64 {
        panic!("unexpected call: bus_gain_reduction_db")
    }

    fn set_track_send_level(&self, _track_id: &str, _slot: usize, _level_db: f64) {}

    fn set_bus_send_level(&self, _bus_id: &str, _slot: usize, _level_db: f64) {}

    fn set_reverb(&self, reverb: &ReverbSettings) {
        self.pushed().reverb = Some(*reverb);
    }

    fn track_gain_reduction_db(&self, _track_id: &str) -> f64 {
        panic!("unexpected call: track_gain_reduction_db")
    }

    fn dispose(&self) {
        panic!("unexpected call: dispose")
    }
}

/// The view model's side of the wiring: the practice model plus the
/// selection hooks and the recording state.
#[derive(Default)]
struct State {
    practice: Option<SongPractice>,
    toasts: Vec<String>,
    selected_buses: Vec<String>,
    cleared_buses: Vec<String>,
    recording: bool,
    recording_target: Option<String>,
    rec_peaks: Vec<f64>,
    rec_start_ms: i64,
    rec_duration_ms: i64,
    position: Duration,
}

impl MixerHost for State {
    fn practice(&self) -> Option<&SongPractice> {
        self.practice.as_ref()
    }

    fn set_practice(&mut self, practice: SongPractice) {
        self.practice = Some(practice);
    }

    fn toast(&mut self, message: &str) {
        self.toasts.push(message.to_string());
    }

    fn select_bus_after_mutation(&mut self, bus_id: &str) {
        self.selected_buses.push(bus_id.to_string());
    }

    fn clear_bus_selection_if(&mut self, bus_id: &str) {
        self.cleared_buses.push(bus_id.to_string());
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn recording_target_id(&self) -> Option<&str> {
        self.recording_target.as_deref()
    }

    fn recording_peaks(&self) -> &[f64] {
        &self.rec_peaks
    }

    fn recording_start_ms(&self) -> i64 {
        self.rec_start_ms
    }

    fn recording_duration_ms(&self) -> i64 {
        self.rec_duration_ms
    }

    fn position(&self) -> Duration {
        self.position
    }
}

/// A live host harness mirroring the view model's wiring.
struct Host {
    s: State,
    engine: Arc<FakeEngine>,
    mixer: MixerController,
    saves: usize,
}

impl Host {
    fn new(practice: Option<SongPractice>) -> Host {
        let engine = Arc::new(FakeEngine::default());
        let mixer = MixerController::new(EngineHandle::Other(
            Arc::clone(&engine) as Arc<dyn PlaybackEngine>
        ));
        Host {
            s: State {
                practice,
                ..State::default()
            },
            engine,
            mixer,
            saves: 0,
        }
    }

    /// A fresh controller over the same state: the oracle for the envelope
    /// cache.
    fn fresh(&self) -> Host {
        let mut h = Host::new(self.s.practice.clone());
        h.s.recording = self.s.recording;
        h.s.recording_target = self.s.recording_target.clone();
        h.s.rec_peaks = self.s.rec_peaks.clone();
        h.s.rec_start_ms = self.s.rec_start_ms;
        h.s.rec_duration_ms = self.s.rec_duration_ms;
        h.mixer
            .set_play_reference_silently(self.mixer.play_reference());
        h
    }

    /// The view model's save: counted, and its tail resyncs the engine.
    fn save(&mut self) {
        self.saves += 1;
        self.mixer.resync_playback(&self.s).wait().unwrap();
    }

    /// Runs a mixer edit and then the save it asks for, as Dart awaited it.
    fn edit(&mut self, f: impl FnOnce(&mut MixerController, &mut State) -> Option<SaveRequest>) {
        if f(&mut self.mixer, &mut self.s).is_some() {
            self.save();
        }
    }

    fn create_bus(&mut self, name: &str) -> String {
        let (id, _save) = self.mixer.create_bus(&mut self.s, name).unwrap();
        self.save();
        id
    }

    fn practice(&self) -> &SongPractice {
        self.s.practice.as_ref().unwrap()
    }

    fn track(&self, id: &str) -> Arc<AudioTrack> {
        self.s.track_by_id(id).unwrap()
    }

    fn bus(&self, id: &str) -> Arc<FxBus> {
        Arc::clone(self.practice().bus_by_id(Some(id)).unwrap())
    }

    /// The send targets of a lane, in slot order.
    fn sends(&self, id: &str) -> Vec<String> {
        self.s
            .track_by_id(id)
            .map(|t| t.sends.iter().map(|s| s.bus_id.clone()).collect())
            .unwrap_or_default()
    }

    fn bus_names(&self) -> Vec<String> {
        self.mixer
            .fx_buses(&self.s)
            .iter()
            .map(|b| b.name.clone())
            .collect()
    }

    fn master(&self) -> Arc<[f64]> {
        self.mixer.master_waveform_peaks(&self.s)
    }

    fn bus_lane(&self, id: &str) -> Arc<[f64]> {
        self.mixer.bus_waveform_peaks(&self.s, id)
    }

    /// The engine sources' mutes in the last graph sync.
    fn synced_mutes(&self) -> HashMap<String, bool> {
        self.engine
            .pushed()
            .last_tracks
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| (t.id.clone(), t.muted))
            .collect()
    }
}

fn at() -> DateTime {
    DateTime::local_ymd(2026, 1, 1)
}

fn backing(id: &str, kind: AudioTrackKind) -> AudioTrack {
    AudioTrack::new(id, kind, id, at()).with_audio(format!("{id}.wav"), 0, 10_000, Vec::new())
}

fn vocal(id: &str) -> AudioTrack {
    vocal_with(id, Vec::new())
}

fn vocal_with(id: &str, peaks: Vec<f64>) -> AudioTrack {
    AudioTrack::new(id, AudioTrackKind::Vocal, id, at()).with_audio(
        format!("{id}.wav"),
        0,
        10_000,
        peaks,
    )
}

fn routed(t: AudioTrack, bus_id: Option<&str>, sends: Vec<TrackSend>) -> AudioTrack {
    AudioTrack {
        bus_id: bus_id.map(str::to_string),
        sends,
        ..t
    }
}

fn send(bus_id: &str) -> TrackSend {
    TrackSend::new(bus_id, 0.0)
}

fn bus(id: &str) -> FxBus {
    FxBus::new(id, id, TrackFx::flat())
}

fn bus_routed(id: &str, output_bus_id: Option<&str>, sends: Vec<TrackSend>) -> FxBus {
    FxBus {
        output_bus_id: output_bus_id.map(str::to_string),
        sends,
        ..bus(id)
    }
}

fn practice_of(vocals: Vec<AudioTrack>, buses: Vec<FxBus>, master_volume: f64) -> SongPractice {
    SongPractice {
        accompaniment: Some(Arc::new(backing("acc", AudioTrackKind::Accompaniment))),
        reference: Some(Arc::new(backing("ref", AudioTrackKind::Reference))),
        vocal_tracks: vocals.into_iter().map(Arc::new).collect(),
        fx_buses: buses.into_iter().map(Arc::new).collect(),
        master_volume,
        timeline_duration_ms: 10_000,
        ..SongPractice::new("p1", "Song", at(), at())
    }
}

fn host(vocals: Vec<AudioTrack>, buses: Vec<FxBus>) -> Host {
    Host::new(Some(practice_of(vocals, buses, 1.0)))
}

fn assert_close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} is not within {tolerance} of {expected}"
    );
}

// --- a structural edit shows before its save lands ------------------------------
//
// The view model notifies only once a save lands, after the engine graph
// rebuild; until then the screen showed the old routing, and a second action
// used it.

#[test]
fn a_removed_send_leaves_its_row_at_once() {
    let mut h = host(
        vec![routed(
            vocal("v"),
            None,
            vec![send("a"), send("b"), send("c")],
        )],
        vec![bus("a"), bus("b"), bus("c")],
    );
    let notified = h.mixer.revision();

    let save = h.mixer.remove_track_send(&mut h.s, "v", 1);
    assert!(h.mixer.revision() > notified, "the save is still parked");
    assert_eq!(h.sends("v"), ["a", "c"]);
    assert_eq!(save, Some(SaveRequest));
    h.save();
}

#[test]
fn an_fx_edit_made_from_before_a_reorder_keeps_the_reorder() {
    let mut h = host(vec![vocal("v")], vec![]);
    let before = h.track("v").fx.clone().unwrap_or_else(TrackFx::flat);
    let reordered: Vec<FxKind> = before.order.iter().rev().copied().collect();

    let save = h
        .mixer
        .reorder_track_inserts(&mut h.s, "v", reordered.clone());
    // An on/off clicked on the panel as it was built before the reorder.
    h.mixer.set_track_fx_live(
        &mut h.s,
        "v",
        TrackFx {
            comp_on: !before.comp_on,
            ..before.clone()
        },
    );
    let fx = h.track("v").fx.clone().unwrap();
    assert_eq!(fx.order, reordered);
    assert_eq!(fx.comp_on, !before.comp_on);
    assert!(save.is_some());
    h.save();
}

#[test]
fn a_send_turned_back_on_while_a_bus_is_soloed_is_heard_as_it_is_dragged() {
    // ... not from the release.
    let mut h = host(
        vec![routed(
            vocal("v1"),
            None,
            vec![TrackSend::new("verb", SEND_OFF_DB)],
        )],
        vec![bus("verb")],
    );
    h.edit(|m, s| m.toggle_bus_solo(s, "verb"));
    assert_eq!(h.engine.muted("v1-c0"), Some(true), "off: no route");

    h.mixer.set_track_send_level_live(&mut h.s, "v1", 0, -12.0);
    assert_eq!(h.engine.muted("v1-c0"), Some(false));
    h.mixer
        .set_track_send_level_live(&mut h.s, "v1", 0, SEND_OFF_DB);
    assert_eq!(h.engine.muted("v1-c0"), Some(true));
}

// --- backing A/B (play_reference) ------------------------------------------------

#[test]
fn the_ab_flips_the_audible_backing_and_pushes_the_swap_to_the_engine() {
    let mut h = host(vec![], vec![]);
    assert_eq!(h.mixer.active_backing(&h.s).unwrap().id, "acc");

    h.mixer.set_play_reference(&h.s, true);

    assert_eq!(h.mixer.active_backing(&h.s).unwrap().id, "ref");
    assert_eq!(h.engine.muted("acc-c0"), Some(true));
    assert_eq!(h.engine.muted("ref-c0"), Some(false));

    h.mixer.set_play_reference(&h.s, false);
    assert_eq!(h.engine.muted("acc-c0"), Some(false));
    assert_eq!(h.engine.muted("ref-c0"), Some(true));
}

#[test]
fn set_play_reference_silently_changes_the_flag_without_engine_pushes() {
    let mut h = host(vec![], vec![]);
    h.mixer.set_play_reference_silently(true);
    assert!(h.mixer.play_reference());
    assert!(h.engine.pushed().volumes.is_empty());
}

#[test]
fn visible_note_count_counts_only_the_shown_backing_plus_the_active_takes() {
    // The workspace shows ONE backing at a time, so the transport bar's note
    // count must never include the hidden backing's notes (the model's
    // active_note_count deliberately counts both; see song_practice's tests).
    let note = |id: &str| PracticeNote {
        id: id.to_string(),
        position_ms: 100,
        end_ms: None,
        content: format!("note {id}"),
        created_at: at(),
        updated_at: at(),
    };
    let base = practice_of(vec![vocal("v1"), vocal("v2")], vec![], 1.0);
    let with_notes = |t: &Arc<AudioTrack>, notes: Vec<PracticeNote>| {
        Arc::new(AudioTrack {
            notes,
            ..(**t).clone()
        })
    };
    let p = SongPractice {
        accompaniment: Some(with_notes(
            base.accompaniment.as_ref().unwrap(),
            vec![note("a1")],
        )),
        reference: Some(with_notes(
            base.reference.as_ref().unwrap(),
            vec![note("r1"), note("r2")],
        )),
        vocal_tracks: vec![
            with_notes(&base.vocal_tracks[0], vec![note("t1")]),
            Arc::clone(&base.vocal_tracks[1]),
        ],
        ..base.clone()
    };
    let mut h = Host::new(Some(p));

    // Accompaniment shown: its 1 note + the take's 1; the reference's 2 stay
    // hidden with the lane.
    assert_eq!(h.mixer.visible_note_count(&h.s), 2);
    // Original shown: the reference's 2 + the take's 1; the accompaniment's 1
    // drops out.
    h.mixer.set_play_reference(&h.s, true);
    assert_eq!(h.mixer.visible_note_count(&h.s), 3);
    // The model keeps counting both backings regardless of the toggle.
    assert_eq!(h.practice().active_note_count(), 4);
}

#[test]
fn a_solo_left_on_the_hidden_backing_does_not_silence_the_mix() {
    let mut h = host(vec![vocal("v1")], vec![]);
    // Listen to the original alone: switch to it and solo it.
    h.mixer.set_play_reference(&h.s, true);
    h.edit(|m, s| Some(m.toggle_solo(s, "ref")));
    // Back to singing over the accompaniment: the reference lane, with its
    // lit S, is no longer shown anywhere.
    h.mixer.set_play_reference(&h.s, false);
    h.mixer.resync_playback(&h.s).wait().unwrap();
    let muted = h.synced_mutes();
    assert!(!muted["acc-c0"], "the shown backing must play");
    assert!(!muted["v1-c0"], "the take must play");
    assert!(muted["ref-c0"], "the hidden backing stays silent");
}

// --- resync_playback -----------------------------------------------------------

#[test]
fn resync_mutes_exactly_the_take_being_punched_into_while_recording() {
    let mut h = host(vec![vocal("v1"), vocal("v2")], vec![]);
    h.s.recording = true;
    h.s.recording_target = Some("v1".into());

    h.mixer.resync_playback(&h.s).wait().unwrap();

    let muted = h.synced_mutes();
    assert!(muted["v1-c0"]);
    assert!(!muted["v2-c0"]);
}

#[test]
fn resync_pushes_reverb_and_master_gain_alongside_the_graph() {
    let h = Host::new(Some(practice_of(vec![], vec![], 0.5)));
    let pending = h.mixer.resync_playback(&h.s);
    // Another engine than the graph one is synced in place.
    assert!(pending.is_done());
    pending.wait().unwrap();
    let p = h.engine.pushed();
    assert_eq!(p.master_gain, Some(0.5));
    assert!(p.reverb.is_some());
    assert_eq!(p.last_buses.as_deref(), Some(&[][..]));
}

#[test]
fn resync_flattens_each_lane_to_one_source_per_clip_through_the_lane_chain() {
    // Backings go to the master dry of routing (no bus, no sends); an empty
    // take contributes none.
    let take = AudioTrack {
        clips: vec![
            AudioClip::new("c1", "c1.wav", 1000),
            AudioClip {
                start_ms: 3000,
                ..AudioClip::new("c2", "c2.wav", 500)
            },
        ],
        volume: 0.7,
        pan: -0.5,
        ..routed(vocal("v1"), Some("b"), vec![send("b")])
    };
    let empty = AudioTrack::new("v2", AudioTrackKind::Vocal, "v2", at());
    let mut p = practice_of(vec![take, empty], vec![bus("b")], 1.0);
    p.accompaniment = Some(Arc::new(AudioTrack {
        bus_id: Some("b".into()),
        sends: vec![send("b")],
        ..backing("acc", AudioTrackKind::Accompaniment)
    }));
    let h = Host::new(Some(p));
    h.mixer.resync_playback(&h.s).wait().unwrap();
    let pushed = h.engine.pushed();
    let tracks = pushed.last_tracks.as_ref().unwrap();
    let ids: Vec<&str> = tracks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["acc-c0", "ref-c0", "c1", "c2"]);
    let acc = &tracks[0];
    assert!(!acc.is_vocal && acc.bus_id.is_none() && acc.sends.is_empty());
    assert_eq!(acc.lane_id.as_deref(), Some("acc"));
    assert!(tracks[1].muted, "the hidden backing");
    let c2 = &tracks[3];
    assert_eq!((c2.start_ms, c2.duration_ms), (3000, 500));
    assert_eq!((c2.volume, c2.pan), (0.7, -0.5));
    assert!(c2.is_vocal);
    assert_eq!(c2.bus_id.as_deref(), Some("b"));
    assert_eq!(c2.sends, vec![send("b")]);
    assert_eq!(c2.lane_id.as_deref(), Some("v1"));
}

#[test]
fn a_phrase_sung_again_over_a_take_plays_alone_where_they_overlap() {
    // The take 0-60 s, then the phrase at 20 s re-sung onto it as a clip of
    // its own: the take plays either side of it, the second stretch from 30 s
    // into its file, each fading where the phrase cuts in or lets go.
    let take = AudioTrack {
        clips: vec![
            AudioClip::new("take", "take.wav", 60_000),
            AudioClip {
                start_ms: 20_000,
                ..AudioClip::new("phrase", "phrase.wav", 10_000)
            },
        ],
        ..vocal("v1")
    };
    let h = Host::new(Some(practice_of(vec![take], vec![], 1.0)));
    h.mixer.resync_playback(&h.s).wait().unwrap();
    let pushed = h.engine.pushed();
    let tracks: Vec<_> = pushed
        .last_tracks
        .as_ref()
        .unwrap()
        .iter()
        .filter(|t| t.lane_id.as_deref() == Some("v1"))
        .map(|t| {
            (
                t.id.as_str(),
                t.path.as_str(),
                t.start_ms,
                t.duration_ms,
                t.src_offset_ms,
                t.fade_in,
                t.fade_out,
            )
        })
        .collect();
    assert_eq!(
        tracks,
        [
            ("take", "take.wav", 0, 20_000, 0, false, true),
            ("take#1", "take.wav", 30_000, 30_000, 30_000, true, false),
            ("phrase", "phrase.wav", 20_000, 10_000, 0, false, false),
        ]
    );
}

#[test]
fn resync_without_a_practice_does_nothing() {
    let h = Host::new(None);
    assert!(h.mixer.resync_playback(&h.s).is_done());
    let p = h.engine.pushed();
    assert!(p.last_tracks.is_none() && p.master_gain.is_none() && p.reverb.is_none());
}

// --- bus routing cycle refusal -------------------------------------------------

#[test]
fn set_bus_output_refuses_a_route_that_would_close_a_feedback_loop() {
    // B already outputs into A; routing A into B would close the loop.
    let mut h = host(
        vec![],
        vec![bus("busA"), bus_routed("busB", Some("busA"), vec![])],
    );
    h.edit(|m, s| m.set_bus_output(s, "busA", Some("busB")));

    assert_eq!(h.s.toasts.len(), 1);
    assert!(h.s.toasts[0].contains("feedback loop"));
    assert!(h.bus("busA").output_bus_id.is_none());
    assert_eq!(h.saves, 0);
}

#[test]
fn add_bus_send_refuses_a_send_that_would_close_a_feedback_loop() {
    let mut h = host(
        vec![],
        vec![bus("busA"), bus_routed("busB", Some("busA"), vec![])],
    );
    h.edit(|m, s| m.add_bus_send(s, "busA", "busB"));

    assert_eq!(h.s.toasts.len(), 1);
    assert!(h.s.toasts[0].contains("feedback loop"));
    assert!(h.bus("busA").sends.is_empty());
    assert_eq!(h.saves, 0);
}

#[test]
fn a_legal_route_still_applies_and_rebuilds() {
    let mut h = host(vec![], vec![bus("busA"), bus("busB")]);
    h.edit(|m, s| m.set_bus_output(s, "busA", Some("busB")));

    assert_eq!(h.bus("busA").output_bus_id.as_deref(), Some("busB"));
    assert_eq!(h.saves, 1);
    assert!(h.engine.pushed().last_buses.is_some());
    assert!(h.s.toasts.is_empty());
}

#[test]
fn a_bus_never_routes_into_itself() {
    let mut h = host(vec![], vec![bus("busA")]);
    h.edit(|m, s| m.set_bus_output(s, "busA", Some("busA")));
    h.edit(|m, s| m.add_bus_send(s, "busA", "busA"));
    assert_eq!(h.saves, 0);
    assert!(h.s.toasts.is_empty(), "refused before the cycle check");
    // Back to the master is never a loop.
    h.edit(|m, s| m.set_bus_output(s, "busA", None));
    assert_eq!(h.saves, 1);
}

// --- duplicate_bus ---------------------------------------------------------------

#[test]
fn the_copy_carries_every_mix_setting_solo_included() {
    // A copy that is not in the solo group is inaudible the moment you make
    // it — which is exactly when a solo is active. Duplicating a track carries
    // solo (a struct copy); this path spells the fields out.
    let mut h = host(
        vec![],
        vec![FxBus {
            color_value: Some(0xFF01_0203),
            volume: 0.33,
            muted: true,
            soloed: true,
            pan: -0.25,
            ..FxBus::new(
                "busA",
                "Lead",
                TrackFx {
                    eq_on: true,
                    ..TrackFx::flat()
                },
            )
        }],
    );

    h.edit(|m, s| m.duplicate_bus(s, "busA"));

    let copy = h
        .practice()
        .fx_buses
        .iter()
        .find(|b| b.id != "busA")
        .cloned()
        .unwrap();
    assert_eq!(copy.name, "Lead copy");
    assert_close(copy.volume, 0.33, 1e-9);
    assert_close(copy.pan, -0.25, 1e-9);
    assert!(copy.muted);
    assert!(copy.soloed);
    assert_eq!(copy.color_value, Some(0xFF01_0203));
    assert!(copy.fx.eq_on);
    // Inserted right after its source, and selected.
    assert_eq!(h.practice().fx_buses[1].id, copy.id);
    assert_eq!(h.s.selected_buses, std::slice::from_ref(&copy.id));
}

// --- send retargeting --------------------------------------------------------------

#[test]
fn retargeting_a_slot_onto_a_bus_another_slot_already_feeds_is_refused() {
    // Two slots into one bus feed it twice, so the bus hears the sum and
    // neither fader reads what you hear. add_track_send refuses it and the
    // menu hides an already-used bus; the view model has to hold the line
    // too, for the window where the menu's options came from a `sends` the
    // previous retarget's save has not rebuilt yet.
    let mut h = host(
        vec![routed(vocal("t1"), None, vec![send("busA"), send("busB")])],
        vec![bus("busA"), bus("busB")],
    );

    h.edit(|m, s| m.set_track_send_target(s, "t1", 1, "busA"));

    assert_eq!(h.sends("t1"), ["busA", "busB"]);
    assert_eq!(h.saves, 0);
}

#[test]
fn retargeting_a_bus_send_onto_an_already_fed_bus_is_refused() {
    let mut h = host(
        vec![],
        vec![
            bus_routed("busA", None, vec![send("busB"), send("busC")]),
            bus("busB"),
            bus("busC"),
        ],
    );

    h.edit(|m, s| m.set_bus_send_target(s, "busA", 1, "busB"));

    let targets: Vec<String> = h
        .bus("busA")
        .sends
        .iter()
        .map(|s| s.bus_id.clone())
        .collect();
    assert_eq!(targets, ["busB", "busC"]);
    assert_eq!(h.saves, 0);
}

#[test]
fn a_genuine_retarget_still_applies_and_rebuilds() {
    let mut h = host(
        vec![routed(vocal("t1"), None, vec![send("busA")])],
        vec![bus("busA"), bus("busB")],
    );

    h.edit(|m, s| m.set_track_send_target(s, "t1", 0, "busB"));

    assert_eq!(h.sends("t1"), ["busB"]);
    assert_eq!(h.saves, 1);
}

#[test]
fn sends_are_capped_and_never_doubled() {
    let mut h = host(
        vec![vocal("t1")],
        vec![bus("a"), bus("b"), bus("c"), bus("d"), bus("e")],
    );
    for b in ["a", "a", "b", "c", "d", "e"] {
        h.edit(|m, s| m.add_track_send(s, "t1", b));
    }
    assert_eq!(h.sends("t1"), ["a", "b", "c", "d"]);
    assert_eq!(h.saves, 4);
    // Slots past the end are no edit.
    h.edit(|m, s| m.remove_track_send(s, "t1", 4));
    h.mixer.set_track_send_level_live(&mut h.s, "t1", 9, -6.0);
    assert_eq!(h.saves, 4);
}

// --- the output and a send never feed one bus -------------------------------------
//
// A take out to a bus that also sends to it feeds the bus twice: the bus
// hears the sum, at the fader plus the send level, and neither reads what
// you hear.

#[test]
fn no_send_goes_to_the_bus_the_output_feeds() {
    let mut h = host(
        vec![routed(vocal("t1"), Some("a"), vec![send("b")])],
        vec![bus("a"), bus("b")],
    );
    h.edit(|m, s| m.add_track_send(s, "t1", "a"));
    h.edit(|m, s| m.set_track_send_target(s, "t1", 0, "a"));
    assert_eq!(h.sends("t1"), ["b"]);
    assert_eq!(h.saves, 0);

    let mut h = host(
        vec![],
        vec![
            bus_routed("a", Some("b"), vec![send("c")]),
            bus("b"),
            bus("c"),
        ],
    );
    h.edit(|m, s| m.add_bus_send(s, "a", "b"));
    h.edit(|m, s| m.set_bus_send_target(s, "a", 0, "b"));
    let targets: Vec<String> = h.bus("a").sends.iter().map(|s| s.bus_id.clone()).collect();
    assert_eq!(targets, ["c"]);
    assert_eq!(h.saves, 0);
}

#[test]
fn routing_the_output_to_a_bus_takes_the_send_into_it_away() {
    let mut h = host(
        vec![routed(vocal("t1"), None, vec![send("a"), send("b")])],
        vec![bus("a"), bus("b")],
    );
    h.edit(|m, s| Some(m.set_track_bus(s, "t1", Some("a"))));
    assert_eq!(h.track("t1").bus_id.as_deref(), Some("a"));
    assert_eq!(h.sends("t1"), ["b"]);
    // Back to the master: the remaining send stays.
    h.edit(|m, s| Some(m.set_track_bus(s, "t1", None)));
    assert_eq!(h.sends("t1"), ["b"]);

    let mut h = host(
        vec![],
        vec![
            bus_routed("a", None, vec![send("b"), send("c")]),
            bus("b"),
            bus("c"),
        ],
    );
    h.edit(|m, s| m.set_bus_output(s, "a", Some("c")));
    let a = h.bus("a");
    assert_eq!(a.output_bus_id.as_deref(), Some("c"));
    let targets: Vec<String> = a.sends.iter().map(|s| s.bus_id.clone()).collect();
    assert_eq!(targets, ["b"]);
}

// --- delete_bus ------------------------------------------------------------------

#[test]
fn delete_bus_re_routes_every_feeder_to_the_master_and_clears_the_selection() {
    let mut h = host(
        vec![routed(vocal("v1"), Some("busA"), vec![send("busA")])],
        vec![bus("busA"), bus_routed("busB", Some("busA"), vec![])],
    );
    h.edit(|m, s| m.delete_bus(s, "busA"));

    let v1 = h.track("v1");
    assert!(v1.bus_id.is_none());
    assert!(v1.sends.is_empty());
    let ids: Vec<&str> = h
        .practice()
        .fx_buses
        .iter()
        .map(|b| b.id.as_str())
        .collect();
    assert_eq!(ids, ["busB"]);
    assert!(h.bus("busB").output_bus_id.is_none());
    assert_eq!(h.s.cleared_buses, ["busA"]);
    assert_eq!(h.saves, 1);
    assert!(h.engine.pushed().last_tracks.is_some()); // graph rebuilt
}

// --- create_bus ------------------------------------------------------------------

#[test]
fn create_bus_appends_a_default_named_bus_and_selects_it_through_the_host_hook() {
    let mut h = host(vec![], vec![bus("busA"), bus("busB")]);
    let id = h.create_bus("   ");

    assert!(!id.is_empty());
    assert_eq!(h.practice().fx_buses.len(), 3);
    assert_eq!(h.practice().fx_buses[2].name, "Bus 3");
    assert_eq!(h.s.selected_buses, [id]);
}

#[test]
fn create_bus_without_a_practice_creates_nothing() {
    let mut h = Host::new(None);
    assert!(h.mixer.create_bus(&mut h.s, "Verb").is_none());
    assert!(h.s.selected_buses.is_empty());
}

#[test]
fn a_new_bus_never_takes_the_default_name_of_one_still_there() {
    // Named by count, a delete then a create left two buses called "Bus 2",
    // which the routing menus list by name alone.
    let mut h = host(vec![], vec![]);
    let a = h.create_bus("");
    h.create_bus("");
    h.edit(|m, s| m.delete_bus(s, &a));
    h.create_bus("");
    assert_eq!(h.bus_names(), ["Bus 2", "Bus 3"]);
}

#[test]
fn a_second_copy_of_a_bus_gets_a_name_of_its_own() {
    let mut h = host(vec![], vec![]);
    let a = h.create_bus("");
    h.edit(|m, s| m.duplicate_bus(s, &a));
    h.edit(|m, s| m.duplicate_bus(s, &a));
    let mut names = h.bus_names();
    names.sort();
    assert_eq!(names, ["Bus 1", "Bus 1 copy", "Bus 1 copy 2"]);
}

#[test]
fn a_bus_is_renamed_to_its_trimmed_name_and_never_to_nothing() {
    let mut h = host(vec![], vec![bus("busA")]);
    h.edit(|m, s| m.rename_bus(s, "busA", "  Verb \u{FEFF}"));
    assert_eq!(h.bus("busA").name, "Verb");
    h.edit(|m, s| m.rename_bus(s, "busA", " \t "));
    assert_eq!(h.bus("busA").name, "Verb");
    assert_eq!(h.saves, 1);
}

// --- bus solo --------------------------------------------------------------------

#[test]
fn soloing_a_bus_keeps_its_transitive_feeders_audible_and_mutes_the_rest() {
    // v1 feeds busB which outputs into busA; v2 goes straight to master.
    let mut h = host(
        vec![routed(vocal("v1"), Some("busB"), vec![]), vocal("v2")],
        vec![bus("busA"), bus_routed("busB", Some("busA"), vec![])],
    );
    h.edit(|m, s| m.toggle_bus_solo(s, "busA"));

    assert_eq!(
        h.engine.muted("v1-c0"),
        Some(false),
        "the take feeding the soloed bus must stay audible"
    );
    assert_eq!(h.engine.muted("v2-c0"), Some(true));
    assert_eq!(
        h.engine.muted("acc-c0"),
        Some(true),
        "a bus solo isolates: the backing drops out too"
    );
    assert_eq!(h.engine.bus_muted("busA"), Some(false));
    assert_eq!(
        h.engine.bus_muted("busB"),
        Some(false),
        "the carrying bus is part of the soloed path"
    );
}

#[test]
fn a_bus_fader_pushes_the_bus_muted_by_the_solo_it_is_not_in() {
    let mut h = host(
        vec![routed(vocal("v1"), Some("busA"), vec![])],
        vec![bus("busA"), bus("busB")],
    );
    h.edit(|m, s| Some(m.toggle_solo(s, "v1")));
    h.mixer.set_bus_volume_live(&mut h.s, "busB", 0.4);
    assert_eq!(h.engine.pushed().bus_volumes["busB"], (0.4, true));
    h.mixer.set_bus_volume_live(&mut h.s, "busA", 0.6);
    assert_eq!(h.engine.pushed().bus_volumes["busA"], (0.6, false));
    assert_close(h.bus("busA").volume, 0.6, 0.0);
}

// --- composite display waveforms -------------------------------------------------

#[test]
fn master_waveform_peaks_folds_audible_takes_the_live_recording_and_the_master_gain() {
    // v1's single peak (0.8) lands mid-clip → bin 500 of 1000.
    let mut h = Host::new(Some(practice_of(
        vec![vocal_with("v1", vec![0.8])],
        vec![],
        0.5,
    )));
    let peaks = h.master();
    assert_close(peaks[500], 0.4, 1e-9); // 0.8 × master 0.5
    assert_eq!(peaks[100], 0.0);

    // The in-flight take isn't a track yet — its live envelope folds in.
    h.s.recording = true;
    h.s.rec_peaks = vec![0.5];
    h.s.rec_duration_ms = 2000; // single live peak at ~1000 ms → bin 100
    let peaks = h.master();
    assert_close(peaks[100], 0.25, 1e-9); // 0.5 × master 0.5
    assert_close(peaks[500], 0.4, 1e-9);

    // A take started at 0 draws its pre-roll before the timeline, which the
    // stop cuts: none of it lands in the first bin.
    h.s.rec_peaks = vec![0.9, 0.1];
    h.s.rec_start_ms = -300;
    h.s.rec_duration_ms = 400; // 0.9 at -200 ms, 0.1 at 0 ms
    assert_close(h.master()[0], 0.05, 1e-9);

    // Nor does a take sung on past the timeline's end (10 s) pile into the
    // last bin.
    h.s.rec_peaks = vec![0.2, 0.9];
    h.s.rec_start_ms = 8000;
    h.s.rec_duration_ms = 4000; // 0.2 at 9 s, 0.9 at 11 s
    let peaks = h.master();
    assert_close(peaks[900], 0.1, 1e-9);
    assert_eq!(peaks[999], 0.0);

    // Muting the take removes its envelope but keeps the live one.
    h.s.recording = false;
    h.edit(|m, s| Some(m.toggle_mute(s, "v1")));
    assert_eq!(h.master()[500], 0.0);
}

#[test]
fn master_waveform_peaks_leaves_out_a_take_whose_every_route_is_muted() {
    let mut h = host(
        vec![routed(vocal_with("v1", vec![0.8]), Some("busA"), vec![])],
        vec![bus("busA"), bus("verb")],
    );
    assert_close(h.master()[500], 0.8, 1e-9);

    h.edit(|m, s| m.toggle_bus_mute(s, "busA"));
    assert_eq!(h.master()[500], 0.0);

    // A send to an unmuted bus still carries it, at the send's level.
    h.edit(|m, s| m.add_track_send(s, "v1", "verb"));
    h.mixer.set_track_send_level_live(&mut h.s, "v1", 0, -6.0);
    assert_close(h.master()[500], 0.8 * 10f64.powf(-6.0 / 20.0), 1e-9);
}

// The master and bus lanes rebuild on every notification of the view model,
// and each rebuild folded every clip's peaks again (~0.5 ms over 12
// three-clip lanes in a debug build). Each envelope is now folded once per
// change of what it reads — and must still follow every such change: a fresh
// controller over the same state is the oracle.
#[test]
fn the_lane_envelopes_are_refolded_exactly_when_what_they_show_changes() {
    let mut h = host(
        vec![
            routed(vocal_with("v1", vec![0.8]), Some("busA"), vec![]),
            vocal_with("v2", vec![0.3, 0.6]),
        ],
        vec![bus("busA")],
    );
    let fresh_master = |h: &Host| h.fresh().master();
    let fresh_bus = |h: &Host| h.fresh().bus_lane("busA");

    let master = h.master();
    let bus_lane = h.bus_lane("busA");
    assert!(Arc::ptr_eq(&h.master(), &master));
    assert!(Arc::ptr_eq(&h.bus_lane("busA"), &bus_lane));

    let expect_refolded = |h: &mut Host, what: &str, change: &dyn Fn(&mut Host)| {
        change(h);
        let m = h.master();
        let b = h.bus_lane("busA");
        assert_eq!(*m, *fresh_master(h), "{what}");
        assert_eq!(*b, *fresh_bus(h), "{what}");
        assert!(Arc::ptr_eq(&h.master(), &m), "{what}: then kept");
        (m, b)
    };

    let (master, _) = expect_refolded(&mut h, "a fader", &|h| {
        h.mixer.set_track_volume_live(&mut h.s, "v1", 0.5);
    });
    assert_close(master[500], 0.4, 1e-9);
    expect_refolded(&mut h, "a mute", &|h| {
        h.edit(|m, s| Some(m.toggle_mute(s, "v2")));
    });
    expect_refolded(&mut h, "a solo", &|h| {
        h.edit(|m, s| Some(m.toggle_solo(s, "v2")));
    });
    expect_refolded(&mut h, "the master", &|h| {
        h.mixer.set_master_volume_live(&mut h.s, 0.5);
    });
    expect_refolded(&mut h, "the A/B backing", &|h| {
        h.mixer.set_play_reference(&h.s, true);
    });
    expect_refolded(&mut h, "a clip move", &|h| {
        h.s.update_track("v1", &|t| AudioTrack {
            clips: t
                .clips
                .iter()
                .map(|c| AudioClip {
                    start_ms: 2000,
                    ..c.clone()
                })
                .collect(),
            ..t.clone()
        });
    });
    expect_refolded(&mut h, "a routing change", &|h| {
        let p = h.practice().clone();
        h.s.practice = Some(SongPractice {
            fx_buses: vec![Arc::new(bus("busA")), Arc::new(bus("busB"))],
            ..p
        });
    });
    expect_refolded(&mut h, "a take starting", &|h| {
        h.s.recording = true;
        h.s.recording_target = Some("v1".into());
        h.s.rec_peaks = vec![0.5];
        h.s.rec_duration_ms = 2000;
    });
    // A record tick reaches only the master: no bus lane folds the live take
    // in, yet every bus lane was refolded 20 times a second for it.
    let bus_over_take = h.bus_lane("busA");
    h.s.rec_peaks = vec![0.5, 0.7];
    h.s.rec_duration_ms = 2025;
    assert!(Arc::ptr_eq(&h.bus_lane("busA"), &bus_over_take));
    expect_refolded(&mut h, "a recording tick", &|h| {
        h.s.rec_peaks = vec![0.5, 0.9];
        h.s.rec_duration_ms = 2050;
    });
    let (master, bus_lane) = expect_refolded(&mut h, "the take stopping", &|h| {
        h.s.recording = false;
        h.s.rec_peaks = Vec::new();
    });
    // What no envelope shows keeps them: keyed on whole tracks and the bus
    // list, a pan or a bus level tick refolded every lane at pointer rate.
    h.mixer.set_track_pan_live(&mut h.s, "v1", 0.3);
    h.mixer.set_bus_volume_live(&mut h.s, "busA", 0.4);
    assert!(Arc::ptr_eq(&h.master(), &master));
    assert!(Arc::ptr_eq(&h.bus_lane("busA"), &bus_lane));
    expect_refolded(&mut h, "a bus mute", &|h| {
        h.edit(|m, s| m.toggle_bus_mute(s, "busA"));
    });
    expect_refolded(&mut h, "a send", &|h| {
        h.edit(|m, s| m.add_track_send(s, "v2", "busB"));
    });
}

#[test]
fn bus_waveform_peaks_folds_only_takes_routed_into_the_bus_including_through_an_upstream_bus() {
    let h = host(
        vec![
            routed(vocal_with("v1", vec![0.8]), Some("busB"), vec![]),
            vocal_with("v2", vec![0.6]), // straight to master
        ],
        vec![bus("busA"), bus_routed("busB", Some("busA"), vec![])],
    );
    // v1 reaches busA via busB; v2 feeds neither bus.
    assert_close(h.bus_lane("busA")[500], 0.8, 1e-9);
    assert_close(h.bus_lane("busB")[500], 0.8, 1e-9);
    assert!(!h.bus_lane("busA").contains(&0.6));
}

#[test]
fn bus_waveform_peaks_follows_aux_sends_scaled_by_the_send_level() {
    // The ordinary reverb return: the take stays on the master and only FEEDS
    // the bus. Following the main output alone drew a flat line for a bus
    // that was plainly audible, because resync_playback does pass sends to
    // the engine.
    let h = host(
        vec![routed(vocal_with("v1", vec![0.8]), None, vec![send("rev")])],
        vec![bus("rev")],
    );
    assert_close(h.bus_lane("rev")[500], 0.8, 1e-9);
}

#[test]
fn a_send_level_scales_the_envelope_an_off_send_contributes_nothing() {
    let at_level = |db: f64| {
        host(
            vec![routed(
                vocal_with("v1", vec![0.8]),
                None,
                vec![TrackSend::new("rev", db)],
            )],
            vec![bus("rev")],
        )
    };
    assert_close(at_level(-6.0).bus_lane("rev")[500], 0.8 * 0.501187, 1e-5);
    assert_eq!(at_level(SEND_OFF_DB).bus_lane("rev")[500], 0.0);
}

#[test]
fn a_bus_to_bus_send_carries_the_envelope_onward_too() {
    let h = host(
        vec![routed(vocal_with("v1", vec![0.8]), Some("busB"), vec![])],
        vec![bus("busA"), bus_routed("busB", None, vec![send("busA")])],
    );
    // v1 reaches busA only through busB's send, not its main output.
    assert_close(h.bus_lane("busA")[500], 0.8, 1e-9);
}

#[test]
fn a_take_reaching_a_bus_two_ways_takes_the_louder_route() {
    let h = host(
        vec![routed(
            vocal_with("v1", vec![0.8]),
            Some("rev"), // unity
            vec![TrackSend::new("rev", -12.0)],
        )],
        vec![bus("rev")],
    );
    assert_close(h.bus_lane("rev")[500], 0.8, 1e-9);
}

#[test]
fn the_envelope_cache_can_be_turned_off_for_an_oracle() {
    let mut h = host(vec![vocal_with("v1", vec![0.8])], vec![]);
    let cached = h.master();
    assert!(Arc::ptr_eq(&h.master(), &cached));
    h.mixer.set_debug_no_envelope_cache(true);
    let a = h.master();
    assert!(!Arc::ptr_eq(&a, &h.master()));
    assert_eq!(*a, *cached);
}

#[test]
fn the_gain_reduction_meter_reads_the_clip_under_the_playhead() {
    struct Meter;
    impl PlaybackEngine for Meter {
        fn position(&self) -> Subscription<Duration> {
            unreachable!()
        }
        fn playing(&self) -> Subscription<bool> {
            unreachable!()
        }
        fn current_position(&self) -> Duration {
            unreachable!()
        }
        fn is_playing(&self) -> bool {
            unreachable!()
        }
        fn set_loop(&self, _: bool, _: i64, _: i64) {}
        fn sync(&self, _: Vec<TrackPlayback>, _: Vec<BusPlayback>) {}
        fn play(&self) {}
        fn pause(&self) {}
        fn seek(&self, _: Duration) {}
        fn set_volume(&self, _: &str, _: f64, _: bool) {}
        fn set_track_pan(&self, _: &str, _: f64) {}
        fn set_master_volume(&self, _: f64) {}
        fn supports_effects(&self) -> bool {
            false
        }
        fn set_track_fx(&self, _: &str, _: &TrackFx) {}
        fn set_bus_fx(&self, _: &str, _: &TrackFx) {}
        fn set_bus_volume(&self, _: &str, _: f64, _: bool) {}
        fn set_bus_pan(&self, _: &str, _: f64) {}
        fn bus_gain_reduction_db(&self, bus_id: &str) -> f64 {
            if bus_id == "b" {
                4.0
            } else {
                0.0
            }
        }
        fn set_track_send_level(&self, _: &str, _: usize, _: f64) {}
        fn set_bus_send_level(&self, _: &str, _: usize, _: f64) {}
        fn set_reverb(&self, _: &ReverbSettings) {}
        fn track_gain_reduction_db(&self, track_id: &str) -> f64 {
            match track_id {
                "c1" => 3.0,
                "c2" => 6.0,
                _ => 0.0,
            }
        }
        fn dispose(&self) {}
    }
    let take = AudioTrack {
        clips: vec![
            AudioClip::new("c1", "c1.wav", 1000),
            AudioClip {
                start_ms: 2000,
                ..AudioClip::new("c2", "c2.wav", 1000)
            },
        ],
        ..vocal("v1")
    };
    let mut s = State {
        practice: Some(practice_of(vec![take], vec![], 1.0)),
        ..State::default()
    };
    let mixer = MixerController::new(EngineHandle::Other(Arc::new(Meter)));
    assert!(!mixer.supports_effects());
    assert_eq!(mixer.gain_reduction_db(&s, "v1"), 3.0);
    s.position = Duration::from_millis(1500); // the gap between the clips
    assert_eq!(mixer.gain_reduction_db(&s, "v1"), 0.0);
    s.position = Duration::from_millis(2999);
    assert_eq!(mixer.gain_reduction_db(&s, "v1"), 6.0);
    s.position = Duration::from_millis(3000); // a clip's end is not in it
    assert_eq!(mixer.gain_reduction_db(&s, "v1"), 0.0);
    assert_eq!(mixer.gain_reduction_db(&s, "gone"), 0.0);
    assert_eq!(mixer.bus_gain_reduction_db("b"), 4.0);
}

#[test]
fn notifications_stop_at_dispose() {
    let mut h = host(vec![vocal("v1")], vec![]);
    h.mixer.dispose();
    let rev = h.mixer.revision();
    h.mixer.set_track_volume_live(&mut h.s, "v1", 0.2);
    h.mixer.set_play_reference(&h.s, true);
    assert_eq!(h.mixer.revision(), rev);
}

// --- the envelope cache ------------------------------------------------------------
//
// The lane envelopes are cached, keyed on what they read (see
// `MixerController::envelope`). A key that misses an input serves a stale
// envelope; this walk checks the cache against a fresh fold — a new host on
// the same state — after every one of thousands of random edits.

/// `[for (var i = 0; i < 1 + r.nextInt(n); i++) item()]`: Dart re-draws the
/// bound before every iteration, so the draws interleave exactly so.
fn dart_list<T>(
    r: &mut DartRandom,
    n: usize,
    mut item: impl FnMut(&mut DartRandom, usize) -> T,
) -> Vec<T> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < 1 + r.next_int(n) {
        out.push(item(r, i));
        i += 1;
    }
    out
}

fn pk(r: &mut DartRandom) -> Vec<f64> {
    dart_list(r, 30, |r, _| r.next_double())
}

fn walk_vocal(r: &mut DartRandom, id: &str, version: &str) -> AudioTrack {
    let clips = dart_list(r, 3, |r, c| {
        let duration_ms = 500 + r.next_i64(4000);
        let start_ms = r.next_i64(8000);
        AudioClip {
            id: format!("{id}-c{c}"),
            audio_file_path: format!("{id}-{c}.wav"),
            duration_ms,
            start_ms,
            waveform_peaks: pk(r),
        }
    });
    AudioTrack {
        version_id: Some(version.to_string()),
        clips,
        ..AudioTrack::new(id, AudioTrackKind::Vocal, id, at())
    }
}

fn walk_backing(r: &mut DartRandom, id: &str, kind: AudioTrackKind) -> AudioTrack {
    let peaks = pk(r);
    AudioTrack::new(id, kind, id, at()).with_audio(format!("{id}.wav"), 0, 10_000, peaks)
}

fn pick(r: &mut DartRandom, l: &[String]) -> String {
    l[r.next_int(l.len())].clone()
}

fn pick_or_null(r: &mut DartRandom, l: &[String]) -> Option<String> {
    if r.next_int(3) == 0 || l.is_empty() {
        None
    } else {
        Some(pick(r, l))
    }
}

const OPS: [&str; 23] = [
    "vol",
    "mute",
    "solo",
    "master",
    "ab",
    "route",
    "busOut",
    "busMute",
    "busSolo",
    "send",
    "sendLvl",
    "busSend",
    "busSendLvl",
    "clipMove",
    "clipDel",
    "addTake",
    "delTake",
    "interp",
    "timeline",
    "recStart",
    "recTick",
    "recStop",
    "selectWhileRec",
];

#[test]
fn the_envelope_cache_equals_a_fresh_fold_through_a_random_walk_of_edits() {
    for seed in 0..40 {
        let mut r = DartRandom::new(seed);
        let mut n = 0;
        let accompaniment = walk_backing(&mut r, "acc", AudioTrackKind::Accompaniment);
        let reference = walk_backing(&mut r, "ref", AudioTrackKind::Reference);
        let mut vocal_tracks = Vec::new();
        for i in 0..4 {
            let id = format!("t{n}");
            n += 1;
            vocal_tracks.push(Arc::new(walk_vocal(
                &mut r,
                &id,
                if i % 2 == 0 { "v1" } else { "v2" },
            )));
        }
        let mut h = Host::new(Some(SongPractice {
            accompaniment: Some(Arc::new(accompaniment)),
            reference: Some(Arc::new(reference)),
            versions: vec![
                Arc::new(TakeVersion::new("v1", "a", at())),
                Arc::new(TakeVersion::new("v2", "b", at())),
            ],
            active_version_id: Some("v1".into()),
            vocal_tracks,
            fx_buses: vec![
                Arc::new(bus("b0")),
                Arc::new(bus("b1")),
                Arc::new(bus("b2")),
            ],
            timeline_duration_ms: 12_000,
            ..SongPractice::new("p1", "S", at(), at())
        }));
        let tracks =
            |h: &Host| -> Vec<String> { h.s.all_tracks().iter().map(|t| t.id.clone()).collect() };
        let vocals = |h: &Host| -> Vec<String> {
            h.practice()
                .vocal_tracks
                .iter()
                .map(|t| t.id.clone())
                .collect()
        };
        let buses = |h: &Host| -> Vec<String> {
            h.mixer
                .fx_buses(&h.s)
                .iter()
                .map(|b| b.id.clone())
                .collect()
        };

        for step in 0..150 {
            let name = OPS[r.next_int(OPS.len())];
            match name {
                "vol" => {
                    let t = tracks(&h);
                    if !t.is_empty() {
                        let id = pick(&mut r, &t);
                        let v = r.next_double();
                        h.mixer.set_track_volume_live(&mut h.s, &id, v);
                    }
                }
                "mute" => {
                    let t = tracks(&h);
                    if !t.is_empty() {
                        let id = pick(&mut r, &t);
                        h.edit(|m, s| Some(m.toggle_mute(s, &id)));
                    }
                }
                "solo" => {
                    let t = tracks(&h);
                    if !t.is_empty() {
                        let id = pick(&mut r, &t);
                        h.edit(|m, s| Some(m.toggle_solo(s, &id)));
                    }
                }
                "master" => {
                    let v = r.next_double();
                    h.mixer.set_master_volume_live(&mut h.s, v);
                }
                "ab" => {
                    let v = r.next_bool();
                    h.mixer.set_play_reference(&h.s, v);
                }
                "route" => {
                    let v = vocals(&h);
                    if !v.is_empty() {
                        let id = pick(&mut r, &v);
                        let to = pick_or_null(&mut r, &buses(&h));
                        h.edit(|m, s| Some(m.set_track_bus(s, &id, to.as_deref())));
                    }
                }
                "busOut" => {
                    let b = buses(&h);
                    if !b.is_empty() {
                        let id = pick(&mut r, &b);
                        let to = pick_or_null(&mut r, &b);
                        h.edit(|m, s| m.set_bus_output(s, &id, to.as_deref()));
                    }
                }
                "busMute" => {
                    let b = buses(&h);
                    if !b.is_empty() {
                        let id = pick(&mut r, &b);
                        h.edit(|m, s| m.toggle_bus_mute(s, &id));
                    }
                }
                "busSolo" => {
                    let b = buses(&h);
                    if !b.is_empty() {
                        let id = pick(&mut r, &b);
                        h.edit(|m, s| m.toggle_bus_solo(s, &id));
                    }
                }
                "send" => {
                    let (v, b) = (vocals(&h), buses(&h));
                    if !v.is_empty() && !b.is_empty() {
                        let id = pick(&mut r, &v);
                        let to = pick(&mut r, &b);
                        h.edit(|m, s| m.add_track_send(s, &id, &to));
                    }
                }
                "sendLvl" => {
                    let with_sends: Vec<Arc<AudioTrack>> = h
                        .practice()
                        .vocal_tracks
                        .iter()
                        .filter(|t| !t.sends.is_empty())
                        .cloned()
                        .collect();
                    if !with_sends.is_empty() {
                        let t = &with_sends[r.next_int(with_sends.len())];
                        let slot = r.next_int(t.sends.len());
                        let level = -40.0 + r.next_double() * 40.0;
                        h.mixer
                            .set_track_send_level_live(&mut h.s, &t.id, slot, level);
                    }
                }
                "busSend" => {
                    let b = buses(&h);
                    if b.len() > 1 {
                        let from = pick(&mut r, &b);
                        let to = pick(&mut r, &b);
                        h.edit(|m, s| m.add_bus_send(s, &from, &to));
                    }
                }
                "busSendLvl" => {
                    let with_sends: Vec<Arc<FxBus>> = h
                        .mixer
                        .fx_buses(&h.s)
                        .iter()
                        .filter(|b| !b.sends.is_empty())
                        .cloned()
                        .collect();
                    if !with_sends.is_empty() {
                        let b = &with_sends[r.next_int(with_sends.len())];
                        let slot = r.next_int(b.sends.len());
                        let level = -40.0 + r.next_double() * 40.0;
                        h.mixer
                            .set_bus_send_level_live(&mut h.s, &b.id, slot, level);
                    }
                }
                "clipMove" => {
                    let v = vocals(&h);
                    if !v.is_empty() {
                        let id = pick(&mut r, &v);
                        // Dart drew one start per clip inside the update.
                        let starts: Vec<i64> = (0..h.track(&id).clips.len())
                            .map(|_| r.next_i64(9000))
                            .collect();
                        h.s.update_track(&id, &|t| AudioTrack {
                            clips: t
                                .clips
                                .iter()
                                .zip(&starts)
                                .map(|(c, &start_ms)| AudioClip {
                                    start_ms,
                                    ..c.clone()
                                })
                                .collect(),
                            ..t.clone()
                        });
                    }
                }
                "clipDel" => {
                    let v = vocals(&h);
                    if !v.is_empty() {
                        let id = pick(&mut r, &v);
                        h.s.update_track(&id, &|t| AudioTrack {
                            clips: if t.clips.len() > 1 {
                                t.clips[1..].to_vec()
                            } else {
                                t.clips.clone()
                            },
                            ..t.clone()
                        });
                    }
                }
                "addTake" => {
                    let id = format!("t{n}");
                    n += 1;
                    let version = if r.next_bool() { "v1" } else { "v2" };
                    let take = walk_vocal(&mut r, &id, version);
                    let mut p = h.practice().clone();
                    p.vocal_tracks.push(Arc::new(take));
                    h.s.practice = Some(p);
                }
                "delTake" => {
                    if !h.practice().vocal_tracks.is_empty() {
                        let id = pick(&mut r, &vocals(&h));
                        let mut p = h.practice().clone();
                        p.vocal_tracks.retain(|t| t.id != id);
                        h.s.practice = Some(p);
                    }
                }
                "interp" => {
                    let mut p = h.practice().clone();
                    p.active_version_id = Some(
                        if p.active_version_id.as_deref() == Some("v1") {
                            "v2"
                        } else {
                            "v1"
                        }
                        .to_string(),
                    );
                    h.s.practice = Some(p);
                }
                "timeline" => {
                    let mut p = h.practice().clone();
                    p.timeline_duration_ms = 8000 + r.next_i64(8000);
                    h.s.practice = Some(p);
                }
                "recStart" => {
                    h.s.recording = true;
                    h.s.recording_target = pick_or_null(&mut r, &vocals(&h));
                    h.s.rec_start_ms = r.next_i64(5000);
                    h.s.rec_peaks = pk(&mut r);
                    h.s.rec_duration_ms = h.s.rec_peaks.len() as i64 * 50;
                }
                "recTick" => {
                    if h.s.recording {
                        let v = r.next_double();
                        h.s.rec_peaks.push(v);
                        h.s.rec_duration_ms = h.s.rec_peaks.len() as i64 * 50;
                    }
                }
                "recStop" => {
                    h.s.recording = false;
                    h.s.rec_peaks = Vec::new();
                }
                "selectWhileRec" => {
                    h.s.recording_target = pick_or_null(&mut r, &vocals(&h));
                }
                _ => unreachable!(),
            }
            let fresh = h.fresh();
            assert_eq!(
                *h.master(),
                *fresh.master(),
                "seed {seed} step {step} {name} master"
            );
            for b in buses(&h) {
                assert_eq!(
                    *h.bus_lane(&b),
                    *fresh.bus_lane(&b),
                    "seed {seed} step {step} {name} bus {b}"
                );
            }
        }
    }
}

// --- audibility_test.dart ----------------------------------------------------------

#[test]
fn compute_audible_with_no_solo_only_mute_decides() {
    assert!(compute_audible(false, false, false));
    assert!(!compute_audible(false, true, false));
}

#[test]
fn compute_audible_with_a_solo_active_only_soloed_tracks_are_heard() {
    assert!(compute_audible(true, false, true));
    assert!(!compute_audible(false, false, true));
}

#[test]
fn compute_audible_mute_still_wins_over_solo() {
    // A soloed-but-muted track must be silent — previously mute was ignored
    // whenever any track was soloed, which made "M" appear to do nothing.
    assert!(!compute_audible(true, true, true));
}

// --- solo_resolution_test.dart -----------------------------------------------------

fn solo_track(id: &str, soloed: bool, bus_id: Option<&str>, sends: Vec<TrackSend>) -> AudioTrack {
    AudioTrack {
        soloed,
        bus_id: bus_id.map(str::to_string),
        sends,
        ..AudioTrack::new(id, AudioTrackKind::Vocal, id, at()).with_audio(
            format!("/{id}.wav"),
            0,
            1000,
            Vec::new(),
        )
    }
}

fn t(id: &str) -> AudioTrack {
    solo_track(id, false, None, Vec::new())
}

fn solo_bus(id: &str, soloed: bool, output_bus_id: Option<&str>, sends: Vec<TrackSend>) -> FxBus {
    FxBus {
        soloed,
        output_bus_id: output_bus_id.map(str::to_string),
        sends,
        ..bus(id)
    }
}

fn b(id: &str) -> FxBus {
    solo_bus(id, false, None, Vec::new())
}

#[test]
fn nothing_soloed_every_source_is_in_solo() {
    let solo = resolve_solo(
        &[t("t1"), solo_track("t2", false, Some("b1"), vec![])],
        &[b("b1")],
    );
    assert!(!solo.any_solo());
    assert!(solo.track_in_solo("t1"));
    assert!(solo.track_in_solo("t2"));
    assert!(solo.bus_in_solo("b1"));
}

#[test]
fn soloing_a_track_to_master_isolates_it_no_buses_audible() {
    let solo = resolve_solo(
        &[
            solo_track("t1", true, None, vec![]),
            solo_track("t2", false, Some("b1"), vec![]),
        ],
        &[b("b1")],
    );
    assert!(solo.any_solo());
    assert!(solo.track_in_solo("t1"));
    assert!(!solo.track_in_solo("t2"));
    // t1 goes straight to master, so its bus chain is empty.
    assert!(!solo.bus_in_solo("b1"));
}

#[test]
fn soloing_a_track_through_a_bus_keeps_that_bus_audible() {
    let solo = resolve_solo(
        &[
            solo_track("t1", true, Some("b1"), vec![]),
            solo_track("t2", false, Some("b2"), vec![]),
        ],
        &[b("b1"), b("b2")],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.bus_in_solo("b1"));
    assert!(!solo.track_in_solo("t2"));
    assert!(!solo.bus_in_solo("b2"));
}

#[test]
fn a_send_turned_all_the_way_down_does_not_carry_a_source_into_a_soloed_bus() {
    // Counted as a route, soloing the bus un-muted the take's dry path,
    // though the send fed the bus nothing.
    let solo = resolve_solo(
        &[
            solo_track("t1", false, None, vec![TrackSend::new("b1", SEND_OFF_DB)]),
            solo_track("t2", false, None, vec![TrackSend::new("b1", -12.0)]),
        ],
        &[solo_bus("b1", true, None, vec![])],
    );
    assert!(!solo.track_in_solo("t1"));
    assert!(solo.track_in_solo("t2"));
}

#[test]
fn soloing_a_bus_keeps_the_takes_feeding_it_audible() {
    let solo = resolve_solo(
        &[
            solo_track("t1", false, Some("b1"), vec![]), // feeds the soloed bus
            solo_track("t2", false, Some("b2"), vec![]), // unrelated
            t("t3"),                                     // straight to master
        ],
        &[solo_bus("b1", true, None, vec![]), b("b2")],
    );
    assert!(solo.bus_in_solo("b1"));
    assert!(solo.track_in_solo("t1"), "t1 feeds the soloed bus");
    assert!(!solo.track_in_solo("t2"));
    assert!(!solo.track_in_solo("t3"));
    assert!(!solo.bus_in_solo("b2"));
}

#[test]
fn soloing_a_downstream_bus_pulls_the_whole_input_chain_in() {
    // t1 -> b1 -> b2 -> master; solo b2.
    let solo = resolve_solo(
        &[solo_track("t1", false, Some("b1"), vec![])],
        &[
            solo_bus("b1", false, Some("b2"), vec![]),
            solo_bus("b2", true, None, vec![]),
        ],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.bus_in_solo("b1"));
    assert!(solo.bus_in_solo("b2"));
}

#[test]
fn soloing_an_upstream_bus_keeps_its_output_chain_to_master_audible() {
    // t1 -> b1 -> b2 -> master; solo b1. b2 must stay audible to carry b1 out.
    let solo = resolve_solo(
        &[solo_track("t1", false, Some("b1"), vec![])],
        &[solo_bus("b1", true, Some("b2"), vec![]), b("b2")],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.bus_in_solo("b1"));
    assert!(solo.bus_in_solo("b2"));
}

#[test]
fn soloing_an_empty_bus_is_silent_but_still_marks_the_bus_in_solo() {
    let solo = resolve_solo(
        &[solo_track("t1", false, Some("b2"), vec![])],
        &[
            solo_bus("b1", true, None, vec![]), // nothing feeds it
            b("b2"),
        ],
    );
    assert!(solo.bus_in_solo("b1"));
    assert!(!solo.track_in_solo("t1"));
    assert!(!solo.bus_in_solo("b2"));
}

#[test]
fn a_routing_cycle_does_not_hang_and_resolves_both_buses() {
    // b1 -> b2 -> b1 (cycle); t1 -> b1, solo t1.
    let solo = resolve_solo(
        &[solo_track("t1", true, Some("b1"), vec![])],
        &[
            solo_bus("b1", false, Some("b2"), vec![]),
            solo_bus("b2", false, Some("b1"), vec![]),
        ],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.bus_in_solo("b1"));
    assert!(solo.bus_in_solo("b2"));
}

#[test]
fn soloing_a_take_keeps_its_reverb_send_return_bus_audible() {
    // t1 -> master (dry) + post-fader send to reverb bR -> master. Solo t1:
    // its reverb tail must survive, so bR stays audible.
    let solo = resolve_solo(
        &[solo_track("t1", true, None, vec![send("bR")]), t("t2")],
        &[b("bR")],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.bus_in_solo("bR"), "t1's reverb must be heard");
    assert!(!solo.track_in_solo("t2"));
}

#[test]
fn soloing_a_send_only_return_bus_keeps_its_feeder_take_audible() {
    // bR fed ONLY by t1's send (t1 main -> master). Solo bR: t1 must stay
    // audible so its send actually reaches bR (a muted source zeroes sends).
    let solo = resolve_solo(
        &[solo_track("t1", false, None, vec![send("bR")]), t("t2")],
        &[solo_bus("bR", true, None, vec![])],
    );
    assert!(solo.bus_in_solo("bR"));
    assert!(solo.track_in_solo("t1"), "t1 feeds bR via a send");
    assert!(!solo.track_in_solo("t2"));
}

#[test]
fn a_non_soloed_take_that_only_sends_to_a_soloed_bus_is_dropped() {
    // t2 sends to bR but bR is not soloed and t1 (soloed) does not feed bR.
    let solo = resolve_solo(
        &[
            solo_track("t1", true, None, vec![]),
            solo_track("t2", false, None, vec![send("bR")]),
        ],
        &[b("bR")],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(!solo.track_in_solo("t2"));
    assert!(!solo.bus_in_solo("bR"));
}

#[test]
fn track_solo_and_bus_solo_combine_as_the_union_of_both_isolations() {
    // t1 soloed to master; b1 soloed fed by t2.
    let solo = resolve_solo(
        &[
            solo_track("t1", true, None, vec![]),
            solo_track("t2", false, Some("b1"), vec![]),
            t("t3"),
        ],
        &[solo_bus("b1", true, None, vec![])],
    );
    assert!(solo.track_in_solo("t1"));
    assert!(solo.track_in_solo("t2"));
    assert!(solo.bus_in_solo("b1"));
    assert!(!solo.track_in_solo("t3"));
}

// Hand-written cases pin the shapes we reasoned about. This one generates
// routing graphs — including ones a single device cannot build, since a merge
// can — and checks the two properties the feature actually promises, on every
// one of them.
#[test]
fn resolve_solo_invariants_hold_over_random_graphs() {
    /// Every bus a source reaches, following main outputs and aux sends.
    fn route_of(
        bus_id: Option<&str>,
        sends: &[TrackSend],
        by_id: &HashMap<String, FxBus>,
    ) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut queue: Vec<String> = Vec::new();
        let push = |id: Option<&str>, seen: &mut Vec<String>, queue: &mut Vec<String>| {
            if let Some(id) = id {
                if by_id.contains_key(id) && !seen.iter().any(|s| s == id) {
                    seen.push(id.to_string());
                    queue.push(id.to_string());
                }
            }
        };
        push(bus_id, &mut seen, &mut queue);
        for s in sends {
            push(Some(&s.bus_id), &mut seen, &mut queue);
        }
        while let Some(id) = queue.pop() {
            let b = &by_id[&id];
            push(b.output_bus_id.as_deref(), &mut seen, &mut queue);
            for s in &b.sends {
                push(Some(&s.bus_id), &mut seen, &mut queue);
            }
        }
        seen
    }

    for seed in 0..200 {
        let mut rnd = DartRandom::new(seed);
        let bus_count = 1 + rnd.next_int(4);
        let ids: Vec<String> = (0..bus_count).map(|i| format!("b{i}")).collect();
        let maybe_bus = |rnd: &mut DartRandom| -> Option<String> {
            if rnd.next_int(3) == 0 {
                None
            } else {
                Some(ids[rnd.next_int(bus_count)].clone())
            }
        };
        // `[for (var i = 0; i < rnd.nextInt(3); i++) ...]`: the bound is
        // re-drawn before every iteration.
        let some_sends = |rnd: &mut DartRandom| -> Vec<TrackSend> {
            let mut out = Vec::new();
            let mut i = 0;
            while i < rnd.next_int(3) {
                out.push(send(&ids[rnd.next_int(bus_count)]));
                i += 1;
            }
            out
        };

        let mut buses = Vec::new();
        for id in &ids {
            let soloed = rnd.next_int(5) == 0;
            // Deliberately unconstrained: self-routes and cycles included.
            let output = maybe_bus(&mut rnd);
            let sends = some_sends(&mut rnd);
            buses.push(solo_bus(id, soloed, output.as_deref(), sends));
        }
        let tracks = dart_list(&mut rnd, 4, |rnd, i| {
            let soloed = rnd.next_int(4) == 0;
            let bus_id = maybe_bus(rnd);
            let sends = some_sends(rnd);
            solo_track(&format!("t{i}"), soloed, bus_id.as_deref(), sends)
        });

        let by_id: HashMap<String, FxBus> =
            buses.iter().map(|b| (b.id.clone(), b.clone())).collect();
        let r = resolve_solo(&tracks, &buses);
        let any_solo = tracks.iter().any(|t| t.soloed) || buses.iter().any(|b| b.soloed);
        if !any_solo {
            continue; // the all-audible short circuit
        }

        for t in &tracks {
            if t.soloed {
                assert!(
                    r.track_in_solo(&t.id),
                    "seed {seed}: a soloed take must be audible"
                );
            }
            if !r.track_in_solo(&t.id) {
                continue;
            }
            // The promise that makes solo useful: an audible take is actually
            // HEARD, so every bus carrying it to the master stays audible too.
            for bus_id in route_of(t.bus_id.as_deref(), &t.sends, &by_id) {
                assert!(
                    r.bus_in_solo(&bus_id),
                    "seed {seed}: take {} is audible but its route through {bus_id} is muted, so nothing comes out",
                    t.id
                );
            }
        }
        for b in &buses {
            if b.soloed {
                assert!(
                    r.bus_in_solo(&b.id),
                    "seed {seed}: a soloed bus must be audible"
                );
            }
        }
    }
}
