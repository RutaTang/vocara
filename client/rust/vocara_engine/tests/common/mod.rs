//! Shared test helpers (not a test target itself). Each test file uses a subset.
#![allow(dead_code)]

use vocara_engine::{Node, ProcessIo, CHANNELS};

/// A mono sine of `frames` samples.
pub fn sine_mono(freq: f64, sr: f64, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| (amp as f64 * (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin()) as f32)
        .collect()
}

/// Interleave a mono buffer to stereo (same value both channels).
pub fn to_stereo(mono: &[f32]) -> Vec<f32> {
    let mut v = vec![0.0; mono.len() * CHANNELS];
    for (i, &s) in mono.iter().enumerate() {
        v[CHANNELS * i] = s;
        v[CHANNELS * i + 1] = s;
    }
    v
}

pub fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let s: f64 = buf.iter().map(|&x| (x as f64) * (x as f64)).sum();
    (s / buf.len() as f64).sqrt() as f32
}

pub fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(CHANNELS).copied().collect()
}

pub fn right(inter: &[f32]) -> Vec<f32> {
    inter[1..].iter().step_by(CHANNELS).copied().collect()
}

pub fn all_finite(buf: &[f32]) -> bool {
    buf.iter().all(|x| x.is_finite())
}

/// Run a single node over an interleaved-stereo input in `block`-sized chunks; returns the
/// interleaved output. Drives the node exactly like the graph renderer would.
pub fn run_node(node: &mut dyn Node, input: &[f32], sr: f64, block: usize) -> Vec<f32> {
    node.prepare(sr, block);
    let total_frames = input.len() / CHANNELS;
    let mut out = vec![0.0; input.len()];
    let mut f = 0;
    while f < total_frames {
        let n = block.min(total_frames - f);
        let in_slice = &input[f * CHANNELS..(f + n) * CHANNELS];
        let mut out_buf = vec![0.0; n * CHANNELS];
        {
            let inputs: [&[f32]; 1] = [in_slice];
            let mut outs: [&mut [f32]; 1] = [&mut out_buf[..]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames: n,
                tl: f as i64,
                sr,
            };
            node.process(&mut io);
        }
        out[f * CHANNELS..(f + n) * CHANNELS].copy_from_slice(&out_buf);
        f += n;
    }
    out
}
