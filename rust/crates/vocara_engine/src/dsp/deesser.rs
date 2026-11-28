//! De-esser: a dynamic high shelf for sibilance.
//!
//! A 2nd-order high-pass at the detector frequency measures the sibilance band; when its
//! level exceeds the threshold, a high shelf cornered at the same frequency cuts by the
//! overshoot (clamped to `range`), so the body of the voice below the corner is left alone.
//! The shelf is RBJ's with slope S = 1, the steepest that stays monotonic: its response falls
//! from unity below the corner to the reduction above it, so a reduction never boosts any
//! frequency. At 0 dB its numerator equals its denominator, so an idle de-esser is exactly
//! its input: a bypass fade or a Freq change at idle is transparent.
//!
//! (The two splits this replaced each broke one of those: `x - HP2(x)` was the input when
//! idle but lifted 3–7 kHz by up to ~2 dB while cutting, and an LR4 crossover never boosted
//! but summed to an allpass, not to its input — fades between dry and processed, and Freq
//! drags, cancelled a tone at the corner by up to ~34 dB even with nothing over threshold.)

use crate::dsp::biquad::{BandKind, BandSpec, Biquad, Coeffs};
use crate::dsp::denormals::sanitize;
use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::CHANNELS;

pub mod param {
    pub const FREQ: u16 = 0; // shelf corner / detector frequency (Hz)
    pub const THRESHOLD_DB: u16 = 1;
    pub const RANGE_DB: u16 = 2; // maximum reduction (dB, >= 0)
    pub const BYPASS: u16 = 3;
}

/// Cross-fade length (frames) when the detector frequency changes, so swapping the
/// coefficients of the stateful filters in place does not produce a zipper/click. Mirrors
/// the EQ `Band` cross-fade in `biquad.rs`.
const XFADE_FRAMES: f32 = 256.0;

/// One biquad history, driven with coefficients held by its [`Bank`] (shared by L and R).
#[derive(Clone, Copy, Default)]
struct History {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl History {
    #[inline]
    fn tick(&mut self, c: &Coeffs, x: f32) -> f32 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        let y = sanitize(y);
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// What the 0 dB shelf computes (numerator equals denominator), without its rounding:
    /// idle stays bit-exact, and a reduction that starts later continues from the history
    /// the identity filter would have left.
    #[inline]
    fn pass(&mut self, x: f32) -> f32 {
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = x;
        x
    }
}

/// Everything tuned to one corner frequency: both channels' detector high-pass and shelf.
/// `Copy` so a retune can seed its cross-fade target from the current bank — a value copy
/// that keeps the filters' history.
#[derive(Clone, Copy)]
struct Bank {
    det_l: Biquad,
    det_r: Biquad,
    /// The shelf's corner terms — with S = 1 its alpha does not depend on the gain, so a
    /// reduction change only recomputes A — and its coefficients at the current reduction.
    cosw: f32,
    alpha: f32,
    shelf: Coeffs,
    shelf_l: History,
    shelf_r: History,
}

impl Bank {
    fn new() -> Self {
        Bank {
            det_l: Biquad::new(Coeffs::PASS),
            det_r: Biquad::new(Coeffs::PASS),
            cosw: 1.0,
            alpha: 0.0,
            shelf: Coeffs::PASS,
            shelf_l: History::default(),
            shelf_r: History::default(),
        }
    }

    fn tune(&mut self, freq: f32, sr: f64, red_db: f32) {
        // The detector: a Butterworth (Q 0.707) high-pass, the signal the de-esser has
        // always detected on, so thresholds and presets keep their calibration.
        let hp = BandSpec {
            kind: BandKind::HighPass,
            freq,
            q: 0.707,
            gain_db: 0.0,
            enabled: true,
        }
        .coeffs(sr);
        self.det_l.set_coeffs(hp);
        self.det_r.set_coeffs(hp);
        let f0 = (freq as f64).clamp(10.0, sr * 0.45); // BandSpec's own guard
        let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
        self.cosw = w0.cos() as f32;
        self.alpha = (w0.sin() * std::f64::consts::FRAC_1_SQRT_2) as f32; // S = 1
        self.set_reduction(red_db);
    }

