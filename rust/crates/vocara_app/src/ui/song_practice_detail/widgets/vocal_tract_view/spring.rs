//! Critically damped springs: how the figure moves from one estimate to the
//! next. Each number of the pose follows its target as a critically damped
//! second-order system (no overshoot, its velocity carried across new
//! targets), stepped exactly for any frame time, so motion is smooth however
//! irregularly estimates and frames arrive.

use std::time::Duration;

use iced::time::Instant;

use super::pose::TractPose;

/// How long the figure takes to cover 90 % of the way to a new estimate.
pub const POSE_SETTLE: Duration = Duration::from_millis(70);

/// How long the figure takes to dim (or light up again) as the voice stops
/// (or starts).
pub const REST_SETTLE: Duration = Duration::from_millis(260);

/// The angular frequency of a critically damped spring that covers 90 % of
/// a step in `settle`: its step response is 1 − (1 + ωt)e^(−ωt).
pub fn omega(settle: Duration) -> f32 {
    3.89 / settle.as_secs_f32().max(1e-3)
}

/// Steps a critically damped spring (`x`, `v`) toward `goal` by `dt` s,
/// exactly.
pub fn step(x: &mut f32, v: &mut f32, goal: f32, omega: f32, dt: f32) {
    let c1 = *x - goal;
    let c2 = *v + omega * c1;
    let e = (-omega * dt).exp();
    *x = goal + (c1 + c2 * dt) * e;
    *v = (c2 - omega * (c1 + c2 * dt)) * e;
}

/// One number on a spring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub goal: f32,
    omega: f32,
}

impl Spring {
    pub fn new(value: f32, settle: Duration) -> Self {
        Self {
            value,
            velocity: 0.0,
            goal: value,
            omega: omega(settle),
        }
    }

    pub fn advance(&mut self, dt: f32) {
        step(
            &mut self.value,
            &mut self.velocity,
            self.goal,
            self.omega,
            dt,
        );
        if self.settled() {
            self.value = self.goal;
            self.velocity = 0.0;
        }
    }

    pub fn settled(&self) -> bool {
        (self.value - self.goal).abs() < 1e-4 && self.velocity.abs() < 1e-3
    }

    /// Puts it at `value` at once, at rest.
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.goal = value;
        self.velocity = 0.0;
    }
}

/// The pose on screen, following the estimates (see the module docs). An
/// unvoiced estimate leaves a voiced shape where it is and lets the figure
/// dim ([`PoseSpring::rest`]); until a first voiced one, the figure follows
/// the unvoiced estimates (a resting shape to start from).
#[derive(Debug, Default)]
pub struct PoseSpring {
    /// The last voiced estimate: the layout and what is not a number.
    template: Option<TractPose>,
    x: Vec<f32>,
    v: Vec<f32>,
    goal: Vec<f32>,
    rest: Option<Spring>,
    voiced: bool,
    /// Whether a voiced estimate has come (the shape held from then on).
    seen_voiced: bool,
    last: Option<Instant>,
}

/// Below this, a number has arrived (cm, Hz, radians alike: far below a
/// pixel).
const ARRIVED: f32 = 1e-3;

impl PoseSpring {
    /// Takes in an estimate made at `now`.
    pub fn push(&mut self, pose: TractPose, now: Instant) {
        self.advance(now);
        let rest = self
            .rest
            .get_or_insert_with(|| Spring::new(if pose.voiced { 0.0 } else { 1.0 }, REST_SETTLE));
        rest.goal = if pose.voiced { 0.0 } else { 1.0 };
        self.voiced = pose.voiced;
        if !pose.voiced && self.seen_voiced {
            // Hold the last voiced shape.
            return;
        }
        self.seen_voiced |= pose.voiced;
        let goal = pose.values();
        match &self.template {
            Some(shown) if shown.layout() == pose.layout() => {}
            Some(shown) => {
                // Another layout: move on from the shape shown, resampled.
                let now_shown = shown.with_values(&self.x);
                self.x = now_shown.resampled(&pose.layout()).values();
                self.v = vec![0.0; self.x.len()];
            }
            None => {
                self.x = goal.clone();
                self.v = vec![0.0; goal.len()];
            }
        }
        self.goal = goal;
        self.template = Some(pose);
    }

    /// Moves everything on to `now`.
    pub fn advance(&mut self, now: Instant) {
        let dt = self
            .last
            .map_or(0.0, |last| {
                now.saturating_duration_since(last).as_secs_f32()
            })
            .min(0.25);
        self.last = Some(now);
        if dt <= 0.0 {
            return;
        }
        let w = omega(POSE_SETTLE);
        let mut moving = false;
        for ((x, v), g) in self.x.iter_mut().zip(&mut self.v).zip(&self.goal) {
            step(x, v, *g, w, dt);
            if (*x - g).abs() > ARRIVED || v.abs() > ARRIVED {
                moving = true;
            }
        }
        if !moving {
            self.x.clone_from(&self.goal);
            self.v.iter_mut().for_each(|v| *v = 0.0);
        }
        if let Some(rest) = &mut self.rest {
            rest.advance(dt);
        }
    }

    /// Whether anything still moves (the screen asks for frames while so).
    pub fn animating(&self) -> bool {
        let moving = self
            .x
            .iter()
            .zip(&self.v)
            .zip(&self.goal)
            .any(|((x, v), g)| (x - g).abs() > ARRIVED || v.abs() > ARRIVED);
        moving || self.rest.is_some_and(|r| !r.settled())
    }

    /// The pose on screen, if any estimate came yet. The pitch is the
    /// estimate's, not on a spring: a note read mid-glide (from no voice,
    /// 0 Hz, up to the sung one) would name notes nobody sang.
    pub fn shown(&self) -> Option<TractPose> {
        let template = self.template.as_ref()?;
        let mut pose = template.with_values(&self.x);
        pose.voiced = self.voiced;
        pose.components.f0_hz = template.components.f0_hz;
        Some(pose)
    }

    /// How far the figure has dimmed into rest: 0 voiced, 1 resting.
    pub fn rest(&self) -> f32 {
        self.rest.map_or(1.0, |r| r.value.clamp(0.0, 1.0))
    }

    /// Whether the last estimate was voiced.
    pub fn voiced(&self) -> bool {
        self.voiced
    }

    /// Whether a voiced estimate has come since the last clear: a shape to
    /// hold.
    pub fn seen_voiced(&self) -> bool {
        self.seen_voiced
    }

    /// Forgets every estimate (the figure goes back to its resting look).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
