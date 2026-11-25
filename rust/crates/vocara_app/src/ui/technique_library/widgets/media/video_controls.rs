//! Port of media_kit_video 2.0.1's desktop video controls
//! (`lib/media_kit_video_controls/src/controls/material_desktop.dart`,
//! cited below by line): what `AdaptiveVideoControls` gave the technique
//! video on macOS (`adaptive.dart`), with the theme the Dart player set
//! (`MaterialDesktopVideoControlsThemeData(modifyVolumeOnScroll: false)`,
//! the same for fullscreen; every other value the default), under the
//! app's Material 3 theme (forui's `toApproximateMaterialTheme`).
//!
//! One widget fills the video and draws and handles all of it; the player
//! state it shows comes in as a [`Playback`] snapshot, and what it does
//! goes out as [`Command`]s. Its own state (what each Flutter `State`
//! held) lives in the widget tree, and its time comes from the frames
//! (`RedrawRequested`): it asks for frames only while something moves,
//! and for one at the hide timer's deadline.
//!
//! # Layout (the default theme, `material_desktop.dart:183-231`)
//!
//! Over the whole video, a bottom gradient (`:702-718`: transparent from
//! the top to halfway down, then to black at 38 % at the bottom edge).
//! Mounted only while shown (`if (mount)`, `:719`), a column the video's
//! height: the empty top bar (56 tall), the empty primary bar (the rest),
//! the seek bar (36 tall, drawn and hit 16 px lower, `:767-772`) and the
//! bottom bar (56 tall), each 16 in from the sides (`:199-215`). The
//! bottom bar's row (`:200-208`, centred on its height): the play/pause
//! button; the volume button (4 px, then a 40 px button, then the slider's
//! strip); the position text; then, at the far end, the fullscreen button
//! (the skip buttons are hidden for a one-clip playlist, `:1215`).
//!
//! # The parts
//!
//! - **Buttons** are Material 3 `IconButton`s (`:1179`, `:1287`, `:1437`):
//!   padding 8 round a 28 px icon (44 × 44; the volume's 22.4 px icon,
//!   `buttonBarButtonSize * 0.8`, makes it the 40 × 40 minimum), in white.
//!   forui's icon-button theme gives them its ghost look: under the pointer
//!   or pressed a rounded square (radius 8) of its `secondary` at once,
//!   over it the `InkWell`'s highlights, white at 8 % hovered (fading over
//!   50 ms) and at 10 % pressed (200 ms). A press is a tap if it stays
//!   within 18 px (`kTouchSlop`) until released.
//! - **Play/pause** (`:1140-1191`) is `AnimatedIcons.play_pause`, its
//!   controller running 200 ms toward pause while playing and back.
//! - **The seek bar** (`:901-1117`): a 3.2 px bar of white at 24 % (5.6
//!   under the pointer; 150 ms), over it the hovered span (from the start
//!   to the pointer), the buffered span, both white at 24 %, and the
//!   played span in red; a red thumb (12 px, growing from nothing over 150
//!   ms while hovered or pressed) whose left edge sits at `(width - 6) ×`
//!   the fraction. A press cancels the hide timer; each move while pressed
//!   seeks to the pointer's fraction, as does the release, which restarts
//!   the timer; while pressed the played span follows the pointer. The
//!   pointer leaving the bar drops the hover (and its span) to nothing. A
//!   completed clip resets the played span to the start until the next
//!   position (`:931-935`).
//! - **The volume button** (`:1371-1533`): the button mutes (to 0, keeping
//!   the volume to restore), restores, or from 0 goes to full
//!   (`media-kit#250`); its icon (`volume_off` at 0, `volume_down` under
//!   50, else `volume_up`) cross-fades over 150 ms. Under the pointer the
//!   group opens its strip from 12 to 82 px and fades the slider in (150
//!   ms each, linear): a Material slider 52 wide (its track the full width,
//!   1.2 px of white at 24 % after the thumb and 3.2 of white before it, a
//!   12 px white thumb, no overlay), which sets the volume where a press
//!   lands (on its release or after 100 ms held) and follows a drag. A
//!   wheel over the group steps the volume by 5 — that handler is the
//!   button's own (`:1420-1433`), which `modifyVolumeOnScroll` never
//!   turned off. Its states (the mute, the saved volume, the hover) are
//!   the button's: they go when the bars unmount.
//! - **The position** (`:1593-1601`): `position / duration` in Inter 12,
//!   white, line height 1, as `DurationExtension.label` writes them
//!   ([`duration_label`]).
//! - **Buffering** (`:817-870`): a 36 px white spinner (Material's
//!   indeterminate `CircularProgressIndicator`) centred in the primary
//!   bar's space, fading in and out over 150 ms; it is not part of the
//!   controls' fade.
//!
//! # Showing and hiding (`:475-515`, `:668-678`)
//!
//! The pointer entering the video, or moving over it with no button held,
//! mounts and shows the controls (fading in over 150 ms, `easeInOut`) and
//! (re)starts a 3 s timer; when it fires, or the pointer leaves, they fade
//! out, and unmount when the fade ends. The cursor stays the arrow
//! (`hideMouseOnControlsRemoval` is false by default); over the buttons,
//! the slider and the seek bar it is the hand.
//!
//! # Double press (`:630-639`)
//!
//! Every tap the controls' own recognizer wins — on the video, the seek
//! bar, the strip, the text, not on a button or the slider — compares its
//! release with the last one (at first, the controls' creation): within
//! 400 ms it toggles fullscreen.
//!
//! # Keyboard (`:526-575`)
//!
//! The default shortcuts are [`shortcut`]'s; the host routes them (the
//! controls' `Focus` held the keyboard). A shortcut's seek goes where
//! mpv took media_kit's absolute seek ([`seek_target`]): J two seconds in
//! lands 8 s before the end. The media keys are bound, but macOS hands
//! them to its Now Playing service, not to the window (neither Flutter's
//! nor winit's), so they never fire, here as in the Dart app.
//!
//! # Not as Flutter had it
//!
//! - iced has no group opacity: the fade multiplies each part's own alpha.
//!   Where parts overlap (a lit button under its icon) a half-faded frame
//!   blends a little differently from Flutter's `AnimatedOpacity`, which
//!   faded the composited bars as one.
//! - Times come from the frames: a hover restarts the 3 s timer, and a tap
//!   is timed for the double press, at the frame that follows it (iced
//!   draws one at once), not at the event.
//! - The volume strip was a horizontal `SingleChildScrollView`; a sideways
//!   scroll over the 12 px closed strip could scroll it. Not ported.
//! - A completed clip's played span resets to the start (`:931-935`) and
//!   stays there until the position moves; the same reset on every seek,
//!   which mpv's next position event undid within a frame, is not drawn.
//! - The position text is Inter 12 at line height 1, its glyphs placed in
//!   the line as cosmic-text centres them, under a pixel below Flutter's
//!   proportional leading.
//! - Nothing here can take Tab's focus (iced has no focus traversal), so
//!   the buttons' focus highlight and the slider's own arrow keys (when
//!   Tab had focused it) do not exist.

use std::time::Duration;