    /// RBJ high-shelf coefficients for a cut of `red_db` (<= 0).
    fn set_reduction(&mut self, red_db: f32) {
        let sqrt_a = 10f32.powf(red_db / 80.0);
        let a = sqrt_a * sqrt_a; // 10^(G/40)
        let (c, s) = (self.cosw, 2.0 * sqrt_a * self.alpha);
        let inv_a0 = 1.0 / ((a + 1.0) - (a - 1.0) * c + s);
        self.shelf = Coeffs {
            b0: a * ((a + 1.0) + (a - 1.0) * c + s) * inv_a0,
            b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * c) * inv_a0,
            b2: a * ((a + 1.0) + (a - 1.0) * c - s) * inv_a0,
            a1: 2.0 * ((a - 1.0) - (a + 1.0) * c) * inv_a0,
            a2: ((a + 1.0) - (a - 1.0) * c - s) * inv_a0,
        };
    }

    /// The detector signal for one frame.
    #[inline]
    fn detect(&mut self, l: f32, r: f32) -> (f32, f32) {
        (self.det_l.tick(l), self.det_r.tick(r))
    }

    /// The shelf's output for one frame; `flat` (no reduction) is the input, exactly.
    #[inline]
    fn shelve(&mut self, l: f32, r: f32, flat: bool) -> (f32, f32) {
        if flat {
            (self.shelf_l.pass(l), self.shelf_r.pass(r))
        } else {
            (
                self.shelf_l.tick(&self.shelf, l),
                self.shelf_r.tick(&self.shelf, r),
            )
        }
    }

    fn reset_state(&mut self) {
        self.det_l.reset_state();
        self.det_r.reset_state();
        self.shelf_l = History::default();
        self.shelf_r = History::default();
    }
}

pub struct DeEsserNode {
    bank: Bank,
    // The retune target, valid while `xfading`: seeded from the current bank (copies its
    // history) at the new corner, then blended in over XFADE_FRAMES.
    bank2: Bank,
    xfading: bool,
    xmix: f32,
    xinc: f32,
    /// Whether the filters have run on audio. Until then a FREQ change retunes directly:
    /// there is no filter history to cross-fade from.
    heard: bool,
    freq: f32,
    // Smoothed so a threshold/range knob drag or automation ramps the high-band reduction
    // instead of stepping it per block (zipper/click). Mirrors the compressor's threshold.
    threshold: Smoothed,
    range: Smoothed,
    env_db: f32,
    attack_coef: f32,
    release_coef: f32,
    /// The reduction (dB, <= 0) the banks' shelf coefficients were last computed for.
    red_db: f32,
    fade: BypassFade,
    sr: f64,
}

impl DeEsserNode {
    pub fn new() -> Self {
        let mut d = DeEsserNode {
            bank: Bank::new(),
            bank2: Bank::new(),
            xfading: false,
            xmix: 0.0,
            xinc: 1.0 / XFADE_FRAMES,
            heard: false,
            freq: 6500.0,
            threshold: Smoothed::new(-28.0),
            range: Smoothed::new(12.0),
            env_db: -120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            red_db: 0.0,
            fade: BypassFade::new(true),
            sr: 48_000.0,
        };
        d.retune_now();
        d.recompute_ballistics();
        d
    }

    /// Immediate retune (pre-roll: new/prepare, or nothing heard yet) — no cross-fade.
    fn retune_now(&mut self) {
        self.bank.tune(self.freq, self.sr, self.red_db);
        self.bank2 = self.bank;
        self.xfading = false;
        self.xmix = 0.0;
    }

    /// Retune with an output cross-fade — used for a runtime FREQ change so the
    /// coefficient swap on the stateful filters does not click. (At idle both banks pass
    /// the input exactly, so the fade is transparent; it matters while reducing.)
    fn retune_xfade(&mut self) {
        // Collapse an in-flight fade into the current bank so the new fade starts from a
        // defined state.
        if self.xfading {
            self.bank = self.bank2;
        }
        // Seed the target from the current bank (copies history), then retune it.
        self.bank2 = self.bank;
        self.bank2.tune(self.freq, self.sr, self.red_db);
        self.xfading = true;
        self.xmix = 0.0;
    }

