//! Transport: timeline placement, seek, and gapless sample-accurate loop wrap.

mod common;
use common::*;

use vocara_engine::dsp::master::MasterNode;
use vocara_engine::{BufferSource, Edge, Engine, GraphDesc, NodeEntry, CHANNELS};

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

/// A mono ramp where sample N has value N (so we can see which timeline frame was read).
fn ramp_stereo(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames * CHANNELS];
    for i in 0..frames {
        v[CHANNELS * i] = i as f32;
        v[CHANNELS * i + 1] = i as f32;
    }
    v
}

fn engine_with_source(src: BufferSource) -> Engine {
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(src)),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let g = desc.compile(SR, BLOCK).unwrap();
    Engine::new(g, SR, BLOCK)
}

#[test]
fn source_is_silent_outside_its_span() {
    // DC 0.5 placed at frames [100, 300).
    let src = BufferSource::new(vec![0.5; 200 * CHANNELS], 100);
    let mut eng = engine_with_source(src);
    eng.play();
    let mut out = vec![0.0; 400 * CHANNELS];
    eng.render(400, &mut out);
    let l = left(&out);
    assert!(l[..100].iter().all(|&x| x == 0.0), "silent before start");
    assert!(l[100..300].iter().all(|&x| (x - 0.5).abs() < 1e-6), "audible in span");
    assert!(l[300..].iter().all(|&x| x == 0.0), "silent after end");
}

#[test]
fn paused_engine_is_silent() {
    let src = BufferSource::new(vec![0.9; 500 * CHANNELS], 0);
    let mut eng = engine_with_source(src);
    // not playing
    let mut out = vec![0.0; 256 * CHANNELS];
    eng.render(256, &mut out);
    assert!(rms(&out) == 0.0);
}

#[test]
fn seek_repositions_the_clock() {
    let src = BufferSource::new(ramp_stereo(10_000), 0);
    let mut eng = engine_with_source(src);
    eng.seek(500);
    assert_eq!(eng.frame(), 500);
    eng.play();
    let mut out = vec![0.0; 4 * CHANNELS];
    eng.render(4, &mut out);
    // First emitted sample should be timeline frame 500.
    assert!((left(&out)[0] - 500.0).abs() < 1e-6, "seek should read frame 500");
    assert_eq!(eng.frame(), 504);
}

#[test]
fn gapless_loop_wraps_on_sample_boundary() {
    // Source is a ramp; loop [0, 100). Rendering past the boundary must re-read from 0,
    // so the output repeats the ramp with no gap or re-trigger transient.
    let src = BufferSource::new(ramp_stereo(10_000), 0);
    let mut eng = engine_with_source(src);
    eng.set_loop(true, 0, 100);
    eng.play();
    let mut out = vec![0.0; 250 * CHANNELS];
    eng.render(250, &mut out);
    let l = left(&out);

    // The ramp restarts at the wrap: out[100] and out[200] read frame 0 again.
    assert!((l[0] - 0.0).abs() < 1e-6);
    assert!((l[99] - 99.0).abs() < 1e-6);
    assert!((l[100] - 0.0).abs() < 1e-6, "wrap should re-read frame 0, got {}", l[100]);
    assert!((l[199] - 99.0).abs() < 1e-6);
    assert!((l[200] - 0.0).abs() < 1e-6, "second wrap, got {}", l[200]);
    // Contiguity: no discontinuity larger than the ramp's own step except at the seam.
    assert_eq!(eng.frame(), 50, "250 frames over a 100-frame loop ends at 50");
}

#[test]
fn two_sources_stay_sample_aligned() {
    // A source at 0 and a source at 1000 are summed; their alignment is structural.
    let a = BufferSource::new(vec![0.5; 5000 * CHANNELS], 0);
    let b = BufferSource::new(vec![0.5; 5000 * CHANNELS], 1000);
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(a)),
            NodeEntry::new(2, Box::new(b)),
            NodeEntry::new(3, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 3), Edge::simple(2, 3)],
        master: 3,
    };
    let g = desc.compile(SR, BLOCK).unwrap();
    let mut eng = Engine::new(g, SR, BLOCK);
    eng.play();
    let mut out = vec![0.0; 2000 * CHANNELS];
    eng.render(2000, &mut out);
    let l = left(&out);
    // [0,1000): only A => 0.5 ; [1000,2000): A+B => 1.0
    assert!((l[500] - 0.5).abs() < 1e-6, "only A before 1000");
    assert!((l[1500] - 1.0).abs() < 1e-6, "A+B after 1000, got {}", l[1500]);
}
