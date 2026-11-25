//! Port of Flutter's `Dismissible` as a `SnackBar` wraps itself in one
//! (`material_snack_bar.dart`: `direction: DismissDirection.down`,
//! `resizeDuration: null`), with the vertical drag recognizer, velocity
//! tracker and `AnimationController` it runs on, for a mouse and a
//! trackpad.
//!
//! - A primary press where the widget takes hits starts a drag at once: its
//!   `VerticalDragGestureRecognizer` is alone in the gesture arena (nothing
//!   else on a snack bar recognizes a gesture, and the snack bar takes the
//!   hit before the page beneath), so the arena hands it the pointer as it
//!   closes, before any movement (`DragStartBehavior.start`, nothing
//!   swallowed). Another button pressed meanwhile changes the pointer's
//!   buttons, which ends the drag.
//! - Dragged, the content follows the pointer down, and only down: a move
//!   that would take the drag above where it started is dropped
//!   (`_handleDragUpdate`). How far down, over the widget's height, is the
//!   move controller's value; the content is drawn that share of its height
//!   lower (`SlideTransition`).
//! - Let go, a fling (the pointer's velocity, `VelocityTracker`'s least
//!   squares over its last 100 ms, at least 700 px/s along the drag,
//!   `_kMinFlingVelocity`) flies it out on `AnimationController.fling`'s
//!   spring at a three-hundredth of that velocity, or back if flung up;
//!   short of a fling, past 40 % of its height (`_kDismissThreshold`) it
//!   slides the rest of the way out, linearly over that share of 200 ms
//!   (`movementDuration`), and back otherwise.
//! - All the way out (and not held by a drag) it reports `on_dismissed`, once
//!   (no `resizeDuration`: no collapse first). A press while it slides
//!   catches it where it is, and the drag goes on from there.
//! - A trackpad's two-finger swipe that starts over it drags it too: macOS
//!   gave Flutter such a scroll as a pan-zoom gesture
//!   ([`pan_zoom`](super::pan_zoom)), which the drag recognizer takes as
//!   it takes a press (it accepts every device), alone in the arena at the
//!   gesture's start. The content follows the pan down; the fingers
//!   lifting end the drag as a release does, its fling judged on the pan's
//!   samples at their events' times, past `kTouchSlop` (18 px, a
//!   trackpad's hit slop, where a mouse's is 1). The momentum macOS sends
//!   after it moves nothing (Flutter got none), and nothing beneath sees
//!   it. A gesture that started elsewhere stays the page's.
//!
//! The slide is the widget's own: it keeps it in the widget tree, takes its
//! time from `RedrawRequested` and asks for frames only while it moves. Its
//! owner learns of the dismissal from the message alone, when the slide has
//! completed, as Dart's `onDismissed` came: the model waits on nothing.
//! iced's events carry no time stamps; the drag samples each at the time it
//! is handled.
//!
//! Where it takes hits it takes the wheel too: the page beneath does not
//! scroll under it (Flutter's hit test stopped at the snack bar, so no
//! scrollable beneath saw the pointer signal).

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::{
    mouse, window, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Theme, Vector,
};

use super::pan_zoom::{self, Held, Scroll, TOUCH_SLOP};

// ----------------------------------------------------------------- constants

/// `movementDuration`: how long the whole slide out (or back) takes.
pub const MOVEMENT: Duration = Duration::from_millis(200);

/// `_kDismissThreshold`: how far down, over its height, a drag let go
/// without a fling must have taken it to slide out.
pub const DISMISS_THRESHOLD: f32 = 0.4;

/// `_kMinFlingVelocity`: the slowest fling along the drag (px/s).
pub const MIN_DISMISS_FLING: f64 = 700.0;

/// `_kMinFlingVelocityDelta`: how much faster along the drag than across a
/// fling must be (px/s).
const MIN_FLING_VELOCITY_DELTA: f64 = 400.0;

/// `_kFlingVelocityScale`: a fling's velocity, in heights per second, per
/// px/s.
const FLING_VELOCITY_SCALE: f64 = 1.0 / 300.0;

/// `kMinFlingVelocity`: the slowest drag end the recognizer calls a fling
/// (px/s).
const MIN_FLING_VELOCITY: f64 = 50.0;

