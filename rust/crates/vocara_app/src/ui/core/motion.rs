//! Flutter's animation primitives, for the port's animations: the curves
//! (`Curves.*`, with Flutter's own cubic solver), the implicit animation
//! every `Animated*` widget ran ([`Implicit`]), an `AnimationController`
//! ([`Controller`]) for the transitions Material drove with one (routes,
//! dialogs, snack bars), and the frame subscription they run on.
//!
//! Both run on [`iced::Animation`]: an [`Implicit`] is one `Animation`
//! run from the value it shows to its new target; a [`Controller`] is one
//! linear run from its value to 0 or 1. Every reading takes the time
//! (`now`), so a test injects it.
//!
//! An animation runs only while it moves: a model that holds one subscribes
//! to [`frames`] only while [`Implicit::is_animating`] /
//! [`Controller::is_animating`] says so, and a widget that holds one asks
//! for the next redraw only then. An idle app does not redraw.
//!
//! The model's state never waits for an animation: it changes at once, and
//! what animates is only how the change is drawn.

use std::time::Duration;

use iced::animation::Easing;
use iced::time::Instant;
use iced::{window, Animation, Color, Padding, Subscription};

// -------------------------------------------------------------------- curves

/// A Flutter curve (`Curves.*`), evaluated as Flutter does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    /// `Curves.linear` (every implicit animation's default).
    Linear,
    /// `Curves.ease`: `Cubic(0.25, 0.1, 0.25, 1.0)`.
    Ease,
    /// `Curves.easeIn`: `Cubic(0.42, 0.0, 1.0, 1.0)`.
    EaseIn,
    /// `Curves.easeOut`: `Cubic(0.0, 0.0, 0.58, 1.0)`.
    EaseOut,
    /// `Curves.fastOutSlowIn`: `Cubic(0.4, 0.0, 0.2, 1.0)`.
    FastOutSlowIn,
    /// `Curves.easeInOutQuart`: `Cubic(0.77, 0.0, 0.175, 1.0)`.
    EaseInOutQuart,
    /// `Curves.linearToEaseOut`: `Cubic(0.35, 0.91, 0.33, 0.97)`.
    LinearToEaseOut,
    /// `Curves.easeInToLinear`: `Cubic(0.67, 0.03, 0.65, 0.09)`.
    EaseInToLinear,
    /// `Curves.fastEaseInToSlowEaseOut`, a `ThreePointCubic`.
    FastEaseInToSlowEaseOut,
    /// `Curves.fastEaseInToSlowEaseOut.flipped`.
    FastEaseInToSlowEaseOutFlipped,
    /// Material 3's floating snack bar fade:
    /// `Interval(0.4, 0.6, curve: Curves.easeInCirc)`.
    SnackBarM3FadeIn,
}

impl Curve {
    /// The curve's value at `t` (`Curve.transform`): 0 and 1 map to
    /// themselves, as Flutter's parametric curves guarantee.
    pub fn transform(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let t = f64::from(t);
        let y = match self {
            Curve::Linear => t,
            Curve::Ease => cubic(0.25, 0.1, 0.25, 1.0, t),
            Curve::EaseIn => cubic(0.42, 0.0, 1.0, 1.0, t),
            Curve::EaseOut => cubic(0.0, 0.0, 0.58, 1.0, t),
            Curve::FastOutSlowIn => cubic(0.4, 0.0, 0.2, 1.0, t),
            Curve::EaseInOutQuart => cubic(0.77, 0.0, 0.175, 1.0, t),
            Curve::LinearToEaseOut => cubic(0.35, 0.91, 0.33, 0.97, t),
            Curve::EaseInToLinear => cubic(0.67, 0.03, 0.65, 0.09, t),
            Curve::FastEaseInToSlowEaseOut => fast_ease_in_to_slow_ease_out(t),
            Curve::FastEaseInToSlowEaseOutFlipped => 1.0 - fast_ease_in_to_slow_ease_out(1.0 - t),
            Curve::SnackBarM3FadeIn => {
                // Interval.transformInternal: clamp into the interval, and
                // its ends map to themselves.
                let t = ((t - 0.4) / (0.6 - 0.4)).clamp(0.0, 1.0);
                if t == 0.0 || t == 1.0 {
                    t
                } else {
                    cubic(0.6, 0.04, 0.98, 0.335, t)
                }
            }
        };
        y as f32
    }

