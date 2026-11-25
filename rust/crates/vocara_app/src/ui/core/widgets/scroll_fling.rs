//! The fling a Flutter scroll view ran when a trackpad's drag on it ended,
//! on macOS under the app's `ClampingScrollPhysics` (`app.dart`'s
//! `_NoBounceScrollBehavior`), Flutter 3.38:
//!
//! - the `Scrollable`'s drag recognizer sampled the pan with
//!   `MacOSScrollViewFlingVelocityTracker` ([`MacOsVelocityTracker`]), its
//!   behavior's `velocityTrackerBuilder` on macOS;
//! - at the end it called the drag a fling past the physics' least
//!   distance (`kTouchSlop`) and velocity (`kMinFlingVelocity`), clamped to
//!   `kMaxFlingVelocity` (`considerFling`), and `ScrollDragController.end`
//!   handed the position the opposite of that velocity ([`drag_end`]);
//! - the position went ballistic (`ScrollPositionWithSingleContext.
//!   goBallistic`): `ClampingScrollPhysics.createBallisticSimulation`'s
//!   [`ClampingScrollSimulation`], run by a `BallisticScrollActivity` from
//!   its first frame until it was done or met an end ([`Ballistic`]).
//!
//! Times are the events' (`PointerEvent.timeStamp`), the "has it stopped"
//! stopwatch the time the sample was taken, as Flutter's was.

use iced::time::Instant;

use crate::ui::core::scale_factor::scale_factor;

use super::pan_zoom::TOUCH_SLOP;

/// `kMinFlingVelocity` (px/s).
pub const MIN_FLING_VELOCITY: f64 = 50.0;

/// `kMaxFlingVelocity` (px/s).
pub const MAX_FLING_VELOCITY: f64 = 8000.0;

/// `VelocityTracker._assumePointerMoveStoppedMilliseconds`.
const STOPPED_MS: u128 = 40;

/// `IOSScrollViewFlingVelocityTracker._sampleSize`.
const SAMPLES: usize = 20;

/// What a velocity tracker estimates along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    /// `pixelsPerSecond`.
    pub velocity: f64,
    /// `offset`: from the oldest sample read to the newest.
    pub offset: f64,
}

/// `MacOSScrollViewFlingVelocityTracker` along one axis: the last 20
/// positions, the velocity a weighted mean of the last three steps'.
#[derive(Debug, Clone, Default)]
pub struct MacOsVelocityTracker {
    samples: [Option<(Instant, f64)>; SAMPLES],
    index: usize,
    /// When the last sample was added (`_sinceLastSample`'s start).
    added: Option<Instant>,
}

impl MacOsVelocityTracker {
    /// `addPosition`: the pointer was at `position` at `time` (the
    /// event's), added at `now`.
    pub fn add(&mut self, time: Instant, position: f32, now: Instant) {
        self.added = Some(now);
        self.index = (self.index + 1) % SAMPLES;
        self.samples[self.index] = Some((time, f64::from(position)));
    }

    /// `_previousVelocityAt(index)`: the velocity between the samples
    /// `index` and `index - 1` back from the newest (0 is the newest).
    fn previous_velocity_at(&self, index: isize) -> f64 {
        let at = |i: isize| {
            self.samples[(self.index as isize + i).rem_euclid(SAMPLES as isize) as usize]
        };
        let (Some((end_at, end)), Some((start_at, start))) = (at(index), at(index - 1)) else {
            return 0.0;
        };
        let dt = end_at.saturating_duration_since(start_at).as_micros();
        if dt > 0 {
            (end - start) * 1000.0 / (dt as f64 / 1000.0)
        } else {
            0.0
        }
    }