/// `kMaxFlingVelocity`: the fastest fling it reports (px/s).
const MAX_FLING_VELOCITY: f64 = 8000.0;

/// `kPrecisePointerHitSlop`: how far a mouse must have moved over the
/// velocity's samples for its drag end to be a fling.
const PRECISE_HIT_SLOP: f64 = 1.0;

// ------------------------------------------------------------ velocity

/// `VelocityTracker._historySize`.
const HISTORY: usize = 20;
/// `VelocityTracker._horizonMilliseconds`.
const HORIZON_MS: f64 = 100.0;
/// `VelocityTracker._assumePointerMoveStoppedMilliseconds`.
const STOPPED_MS: f64 = 40.0;
/// `VelocityTracker._minSampleSize`.
const MIN_SAMPLES: usize = 3;

/// Milliseconds from `earlier` to `later`, at a microsecond's resolution
/// (`Duration.inMicroseconds / 1000`), negative when `later` is earlier.
fn ms_between(later: Instant, earlier: Instant) -> f64 {
    if later >= earlier {
        later.duration_since(earlier).as_micros() as f64 / 1000.0
    } else {
        -(earlier.duration_since(later).as_micros() as f64 / 1000.0)
    }
}

/// What a [`VelocityTracker`] estimates at a drag's end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VelocityEstimate {
    /// The pointer's vertical velocity (px/s).
    pub velocity_y: f64,
    /// How far the samples the estimate used moved, vertically.
    pub offset_y: f64,
}

/// Flutter's `VelocityTracker` (a pointer's, not a scroll view's): the
/// positions of the last 20 events, and a degree-2 least-squares fit of the
/// recent ones. Only the vertical fit is kept: a vertical drag reads no
/// other, and the horizontal fit fails exactly when the vertical one does
/// (both solve the same matrix of sample times).
#[derive(Debug, Clone, Default)]
pub struct VelocityTracker {
    samples: [Option<(Instant, f64)>; HISTORY],
    index: usize,
    /// When the last sample was added (`_sinceLastSample`'s start).
    added: Option<Instant>,
}

impl VelocityTracker {
    /// The pointer was at `y` at `at`, handled then.
    pub fn add(&mut self, at: Instant, y: f32) {
        self.add_at(at, y, at);
    }

    /// The pointer was at `y` at `at` (its event's time), the sample added
    /// at `now`.
    pub fn add_at(&mut self, at: Instant, y: f32, now: Instant) {
        self.added = Some(now);
        self.index = (self.index + 1) % HISTORY;
        self.samples[self.index] = Some((at, f64::from(y)));
    }

    /// The estimate at `now`; `None` with no sample at all.
    pub fn estimate(&self, now: Instant) -> Option<VelocityEstimate> {
        let (newest_at, newest_y) = self.samples[self.index]?;
        // Has the pointer been still since its last sample was added? The
        // stopwatch counts whole milliseconds.
        if ms_between(now, self.added.unwrap_or(newest_at)).floor() > STOPPED_MS {
            return Some(VelocityEstimate {
                velocity_y: 0.0,
                offset_y: 0.0,
            });
        }
        let (mut time, mut y) = (Vec::new(), Vec::new());
        let mut index = self.index;
        let mut previous = newest_at;
        let mut oldest_y = newest_y;
        while time.len() < HISTORY {
            let Some((at, sample_y)) = self.samples[index] else {
                break;
            };
            let age = ms_between(newest_at, at);
            let delta = ms_between(at, previous).abs();
            previous = at;
            if age > HORIZON_MS || delta > STOPPED_MS {
                break;
            }
            oldest_y = sample_y;
            y.push(sample_y);
            time.push(-age);
            index = (if index == 0 { HISTORY } else { index }) - 1;
        }
        let velocity_y = if time.len() >= MIN_SAMPLES {
            least_squares_slope(&time, &y).map_or(0.0, |slope| slope * 1000.0)
        } else {
            0.0
        };
        Some(VelocityEstimate {
            velocity_y,
            offset_y: newest_y - oldest_y,
        })
    }
}

/// `precisionErrorTolerance`.
const PRECISION_ERROR_TOLERANCE: f64 = 1e-10;