    /// The curve as an [`iced::Animation`]'s easing.
    pub fn easing(self) -> Easing {
        Easing::Custom(match self {
            Curve::Linear => |t| Curve::Linear.transform(t),
            Curve::Ease => |t| Curve::Ease.transform(t),
            Curve::EaseIn => |t| Curve::EaseIn.transform(t),
            Curve::EaseOut => |t| Curve::EaseOut.transform(t),
            Curve::FastOutSlowIn => |t| Curve::FastOutSlowIn.transform(t),
            Curve::EaseInOutQuart => |t| Curve::EaseInOutQuart.transform(t),
            Curve::LinearToEaseOut => |t| Curve::LinearToEaseOut.transform(t),
            Curve::EaseInToLinear => |t| Curve::EaseInToLinear.transform(t),
            Curve::FastEaseInToSlowEaseOut => |t| Curve::FastEaseInToSlowEaseOut.transform(t),
            Curve::FastEaseInToSlowEaseOutFlipped => {
                |t| Curve::FastEaseInToSlowEaseOutFlipped.transform(t)
            }
            Curve::SnackBarM3FadeIn => |t| Curve::SnackBarM3FadeIn.transform(t),
        })
    }
}

/// Flutter's `Cubic(a, b, c, d).transformInternal(t)`: bisects the curve's
/// x for `t` to within its error bound, then reads y there.
fn cubic(a: f64, b: f64, c: f64, d: f64, t: f64) -> f64 {
    const ERROR_BOUND: f64 = 0.001;
    let evaluate = |a: f64, b: f64, m: f64| {
        3.0 * a * (1.0 - m) * (1.0 - m) * m + 3.0 * b * (1.0 - m) * m * m + m * m * m
    };
    let (mut start, mut end) = (0.0_f64, 1.0_f64);
    loop {
        let midpoint = (start + end) / 2.0;
        let estimate = evaluate(a, c, midpoint);
        if (t - estimate).abs() < ERROR_BOUND {
            return evaluate(b, d, midpoint);
        }
        if estimate < t {
            start = midpoint;
        } else {
            end = midpoint;
        }
    }
}

/// `Cubic.transform`: a cubic's ends map to themselves.
fn cubic_transform(a: f64, b: f64, c: f64, d: f64, t: f64) -> f64 {
    if t == 0.0 || t == 1.0 {
        t
    } else {
        cubic(a, b, c, d, t)
    }
}

/// `Curves.fastEaseInToSlowEaseOut`: `ThreePointCubic(a1: (0.056, 0.024),
/// b1: (0.108, 0.3085), midpoint: (0.198, 0.541), a2: (0.3655, 1.0),
/// b2: (0.5465, 0.989))`, two cubics joined at the midpoint.
fn fast_ease_in_to_slow_ease_out(t: f64) -> f64 {
    const A1: (f64, f64) = (0.056, 0.024);
    const B1: (f64, f64) = (0.108, 0.3085);
    const MID: (f64, f64) = (0.198, 0.541);
    const A2: (f64, f64) = (0.3655, 1.0);
    const B2: (f64, f64) = (0.5465, 0.989);
    if t == 0.0 || t == 1.0 {
        return t;
    }
    let first = t < MID.0;
    let scale_x = if first { MID.0 } else { 1.0 - MID.0 };
    let scale_y = if first { MID.1 } else { 1.0 - MID.1 };
    let scaled_t = (t - if first { 0.0 } else { MID.0 }) / scale_x;
    if first {
        cubic_transform(
            A1.0 / scale_x,
            A1.1 / scale_y,
            B1.0 / scale_x,
            B1.1 / scale_y,
            scaled_t,
        ) * scale_y
    } else {
        cubic_transform(
            (A2.0 - MID.0) / scale_x,
            (A2.1 - MID.1) / scale_y,
            (B2.0 - MID.0) / scale_x,
            (B2.1 - MID.1) / scale_y,
            scaled_t,
        ) * scale_y
            + MID.1
    }
}

