//! A one-pole parameter smoother. Moves `cur` toward `target` each sample so a control
//! change (gain, pan, threshold, makeup, wet) never steps instantaneously → no zipper.

pub struct Smoothed {
    cur: f32,
    target: f32,
    coef: f32,
}

impl Smoothed {
    pub fn new(v: f32) -> Self {
        Smoothed {
            cur: v,
            target: v,
            coef: 1.0,
        }
    }

    /// Set the smoothing time constant (seconds) at sample rate `sr`.
    pub fn set_time(&mut self, tau_s: f32, sr: f64) {
        self.coef = 1.0 - (-1.0 / (tau_s.max(1.0e-5) * sr as f32)).exp();
    }

    #[inline]
    pub fn set_target(&mut self, t: f32) {
        self.target = t;
    }

    /// Jump immediately to `v` (used during pre-roll / reset, no ramp).
    pub fn snap(&mut self, v: f32) {
        self.cur = v;
        self.target = v;
    }

    /// Advance one sample toward the target and return the new value. (Named `next`
    /// deliberately for read flow; it is not an `Iterator`.)
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.cur += (self.target - self.cur) * self.coef;
        self.cur
    }

    #[inline]
    pub fn current(&self) -> f32 {
        self.cur
    }
}
