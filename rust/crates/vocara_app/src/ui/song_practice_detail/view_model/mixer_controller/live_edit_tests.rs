//! The controller half of `test/playback_live_edits_test.dart`: the same
//! scenarios driven through the mixer, as the screen drives them, over the
//! graph engine on fake bindings. (The service half, which calls the
//! engine's setters directly, is ported with the service.)
//!
//!  * A live edit the mixer makes while its resync waits (on the chain, or on
//!    a decode) survives that sync's push of its older snapshot. This is
//!    what [`MixerController::resync_playback`] queuing the sync in the call
//!    itself guarantees.
//!  * A lane's chain-level settings (pan, FX, sends), which the mixer pushes
//!    to every clip of the lane, reach the engine once per lane.

use std::collections::{BTreeMap, HashSet};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::engine::media::MediaError;
use vocara_services::engine::{EngineBindings, EngineError, Pcm};
use vocara_services::graph_audio_playback_service::{GraphAudioPlaybackService, StagingDecode};

use super::*;

/// Fake engine bindings: keeps each graph's last (gain, muted) per track
/// index — a value set while a graph is staged lands on it, and it replaces
/// the live one at publish — and counts the chain-level setter calls (pan,
/// sends, insert FX) by track index. Everything else the pipeline calls is
/// accepted.
#[derive(Default)]
struct Fake {
    s: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    cached_paths: HashSet<String>,
    staging: Vec<String>,
    live: BTreeMap<i32, (f32, bool)>,
    staged: BTreeMap<i32, (f32, bool)>,
    is_staged: bool,
    chain_calls: Vec<i32>,
}

impl Fake {
    fn s(&self) -> MutexGuard<'_, FakeState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn chain_call(&self, track: i32) -> Result<(), EngineError> {
        self.s().chain_calls.push(track);
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
impl EngineBindings for Fake {
    fn destroy(&self) {}

    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn commit_degraded_mask(&self) -> u64 {
        0
    }

    fn frame_pos(&self) -> i64 {
        0
    }

    fn tracks_begin(&self) -> Result<(), EngineError> {
        self.s().staging.clear();
        Ok(())
    }

    fn tracks_add(
        &self,
        path: &str,
        _start_ms: i64,
        _gain: f32,
        _muted: bool,
        _is_vocal: bool,
        _bus: i32,
    ) -> Result<i32, EngineError> {
        let mut s = self.s();
        if !s.cached_paths.contains(path) {
            return Err(EngineError::Refused {
                call: "ve_tracks_add",
                code: -1,
            });
        }
        s.staging.push(path.to_string());
        Ok(s.staging.len() as i32 - 1)
    }

    fn tracks_add_pcm(
        &self,
        path: &str,
        _pcm: &[f32],
        _start_ms: i64,
        _gain: f32,
        _muted: bool,
        _is_vocal: bool,
        _bus: i32,
    ) -> Result<i32, EngineError> {
        let mut s = self.s();
        s.staging.push(path.to_string());
        Ok(s.staging.len() as i32 - 1)
    }

    fn tracks_commit_staged(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        s.is_staged = true;
        s.staged = BTreeMap::new();
        Ok(())
    }

    fn tracks_publish(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        if s.is_staged {
            s.live = std::mem::take(&mut s.staged);
            s.is_staged = false;
        }
        Ok(())
    }

    fn set_track_volume(&self, index: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        let mut s = self.s();
        if s.is_staged {
            s.staged.insert(index, (gain, muted));
        } else {
            s.live.insert(index, (gain, muted));
        }
        Ok(())
    }

