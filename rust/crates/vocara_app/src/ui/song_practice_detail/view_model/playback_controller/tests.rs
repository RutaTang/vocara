//! Port of `test/playback_controller_test.dart`: the transport counter, the
//! viewport follow (playing and recording clocks), the loop region and its
//! growth with the content, and the transport during a take.
//!
//! Where Dart awaited `pumpEventQueue()` for the engine's stream events to
//! reach the controller, the port calls
//! [`PlaybackController::drain_engine_events`].

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::playback_engine::{
    Broadcast, BusPlayback, PlaybackEngine, Subscription, TrackPlayback,
};

use super::*;

/// Transport fake: the position/playing streams are driven by the test, and
/// every engine call is recorded so the controller's pushes are assertable.
#[derive(Default)]
struct FakeEngine {
    position: Broadcast<Duration>,
    playing: Broadcast<bool>,
    calls: Mutex<Vec<String>>,
}

impl FakeEngine {
    fn calls(&self) -> MutexGuard<'_, Vec<String>> {
        self.calls.lock().unwrap()
    }

    fn emit_position(&self, d: Duration) {
        self.position.add(d);
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
        Duration::ZERO
    }

    fn is_playing(&self) -> bool {
        false
    }

    fn set_loop(&self, enabled: bool, start_ms: i64, end_ms: i64) {
        self.calls()
            .push(format!("loop:{enabled}:{start_ms}:{end_ms}"));
    }

    fn sync(&self, _tracks: Vec<TrackPlayback>, _buses: Vec<BusPlayback>) {}

    fn play(&self) {
        self.calls().push("play".into());
    }

    fn pause(&self) {
        self.calls().push("pause".into());
    }

    fn seek(&self, position: Duration) {
        self.calls().push(format!("seek:{}", position.as_millis()));
    }

    fn set_volume(&self, _id: &str, _volume: f64, _muted: bool) {}

    fn set_track_pan(&self, _id: &str, _pan: f64) {}

    fn set_master_volume(&self, _gain: f64) {}

    fn supports_effects(&self) -> bool {
        false
    }

    fn set_track_fx(&self, _track_id: &str, _fx: &TrackFx) {}

    fn set_bus_fx(&self, _bus_id: &str, _fx: &TrackFx) {}

    fn set_bus_volume(&self, _bus_id: &str, _volume: f64, _muted: bool) {}

    fn set_bus_pan(&self, _bus_id: &str, _pan: f64) {}

    fn bus_gain_reduction_db(&self, _bus_id: &str) -> f64 {
        0.0
    }

    fn set_track_send_level(&self, _track_id: &str, _slot: usize, _level_db: f64) {}

    fn set_bus_send_level(&self, _bus_id: &str, _slot: usize, _level_db: f64) {}

    fn set_reverb(&self, _reverb: &ReverbSettings) {}

    fn track_gain_reduction_db(&self, _track_id: &str) -> f64 {
        0.0
    }

    fn dispose(&self) {}
}

/// A practice whose timeline is `ms` long, with one take so the content end
/// is real (the loop clamps to content, not to the padded timeline).
fn practice(ms: i64) -> SongPractice {
    let at = DateTime::local_ymd(2026, 1, 1);
    SongPractice {
        timeline_duration_ms: ms,
        vocal_tracks: vec![Arc::new(
            AudioTrack::new("t1", AudioTrackKind::Vocal, "Take", at).with_audio(
                "/pool/a.wav",
                0,
                ms,
                Vec::new(),
            ),
        )],
        ..SongPractice::new("p1", "Song", at, at)
    }
}

/// The view model's side: the live practice and the take's state.
struct Host {
    practice: SongPractice,
    recording: bool,
    starting: bool,
    available: bool,
    toasts: Vec<String>,
}

impl Host {
    fn new(practice: SongPractice) -> Host {
        Host {
            practice,
            recording: false,
            starting: false,
            available: true,
            toasts: Vec::new(),
        }
    }
}