use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::text::{self as core_text, Renderer as _};
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::border::Radius;
use iced::time::Instant;
use iced::widget::canvas::{self, path::Arc, Frame, Path, Stroke};
use iced::{
    gradient, keyboard, mouse, window, Background, Border, Color, Element, Event, Length, Pixels,
    Point, Radians, Rectangle, Renderer, Shadow, Size, Theme,
};

use crate::ui::core::material_icons::{self, MaterialIcon};
use crate::ui::core::motion::{Controller, Curve, Implicit};
use crate::ui::core::theme::{faded, font, Zinc};
use crate::ui::core::widgets::pan_zoom::{self, Scroll};
use crate::ui::core::widgets::studio_hint::arc_at;

// ------------------------------------------------------------- the theme

/// `controlsHoverDuration`: how long the controls stay after the pointer
/// last moved.
pub const CONTROLS_HOVER_DURATION: Duration = Duration::from_secs(3);

/// `controlsTransitionDuration`: the controls' fade, and the buffering
/// indicator's.
pub const CONTROLS_TRANSITION_DURATION: Duration = Duration::from_millis(150);

/// `buttonBarHeight`: the top and bottom bars' height.
pub const BUTTON_BAR_HEIGHT: f32 = 56.0;

/// The bars' and the seek bar's side margin (`topButtonBarMargin`,
/// `bottomButtonBarMargin`, `seekBarMargin`).
pub const BAR_MARGIN: f32 = 16.0;

/// `buttonBarButtonSize`: the play/pause and fullscreen icons.
pub const BUTTON_SIZE: f32 = 28.0;

/// The volume icon (`buttonBarButtonSize * 0.8`).
pub const VOLUME_ICON_SIZE: f32 = BUTTON_SIZE * 0.8;

/// `buttonBarButtonColor`.
pub const BUTTON_COLOR: Color = Color::WHITE;

/// Material 3's `IconButton` padding round the icon.
pub const BUTTON_PADDING: f32 = 8.0;

/// Material 3's `IconButton` minimum side.
pub const BUTTON_MIN_SIZE: f32 = 40.0;

/// forui's icon-button shape (its `style.borderRadius`).
pub const BUTTON_RADIUS: f32 = 8.0;

/// forui's ghost button background, hovered or pressed.
pub const BUTTON_LIT: Color = Zinc::SECONDARY;

/// The `InkWell`'s hovered highlight (`IconButton.styleFrom`'s overlay:
/// the foreground at 8 %).
pub const INK_HOVERED: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.08);

/// The pressed highlight (the foreground at 10 %).
pub const INK_PRESSED: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.10);

/// How long the hovered highlight fades (`InkWell.hoverDuration`).
pub const INK_HOVER_FADE: Duration = Duration::from_millis(50);

/// How long the pressed highlight fades.
pub const INK_PRESS_FADE: Duration = Duration::from_millis(200);

/// The play/pause icon's animation (`AnimationController(duration: 200
/// ms)`).
pub const PLAY_PAUSE_DURATION: Duration = Duration::from_millis(200);

/// `seekBarThumbTransitionDuration`: the seek bar's height and thumb.
pub const SEEK_BAR_TRANSITION: Duration = Duration::from_millis(150);

/// `seekBarHeight`.
pub const SEEK_BAR_HEIGHT: f32 = 3.2;

/// `seekBarHoverHeight`.
pub const SEEK_BAR_HOVER_HEIGHT: f32 = 5.6;

/// `seekBarContainerHeight`: the seek bar's hit area.
pub const SEEK_BAR_CONTAINER_HEIGHT: f32 = 36.0;

/// The seek bar's `Transform.translate` (with a bottom bar).
pub const SEEK_BAR_OFFSET: f32 = 16.0;

/// `seekBarColor`, `seekBarHoverColor`, `seekBarBufferColor`: white at
/// `0x3D`.
pub const SEEK_BAR_COLOR: Color = Color::from_rgba(1.0, 1.0, 1.0, 0x3D as f32 / 255.0);

/// `seekBarPositionColor` and `seekBarThumbColor`.
pub const SEEK_BAR_POSITION_COLOR: Color = Color::from_rgb(1.0, 0.0, 0.0);

/// `seekBarThumbSize`.
pub const SEEK_BAR_THUMB_SIZE: f32 = 12.0;

/// `volumeBarTransitionDuration`: the strip, the slider's fade and the
/// icon's cross-fade.
pub const VOLUME_TRANSITION: Duration = Duration::from_millis(150);

/// The volume slider's width (`sliderWidth ?? 52`).
pub const VOLUME_SLIDER_WIDTH: f32 = 52.0;

/// The strip before and after the slider, and the strip closed.
pub const VOLUME_STRIP_LEAD: f32 = 12.0;
pub const VOLUME_STRIP_TAIL: f32 = 18.0;

/// The strip open: `12 + 52 + 18`.
pub const VOLUME_STRIP_OPEN: f32 = VOLUME_STRIP_LEAD + VOLUME_SLIDER_WIDTH + VOLUME_STRIP_TAIL;

/// The gap before the volume button.
pub const VOLUME_LEAD: f32 = 4.0;

/// The slider's track (`trackHeight`), and how much taller it is before
/// the thumb (`RoundedRectSliderTrackShape`'s `additionalActiveTrackHeight`).
pub const VOLUME_TRACK_HEIGHT: f32 = 1.2;
pub const VOLUME_ACTIVE_EXTRA: f32 = 2.0;

/// `volumeBarColor`.
pub const VOLUME_BAR_COLOR: Color = SEEK_BAR_COLOR;

/// `volumeBarActiveColor`, `volumeBarThumbColor`.
pub const VOLUME_BAR_ACTIVE_COLOR: Color = Color::WHITE;

/// `volumeBarThumbSize`.
pub const VOLUME_THUMB_SIZE: f32 = 12.0;

/// A wheel step over the volume button.
pub const VOLUME_STEP: f64 = 5.0;

/// The position text's size.
pub const POSITION_TEXT_SIZE: f32 = 12.0;

/// The bottom gradient's color at the bottom edge (`0x61000000`), and
/// where it starts (halfway down).
pub const GRADIENT_COLOR: Color = Color::from_rgba(0.0, 0.0, 0.0, 0x61 as f32 / 255.0);
pub const GRADIENT_START: f32 = 0.5;

/// The buffering indicator's side and stroke (Material 3's 2023
/// `CircularProgressIndicator`).
pub const SPINNER_SIZE: f32 = 36.0;
pub const SPINNER_STROKE: f32 = 4.0;

/// Two taps' releases this close toggle fullscreen.
pub const DOUBLE_PRESS: Duration = Duration::from_millis(400);

/// `kTouchSlop`: how far a press may wander and still be a tap.
pub const TOUCH_SLOP: f32 = 18.0;

/// `kPressTimeout`: a press held this long reports its tap-down.
pub const PRESS_TIMEOUT: Duration = Duration::from_millis(100);

/// A mouse's horizontal-drag slop (`kPrecisePointerHitSlop`).
pub const DRAG_SLOP: f32 = 1.0;

/// media_kit's full volume.
pub const FULL_VOLUME: f64 = 100.0;

// --------------------------------------------------------- what goes in/out

