//! Port of `ui/technique_library/widgets/media/technique_audio_player.dart`:
//! a compact inline audio player — a play/pause button, a scrubbable
//! progress bar and a time readout — over a [`MediaPlayer`] it owns and
//! drops (disposes) with it. The file is opened paused at creation, so the
//! duration shows without pressing play first.
//!
//! Drawn as `DESIGN.md`'s controls: a 32 px well (`Studio::WELL`, radius
//! 6, an inset `BORDER_STRONG` outline) holding a 24 px icon button with
//! the filled play or pause glyph, the thin scrubber (a 2 px track, its
//! played part in the slider's neutral fill, a 10 px knob while hovered)
//! and the 11 px tabular readout.
//!
//! The Dart file also held what the video player shares: the player seam
//! (`createExamplePlayer`, here a [`MediaPlayerFactory`] the host passes
//! in), [`pause_while_play_blocked`] and [`UNAVAILABLE_CLIP_MESSAGE`]; the
//! playback state both players poll (`Transport`) lives here too.
//!
//! # From streams to polling
//!
//! Dart listened to media_kit's `playing`, `position` and `duration`
//! streams. Here the host polls: while [`TechniqueAudioPlayer::needs_tick`]
//! (playing, still loading its duration, or just commanded), it subscribes
//! to [`TechniqueAudioPlayer::subscription`] and sends [`Message::Tick`],
//! and each poll applies what changed as the listeners did: `playing`
//! first, the blocked-play guard, the duration, then the position (an event
//! only when it moved; dropped while a drag holds the bar).
//!
//! # The widget's parameters
//!
//! - `path`: fixed at creation (`initState` read it once; the host keys the
//!   state by `(id, unavailable)`, see `example_players`).
//! - `onPosition`: [`TechniqueAudioPlayer::update`] returns the position to
//!   report; the host forwards it only for the clip that drives the analysis.
//! - `seekSignal` → [`TechniqueAudioPlayer::seek_signal`].
//! - `pauseSignal` → [`TechniqueAudioPlayer::pause_signal`].
//! - `playBlocked` → the `play_blocked` argument of `update` and `view`.

use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::text::Wrapping;
use iced::widget::{canvas as canvas_widget, container, row, Space};
use iced::{
    mouse, Color, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Subscription,
    Theme,
};
use vocara_services::media_player::{
    MediaKind, MediaPlayer, MediaPlayerError, MediaPlayerFactory, PlayerState, VideoFrame,
    FULL_VOLUME,
};
use vocara_store::db::asset_store::AssetStore;

use super::video_controls::Playback;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{boxed, height, radius, type_scale, Studio};
use crate::ui::core::widgets::controls::{icon_button, labelled, SLIDER_FILL, SLIDER_TRACK};
use crate::ui::core::widgets::line_icon::{glyphs, line_icon};
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::wrap::Wrap;

/// What an example whose file is missing on this device says instead of
/// playing (shared by the audio and video players).
pub const UNAVAILABLE_CLIP_MESSAGE: &str = "This clip's file isn't on this device.";

/// How often an audio player is polled while it needs it: the playhead and
/// the analysis it drives move at 30 Hz.
pub const AUDIO_POLL_INTERVAL: Duration = Duration::from_millis(33);

/// How many polls follow a command (play, pause, seek): the native player
/// applies some of them asynchronously, and only a poll sees it. ~0.5 s.
const SETTLE_TICKS: u8 = 15;

/// The player row's height (the live and the unavailable row alike).
pub const HEIGHT: f32 = 32.0;

/// The readout's column at the bar's right end.
pub const READOUT_WIDTH: f32 = 76.0;

/// True when the clip's bytes are not on this device: the repository's
/// `asset://` placeholder for a missing pooled file. Nothing can play it.
pub fn is_unavailable_path(path: &str) -> bool {
    AssetStore::asset_id_of(path).is_some()
}