// --------------------------------------------------------------------- lerp

/// A value an animation can move between (Flutter's `lerp`s).
pub trait Lerp: Copy + PartialEq {
    /// The value `t` of the way from `self` to `to` (0 is `self`, 1 `to`).
    fn lerp(self, to: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, to: Self, t: f32) -> Self {
        self + (to - self) * t
    }
}

/// `Color.lerp`: each component on its own, alpha clamped.
impl Lerp for Color {
    fn lerp(self, to: Self, t: f32) -> Self {
        Color {
            r: self.r.lerp(to.r, t),
            g: self.g.lerp(to.g, t),
            b: self.b.lerp(to.b, t),
            a: self.a.lerp(to.a, t).clamp(0.0, 1.0),
        }
    }
}

/// `EdgeInsets.lerp`.
impl Lerp for Padding {
    fn lerp(self, to: Self, t: f32) -> Self {
        Padding {
            top: self.top.lerp(to.top, t),
            right: self.right.lerp(to.right, t),
            bottom: self.bottom.lerp(to.bottom, t),
            left: self.left.lerp(to.left, t),
        }
    }
}

impl<A: Lerp, B: Lerp> Lerp for (A, B) {
    fn lerp(self, to: Self, t: f32) -> Self {
        (self.0.lerp(to.0, t), self.1.lerp(to.1, t))
    }
}

impl<A: Lerp, B: Lerp, C: Lerp> Lerp for (A, B, C) {
    fn lerp(self, to: Self, t: f32) -> Self {
        (
            self.0.lerp(to.0, t),
            self.1.lerp(to.1, t),
            self.2.lerp(to.2, t),
        )
    }
}

/// A linear run of `duration` from `at`: its progress goes 0 to 1, eased by
/// `curve`.
fn run(duration: Duration, curve: Curve, at: Instant) -> Animation<bool> {
    Animation::new(false)
        .duration(duration)
        .easing(curve.easing())
        .go(true, at)
}

// ----------------------------------------------------------------- implicit

/// An implicit animation (`ImplicitlyAnimatedWidget`: `AnimatedContainer`,
/// `AnimatedOpacity`, `AnimatedAlign`, `AnimatedRotation`): it shows its
/// target, and when the target changes it moves there from the value it
/// shows at that moment, over its whole duration along its curve, as
/// Flutter restarted the controller with the tween's begin set to the
/// current value. The first target shows at once.
#[derive(Debug, Clone)]
pub struct Implicit<T> {
    from: T,
    to: T,
    duration: Duration,
    curve: Curve,
    /// The run toward `to`; `None` until the target first changes.
    run: Option<Animation<bool>>,
}

impl<T: Lerp> Implicit<T> {
    /// Shows `value`, still.
    pub fn new(value: T, duration: Duration, curve: Curve) -> Self {
        Self {
            from: value,
            to: value,
            duration,
            curve,
            run: None,
        }
    }

    /// The value it is moving to (or rests at).
    pub fn target(&self) -> T {
        self.to
    }

    /// Moves to `target` from what it shows `now`; nothing when that is
    /// its target already.
    pub fn retarget(&mut self, target: T, now: Instant) {
        if target == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = target;
        self.run = Some(run(self.duration, self.curve, now));
    }

    /// Shows `value` at once, stopping any run.
    pub fn jump(&mut self, value: T) {
        self.from = value;
        self.to = value;
        self.run = None;
    }