impl PlaybackHost for Host {
    fn practice(&self) -> Option<&SongPractice> {
        Some(&self.practice)
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn take_holds_start(&self) -> bool {
        self.starting
    }

    fn engine_available(&self) -> bool {
        self.available
    }

    fn toast(&mut self, message: &str) {
        self.toasts.push(message.to_string());
    }
}

/// A controller over a fresh fake, on a practice `timeline_ms` long.
fn controller(timeline_ms: i64) -> (PlaybackController, Arc<FakeEngine>, Host) {
    let engine = Arc::new(FakeEngine::default());
    let c = PlaybackController::new(Arc::clone(&engine) as Arc<dyn PlaybackEngine>);
    (c, engine, Host::new(practice(timeline_ms)))
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn span_of(c: &PlaybackController, h: &Host) -> u64 {
    u64::try_from(c.view_span_ms(h)).unwrap()
}

// An engine that failed to open played nothing, with no word why.
#[test]
fn play_on_an_engine_that_failed_to_open_says_so_and_plays_nothing() {
    let (mut c, engine, mut h) = controller(300_000);
    h.available = false;

    c.toggle_play(&mut h);

    assert!(!engine.calls().contains(&"play".to_string()));
    assert_eq!(h.toasts, [PlaybackController::NO_DEVICE_MESSAGE]);
}

#[test]
fn play_pause_a_seek_and_stop_count_as_transport_presses() {
    // What a switch of interpretation checks before playing on: the playhead
    // was no test of them (a render under way at a pause still moves it).
    let (mut c, _engine, mut h) = controller(300_000);
    assert_eq!(c.transport_moves(), 0);
    c.toggle_play(&mut h);
    c.seek(&h, Duration::from_secs(3));
    c.stop(&h);
    assert_eq!(c.transport_moves(), 3);
    // Ignored mid-take, so not counted either.
    h.recording = true;
    c.seek(&h, Duration::from_secs(1));
    c.stop(&h);
    assert_eq!(c.transport_moves(), 3);
}

// --- viewport follow ---------------------------------------------------------

#[test]
fn a_playing_cursor_pages_the_view_forward_once_it_nears_the_edge() {
    let (mut c, engine, h) = controller(300_000);
    c.zoom_timeline(&h, 0.1); // a 30 s window
    let span = span_of(&c, &h);
    assert_eq!(c.view_start_ms(&h), 0);

    // Comfortably inside the window: the view stays put (no stutter).
    engine.emit_position(ms(span / 2));
    assert!(c.drain_engine_events(&h));
    assert_eq!(c.view_start_ms(&h), 0);

    // Past the trigger band: page forward.
    engine.emit_position(ms(span - 1));
    c.drain_engine_events(&h);
    assert!(c.view_start_ms(&h) > 0);
}

#[test]
fn a_manual_pan_turns_follow_off_and_the_view_stops_tracking() {
    let (mut c, engine, h) = controller(300_000);
    c.zoom_timeline(&h, 0.1);
    let span = span_of(&c, &h);

    c.pan_view(&h, 1000);
    assert!(!c.follow_cursor());
    let parked = c.view_start_ms(&h);

    engine.emit_position(ms(span * 3));
    c.drain_engine_events(&h);
    assert_eq!(c.view_start_ms(&h), parked);
}

// Regression guard: while a take is armed the engine's position stream is
// ignored (the record tick owns the playhead), and the record tick wrote the
// notifier directly — so auto-scroll silently stopped for the whole take and
// the record head ran off the right edge of the window.
#[test]
fn the_recording_clock_follows_the_cursor_too() {
    let (mut c, engine, mut h) = controller(300_000);
    c.zoom_timeline(&h, 0.1);
    let span = span_of(&c, &h);
    h.recording = true;

    // The engine keeps streaming during a take; the controller ignores it.
    engine.emit_position(ms(span * 2));
    c.drain_engine_events(&h);
    assert_eq!(c.view_start_ms(&h), 0, "the record clock owns the playhead");

    c.set_recording_position(&h, ms(span - 1));
    assert!(
        c.view_start_ms(&h) > 0,
        "the view must track the record head"
    );
    assert_eq!(c.position(), ms(span - 1));
}

#[test]
fn a_recording_that_stays_inside_the_window_does_not_scroll_it() {
    let (mut c, _engine, mut h) = controller(300_000);
    c.zoom_timeline(&h, 0.1);
    h.recording = true;

    c.set_recording_position(&h, ms(span_of(&c, &h) / 2));
    assert_eq!(c.view_start_ms(&h), 0);
}

// Regression guard: the timeline only grows when a save sees the take, and
// nothing saves mid-take. A take running past the timeline's end (an a
// cappella first take on the default 20 s timeline, singing on past the
// backing) ran the record head and its forming waveform off the window, and
// the view had nowhere to scroll after them.
#[test]
fn a_take_running_past_the_timeline_end_stays_in_view() {
    let (mut c, _engine, mut h) = controller(20_000);
    let layouts = c.viewport_revision();
    h.recording = true;

    c.set_recording_position(&h, ms(26_000));
    assert!(
        c.view_start_ms(&h) + c.view_span_ms(&h) > 26_000,
        "the fit-all view must reach the record head"
    );
    assert!(
        c.viewport_revision() > layouts,
        "the timeline must relayout"
    );
    assert!(
        c.extent_ms(&h) > 26_000,
        "the scrollbar pans over the same extent as the view"
    );

    // Zoomed in, follow pages on past the old end.
    c.zoom_timeline(&h, 0.25);
    c.set_recording_position(&h, ms(40_000));
    assert!(c.view_start_ms(&h) < 40_000);
    assert!(c.view_start_ms(&h) + c.view_span_ms(&h) > 40_000);
}

#[test]
fn the_record_head_stretches_the_extent_only_while_recording() {
    let (mut c, _engine, mut h) = controller(20_000);
    h.recording = true;
    c.set_recording_position(&h, ms(26_000));
    h.recording = false;
    assert_eq!(c.view_span_ms(&h), 20_000, "the saved timeline takes over");
    assert_eq!(c.extent_ms(&h), 20_000);
}

// --- during a take -----------------------------------------------------------
//
// A take plays the backing freely from its start and disables the engine's
// loop wrap. Every controller push used to re-send the remembered loop, so
// any save during the take (a fader release → ensure_loop_range), a loop
// toggle or handle drag re-armed the wrap, and the backing jumped back at
// loop end under a mic that kept recording straight on: everything sung
// after the jump landed against the wrong part of the song.

fn looping() -> (PlaybackController, Arc<FakeEngine>, Host) {
    let (mut c, engine, mut h) = controller(60_000);
    c.ensure_loop_range(&h);
    c.set_loop_start(&h, 10_000);
    c.set_loop_end(&h, 20_000);
    c.toggle_loop(&h);
    // REC: the recording controller disables the wrap directly.
    h.recording = true;
    c.engine().set_loop(false, 0, 0);
    c.set_recording_position(&h, ms(30_000));
    engine.calls().clear();
    (c, engine, h)
}

#[test]
fn a_saves_ensure_loop_range_keeps_the_engine_wrap_off() {
    let (mut c, engine, h) = looping();
    c.ensure_loop_range(&h);
    assert_eq!(*engine.calls(), ["loop:false:10000:20000"]);
    assert!(c.loop_enabled(), "the region itself is kept");
}

#[test]
fn loop_toggles_and_handle_drags_neither_re_arm_the_wrap_nor_seek() {
    let (mut c, engine, h) = looping();
    c.toggle_loop(&h);
    c.toggle_loop(&h);
    c.set_loop_start(&h, 12_000);
    c.set_loop_end(&h, 18_000);
    assert!(engine
        .calls()
        .iter()
        .all(|x| !x.starts_with("seek") && !x.starts_with("loop:true")));
    assert_eq!((c.loop_start_ms(), c.loop_end_ms()), (12_000, 18_000));
    assert_eq!(c.position(), ms(30_000));
}

#[test]
fn seek_and_stop_leave_the_backing_alone() {
    let (mut c, engine, h) = looping();
    c.seek(&h, ms(5000));
    c.stop(&h);
    assert!(engine.calls().is_empty());
}

#[test]
fn the_wrap_re_arms_once_the_take_has_stopped() {
    let (mut c, engine, mut h) = looping();
    h.recording = false;
    c.ensure_loop_range(&h);
    assert_eq!(*engine.calls(), ["loop:true:10000:20000"]);
}

#[test]
fn a_seek_or_stop_once_an_arming_take_has_its_start_position_does_nothing() {
    // REC takes the start position first and seeks the backing back to it
    // once the mic is ready: a ruler click in between moved the playhead,
    // then was undone, and the take landed where REC was pressed.
    let (mut c, engine, mut h) = controller(300_000);
    h.starting = true;
    c.seek(&h, Duration::from_secs(3));
    c.stop(&h);
    assert!(engine.calls().is_empty());
    assert_eq!(c.transport_moves(), 0);

    h.starting = false; // the take ran and stopped, or never started
    c.seek(&h, Duration::from_secs(3));
    assert_eq!(*engine.calls(), ["seek:3000"]);
}

// --- loop region -------------------------------------------------------------

#[test]
fn ensure_loop_range_defaults_the_region_to_the_content_length() {
    let (mut c, engine, h) = controller(60_000);
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_start_ms(), 0);
    assert_eq!(c.loop_end_ms(), 60_000);
    assert!(engine.calls().contains(&"loop:false:0:60000".to_string()));
}

#[test]
fn the_loop_edges_keep_a_minimum_span_in_both_directions() {
    let (mut c, _engine, h) = controller(60_000);
    c.ensure_loop_range(&h);

    c.set_loop_start(&h, 59_990);
    assert_eq!(c.loop_start_ms(), 60_000 - PlaybackController::MIN_LOOP_MS);

    c.set_loop_end(&h, 10);
    assert_eq!(
        c.loop_end_ms(),
        c.loop_start_ms() + PlaybackController::MIN_LOOP_MS
    );
}

// Regression guard: ensure_loop_range only ever pulled the end BACK. A new
// practice opens on the 20 s placeholder timeline, so the first import (or a
// first take) left the region — and every pass of the loop — on the first 20
// seconds of a three-minute song.

#[test]
fn a_default_end_follows_the_content_end_upward() {
    let (mut c, engine, mut h) = controller(20_000);
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 20_000);

