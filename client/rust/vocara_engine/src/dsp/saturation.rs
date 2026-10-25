//! Soft saturation / harmonic exciter: a `tanh` waveshaper with a drive control and unity-ish
//! makeup, blended wet/dry. Adds warmth + harmonics. Memoryless → unconditionally stable.

use crate::node::{Node, ProcessIo};
use crate::sample::{db_to_lin, CHANNELS};

pub mod param {
    pub const DRIVE_DB: u16 = 0;
    pub const MIX: u16 = 1; // 0..1 wet
    pub const BYPASS: u16 = 2;
}

pub struct SaturationNode {
    drive_db: f32,
    mix: f32,
    bypass: bool,
}

impl SaturationNode {
    pub fn new() -> Self {
        SaturationNode {
            drive_db: 6.0,
            mix: 0.0,
            bypass: true,
        }
    }

    /// The waveshaper's constants for the current drive: the linear drive gain `d` and the
    /// normaliser `1 / tanh(d)` that keeps a full-scale input ~full-scale. Evaluated once per
    /// block — computing the `powf` and the extra `tanh` per sample made this the costliest
    /// insert in the chain.
    fn shape_consts(&self) -> (f32, f32) {
        let d = db_to_lin(self.drive_db).max(1.0);
        (d, 1.0 / d.tanh())
    }
}

impl Default for SaturationNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for SaturationNode {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

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
        if self.bypass {
            return;
        }
        let mix = self.mix.clamp(0.0, 1.0);
        let (d, norm) = self.shape_consts();
        for s in &mut out[..n] {
            let dry = *s;
            *s = dry * (1.0 - mix) + (d * dry).tanh() * norm * mix;
        }
    }

    fn set_param(&mut self, param: u16, value: f32) {
        match param {
            param::DRIVE_DB => self.drive_db = value.clamp(0.0, 36.0),
            param::MIX => self.mix = value.clamp(0.0, 1.0),
            param::BYPASS => self.bypass = value >= 0.5,
            _ => {}
        }
    }
}