    fn recompute_ballistics(&mut self) {
        let a = 0.001 * self.sr as f32; // ~1 ms attack
        let r = 0.060 * self.sr as f32; // ~60 ms release
        self.attack_coef = 1.0 - (-1.0 / a).exp();
        self.release_coef = 1.0 - (-1.0 / r).exp();
    }
}

impl Default for DeEsserNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for DeEsserNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.retune_now();
        self.recompute_ballistics();
        let tau = 0.01;
        self.threshold.set_time(tau, sample_rate);
        self.range.set_time(tau, sample_rate);
        self.fade.prepare(sample_rate);
        self.env_db = -120.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the detector/shelf filter tails and the held sibilance envelope
        // carry pre-seek history across the discontinuity (click + wrong gain
        // reduction). Clear filter state (coeffs intact) and the envelope to its
        // idle value — matching Band::reset and the delay/doubler/reverb resets.
        // Commit a pending coefficient cross-fade into the current filters first,
        // otherwise reset would revert to the OLD coeffs and desync from `freq`.
        if self.xfading {
            self.bank = self.bank2;
            self.xfading = false;
        }
        self.bank.reset_state();
        self.bank2.reset_state();
        self.xmix = 0.0;
        self.env_db = -120.0;
        self.fade.settle();
    }

    /// The detector and shelf histories and the sibilance envelope carry, when both sit at
    /// one corner (a rebuild pushes the setting the old node had; a corner change caught mid
    /// cross-fade restarts, as before).
    fn carry_state(&mut self, old: &mut dyn Node) {
        let Some(old) = crate::node::same_kind::<DeEsserNode>(old) else {
            return;
        };
        if old.sr != self.sr || old.xfading || self.xfading || old.freq != self.freq {
            return;
        }
        self.bank = old.bank;
        self.env_db = old.env_db;
        self.red_db = old.red_db;
        self.heard = old.heard;
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let frames = io.frames;
        let n = frames * CHANNELS;
        let inp = io.inputs.first().copied();
        let out = &mut *io.outputs[0];
        match inp {
            Some(inp) => out[..n].copy_from_slice(&inp[..n]),
            None => {
                for s in &mut out[..n] {
                    *s = 0.0;
                }
            }
        }
        if self.fade.begin_block() {
            return;
        }
        self.heard = true;
        // Landed controls are one value all block (see `Smoothed::settled`).
        let held = (
            self.threshold.settled(frames),
            self.range.settled(frames),
            self.fade.settled(),
        );

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            // Cross-fade the two banks on a freq change so a coefficient swap on the
            // stateful filters doesn't click.
            let m = self.xmix;
            let blend =
                |a: (f32, f32), b: (f32, f32)| (a.0 + (b.0 - a.0) * m, a.1 + (b.1 - a.1) * m);
            let (dl, dr) = if self.xfading {
                blend(self.bank.detect(l, r), self.bank2.detect(l, r))
            } else {
                self.bank.detect(l, r)
            };

            let peak = dl.abs().max(dr.abs());
            let x_db = 20.0 * (peak + 1.0e-12).log10();
            let coef = if x_db > self.env_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.env_db += (x_db - self.env_db) * coef;

            let threshold_db = held.0.unwrap_or_else(|| self.threshold.next());
            let range_db = held.1.unwrap_or_else(|| self.range.next());
            let over = self.env_db - threshold_db;
            let red_db = if over > 0.0 {
                (-over).max(-range_db)
            } else {
                0.0
            };
            // The shelf follows the reduction sample by sample (the envelope's ballistics
            // already smooth it); at 0 dB it is not evaluated at all.
            let flat = red_db == 0.0;
            if red_db != self.red_db {
                if !flat {
                    self.bank.set_reduction(red_db);
                    if self.xfading {
                        self.bank2.set_reduction(red_db);
                    }
                }
                self.red_db = red_db;
            }
            let (pl, pr) = if self.xfading {
                let a = self.bank.shelve(l, r, flat);
                let b = self.bank2.shelve(l, r, flat);
                self.xmix += self.xinc;
                if self.xmix >= 1.0 {
                    self.xmix = 1.0;
                    self.bank = self.bank2;
                    self.xfading = false;
                }
                if flat {
                    (l, r)
                } else {
                    blend(a, b)
                }
            } else {
                self.bank.shelve(l, r, flat)
            };
            // Blended toward the dry input while the bypass switch fades.
            let f = held.2.unwrap_or_else(|| self.fade.next());
            out[CHANNELS * i] = l + (pl - l) * f;
            out[CHANNELS * i + 1] = r + (pr - r) * f;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::FREQ => {
                self.freq = value.clamp(2000.0, 16000.0);
                if self.heard {
                    self.retune_xfade();
                } else {
                    self.retune_now();
                }
            }
            param::THRESHOLD_DB => self.threshold.set_target(value),
            param::RANGE_DB => self.range.set_target(value.max(0.0)),
            param::BYPASS => {
                let bypass = value >= 0.5;
                // The sibilance envelope and detector tails froze with whatever the material was doing when the
                // node went fully bypassed; switched back on over a quieter passage it
                // ducked that by the stale state for a release time. Start idle, as
                // after a seek, like the compressor. (Switched back on mid-fade-out,
                // the state is still live and is kept.)
                if !bypass && self.fade.idle() {
                    self.reset();
                }
                self.fade.set_bypass(bypass);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `process` as it was before the landed threshold/range/fade constants, verbatim.
    fn reference(d: &mut DeEsserNode, inp: &[f32], out: &mut [f32]) {
        let frames = inp.len() / CHANNELS;
        out.copy_from_slice(inp);
        if d.fade.begin_block() {
            return;
        }
        d.heard = true;
        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let m = d.xmix;
            let blend =
                |a: (f32, f32), b: (f32, f32)| (a.0 + (b.0 - a.0) * m, a.1 + (b.1 - a.1) * m);
            let (dl, dr) = if d.xfading {
                blend(d.bank.detect(l, r), d.bank2.detect(l, r))
            } else {
                d.bank.detect(l, r)
            };
            let peak = dl.abs().max(dr.abs());
            let x_db = 20.0 * (peak + 1.0e-12).log10();
            let coef = if x_db > d.env_db {
                d.attack_coef
            } else {
                d.release_coef
            };
            d.env_db += (x_db - d.env_db) * coef;
            let threshold_db = d.threshold.next();
            let range_db = d.range.next();
            let over = d.env_db - threshold_db;
            let red_db = if over > 0.0 {
                (-over).max(-range_db)
            } else {
                0.0
            };
            let flat = red_db == 0.0;
            if red_db != d.red_db {
                if !flat {
                    d.bank.set_reduction(red_db);
                    if d.xfading {
                        d.bank2.set_reduction(red_db);
                    }
                }
                d.red_db = red_db;
            }
            let (pl, pr) = if d.xfading {
                let a = d.bank.shelve(l, r, flat);
                let b = d.bank2.shelve(l, r, flat);
                d.xmix += d.xinc;
                if d.xmix >= 1.0 {
                    d.xmix = 1.0;
                    d.bank = d.bank2;
                    d.xfading = false;
                }
                if flat {
                    (l, r)
                } else {
                    blend(a, b)
                }
            } else {
                d.bank.shelve(l, r, flat)
            };
            let f = d.fade.next();
            out[CHANNELS * i] = l + (pl - l) * f;
            out[CHANNELS * i + 1] = r + (pr - r) * f;
        }
    }

    /// Bit for bit what the old loop rendered: switching on, threshold and range ramps into
    /// and out of reduction, a retune cross-fade while reducing, and the bypass fade-out.
    #[test]
    fn landed_controls_render_exactly_like_the_per_sample_ramps() {
        let edits = [
            (0, param::BYPASS, 0.0),
            (3, param::THRESHOLD_DB, -45.0),
            (9, param::RANGE_DB, 3.0),
            (15, param::FREQ, 3_000.0),
            (16, param::THRESHOLD_DB, -20.0),
            (30, param::RANGE_DB, 20.0),
            (45, param::BYPASS, 1.0),
            (50, param::BYPASS, 0.0),
        ];
        let (mut fast, mut slow) = (DeEsserNode::new(), DeEsserNode::new());
        fast.prepare(48_000.0, 1024);
        slow.prepare(48_000.0, 1024);
        crate::dsp::test_support::assert_matches_reference(
            &mut fast, &mut slow, reference, &edits, 70,
        );
    }
}
