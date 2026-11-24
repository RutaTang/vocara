//! Material 3's `Switch` (Flutter 3.38) as the Dart app themed it: forui's
//! `toApproximateMaterialTheme()` gave it a `SwitchThemeData` whose thumb
//! is the background color in every state and whose track outline is
//! forui's switch track (the border off, the primary on), and the Dart call
//! set the track colors (`activeTrackColor`, `inactiveTrackColor`).
//!
//! What `_MaterialSwitchState` and `_SwitchPainter` do, for a mouse on a
//! desktop (`MaterialTapTargetSize.shrinkWrap`):
//!
//! - It is 60 × 40: the 52 × 32 track with 4 px either side.
//! - A toggle runs its position controller over 300 ms. The thumb slides on
//!   `Curves.easeIn` / `easeOut` until the first toggle from rest, then on
//!   `Curves.easeOutBack` (and its flip back), overshooting the end a
//!   little before settling; the colors follow on `easeOut` / `easeIn`.
//! - The thumb is 16 px off and 24 px on, and on its way it stretches into
//!   a 34 × 22 pill (`transitionalThumbSize`) and back, as the painter's
//!   size `TweenSequence` has it.
//! - A press held past `kPressTimeout` (100 ms; a click shorter than that
//!   barely starts it) runs the reaction over 100 ms on `fastOutSlowIn`:
//!   the thumb grows to 28 px, and without the pointer over it a dark ring
//!   of radius 20 grows from the thumb's centre (the background at
//!   `kRadialReactionAlpha`).
//! - The pointer over it shows the hover ring at once at radius 20, its
//!   color fading in over 50 ms to the primary at 8 %; it goes at once when
//!   the pointer leaves.
//! - A press that moves more than 1 px sideways (a mouse's hit slop) is a
//!   drag (the movement to there swallowed, `DragStartBehavior.start`): the
//!   thumb follows the pointer, linearly, and a release past halfway
//!   toggles, otherwise the thumb goes back. A press that wanders 18 px
//!   (`kTouchSlop`) is no tap.
//!
//! The switch takes no keyboard focus (iced's widgets have none to give it),
//! so it has no focus ring.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

use crate::ui::core::motion::{cubic_curve, Curve, Lerp};
use crate::ui::core::theme::{alpha, Zinc};

/// How long a toggle takes (`_SwitchConfigM3.toggleDuration`).
pub const TOGGLE: Duration = Duration::from_millis(300);

/// How long the press reaction takes (`kRadialReactionDuration`).
pub const REACTION: Duration = Duration::from_millis(100);

/// How long the hover ring's color takes to fade (`_kReactionFadeDuration`).
pub const HOVER_FADE: Duration = Duration::from_millis(50);

/// How long a press is held before it counts as one (`kPressTimeout`).
pub const PRESS_TIMEOUT: Duration = Duration::from_millis(100);

/// The switch's size: the track's 52 plus 4 px either side, by the collapsed
/// height (`switchMinSize.height`).
pub const SIZE: Size = Size::new(60.0, 40.0);

const TRACK: Size = Size::new(52.0, 32.0);
const TRACK_RADIUS: f32 = TRACK.height / 2.0;
/// How far the thumb's centre travels (`_trackInnerLength`).
const TRACK_INNER: f32 = TRACK.width - TRACK.height;
const OUTLINE: f32 = 2.0;
const INACTIVE_RADIUS: f32 = 8.0;
const ACTIVE_RADIUS: f32 = 12.0;
const PRESSED_RADIUS: f32 = 14.0;
const TRANSITIONAL: Size = Size::new(34.0, 22.0);
const SPLASH_RADIUS: f32 = 20.0;
/// A mouse's drag slop (`kPrecisePointerHitSlop`).
const HIT_SLOP: f32 = 1.0;
/// How far a press may wander and still tap (`kTouchSlop`).
const TOUCH_SLOP: f32 = 18.0;

/// A Material 3 switch showing `on`, publishing `toggle` when a tap (or a
/// drag let go past halfway) flips it; its track `active_track` on and
/// `inactive_track` off.
pub fn material_switch<'a, M: Clone + 'a>(
    on: bool,
    toggle: M,
    active_track: Color,
    inactive_track: Color,
) -> Element<'a, M> {
    Element::new(MaterialSwitch {
        on,
        toggle,
        active_track,
        inactive_track,
    })
}

struct MaterialSwitch<M> {
    on: bool,
    toggle: M,
    active_track: Color,
    inactive_track: Color,
}

