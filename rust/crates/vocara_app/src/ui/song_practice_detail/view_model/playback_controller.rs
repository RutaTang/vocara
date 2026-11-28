//! Transport, seek, loop/cycle region, the master-clock playhead, and
//! timeline zoom / viewport pan / follow-cursor for the song detail screen
//! (port of `ui/song_practice_detail/view_model/playback_controller.dart`).
//!
//! # How the view model drives it
//!
//! Dart's controller held closures onto the view model: the live practice,
//! whether a take is recording or holds its start, whether the engine opened,
//! and the toast. Here they are a [`PlaybackHost`] the view model hands each
//! call; it is read at the moment of the call, as the closures were.
//!
//! Dart's three notification channels become what the widgets compare
//! between builds:
//!
//! - [`PlaybackController::revision`]: the controller's own
//!   `notifyListeners` (the playing flag, the loop toggle);
//! - [`PlaybackController::viewport_revision`]: the `viewport` listenable,
//!   for changes to the timeline VIEW alone (pan, zoom, follow on/off and
//!   page-flips, loop-edge drags, the extent stepping on under a take),
//!   which do not bump `revision`: they arrive at trackpad, drag or playhead
//!   rate, and only the timeline and the transport's zoom, follow and loop
//!   controls read that state;
//! - [`PlaybackController::position`]: the `position` notifier, the
//!   playhead.
//!
//! Every method returns at once: the engine's transport calls do (only its
//! `sync` blocks, and that is the mixer's).
//!
//! # The engine's streams
//!
//! The engine's position and playing streams are subscribed at construction,
//! as Dart listened in its constructor, so no event added after it is lost.
//! Dart's listeners ran from the event loop after the engine added an event;
//! here [`PlaybackController::drain_engine_events`] runs them, in order. The
//! screen calls it on each frame tick while the page is open and after any
//! message that moved the transport; a test calls it where Dart pumped the
//! event queue.

#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::error::TryRecvError;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_num::{clamp_f64, clamp_i64, pow_f64, round_to_i64};
use vocara_services::playback_engine::{PlaybackEngine, Subscription};

/// What the transport reads from the view model that owns it (Dart's
/// constructor closures).
pub trait PlaybackHost {
    /// The live practice model (timeline/content extents for clamping).
    fn practice(&self) -> Option<&SongPractice>;

    /// Whether a take is being recorded: the position stream must yield to
    /// the recording clock then, or the playhead fights the record tick.
    fn is_recording(&self) -> bool;

    /// Whether a take arming has taken its start position: the mic, once
    /// ready, seeks the backing back there, so a transport move then was
    /// accepted and silently undone, the take landing where it began.
    /// Earlier in REC (the permission prompt, the empty take's save) a move
    /// still sets where the take starts. Only the transport moves check it:
    /// the position stream and the loop wrap follow the engine until the take
    /// runs. Never, by default.
    fn take_holds_start(&self) -> bool {
        false
    }

    /// Whether the native engine opened. One that failed (no output device
    /// when the song opened) plays nothing and never opens by itself. Always,
    /// by default.
    fn engine_available(&self) -> bool {
        true
    }

    /// Shows a toast. Dart's was optional; none by default.
    fn toast(&mut self, _message: &str) {}
}

/// Most zoomed in: a 1-second window.
const VIEW_MIN_MS: i64 = 1000;

/// How far past the record head the extent reaches while a take runs on
/// beyond the timeline's end — a page at a time, so a fit-all view rescales
/// every few seconds rather than on every record tick.
const RECORD_REACH_STEP_MS: i64 = 10_000;

/// A whole-millisecond `Duration` (`Duration(milliseconds: ms)`); the loop
/// edges it is made from are never negative.
fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

