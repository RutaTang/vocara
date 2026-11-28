//! The graph engine's sync pipeline and live edits, driven over fake engine
//! bindings: the ports of `test/graph_audio_playback_service_test.dart` and
//! `test/playback_live_edits_test.dart`, plus the Rust-side threading (the
//! position poll, the transport events, the unavailable engine).
//!
//! Contracts pinned here:
//!  * A track whose file decode is REFUSED (gone, unreadable, over-cap) is
//!    left out of this round's graph; the commit still runs for the rest, and
//!    the skipped track restages on the next topology change. Programming
//!    errors are not absorbed by that skip.
//!  * The degraded-commit mask is a full u64 (bit 63 is the overflow fold).
//!  * The one rebuild a degraded commit queues reflects the model as it
//!    stands when the retry RUNS: a sync enqueued during the degraded job wins
//!    over the degraded job's own captured topology.
//!  * A live edit made while a sync job waits (on the chain, or on a decode)
//!    survives that job's push of its older snapshot.
//!  * A lane's chain-level settings (pan, FX, sends) reach the engine once per
//!    lane, not once per clip: every clip of a lane plays through one chain.
//!
//! Where a Dart test queued syncs back to back before any ran (two `sync()`
//! calls in one synchronous stretch), the port holds the chain with a blocking
//! job first; where it awaited `pumpEventQueue()` until a decode was in
//! flight, the port's gated decode hands the test each decode as it starts.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::mpsc::error::TryRecvError;
use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};

use super::*;
use crate::engine::EngineError;

/// Fake engine bindings: records what each `tracks_commit_staged` committed
/// (the staged file paths, in staging order), feeds scripted degraded masks
/// and commit results, and keeps each graph's last (gain, muted) per track
/// index: a value set while a graph is staged lands on it, and it replaces
/// the live one at publish. Anything else the pipeline calls panics (the
/// trait's default), so an unexpected engine call fails the test instead of
/// hiding.
#[derive(Default)]
struct Fake {
    s: Mutex<FakeState>,
    /// The playhead `frame_pos` reports.
    frame: AtomicI64,
}

#[derive(Default)]
struct FakeState {
    /// Paths `tracks_add` treats as PCM-cache hits (default: all miss).
    cached_paths: HashSet<String>,
    /// The staged paths of every successful commit, in commit order.
    commits: Vec<Vec<String>>,
    /// Scripted `commit_degraded_mask` results, consumed front-first (empty =
    /// 0, the healthy answer).
    degraded_masks: VecDeque<u64>,
    /// Scripted `tracks_commit_staged` results, consumed front-first (empty =
    /// 0, accepted). A non-zero result models the engine REJECTING the
    /// topology and keeping its old graph; only accepted commits land in
    /// `commits`.
    commit_results: VecDeque<i32>,
    /// Commit, publish and live-value calls in call order.
    events: Vec<String>,
    staging: Vec<String>,
    /// Every `set_input_channel` mode, in call order.
    input_channels: Vec<u32>,
    /// Every `track_lane` call as (track, leader), in call order.
    lanes: Vec<(i32, i32)>,
    /// Every `track_key` call as (track, key), in call order.
    keys: Vec<(i32, u64)>,
    /// Every `track_span` call as (track, offset, length, fades), in call order.
    spans: Vec<(i32, i64, i64, u32)>,
    /// Makes `commit_degraded_mask` panic (something failing after a commit).
    panic_on_degraded_mask: bool,
    live: BTreeMap<i32, (f32, bool)>,
    staged: BTreeMap<i32, (f32, bool)>,
    is_staged: bool,
    /// The track index of every chain-level setter call (pan, sends, insert
    /// FX), in call order.
    chain_calls: Vec<i32>,
    /// Decoded buffers handed to `tracks_add_pcm`.
    pcm_adds: usize,
    transport: Vec<String>,
}

impl Fake {
    fn new() -> Arc<Fake> {
        Arc::new(Fake::default())
    }

    fn s(&self) -> MutexGuard<'_, FakeState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cache(&self, paths: &[&str]) {
        self.s()
            .cached_paths
            .extend(paths.iter().map(|p| p.to_string()));
    }

    fn chain_call(&self, track: i32) -> Result<(), EngineError> {
        self.s().chain_calls.push(track);
        Ok(())
    }
}

fn refused(call: &'static str, code: i32) -> EngineError {
    EngineError::Refused { call, code }
}

impl EngineBindings for Fake {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    fn set_input_channel(&self, mode: u32) -> Result<(), EngineError> {
        self.s().input_channels.push(mode);
        Ok(())
    }

    fn buses_begin(&self) -> Result<(), EngineError> {
        Ok(())
    }

    fn buses_add(&self, _gain: f32, _muted: bool, _output: i32) -> Result<i32, EngineError> {
        Ok(0)
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
            return Err(refused("ve_tracks_add", -1));
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
        s.pcm_adds += 1;
        s.staging.push(path.to_string());
        Ok(s.staging.len() as i32 - 1)
    }

    fn tracks_commit_staged(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        let result = s.commit_results.pop_front().unwrap_or(0);
        if result == 0 {
            let staged = s.staging.clone();
            s.commits.push(staged);
            s.is_staged = true;
            s.staged = BTreeMap::new();
        }
        s.events.push("commit".into());
        if result == 0 {
            Ok(())
        } else {
            Err(refused("ve_tracks_commit_staged", result))
        }
    }