/// What the controls show of the player (its state streams' last values).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    pub completed: bool,
    pub position: Duration,
    pub duration: Duration,
    pub buffer: Duration,
    pub buffering: bool,
    /// 0 to 100.
    pub volume: f64,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            playing: false,
            completed: false,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            buffer: Duration::ZERO,
            buffering: false,
            volume: FULL_VOLUME,
        }
    }
}

/// What the controls ask of the player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// The play/pause button (`playOrPause`).
    PlayOrPause,
    /// The seek bar: a move while pressed, or the release.
    Seek(Duration),
    /// The volume button, its slider or the wheel over it.
    SetVolume(f64),
    /// The fullscreen button, or a double press.
    ToggleFullscreen,
}

/// The default `keyboardShortcuts` (`:526-575`, "https://support.google.com/
/// youtube/answer/7631406").
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shortcut {
    /// `mediaPlay`.
    Play,
    /// `mediaPause`.
    Pause,
    /// `mediaPlayPause`, Space.
    PlayOrPause,
    /// `mediaTrackNext`: `player.next()`, which a one-clip playlist ignores.
    Next,
    /// `mediaTrackPrevious`: `player.previous()`, likewise ignored.
    Previous,
    /// J (−10 s), I (+10 s), ← (−2 s), → (+2 s), from the position, in
    /// milliseconds.
    SeekBy(i64),
    /// ↑ (+5), ↓ (−5), clamped to 0–100.
    VolumeBy(f64),
    /// F.
    ToggleFullscreen,
    /// Escape (nothing unless fullscreen).
    ExitFullscreen,
}

/// The shortcut a key press triggers, if any: each a `SingleActivator`,
/// so no Control, Shift, Option or Command may be held, and a repeat
/// triggers it again (`includeRepeats`). Letters match either case (a
/// logical key ignores Caps Lock).
pub fn shortcut(event: &keyboard::Event) -> Option<Shortcut> {
    use keyboard::key::Named;
    use keyboard::Key;
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };
    if modifiers.control() || modifiers.shift() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    Some(match key {
        Key::Named(Named::MediaPlay) => Shortcut::Play,
        Key::Named(Named::MediaPause) => Shortcut::Pause,
        Key::Named(Named::MediaPlayPause) | Key::Named(Named::Space) => Shortcut::PlayOrPause,
        Key::Named(Named::MediaTrackNext) => Shortcut::Next,
        Key::Named(Named::MediaTrackPrevious) => Shortcut::Previous,
        Key::Named(Named::ArrowLeft) => Shortcut::SeekBy(-2_000),
        Key::Named(Named::ArrowRight) => Shortcut::SeekBy(2_000),
        Key::Named(Named::ArrowUp) => Shortcut::VolumeBy(VOLUME_STEP),
        Key::Named(Named::ArrowDown) => Shortcut::VolumeBy(-VOLUME_STEP),
        Key::Named(Named::Escape) => Shortcut::ExitFullscreen,
        Key::Character(c) if c.eq_ignore_ascii_case("j") => Shortcut::SeekBy(-10_000),
        Key::Character(c) if c.eq_ignore_ascii_case("i") => Shortcut::SeekBy(10_000),
        Key::Character(c) if c.eq_ignore_ascii_case("f") => Shortcut::ToggleFullscreen,
        _ => return None,
    })
}

/// Where a shortcut's `player.seek(position ± delta)` lands. media_kit
/// sent mpv `seek <whole milliseconds / 1000> absolute`, and mpv takes a
/// negative absolute time from the end of the clip: J near the start
/// jumps toward the end (nothing, while the length is unknown). Past the
/// end it stops at the end.
pub fn seek_target(position: Duration, delta_ms: i64, duration: Duration) -> Option<Duration> {
    let micros = position.as_micros() as i128 + i128::from(delta_ms) * 1000;
    // `inMilliseconds`: truncated toward zero.
    let ms = micros / 1000;
    if ms < 0 {
        if duration.is_zero() {
            return None;
        }
        let back = Duration::from_millis(ms.unsigned_abs().min(u64::MAX as u128) as u64);
        return Some(duration.saturating_sub(back));
    }
    let target = Duration::from_millis(ms.min(u64::MAX as i128) as u64);
    Some(if duration.is_zero() {
        target
    } else {
        target.min(duration)
    })
}

/// `DurationExtension.label(reference:)`: `mm:ss`, `hh:mm:ss` once the
/// reference passes an hour, `ddd:hh:mm:ss` once it passes a day; the
/// leading field is the whole count (61 minutes is `61:00` under an hour's
/// reference).
pub fn duration_label(d: Duration, reference: Duration) -> String {
    let secs = d.as_secs();
    let (days, hours, minutes) = (secs / 86_400, secs / 3_600, secs / 60);
    let (h, m, s) = (hours - days * 24, minutes - hours * 60, secs - minutes * 60);
    if reference > Duration::from_secs(86_400) {
        format!("{days:03}:{h:02}:{m:02}:{s:02}")
    } else if reference > Duration::from_secs(3_600) {
        format!("{hours:02}:{m:02}:{s:02}")
    } else {
        format!("{minutes:02}:{s:02}")
    }
}

/// The position text: `position / duration`, each labelled against the
/// duration.
pub fn position_text(position: Duration, duration: Duration) -> String {
    format!(
        "{} / {}",
        duration_label(position, duration),
        duration_label(duration, duration)
    )
}

/// The volume icon for `volume`.
pub fn volume_icon(volume: f64) -> MaterialIcon {
    if volume == 0.0 {
        MaterialIcon::VolumeOff
    } else if volume < 50.0 {
        MaterialIcon::VolumeDown
    } else {
        MaterialIcon::VolumeUp
    }
}

/// `positionPercent` / `bufferPercent`: `part` over `whole` in whole
/// milliseconds, 0 when either is zero.
fn percent(part: Duration, whole: Duration) -> f32 {
    if part.is_zero() || whole.is_zero() {
        return 0.0;
    }
    (part.as_millis() as f64 / whole.as_millis().max(1) as f64).clamp(0.0, 1.0) as f32
}

/// `duration * fraction` (Dart's `Duration * double`: microseconds,
/// rounded).
fn scale(duration: Duration, fraction: f32) -> Duration {
    Duration::from_micros((duration.as_micros() as f64 * f64::from(fraction)).round() as u64)
}

// ---------------------------------------------------------------- geometry

/// Where everything is in the controls' `bounds`, the volume strip
/// `strip` wide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Geometry {
    pub bounds: Rectangle,
    /// The seek bar's hit area (where it is drawn: 16 px below its slot).
    pub seek: Rectangle,
    /// The seek bar's centre line.
    pub seek_y: f32,
    pub play: Rectangle,
    pub volume: Rectangle,
    /// The slider's strip (clipping it).
    pub strip: Rectangle,
    /// The slider's own box.
    pub slider: Rectangle,
    /// The position text's top left.
    pub text: Point,
    pub fullscreen: Rectangle,
    /// The buffering indicator's centre.
    pub spinner: Point,
}