    h.practice = practice(180_000); // a 3-minute backing lands
    c.ensure_loop_range(&h);

    assert_eq!(c.loop_end_ms(), 180_000);
    assert_eq!(
        engine.calls().last().map(String::as_str),
        Some("loop:false:0:180000")
    );
}

#[test]
fn an_empty_practices_placeholder_end_gives_way_to_the_first_content() {
    let at = DateTime::local_ymd(2026, 1, 1);
    let (mut c, _engine, mut h) = controller(20_000);
    h.practice = SongPractice {
        timeline_duration_ms: 20_000,
        ..SongPractice::new("p1", "Song", at, at)
    };
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 20_000, "no content: the timeline");

    h.practice = practice(180_000);
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 180_000);
}

#[test]
fn an_end_the_user_dragged_is_never_moved_by_growth() {
    let (mut c, _engine, mut h) = controller(60_000);
    c.ensure_loop_range(&h);
    c.set_loop_end(&h, 45_000);

    h.practice = practice(180_000);
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 45_000);

    // Dragged back all the way to the end is the default end again: it
    // follows the content from there instead of pinning it.
    c.set_loop_end(&h, 1 << 30);
    assert_eq!(c.loop_end_ms(), 180_000);
    h.practice = practice(240_000);
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 240_000);
}