/// `Duration.inMilliseconds`: whole milliseconds, truncated.
fn in_milliseconds(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// Transport, seek, loop/cycle region, the master-clock playhead, and
/// timeline zoom / viewport pan / follow-cursor for the song detail screen.
/// Owns the engine's position/playing stream subscriptions; does NOT own the
/// engine itself (the view model creates, attaches and disposes it).
pub struct PlaybackController {
    engine: Arc<dyn PlaybackEngine>,
    position_events: Option<Subscription<Duration>>,
    playing_events: Option<Subscription<bool>>,

    /// Master-clock playhead position. Drives the playhead overlay only.
    position: Duration,

    is_playing: bool,

    /// Set in [`PlaybackController::dispose`]. Guards the notifications and
    /// stream deliveries that can race the tear-down.
    disposed: bool,

    revision: u64,
    viewport_revision: u64,

    /// Counts the user's own transport presses (see
    /// [`PlaybackController::transport_moves`]).
    transport_moves: u64,

    loop_enabled: bool,
    loop_start_ms: i64,
    loop_end_ms: i64,

    /// Whether the loop's end is still the default — the end of the content —
    /// rather than an edge the user dragged. A default end follows the content
    /// as it grows: a new practice opens on the 20 s placeholder timeline, and
    /// the first import or take left the region (and every loop pass) stuck on
    /// those 20 seconds of a three-minute song. Cleared when the user drags the
    /// end handle short of the content's end, set again when they drag it back
    /// to it; a dragged start leaves the end the default. Transient, like the
    /// region itself (neither is saved), so a reopen starts from the default.
    loop_end_follows_content: bool,

    // The timeline extent (the full arrangement, persisted) is the "fit all"
    // view. The zoom narrows the *visible* span — anchored at the start —
    // down to a 1-second window, so the grid can refine to one line per
    // second. The view span is transient (not persisted); it resets to
    // fit-all on reopen.
    /// 0 = fit all (the whole extent).
    view_span_ms: i64,

    /// Left edge of the visible window (ms).
    view_start_ms: i64,

    follow_cursor: bool,
}

impl fmt::Debug for PlaybackController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlaybackController")
            .field("position", &self.position)
            .field("is_playing", &self.is_playing)
            .field("loop_enabled", &self.loop_enabled)
            .field("loop_start_ms", &self.loop_start_ms)
            .field("loop_end_ms", &self.loop_end_ms)
            .field("view_span_ms", &self.view_span_ms)
            .field("view_start_ms", &self.view_start_ms)
            .field("follow_cursor", &self.follow_cursor)
            .finish_non_exhaustive()
    }
}

impl PlaybackController {
    /// What Play and Monitor say on an engine that failed to open: both did
    /// nothing at all, and Monitor even showed itself on.
    pub const NO_DEVICE_MESSAGE: &'static str =
        "No audio device is available. Connect one, then reopen this song.";

    /// Shortest allowed loop span — also the "too close to a clip edge" guard
    /// the view model's split action reuses.
    pub const MIN_LOOP_MS: i64 = 150;

    /// The transport over `engine`, listening to its playing and position
    /// streams from now on.
    pub fn new(engine: Arc<dyn PlaybackEngine>) -> PlaybackController {
        let playing_events = Some(engine.playing());
        let position_events = Some(engine.position());
        PlaybackController {
            engine,
            position_events,
            playing_events,
            position: Duration::ZERO,
            is_playing: false,
            disposed: false,
            revision: 0,
            viewport_revision: 0,
            transport_moves: 0,
            loop_enabled: false,
            loop_start_ms: 0,
            loop_end_ms: 0,
            loop_end_follows_content: true,
            view_span_ms: 0,
            view_start_ms: 0,
            follow_cursor: true,
        }
    }

    /// The engine, for sibling controllers' transport primitives (recording
    /// seeks/plays/pauses and disables the loop wrap through the SAME
    /// instance). Ownership (creation, settings attach, disposal) stays with
    /// the view model.
    pub fn engine(&self) -> &Arc<dyn PlaybackEngine> {
        &self.engine
    }