pub(crate) fn geometry(bounds: Rectangle, strip: f32) -> Geometry {
    let Rectangle {
        x,
        y,
        width,
        height,
    } = bounds;
    let bar_top = y + height - BUTTON_BAR_HEIGHT;
    let big = BUTTON_SIZE + 2.0 * BUTTON_PADDING;
    let small = (VOLUME_ICON_SIZE + 2.0 * BUTTON_PADDING).max(BUTTON_MIN_SIZE);
    let centred = |side: f32| bar_top + (BUTTON_BAR_HEIGHT - side) / 2.0;
    let left = x + BAR_MARGIN;
    let play = Rectangle::new(Point::new(left, centred(big)), Size::new(big, big));
    let volume_x = left + big + VOLUME_LEAD;
    let volume = Rectangle::new(
        Point::new(volume_x, centred(small)),
        Size::new(small, small),
    );
    let strip_x = volume_x + small;
    let seek_top = bar_top - SEEK_BAR_CONTAINER_HEIGHT + SEEK_BAR_OFFSET;
    Geometry {
        bounds,
        seek: Rectangle::new(
            Point::new(left, seek_top),
            Size::new(
                (width - 2.0 * BAR_MARGIN).max(0.0),
                SEEK_BAR_CONTAINER_HEIGHT,
            ),
        ),
        seek_y: seek_top + SEEK_BAR_CONTAINER_HEIGHT / 2.0,
        play,
        volume,
        strip: Rectangle::new(
            Point::new(strip_x, bar_top),
            Size::new(strip, BUTTON_BAR_HEIGHT),
        ),
        slider: Rectangle::new(
            Point::new(strip_x + VOLUME_STRIP_LEAD, bar_top),
            Size::new(VOLUME_SLIDER_WIDTH, BUTTON_BAR_HEIGHT),
        ),
        text: Point::new(
            strip_x + strip,
            bar_top + (BUTTON_BAR_HEIGHT - POSITION_TEXT_SIZE) / 2.0,
        ),
        fullscreen: Rectangle::new(
            Point::new(x + width - BAR_MARGIN - big, centred(big)),
            Size::new(big, big),
        ),
        spinner: Point::new(x + width / 2.0, y + height / 2.0),
    }
}

/// What a point is over, topmost first (the bottom bar is hit before the
/// seek bar it overlaps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Button(Button),
    /// The strip, outside the slider.
    Strip,
    Slider,
    Seek,
    Video,
}

/// The controls' buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Play,
    Volume,
    Fullscreen,
}

impl Button {
    const ALL: [Button; 3] = [Button::Play, Button::Volume, Button::Fullscreen];

    fn index(self) -> usize {
        self as usize
    }

    fn rect(self, g: &Geometry) -> Rectangle {
        match self {
            Button::Play => g.play,
            Button::Volume => g.volume,
            Button::Fullscreen => g.fullscreen,
        }
    }
}

fn target(g: &Geometry, at: Point, mounted: bool) -> Option<Target> {
    if !g.bounds.contains(at) {
        return None;
    }
    if !mounted {
        return Some(Target::Video);
    }
    if let Some(button) = Button::ALL.into_iter().find(|b| b.rect(g).contains(at)) {
        return Some(Target::Button(button));
    }
    if g.strip.contains(at) {
        let shown = g.strip.intersection(&g.slider);
        return Some(if shown.is_some_and(|s| s.contains(at)) {
            Target::Slider
        } else {
            Target::Strip
        });
    }
    if g.seek.contains(at) {
        return Some(Target::Seek);
    }
    Some(Target::Video)
}

// ------------------------------------------------------------------- state

/// A two-way `AnimationController` that follows a flag.
#[derive(Debug, Clone)]
struct Toggle {
    on: bool,
    controller: Controller,
}

impl Toggle {
    fn new(duration: Duration, on: bool) -> Self {
        let controller = if on {
            Controller::completed(duration, duration)
        } else {
            Controller::new(duration, duration)
        };
        Toggle { on, controller }
    }

    fn follow(&mut self, on: bool, now: Instant) {
        if on != self.on {
            self.on = on;
            if on {
                self.controller.forward(now);
            } else {
                self.controller.reverse(now);
            }
        }
    }
}

/// An implicit animation follows its target.
fn follow(animation: &mut Implicit<f32>, target: f32, now: Instant) {
    if animation.target() != target {
        animation.retarget(target, now);
    }
}

/// One button's `InkWell` and its states.
#[derive(Debug, Clone)]
struct Ink {
    hovered: bool,
    pressed: bool,
    hover: Toggle,
    press: Toggle,
}

impl Ink {
    fn new() -> Self {
        Ink {
            hovered: false,
            pressed: false,
            hover: Toggle::new(INK_HOVER_FADE, false),
            press: Toggle::new(INK_PRESS_FADE, false),
        }
    }
}

/// A press on a button, until its release.
#[derive(Debug, Clone, Copy)]
struct ButtonPress {
    button: Button,
    at: Point,
}

/// `MaterialDesktopSeekBarState`.
#[derive(Debug, Clone)]
struct SeekBar {
    /// The pointer is over the bar's area (its `MouseRegion`).
    over: bool,
    hover: bool,
    click: bool,
    slider: f32,
    height: Implicit<f32>,
    thumb: Implicit<f32>,
    /// The `completed` it last saw.
    completed: bool,
    /// The position when the clip completed: the played span shows the
    /// start while the position stays there.
    zeroed: Option<Duration>,
}

/// A press on the volume slider (its tap and drag recognizers).
#[derive(Debug, Clone, Copy)]
struct SliderPress {
    start: Point,
    /// The press's frame.
    time: Option<Instant>,
    /// The slider took it (`_active`): by the tap-down or the drag.
    active: bool,
    /// The drag won: moves follow the pointer.
    dragging: bool,
    /// Horizontal travel before the drag won.
    travel: f32,
    /// The pointer's offset to the value (a drag that followed a tap-down
    /// adds its movement to the tapped value).
    offset: f32,
    last_x: f32,
    /// The value last reported in this interaction.
    reported: Option<f64>,
}

/// `MaterialDesktopVolumeButtonState`.
#[derive(Debug, Clone)]
struct VolumeButton {
    hover: bool,
    mute: bool,
    /// `_volume`: the volume to restore.
    saved: f64,
    strip: Implicit<f32>,
    opacity: Implicit<f32>,
    /// The `AnimatedSwitcher`'s children: the one coming in last.
    icons: Vec<(MaterialIcon, Controller)>,
    press: Option<SliderPress>,
}

/// The mounted column (`if (mount)`): its widgets' states, made anew each
/// time it mounts.
#[derive(Debug, Clone)]
struct Bars {
    seek: SeekBar,
    play_pause: Toggle,
    volume: VolumeButton,
    ink: [Ink; 3],
    press: Option<ButtonPress>,
}

impl Bars {
    fn new(playback: &Playback) -> Self {
        let mut icon = Controller::new(VOLUME_TRANSITION, VOLUME_TRANSITION);
        icon.set(1.0);
        Bars {
            seek: SeekBar {
                over: false,
                hover: false,
                click: false,
                slider: 0.0,
                height: Implicit::new(SEEK_BAR_HEIGHT, SEEK_BAR_TRANSITION, Curve::Linear),
                thumb: Implicit::new(0.0, SEEK_BAR_TRANSITION, Curve::Linear),
                completed: playback.completed,
                zeroed: None,
            },
            play_pause: Toggle::new(PLAY_PAUSE_DURATION, playback.playing),
            volume: VolumeButton {
                hover: false,
                mute: false,
                saved: 0.0,
                strip: Implicit::new(VOLUME_STRIP_LEAD, VOLUME_TRANSITION, Curve::Linear),
                opacity: Implicit::new(0.0, VOLUME_TRANSITION, Curve::Linear),
                icons: vec![(volume_icon(playback.volume), icon)],
                press: None,
            },
            ink: [Ink::new(), Ink::new(), Ink::new()],
            press: None,
        }
    }
}