#[test]
fn a_dragged_start_leaves_the_default_end_following() {
    let (mut c, _engine, mut h) = controller(60_000);
    c.ensure_loop_range(&h);
    c.set_loop_start(&h, 30_000);

    h.practice = practice(180_000);
    c.ensure_loop_range(&h);
    assert_eq!((c.loop_start_ms(), c.loop_end_ms()), (30_000, 180_000));
}

#[test]
fn shrinking_content_still_pulls_the_end_back_dragged_or_not() {
    let (mut c, _engine, mut h) = controller(180_000);
    c.ensure_loop_range(&h);
    c.set_loop_end(&h, 150_000);

    h.practice = practice(100_000); // the longest take was deleted
    c.ensure_loop_range(&h);
    assert_eq!(c.loop_end_ms(), 100_000);
}

#[test]
fn enabling_the_loop_snaps_a_playhead_sitting_outside_it_inside() {
    let (mut c, engine, h) = controller(60_000);
    c.ensure_loop_range(&h);
    c.set_loop_start(&h, 20_000);
    engine.calls().clear();

    c.toggle_loop(&h);

    assert_eq!(c.position(), ms(20_000));
    assert!(engine.calls().contains(&"seek:20000".to_string()));
}

// --- The Rust side: the notification channels and the streams ---------------

#[test]
fn the_playing_stream_notifies_and_the_view_changes_bump_only_the_viewport() {
    let (mut c, engine, h) = controller(300_000);
    let (rev, view) = (c.revision(), c.viewport_revision());
    engine.playing.add(true);
    assert!(c.drain_engine_events(&h));
    assert!(c.is_playing());
    assert!(c.revision() > rev);

    // Zoom, pan and loop-edge drags are the timeline view's alone.
    let rev = c.revision();
    c.set_timeline_zoom_live(&h, 0.5);
    c.pan_view(&h, 2000);
    c.ensure_loop_range(&h);
    c.set_loop_end(&h, 100_000);
    assert_eq!(c.revision(), rev);
    assert!(c.viewport_revision() > view);

    // The loop toggle is the controller's own.
    c.toggle_loop(&h);
    assert!(c.revision() > rev);
}

#[test]
fn nothing_is_delivered_or_notified_after_dispose() {
    let (mut c, engine, h) = controller(300_000);
    c.dispose();
    let (rev, view) = (c.revision(), c.viewport_revision());
    engine.emit_position(ms(5000));
    engine.playing.add(true);
    assert!(!c.drain_engine_events(&h));
    assert_eq!(c.position(), Duration::ZERO);
    c.toggle_loop(&h);
    c.pan_view(&h, 1000);
    assert_eq!((c.revision(), c.viewport_revision()), (rev, view));
}

#[test]
fn the_zoom_slider_round_trips_between_fit_all_and_a_one_second_window() {
    let (mut c, _engine, h) = controller(300_000);
    assert_eq!(c.timeline_zoom(&h), 0.0);
    assert_eq!(c.view_span_ms(&h), 300_000);
    c.set_timeline_zoom_live(&h, 1.0);
    assert_eq!(c.view_span_ms(&h), 1000);
    assert_eq!(c.timeline_zoom(&h), 1.0);
    c.set_timeline_zoom_live(&h, 0.0);
    assert_eq!(c.view_span_ms(&h), 300_000);
}
