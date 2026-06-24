//! Headless offline render — the deterministic golden-file / listening harness.
//!
//! Builds the full vocara graph (two vocal tracks with active EQ + compression into one
//! shared reverb send bus, plus a backing track) from synthetic tones, renders a few
//! seconds through the exact same node `process()` code the live engine runs, and writes a
//! 16-bit WAV. Usage: `cargo run --bin render -- [out.wav] [seconds]`.

use vocara_engine::build::{self, build, ProjectSpec, TrackSpec};
use vocara_engine::dsp::biquad::param as eqp;
use vocara_engine::dsp::compressor::param as cp;
use vocara_engine::{BufferSource, Engine, CHANNELS};
use vocara_engine::wav::write_wav_i16;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn sine(freq: f64, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| (amp as f64 * (2.0 * std::f64::consts::PI * freq * i as f64 / SR).sin()) as f32)
        .collect()
}

fn stereo(mono: &[f32]) -> Vec<f32> {
    let mut v = vec![0.0; mono.len() * CHANNELS];
    for (i, &s) in mono.iter().enumerate() {
        v[CHANNELS * i] = s;
        v[CHANNELS * i + 1] = s;
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "render_out.wav".to_string());
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let total_frames = (SR * seconds) as usize;

    // Two vocal tones a major third apart, panned apart, into a shared reverb; one backing.
    let v1 = stereo(&sine(330.0, total_frames, 0.35));
    let v2 = stereo(&sine(415.3, total_frames, 0.35));
    let backing = stereo(&sine(110.0, total_frames, 0.25));

    let spec = ProjectSpec::new(vec![
        TrackSpec {
            pan: -0.4,
            send_db: -8.0,
            ..TrackSpec::vocal(1, Box::new(BufferSource::new(v1, 0)))
        },
        TrackSpec {
            pan: 0.4,
            send_db: -8.0,
            ..TrackSpec::vocal(2, Box::new(BufferSource::new(v2, 0)))
        },
        TrackSpec::backing(3, Box::new(BufferSource::new(backing, 0))),
    ]);

    let desc = build(spec);
    let mut graph = desc.compile(SR, BLOCK).unwrap();

    // Engage real effects so the render exercises active DSP (not bypassed defaults):
    // a presence boost on vocal 1, and the compressor on both vocals.
    // EQ band 2 (a peaking band) +6 dB, and enable the compressor.
    graph.set_param(build::eq_id(1), (2 << eqp::FIELD_BITS) | eqp::GAIN_DB, 6.0);
    graph.set_param(build::comp_id(1), cp::BYPASS, 0.0);
    graph.set_param(build::comp_id(1), cp::THRESHOLD_DB, -24.0);
    graph.set_param(build::comp_id(2), cp::BYPASS, 0.0);
    graph.set_param(build::comp_id(2), cp::THRESHOLD_DB, -24.0);

    let mut engine = Engine::new(graph, SR, BLOCK);
    engine.play();

    let mut out_buf = vec![0.0f32; total_frames * CHANNELS];
    let mut pos = 0;
    while pos < total_frames {
        let n = BLOCK.min(total_frames - pos);
        engine.render(n, &mut out_buf[pos * CHANNELS..(pos + n) * CHANNELS]);
        pos += n;
    }

    let peak = out_buf.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    let finite = out_buf.iter().all(|x| x.is_finite());
    write_wav_i16(&out, &out_buf, CHANNELS as u16, SR as u32).expect("write wav");
    println!(
        "rendered {seconds:.1}s ({total_frames} frames) -> {out}  peak={peak:.3} finite={finite}"
    );
}