/// Keeps `player` paused while `blocked` says a take is arming or recording:
/// whatever started it — a key, a media key, a control the block did not
/// reach — it is paused again as soon as a poll reports it `playing`, before
/// it plays on speakers into the take. Returns whether it paused.
pub fn pause_while_play_blocked(
    player: &mut dyn MediaPlayer,
    state: &PlayerState,
    blocked: bool,
) -> bool {
    if state.playing && blocked {
        player.pause();
        return true;
    }
    false
}

/// Dart's `_fmt`: whole minutes, then the seconds padded to two digits.
pub fn format_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The player bar's messages: Dart's callbacks.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The disc (`playOrPause`).
    PlayOrPause,
    /// The scrubber's `onChangeStart`.
    ChangeStart,
    /// The scrubber's `onChanged(fraction)`.
    Changed(f64),
    /// The scrubber's `onChangeEnd`: at the fraction it carries (a tap), or,
    /// with `None`, at the fraction the bar shows (a drag's end, which Dart
    /// passed as `widget.fraction`).
    ChangeEnd(Option<f64>),
    /// The poll.
    Tick,
}

/// The playback state Dart's `_TechniqueAudioPlayerState` kept: the player
/// and what its streams last said, plus the bar's drag. The video player
/// runs on it too, and reads what its controls listened to besides
/// (`completed`, `buffering`, `buffer`, `volume`).
pub(super) struct Transport {
    player: Option<Box<dyn MediaPlayer>>,
    playing: bool,
    position: Duration,
    duration: Duration,
    completed: bool,
    buffering: bool,
    buffer: Duration,
    volume: f64,
    dragging: bool,
    drag_value: f64,
    /// The position the player last reported: a poll that reads another is
    /// a position event.
    reported: Duration,
    /// Still loading its duration (poll until it has).
    loading: bool,
    /// Polls still owed to the last command.
    settle: u8,
}

impl Transport {
    pub(super) fn new(player: Option<Box<dyn MediaPlayer>>) -> Transport {
        let loading = player.is_some();
        Transport {
            player,
            playing: false,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            completed: false,
            buffering: false,
            buffer: Duration::ZERO,
            volume: FULL_VOLUME,
            dragging: false,
            drag_value: 0.0,
            reported: Duration::ZERO,
            loading,
            settle: 0,
        }
    }

    pub(super) fn has_player(&self) -> bool {
        self.player.is_some()
    }

    pub(super) fn playing(&self) -> bool {
        self.playing
    }

    pub(super) fn needs_tick(&self) -> bool {
        self.player.is_some() && (self.playing || self.loading || self.settle > 0)
    }

    /// Reads the player as Dart's listeners did; returns the position to
    /// report (`onPosition`), if any.
    pub(super) fn poll(&mut self, play_blocked: bool) -> Option<Duration> {
        let player = self.player.as_mut()?;
        let mut state = player.state();
        if pause_while_play_blocked(player.as_mut(), &state, play_blocked) {
            state = player.state();
        }
        self.playing = state.playing;
        self.duration = state.duration;
        self.loading = state.loading;
        self.completed = state.completed;
        self.buffering = state.buffering;
        self.buffer = state.buffer;
        self.volume = state.volume;
        self.settle = self.settle.saturating_sub(1);
        if state.position == self.reported {
            return None;
        }
        self.reported = state.position;
        if self.dragging {
            return None;
        }
        self.position = state.position;
        // Only drive the analysis playhead while actually playing — the
        // initial paused 0 (and seeks issued FROM the analysis) must not
        // stomp the playhead or loop back.
        self.playing.then_some(state.position)
    }

    /// The video frame due now (video players).
    pub(super) fn frame(&mut self) -> Option<VideoFrame> {
        self.player.as_mut()?.frame()
    }

    /// A command went to the player: poll until it shows, and take its
    /// `playing` and `volume` at once (media_kit reported them
    /// synchronously).
    fn commanded(&mut self) {
        self.settle = SETTLE_TICKS;
        if let Some(player) = &self.player {
            let state = player.state();
            self.playing = state.playing;
            self.volume = state.volume;
        }
    }

    /// What the video controls show.
    pub(super) fn playback(&self) -> Playback {
        Playback {
            playing: self.playing,
            completed: self.completed,
            position: self.position,
            duration: self.duration,
            buffer: self.buffer,
            buffering: self.buffering,
            volume: self.volume,
        }
    }