    /// What it shows at `now`.
    pub fn value(&self, now: Instant) -> T {
        match &self.run {
            Some(run) if run.is_animating(now) => {
                self.from.lerp(self.to, run.interpolate(0.0_f32, 1.0, now))
            }
            _ => self.to,
        }
    }

    /// Whether it is still moving at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.run.as_ref().is_some_and(|run| run.is_animating(now))
    }
}

// --------------------------------------------------------------- controller

/// Where a [`Controller`] is heading (`AnimationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// At 0, still.
    Dismissed,
    /// Running toward 1.
    Forward,
    /// Running toward 0.
    Reverse,
    /// At 1, still.
    Completed,
}

/// An `AnimationController`: a value from 0 to 1 that runs forward over
/// `forward` and back over `reverse`; a run started part of the way covers
/// only what is left, in that share of the duration
/// (`AnimationController._animateToInternal`).
#[derive(Debug, Clone)]
pub struct Controller {
    forward: Duration,
    reverse: Duration,
    from: f32,
    to: f32,
    run: Option<Animation<bool>>,
    /// The direction the run that the curves follow started in
    /// (`CurvedAnimation._curveDirection`): a reverse begun before a
    /// forward run ended keeps the forward curve until it is dismissed.
    curve_forward: bool,
}

impl Controller {
    /// Dismissed (at 0).
    pub fn new(forward: Duration, reverse: Duration) -> Self {
        Self {
            forward,
            reverse,
            from: 0.0,
            to: 0.0,
            run: None,
            curve_forward: true,
        }
    }

    /// Completed (at 1), as a route that is there from the start.
    pub fn completed(forward: Duration, reverse: Duration) -> Self {
        Self {
            from: 1.0,
            to: 1.0,
            ..Self::new(forward, reverse)
        }
    }

    /// Its value at `now`, from 0 to 1.
    pub fn value(&self, now: Instant) -> f32 {
        match &self.run {
            Some(run) if run.is_animating(now) => {
                self.from.lerp(self.to, run.interpolate(0.0_f32, 1.0, now))
            }
            _ => self.to,
        }
    }

    /// Where it is heading at `now`.
    pub fn status(&self, now: Instant) -> Status {
        if self.is_animating(now) {
            if self.to > self.from {
                Status::Forward
            } else {
                Status::Reverse
            }
        } else if self.to >= 1.0 {
            Status::Completed
        } else {
            Status::Dismissed
        }
    }

    /// Whether it is running at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.run.as_ref().is_some_and(|run| run.is_animating(now))
    }

    /// When its run ends (`now` or earlier once it has).
    pub fn ends_at(&self, now: Instant) -> Instant {
        let left = if self.is_animating(now) {
            let total = self.duration_toward(self.to);
            let share = (self.to - self.value(now)).abs();
            total.mul_f32(share)
        } else {
            Duration::ZERO
        };
        now + left
    }

    fn duration_toward(&self, target: f32) -> Duration {
        if target >= 1.0 {
            self.forward
        } else {
            self.reverse
        }
    }

    /// Runs toward 1 from where it is (`forward()`).
    pub fn forward(&mut self, now: Instant) {
        self.animate_to(1.0, now);
    }

    /// Runs toward 0 from where it is (`reverse()`).
    pub fn reverse(&mut self, now: Instant) {
        self.animate_to(0.0, now);
    }

    /// Jumps to `value`, still.
    pub fn set(&mut self, value: f32) {
        self.from = value;
        self.to = value;
        self.run = None;
    }

    fn animate_to(&mut self, target: f32, now: Instant) {
        let value = self.value(now);
        if !self.is_animating(now) {
            // A run from rest sets the direction the curves follow.
            self.curve_forward = target >= 1.0;
        }
        let share = (target - value).abs();
        let duration = self.duration_toward(target).mul_f32(share);
        self.from = value;
        self.to = target;
        self.run = (share > 0.0).then(|| run(duration, Curve::Linear, now));
    }

    /// Its value at `now` through `curve` going forward and `reverse_curve`
    /// going back (`CurvedAnimation(curve:, reverseCurve:)`); without a
    /// reverse curve the forward one serves both ways.
    pub fn curved(&self, curve: Curve, reverse_curve: Option<Curve>, now: Instant) -> f32 {
        let t = self.value(now);
        let reversing = self.is_animating(now) && !self.curve_forward;
        match reverse_curve {
            Some(reverse) if reversing => reverse.transform(t),
            _ => curve.transform(t),
        }
    }
}