/// `LeastSquaresSolver(x, y, w: 1s).solve(2).coefficients[1]`: the slope at
/// `x = 0` of the degree-2 polynomial fit of `y` over `x` (Gram-Schmidt QR,
/// as Flutter solves it); `None` when the `x`s are degenerate.
fn least_squares_slope(x: &[f64], y: &[f64]) -> Option<f64> {
    const N: usize = 3;
    let m = x.len();
    if N - 1 > m {
        return None;
    }
    // A: row i holds x^i for every sample (weights 1).
    let mut a = vec![[0.0f64; N]; m];
    for (h, row) in a.iter_mut().enumerate() {
        row[0] = 1.0;
        for i in 1..N {
            row[i] = row[i - 1] * x[h];
        }
    }
    let dot = |u: &dyn Fn(usize) -> f64, v: &dyn Fn(usize) -> f64| -> f64 {
        (0..m).map(|h| u(h) * v(h)).sum()
    };
    let mut q = vec![[0.0f64; N]; m];
    let mut r = [[0.0f64; N]; N];
    for j in 0..N {
        for (row, from) in q.iter_mut().zip(&a) {
            row[j] = from[j];
        }
        for i in 0..j {
            let d = dot(&|h| q[h][j], &|h| q[h][i]);
            for row in q.iter_mut() {
                row[j] -= d * row[i];
            }
        }
        let norm = dot(&|h| q[h][j], &|h| q[h][j]).sqrt();
        if norm < PRECISION_ERROR_TOLERANCE {
            return None;
        }
        let inverse = 1.0 / norm;
        for row in q.iter_mut() {
            row[j] *= inverse;
        }
        for (i, cell) in r[j].iter_mut().enumerate() {
            *cell = if i < j {
                0.0
            } else {
                dot(&|h| q[h][j], &|h| a[h][i])
            };
        }
    }
    let mut b = [0.0f64; N];
    for i in (0..N).rev() {
        b[i] = dot(&|h| q[h][i], &|h| y[h]);
        for j in (i + 1..N).rev() {
            b[i] -= r[i][j] * b[j];
        }
        b[i] /= r[i][i];
    }
    Some(b[1])
}

/// `VerticalDragGestureRecognizer.considerFling` for a device whose hit
/// slop is `slop` (a mouse's, [`PRECISE_HIT_SLOP`], or a trackpad's,
/// `kTouchSlop`): the estimate's velocity if it is a fling (faster than
/// `kMinFlingVelocity`, over more than the slop), clamped to
/// `kMaxFlingVelocity`; none otherwise.
fn fling_velocity(estimate: Option<VelocityEstimate>, slop: f64) -> f64 {
    match estimate {
        Some(e) if e.velocity_y.abs() > MIN_FLING_VELOCITY && e.offset_y.abs() > slop => {
            e.velocity_y.clamp(-MAX_FLING_VELOCITY, MAX_FLING_VELOCITY)
        }
        _ => 0.0,
    }
}

// ----------------------------------------------------------------- spring

/// `fling`'s spring: `SpringDescription.withDampingRatio(mass: 1,
/// stiffness: 500)`.
const SPRING_MASS: f64 = 1.0;
const SPRING_STIFFNESS: f64 = 500.0;

/// `_kFlingTolerance.distance`: how close to its end (1.01 or -0.01) a
/// fling is done; its velocity tolerance is infinite.
const FLING_TOLERANCE: f64 = 0.01;

/// `nearZero(a, epsilon)`.
fn near_zero(a: f64, epsilon: f64) -> bool {
    (a > -epsilon && a < epsilon) || a == 0.0
}

/// A spring's motion relative to its end (`_SpringSolution`). The fling
/// spring is critically damped on paper, but its damping squared minus four
/// times its mass by its stiffness comes out at 2.3e-13 in doubles, so
/// Flutter solves it as overdamped; so does this. An underdamped spring
/// (which `fling` refuses) cannot arise.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Solution {
    Critical { r: f64, c1: f64, c2: f64 },
    Overdamped { r1: f64, r2: f64, c1: f64, c2: f64 },
}

