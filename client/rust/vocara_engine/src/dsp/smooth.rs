//! A one-pole parameter smoother. Moves `cur` toward `target` each sample so a control
//! change (gain, pan, threshold, makeup, wet) never steps instantaneously → no zipper. Plus
//! the short cross-fade every insert's bypass switch uses ([`BypassFade`]).

pub struct Smoothed {
    cur: f32,
    target: f32,
    coef: f32,
    /// Whether the value has been used for audio yet (any [`next`](Self::next) call).
    live: bool,
}

impl Smoothed {
    pub fn new(v: f32) -> Self {
        Smoothed {
            cur: v,
            target: v,
            coef: 1.0,
            live: false,
        }
    }

    /// Set the smoothing time constant (seconds) at sample rate `sr`.
    pub fn set_time(&mut self, tau_s: f32, sr: f64) {
        self.coef = 1.0 - (-1.0 / (tau_s.max(1.0e-5) * sr as f32)).exp();
    }

    /// Ramp toward `t` — or jump straight to it while nothing has been heard yet. A rebuilt
    /// graph starts every node at its build defaults and the host then pushes each live
    /// value; ramping those in made the first ~30 ms after every rebuild glide from the
    /// defaults (a quarter-volume master at up to +12 dB, pans sweeping in from the centre).
    #[inline]
    pub fn set_target(&mut self, t: f32) {
        self.target = t;
        if !self.live {
            self.cur = t;
        }
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
        self.live = true;
        self.cur += (self.target - self.cur) * self.coef;
        self.cur
    }

    #[inline]
    pub fn current(&self) -> f32 {
        self.cur
    }

    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Advance `n` samples at once (the closed form of `n` calls to [`next`](Self::next))
    /// and return the new value — for a control evaluated once per block, not per sample.
    pub fn advance(&mut self, n: usize) -> f32 {
        if n > 0 {
            self.live = true;
            let keep = (1.0 - self.coef).powi(n.min(i32::MAX as usize) as i32);
            self.cur = self.target + (self.cur - self.target) * keep;
        }
        self.cur
    }
}

/// How long an insert's on/off switch takes to cross-fade.
const BYPASS_FADE_S: f32 = 0.005;

/// A short linear cross-fade for an insert's on/off switch. Bypass used to be a hard switch:
/// turning a working compressor, a driven saturator or a ringing delay off (or on) stepped
/// the output straight from one signal to the other — a click on every A/B toggle. A node
/// outputs `dry + (processed - dry) * fade` with the per-sample value [`next`](Self::next)
/// returns, and skips its processing entirely while [`begin_block`](Self::begin_block)
/// reports it idle (switched off and fully faded out).
///
/// Like [`Smoothed`], a switch set before the node has processed any audio applies at once:
/// there is nothing audible to fade from.
pub struct BypassFade {
    /// 1 = fully processed, 0 = fully dry.
    mix: f32,
    on: bool,
    step: f32,
    live: bool,
}

impl BypassFade {
    pub fn new(bypassed: bool) -> Self {
        BypassFade {
            mix: if bypassed { 0.0 } else { 1.0 },
            on: !bypassed,
            step: 1.0 / (BYPASS_FADE_S * 48_000.0),
            live: false,
        }
    }

    /// Size the fade for the node's sample rate (call from `prepare`).
    pub fn prepare(&mut self, sample_rate: f64) {
        self.step = 1.0 / (BYPASS_FADE_S * sample_rate as f32).max(1.0);
    }

    pub fn set_bypass(&mut self, bypassed: bool) {
        self.on = !bypassed;
        if !self.live {
            self.mix = if self.on { 1.0 } else { 0.0 };
        }
    }

    /// The switch position, whatever the fade's progress.
    pub fn bypassed(&self) -> bool {
        !self.on
    }

    /// Switched off and fully faded out: the node passes its input through untouched.
    pub fn idle(&self) -> bool {
        !self.on && self.mix == 0.0
    }

    /// Call first thing in every `process`: audio is flowing from here on (so a later switch
    /// fades). Returns [`idle`](Self::idle).
    #[inline]
    pub fn begin_block(&mut self) -> bool {
        self.live = true;
        self.idle()
    }

    /// Advance one sample and return the processed signal's share. (Named `next` like
    /// [`Smoothed::next`]; it is not an `Iterator`.)
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.mix = if self.on {
            (self.mix + self.step).min(1.0)
        } else {
            (self.mix - self.step).max(0.0)
        };
        self.mix
    }
}