    /// `getVelocityEstimate` at `now`.
    pub fn estimate(&self, now: Instant) -> Estimate {
        let stopped = self
            .added
            .is_none_or(|added| now.saturating_duration_since(added).as_millis() > STOPPED_MS);
        if stopped {
            return Estimate {
                velocity: 0.0,
                offset: 0.0,
            };
        }
        let velocity = self.previous_velocity_at(-2) * 0.15
            + self.previous_velocity_at(-1) * 0.65
            + self.previous_velocity_at(0) * 0.2;
        let newest = self.samples[self.index];
        let oldest = (1..=SAMPLES).find_map(|i| self.samples[(self.index + i) % SAMPLES]);
        let offset = match (newest, oldest) {
            (Some((_, newest)), Some((_, oldest))) => newest - oldest,
            _ => 0.0,
        };
        Estimate { velocity, offset }
    }
}

/// The velocity a scroll view's drag that ended with `estimate` hands its
/// position (`considerFling` with the physics' `minFlingDistance`,
/// `minFlingVelocity` and `maxFlingVelocity`, then
/// `ScrollDragController.end`'s negation): the scroll's, positive toward
/// the content's end; 0 when the drag was no fling.
pub fn drag_end(estimate: Estimate) -> f64 {
    let fling = estimate.velocity.abs() > MIN_FLING_VELOCITY && estimate.offset.abs() > TOUCH_SLOP;
    if fling {
        -estimate
            .velocity
            .clamp(-MAX_FLING_VELOCITY, MAX_FLING_VELOCITY)
    } else {
        0.0
    }
}

/// `ScrollPhysics.toleranceFor`: a ballistic's least velocity (px/s) and
/// its distance tolerance, by the window's scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    pub velocity: f64,
    pub distance: f64,
}

impl Tolerance {
    /// For the window's scale factor (`devicePixelRatio`).
    pub fn of_window() -> Self {
        let ratio = f64::from(scale_factor());
        Tolerance {
            velocity: 1.0 / (0.050 * ratio),
            distance: 1.0 / ratio,
        }
    }
}

/// `ClampingScrollSimulation` with its default friction (0.015): Android's
/// fling, as Flutter ran it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClampingScrollSimulation {
    position: f64,
    velocity: f64,
    duration: f64,
    distance: f64,
}

/// `_kDecelerationRate`.
fn deceleration_rate() -> f64 {
    0.78f64.ln() / 0.9f64.ln()
}

/// `_kInflexion`.
const INFLEXION: f64 = 0.35;

/// `_physicalCoeff`: 0.84 g in logical pixels per second squared.
const PHYSICAL_COEFF: f64 = 9.80665 * 39.37 * 160.0 * 0.84;

/// `friction`'s default.
const FRICTION: f64 = 0.015;

impl ClampingScrollSimulation {
    /// From `position` at `velocity` (px/s).
    pub fn new(position: f64, velocity: f64) -> Self {
        let rate = deceleration_rate();
        // `_flingDuration`.
        let reference_velocity = FRICTION * PHYSICAL_COEFF / INFLEXION;
        let android_duration = (velocity.abs() / reference_velocity).powf(1.0 / (rate - 1.0));
        let duration = rate * INFLEXION * android_duration;
        // `_flingDistance`.
        let distance = velocity * duration / rate;
        Self {
            position,
            velocity,
            duration,
            distance,
        }
    }

    /// Where it is `t` seconds in.
    pub fn x(&self, t: f64) -> f64 {
        let t = (t / self.duration).clamp(0.0, 1.0);
        self.position + self.distance * (1.0 - (1.0 - t).powf(deceleration_rate()))
    }

    /// Its velocity `t` seconds in.
    pub fn dx(&self, t: f64) -> f64 {
        let t = (t / self.duration).clamp(0.0, 1.0);
        self.velocity * (1.0 - t).powf(deceleration_rate() - 1.0)
    }

    /// Whether it is done `t` seconds in.
    pub fn is_done(&self, t: f64) -> bool {
        t >= self.duration
    }
}

/// A scroll position's `BallisticScrollActivity` under
/// `ClampingScrollPhysics`: from its first frame (a ticker's time starts at
/// its first tick), the simulation's place each frame until it is done or
/// meets an end of `0..=max`, where it stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ballistic {
    simulation: ClampingScrollSimulation,
    start: Option<Instant>,
}