    fn tracks_publish(&self) -> Result<(), EngineError> {
        let mut s = self.s();
        s.events.push("publish".into());
        if s.is_staged {
            s.live = std::mem::take(&mut s.staged);
            s.is_staged = false;
        }
        Ok(())
    }

    fn track_lane(&self, track: i32, leader: i32) -> Result<(), EngineError> {
        self.s().lanes.push((track, leader));
        Ok(())
    }

    fn track_key(&self, track: i32, key: u64) -> Result<(), EngineError> {
        self.s().keys.push((track, key));
        Ok(())
    }

    fn track_span(
        &self,
        track: i32,
        src_offset_ms: i64,
        len_ms: i64,
        fades: u32,
    ) -> Result<(), EngineError> {
        self.s().spans.push((track, src_offset_ms, len_ms, fades));
        Ok(())
    }

    fn track_chain(&self, _track: i32, _tags: &[u8]) -> Result<(), EngineError> {
        Ok(())
    }

    fn commit_degraded_mask(&self) -> u64 {
        let panic = self.s().panic_on_degraded_mask;
        if panic {
            panic!("the read after the commit");
        }
        self.s().degraded_masks.pop_front().unwrap_or(0)
    }

    fn set_track_volume(&self, index: i32, gain: f32, muted: bool) -> Result<(), EngineError> {
        let mut s = self.s();
        s.events.push("volume".into());
        if s.is_staged {
            s.staged.insert(index, (gain, muted));
        } else {
            s.live.insert(index, (gain, muted));
        }
        Ok(())
    }