/// The video's tap recognizer: a press it may win.
#[derive(Debug, Clone, Copy)]
struct VideoTap {
    at: Point,
}

/// `_MaterialDesktopVideoControlsState`, and its column's widgets'.
struct State {
    /// The last frame's time.
    now: Option<Instant>,
    /// The pointer is over the controls (their `MouseRegion`).
    over: bool,
    visible: bool,
    opacity: Implicit<f32>,
    /// The fade toward the last target is running (its end unmounts).
    fading: bool,
    bars: Option<Bars>,
    /// `_timer`'s deadline.
    hide_at: Option<Instant>,
    /// (Re)start the timer at the next frame.
    restart: bool,
    /// `last`: the last tap's release (the controls' creation at first).
    last_tap: Option<Instant>,
    tap: Option<VideoTap>,
    tap_up: bool,
    /// The buffering indicator's tween, and when its spinner mounted.
    buffering: Implicit<f32>,
    spinner_since: Option<Instant>,
    /// The mouse buttons held.
    buttons: u8,
    shift: bool,
    /// Where the pointer last moved to.
    cursor: Option<Point>,
}

impl State {
    fn new() -> Self {
        State {
            now: None,
            over: false,
            visible: false,
            opacity: Implicit::new(0.0, CONTROLS_TRANSITION_DURATION, Curve::EaseInOut),
            fading: false,
            bars: None,
            hide_at: None,
            restart: false,
            last_tap: None,
            tap: None,
            tap_up: false,
            buffering: Implicit::new(0.0, CONTROLS_TRANSITION_DURATION, Curve::Linear),
            spinner_since: None,
            buttons: 0,
            shift: false,
            cursor: None,
        }
    }

    fn strip(&self, now: Instant) -> f32 {
        self.bars
            .as_ref()
            .map_or(VOLUME_STRIP_LEAD, |b| b.volume.strip.value(now))
    }

    /// `onEnter` / `onHover`: mount, show, (re)start the timer.
    fn show(&mut self, playback: &Playback) {
        if self.bars.is_none() {
            self.bars = Some(Bars::new(playback));
        }
        self.visible = true;
        self.restart = true;
        self.hide_at = None;
    }

    /// `onExit`: hide, no timer.
    fn hide(&mut self) {
        self.visible = false;
        self.restart = false;
        self.hide_at = None;
    }
}

fn button_bit(button: mouse::Button) -> u8 {
    match button {
        mouse::Button::Left => 1,
        mouse::Button::Right => 2,
        mouse::Button::Middle => 4,
        mouse::Button::Back => 8,
        mouse::Button::Forward => 16,
        mouse::Button::Other(_) => 32,
    }
}

// ------------------------------------------------------------------ widget

/// The controls over a video (see the module docs).
pub struct VideoControls<'a, M> {
    playback: Playback,
    fullscreen: bool,
    enabled: bool,
    radius: f32,
    on_command: Box<dyn Fn(Command) -> M + 'a>,
}

/// The controls showing `playback`, reporting through `on_command`.
pub fn video_controls<'a, M>(
    playback: Playback,
    on_command: impl Fn(Command) -> M + 'a,
) -> VideoControls<'a, M> {
    VideoControls {
        playback,
        fullscreen: false,
        enabled: true,
        radius: 0.0,
        on_command: Box::new(on_command),
    }
}

