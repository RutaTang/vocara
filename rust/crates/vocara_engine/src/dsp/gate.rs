//! Noise gate / downward expander: attenuates the signal when its level falls below the
//! threshold (cleans breaths / room tone between phrases). Stereo-linked peak-hold detector
//! with attack/release ballistics on the gain-reduction envelope. No recursive audio path.

use crate::dsp::denormals::sanitize;
use crate::dsp::smooth::BypassFade;
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const THRESHOLD_DB: u16 = 0;
    pub const RANGE_DB: u16 = 1; // attenuation when fully closed (dB, >= 0)
    pub const ATTACK_MS: u16 = 2;
    pub const RELEASE_MS: u16 = 3;
    pub const BYPASS: u16 = 4;
}

const RATIO: f32 = 3.0; // expander slope below the threshold

/// The detector holds each peak this long before letting it fall: longer than half a period
/// of the lowest sung fundamentals (~50 Hz), so the level the threshold sees no longer drops
/// to silence at every zero crossing. Comparing each sample's |x| directly made a low note
/// just over the threshold chatter (~9% THD at 100 Hz with a short release).
const HOLD_S: f64 = 0.010;
/// Decay time constant of the held peak once its hold has lapsed.
const DETECT_DECAY_S: f64 = 0.005;

pub struct GateNode {
    threshold_db: f32,
    range_db: f32,
    attack_ms: f32,
    release_ms: f32,
    attack_coef: f32,
    release_coef: f32,
    env_db: f32, // current gain reduction (dB, <= 0)
    /// The held peak level (linear), the frames its hold has left, and the hold length and
    /// per-sample decay derived from the sample rate.
    det: f32,
    hold: usize,
    hold_frames: usize,
    det_decay: f32,
    fade: BypassFade,
    sr: f64,
}

impl GateNode {
    pub fn new() -> Self {
        let mut g = GateNode {
            threshold_db: -45.0,
            range_db: 60.0,
            attack_ms: 2.0,
            release_ms: 120.0,
            attack_coef: 1.0,
            release_coef: 1.0,
            env_db: 0.0,
            det: 0.0,
            hold: 0,
            hold_frames: 0,
            det_decay: 0.0,
            fade: BypassFade::new(true),
            sr: 48_000.0,
        };
        g.recompute_ballistics();
        g
    }

    fn recompute_ballistics(&mut self) {
        let a = self.attack_ms.max(0.01) * 0.001 * self.sr as f32;
        let r = self.release_ms.max(0.01) * 0.001 * self.sr as f32;
        self.attack_coef = 1.0 - (-1.0 / a).exp();
        self.release_coef = 1.0 - (-1.0 / r).exp();
        self.hold_frames = (HOLD_S * self.sr) as usize;
        self.det_decay = (-1.0 / (DETECT_DECAY_S * self.sr)).exp() as f32;
    }
}

impl Default for GateNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for GateNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        self.recompute_ballistics();
        self.fade.prepare(sample_rate);
        self.env_db = 0.0;
        self.det = 0.0;
        self.hold = 0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (inherited no-op)
        // the held gain-reduction envelope carries across the discontinuity and
        // recovers only over the attack/release time — an audible momentary wrong
        // level right after a seek. Matches CompressorNode/DeEsserNode::reset.
        self.env_db = 0.0;
        self.det = 0.0;
        self.hold = 0;
        self.fade.settle();
    }

    /// The envelope, the held peak and its hold carry: a fresh gate reopened (or shut)
    /// mid-note at every rebuild.
    fn carry_state(&mut self, old: &mut dyn Node) {
        if let Some(old) = crate::node::same_kind::<GateNode>(old) {
            self.env_db = old.env_db;
            self.det = old.det;
            self.hold = old.hold;
        }
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

        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];
            let peak = l.abs().max(r.abs());
            // Peak-hold: a new peak takes over at once; a lower sample only pulls the level
            // down after the hold, and then gradually. (Sanitized: an infinite sample must
            // not pin the gate open for good.)
            if peak >= self.det {
                self.det = sanitize(peak);
                self.hold = self.hold_frames;
            } else if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.det = peak.max(self.det * self.det_decay);
            }
            let x_db = 20.0 * (self.det + 1.0e-12).log10();

            // Below threshold → expand downward; clamp to -range.
            let target_db = if x_db >= self.threshold_db {
                0.0
            } else {
                ((x_db - self.threshold_db) * RATIO).max(-self.range_db)
            };
            // Opening (less reduction) is the attack direction.
            let coef = if target_db > self.env_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            // Defensive: target_db is already kept finite by the `.max(-range_db)` clamp
            // above (f32::max drops NaN, +Inf takes the `>=` branch), so env_db stays finite
            // for finite range_db. Sanitize anyway to honor the biquad/reverb recursive-state
            // invariant in denormals.rs and stay robust to future detector changes.
            self.env_db = sanitize(self.env_db + (target_db - self.env_db) * coef);

            // Blended toward unity while the bypass switch fades.
            let g = 1.0 + (db_to_lin(self.env_db) - 1.0) * self.fade.next();
            out[CHANNELS * i] = l * g;
            out[CHANNELS * i + 1] = r * g;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::THRESHOLD_DB => self.threshold_db = value,
            param::RANGE_DB => self.range_db = value.max(0.0),
            param::ATTACK_MS => {
                self.attack_ms = value;
                self.recompute_ballistics();
            }
            param::RELEASE_MS => {
                self.release_ms = value;
                self.recompute_ballistics();
            }
            param::BYPASS => {
                let bypass = value >= 0.5;
                // The envelope froze with whatever the material was doing when the
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