/// An `AnimationController`'s status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Dismissed,
    Forward,
    Reverse,
    Completed,
}

/// An `AnimationController` from 0 to 1, and the direction its
/// `CurvedAnimation`s pick their curve by (`_curveDirection`).
#[derive(Debug, Clone)]
struct Anim {
    duration: Duration,
    from: f32,
    to: f32,
    /// The run's start and length; none at rest (at `to`).
    run: Option<(Instant, Duration)>,
    /// `_direction`: whether it last ran forward.
    forward: bool,
    curve_forward: Option<bool>,
}

impl Anim {
    fn new(duration: Duration, value: f32) -> Self {
        Self {
            duration,
            from: value,
            to: value,
            run: None,
            forward: true,
            curve_forward: None,
        }
    }

    fn running(&self, now: Instant) -> bool {
        self.run.is_some_and(|(began, length)| now < began + length)
    }

    fn value(&self, now: Instant) -> f32 {
        match self.run {
            Some((began, length)) if self.running(now) => {
                let share =
                    now.saturating_duration_since(began).as_secs_f32() / length.as_secs_f32();
                self.from.lerp(self.to, share.min(1.0))
            }
            _ => self.to,
        }
    }

    fn status(&self, now: Instant) -> Status {
        if self.running(now) {
            return if self.forward {
                Status::Forward
            } else {
                Status::Reverse
            };
        }
        let value = self.value(now);
        if value >= 1.0 {
            Status::Completed
        } else if value <= 0.0 {
            Status::Dismissed
        } else if self.forward {
            Status::Forward
        } else {
            Status::Reverse
        }
    }

    fn at_rest(&self, now: Instant) -> bool {
        matches!(self.status(now), Status::Dismissed | Status::Completed)
    }

    /// Stops a finished run, and forgets the curve direction at rest (the
    /// status listener `CurvedAnimation` keeps).
    fn settle(&mut self, now: Instant) {
        let value = self.value(now);
        if !self.running(now) {
            self.run = None;
            self.from = value;
            self.to = value;
        }
        if self.at_rest(now) {
            self.curve_forward = None;
        }
    }

    /// `forward()` / `reverse()`: runs from where it is to 1 or 0 in the
    /// share of the duration that is left.
    fn animate(&mut self, forward: bool, now: Instant) {
        self.settle(now);
        let value = self.value(now);
        let target = if forward { 1.0 } else { 0.0 };
        self.forward = forward;
        self.from = value;
        self.to = target;
        let share = (target - value).abs();
        if share == 0.0 {
            self.run = None;
            self.curve_forward = None;
        } else {
            self.run = Some((now, self.duration.mul_f32(share)));
            self.curve_forward = self.curve_forward.or(Some(forward));
        }
    }

    /// `value =`: jumps there, still.
    fn set(&mut self, value: f32, now: Instant) {
        self.settle(now);
        let value = value.clamp(0.0, 1.0);
        self.run = None;
        self.from = value;
        self.to = value;
        self.curve_forward = if self.at_rest(now) {
            None
        } else {
            self.curve_forward.or(Some(self.forward))
        };
    }

    /// A `CurvedAnimation` over it with `curve`, and `reverse` running back.
    fn curved(&self, curve: Curve, reverse: Option<Curve>, now: Instant) -> f32 {
        let t = self.value(now);
        let backwards = match self.curve_forward {
            Some(forward) if !self.at_rest(now) => !forward,
            _ => self.status(now) == Status::Reverse,
        };
        let active = match reverse {
            Some(reverse) if backwards => reverse,
            _ => curve,
        };
        if t == 0.0 || t == 1.0 {
            t
        } else {
            active.transform(t)
        }
    }
}

/// A press in progress, as the tap and horizontal drag recognizers see it.
#[derive(Debug, Clone)]
struct Press {
    origin: Point,
    last_x: f32,
    /// The first frame after it (the press timeout counts from there).
    began: Option<Instant>,
    /// `onTapDown` has fired.
    down_sent: bool,
    /// The tap is still in the arena.
    tap: bool,
    /// The drag has won.
    dragging: bool,
    /// The sideways travel so far, signed, before the drag wins.
    travelled: f32,
}

/// What a gesture asks of the animations, applied at the next frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    TapDown,
    TapEnd,
    DragStart,
    DragUpdate(f32),
    DragEnd,
}