impl<M> VideoControls<'_, M> {
    /// In the fullscreen view (its button shows `fullscreen_exit`).
    pub fn fullscreen(mut self, fullscreen: bool) -> Self {
        self.fullscreen = fullscreen;
        self
    }

    /// Whether the pointer reaches them (`IgnorePointer(ignoring: !enabled)`).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The radius of the bottom corners the video is clipped to (the
    /// gradient follows it).
    pub fn corner_radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    fn publish(&self, shell: &mut Shell<'_, M>, command: Command) {
        shell.publish((self.on_command)(command));
    }

    /// What the volume button does when pressed (`:1438-1455`).
    fn toggle_mute(&self, volume: &mut VolumeButton, shell: &mut Shell<'_, M>) {
        let current = self.playback.volume;
        if volume.mute {
            self.publish(shell, Command::SetVolume(volume.saved));
            volume.mute = false;
        } else if current == 0.0 {
            volume.saved = FULL_VOLUME;
            self.publish(shell, Command::SetVolume(FULL_VOLUME));
            volume.mute = false;
        } else {
            volume.saved = current;
            self.publish(shell, Command::SetVolume(0.0));
            volume.mute = true;
        }
    }

    /// The slider's `onChanged` for a value at `x` (a fraction of its
    /// width, clamped), when it changed.
    fn slide(
        &self,
        volume: &mut VolumeButton,
        slider: Rectangle,
        x: f32,
        shell: &mut Shell<'_, M>,
    ) {
        let fraction = if slider.width > 0.0 {
            f64::from((x - slider.x) / slider.width).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let value = fraction * FULL_VOLUME;
        let Some(press) = volume.press.as_mut() else {
            return;
        };
        if press.reported == Some(value) {
            return;
        }
        press.reported = Some(value);
        if value != self.playback.volume {
            self.publish(shell, Command::SetVolume(value));
            volume.mute = false;
        }
    }

    /// One frame at `now`: the timers, the taps and every animation.
    fn frame(&self, state: &mut State, g: &Geometry, now: Instant, shell: &mut Shell<'_, M>) {
        let now = state.now.map_or(now, |was| was.max(now));
        state.now = Some(now);
        if state.last_tap.is_none() {
            state.last_tap = Some(now);
        }
        if std::mem::take(&mut state.tap_up) {
            let last = state.last_tap.replace(now).unwrap_or(now);
            if now.saturating_duration_since(last) < DOUBLE_PRESS {
                self.publish(shell, Command::ToggleFullscreen);
            }
        }
        if std::mem::take(&mut state.restart) {
            state.hide_at = Some(now + CONTROLS_HOVER_DURATION);
        }
        if state.hide_at.is_some_and(|at| now >= at) {
            state.hide_at = None;
            state.visible = false;
        }
        // AnimatedOpacity, and its onEnd: unmount once hidden.
        let target = if state.visible { 1.0 } else { 0.0 };
        if state.opacity.target() != target {
            state.opacity.retarget(target, now);
            state.fading = true;
        }
        if state.fading && !state.opacity.is_animating(now) {
            state.fading = false;
            if !state.visible {
                state.bars = None;
            }
        }
        // The buffering indicator's tween; its spinner mounts while shown.
        follow(
            &mut state.buffering,
            if self.playback.buffering { 1.0 } else { 0.0 },
            now,
        );
        if state.buffering.value(now) > 0.0 {
            state.spinner_since.get_or_insert(now);
        } else {
            state.spinner_since = None;
        }
        let mut deadline = state.hide_at;
        if let Some(bars) = state.bars.as_mut() {
            let seek = &mut bars.seek;
            follow(
                &mut seek.height,
                if seek.hover {
                    SEEK_BAR_HOVER_HEIGHT
                } else {
                    SEEK_BAR_HEIGHT
                },
                now,
            );
            follow(
                &mut seek.thumb,
                if seek.hover || seek.click {
                    SEEK_BAR_THUMB_SIZE
                } else {
                    0.0
                },
                now,
            );
            if self.playback.completed != seek.completed {
                seek.completed = self.playback.completed;
                if seek.completed {
                    seek.zeroed = Some(self.playback.position);
                }
            }
            if seek.zeroed.is_some_and(|at| at != self.playback.position) {
                seek.zeroed = None;
            }
            bars.play_pause.follow(self.playback.playing, now);
            let volume = &mut bars.volume;
            follow(
                &mut volume.strip,
                if volume.hover {
                    VOLUME_STRIP_OPEN
                } else {
                    VOLUME_STRIP_LEAD
                },
                now,
            );
            follow(
                &mut volume.opacity,
                if volume.hover { 1.0 } else { 0.0 },
                now,
            );
            let icon = volume_icon(self.playback.volume);
            if volume.icons.last().map(|(i, _)| *i) != Some(icon) {
                for (_, c) in &mut volume.icons {
                    c.reverse(now);
                }
                let mut entering = Controller::new(VOLUME_TRANSITION, VOLUME_TRANSITION);
                entering.forward(now);
                volume.icons.push((icon, entering));
            }
            let last = volume.icons.len() - 1;
            let mut i = 0;
            volume.icons.retain(|(_, c)| {
                let keep = i == last || c.is_animating(now) || c.value(now) > 0.0;
                i += 1;
                keep
            });
            // The slider's tap-down, once a press is held 100 ms.
            let mut tap_down = None;
            if let Some(press) = volume.press.as_mut() {
                let time = *press.time.get_or_insert(now);
                if !press.active {
                    let due = time + PRESS_TIMEOUT;
                    if now >= due {
                        press.active = true;
                        press.offset = 0.0;
                        tap_down = Some(press.start.x);
                    } else {
                        deadline = Some(deadline.map_or(due, |d| d.min(due)));
                    }
                }
            }
            if let Some(x) = tap_down {
                self.slide(volume, g.slider, x, shell);
            }
            for ink in &mut bars.ink {
                ink.hover.follow(ink.hovered, now);
                ink.press.follow(ink.pressed, now);
            }
        }
        if animating(state, now) {
            shell.request_redraw();
        } else if let Some(at) = deadline {
            shell.request_redraw_at(at);
        }
    }

    /// Hover over the column's parts (their `MouseRegion`s), as the mouse
    /// tracker found it.
    fn track(&self, state: &mut State, g: &Geometry, at: Option<Point>) -> bool {
        let Some(bars) = state.bars.as_mut() else {
            return false;
        };
        let target = at.and_then(|p| target(g, p, true));
        let mut changed = false;
        // The seek bar's region: enter and hover put the hover span at the
        // pointer, exit drops it.
        let over_seek = target == Some(Target::Seek);
        if over_seek {
            let p = at.expect("over it");
            let slider = fraction(g.seek, p.x);
            changed |= !bars.seek.hover || bars.seek.slider != slider;
            bars.seek.hover = true;
            bars.seek.slider = slider;
        } else if bars.seek.over {
            bars.seek.hover = false;
            bars.seek.slider = 0.0;
            changed = true;
        }
        bars.seek.over = over_seek;
        let over_volume = matches!(
            target,
            Some(Target::Button(Button::Volume) | Target::Strip | Target::Slider)
        );
        changed |= bars.volume.hover != over_volume;
        bars.volume.hover = over_volume;
        for button in Button::ALL {
            let hovered = target == Some(Target::Button(button));
            let ink = &mut bars.ink[button.index()];
            changed |= ink.hovered != hovered;
            ink.hovered = hovered;
        }
        changed
    }
}

/// The share of `area`'s width at `x`, clamped.
fn fraction(area: Rectangle, x: f32) -> f32 {
    if area.width <= 0.0 {
        return 0.0;
    }
    ((x - area.x) / area.width).clamp(0.0, 1.0)
}

/// Whether anything the controls draw is moving at `now`.
fn animating(state: &State, now: Instant) -> bool {
    if state.opacity.is_animating(now)
        || state.buffering.is_animating(now)
        || state.spinner_since.is_some()
    {
        return true;
    }
    state.bars.as_ref().is_some_and(|b| {
        b.seek.height.is_animating(now)
            || b.seek.thumb.is_animating(now)
            || b.play_pause.controller.is_animating(now)
            || b.volume.strip.is_animating(now)
            || b.volume.opacity.is_animating(now)
            || b.volume.icons.iter().any(|(_, c)| c.is_animating(now))
            || b.ink.iter().any(|i| {
                i.hover.controller.is_animating(now) || i.press.controller.is_animating(now)
            })
    })
}

