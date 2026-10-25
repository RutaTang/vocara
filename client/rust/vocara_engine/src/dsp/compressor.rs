//! Feed-forward, log-domain, soft-knee compressor with a stereo-linked detector.
//!
//! Detector → dB → C¹-continuous soft-knee gain computer → branching attack/release
//! ballistics on the gain-reduction envelope → makeup. The knee parabola's value and slope
//! match the outer segments at both edges, so there is no corner-pumping. No recursive
//! audio path → unconditionally stable. Reports peak gain-reduction for a meter.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::dsp::denormals::sanitize;
use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const THRESHOLD_DB: u16 = 0;
    pub const RATIO: u16 = 1;
    pub const KNEE_DB: u16 = 2;
    pub const ATTACK_MS: u16 = 3;
    pub const RELEASE_MS: u16 = 4;
    pub const MAKEUP_DB: u16 = 5;
    pub const BYPASS: u16 = 6;
}

pub struct CompressorNode {
    threshold: Smoothed,
    ratio: Smoothed,
    knee: Smoothed,
    makeup: Smoothed,
    attack_ms: f32,
    release_ms: f32,
    attack_coef: f32,
    release_coef: f32,
    env_db: f32, // current gain reduction in dB, <= 0
    gr_meter: f32,
    fade: BypassFade,
    sr: f64,
    // Publishes the block's peak gain reduction (dB, >= 0, f32 bits) for a UI meter.
    meter: Option<Arc<AtomicU32>>,
}

impl CompressorNode {
    pub fn new(
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
    ) -> Self {
        let mut c = CompressorNode {
            threshold: Smoothed::new(threshold_db),
            ratio: Smoothed::new(ratio.max(1.0)),
            knee: Smoothed::new(knee_db.max(0.0)),
            makeup: Smoothed::new(makeup_db),
            attack_ms,
            release_ms,
            attack_coef: 1.0,
            release_coef: 1.0,
            env_db: 0.0,
            gr_meter: 0.0,
            fade: BypassFade::new(false),
            sr: 48_000.0,
            meter: None,
        };
        c.recompute_ballistics();
        c
    }

    /// Attach a shared gain-reduction meter (the node writes its block peak to it).
    pub fn with_meter(mut self, meter: Option<Arc<AtomicU32>>) -> Self {
        self.meter = meter;
        self
    }

    /// A gentle default: light vocal leveling, bypassed until the user dials it.
    pub fn default_vocal() -> Self {
        let mut c = CompressorNode::new(-18.0, 3.0, 6.0, 10.0, 120.0, 0.0);
        c.fade = BypassFade::new(true);
        c
    }

    fn recompute_ballistics(&mut self) {
        let a = self.attack_ms.max(0.01) * 0.001 * self.sr as f32;
        let r = self.release_ms.max(0.01) * 0.001 * self.sr as f32;
        self.attack_coef = 1.0 - (-1.0 / a).exp();
        self.release_coef = 1.0 - (-1.0 / r).exp();
    }

    /// Peak gain reduction (dB, >= 0) seen during the last processed block — for a meter.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_meter
    }
}

impl Node for CompressorNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sr = sample_rate;
        let tau = 0.01;
        self.threshold.set_time(tau, sample_rate);
        self.ratio.set_time(tau, sample_rate);
        self.knee.set_time(tau, sample_rate);
        self.makeup.set_time(tau, sample_rate);
        self.fade.prepare(sample_rate);
        self.recompute_ballistics();
        self.env_db = 0.0;
    }

    fn reset(&mut self) {
        // Seek clears every node's state. Without this override (the inherited
        // no-op) the held gain-reduction envelope carries across the discontinuity
        // and recovers only over the attack/release time — an audible momentary
        // wrong level right after a seek. Clear it to the idle value prepare()
        // uses (mirrors deesser.rs's reset). gr_meter is recomputed per block and
        // the Smoothed params target-track on seek, so only env_db is stale.
        self.env_db = 0.0;
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
            // The smoothers and envelope hold while bypassed; set_param(BYPASS)
            // clears the envelope on re-enabling from here, so it starts idle.
            if let Some(m) = &self.meter {
                m.store(0, Ordering::Relaxed);
            }
            return;
        }

        self.gr_meter = 0.0;
        for i in 0..frames {
            let l = out[CHANNELS * i];
            let r = out[CHANNELS * i + 1];

            let t = self.threshold.next();
            let ratio = self.ratio.next().max(1.0);
            let w = self.knee.next().max(0.0);
            let mk = self.makeup.next();

            // Stereo-linked detector (peak).
            let peak = l.abs().max(r.abs());
            let x_db = 20.0 * (peak + 1.0e-12).log10();

            // Soft-knee gain computer → reduction (dB, <= 0).
            let over = x_db - t;
            let slope = 1.0 / ratio - 1.0; // <= 0
            let gain_db = if w <= 1.0e-3 {
                if over <= 0.0 {
                    0.0
                } else {
                    slope * over
                }
            } else if 2.0 * over < -w {
                0.0
            } else if 2.0 * over > w {
                slope * over
            } else {
                let knee_term = over + 0.5 * w;
                slope * knee_term * knee_term / (2.0 * w)
            };

            // Branching ballistics on the reduction envelope.
            let coef = if gain_db < self.env_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            // Sanitize the recursive envelope so one non-finite input sample (Inf, or NaN
            // in both channels) can't poison env_db for the rest of playback — mirrors the
            // biquad/reverb invariant in denormals.rs.
            self.env_db = sanitize(self.env_db + (gain_db - self.env_db) * coef);

            // Blended toward unity while the bypass switch fades.
            let g = 1.0 + (db_to_lin(self.env_db + mk) - 1.0) * self.fade.next();
            out[CHANNELS * i] = l * g;
            out[CHANNELS * i + 1] = r * g;

            let reduction = -self.env_db;
            if reduction > self.gr_meter {
                self.gr_meter = reduction;
            }
        }

        if let Some(m) = &self.meter {
            m.store(self.gr_meter.to_bits(), Ordering::Relaxed);
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::THRESHOLD_DB => self.threshold.set_target(value),
            param::RATIO => self.ratio.set_target(value.max(1.0)),
            param::KNEE_DB => self.knee.set_target(value.max(0.0)),
            param::ATTACK_MS => {
                self.attack_ms = value;
                self.recompute_ballistics();
            }
            param::RELEASE_MS => {
                self.release_ms = value;
                self.recompute_ballistics();
            }
            param::MAKEUP_DB => self.makeup.set_target(value),
            param::BYPASS => {
                let bypass = value >= 0.5;
                // The envelope froze at whatever the material was doing when the node
                // went fully bypassed; re-enabling over a quieter passage would duck it
                // by that stale reduction for a whole release time. Start idle, as
                // reset() does after a seek. (Switched back on mid-fade-out, the
                // envelope is still live and is kept.)
                if !bypass && self.fade.idle() {
                    self.env_db = 0.0;
                }
                self.fade.set_bypass(bypass);
            }
            _ => {}
        }
    }
}