    fn buses_begin(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn buses_add(&self, _gain: f32, _muted: bool, _output: i32) -> Result<i32, EngineError> {
        Ok(0)
    }

    fn track_chain(&self, _track: i32, _tags: &[u8]) -> Result<(), EngineError> {
        Ok(())
    }

    fn track_lane(&self, _track: i32, _leader: i32) -> Result<(), EngineError> {
        Ok(())
    }

    fn bus_chain(&self, _bus: i32, _tags: &[u8]) -> Result<(), EngineError> {
        Ok(())
    }

    fn track_sends(&self, _track: i32, _sends: &[(i32, f32)]) -> Result<(), EngineError> {
        Ok(())
    }

    fn bus_sends(&self, _bus: i32, _sends: &[(i32, f32)]) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_master(&self, _gain: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_reverb(
        &self,
        _room: f32,
        _damping: f32,
        _wet: f32,
        _dry: f32,
        _width: f32,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    // Every other track setter (pan, sends, each insert's parameters)
    // addresses the chain: count it by its track index.

    fn set_pan(&self, index: i32, _pan: f32) -> Result<(), EngineError> {
        self.chain_call(index)
    }

    fn set_send_level(&self, track: i32, _slot: i32, _level_db: f32) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_send(&self, track: i32, _send_db: f32) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_eq_band(
        &self,
        track: i32,
        _band: i32,
        _freq: f32,
        _q: f32,
        _gain_db: f32,
        _enabled: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_comp(
        &self,
        track: i32,
        _threshold_db: f32,
        _ratio: f32,
        _knee_db: f32,
        _attack_ms: f32,
        _release_ms: f32,
        _makeup_db: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_deesser(
        &self,
        track: i32,
        _freq: f32,
        _threshold_db: f32,
        _range_db: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_gate(
        &self,
        track: i32,
        _threshold_db: f32,
        _range_db: f32,
        _attack_ms: f32,
        _release_ms: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_delay(
        &self,
        track: i32,
        _time_ms: f32,
        _feedback: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_saturation(
        &self,
        track: i32,
        _drive_db: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_doubler(
        &self,
        track: i32,
        _delay_ms: f32,
        _depth_ms: f32,
        _rate_hz: f32,
        _mix: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_limiter(
        &self,
        track: i32,
        _ceiling_db: f32,
        _release_ms: f32,
        _bypass: bool,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }

    fn set_reverb_insert(
        &self,
        track: i32,
        _room: f32,
        _damping: f32,
        _wet: f32,
        _width: f32,
    ) -> Result<(), EngineError> {
        self.chain_call(track)
    }
}

/// A decoded buffer for the fake to stage.
fn pcm() -> Pcm {
    Pcm::interleaved_stereo(vec![0.0; 20], 48_000)
}

/// A staging decode that succeeds at once.
fn instant() -> Option<StagingDecode> {
    Some(Arc::new(|_: &str, _: u32| Ok(Some(pcm()))))
}

type DecodeOutcome = Result<Option<Pcm>, MediaError>;

/// A decode in flight: the test completes it.
struct PendingDecode {
    reply: mpsc::SyncSender<DecodeOutcome>,
}

impl PendingDecode {
    fn complete(self) {
        let _ = self.reply.send(Ok(Some(pcm())));
    }
}

/// The test's end of a gated decode: each decode, as it starts, waiting for
/// the test to complete it (Dart's decode that returned a completer's
/// future).
struct DecodeGate {
    calls: mpsc::Receiver<PendingDecode>,
}

impl DecodeGate {
    /// The next decode to start (Dart's `pumpEventQueue` until it had).
    fn next(&self) -> PendingDecode {
        self.calls
            .recv_timeout(Duration::from_secs(10))
            .expect("a decode starts")
    }
}

fn gated() -> (Option<StagingDecode>, DecodeGate) {
    let (tx, rx) = mpsc::channel::<PendingDecode>();
    let tx = Mutex::new(tx);
    let decode: StagingDecode = Arc::new(move |_path: &str, _: u32| {
        let (reply, answer) = mpsc::sync_channel(1);
        let sent = tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(PendingDecode { reply });
        if sent.is_err() {
            return Ok(None);
        }
        answer.recv().unwrap_or(Ok(None))
    });
    (Some(decode), DecodeGate { calls: rx })
}

/// The view model's side: the practice model and no take in flight.
struct State {
    practice: SongPractice,
}

impl MixerHost for State {
    fn practice(&self) -> Option<&SongPractice> {
        Some(&self.practice)
    }

    fn set_practice(&mut self, practice: SongPractice) {
        self.practice = practice;
    }

    fn toast(&mut self, _message: &str) {}

    fn select_bus_after_mutation(&mut self, _bus_id: &str) {}

    fn clear_bus_selection_if(&mut self, _bus_id: &str) {}

    fn is_recording(&self) -> bool {
        false
    }

    fn recording_target_id(&self) -> Option<&str> {
        None
    }

    fn recording_peaks(&self) -> &[f64] {
        &[]
    }

    fn recording_start_ms(&self) -> i64 {
        0
    }

    fn recording_duration_ms(&self) -> i64 {
        0
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }
}

fn at() -> DateTime {
    DateTime::local_ymd(2026, 1, 1)
}

fn lane(id: &str, kind: AudioTrackKind, path: &str) -> Arc<AudioTrack> {
    Arc::new(AudioTrack::new(id, kind, id, at()).with_audio(path, 0, 10_000, Vec::new()))
}

/// The mixer over the graph engine on `fake`, with the practice `practice`.
fn mixer_over(
    fake: &Arc<Fake>,
    decode: Option<StagingDecode>,
    practice: SongPractice,
) -> (MixerController, State, Arc<GraphAudioPlaybackService>) {
    let svc = Arc::new(GraphAudioPlaybackService::with_bindings(
        Arc::clone(fake) as Arc<dyn EngineBindings>,
        decode,
    ));
    let mixer = MixerController::new(EngineHandle::Graph(Arc::clone(&svc)));
    (mixer, State { practice }, svc)
}

fn song() -> SongPractice {
    SongPractice::new("p1", "Song", at(), at())
}

#[test]
fn an_ab_backing_flip_during_the_page_open_decode_stays() {
    // The mixer's A/B flip is live volume calls only; no save and no resync
    // follow it, so a lost flip stayed wrong for the session.
    let fake = Arc::new(Fake::default());
    let (decode, gate) = gated();
    let (mut mixer, s, _svc) = mixer_over(
        &fake,
        decode,
        SongPractice {
            accompaniment: Some(lane("acc", AudioTrackKind::Accompaniment, "acc.mp3")),
            reference: Some(lane("ref", AudioTrackKind::Reference, "ref.mp3")),
            ..song()
        },
    );
    // The page-open sync: the accompaniment heard, the reference not.
    let load = mixer.resync_playback(&s);
    assert!(!load.is_done(), "the graph engine queues its sync");
    let first = gate.next();
    mixer.set_play_reference(&s, true);
    first.complete();
    gate.next().complete();
    load.wait().unwrap();
    assert_eq!(
        fake.s().live,
        BTreeMap::from([(0, (1.0, true)), (1, (1.0, false))])
    );
}

#[test]
fn a_fader_moved_during_a_rebuild_keeps_its_value() {
    let fake = Arc::new(Fake::default());
    fake.s().cached_paths.insert("take.wav".into());
    let (decode, gate) = gated();
    let (mut mixer, mut s, _svc) = mixer_over(
        &fake,
        decode,
        SongPractice {
            vocal_tracks: vec![lane("t", AudioTrackKind::Vocal, "take.wav")],
            ..song()
        },
    );
    mixer.resync_playback(&s).wait().unwrap();

    // A take lands: its save's resync rebuilds, decoding the new file.
    s.practice
        .vocal_tracks
        .push(lane("b", AudioTrackKind::Vocal, "ref.m4a"));
    let rebuild = mixer.resync_playback(&s);
    let decode = gate.next();
    mixer.set_track_volume_live(&mut s, "t", 0.2);
    // Heard at once on the old graph.
    assert_eq!(fake.s().live[&0], (0.2, false));
    decode.complete();
    rebuild.wait().unwrap();
    assert_eq!(fake.s().live[&0], (0.2, false));
}

#[test]
fn an_edit_queued_behind_a_rebuild_survives_the_fast_path_too() {
    let fake = Arc::new(Fake::default());
    fake.s().cached_paths.insert("take.wav".into());
    let (decode, gate) = gated();
    let (mut mixer, mut s, _svc) = mixer_over(
        &fake,
        decode,
        SongPractice {
            vocal_tracks: vec![
                lane("t", AudioTrackKind::Vocal, "take.wav"),
                lane("b", AudioTrackKind::Vocal, "ref.m4a"),
            ],
            ..song()
        },
    );
    let rebuild = mixer.resync_playback(&s);
    // Same topology: this one takes the fast path once the rebuild lands.
    let fast = mixer.resync_playback(&s);
    let decode = gate.next();
    mixer.set_track_volume_live(&mut s, "t", 0.5);
    let _save = mixer.toggle_mute(&mut s, "t");
    decode.complete();
    rebuild.wait().unwrap();
    fast.wait().unwrap();
    assert_eq!(fake.s().live[&0], (0.5, true));
}

#[test]
fn a_newer_sync_wins_over_an_edit_its_model_already_carries() {
    // A failed save reverts the model and resyncs: that snapshot is newer
    // than the edit it reverts, so the edit must not be replayed over it.
    let fake = Arc::new(Fake::default());
    fake.s().cached_paths.insert("take.wav".into());
    let (mut mixer, mut s, _svc) = mixer_over(
        &fake,
        None,
        SongPractice {
            vocal_tracks: vec![lane("t", AudioTrackKind::Vocal, "take.wav")],
            ..song()
        },
    );
    mixer.resync_playback(&s).wait().unwrap();
    let disk = s.practice.clone();
    mixer.set_track_volume_live(&mut s, "t", 0.2);
    s.practice = disk; // the revert
    mixer.resync_playback(&s).wait().unwrap();
    assert_eq!(fake.s().live[&0], (1.0, false));
}

/// A take of `n` clips, one a second.
fn many_clip_take(n: usize) -> SongPractice {
    let clips = (0..n)
        .map(|i| AudioClip {
            start_ms: i as i64 * 1000,
            ..AudioClip::new(format!("c{i}"), format!("c{i}.wav"), 1000)
        })
        .collect();
    SongPractice {
        vocal_tracks: vec![Arc::new(AudioTrack {
            clips,
            fx: Some(TrackFx::flat()),
            ..AudioTrack::new("take", AudioTrackKind::Vocal, "take", at())
        })],
        ..song()
    }
}

#[test]
fn a_lane_whose_first_clip_left_the_model_still_hears_its_fx() {
    // The first clip of a take deleted, its save still in flight: the mixer
    // sends the lane's FX and pan to the remaining clips, which the playing
    // graph still runs through the deleted clip's chain. Skipping followers
    // dropped every one of them until the resync committed.
    let fake = Arc::new(Fake::default());
    let (mut mixer, mut s, _svc) = mixer_over(&fake, instant(), many_clip_take(3));
    mixer.resync_playback(&s).wait().unwrap();
    let per_sync = fake.s().chain_calls.len();
    fake.s().chain_calls.clear();
    s.update_track("take", &|t| AudioTrack {
        clips: t.clips[1..].to_vec(),
        ..t.clone()
    });
    mixer.set_track_fx_live(
        &mut s,
        "take",
        TrackFx {
            eq_on: false,
            ..TrackFx::flat()
        },
    );
    mixer.set_track_pan_live(&mut s, "take", 0.3);
    // Once, not per clip.
    assert_eq!(fake.s().chain_calls.len(), per_sync);
}

#[test]
fn an_fx_drag_on_a_many_clip_lane_sets_the_lane_chain_once() {
    let fake = Arc::new(Fake::default());
    let (mut mixer, mut s, _svc) = mixer_over(&fake, instant(), many_clip_take(20));
    mixer.resync_playback(&s).wait().unwrap();
    let per_sync = fake.s().chain_calls.len();
    assert_eq!(
        fake.s().chain_calls.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([0]),
        "only the leader is set"
    );

    fake.s().chain_calls.clear();
    // The mixer pushes the lane's value to each of its 20 clips.
    for tick in 0..2 {
        let fx = TrackFx {
            eq_on: tick % 2 == 0,
            ..TrackFx::flat()
        };
        mixer.set_track_fx_live(&mut s, "take", fx);
        mixer.set_track_pan_live(&mut s, "take", 0.3 + f64::from(tick) * 0.1);
    }
    assert_eq!(fake.s().chain_calls.len(), 2 * per_sync);
}