impl Solution {
    /// From `distance` off its end at `velocity`.
    fn new(distance: f64, velocity: f64) -> Self {
        let damping = 2.0 * (SPRING_MASS * SPRING_STIFFNESS).sqrt();
        let cmk = damping * damping - 4.0 * SPRING_MASS * SPRING_STIFFNESS;
        if cmk > 0.0 {
            let r1 = (-damping - cmk.sqrt()) / (2.0 * SPRING_MASS);
            let r2 = (-damping + cmk.sqrt()) / (2.0 * SPRING_MASS);
            let c2 = (velocity - r1 * distance) / (r2 - r1);
            let c1 = distance - c2;
            Solution::Overdamped { r1, r2, c1, c2 }
        } else {
            let r = -damping / (2.0 * SPRING_MASS);
            Solution::Critical {
                r,
                c1: distance,
                c2: velocity - r * distance,
            }
        }
    }

    fn x(&self, t: f64) -> f64 {
        match *self {
            Solution::Critical { r, c1, c2 } => (c1 + c2 * t) * (r * t).exp(),
            Solution::Overdamped { r1, r2, c1, c2 } => c1 * (r1 * t).exp() + c2 * (r2 * t).exp(),
        }
    }

    fn dx(&self, t: f64) -> f64 {
        match *self {
            Solution::Critical { r, c1, c2 } => {
                let power = (r * t).exp();
                r * (c1 + c2 * t) * power + c2 * power
            }
            Solution::Overdamped { r1, r2, c1, c2 } => {
                c1 * r1 * (r1 * t).exp() + c2 * r2 * (r2 * t).exp()
            }
        }
    }
}

// ------------------------------------------------------------- controller

/// What a [`MoveController`] runs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Run {
    /// `_InterpolationSimulation` on `Curves.linear`.
    Linear { from: f64, to: f64, seconds: f64 },
    /// `fling`'s `SpringSimulation` toward `end`.
    Spring { end: f64, solution: Solution },
}

impl Run {
    fn x(&self, t: f64) -> f64 {
        match *self {
            Run::Linear { from, to, seconds } => {
                let p = (t / seconds).clamp(0.0, 1.0);
                if p == 0.0 {
                    from
                } else if p == 1.0 {
                    to
                } else {
                    from + (to - from) * p
                }
            }
            Run::Spring { end, solution } => end + solution.x(t),
        }
    }

    fn is_done(&self, t: f64) -> bool {
        match *self {
            Run::Linear { seconds, .. } => t > seconds,
            Run::Spring { solution, .. } => {
                near_zero(solution.x(t), FLING_TOLERANCE)
                    && near_zero(solution.dx(t), f64::INFINITY)
            }
        }
    }
}

/// Where a [`MoveController`] is (`AnimationStatus`, as `isCompleted` and
/// `isDismissed` read it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// At 0, still.
    Dismissed,
    /// Moving, or held part of the way.
    Between,
    /// At 1, still.
    Completed,
}

/// The `Dismissible`'s `_moveController`: a value from 0 to 1, set by a
/// drag, or run linearly over [`MOVEMENT`] (scaled by the distance left), or
/// flung on a spring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveController {
    value: f64,
    run: Option<(Run, Instant, bool)>,
}

impl Default for MoveController {
    fn default() -> Self {
        Self {
            value: 0.0,
            run: None,
        }
    }
}

impl MoveController {
    fn seconds(&self, start: Instant, now: Instant) -> f64 {
        now.saturating_duration_since(start).as_micros() as f64 / 1e6
    }

    /// Its value at `now`.
    pub fn value(&self, now: Instant) -> f64 {
        match self.run {
            Some((run, start, _)) => run.x(self.seconds(start, now)).clamp(0.0, 1.0),
            None => self.value,
        }
    }