    fn set_bus_volume(&self, _bus: i32, _gain: f32, _muted: bool) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_bus_pan(&self, _bus: i32, _pan: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn set_master(&self, _gain: f32) -> Result<(), EngineError> {
        self.s().events.push("master".into());
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

    fn set_send(&self, track: i32, _send_db: f32) -> Result<(), EngineError> {
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

    // The transport, for the poll and event tests.

    fn play(&self) -> Result<(), EngineError> {
        self.s().transport.push("play".into());
        Ok(())
    }

    fn pause(&self) -> Result<(), EngineError> {
        self.s().transport.push("pause".into());
        Ok(())
    }

    fn seek(&self, frame: i64) -> Result<(), EngineError> {
        self.s().transport.push(format!("seek {frame}"));
        self.frame.store(frame, Ordering::SeqCst);
        Ok(())
    }

    fn set_loop(&self, enabled: bool, start: i64, end: i64) -> Result<(), EngineError> {
        self.s()
            .transport
            .push(format!("loop {enabled} {start} {end}"));
        Ok(())
    }

    fn frame_pos(&self) -> i64 {
        self.frame.load(Ordering::SeqCst)
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

/// A staging decode that answers `f(path)` at once.
fn decode_with(
    f: impl Fn(&str) -> Result<Option<Pcm>, MediaError> + Send + Sync + 'static,
) -> Option<StagingDecode> {
    Some(Arc::new(move |path: &str, _: u32| f(path)))
}

type DecodeOutcome = Result<Option<Pcm>, MediaError>;

/// A decode in flight: the test completes it.
struct PendingDecode {
    path: String,
    reply: mpsc::SyncSender<DecodeOutcome>,
}

impl PendingDecode {
    fn complete(self, outcome: DecodeOutcome) {
        let _ = self.reply.send(outcome);
    }
}

/// The test's end of a gated decode: each decode, as it starts, waiting for
/// the test to complete it (Dart's decode that returned a `Completer`'s
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

    /// No decode started (or is starting).
    fn assert_idle(&self) {
        assert!(self.calls.recv_timeout(Duration::from_millis(50)).is_err());
    }
}

fn gated() -> (Option<StagingDecode>, DecodeGate) {
    let (tx, rx) = mpsc::channel::<PendingDecode>();
    let tx = Mutex::new(tx);
    let decode: StagingDecode = Arc::new(move |path: &str, _: u32| {
        let (reply, answer) = mpsc::sync_channel(1);
        let sent = tx.lock().unwrap().send(PendingDecode {
            path: path.to_string(),
            reply,
        });
        if sent.is_err() {
            return Ok(None);
        }
        answer.recv().unwrap_or(Ok(None))
    });
    (Some(decode), DecodeGate { calls: rx })
}

fn service(fake: &Arc<Fake>, decode: Option<StagingDecode>) -> GraphAudioPlaybackService {
    GraphAudioPlaybackService::with_bindings(Arc::clone(fake) as Arc<dyn EngineBindings>, decode)
}

/// Holds the sync chain until released, so the syncs queued meanwhile are
/// queued back to back before any runs (as Dart's synchronous `sync()` calls
/// were).
struct ChainHold {
    release: mpsc::SyncSender<()>,
    ticket: SyncTicket<()>,
}

impl ChainHold {
    fn new(svc: &GraphAudioPlaybackService) -> ChainHold {
        let (release, wait) = mpsc::sync_channel::<()>(1);
        let (started_tx, started) = mpsc::sync_channel::<()>(1);
        let ticket = svc.shared.chain.add(move || {
            let _ = started_tx.send(());
            let _ = wait.recv();
        });
        started.recv().expect("the hold runs");
        ChainHold { release, ticket }
    }

    fn release(self) {
        let _ = self.release.send(());
        self.ticket.wait().unwrap();
    }
}

fn track(id: &str, path: &str) -> TrackPlayback {
    TrackPlayback::new(id, path)
}

fn at(id: &str, path: &str, start_ms: i64) -> TrackPlayback {
    TrackPlayback {
        start_ms,
        ..TrackPlayback::new(id, path)
    }
}

fn in_lane(id: &str, path: &str, start_ms: i64, lane: &str) -> TrackPlayback {
    TrackPlayback {
        start_ms,
        lane_id: Some(lane.to_string()),
        ..TrackPlayback::new(id, path)
    }
}

fn paths(commit: &[&str]) -> Vec<String> {
    commit.iter().map(|p| p.to_string()).collect()
}

// --- graph_audio_playback_service_test.dart ---------------------------------------

#[test]
fn bit_count_counts_ordinary_masks() {
    assert_eq!(GraphAudioPlaybackService::bit_count(0), 0);
    assert_eq!(GraphAudioPlaybackService::bit_count(1), 1);
    assert_eq!(GraphAudioPlaybackService::bit_count(0x2d), 4);
}

#[test]
fn bit_count_terminates_on_a_mask_with_bit_63_the_degraded_overflow_fold() {
    // The engine folds staged indices >= 63 into bit 63 of the u64 mask,
    // which Dart received as a NEGATIVE int: an arithmetic shift looped
    // forever on it.
    assert_eq!(GraphAudioPlaybackService::bit_count(1 << 63), 1);
    assert_eq!(GraphAudioPlaybackService::bit_count((1 << 63) | 0x3), 3);
    assert_eq!(GraphAudioPlaybackService::bit_count(u64::MAX), 64);
}

#[test]
fn the_input_channel_choice_reaches_the_engine_as_its_mode() {
    let fake = Fake::new();
    let svc = service(&fake, None);
    svc.set_input_channel(GraphAudioPlaybackService::INPUT_CHANNEL_2);
    svc.set_input_channel(GraphAudioPlaybackService::INPUT_CHANNEL_1);
    svc.set_input_channel(GraphAudioPlaybackService::INPUT_CHANNEL_AUTO);
    assert_eq!(fake.s().input_channels, [2, 1, 0]);
}

#[test]
fn a_rebuild_sets_its_live_values_on_the_staged_graph_before_it_goes_live() {
    // A rebuilt graph starts every node at its build defaults. Published at
    // the commit, the engine rendered those defaults (and every smoother
    // ramped away from them) for the blocks the re-push took to land; staged,
    // the graph goes live only once each value is already set.
    let fake = Fake::new();
    fake.cache(&["a.wav"]);
    let svc = service(&fake, instant());

    svc.sync_checked(vec![track("a", "a.wav")], vec![]).unwrap();
    assert_eq!(fake.s().events, ["commit", "volume", "master", "publish"]);

    // A rejected topology stages nothing: the publish that follows it anyway
    // (see the next test) is a no-op, and no live value is pushed.
    fake.s().events.clear();
    fake.s().commit_results.push_back(-1);
    svc.sync_checked(vec![at("a", "a.wav", 10)], vec![])
        .unwrap();
    assert_eq!(fake.s().events, ["commit", "publish"]);
}

#[test]
fn a_commit_that_fails_after_staging_still_goes_live() {
    // Staged but never published, the old graph kept playing while every
    // setter addressed the staged one: mute, faders and FX went dead until
    // the next topology change.
    let fake = Fake::new();
    fake.cache(&["a.wav"]);
    fake.s().panic_on_degraded_mask = true;
    let svc = service(&fake, instant());

    let outcome = svc.sync_checked(vec![track("a", "a.wav")], vec![]);
    assert_eq!(
        outcome,
        Err(SyncError::Panicked("the read after the commit".into()))
    );
    assert_eq!(fake.s().events, ["commit", "publish"]);
}

#[test]
fn a_lanes_later_clips_play_through_its_first_clips_chain() {
    // A chain per clip ran a compressor per clip, each seeing only its own
    // clip: every clip boundary restarted the envelope cold.
    let fake = Fake::new();
    let svc = service(&fake, instant());

    svc.sync_checked(
        vec![
            in_lane("c1", "a.wav", 0, "take"),
            in_lane("b1", "b.wav", 0, "backing"),
            in_lane("c2", "c.wav", 5000, "take"),
            in_lane("c3", "d.wav", 9000, "take"),
        ],
        vec![],
    )
    .unwrap();
    assert_eq!(fake.s().lanes, [(2, 0), (3, 0)]);
}

#[test]
fn a_clip_cut_by_one_above_it_stages_just_the_part_heard() {
    // A take 0-60 s with a phrase re-sung over 20-30 s: the take plays as its
    // two stretches either side, the second from 30 s into its file, each
    // fading where the phrase cuts in or lets go.
    let fake = Fake::new();
    let svc = service(&fake, instant());
    let before = TrackPlayback {
        duration_ms: 20_000,
        fade_out: true,
        ..in_lane("take", "take.wav", 0, "lane")
    };
    let phrase = TrackPlayback {
        duration_ms: 10_000,
        ..in_lane("phrase", "phrase.wav", 20_000, "lane")
    };
    let after = TrackPlayback {
        duration_ms: 30_000,
        src_offset_ms: 30_000,
        fade_in: true,
        ..in_lane("take#1", "take.wav", 30_000, "lane")
    };
    svc.sync_checked(vec![before.clone(), phrase.clone(), after.clone()], vec![])
        .unwrap();
    assert_eq!(fake.s().spans, [(0, 0, 20_000, 2), (2, 30_000, -1, 1)]);

    // The phrase moved later: the take's stretches move with it.
    let moved = vec![
        TrackPlayback {
            duration_ms: 25_000,
            ..before
        },
        TrackPlayback {
            start_ms: 25_000,
            ..phrase
        },
        TrackPlayback {
            start_ms: 35_000,
            duration_ms: 25_000,
            src_offset_ms: 35_000,
            ..after
        },
    ];
    svc.sync_checked(moved, vec![]).unwrap();
    assert_eq!(fake.s().commits.len(), 2);
    assert_eq!(fake.s().spans[2..], [(0, 0, 25_000, 2), (2, 35_000, -1, 1)]);
}

#[test]
fn each_staged_clip_names_its_lane_by_a_key_that_survives_a_renumbering() {
    // The engine keeps a lane's chain (and the state of its effects) across a
    // rebuild by this key; the staged index moves whenever an earlier lane
    // gains or loses a clip.
    let fake = Fake::new();
    let svc = service(&fake, instant());
    let a1 = in_lane("a1", "a1.wav", 0, "lane-a");
    let b1 = in_lane("b1", "b1.wav", 0, "lane-b");
    svc.sync_checked(vec![a1.clone(), b1.clone()], vec![])
        .unwrap();
    let first = std::mem::take(&mut fake.s().keys);
    svc.sync_checked(
        vec![a1, in_lane("a2", "a2.wav", 9000, "lane-a"), b1],
        vec![],
    )
    .unwrap();
    let second = fake.s().keys.clone();
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 3);
    assert_ne!(first[0].1, first[1].1, "two lanes, two keys");
    assert_eq!((second[0].0, second[0].1), (0, first[0].1));
    assert_eq!(second[1].1, first[0].1, "a lane's clips share its key");
    assert_eq!(
        (second[2].0, second[2].1),
        (2, first[1].1),
        "lane B, moved from 1 to 2"
    );
}

#[test]
fn a_lane_whose_first_clip_cannot_stage_leads_with_the_next_one() {
    let fake = Fake::new();
    // a.wav decodes to nothing: that clip is left out of the graph.
    let svc = service(&fake, decode_with(|path| Ok((path != "a.wav").then(pcm))));

    svc.sync_checked(
        vec![
            in_lane("c1", "a.wav", 0, "take"),
            in_lane("c2", "b.wav", 5000, "take"),
            in_lane("c3", "c.wav", 9000, "take"),
        ],
        vec![],
    )
    .unwrap();
    assert_eq!(fake.s().commits, [paths(&["b.wav", "c.wav"])]);
    assert_eq!(fake.s().lanes, [(1, 0)]);
}

#[test]
fn moving_a_clip_to_another_lane_rebuilds_the_graph() {
    let fake = Fake::new();
    let svc = service(&fake, instant());

    let a = in_lane("c1", "a.wav", 0, "take1");
    svc.sync_checked(vec![a.clone(), in_lane("c2", "b.wav", 0, "take1")], vec![])
        .unwrap();
    svc.sync_checked(vec![a, in_lane("c2", "b.wav", 0, "take2")], vec![])
        .unwrap();
    assert_eq!(fake.s().commits.len(), 2);
    assert_eq!(fake.s().lanes, [(1, 0)]);
}

#[test]
fn a_refused_decode_skips_just_that_track_and_it_restages_on_the_next_topology_change() {
    let fake = Fake::new();
    let gone_readable = Arc::new(AtomicBool::new(false));
    let readable = Arc::clone(&gone_readable);
    let svc = service(
        &fake,
        decode_with(move |path| {
            if path == "gone.wav" && !readable.load(Ordering::SeqCst) {
                return Err(MediaError::Unsupported(
                    "The file could not be read.".into(),
                ));
            }
            if path == "long.wav" {
                return Err(MediaError::TooLong {
                    max_duration: Duration::from_secs(60 * 60),
                });
            }
            Ok(Some(pcm()))
        }),
    );

    let gone = track("g", "gone.wav");
    let long = track("l", "long.wav");
    let good = track("ok", "good.wav");

    // Both refused tracks are left out; the healthy one still commits.
    svc.sync_checked(vec![gone.clone(), long.clone(), good.clone()], vec![])
        .unwrap();
    assert_eq!(fake.s().commits, [paths(&["good.wav"])]);

    // The signature was recorded despite the skips: an identical re-sync
    // takes the live-params fast path instead of rebuilding (which would
    // re-run the dead file's decode on every fader move).
    svc.sync_checked(vec![gone.clone(), long.clone(), good], vec![])
        .unwrap();
    assert_eq!(fake.s().commits.len(), 1);

    // The file becomes readable again: the next TOPOLOGY change (a moved
    // clip) restages the previously skipped track, as promised.
    gone_readable.store(true, Ordering::SeqCst);
    let moved = at("ok", "good.wav", 500);
    svc.sync_checked(vec![gone, long, moved], vec![]).unwrap();
    assert_eq!(fake.s().commits.len(), 2);
    assert_eq!(
        fake.s().commits.last().unwrap(),
        &paths(&["gone.wav", "good.wav"])
    );
}

#[test]
fn a_read_refusal_skips_the_track_too() {
    let fake = Fake::new();
    let svc = service(
        &fake,
        decode_with(|path| {
            if path == "busy.wav" {
                Err(MediaError::Read)
            } else {
                Ok(Some(pcm()))
            }
        }),
    );
    svc.sync_checked(vec![track("b", "busy.wav"), track("ok", "ok.wav")], vec![])
        .unwrap();
    assert_eq!(fake.s().commits, [paths(&["ok.wav"])]);
}

#[test]
fn a_programming_error_during_staging_still_aborts_the_sync() {
    // No engine to decode with is none of the refusals: it escapes.
    let fake = Fake::new();
    let svc = service(&fake, decode_with(|_| Err(MediaError::EngineUnavailable)));

    let outcome = svc.sync_checked(vec![track("t", "t.wav")], vec![]);
    assert_eq!(
        outcome,
        Err(SyncError::Decode(MediaError::EngineUnavailable))
    );
    assert!(fake.s().commits.is_empty());
}

#[test]
fn a_panic_during_staging_aborts_the_sync_but_not_the_chain() {
    let fake = Fake::new();
    fake.cache(&["ok.wav"]);
    let svc = service(&fake, decode_with(|_| panic!("bug in staging")));

    let outcome = svc.sync_checked(vec![track("t", "t.wav")], vec![]);
    assert_eq!(outcome, Err(SyncError::Panicked("bug in staging".into())));
    assert!(fake.s().commits.is_empty());
    // The next sync still runs (the chain absorbed the failure).
    svc.sync_checked(vec![track("ok", "ok.wav")], vec![])
        .unwrap();
    assert_eq!(fake.s().commits, [paths(&["ok.wav"])]);
}

#[test]
fn closing_the_engine_mid_rebuild_stops_decoding_the_remaining_tracks() {
    // The sync checked availability only on entry, so after the page closed
    // during the first decode every remaining track was still decoded in
    // full (seconds of CPU, up to gigabytes of PCM) for a null engine.
    let fake = Fake::new();
    let (decode, gate) = gated();
    let svc = service(&fake, decode);
    let sync = svc.enqueue_sync(
        vec![
            track("a", "a.mp3"),
            track("b", "b.mp3"),
            track("c", "c.mp3"),
        ],
        vec![],
    );
    let first = gate.next();
    assert_eq!(first.path, "a.mp3");

    PlaybackEngine::dispose(&svc); // the user leaves the page mid-load
    first.complete(Ok(Some(pcm())));
    sync.wait().unwrap();

    gate.assert_idle();
    // The decode in flight is still freed (the buffer drops), and nothing
    // took it.
    assert_eq!(fake.s().pcm_adds, 0);
    assert!(fake.s().commits.is_empty());
}

#[test]
fn the_degraded_retry_rebuilds_from_the_model_current_when_it_runs() {
    let fake = Fake::new();
    fake.cache(&["take.wav", "backing.wav"]);
    // Only the first commit (the degraded job's) reports a degraded track.
    fake.s().degraded_masks.push_back(1);
    let svc = service(&fake, None);
    let take = track("t", "take.wav");
    let backing = track("b", "backing.wav");

    // The degraded job stages take + backing; while it is in flight the user
    // deletes the take, so the newer model lands on the chain BETWEEN the
    // degraded job and the retry it queues.
    let hold = ChainHold::new(&svc);
    let degraded_job = svc.enqueue_sync(vec![take, backing.clone()], vec![]);
    let newer_sync = svc.enqueue_sync(vec![backing.clone()], vec![]);
    hold.release();
    degraded_job.wait().unwrap();
    newer_sync.wait().unwrap();
    // Drain the chain: an identical re-sync serializes after the retry.
    svc.sync_checked(vec![backing], vec![]).unwrap();

    // The retry must never re-commit the stale topology over the newer
    // commit: the deleted take would be audible again while the mixer shows
    // it gone.
    let commits = fake.s().commits.clone();
    assert_eq!(commits[0], paths(&["take.wav", "backing.wav"]));
    for commit in &commits[1..] {
        assert_eq!(commit, &paths(&["backing.wav"]));
    }
}

#[test]
fn a_rejected_commit_in_between_does_not_cost_a_later_degraded_commit_its_retry() {
    // The retry used to be gated on a flag the degraded commit set and only a
    // later SUCCESSFUL commit cleared. A rejected commit (the engine kept its
    // old graph) returns early, so with the queued sync and the retry both
    // rejected the flag stayed set, and the next degraded commit was recorded
    // as committed with its track silent for the session.
    let fake = Fake::new();
    fake.cache(&["take.wav", "backing.wav"]);
    let take = track("t", "take.wav");
    let backing = track("b", "backing.wav");
    let moved = at("t", "take.wav", 500);
    let svc = service(&fake, None);

    // The first accepted commit degrades; the sync queued behind it and the
    // retry it queues are both rejected; the next accepted commit (an
    // unrelated edit) degrades too.
    fake.s().degraded_masks.extend([1, 1]);
    fake.s().commit_results.extend([0, 1, 1]);
    let hold = ChainHold::new(&svc);
    let degraded_job = svc.enqueue_sync(vec![take, backing.clone()], vec![]);
    let rejected = svc.enqueue_sync(vec![backing.clone()], vec![]);
    hold.release();
    degraded_job.wait().unwrap();
    rejected.wait().unwrap();
    // After the retry:
    svc.sync_checked(vec![moved.clone(), backing.clone()], vec![])
        .unwrap();
    // Drain ITS retry:
    svc.sync_checked(vec![moved, backing], vec![]).unwrap();

    assert_eq!(
        fake.s().commits,
        [
            paths(&["take.wav", "backing.wav"]),
            paths(&["take.wav", "backing.wav"]),
            // The second degraded commit's retry.
            paths(&["take.wav", "backing.wav"]),
        ]
    );
}

#[test]
fn with_no_newer_sync_in_between_the_retry_recommits_the_degraded_model_once() {
    let fake = Fake::new();
    fake.cache(&["take.wav", "backing.wav"]);
    fake.s().degraded_masks.push_back(1);
    let svc = service(&fake, None);
    let model = vec![track("t", "take.wav"), track("b", "backing.wav")];

    svc.sync_checked(model.clone(), vec![]).unwrap();
    // Drain the chain (an identical sync serializes after the retry, and
    // fast-paths once the retry's healthy commit records the signature).
    svc.sync_checked(model, vec![]).unwrap();

    assert_eq!(
        fake.s().commits,
        [
            paths(&["take.wav", "backing.wav"]),
            paths(&["take.wav", "backing.wav"]),
        ]
    );
}

// --- playback_live_edits_test.dart ------------------------------------------------

#[test]
fn an_ab_backing_flip_during_the_page_open_decode_stays() {
    // The mixer's A/B flip is live volume calls only; no save and no resync
    // follow it, so a lost flip stayed wrong for the session.
    let fake = Fake::new();
    let (decode, gate) = gated();
    let svc = service(&fake, decode);
    let load = svc.enqueue_sync(
        vec![
            track("acc", "acc.mp3"),
            TrackPlayback {
                muted: true,
                ..track("ref", "ref.mp3")
            },
        ],
        vec![],
    );
    let first = gate.next();
    svc.set_volume("acc", 1.0, true);
    svc.set_volume("ref", 1.0, false);
    first.complete(Ok(Some(pcm())));
    gate.next().complete(Ok(Some(pcm())));
    load.wait().unwrap();
    assert_eq!(
        fake.s().live,
        BTreeMap::from([(0, (1.0, true)), (1, (1.0, false))])
    );
}

#[test]
fn a_fader_moved_during_a_rebuild_keeps_its_value() {
    let fake = Fake::new();
    fake.cache(&["take.wav"]);
    let (decode, gate) = gated();
    let svc = service(&fake, decode);
    let take = TrackPlayback {
        is_vocal: true,
        ..track("t", "take.wav")
    };
    svc.sync_checked(vec![take.clone()], vec![]).unwrap();

    let rebuild = svc.enqueue_sync(vec![take, track("b", "ref.m4a")], vec![]);
    let decode = gate.next();
    svc.set_volume("t", 0.2, false);
    // Heard at once on the old graph.
    assert_eq!(fake.s().live[&0], (0.2, false));
    decode.complete(Ok(Some(pcm())));
    rebuild.wait().unwrap();
    assert_eq!(fake.s().live[&0], (0.2, false));
}

#[test]
fn an_edit_queued_behind_a_rebuild_survives_the_fast_path_too() {
    let fake = Fake::new();
    fake.cache(&["take.wav"]);
    let (decode, gate) = gated();
    let svc = service(&fake, decode);
    let take = track("t", "take.wav");
    let b = track("b", "ref.m4a");
    let rebuild = svc.enqueue_sync(vec![take.clone(), b.clone()], vec![]);
    // Same topology: this one takes the fast path once the rebuild lands.
    let fast = svc.enqueue_sync(vec![take, b], vec![]);
    let decode = gate.next();
    svc.set_volume("t", 0.5, true);
    decode.complete(Ok(Some(pcm())));
    rebuild.wait().unwrap();
    fast.wait().unwrap();
    assert_eq!(fake.s().live[&0], (0.5, true));
}

#[test]
fn a_newer_sync_wins_over_an_edit_its_model_already_carries() {
    // A failed save reverts the model and resyncs: that snapshot is newer
    // than the edit it reverts, so the edit must not be replayed over it.
    let fake = Fake::new();
    fake.cache(&["take.wav"]);
    let svc = service(&fake, None);
    let take = track("t", "take.wav");
    svc.sync_checked(vec![take.clone()], vec![]).unwrap();
    svc.set_volume("t", 0.2, false);
    svc.sync_checked(vec![take], vec![]).unwrap();
    assert_eq!(fake.s().live[&0], (1.0, false));
}

fn lane_clips(n: usize) -> Vec<TrackPlayback> {
    (0..n)
        .map(|i| TrackPlayback {
            fx: Some(TrackFx::flat()),
            ..in_lane(
                &format!("c{i}"),
                &format!("c{i}.wav"),
                i as i64 * 1000,
                "take",
            )
        })
        .collect()
}

#[test]
fn a_lane_whose_first_clip_left_the_model_still_hears_its_fx() {
    // The first clip of a take deleted, its save still in flight: the mixer
    // sends the lane's FX and pan to the remaining clips, which the playing
    // graph still runs through the deleted clip's chain. Skipping followers
    // dropped every one of them until the resync committed.
    let fake = Fake::new();
    let svc = service(&fake, instant());
    let clips = lane_clips(3);
    svc.sync_checked(clips.clone(), vec![]).unwrap();
    let per_sync = fake.s().chain_calls.len();
    fake.s().chain_calls.clear();
    let fx = TrackFx {
        eq_on: false,
        ..TrackFx::flat()
    };
    for c in &clips[1..] {
        svc.set_track_fx(&c.id, &fx);
        svc.set_track_pan(&c.id, 0.3);
    }
    // Once, not per clip.
    assert_eq!(fake.s().chain_calls.len(), per_sync);
}

#[test]
fn an_fx_drag_on_a_many_clip_lane_sets_the_lane_chain_once() {
    let fake = Fake::new();
    let svc = service(&fake, instant());
    let clips = lane_clips(20);
    svc.sync_checked(clips.clone(), vec![]).unwrap();
    let per_sync = fake.s().chain_calls.len();
    // Only the leader is set.
    assert_eq!(
        fake.s().chain_calls.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([0])
    );

    fake.s().chain_calls.clear();
    // The mixer pushes the lane's value (one object per tick) to each clip.
    for tick in 0..2 {
        let fx = TrackFx {
            eq_on: tick % 2 == 0,
            ..TrackFx::flat()
        };
        for c in &clips {
            svc.set_track_fx(&c.id, &fx);
            svc.set_track_pan(&c.id, 0.3 + f64::from(tick) * 0.1);
        }
    }
    assert_eq!(fake.s().chain_calls.len(), 2 * per_sync);
}

// --- The Rust side: the transport, the position poll, an absent engine ---------------

#[test]
fn seek_emits_the_position_at_once_and_set_loop_converts_to_frames() {
    let fake = Fake::new();
    let svc = service(&fake, None);
    let mut position = svc.position();
    svc.seek(Duration::from_millis(1500));
    assert_eq!(position.try_recv(), Ok(Duration::from_millis(1500)));
    assert_eq!(svc.current_position(), Duration::from_millis(1500));
    svc.set_loop(true, 1000, 2500);
    assert_eq!(fake.s().transport, ["seek 72000", "loop true 48000 120000"]);
}

/// The subscription's next event, waiting up to 5 s for it.
fn next_event<T>(sub: &mut Subscription<T>) -> T {
    for _ in 0..500 {
        match sub.try_recv() {
            Ok(event) => return event,
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
            Err(TryRecvError::Disconnected) => panic!("the stream ended"),
        }
    }
    panic!("no event within 5 s")
}

#[test]
fn play_polls_the_playhead_until_pause() {
    let fake = Fake::new();
    let svc = service(&fake, None);
    let mut playing = svc.playing();
    let mut position = svc.position();
    fake.frame.store(48_000, Ordering::SeqCst);
    svc.play();
    assert!(svc.is_playing());
    assert_eq!(playing.try_recv(), Ok(true));
    // The poll ticks every 16 ms with the engine's frame position.
    for _ in 0..2 {
        assert_eq!(next_event(&mut position), Duration::from_secs(1));
    }
    svc.pause();
    assert!(!svc.is_playing());
    assert_eq!(playing.try_recv(), Ok(false));
    // Cancelled: nothing after the pause (drain what raced it first).
    while position.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(position.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(fake.s().transport, ["play", "pause"]);
}

#[test]
fn dispose_ends_the_streams_and_stops_the_poll() {
    let fake = Fake::new();
    let svc = service(&fake, None);
    let mut position = svc.position();
    let mut playing = svc.playing();
    svc.play();
    PlaybackEngine::dispose(&svc);
    assert!(!svc.is_available());
    assert_eq!(playing.try_recv(), Ok(true));
    assert_eq!(playing.try_recv(), Err(TryRecvError::Disconnected));
    while position.try_recv().is_ok() {}
    assert_eq!(position.try_recv(), Err(TryRecvError::Disconnected));
    // Every call is a no-op now.
    svc.play();
    svc.seek(Duration::from_secs(1));
    assert_eq!(svc.current_position(), Duration::ZERO);
    assert_eq!(fake.s().transport, ["play"]);
    svc.sync_checked(vec![track("a", "a.wav")], vec![]).unwrap();
    assert!(fake.s().commits.is_empty());
}

#[test]
fn the_shared_reverb_and_master_are_reapplied_after_a_rebuild() {
    #[derive(Default)]
    struct Reverb {
        inner: Fake,
        reverbs: Mutex<Vec<(f32, f32)>>,
        masters: Mutex<Vec<f32>>,
    }
    impl EngineBindings for Reverb {
        fn sample_rate(&self) -> f64 {
            48_000.0
        }
        fn destroy(&self) {}
        fn buses_begin(&self) -> Result<(), EngineError> {
            Ok(())
        }
        fn tracks_begin(&self) -> Result<(), EngineError> {
            self.inner.tracks_begin()
        }
        fn tracks_add_pcm(
            &self,
            path: &str,
            pcm: &[f32],
            start_ms: i64,
            gain: f32,
            muted: bool,
            is_vocal: bool,
            bus: i32,
        ) -> Result<i32, EngineError> {
            self.inner
                .tracks_add_pcm(path, pcm, start_ms, gain, muted, is_vocal, bus)
        }
        fn tracks_add(
            &self,
            path: &str,
            start_ms: i64,
            gain: f32,
            muted: bool,
            is_vocal: bool,
            bus: i32,
        ) -> Result<i32, EngineError> {
            self.inner
                .tracks_add(path, start_ms, gain, muted, is_vocal, bus)
        }
        fn tracks_commit_staged(&self) -> Result<(), EngineError> {
            self.inner.tracks_commit_staged()
        }
        fn tracks_publish(&self) -> Result<(), EngineError> {
            self.inner.tracks_publish()
        }
        fn commit_degraded_mask(&self) -> u64 {
            0
        }
        fn set_track_volume(&self, i: i32, g: f32, m: bool) -> Result<(), EngineError> {
            self.inner.set_track_volume(i, g, m)
        }
        fn set_pan(&self, _: i32, _: f32) -> Result<(), EngineError> {
            Ok(())
        }
        fn set_reverb(
            &self,
            room: f32,
            _damping: f32,
            _wet: f32,
            dry: f32,
            _width: f32,
        ) -> Result<(), EngineError> {
            self.reverbs.lock().unwrap().push((room, dry));
            Ok(())
        }
        fn set_master(&self, gain: f32) -> Result<(), EngineError> {
            self.masters.lock().unwrap().push(gain);
            Ok(())
        }
    }
    let fake = Arc::new(Reverb::default());
    let svc = GraphAudioPlaybackService::with_bindings(
        Arc::clone(&fake) as Arc<dyn EngineBindings>,
        instant(),
    );
    svc.set_reverb(&ReverbSettings {
        room_size: 0.75,
        ..ReverbSettings::DEFAULT
    });
    svc.set_master_volume(0.5);
    svc.sync_checked(vec![track("a", "a.wav")], vec![]).unwrap();
    // Set live, then again on the rebuilt graph; the bus is wet-only (dry 0).
    assert_eq!(*fake.reverbs.lock().unwrap(), [(0.75, 0.0), (0.75, 0.0)]);
    assert_eq!(*fake.masters.lock().unwrap(), [0.5, 0.5]);
}

#[test]
fn the_topology_signature_covers_wiring_and_nothing_live() {
    let base = TrackPlayback {
        bus_id: Some("bus".into()),
        fx: Some(TrackFx::flat()),
        sends: vec![TrackSend::new("aux", -6.0)],
        lane_id: Some("lane".into()),
        ..at("t", "t.wav", 250)
    };
    let sig = topology_sig(std::slice::from_ref(&base), &[]);
    assert_eq!(sig, "t.wav@250>bus:0,1,2,3,4,5,6,7,8;aux~lane|#");
    // Live values leave it alone.
    let live = TrackPlayback {
        volume: 0.1,
        muted: true,
        pan: -1.0,
        sends: vec![TrackSend::new("aux", -60.0)],
        ..base.clone()
    };
    assert_eq!(topology_sig(&[live], &[]), sig);
    let bus = BusPlayback {
        output_bus_id: Some("master2".into()),
        sends: vec![TrackSend::new("x", 0.0)],
        ..BusPlayback::new("b1", TrackFx::flat())
    };
    assert_eq!(
        topology_sig(&[], &[bus]),
        "#b1>master2:0,1,2,3,4,5,6,7,8;x|"
    );
}

#[test]
fn the_input_word_helpers_split_the_packed_words() {
    let state = (0x00AB_CDEF_u64 << 1) | 1;
    assert!(GraphAudioPlaybackService::input_state_running(state));
    assert!(!GraphAudioPlaybackService::input_state_running(state & !1));
    assert_eq!(GraphAudioPlaybackService::state_gen_of(state), 0xAB_CDEF);
    let applied = (42_u64 << 24) | 0xAB_CDEF;
    assert_eq!(GraphAudioPlaybackService::applied_seq_of(applied), 42);
    assert_eq!(
        GraphAudioPlaybackService::applied_gen_of(applied),
        0xAB_CDEF
    );
    assert_eq!(
        [
            GraphAudioPlaybackService::EVENT_OUTPUT_LOST,
            GraphAudioPlaybackService::EVENT_INPUT_LOST,
            GraphAudioPlaybackService::EVENT_AEC_BYPASSED,
            GraphAudioPlaybackService::EVENT_INPUT_FELL_BACK,
            GraphAudioPlaybackService::EVENT_RECORD_LIMIT,
            GraphAudioPlaybackService::EVENT_OUTPUT_REROUTED,
            GraphAudioPlaybackService::EVENT_RECORD_FAILED,
        ],
        [1, 2, 4, 8, 16, 32, 64]
    );
}

#[cfg(not(feature = "device"))]
#[test]
fn without_an_engine_every_call_is_a_silent_no_op() {
    let svc = GraphAudioPlaybackService::new();
    assert!(!svc.is_available());
    assert_eq!(svc.sample_rate(), 48_000.0);
    let mut position = svc.position();
    svc.sync_checked(vec![track("a", "a.wav")], vec![]).unwrap();
    svc.play();
    svc.seek(Duration::from_secs(2));
    svc.set_volume("a", 0.5, true);
    assert!(!svc.is_playing());
    assert_eq!(svc.current_position(), Duration::ZERO);
    assert_eq!(position.try_recv(), Err(TryRecvError::Empty));
    assert!(!svc.set_output_device("x"));
    assert!(!svc.start_input_recording("/tmp/x.wav"));
    assert_eq!(svc.actual_output_device(), "");
    assert_eq!(svc.poll_device_events(u32::MAX), 0);
    assert!(svc.pull_input_frames().is_empty());
    assert_eq!(svc.record_latency_ms(), 0.0);
    assert_eq!(svc.track_gain_reduction_db("a"), 0.0);
}