impl<M> Widget<M, Theme, Renderer> for VideoControls<'_, M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new())
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let now = state.now.unwrap_or_else(Instant::now);
        let g = geometry(bounds, state.strip(now));
        // IgnorePointer: out of the pointer's reach.
        let at = if self.enabled {
            cursor.position()
        } else {
            None
        };
        let inside = at.is_some_and(|p| bounds.contains(p))
            && !matches!(event, Event::Mouse(mouse::Event::CursorLeft));
        // The mouse tracker, on every event and after every frame: enter
        // and exit, then the column's regions.
        if inside != state.over {
            state.over = inside;
            if inside {
                state.show(&self.playback);
            } else {
                state.hide();
            }
            shell.request_redraw();
        }
        if self.track(state, &g, at.filter(|_| inside)) {
            shell.request_redraw();
        }
        let Event::Mouse(mouse_event) = event else {
            match event {
                Event::Window(window::Event::RedrawRequested(now)) => {
                    let g = geometry(bounds, state.strip(*now));
                    self.frame(state, &g, *now, shell);
                }
                Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => state.shift = m.shift(),
                _ => {}
            }
            return;
        };
        match mouse_event {
            mouse::Event::CursorMoved { position } => {
                // Where the pointer is in the layout's coordinates (a
                // scrollable around translates the cursor, not the event),
                // even over a layer above (a drag goes on there).
                let p = cursor.land().position().unwrap_or(*position);
                // A pointer event only when it moved (a hover or a move
                // event: Flutter sent none for a pointer standing still).
                if state.cursor == Some(p) {
                    return;
                }
                state.cursor = Some(p);
                if inside && state.buttons == 0 {
                    // onHover.
                    state.show(&self.playback);
                    shell.request_redraw();
                }
                if let Some(tap) = state.tap {
                    if tap.at.distance(p) > TOUCH_SLOP {
                        state.tap = None;
                    }
                }
                let Some(bars) = state.bars.as_mut() else {
                    return;
                };
                // The seek bar's Listener: a move while pressed seeks.
                if bars.seek.click {
                    bars.seek.hover = true;
                    bars.seek.slider = fraction(g.seek, p.x);
                    let to = scale(self.playback.duration, bars.seek.slider);
                    self.publish(shell, Command::Seek(to));
                    shell.request_redraw();
                }
                if let Some(press) = bars.press {
                    if press.at.distance(p) > TOUCH_SLOP {
                        bars.press = None;
                        bars.ink[press.button.index()].pressed = false;
                        shell.request_redraw();
                    }
                }
                let volume = &mut bars.volume;
                let mut slide_to = None;
                if let Some(press) = volume.press.as_mut() {
                    let dx = p.x - press.last_x;
                    press.last_x = p.x;
                    if press.dragging {
                        slide_to = Some(p.x + press.offset);
                    } else {
                        press.travel += dx;
                        if press.travel.abs() > DRAG_SLOP {
                            press.dragging = true;
                            if press.active {
                                // After the tap-down: the drag adds its
                                // movement to the tapped value.
                                press.offset = press.start.x - p.x;
                            } else {
                                // The drag starts it where it was won.
                                press.active = true;
                                press.offset = 0.0;
                                slide_to = Some(p.x);
                            }
                        }
                    }
                }
                if let Some(x) = slide_to {
                    self.slide(volume, g.slider, x, shell);
                }
            }
            mouse::Event::ButtonPressed(button) => {
                state.buttons |= button_bit(*button);
                let Some(p) = at.filter(|_| inside) else {
                    return;
                };
                let mounted = state.bars.is_some();
                let hit = target(&g, p, mounted);
                let left = *button == mouse::Button::Left;
                match (hit, state.bars.as_mut()) {
                    (Some(Target::Seek), Some(bars)) => {
                        // onSeekStart: the timer stops.
                        state.hide_at = None;
                        state.restart = false;
                        bars.seek.click = true;
                        if left {
                            state.tap = Some(VideoTap { at: p });
                        }
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    (Some(Target::Button(b)), Some(bars)) if left => {
                        bars.press = Some(ButtonPress { button: b, at: p });
                        bars.ink[b.index()].pressed = true;
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    (Some(Target::Slider), Some(bars)) if left => {
                        bars.volume.press = Some(SliderPress {
                            start: p,
                            time: None,
                            active: false,
                            dragging: false,
                            travel: 0.0,
                            offset: 0.0,
                            last_x: p.x,
                            reported: None,
                        });
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    (Some(_), _) if left => {
                        state.tap = Some(VideoTap { at: p });
                    }
                    _ => {}
                }
            }
            mouse::Event::ButtonReleased(button) => {
                state.buttons &= !button_bit(*button);
                let left = *button == mouse::Button::Left;
                if left && state.tap.take().is_some() {
                    state.tap_up = true;
                    shell.request_redraw();
                }
                let Some(bars) = state.bars.as_mut() else {
                    return;
                };
                if bars.seek.click {
                    // onSeekEnd: a new timer; the release seeks.
                    bars.seek.click = false;
                    state.restart = true;
                    let to = scale(self.playback.duration, bars.seek.slider);
                    self.publish(shell, Command::Seek(to));
                    shell.request_redraw();
                }
                if !left {
                    return;
                }
                if let Some(press) = bars.press.take() {
                    bars.ink[press.button.index()].pressed = false;
                    match press.button {
                        Button::Play => self.publish(shell, Command::PlayOrPause),
                        Button::Volume => self.toggle_mute(&mut bars.volume, shell),
                        Button::Fullscreen => self.publish(shell, Command::ToggleFullscreen),
                    }
                    shell.request_redraw();
                }
                let volume = &mut bars.volume;
                if let Some(press) = volume.press {
                    if !press.active {
                        // A tap: its tap-down, at the press.
                        self.slide(volume, g.slider, press.start.x, shell);
                    }
                    volume.press = None;
                }
            }
            mouse::Event::WheelScrolled { delta } => {
                // The volume button's own `onPointerSignal`: a scroll
                // signal's `scrollDelta.dy` (a mouse wheel's, or a precise
                // device's without gesture phases; Shift's axis swap on
                // macOS put back). A trackpad's swipe was a pan-zoom
                // gesture it never saw, nor the momentum after it.
                let over = state.bars.as_ref().is_some_and(|b| b.volume.hover);
                let dy = pan_zoom::scroll_delta(*delta, state.shift).y;
                if over && dy != 0.0 && pan_zoom::current() == Scroll::Signal {
                    let step = if dy < 0.0 { VOLUME_STEP } else { -VOLUME_STEP };
                    let volume = (self.playback.volume + step).clamp(0.0, FULL_VOLUME);
                    self.publish(shell, Command::SetVolume(volume));
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if !self.enabled {
            return mouse::Interaction::None;
        }
        let state = tree.state.downcast_ref::<State>();
        let now = state.now.unwrap_or_else(Instant::now);
        let g = geometry(layout.bounds(), state.strip(now));
        match cursor
            .position()
            .and_then(|p| target(&g, p, state.bars.is_some()))
        {
            Some(Target::Button(_) | Target::Slider | Target::Seek) => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Idle,
            None => mouse::Interaction::None,
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let now = state.now.unwrap_or_else(Instant::now);
        let g = geometry(bounds, state.strip(now));
        let o = state.opacity.value(now);
        renderer.with_layer(bounds, |renderer| {
            if o > 0.0 {
                self.draw_gradient(renderer, bounds, o);
            }
            if let (Some(bars), true) = (&state.bars, o > 0.0) {
                self.draw_bars(renderer, &g, bars, now, o);
            }
        });
        if let (Some(bars), true) = (&state.bars, o > 0.0) {
            if let Some(clip) = g.strip.intersection(&bounds) {
                let shown = o * bars.volume.opacity.value(now);
                if shown > 0.0 {
                    renderer.with_layer(clip, |renderer| {
                        self.draw_slider(renderer, &g, shown);
                    });
                }
            }
        }
        if let Some(since) = state.spinner_since {
            let shown = state.buffering.value(now);
            if shown > 0.0 {
                renderer.with_layer(bounds, |renderer| {
                    draw_spinner(renderer, &g, now.saturating_duration_since(since), shown);
                });
            }
        }
    }
}

// ----------------------------------------------------------------- drawing

/// A rounded rectangle in `color` (nothing when clear or empty).
fn fill(renderer: &mut Renderer, bounds: Rectangle, radius: impl Into<Radius>, color: Color) {
    if color.a <= 0.0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    renderer.fill_quad(
        Quad {
            bounds,
            border: Border {
                radius: radius.into(),
                ..Border::default()
            },
            shadow: Shadow::default(),
            snap: false,
        },
        Background::Color(color),
    );
}

impl<M> VideoControls<'_, M> {
    /// The bottom gradient, faded to `o`.
    fn draw_gradient(&self, renderer: &mut Renderer, bounds: Rectangle, o: f32) {
        let clear = Color {
            a: 0.0,
            ..GRADIENT_COLOR
        };
        let gradient = gradient::Linear::new(Radians(std::f32::consts::PI))
            .add_stop(GRADIENT_START, clear)
            .add_stop(1.0, faded(GRADIENT_COLOR, o));
        renderer.fill_quad(
            Quad {
                bounds,
                border: Border {
                    radius: Radius {
                        top_left: 0.0,
                        top_right: 0.0,
                        bottom_right: self.radius,
                        bottom_left: self.radius,
                    },
                    ..Border::default()
                },
                shadow: Shadow::default(),
                snap: false,
            },
            Background::Gradient(gradient.into()),
        );
    }

    /// The seek bar and the bottom bar, faded to `o`.
    fn draw_bars(&self, renderer: &mut Renderer, g: &Geometry, bars: &Bars, now: Instant, o: f32) {
        let playback = &self.playback;
        // The seek bar.
        let seek = &bars.seek;
        let width = g.seek.width;
        let height = seek.height.value(now);
        let row = |w: f32| {
            Rectangle::new(
                Point::new(g.seek.x, g.seek_y - height / 2.0),
                Size::new(w.max(0.0), height),
            )
        };
        let position = if seek.zeroed.is_some() {
            Duration::ZERO
        } else {
            playback.position
        };
        let played = if seek.click {
            seek.slider
        } else {
            percent(position, playback.duration)
        };
        fill(renderer, row(width), 0.0, faded(SEEK_BAR_COLOR, o));
        fill(
            renderer,
            row(width * seek.slider),
            0.0,
            faded(SEEK_BAR_COLOR, o),
        );
        fill(
            renderer,
            row(width * percent(playback.buffer, playback.duration)),
            0.0,
            faded(SEEK_BAR_COLOR, o),
        );
        fill(
            renderer,
            row(width * played),
            0.0,
            faded(SEEK_BAR_POSITION_COLOR, o),
        );
        let thumb = seek.thumb.value(now);
        if thumb > 0.0 {
            let left = g.seek.x + (width - SEEK_BAR_THUMB_SIZE / 2.0) * played;
            fill(
                renderer,
                Rectangle::new(
                    Point::new(left, g.seek_y - thumb / 2.0),
                    Size::new(thumb, thumb),
                ),
                (SEEK_BAR_THUMB_SIZE / 2.0).min(thumb / 2.0),
                faded(SEEK_BAR_POSITION_COLOR, o),
            );
        }
        // The buttons' backgrounds and highlights.
        for button in Button::ALL {
            let rect = button.rect(g);
            let ink = &bars.ink[button.index()];
            if ink.hovered || ink.pressed {
                fill(renderer, rect, BUTTON_RADIUS, faded(BUTTON_LIT, o));
            }
            let hover = ink.hover.controller.value(now);
            fill(renderer, rect, BUTTON_RADIUS, faded(INK_HOVERED, o * hover));
            let press = ink.press.controller.value(now);
            fill(renderer, rect, BUTTON_RADIUS, faded(INK_PRESSED, o * press));
        }
        // The position.
        renderer.fill_text(
            core_text::Text {
                content: position_text(playback.position, playback.duration),
                bounds: Size::new(f32::INFINITY, POSITION_TEXT_SIZE),
                size: Pixels(POSITION_TEXT_SIZE),
                line_height: core_text::LineHeight::Relative(1.0),
                font: font::REGULAR,
                align_x: core_text::Alignment::Left,
                align_y: Vertical::Top,
                shaping: core_text::Shaping::Advanced,
                wrapping: core_text::Wrapping::None,
            },
            g.text,
            faded(BUTTON_COLOR, o),
            g.bounds,
        );
        // The icons.
        let mut frame = Frame::with_bounds(renderer, g.bounds);
        let inset = |rect: Rectangle, side: f32| {
            Point::new(
                rect.x + (rect.width - side) / 2.0,
                rect.y + (rect.height - side) / 2.0,
            )
        };
        let progress = bars.play_pause.controller.value(now);
        for (path, opacity) in
            material_icons::play_pause(progress, inset(g.play, BUTTON_SIZE), BUTTON_SIZE)
        {
            frame.fill(&path, faded(BUTTON_COLOR, o * opacity));
        }
        for (icon, controller) in &bars.volume.icons {
            let shown = controller.value(now);
            if shown > 0.0 {
                let path = icon.path(inset(g.volume, VOLUME_ICON_SIZE), VOLUME_ICON_SIZE);
                frame.fill(&path, faded(BUTTON_COLOR, o * shown));
            }
        }
        let icon = if self.fullscreen {
            MaterialIcon::FullscreenExit
        } else {
            MaterialIcon::Fullscreen
        };
        frame.fill(
            &icon.path(inset(g.fullscreen, BUTTON_SIZE), BUTTON_SIZE),
            faded(BUTTON_COLOR, o),
        );
        renderer.draw_geometry(frame.into_geometry());
    }

    /// The volume slider, faded to `o` (its strip clips it).
    fn draw_slider(&self, renderer: &mut Renderer, g: &Geometry, o: f32) {
        let slider = g.slider;
        let value = (self.playback.volume / FULL_VOLUME).clamp(0.0, 1.0) as f32;
        let cy = slider.y + slider.height / 2.0;
        let thumb = slider.x + value * slider.width;
        let half = VOLUME_TRACK_HEIGHT / 2.0;
        let right = slider.x + slider.width;
        if thumb < right - half {
            fill(
                renderer,
                Rectangle::new(
                    Point::new(thumb - half, cy - half),
                    Size::new(right - (thumb - half), VOLUME_TRACK_HEIGHT),
                ),
                half,
                faded(VOLUME_BAR_COLOR, o),
            );
        }
        if thumb > slider.x + half {
            let active = VOLUME_TRACK_HEIGHT + VOLUME_ACTIVE_EXTRA;
            fill(
                renderer,
                Rectangle::new(
                    Point::new(slider.x, cy - active / 2.0),
                    Size::new(thumb + half - slider.x, active),
                ),
                active / 2.0,
                faded(VOLUME_BAR_ACTIVE_COLOR, o),
            );
        }
        let r = VOLUME_THUMB_SIZE / 2.0;
        fill(
            renderer,
            Rectangle::new(Point::new(thumb - r, cy - r), Size::new(2.0 * r, 2.0 * r)),
            r,
            faded(VOLUME_BAR_ACTIVE_COLOR, o),
        );
    }
}

/// The buffering indicator `elapsed` into its spin, at `opacity`.
fn draw_spinner(renderer: &mut Renderer, g: &Geometry, elapsed: Duration, opacity: f32) {
    let mut frame = Frame::with_bounds(renderer, g.bounds);
    let (start, sweep) = arc_at(elapsed.as_secs_f32() * 1000.0);
    let arc = Path::new(|b| {
        b.arc(Arc {
            center: g.spinner,
            radius: SPINNER_SIZE / 2.0,
            start_angle: Radians(start),
            end_angle: Radians(start + sweep),
        })
    });
    frame.stroke(
        &arc,
        Stroke::default()
            .with_color(faded(Color::WHITE, opacity))
            .with_width(SPINNER_STROKE)
            .with_line_cap(canvas::LineCap::Square),
    );
    renderer.draw_geometry(frame.into_geometry());
}

impl<'a, M: 'a> From<VideoControls<'a, M>> for Element<'a, M> {
    fn from(controls: VideoControls<'a, M>) -> Self {
        Element::new(controls)
    }
}

#[cfg(test)]
mod tests;