struct State {
    /// `widget.value` as last seen.
    on: bool,
    position: Anim,
    /// The position animation's curve and reverse curve.
    position_curve: (Curve, Option<Curve>),
    reaction: Anim,
    hover_fade: Anim,
    /// `_hovering`, as the last frame applied it.
    hovering: bool,
    /// Whether the pointer is over the switch now.
    pointer_over: bool,
    press: Option<Press>,
    steps: Vec<Step>,
    needs_position_animation: bool,
    now: Option<Instant>,
    /// The painter's press-radius bookkeeping (`_stopPressAnimation`,
    /// `_pressedInactiveThumbRadius`, `_pressedActiveThumbRadius`).
    stop_press_animation: bool,
    pressed_inactive: Option<f32>,
    pressed_active: Option<f32>,
}

impl State {
    fn new(on: bool) -> Self {
        let value = if on { 1.0 } else { 0.0 };
        Self {
            on,
            position: Anim::new(TOGGLE, value),
            position_curve: (Curve::EaseIn, Some(Curve::EaseOut)),
            reaction: Anim::new(REACTION, 0.0),
            hover_fade: Anim::new(HOVER_FADE, 0.0),
            hovering: false,
            pointer_over: false,
            press: None,
            steps: Vec::new(),
            needs_position_animation: false,
            now: None,
            stop_press_animation: false,
            pressed_inactive: None,
            pressed_active: None,
        }
    }

    fn position_value(&self, now: Instant) -> f32 {
        let (curve, reverse) = self.position_curve;
        self.position.curved(curve, reverse, now)
    }

    /// A frame at `now`: `didUpdateWidget`, the gestures since the last
    /// frame, the press timeout, the hover, then the painter's bookkeeping.
    /// Whether to publish the toggle (a drag let go past halfway).
    fn frame(&mut self, on: bool, now: Instant) -> bool {
        let mut toggle = false;
        if on != self.on {
            self.on = on;
            let value = self.position_value(now);
            if value == 0.0 || value == 1.0 {
                // `updateCurve`.
                self.position_curve = (Curve::EaseOutBack, Some(Curve::EaseOutBackFlipped));
            }
            self.position.animate(on, now);
        }
        if std::mem::take(&mut self.needs_position_animation) {
            self.position.animate(on, now);
        }
        for step in std::mem::take(&mut self.steps) {
            match step {
                Step::TapDown | Step::DragStart => self.reaction.animate(true, now),
                Step::TapEnd => self.reaction.animate(false, now),
                Step::DragUpdate(dx) => {
                    self.position_curve = (Curve::Linear, None);
                    let value = self.position.value(now) + dx / TRACK_INNER;
                    self.position.set(value, now);
                }
                Step::DragEnd => {
                    if (self.position_value(now) >= 0.5) != on {
                        toggle = true;
                        self.needs_position_animation = true;
                    } else {
                        self.position.animate(on, now);
                    }
                    self.reaction.animate(false, now);
                }
            }
        }
        if let Some(press) = &mut self.press {
            let began = *press.began.get_or_insert(now);
            if press.tap && !press.down_sent && now >= began + PRESS_TIMEOUT {
                press.down_sent = true;
                self.reaction.animate(true, now);
            }
        }
        if self.pointer_over != self.hovering {
            self.hovering = self.pointer_over;
            self.hover_fade.animate(self.hovering, now);
        }
        self.track_press_radii(now);
        toggle
    }

    /// The painter's thumb radii under a press, as it updates them on each
    /// paint.
    fn track_press_radii(&mut self, now: Instant) {
        let reaction = self.reaction.status(now);
        self.stop_press_animation = reaction == Status::Reverse && !self.stop_press_animation;
        if self.stop_press_animation {
            return;
        }
        let pressed = self.reaction.curved(Curve::FastOutSlowIn, None, now);
        if reaction == Status::Completed {
            self.pressed_inactive = Some(INACTIVE_RADIUS.lerp(PRESSED_RADIUS, pressed));
            self.pressed_active = Some(ACTIVE_RADIUS.lerp(PRESSED_RADIUS, pressed));
        }
        let value = self.position_value(now);
        if value == 0.0 {
            self.pressed_inactive = Some(INACTIVE_RADIUS.lerp(PRESSED_RADIUS, pressed));
            self.pressed_active = Some(ACTIVE_RADIUS);
        }
        if value == 1.0 {
            self.pressed_active = Some(ACTIVE_RADIUS.lerp(PRESSED_RADIUS, pressed));
            self.pressed_inactive = Some(INACTIVE_RADIUS);
        }
    }

    fn animating(&self, now: Instant) -> bool {
        self.position.running(now)
            || self.reaction.running(now)
            || self.hover_fade.running(now)
            || self
                .press
                .as_ref()
                .is_some_and(|press| press.tap && !press.down_sent)
    }

