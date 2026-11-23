//! A one-pole parameter smoother. Moves `cur` toward `target` each sample so a control
//! change (gain, pan, threshold, makeup, wet) never steps instantaneously → no zipper. Plus
//! the short cross-fade every insert's bypass switch uses ([`BypassFade`]).

#[derive(Clone)]
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
    ///
    /// A non-finite `t` is ignored (the current target stands): the one-pole recurrence never
    /// recovers from one — a NaN stays NaN, and an infinity turns into NaN on the next finite
    /// target (`inf + (x - inf) * c`) — so a single bad value used to silence the node, or
    /// poison everything downstream, for the rest of the session.
    #[inline]
    pub fn set_target(&mut self, t: f32) {
        if !t.is_finite() {
            return;
        }
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

    /// The value every [`next`](Self::next) of a coming `frames`-frame block returns, when that
    /// is one value: the ramp has landed — a step leaves it bit for bit where it is (exactly at
    /// the target, or where rounding stalls it within an ulp or so of it), so it stays there
    /// for the whole block. The caller then applies it as a constant instead of stepping per
    /// sample, which is the same arithmetic without the per-sample dependency chain (a
    /// subtract, a multiply and an add, ~12 cycles of latency per sample, set the speed of
    /// every gain, pan and send). Marks the value used, as `next` would. `None` while the ramp
    /// is still moving, or for an empty block (no `next` would run at all).
    #[inline]
    pub fn settled(&mut self, frames: usize) -> Option<f32> {
        if frames == 0 {
            return None;
        }
        let step = self.cur + (self.target - self.cur) * self.coef;
        if step.to_bits() == self.cur.to_bits() {
            self.live = true;
            Some(self.cur)
        } else {
            None
        }
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
#[derive(Clone)]
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

    /// Land an in-flight fade at the switch position (a seek: the jump is a discontinuity
    /// anyway). A node's reset() must call it — a fade-out left running across a seek kept
    /// processing (and, for a delay, writing post-seek audio into a line the reset had just
    /// cleared and nothing would clear again).
    pub fn settle(&mut self) {
        self.mix = if self.on { 1.0 } else { 0.0 };
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
        self.mix = self.stepped();
        self.mix
    }

    /// The share every [`next`](Self::next) of the coming block returns, when the fade has
    /// landed (see [`Smoothed::settled`]); `None` while it is moving.
    #[inline]
    pub fn settled(&self) -> Option<f32> {
        (self.stepped().to_bits() == self.mix.to_bits()).then_some(self.mix)
    }

    #[inline]
    fn stepped(&self) -> f32 {
        if self.on {
            (self.mix + self.step).min(1.0)
        } else {
            (self.mix - self.step).max(0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `settled` answers exactly "would `next` leave the value where it is": whenever it holds
    /// a value, every `next` returns that value bit for bit and moves nothing; whenever it does
    /// not, the very next step moves the value. Ramps up, down, to the bit-adjacent value,
    /// between signed zeros, and onto values where rounding stalls the ramp short of its target.
    #[test]
    fn settled_holds_exactly_what_next_returns() {
        let cases: [(f32, f32, f32); 9] = [
            (0.0, 1.0, 0.01),
            (1.0, 0.0, 0.01),
            (0.25, f32::from_bits(0.25f32.to_bits() + 1), 0.01),
            (-3.0, 7.5, 0.002),
            (1.0, 1.0, 0.01),
            (0.0, -0.0, 0.01),
            (-0.0, 0.0, 0.01),
            (1.0e-30, 1.0, 0.03),
            (0.707, 0.708, 0.5),
        ];
        for (start, target, tau) in cases {
            let mut s = Smoothed::new(start);
            s.set_time(tau, 48_000.0);
            let _ = s.next(); // heard: a later target ramps instead of jumping
            s.set_target(target);
            let mut landed = false;
            for _ in 0..200_000 {
                let mut probe = s.clone();
                match probe.settled(64) {
                    Some(v) => {
                        assert!(probe.live, "settled must mark the value used, as next does");
                        let mut stepped = s.clone();
                        for _ in 0..64 {
                            assert_eq!(stepped.next().to_bits(), v.to_bits());
                        }
                        assert_eq!(stepped.cur.to_bits(), s.cur.to_bits());
                        landed = true;
                        break;
                    }
                    None => {
                        let before = s.cur;
                        assert_ne!(s.next().to_bits(), before.to_bits(), "None, yet it held");
                    }
                }
            }
            assert!(landed, "{start} -> {target} never landed");
        }
        // An empty block runs no step, so it claims nothing (and does not mark the value used).
        let mut s = Smoothed::new(0.5);
        assert_eq!(s.settled(0), None);
        assert!(!s.live);
    }

    #[test]
    fn bypass_settled_holds_exactly_what_next_returns() {
        for (bypassed_at_start, switch_to) in [(false, true), (true, false), (false, false)] {
            let mut f = BypassFade::new(bypassed_at_start);
            f.prepare(48_000.0);
            let _ = f.begin_block();
            f.set_bypass(switch_to);
            let mut landed = false;
            for _ in 0..10_000 {
                match f.settled() {
                    Some(v) => {
                        let mut stepped = f.clone();
                        for _ in 0..64 {
                            assert_eq!(stepped.next().to_bits(), v.to_bits());
                        }
                        landed = true;
                        break;
                    }
                    None => {
                        let before = f.mix;
                        assert_ne!(f.next().to_bits(), before.to_bits());
                    }
                }
            }
            assert!(landed);
        }
    }
}