// ------------------------------------------------------------------- frames

/// The window's frames while `animating` (each one's time, and a redraw
/// after it, so the animation moves), nothing otherwise: an idle app does
/// not redraw.
pub fn frames(animating: bool) -> Subscription<Instant> {
    if animating {
        window::frames()
    } else {
        Subscription::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 2e-3
    }

    #[test]
    fn curves_match_flutter() {
        // Values Flutter's Curves give (to its cubic solver's precision).
        assert_eq!(Curve::Linear.transform(0.3), 0.3);
        assert!(close(Curve::EaseOut.transform(0.5), 0.6836));
        assert!(close(Curve::Ease.transform(0.5), 0.8020));
        assert!(close(Curve::EaseIn.transform(0.5), 0.3164));
        assert!(close(Curve::FastOutSlowIn.transform(0.5), 0.7756));
        assert!(close(Curve::EaseInOutQuart.transform(0.5), 0.5933));
        assert!(close(Curve::LinearToEaseOut.transform(0.5), 0.9198));
        assert!(close(Curve::EaseInToLinear.transform(0.5), 0.0802));
        assert!(close(
            Curve::FastEaseInToSlowEaseOut.transform(0.198),
            0.541
        ));
        assert!(close(
            Curve::FastEaseInToSlowEaseOutFlipped.transform(0.802),
            0.459
        ));
        // The snack bar's fade happens between 40 % and 60 % of the run.
        assert_eq!(Curve::SnackBarM3FadeIn.transform(0.39), 0.0);
        assert!(Curve::SnackBarM3FadeIn.transform(0.5) > 0.0);
        assert_eq!(Curve::SnackBarM3FadeIn.transform(0.61), 1.0);
        for curve in [
            Curve::Linear,
            Curve::Ease,
            Curve::EaseIn,
            Curve::EaseOut,
            Curve::FastOutSlowIn,
            Curve::EaseInOutQuart,
            Curve::LinearToEaseOut,
            Curve::EaseInToLinear,
            Curve::FastEaseInToSlowEaseOut,
            Curve::FastEaseInToSlowEaseOutFlipped,
            Curve::SnackBarM3FadeIn,
        ] {
            assert_eq!(curve.transform(0.0), 0.0, "{curve:?}");
            assert_eq!(curve.transform(1.0), 1.0, "{curve:?}");
            let Easing::Custom(f) = curve.easing() else {
                panic!("a custom easing");
            };
            assert_eq!(f(0.37), curve.transform(0.37), "{curve:?}");
        }
    }

    #[test]
    fn an_implicit_animation_shows_its_first_value_at_once() {
        let t0 = Instant::now();
        let a = Implicit::new(4.0_f32, 120 * MS, Curve::Linear);
        assert_eq!(a.value(t0), 4.0);
        assert!(!a.is_animating(t0));
    }

    #[test]
    fn an_implicit_animation_moves_to_a_new_target_over_its_duration() {
        let t0 = Instant::now();
        let mut a = Implicit::new(0.0_f32, 120 * MS, Curve::Linear);
        a.retarget(10.0, t0);
        assert_eq!(a.target(), 10.0, "the target changes at once");
        assert_eq!(a.value(t0), 0.0, "start");
        assert!(close(a.value(t0 + 60 * MS), 5.0), "middle");
        assert_eq!(a.value(t0 + 120 * MS), 10.0, "end");
        assert!(a.is_animating(t0 + 60 * MS));
        assert!(!a.is_animating(t0 + 120 * MS));
    }

    #[test]
    fn a_retarget_starts_from_what_shows_and_takes_the_whole_duration() {
        let t0 = Instant::now();
        let mut a = Implicit::new(0.0_f32, 100 * MS, Curve::Linear);
        a.retarget(1.0, t0);
        let t1 = t0 + 40 * MS;
        a.retarget(0.0, t1);
        assert!(close(a.value(t1), 0.4));
        assert!(close(a.value(t1 + 50 * MS), 0.2));
        assert_eq!(a.value(t1 + 100 * MS), 0.0);
        // The same target again changes nothing.
        a.retarget(0.0, t1 + 10 * MS);
        assert!(close(a.value(t1 + 50 * MS), 0.2));
    }

    #[test]
    fn an_implicit_animation_follows_its_curve() {
        let t0 = Instant::now();
        let mut a = Implicit::new(0.0_f32, 100 * MS, Curve::EaseOut);
        a.retarget(1.0, t0);
        assert!(close(a.value(t0 + 50 * MS), Curve::EaseOut.transform(0.5)));
    }

    #[test]
    fn colors_lerp_by_component() {
        let a = Color::from_rgba(0.0, 0.2, 1.0, 0.0);
        let b = Color::from_rgba(1.0, 0.4, 0.0, 1.0);
        let m = a.lerp(b, 0.5);
        assert!(close(m.r, 0.5) && close(m.g, 0.3) && close(m.b, 0.5) && close(m.a, 0.5));
    }

    #[test]
    fn a_controller_runs_forward_and_back() {
        let t0 = Instant::now();
        let mut c = Controller::new(250 * MS, 250 * MS);
        assert_eq!(c.status(t0), Status::Dismissed);
        c.forward(t0);
        assert_eq!(c.value(t0), 0.0);
        assert!(close(c.value(t0 + 125 * MS), 0.5));
        assert_eq!(c.status(t0 + 125 * MS), Status::Forward);
        assert_eq!(c.value(t0 + 250 * MS), 1.0);
        assert_eq!(c.status(t0 + 250 * MS), Status::Completed);
        let t1 = t0 + 300 * MS;
        c.reverse(t1);
        assert_eq!(c.status(t1), Status::Reverse);
        assert!(close(c.value(t1 + 125 * MS), 0.5));
        assert_eq!(c.status(t1 + 250 * MS), Status::Dismissed);
    }

    #[test]
    fn a_controller_reversed_part_way_takes_that_share_of_its_duration() {
        let t0 = Instant::now();
        let mut c = Controller::new(200 * MS, 400 * MS);
        c.forward(t0);
        let t1 = t0 + 100 * MS;
        c.reverse(t1);
        assert!(close(c.value(t1), 0.5));
        assert_eq!(c.ends_at(t1), t1 + 200 * MS, "half of the reverse");
        assert!(close(c.value(t1 + 100 * MS), 0.25));
        assert_eq!(c.value(t1 + 200 * MS), 0.0);
    }

    #[test]
    fn a_reverse_begun_mid_run_keeps_the_forward_curve() {
        let t0 = Instant::now();
        let mut c = Controller::new(100 * MS, 100 * MS);
        c.forward(t0);
        c.reverse(t0 + 50 * MS);
        let at = t0 + 75 * MS;
        let curved = c.curved(Curve::EaseOut, Some(Curve::Linear), at);
        assert!(close(curved, Curve::EaseOut.transform(c.value(at))));
        // From rest, the reverse curve.
        let t1 = t0 + 500 * MS;
        c.forward(t1);
        let t2 = t1 + 200 * MS;
        c.reverse(t2);
        let at = t2 + 30 * MS;
        assert!(close(
            c.curved(Curve::EaseOut, Some(Curve::Linear), at),
            c.value(at)
        ));
    }

    #[test]
    fn the_frames_run_only_while_something_animates() {
        use iced::advanced::subscription::into_recipes;
        assert!(into_recipes(frames(false)).is_empty());
        assert_eq!(into_recipes(frames(true)).len(), 1);
    }
}