    /// Whether it runs at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.run
            .is_some_and(|(run, start, _)| !run.is_done(self.seconds(start, now)))
    }

    /// Where it is at `now`.
    pub fn status(&self, now: Instant) -> Status {
        match self.run {
            Some((run, start, forward)) if run.is_done(self.seconds(start, now)) => {
                if forward {
                    Status::Completed
                } else {
                    Status::Dismissed
                }
            }
            Some(_) => Status::Between,
            None if self.value >= 1.0 => Status::Completed,
            None if self.value <= 0.0 => Status::Dismissed,
            None => Status::Between,
        }
    }

    /// `stop()` at `now`: it stays where it is.
    pub fn stop(&mut self, now: Instant) {
        self.value = self.value(now);
        self.run = None;
    }

    /// `value = v`: still at `v`, clamped.
    pub fn set(&mut self, value: f64) {
        self.value = value.clamp(0.0, 1.0);
        self.run = None;
    }

    /// `forward()` / `reverse()` from `now`.
    pub fn animate_to(&mut self, target: f64, now: Instant) {
        let from = self.value(now);
        let seconds = MOVEMENT.as_secs_f64() * (target - from).abs();
        self.stop(now);
        if seconds == 0.0 {
            self.value = target;
            return;
        }
        let run = Run::Linear {
            from,
            to: target,
            seconds,
        };
        self.run = Some((run, now, target >= 1.0));
    }

    /// `fling(velocity: v)` from `now` (heights per second; up to 1, or down
    /// to 0 when negative).
    pub fn fling(&mut self, velocity: f64, now: Instant) {
        let end = if velocity < 0.0 {
            -FLING_TOLERANCE
        } else {
            1.0 + FLING_TOLERANCE
        };
        let from = self.value(now);
        let run = Run::Spring {
            end,
            solution: Solution::new(from - end, velocity),
        };
        self.stop(now);
        self.run = Some((run, now, velocity >= 0.0));
    }
}

// --------------------------------------------------------------- dismiss

/// `Dart's double.sign`: 0 for 0.
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// `_FlingGestureKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fling {
    None,
    Forward,
    Reverse,
}

/// What a drag follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Device {
    /// The mouse, pressed on it.
    Mouse,
    /// A trackpad's pan-zoom gesture (its number), started over it.
    PanZoom(u64),
}

/// The pointer a drag follows: where it was last (a trackpad's: its pan so
/// far, from 0 at its start).
#[derive(Debug, Clone)]
struct Pointer {
    device: Device,
    last_y: f32,
    tracker: VelocityTracker,
}

/// `_DismissibleState` for `DismissDirection.down`, with its drag
/// recognizer (see the module docs). Each call takes the time, and the
/// widget's height where the drag needs it.
#[derive(Debug, Clone, Default)]
pub struct Dismiss {
    controller: MoveController,
    /// `_dragExtent`: how far the drag has taken it (px; signed).
    extent: f64,
    /// `_moveAnimation`'s end: the sign of the extent when it was last
    /// updated (a fling's new extent does not update it).
    end: f64,
    /// `_dragUnderway`.
    drag_underway: bool,
    /// The pointer a drag follows, while it is down.
    pointer: Option<Pointer>,
    /// Whether it has reported its dismissal.
    reported: bool,
}

impl Dismiss {
    /// A primary press at `at` (where the widget takes hits): the drag
    /// starts there.
    pub fn press(&mut self, at: Point, now: Instant, height: f32) {
        self.start(Device::Mouse, at.y, now, now, height);
    }

    /// A trackpad's pan-zoom `gesture` started over it at `at` (its
    /// event's time), handled at `now`: the drag starts, its pan at 0.
    pub fn pan_zoom_start(&mut self, gesture: u64, at: Instant, now: Instant, height: f32) {
        self.start(Device::PanZoom(gesture), 0.0, at, now, height);
    }

    /// The drag starts, following `device` from `y`, sampled at `at`.
    fn start(&mut self, device: Device, y: f32, at: Instant, now: Instant, height: f32) {
        if self.reported || self.pointer.is_some() {
            return;
        }
        let mut pointer = Pointer {
            device,
            last_y: y,
            tracker: VelocityTracker::default(),
        };
        pointer.tracker.add_at(at, y, now);
        self.pointer = Some(pointer);
        // `_handleDragStart`.
        self.drag_underway = true;
        if self.controller.is_animating(now) {
            self.extent = self.controller.value(now) * f64::from(height) * sign(self.extent);
            self.controller.stop(now);
        } else {
            self.extent = 0.0;
            self.controller.set(0.0);
        }
        self.end = sign(self.extent);
    }

    /// The mouse moved to `to`.
    pub fn moved(&mut self, to: Point, now: Instant, height: f32) {
        self.follow(Device::Mouse, to.y, now, now, height);
    }