    /// The player's state now (not the last poll's), if there is a player.
    pub(super) fn current(&self) -> Option<PlayerState> {
        self.player.as_ref().map(|p| p.state())
    }

    /// `play`.
    pub(super) fn play(&mut self) {
        if let Some(player) = &mut self.player {
            player.play();
            self.commanded();
        }
    }

    /// `playOrPause`, with the take's block: Pause stays live; Play waits
    /// for the take to end.
    pub(super) fn play_or_pause(&mut self, play_blocked: bool) {
        if play_blocked && !self.playing {
            return;
        }
        if let Some(player) = &mut self.player {
            player.play_or_pause();
            self.commanded();
        }
    }

    /// `seek(to)`, the position moved there at once (the seek bar set its
    /// own on release).
    pub(super) fn seek(&mut self, to: Duration) {
        self.position = to;
        if let Some(player) = &mut self.player {
            player.seek(to);
            self.commanded();
        }
    }

    /// `setVolume(volume)`.
    pub(super) fn set_volume(&mut self, volume: f64) {
        if let Some(player) = &mut self.player {
            player.set_volume(volume);
            self.commanded();
        }
    }

    /// `_onPauseSignal`: pause where it is.
    pub(super) fn pause(&mut self) {
        if let Some(player) = &mut self.player {
            player.pause();
            self.commanded();
        }
    }

    /// `_onSeekSignal`: the analysis was scrubbed; move playback to match.
    pub(super) fn seek_signal(&mut self, to: Duration) {
        self.position = to;
        if let Some(player) = &mut self.player {
            player.seek(to);
            self.commanded();
        }
    }

    /// Handles a bar message; returns the position to report, if any.
    pub(super) fn update(&mut self, message: Message, play_blocked: bool) -> Option<Duration> {
        match message {
            Message::PlayOrPause => {
                // Pause stays live; Play waits for the take to end (a press
                // queued before the block arrives after it).
                if play_blocked && !self.playing {
                    return None;
                }
                if let Some(player) = &mut self.player {
                    player.play_or_pause();
                    self.commanded();
                }
                None
            }
            Message::ChangeStart => {
                let (_, _, fraction) = self.shown();
                self.dragging = true;
                self.drag_value = fraction;
                None
            }
            Message::Changed(v) => {
                self.drag_value = v;
                None
            }
            Message::ChangeEnd(v) => {
                let v = v.unwrap_or_else(|| self.shown().2);
                self.dragging = false;
                self.seek_to(v)
            }
            Message::Tick => self.poll(play_blocked),
        }
    }

    /// `_seekTo`: a manual scrub.
    fn seek_to(&mut self, fraction: f64) -> Option<Duration> {
        let total = self.duration.as_millis() as u64;
        if total == 0 {
            return None;
        }
        let target = Duration::from_millis((fraction * total as f64).round() as u64);
        // Optimistically move the readout to the target so the thumb stays
        // where it was released instead of bouncing back to a stale
        // position event.
        self.position = target;
        if let Some(player) = &mut self.player {
            player.seek(target);
            self.commanded();
        }
        // A manual scrub is a deliberate move → always push it to the
        // analysis, even when paused.
        Some(target)
    }

    /// What the bar shows: the position (ms), the duration (ms), and the
    /// fraction played.
    fn shown(&self) -> (u64, u64, f64) {
        let total = self.duration.as_millis() as u64;
        let pos = if self.dragging {
            (self.drag_value * total as f64).round() as u64
        } else {
            (self.position.as_millis() as u64).min(total)
        };
        let fraction = if total == 0 {
            0.0
        } else {
            (pos as f64 / total as f64).clamp(0.0, 1.0)
        };
        (pos, total, fraction)
    }