    /// What the painter draws at `now`.
    fn paint(&self, now: Instant, active_track: Color, inactive_track: Color) -> Paint {
        let value = self.position_value(now);
        let linear = self.position.value(now);
        let inactive = round(self.pressed_inactive.unwrap_or(INACTIVE_RADIUS));
        let active = round(self.pressed_active.unwrap_or(ACTIVE_RADIUS));
        let thumb = if self.reaction.status(now) == Status::Completed {
            round(PRESSED_RADIUS)
        } else if matches!(
            self.position.status(now),
            Status::Dismissed | Status::Forward
        ) {
            thumb_size_forward(linear, inactive, active)
        } else {
            thumb_size_reverse(linear, inactive, active)
        };
        let colors = self
            .position
            .curved(Curve::EaseOut, Some(Curve::EaseIn), now);
        let track_at = Point::new(
            (SIZE.width - TRACK.width) / 2.0,
            (SIZE.height - TRACK.height) / 2.0,
        );
        let thumb_at = Point::new(
            track_at.x + TRACK_RADIUS - thumb.width / 2.0 + value * TRACK_INNER,
            track_at.y - (thumb.height / 2.0 - TRACK_RADIUS),
        );
        let reaction_at = Point::new(thumb_at.x + thumb.height / 2.0, SIZE.height / 2.0);
        // The thumb's pressed overlay: the thumb color (the background) at
        // `kRadialReactionAlpha`, on or off; hovered, Material 3's default
        // (the primary, or the `onSurface`, at 8 %: both forui's near-white).
        let pressed_ring = alpha(Zinc::BACKGROUND, f32::from(0x1F_u8) / 255.0);
        let hover_ring = alpha(
            if self.on {
                Zinc::PRIMARY
            } else {
                Zinc::FOREGROUND
            },
            0.08,
        );
        let ring = (self.reaction.status(now) != Status::Dismissed
            || self.hover_fade.status(now) != Status::Dismissed)
            .then(|| {
                let fade = self.hover_fade.curved(Curve::FastOutSlowIn, None, now);
                let radius = if self.hovering {
                    SPLASH_RADIUS
                } else {
                    SPLASH_RADIUS * self.reaction.curved(Curve::FastOutSlowIn, None, now)
                };
                (pressed_ring.lerp(hover_ring, fade), radius)
            })
            .filter(|&(_, radius)| radius > 0.0);
        Paint {
            track: Rectangle::new(track_at, TRACK),
            track_color: inactive_track.lerp(active_track, colors),
            outline: Zinc::BORDER.lerp(Zinc::PRIMARY, colors),
            ring: ring.map(|(color, radius)| (reaction_at, radius, color)),
            thumb: Rectangle::new(thumb_at, thumb),
            // The thumb color in every state, blended on the surface.
            thumb_color: Zinc::BACKGROUND,
        }
    }
}

/// What one frame draws, relative to the switch's top left.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Paint {
    track: Rectangle,
    track_color: Color,
    outline: Color,
    /// The reaction ring: its centre, radius and color.
    ring: Option<(Point, f32, Color)>,
    thumb: Rectangle,
    thumb_color: Color,
}

/// `Size.fromRadius`.
fn round(radius: f32) -> Size {
    Size::new(2.0 * radius, 2.0 * radius)
}

fn lerp_size(a: Size, b: Size, t: f32) -> Size {
    Size::new(a.width.lerp(b.width, t), a.height.lerp(b.height, t))
}

/// `Cubic(0.31, 0.00, 0.56, 1.00)`.
fn stretch(t: f32) -> f32 {
    cubic_curve(0.31, 0.0, 0.56, 1.0, t)
}

/// `Cubic(0.20, 0.00, 0.00, 1.00)`.
fn settle(t: f32) -> f32 {
    cubic_curve(0.2, 0.0, 0.0, 1.0, t)
}

/// `FlippedCurve`.
fn flipped(curve: fn(f32) -> f32, t: f32) -> f32 {
    if t == 0.0 || t == 1.0 {
        t
    } else {
        1.0 - curve(1.0 - t)
    }
}

/// The thumb's size going on (the painter's forward `TweenSequence`, by the
/// controller's linear value): stretch to the pill over the first 11 %,
/// settle to the on size by 83 %, hold.
fn thumb_size_forward(t: f32, inactive: Size, active: Size) -> Size {
    if t < 0.11 {
        lerp_size(inactive, TRANSITIONAL, stretch(t / 0.11))
    } else if t < 0.83 {
        lerp_size(TRANSITIONAL, active, settle((t - 0.11) / 0.72))
    } else {
        active
    }
}