    /// The trackpad's `gesture` panned by `delta_y` at `at` (its event's
    /// time), handled at `now`.
    pub fn panned(&mut self, gesture: u64, delta_y: f32, at: Instant, now: Instant, height: f32) {
        let Some(pointer) = self.pointer.as_ref() else {
            return;
        };
        let y = pointer.last_y + delta_y;
        self.follow(Device::PanZoom(gesture), y, at, now, height);
    }

    /// `device` moved to `y` (sampled at `at`).
    fn follow(&mut self, device: Device, y: f32, at: Instant, now: Instant, height: f32) {
        let Some(pointer) = self.pointer.as_mut().filter(|p| p.device == device) else {
            return;
        };
        pointer.tracker.add_at(at, y, now);
        let delta = f64::from(y - pointer.last_y);
        pointer.last_y = y;
        // `_handleDragUpdate`, down only.
        if !self.drag_underway || self.controller.is_animating(now) {
            return;
        }
        let old = self.extent;
        if self.extent + delta > 0.0 {
            self.extent += delta;
        }
        if sign(old) != sign(self.extent) {
            self.end = sign(self.extent);
        }
        if height > 0.0 {
            self.controller.set(self.extent.abs() / f64::from(height));
        }
    }

    /// The mouse went up (or its buttons changed) at `now`: whether that
    /// dismissed it.
    pub fn release(&mut self, now: Instant) -> bool {
        self.end(Device::Mouse, PRECISE_HIT_SLOP, now)
    }

    /// The trackpad's `gesture` ended (its fingers lifted) at `now`:
    /// whether that dismissed it.
    pub fn pan_zoom_end(&mut self, gesture: u64, now: Instant) -> bool {
        self.end(Device::PanZoom(gesture), TOUCH_SLOP, now)
    }

    /// `device`'s drag ended: a fling past its hit slop `slop`, or not.
    fn end(&mut self, device: Device, slop: f64, now: Instant) -> bool {
        let Some(pointer) = self.pointer.take_if(|p| p.device == device) else {
            return false;
        };
        let velocity = fling_velocity(pointer.tracker.estimate(now), slop);
        self.drag_end(velocity, now)
    }

    /// Whether a drag follows a pointer (the mouse or a trackpad).
    pub fn is_dragging(&self) -> bool {
        self.pointer.is_some()
    }

    /// Whether a drag follows the mouse.
    pub fn is_mouse_dragging(&self) -> bool {
        self.pointer
            .as_ref()
            .is_some_and(|p| p.device == Device::Mouse)
    }

    /// How far down it is drawn at `now`, over its height
    /// (`SlideTransition`).
    pub fn offset(&self, now: Instant) -> f32 {
        (self.controller.value(now) * self.end) as f32
    }

    /// Whether it slides at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.controller.is_animating(now)
    }

    /// A frame at `now`: whether its slide out has just completed (once).
    pub fn frame(&mut self, now: Instant) -> bool {
        if self.reported || self.drag_underway {
            return false;
        }
        if self.controller.status(now) == Status::Completed {
            self.reported = true;
            return true;
        }
        false
    }

    /// `_extentToDirection(extent)` along the vertical axis: down, up or
    /// none, as 1, -1 or 0.
    fn direction(extent: f64) -> f64 {
        sign(extent)
    }

    /// `_describeFlingGesture` for a vertical velocity (the drag reports no
    /// horizontal one).
    fn describe_fling(&self, velocity: f64) -> Fling {
        if self.extent == 0.0 {
            return Fling::None;
        }
        let (vx, vy) = (0.0f64, velocity);
        if vy.abs() - vx.abs() < MIN_FLING_VELOCITY_DELTA || vy.abs() < MIN_DISMISS_FLING {
            return Fling::None;
        }
        if Self::direction(vy) == Self::direction(self.extent) {
            Fling::Forward
        } else {
            Fling::Reverse
        }
    }

    /// `_handleDragEnd`: whether it is dismissed at once (dragged all the
    /// way out).
    fn drag_end(&mut self, velocity: f64, now: Instant) -> bool {
        if !self.drag_underway || self.controller.is_animating(now) {
            return false;
        }
        self.drag_underway = false;
        if self.controller.status(now) == Status::Completed {
            // `_handleMoveCompleted`.
            self.reported = true;
            return true;
        }
        match self.describe_fling(velocity) {
            Fling::Forward => {
                self.extent = sign(velocity);
                self.controller
                    .fling(velocity.abs() * FLING_VELOCITY_SCALE, now);
            }
            Fling::Reverse => {
                self.extent = sign(velocity);
                self.controller
                    .fling(-velocity.abs() * FLING_VELOCITY_SCALE, now);
            }
            Fling::None => {
                if self.controller.status(now) != Status::Dismissed {
                    let target = if self.controller.value(now) > f64::from(DISMISS_THRESHOLD) {
                        1.0
                    } else {
                        0.0
                    };
                    self.controller.animate_to(target, now);
                }
            }
        }
        false
    }
}

