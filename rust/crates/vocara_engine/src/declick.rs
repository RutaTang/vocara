//! Transport declick: a short crossfade from the last frame put out to whatever follows a
//! discontinuity (play, pause, a seek, a loop wrap, a flushed output FIFO).
//!
//! Each of those used to step the output: play from mid-song started on a full-scale sample,
//! pause and stop dropped the output to zero from wherever it stood, a seek jumped from the old
//! audio to the new, and every pass of a loop over a dense backing clicked at the seam. The
//! audio on the far side of a cut is not there to fade (a seek has already moved the streams
//! on), so the old side is the frame last put out, HELD and faded out while the new side fades
//! in. The output is then continuous at every cut, and a cut into silence (pause) or out of it
//! (play) is a plain fade. Wait-free and allocation-free: a few floats of state.

use crate::sample::{Smp, CHANNELS};

/// A transport crossfade's length: 5 ms at the engine rate.
pub const DECLICK_FRAMES: usize = 240;

/// The fade at a loop seam, each side (see `RtProcessor::render_block`): the pass's last frames
/// fade out to the seam and the next pass's first frames fade in, 2.7 ms each, short enough
/// that the dip between them is not heard as a gap.
pub const LOOP_FADE_FRAMES: usize = 128;

/// The fade's shape at `r` in [0, 1]: smoothstep, so it starts and lands without a kink.
#[inline]
pub fn fade_shape(r: f32) -> f32 {
    let r = r.clamp(0.0, 1.0);
    r * r * (3.0 - 2.0 * r)
}

/// A crossfade from a held frame to the live signal, and the last frame put out (to hold at
/// the next cut).
#[derive(Clone, Copy, Debug, Default)]
pub struct Declick {
    last: [Smp; CHANNELS],
    hold: [Smp; CHANNELS],
    /// Frames of the running crossfade already put out, and its length (`at == len`: none).
    at: usize,
    len: usize,
}

impl Declick {
    /// A cut: from here on, crossfade from the frame last put out to the live signal over `len`
    /// frames. A cut inside a running crossfade holds the crossfaded frame, so it stays
    /// continuous too.
    #[inline]
    pub fn begin(&mut self, len: usize) {
        self.hold = self.last;
        self.at = 0;
        self.len = len;
    }

    /// Apply the running crossfade to `out` (interleaved stereo, `frames` long), in place, and
    /// remember its last frame.
    #[inline]
    pub fn apply(&mut self, out: &mut [Smp], frames: usize) {
        if frames == 0 {
            return;
        }
        if self.at < self.len {
            let n = (self.len - self.at).min(frames);
            let inv = 1.0 / self.len as f32;
            for i in 0..n {
                let r = fade_shape((self.at + i + 1) as f32 * inv);
                for c in 0..CHANNELS {
                    let o = &mut out[CHANNELS * i + c];
                    *o = self.hold[c] + (*o - self.hold[c]) * r;
                }
            }
            self.at += n;
        }
        let l = CHANNELS * (frames - 1);
        self.last = [out[l], out[l + 1]];
    }

    /// Whether a crossfade is still running.
    #[inline]
    pub fn active(&self) -> bool {
        self.at < self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_glides_from_the_held_frame_to_the_live_signal() {
        let mut d = Declick::default();
        let mut a = vec![0.8f32; 64 * CHANNELS];
        d.apply(&mut a, 64);
        d.begin(DECLICK_FRAMES);
        // The live signal jumps to -0.8; the output glides there across three blocks.
        let mut out = Vec::new();
        for _ in 0..5 {
            let mut b = vec![-0.8f32; 100 * CHANNELS];
            d.apply(&mut b, 100);
            out.extend_from_slice(&b);
        }
        let mut prev = 0.8f32;
        for f in out.chunks(CHANNELS) {
            assert!((f[0] - prev).abs() < 0.02, "a step of {}", f[0] - prev);
            prev = f[0];
        }
        assert_eq!(
            out[CHANNELS * (DECLICK_FRAMES - 1)],
            -0.8,
            "lands on the live signal"
        );
        assert!(!d.active());
    }

    #[test]
    fn a_cut_inside_a_crossfade_stays_continuous() {
        let mut d = Declick::default();
        let mut a = vec![1.0f32; 8 * CHANNELS];
        d.apply(&mut a, 8);
        d.begin(DECLICK_FRAMES);
        let mut b = vec![0.0f32; 50 * CHANNELS];
        d.apply(&mut b, 50);
        let mid = b[CHANNELS * 49];
        d.begin(DECLICK_FRAMES);
        let mut c = vec![-1.0f32; 4 * CHANNELS];
        d.apply(&mut c, 4);
        assert!((c[0] - mid).abs() < 0.01);
    }
}