    /// The bar: the disc, the scrubber and the readout.
    pub(super) fn bar(&self, play_blocked: bool) -> Element<'_, Message> {
        let (pos, _, fraction) = self.shown();
        let readout = format!(
            "{} / {}",
            format_clock(Duration::from_millis(pos)),
            format_clock(self.duration)
        );
        row![
            play_button(
                self.playing,
                (!(play_blocked && !self.playing)).then_some(Message::PlayOrPause),
            ),
            Space::new().width(6),
            scrubber(fraction as f32),
            Space::new().width(10),
            studio_text(readout)
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_SECONDARY)
                .wrapping(Wrapping::None)
                .width(READOUT_WIDTH)
                .align_x(Horizontal::Right),
        ]
        .align_y(Vertical::Center)
        .into()
    }
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("has_player", &self.player.is_some())
            .field("playing", &self.playing)
            .field("position", &self.position)
            .field("duration", &self.duration)
            .field("dragging", &self.dragging)
            .finish_non_exhaustive()
    }
}

/// `TechniqueAudioPlayer`'s state (see the module docs).
#[derive(Debug)]
pub struct TechniqueAudioPlayer {
    transport: Transport,
    /// The clip's bytes are not here (or this build plays nothing): the
    /// unavailable row, with nothing to press.
    unavailable: bool,
}

impl TechniqueAudioPlayer {
    /// `initState`: builds the player and opens the clip at `path` paused,
    /// unless the clip is not on this device. A clip that fails to open
    /// drops (disposes) the player it built and leaves a static 0:00
    /// readout, as Dart's catch did; a build with no playback at all shows
    /// the clip unavailable.
    pub fn new(path: &str, players: &dyn MediaPlayerFactory) -> TechniqueAudioPlayer {
        if is_unavailable_path(path) {
            return TechniqueAudioPlayer {
                transport: Transport::new(None),
                unavailable: true,
            };
        }
        let opened = players.create(MediaKind::Audio).and_then(|mut player| {
            // Open paused so the duration loads up front and a scrub can
            // seek before the first play. A failure drops the player here.
            player.open(path, false)?;
            Ok(player)
        });
        let mut audio = match opened {
            Ok(player) => TechniqueAudioPlayer {
                transport: Transport::new(Some(player)),
                unavailable: false,
            },
            Err(MediaPlayerError::Unavailable) => TechniqueAudioPlayer {
                transport: Transport::new(None),
                unavailable: true,
            },
            Err(_) => TechniqueAudioPlayer {
                transport: Transport::new(None),
                unavailable: false,
            },
        };
        let _ = audio.transport.poll(false);
        audio
    }

    /// Whether the clip shows as unavailable.
    pub fn is_unavailable(&self) -> bool {
        self.unavailable
    }

    /// Whether it holds a player.
    pub fn has_player(&self) -> bool {
        self.transport.has_player()
    }

    /// Whether the player reports playing.
    pub fn is_playing(&self) -> bool {
        self.transport.playing
    }

    /// The position the readout shows.
    pub fn position(&self) -> Duration {
        self.transport.position
    }

    /// The duration the readout shows.
    pub fn duration(&self) -> Duration {
        self.transport.duration
    }

    /// Handles a message; returns the position to report to the analysis
    /// (Dart's `onPosition`), if any.
    pub fn update(&mut self, message: Message, play_blocked: bool) -> Option<Duration> {
        self.transport.update(message, play_blocked)
    }

    /// `seekSignal`: the analysis was scrubbed to `to`.
    pub fn seek_signal(&mut self, to: Duration) {
        self.transport.seek_signal(to);
    }

    /// `pauseSignal`: a take is arming; pause where it is.
    pub fn pause_signal(&mut self) {
        self.transport.pause();
    }

    /// Whether the host should poll it now.
    pub fn needs_tick(&self) -> bool {
        self.transport.needs_tick()
    }

    /// The poll, while it needs one.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.needs_tick() {
            iced::time::every(AUDIO_POLL_INTERVAL).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    pub fn view(&self, play_blocked: bool) -> Element<'_, Message> {
        if self.unavailable {
            return unavailable_clip();
        }
        container(self.transport.bar(play_blocked))
            .width(Length::Fill)
            .height(HEIGHT)
            .padding(Padding {
                top: 0.0,
                right: 10.0,
                bottom: 0.0,
                left: 4.0,
            })
            .align_y(Vertical::Center)
            .style(player_well)
            .into()
    }
}