    /// Bumped on every notification of this controller (Dart's
    /// `notifyListeners`); never after [`PlaybackController::dispose`].
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bumped on every change to the timeline view alone (Dart's `viewport`
    /// listenable); see the module doc.
    pub fn viewport_revision(&self) -> u64 {
        self.viewport_revision
    }

    fn notify(&mut self) {
        if self.disposed {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// The viewport notifier's `changed`: guarded like the controller's own
    /// notifications, since a position event can race the tear-down.
    fn viewport_changed(&mut self) {
        if !self.disposed {
            self.viewport_revision = self.viewport_revision.wrapping_add(1);
        }
    }

    /// Master-clock playhead position. Drives the playhead overlay only.
    pub fn position(&self) -> Duration {
        self.position
    }

    /// Writes the playhead (Dart's `position.value = ...`): no follow, no
    /// engine call.
    pub fn set_position(&mut self, position: Duration) {
        self.position = position;
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing
    }

    /// Delivers the engine's playing and position events added since the
    /// last call, in order (Dart's stream listeners): the playing flag
    /// notifies; a position moves the playhead and follows it — except while
    /// recording, when the playhead follows the recording clock instead (the
    /// recording controller's level tick writes it through
    /// [`PlaybackController::set_recording_position`]). Returns whether any
    /// event arrived. After [`PlaybackController::dispose`] nothing is
    /// delivered.
    pub fn drain_engine_events(&mut self, host: &dyn PlaybackHost) -> bool {
        let playing = drain(&mut self.playing_events);
        let positions = drain(&mut self.position_events);
        let any = !playing.is_empty() || !positions.is_empty();
        for p in playing {
            self.is_playing = p;
            self.notify();
        }
        for d in positions {
            if !host.is_recording() {
                self.position = d;
                self.maybe_follow_cursor(host, in_milliseconds(d));
            }
        }
        any
    }

    // --- Transport -----------------------------------------------------------

    /// Whether a take holds the transport: in flight, or about to be.
    fn take_holds_transport(host: &dyn PlaybackHost) -> bool {
        host.is_recording() || host.take_holds_start()
    }

    /// Counts the user's own transport presses — Play/Pause, a seek, Stop —
    /// so code that paused playback for its own work can tell whether the
    /// user moved the transport meanwhile. The engine's position is no test
    /// for it: a render already under way when it paused still lands one
    /// buffer later.
    pub fn transport_moves(&self) -> u64 {
        self.transport_moves
    }

    pub fn toggle_play(&mut self, host: &mut dyn PlaybackHost) {
        self.transport_moves += 1;
        if self.engine.is_playing() {
            self.engine.pause();
        } else if !host.engine_available() {
            host.toast(Self::NO_DEVICE_MESSAGE);
        } else {
            self.engine.play();
        }
    }

    /// Moves the backing. Ignored while a take is in flight: the take is
    /// placed on a straight timeline from where it started, so a ruler click,
    /// lyric tap or analysis scrub that jumped the backing mid-take would
    /// leave everything sung after the jump against the wrong part of the
    /// song. (The recording controller drives its own seeks through
    /// [`PlaybackController::engine`] directly.)
    pub fn seek(&mut self, host: &dyn PlaybackHost, to: Duration) {
        if Self::take_holds_transport(host) {
            return;
        }
        self.transport_moves += 1;
        self.engine.seek(to);
    }

    /// The recording clock's playhead write. While a take is armed the
    /// engine's position stream is ignored (the record tick owns the
    /// playhead), so the viewport has to be followed from here — otherwise
    /// auto-scroll silently stops exactly when it matters most, and the record
    /// head (and the live waveform being drawn under it) runs off the right
    /// edge of the window.
    pub fn set_recording_position(&mut self, host: &dyn PlaybackHost, to: Duration) {
        let extent = self.extent_ms(host);
        self.position = to;
        // Past the timeline's end the extent steps on with the head (see
        // [`PlaybackController::extent_ms`]): the timeline re-lays itself out
        // over it, follow or not.
        if self.extent_ms(host) != extent {
            self.viewport_changed();
        }
        self.maybe_follow_cursor(host, in_milliseconds(to));
    }

    /// Stops playback and returns the playhead to the start of the play
    /// range: the loop's start while it loops, else the song's. (The region
    /// stays set with Loop off, and Stop went back to its start there too,
    /// mid-song.) A no-op while a take is in flight (pausing and rewinding
    /// the backing under a still-rolling mic would misalign the take); the
    /// transport bar routes its Stop to the recording instead.
    pub fn stop(&mut self, host: &dyn PlaybackHost) {
        if Self::take_holds_transport(host) {
            return;
        }
        self.transport_moves += 1;
        self.engine.pause();
        let start = if self.loop_enabled {
            self.loop_start_ms
        } else {
            0
        };
        self.engine.seek(millis(start));
    }

    // --- Loop / cycle region ------------------------------------------------

    pub fn loop_enabled(&self) -> bool {
        self.loop_enabled
    }

    /// Pushes the loop region to the engine. A take disables the engine's
    /// wrap (the recording plays freely from its start), and every push here —
    /// a save's [`PlaybackController::ensure_loop_range`], a loop toggle or
    /// handle drag mid-take — must keep it disabled, or the backing would
    /// jump back at loop end while the mic keeps recording straight on. The
    /// region itself still updates, and `ensure_loop_range` re-arms the wrap
    /// once the take stops.
    fn push_loop(&self, host: &dyn PlaybackHost) {
        self.engine.set_loop(
            self.loop_enabled && !host.is_recording(),
            self.loop_start_ms,
            self.loop_end_ms,
        );
    }

    pub fn loop_start_ms(&self) -> i64 {
        self.loop_start_ms
    }

    pub fn loop_end_ms(&self) -> i64 {
        self.loop_end_ms
    }

    pub fn has_loop(&self) -> bool {
        self.loop_end_ms > self.loop_start_ms
    }

    /// The timeline length the view pans and zooms over: the practice's, or —
    /// while a take runs past its end — reaching past the record head.
    pub fn extent_ms(&self, host: &dyn PlaybackHost) -> i64 {
        let mut c = host.practice().map_or(0, |p| p.timeline_duration_ms);
        // The timeline only grows when a save sees the take, and nothing saves
        // mid-take: a take running past its end (an a cappella first take on
        // the default timeline, singing on past the backing) ran the record
        // head and its forming waveform off the window, with nowhere to scroll
        // after them. While recording, the position is the record head.
        if host.is_recording() {
            let head = in_milliseconds(self.position);
            if head > c {
                c = (head / RECORD_REACH_STEP_MS + 1) * RECORD_REACH_STEP_MS;
            }
        }
        if c < VIEW_MIN_MS {
            VIEW_MIN_MS
        } else {
            c
        }
    }

    /// The visible timeline span in ms — between a 1-second window and
    /// fit-all.
    pub fn view_span_ms(&self, host: &dyn PlaybackHost) -> i64 {
        let ext = self.extent_ms(host);
        let span = if self.view_span_ms <= 0 {
            ext
        } else {
            self.view_span_ms
        };
        clamp_i64(span, VIEW_MIN_MS, ext)
    }

    /// Current zoom as a slider value in [0,1]: 0 = fully zoomed out (fit
    /// all), 1 = fully zoomed in (1-second window).
    pub fn timeline_zoom(&self, host: &dyn PlaybackHost) -> f64 {
        let (lo, hi) = (VIEW_MIN_MS, self.extent_ms(host));
        if hi <= lo {
            return 0.0;
        }
        let span = self.view_span_ms(host) as f64;
        clamp_f64(
            1.0 - (span / lo as f64).ln() / (hi as f64 / lo as f64).ln(),
            0.0,
            1.0,
        )
    }

    /// Sets the zoom from the slider live (transient view state).
    pub fn set_timeline_zoom_live(&mut self, host: &dyn PlaybackHost, value: f64) {
        let (lo, hi) = (VIEW_MIN_MS, self.extent_ms(host));
        if hi <= lo {
            return;
        }
        let t = clamp_f64(value, 0.0, 1.0);
        let span = clamp_i64(
            round_to_i64(lo as f64 * pow_f64(hi as f64 / lo as f64, 1.0 - t)),
            lo,
            hi,
        );
        if span == self.view_span_ms(host) {
            return;
        }
        self.view_span_ms = span;
        self.recenter_view_on_playhead(host);
        self.viewport_changed();
    }

    /// The slider's release: the zoom is transient, so there is nothing to
    /// persist.
    pub fn commit_timeline_zoom(&mut self) {}

    /// Nudges the zoom by `factor` (>1 zooms out / widens, <1 zooms in), for
    /// the slider's end icons.
    pub fn zoom_timeline(&mut self, host: &dyn PlaybackHost, factor: f64) {
        let span = clamp_i64(
            round_to_i64(self.view_span_ms(host) as f64 * factor),
            VIEW_MIN_MS,
            self.extent_ms(host),
        );
        if span == self.view_span_ms(host) {
            return;
        }
        self.view_span_ms = span;
        self.recenter_view_on_playhead(host);
        self.viewport_changed();
    }

    // --- Viewport pan / follow -----------------------------------------------

    /// The left edge of the visible window, clamped so it never scrolls past
    /// the extent. 0 when fit-all.
    pub fn view_start_ms(&self, host: &dyn PlaybackHost) -> i64 {
        let max_start = self.max_view_start(host);
        clamp_i64(self.view_start_ms, 0, max_start)
    }

    /// `(extent - viewSpan).clamp(0, extent)`.
    fn max_view_start(&self, host: &dyn PlaybackHost) -> i64 {
        let extent = self.extent_ms(host);
        clamp_i64(extent - self.view_span_ms(host), 0, extent)
    }

    /// Whether the timeline auto-scrolls to keep the playhead in view (on by
    /// default; a manual scroll turns it off).
    pub fn follow_cursor(&self) -> bool {
        self.follow_cursor
    }

    pub fn toggle_follow_cursor(&mut self, host: &dyn PlaybackHost) {
        self.follow_cursor = !self.follow_cursor;
        if self.follow_cursor {
            self.recenter_view_on_playhead(host);
        }
        self.viewport_changed();
    }

    /// Pans the visible window by `delta_ms` (horizontal scroll / drag).
    /// Turns off follow so a manual scroll sticks.
    pub fn pan_view(&mut self, host: &dyn PlaybackHost, delta_ms: i64) {
        let max_start = self.max_view_start(host);
        if max_start <= 0 {
            return;
        }
        let next = clamp_i64(self.view_start_ms(host) + delta_ms, 0, max_start);
        if next == self.view_start_ms {
            if self.follow_cursor {
                self.follow_cursor = false;
                self.viewport_changed();
            }
            return;
        }
        self.follow_cursor = false;
        self.view_start_ms = next;
        self.viewport_changed();
    }

    fn recenter_view_on_playhead(&mut self, host: &dyn PlaybackHost) {
        let max_start = self.max_view_start(host);
        self.view_start_ms = clamp_i64(
            in_milliseconds(self.position) - self.view_span_ms(host) / 2,
            0,
            max_start,
        );
    }

    fn maybe_follow_cursor(&mut self, host: &dyn PlaybackHost, playhead_ms: i64) {
        if !self.follow_cursor {
            return;
        }
        let span = self.view_span_ms(host);
        let max_start = self.max_view_start(host);
        if max_start <= 0 {
            return;
        }
        let start = self.view_start_ms(host);
        // Page-scroll: while the playhead is comfortably inside the window it
        // moves smoothly across a *static* view (no stutter); only when it
        // nears the right edge (or falls behind the view) do we jump forward
        // by a page.
        if playhead_ms >= start && playhead_ms < start + (span * 88 / 100) {
            return;
        }
        let next = clamp_i64(playhead_ms - span / 10, 0, max_start);
        if next == self.view_start_ms {
            return;
        }
        self.view_start_ms = next;
        self.viewport_changed();
    }

    /// Defaults the play region to the content length (the longest take),
    /// keeps a default end on the content's end as it grows (see
    /// `loop_end_follows_content`), and keeps a dragged one within it. Also
    /// what re-arms the loop after recording disabled the engine's wrap.
    pub fn ensure_loop_range(&mut self, host: &dyn PlaybackHost) {
        let timeline = host.practice().map_or(0, |p| p.timeline_duration_ms);
        // Clamp the loop to real audio (content), not the padded timeline, and
        // pull it BACK when content shrinks — e.g. deleting the longest take
        // must not leave loopEnd cycling through a now-silent tail.
        let max = Self::loop_max_ms(host);
        if self.loop_end_follows_content || self.loop_end_ms <= 0 || self.loop_end_ms > max {
            self.loop_end_ms = max;
        }
        if self.loop_start_ms < 0 {
            self.loop_start_ms = 0;
        }
        if self.loop_start_ms > self.loop_end_ms - Self::MIN_LOOP_MS {
            self.loop_start_ms = clamp_i64(self.loop_end_ms - Self::MIN_LOOP_MS, 0, timeline);
        }
        self.push_loop(host);
    }

    /// When the loop is active, bring the playhead inside [loopStart,
    /// loopEnd). The native engine only WRAPS at loopEnd; it never snaps a
    /// playhead that sits BEFORE loopStart up into the window, so without
    /// this enabling a loop while the playhead is to its left plays a long
    /// out-of-region pass before wrapping.
    fn snap_playhead_into_loop(&mut self, host: &dyn PlaybackHost) {
        // Never mid-take: the record clock owns the playhead, and the seek
        // would jump the backing under the rolling take.
        if !self.loop_enabled || !self.has_loop() || Self::take_holds_transport(host) {
            return;
        }
        let pos_ms = in_milliseconds(self.position);
        if pos_ms < self.loop_start_ms || pos_ms >= self.loop_end_ms {
            self.position = millis(self.loop_start_ms);
            self.engine.seek(millis(self.loop_start_ms));
        }
    }

    /// The furthest a loop edge may go: the end of real audio, NOT the padded
    /// timeline — looping into the ~15% silent tail just plays dead air each
    /// cycle.
    fn loop_max_ms(host: &dyn PlaybackHost) -> i64 {
        let content = host.practice().map_or(0, SongPractice::content_end_ms);
        if content > 0 {
            content
        } else {
            host.practice().map_or(0, |p| p.timeline_duration_ms)
        }
    }

    /// Drags the left edge of the loop region (from the ruler handle).
    pub fn set_loop_start(&mut self, host: &dyn PlaybackHost, ms: i64) {
        // Guard the upper bound: for sub-MIN_LOOP_MS content (a tiny clip)
        // the naive `loop_end_ms - MIN_LOOP_MS` goes negative, and Dart's
        // num.clamp asserted (debug) or returned a negative upper limit
        // (release) when upper < lower.
        let hi = 0.max(self.loop_end_ms - Self::MIN_LOOP_MS);
        self.loop_start_ms = clamp_i64(ms, 0, hi);
        self.push_loop(host);
        self.snap_playhead_into_loop(host);
        self.viewport_changed(); // a handle drag: the region only (see the module doc)
    }

    /// Drags the right edge of the loop region.
    pub fn set_loop_end(&mut self, host: &dyn PlaybackHost, ms: i64) {
        let max = Self::loop_max_ms(host);
        // Guard the lower bound symmetrically: for sub-MIN_LOOP_MS content
        // `loop_start_ms + MIN_LOOP_MS` can exceed the loop max (lower >
        // upper).
        let lo = (self.loop_start_ms + Self::MIN_LOOP_MS).min(max);
        // The user's own end from here on, so content growth no longer moves
        // it — unless they put it back AT the content's end, which is the
        // default end again ("to the end"), not a pin there for the rest of
        // the session.
        self.loop_end_follows_content = ms >= max;
        self.loop_end_ms = clamp_i64(ms, lo, max);
        self.push_loop(host);
        self.snap_playhead_into_loop(host);
        self.viewport_changed(); // a handle drag: the region only (see the module doc)
    }

    pub fn toggle_loop(&mut self, host: &dyn PlaybackHost) {
        self.loop_enabled = !self.loop_enabled;
        self.push_loop(host);
        self.snap_playhead_into_loop(host);
        self.notify();
    }

    /// Stops listening to the engine and silences every later notification.
    /// The engine itself is the view model's to dispose.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.position_events = None;
        self.playing_events = None;
    }
}

/// Every event waiting on `events`, in order; a subscription the engine
/// ended (it was disposed) is dropped once read to its end.
fn drain<T>(events: &mut Option<Subscription<T>>) -> Vec<T> {
    let mut out = Vec::new();
    let mut ended = false;
    if let Some(rx) = events.as_mut() {
        loop {
            match rx.try_recv() {
                Ok(event) => out.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
            }
        }
    }
    if ended {
        *events = None;
    }
    out
}