impl Ballistic {
    /// `createBallisticSimulation` for a position at `pixels` in `0..=max`
    /// going at `velocity`: none when it would not move (too slow, or
    /// already at the end it goes toward).
    pub fn new(pixels: f64, max: f64, velocity: f64) -> Option<Self> {
        let tolerance = Tolerance::of_window();
        if velocity.abs() < tolerance.velocity
            || (velocity > 0.0 && pixels >= max)
            || (velocity < 0.0 && pixels <= 0.0)
        {
            return None;
        }
        Some(Self {
            simulation: ClampingScrollSimulation::new(pixels, velocity),
            start: None,
        })
    }

    /// The frame at `now`: the position to set (clamped to `0..=max`) and
    /// whether the run goes on.
    pub fn frame(&mut self, now: Instant, max: f64) -> (f64, bool) {
        let start = *self.start.get_or_insert(now);
        let t = now.saturating_duration_since(start).as_secs_f64();
        let value = self.simulation.x(t);
        let clamped = value.clamp(0.0, max.max(0.0));
        // `applyMoveTo`: an end met stops it (`goIdle`); done, it settles
        // (`goBallistic(0)`, nothing to do in range).
        let more = clamped == value && !self.simulation.is_done(t);
        (clamped, more)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_macos_tracker_weighs_the_last_three_steps() {
        let t0 = Instant::now();
        let mut tracker = MacOsVelocityTracker::default();
        // 10 px per 10 ms, then 20, then 40: 1000, 2000 and 4000 px/s.
        for (i, y) in [0.0, 10.0, 30.0, 70.0].into_iter().enumerate() {
            let at = t0 + 10 * MS * i as u32;
            tracker.add(at, y, at);
        }
        let estimate = tracker.estimate(t0 + 30 * MS);
        let expected = 1000.0 * 0.15 + 2000.0 * 0.65 + 4000.0 * 0.2;
        assert!((estimate.velocity - expected).abs() < 1e-6, "{estimate:?}");
        assert_eq!(estimate.offset, 70.0);
        // 41 ms after the last sample was added: stopped.
        assert_eq!(
            tracker.estimate(t0 + 72 * MS),
            Estimate {
                velocity: 0.0,
                offset: 0.0
            }
        );
    }

    #[test]
    fn a_drag_end_is_a_fling_past_18_px_and_50_px_s_against_the_drag() {
        let fast = |velocity, offset| drag_end(Estimate { velocity, offset });
        assert_eq!(fast(-1000.0, -40.0), 1000.0);
        assert_eq!(fast(1000.0, 17.0), 0.0);
        assert_eq!(fast(40.0, 40.0), 0.0);
        assert_eq!(fast(-20000.0, -400.0), 8000.0);
    }

    #[test]
    fn the_clamping_simulation_slows_to_a_stop_where_android_would() {
        let sim = ClampingScrollSimulation::new(100.0, 1000.0);
        assert_eq!(sim.x(0.0), 100.0);
        assert_eq!(sim.dx(0.0), 1000.0);
        // Its distance is velocity × duration / rate, and it is done then.
        let rate = deceleration_rate();
        assert!((sim.x(sim.duration) - (100.0 + 1000.0 * sim.duration / rate)).abs() < 1e-9);
        assert!(sim.is_done(sim.duration) && !sim.is_done(sim.duration - 0.01));
        assert!(sim.dx(sim.duration / 2.0) < 1000.0);
    }

    #[test]
    fn a_ballistic_starts_at_its_first_frame_and_stops_at_an_end() {
        assert_eq!(Ballistic::new(0.0, 100.0, -500.0), None, "at the top");
        assert_eq!(Ballistic::new(50.0, 100.0, 10.0), None, "too slow");
        let mut run = Ballistic::new(90.0, 100.0, 2000.0).expect("a fling");
        let t0 = Instant::now();
        assert_eq!(run.frame(t0 + 50 * MS, 100.0), (90.0, true));
        let (at, more) = run.frame(t0 + 150 * MS, 100.0);
        assert_eq!((at, more), (100.0, false), "the end stops it");
    }
}