// ----------------------------------------------------------------- widget

/// When a pointer event happened: iced's events carry no time stamp, so
/// the time it is handled (a test sets it).
fn event_time() -> Instant {
    pan_zoom::event_time()
}

/// Where a [`Dismissible`] takes hits (its `HitTestBehavior`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Behavior {
    /// `HitTestBehavior.opaque`: anywhere in its box.
    Opaque,
    /// `HitTestBehavior.deferToChild`: only where its child does — a
    /// `Material` `inset` in from the box, its corners rounded to `radius`
    /// (a shaped `Material` takes no hit outside its shape) — and there as
    /// the child is drawn, slid along.
    DeferToChild { inset: Padding, radius: f32 },
}

/// See the module docs: `content`, which `on_dismissed` reports dragged
/// away. Its slide belongs to `key`: a widget of another key, built in its
/// place, starts at rest.
pub struct Dismissible<'a, M> {
    content: Element<'a, M>,
    key: u64,
    behavior: Behavior,
    on_dismissed: M,
}

impl<'a, M> Dismissible<'a, M> {
    pub fn new(
        content: impl Into<Element<'a, M>>,
        key: u64,
        behavior: Behavior,
        on_dismissed: M,
    ) -> Self {
        Self {
            content: content.into(),
            key,
            behavior,
            on_dismissed,
        }
    }

    /// Whether the widget in `bounds`, its content slid `shift` down and
    /// shown within `viewport`, takes a hit at `at`.
    fn hits(&self, bounds: Rectangle, shift: f32, viewport: &Rectangle, at: Point) -> bool {
        if !viewport.contains(at) || !bounds.contains(at) {
            return false;
        }
        match self.behavior {
            Behavior::Opaque => true,
            Behavior::DeferToChild { inset, radius } => {
                let child = Rectangle {
                    x: bounds.x + inset.left,
                    y: bounds.y + inset.top + shift,
                    width: bounds.width - inset.left - inset.right,
                    height: bounds.height - inset.top - inset.bottom,
                };
                in_rounded(child, radius, at)
            }
        }
    }
}

/// Whether `at` is inside `rect` with its corners rounded to `radius`.
fn in_rounded(rect: Rectangle, radius: f32, at: Point) -> bool {
    if !rect.contains(at) {
        return false;
    }
    let r = radius.min(rect.width / 2.0).min(rect.height / 2.0).max(0.0);
    let cx = at.x.clamp(rect.x + r, rect.x + rect.width - r);
    let cy = at.y.clamp(rect.y + r, rect.y + rect.height - r);
    Point::new(cx, cy).distance(at) <= r
}

/// A [`Dismissible`]'s state: its key, its drag and slide, the trackpad
/// gesture it took last, and the last frame's time.
struct State {
    key: u64,
    dismiss: Dismiss,
    held: Held,
    now: Instant,
}

impl State {
    fn new(key: u64) -> Self {
        Self {
            key,
            dismiss: Dismiss::default(),
            held: Held::default(),
            now: Instant::now(),
        }
    }
}