/// The player's well: `Studio::WELL`, radius 6, an inset `BORDER_STRONG`
/// outline.
fn player_well(_: &Theme) -> iced::widget::container::Style {
    boxed(
        Some(Studio::WELL),
        radius::WELL,
        Some(Studio::BORDER_STRONG),
    )
}

/// The player row for a clip whose file is not on this device: same
/// footprint as the live player, with nothing to press.
pub fn unavailable_clip<'a, M: 'a>() -> Element<'a, M> {
    container(
        row![
            icons::icon(Icon::FileX, 13.0, Studio::TEXT_FAINT),
            Space::new().width(8),
            studio_text(UNAVAILABLE_CLIP_MESSAGE)
                .size(type_scale::BODY)
                .line_height(iced::Pixels(type_scale::LINE))
                .color(Studio::TEXT_FAINT)
                .width(Length::Fill)
                .ellipsis(1),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(HEIGHT)
    .padding([0, 10])
    .align_y(Vertical::Center)
    .style(player_well)
    .into()
}

// ------------------------------------------------------- the play button

/// The play/pause button: a 24 px icon button (`DESIGN.md`) with the filled
/// play or pause glyph at 12 px and a "Play" / "Pause" tooltip; `on_tap:
/// None` shows it off (`TEXT_DISABLED`, no press).
pub fn play_button<'a, M: Clone + 'a>(playing: bool, on_tap: Option<M>) -> Element<'a, M> {
    let (glyph, tip) = if playing {
        (glyphs::PAUSE, "Pause")
    } else {
        (glyphs::PLAY, "Play")
    };
    match on_tap {
        Some(on_tap) => icon_button(glyph, on_tap)
            .panel()
            .icon_size(PLAY_GLYPH)
            .tooltip(tip)
            .into(),
        // An icon button with no press: its glyph in `TEXT_DISABLED`.
        None => labelled(
            container(line_icon(glyph, PLAY_GLYPH, Studio::TEXT_DISABLED))
                .center(height::PANEL_BUTTON),
            tip,
        ),
    }
}

/// The play and pause glyphs' box.
const PLAY_GLYPH: f32 = 12.0;

// --------------------------------------------------------- the scrubber

/// The scrubber's height.
const SCRUBBER_HEIGHT: f32 = 24.0;
/// The track's thickness (`DESIGN.md`, Slider).
const TRACK: f32 = 2.0;
/// The hover thumb's diameter.
const THUMB: f32 = 10.0;

/// A mouse's horizontal-drag slop (`kPrecisePointerHitSlop`): past it, a
/// press is a drag.
const DRAG_SLOP: f32 = 1.0;
/// `kTouchSlop`: a press that wanders further is no tap.
const TAP_SLOP: f32 = 18.0;
/// `kPressTimeout`: a press held this long reports its tap-down even if a
/// drag follows.
const PRESS_TIMEOUT: Duration = Duration::from_millis(100);

/// The fraction of a `width`-wide bar at `dx` from its left edge.
pub fn fraction_at(dx: f32, width: f32) -> f64 {
    if width <= 0.0 || !dx.is_finite() {
        return 0.0;
    }
    f64::from(dx / width).clamp(0.0, 1.0)
}

/// Where the hover thumb's left edge sits for `fraction` of a
/// `width`-wide bar: centred on the fill's end, clamped to stay fully on
/// the track at both ends.
pub fn thumb_left(fraction: f32, width: f32) -> f32 {
    (fraction * width - THUMB / 2.0).clamp(0.0, (width - THUMB).max(0.0))
}

/// One press on the bar, until its release.
#[derive(Debug, Clone, Copy)]
struct Press {
    at: Point,
    left: f32,
    width: f32,
    time: Instant,
    last_x: f32,
    /// Horizontal travel so far (signed, as the drag recognizer sums it).
    dx: f32,
    /// Wandered past the tap slop: no tap.
    tap_cancelled: bool,
    /// The tap-down already fired (held past the press timeout).
    tapped: bool,
    dragging: bool,
}

#[derive(Debug, Default)]
struct ScrubState {
    press: Option<Press>,
}

