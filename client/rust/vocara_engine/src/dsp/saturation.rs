//! Soft saturation / harmonic exciter: a `tanh` waveshaper with a drive control and unity-ish
//! makeup, blended wet/dry. Adds warmth + harmonics. Memoryless → unconditionally stable.

use crate::dsp::smooth::{BypassFade, Smoothed};
use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const DRIVE_DB: u16 = 0;
    pub const MIX: u16 = 1; // 0..1 wet
    pub const BYPASS: u16 = 2;
}

pub struct SaturationNode {
    /// Smoothed per BLOCK: the waveshaper's constants are evaluated at each block's edges
    /// and interpolated across it (see `process`), so a Drive drag glides instead of
    /// stepping without paying a `powf` + `tanh` per sample.
    drive_db: Smoothed,
    mix: Smoothed,
    fade: BypassFade,
}

/// The waveshaper's constants for a drive (dB): the linear drive gain `d` and the normaliser
/// `1 / tanh(d)` that keeps a full-scale input ~full-scale. Evaluated per block, not per
/// sample — computing the `powf` and the extra `tanh` per sample made this the costliest
/// insert in the chain.
fn shape_consts(drive_db: f32) -> (f32, f32) {
    let d = db_to_lin(drive_db).max(1.0);
    (d, 1.0 / d.tanh())
}

impl SaturationNode {
    pub fn new() -> Self {
        SaturationNode {
            drive_db: Smoothed::new(6.0),
            mix: Smoothed::new(0.0),
            fade: BypassFade::new(true),
        }
    }
}

impl Default for SaturationNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for SaturationNode {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.drive_db.set_time(0.01, sample_rate);
        self.mix.set_time(0.01, sample_rate);
        self.fade.prepare(sample_rate);
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
        // The drive's constants at this block's start and end, interpolated across it.
        let (d0, n0) = shape_consts(self.drive_db.current());
        let (d1, n1) = shape_consts(self.drive_db.advance(frames));
        let step = 1.0 / frames as f32;
        for i in 0..frames {
            let t = (i + 1) as f32 * step;
            let d = d0 + (d1 - d0) * t;
            let norm = n0 + (n1 - n0) * t;
            let mix = self.mix.next();
            // Blended toward the dry input while the bypass switch fades.
            let f = self.fade.next();
            for s in &mut out[CHANNELS * i..CHANNELS * (i + 1)] {
                let dry = *s;
                let wet = dry * (1.0 - mix) + (d * dry).tanh() * norm * mix;
                *s = dry + (wet - dry) * f;
            }
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::DRIVE_DB => self.drive_db.set_target(value.clamp(0.0, 36.0)),
            param::MIX => self.mix.set_target(value.clamp(0.0, 1.0)),
            param::BYPASS => self.fade.set_bypass(value >= 0.5),
            _ => {}
        }
    }
}
