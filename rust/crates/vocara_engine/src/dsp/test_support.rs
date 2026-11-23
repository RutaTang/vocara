//! Equivalence driver for the DSP nodes' optimized loops: a node runs its `process` beside a
//! twin driven by a reference loop (the plain per-sample form the optimization replaced), on
//! the same input and the same parameter edits, and every output sample must match BIT FOR
//! BIT. The input carries what the optimizations must not trip on: exact silence, negative
//! zeros, loud bursts past full scale, and ragged block sizes down to one frame.

use crate::node::{Node, ProcessIo};
use crate::sample::{Smp, CHANNELS};

/// Deterministic test material, interleaved stereo: a few partials over noise, with exact
/// silence (`+0.0` / `-0.0`) and loud bursts in turn.
pub(crate) fn material(frames: usize) -> Vec<Smp> {
    let mut x = 0x9E37_79B9u32;
    let mut v = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let n = (x as f32 / u32::MAX as f32 - 0.5) * 0.05;
        let t = i as f32 / 48_000.0;
        let s = 0.4 * (t * 330.0 * std::f32::consts::TAU).sin()
            + 0.1 * (t * 7_000.0 * std::f32::consts::TAU).sin();
        let (l, r) = match (i / 2_000) % 6 {
            2 => (0.0, -0.0),
            4 => (2.5 * (s + n), -2.0 * (s - n)),
            _ => (s + n, 0.7 * s - n),
        };
        v.push(l);
        v.push(r);
    }
    v
}

/// Block sizes the renderer can hand a node (1..=max_block), cycled.
const SIZES: [usize; 7] = [512, 1, 37, 1024, 256, 129, 700];

/// Runs `blocks` blocks through `fast` (its own `process`) and `slow` (`reference(node, input,
/// output)`), applying each `(block, param, value)` edit to both before that block, and
/// asserts every output bit matches. Both nodes must be built and prepared identically.
pub(crate) fn assert_matches_reference<N: Node>(
    fast: &mut N,
    slow: &mut N,
    reference: impl Fn(&mut N, &[Smp], &mut [Smp]),
    edits: &[(usize, u16, f32)],
    blocks: usize,
) {
    let total: usize = (0..blocks).map(|b| SIZES[b % SIZES.len()]).sum();
    let input = material(total);
    let mut at = 0;
    for b in 0..blocks {
        for &(when, param, value) in edits {
            if when == b {
                fast.set_param(param, value);
                slow.set_param(param, value);
            }
        }
        let frames = SIZES[b % SIZES.len()];
        let inp = &input[at * CHANNELS..(at + frames) * CHANNELS];
        let mut got = vec![0.0; frames * CHANNELS];
        {
            let inputs: [&[Smp]; 1] = [inp];
            let mut outs: [&mut [Smp]; 1] = [&mut got[..]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames,
                tl: at as i64,
                sr: 48_000.0,
            };
            fast.process(&mut io);
        }
        let mut want = vec![0.0; frames * CHANNELS];
        reference(slow, inp, &mut want);
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            // Bit for bit — except that any NaN matches any NaN: which payload and sign a NaN
            // carries out of two NaN operands is the compiler's choice, not the algorithm's.
            assert!(
                g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()),
                "block {b} sample {k}: {g} vs the reference's {w}"
            );
        }
        at += frames;
    }
}