impl<M: Clone> Widget<M, Theme, Renderer> for Dismissible<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new(self.key))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        if tree.state.downcast_ref::<State>().key != self.key {
            tree.state = tree::State::new(State::new(self.key));
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(node.size(), vec![node])
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let height = bounds.height;
        let state = tree.state.downcast_mut::<State>();
        let shift = state.dismiss.offset(state.now) * height;
        let lift = Vector::new(0.0, shift);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("the content's layout"),
            cursor - lift,
            renderer,
            clipboard,
            shell,
            &(*viewport - lift),
        );
        let state = tree.state.downcast_mut::<State>();
        let held = &mut state.held;
        let dismiss = &mut state.dismiss;
        let mut dismissed = false;
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.now = *now;
                dismissed = dismiss.frame(*now);
                if dismiss.is_animating(*now) {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let now = event_time();
                // A slide that ended since the last frame has ended.
                dismissed = dismiss.frame(now);
                if dismiss.is_mouse_dragging() {
                    // Another button: the pointer's buttons changed, which
                    // ends the drag.
                    dismissed |= dismiss.release(now);
                    shell.request_redraw();
                } else if let Some(at) = cursor.position() {
                    if self.hits(bounds, shift, viewport, at) {
                        // (A trackpad's drag under way keeps it.)
                        if *button == mouse::Button::Left && !dismiss.is_dragging() {
                            dismiss.press(at, now, height);
                            shell.request_redraw();
                        }
                        // The snack bar's `Material` takes the hit: nothing
                        // beneath sees the press.
                        shell.capture_event();
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if dismiss.is_mouse_dragging() => {
                dismiss.moved(*position, event_time(), height);
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if dismiss.is_mouse_dragging() =>
            {
                dismissed = dismiss.release(event_time());
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                let over = cursor
                    .position()
                    .is_some_and(|at| self.hits(bounds, shift, viewport, at));
                let scroll = pan_zoom::current();
                match scroll {
                    // A wheel's signal over it: its hit test's, unheard.
                    Scroll::Signal => {
                        if over {
                            shell.capture_event();
                        }
                    }
                    // A swipe starting over it: the drag recognizer's, as
                    // a press is.
                    Scroll::Start { gesture, time } => {
                        if over {
                            let now = event_time();
                            dismissed = dismiss.frame(now);
                            held.take(gesture);
                            dismiss.pan_zoom_start(gesture, time, now, height);
                            shell.capture_event();
                            shell.request_redraw();
                        }
                    }
                    Scroll::Update {
                        gesture,
                        delta,
                        time,
                    } if held.holds(gesture) => {
                        dismiss.panned(gesture, delta.y, time, event_time(), height);
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    Scroll::End { gesture } if held.holds(gesture) => {
                        dismissed = dismiss.pan_zoom_end(gesture, event_time());
                        shell.capture_event();
                        shell.request_redraw();
                    }
                    // Its momentum reached no widget.
                    Scroll::Nothing { .. } if held.owns(&scroll) => shell.capture_event(),
                    // Another's gesture.
                    Scroll::Update { .. } | Scroll::End { .. } | Scroll::Nothing { .. } => {}
                }
            }
            _ => {}
        }
        if dismissed {
            shell.publish(self.on_dismissed.clone());
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let shift = state.dismiss.offset(state.now) * layout.bounds().height;
        let lift = Vector::new(0.0, shift);
        renderer.with_translation(lift, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout.children().next().expect("the content's layout"),
                cursor - lift,
                &(*viewport - lift),
            );
        });
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("the content's layout"),
            renderer,
            operation,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        let shift = state.dismiss.offset(state.now) * layout.bounds().height;
        let lift = Vector::new(0.0, shift);
        let inner = self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("the content's layout"),
            cursor - lift,
            &(*viewport - lift),
            renderer,
        );
        let over = cursor
            .position()
            .is_some_and(|at| self.hits(layout.bounds(), shift, viewport, at));
        if over && inner == mouse::Interaction::None {
            // It takes the hit: the cursor of what lies beneath is not
            // shown (the basic one is).
            mouse::Interaction::Idle
        } else {
            inner
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("the content's layout"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: Clone + 'a> From<Dismissible<'a, M>> for Element<'a, M> {
    fn from(dismissible: Dismissible<'a, M>) -> Self {
        Element::new(dismissible)
    }
}

/// The pointer events' clock, for tests.
#[cfg(test)]
pub(crate) mod testing {
    use iced::time::Instant;

    /// Pointer events are handled at `at` (or at the time they are, with
    /// `None`) on this thread.
    pub fn set_event_time(at: Option<Instant>) {
        super::pan_zoom::testing::set_event_time(at);
    }
}

#[cfg(test)]
mod tests;