/// The thumb's size going off: the same played backwards.
fn thumb_size_reverse(t: f32, inactive: Size, active: Size) -> Size {
    if t < 0.17 {
        inactive
    } else if t < 0.89 {
        lerp_size(inactive, TRANSITIONAL, flipped(settle, (t - 0.17) / 0.72))
    } else {
        lerp_size(TRANSITIONAL, active, flipped(stretch, (t - 0.89) / 0.11))
    }
}

impl<M: Clone> Widget<M, Theme, Renderer> for MaterialSwitch<M> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(SIZE.width), Length::Fixed(SIZE.height))
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.on))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(
            Length::Fixed(SIZE.width),
            Length::Fixed(SIZE.height),
            SIZE,
        ))
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
        match event {
            Event::Window(window::Event::RedrawRequested(at)) => {
                let now = state.now.map_or(*at, |was| was.max(*at));
                state.now = Some(now);
                if state.frame(self.on, now) {
                    shell.publish(self.toggle.clone());
                }
                if state.animating(now) || state.needs_position_animation {
                    shell.request_redraw();
                }
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(at) = cursor.position_over(bounds) {
                    state.press = Some(Press {
                        origin: at,
                        last_x: at.x,
                        began: None,
                        down_sent: false,
                        tap: true,
                        dragging: false,
                        travelled: 0.0,
                    });
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(press) = state.press.take() {
                    if press.dragging {
                        state.steps.push(Step::DragEnd);
                    } else if press.tap {
                        // The arena's sweep hands the tap the win:
                        // `onTapDown` if it has not fired, `onTapUp`, `onTap`.
                        if !press.down_sent {
                            state.steps.push(Step::TapDown);
                        }
                        state.steps.push(Step::TapEnd);
                        shell.publish(self.toggle.clone());
                    }
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position: at }) => {
                if let Some(press) = &mut state.press {
                    let at = *at;
                    let dx = at.x - press.last_x;
                    press.last_x = at.x;
                    if press.dragging {
                        if dx != 0.0 {
                            state.steps.push(Step::DragUpdate(dx));
                        }
                    } else {
                        press.travelled += dx;
                        // The drag wins past its slop, or once the tap has
                        // wandered off and left it alone in the arena.
                        let wins = press.travelled.abs() > HIT_SLOP;
                        let wandered = press.tap && at.distance(press.origin) > TOUCH_SLOP;
                        if wins || wandered {
                            if press.down_sent {
                                // `onTapCancel`.
                                state.steps.push(Step::TapEnd);
                            }
                            press.tap = false;
                            press.dragging = true;
                            state.steps.push(Step::DragStart);
                        }
                    }
                    shell.request_redraw();
                }
            }
            _ => {}
        }
        if let Event::Mouse(_) = event {
            let over = cursor.is_over(bounds);
            if over != state.pointer_over {
                state.pointer_over = over;
                shell.request_redraw();
            }
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
        let paint = match state.now {
            Some(now) => state.paint(now, self.active_track, self.inactive_track),
            // Before its first frame: at rest showing `on`.
            None => {
                State::new(self.on).paint(Instant::now(), self.active_track, self.inactive_track)
            }
        };
        let origin = layout.bounds().position();
        let at = |r: Rectangle| Rectangle::new(origin + Vector::new(r.x, r.y), r.size());
        let quad = |bounds: Rectangle, border: Border| renderer::Quad {
            bounds,
            border,
            shadow: Shadow::default(),
            snap: false,
        };
        renderer.fill_quad(
            quad(
                at(paint.track),
                Border {
                    color: paint.outline,
                    width: OUTLINE,
                    radius: TRACK_RADIUS.into(),
                },
            ),
            Background::Color(paint.track_color),
        );
        if let Some((centre, radius, color)) = paint.ring {
            let ring = Rectangle::new(
                Point::new(centre.x - radius, centre.y - radius),
                Size::new(2.0 * radius, 2.0 * radius),
            );
            renderer.fill_quad(
                quad(
                    at(ring),
                    Border {
                        radius: radius.into(),
                        ..Border::default()
                    },
                ),
                Background::Color(color),
            );
        }
        // A stadium: its ends round to half its height.
        renderer.fill_quad(
            quad(
                at(paint.thumb),
                Border {
                    radius: (paint.thumb.height.min(paint.thumb.width) / 2.0).into(),
                    ..Border::default()
                },
            ),
            Background::Color(paint.thumb_color),
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests;