/// Dart's tap-down: `onChangeStart`, `onChanged(f)`, `onChangeEnd(f)` at the
/// press.
fn tap_down(press: &mut Press, shell: &mut iced::advanced::Shell<'_, Message>) {
    press.tapped = true;
    let f = fraction_at(press.at.x - press.left, press.width);
    shell.publish(Message::ChangeStart);
    shell.publish(Message::Changed(f));
    shell.publish(Message::ChangeEnd(Some(f)));
}

/// `_Scrubber`: a thin draggable progress bar (the thumb appears on hover),
/// 24 px tall and as wide as it is given. A tap seeks to where it landed; a
/// horizontal drag follows the pointer and seeks on release.
fn scrubber<'a>(fraction: f32) -> Element<'a, Message> {
    let painter = canvas_widget(ScrubberPainter { fraction })
        .width(Length::Fill)
        .height(SCRUBBER_HEIGHT);
    Wrap::<Message, ScrubState>::new(
        painter,
        Box::new(|state, event, bounds, cursor, shell| {
            let now = Instant::now();
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(p) = cursor.position_over(bounds) {
                        state.press = Some(Press {
                            at: p,
                            left: bounds.x,
                            width: bounds.width,
                            time: now,
                            last_x: p.x,
                            dx: 0.0,
                            tap_cancelled: false,
                            tapped: false,
                            dragging: false,
                        });
                        shell.capture_event();
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    let Some(press) = state.press.as_mut() else {
                        return;
                    };
                    if press.dragging {
                        shell.publish(Message::Changed(fraction_at(
                            position.x - press.left,
                            press.width,
                        )));
                        return;
                    }
                    // A press held past the timeout reported its tap-down
                    // then, before anything that follows.
                    if !press.tapped
                        && !press.tap_cancelled
                        && now.duration_since(press.time) >= PRESS_TIMEOUT
                    {
                        tap_down(press, shell);
                    }
                    press.dx += position.x - press.last_x;
                    press.last_x = position.x;
                    if press.at.distance(*position) > TAP_SLOP {
                        press.tap_cancelled = true;
                    }
                    if press.dx.abs() > DRAG_SLOP {
                        // The drag wins: with DragStartBehavior.start the
                        // move that won it only starts it.
                        press.dragging = true;
                        shell.publish(Message::ChangeStart);
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    let Some(mut press) = state.press.take() else {
                        return;
                    };
                    if press.dragging {
                        shell.publish(Message::ChangeEnd(None));
                    } else if !press.tapped && !press.tap_cancelled {
                        tap_down(&mut press, shell);
                    }
                }
                _ => {}
            }
        }),
    )
    .into()
}

/// Paints the scrubber: the track, the fill up to `fraction`, and the thumb
/// while hovered.
struct ScrubberPainter {
    fraction: f32,
}

impl canvas::Program<Message> for ScrubberPainter {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let width = bounds.width;
        let top = (bounds.height - TRACK) / 2.0;
        let bar = |w: f32| {
            Path::rounded_rectangle(
                Point::new(0.0, top),
                Size::new(w, TRACK),
                (TRACK / 2.0).min(w / 2.0).into(),
            )
        };
        frame.fill(&bar(width), SLIDER_TRACK);
        let filled = self.fraction.clamp(0.0, 1.0) * width;
        if filled > 0.0 {
            frame.fill(&bar(filled), SLIDER_FILL);
        }
        if cursor.is_over(bounds) {
            // The slider's knob: `TEXT_PRIMARY` over a 1 px drop shadow.
            let left = thumb_left(self.fraction.clamp(0.0, 1.0), width);
            let centre = Point::new(left + THUMB / 2.0, bounds.height / 2.0);
            frame.fill(
                &Path::circle(Point::new(centre.x, centre.y + 1.0), THUMB / 2.0),
                Color::from_rgba(0.0, 0.0, 0.0, 0.45),
            );
            frame.fill(&Path::circle(centre, THUMB / 2.0), Studio::TEXT_PRIMARY);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        // A change of cursor is also what redraws the canvas when the
        // pointer enters or leaves it (the thumb).
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests;
