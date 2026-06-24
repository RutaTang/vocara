//! The live engine end to end (offline-driven): ring-fed source streaming, transport,
//! coalesced params through the handle, and an RCU graph hot-swap mid-run.

mod common;
use common::*;

use std::collections::HashMap;

use vocara_engine::dsp::master::{param as mparam, MasterNode};
use vocara_engine::live::spawn;
use vocara_engine::ring::spsc;
use vocara_engine::source::RingSource;
use vocara_engine::{Edge, GraphDesc, NodeEntry, CHANNELS};

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn frames_dc(value: f32, n: usize) -> Vec<[f32; CHANNELS]> {
    vec![[value, value]; n]
}

/// Build a one-source live engine: RingSource(1) -> Master(2).
fn one_source_engine() -> (vocara_engine::live::EngineHandle, vocara_engine::live::RtProcessor, u32) {
    let (prod, cons) = spsc::<[f32; CHANNELS]>(8192);
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(RingSource::new(cons))),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let g = desc.compile(SR, BLOCK).unwrap();
    let mut producers = HashMap::new();
    producers.insert(1u32, prod);
    // The live-input monitor ring is unfed in these tests (monitor gain stays 0).
    let (_mic_prod, mic_cons) = spsc::<[f32; CHANNELS]>(256);
    let (h, rt) = spawn(g, producers, mic_cons, 256);
    (h, rt, 1)
}

#[test]
fn ring_source_streams_fed_pcm() {
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.5, 4096));
    h.play();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    assert!((rms(&out) - 0.5).abs() < 1e-3, "streamed DC 0.5, got {}", rms(&out));
    assert_eq!(h.frame(), BLOCK as i64, "clock advanced by one block");
}

#[test]
fn large_io_block_does_not_overrun_monitor_scratch() {
    // A device IO buffer can exceed the engine's max_block (VPIO permits up to 4096 frames/slice).
    // process() must chunk the monitor mix to max_block; otherwise it indexes the mon_in/mon_out
    // scratch (sized for one max_block) out of bounds and panics on the real-time thread.
    let (mut h, mut rt, src) = one_source_engine(); // max_block = 128
    h.push_pcm(src, &frames_dc(0.25, 8192));
    h.play();
    let big = 512; // 4 * max_block — would overrun the 1-block monitor scratch without chunking
    let mut out = vec![0.0; big * CHANNELS];
    rt.process(&mut out, big); // must not panic
    assert_eq!(h.frame(), big as i64, "clock advanced by the full large block");
    assert!((rms(&out) - 0.25).abs() < 1e-3, "streamed DC across the large block, got {}", rms(&out));
}

#[test]
fn paused_live_engine_is_silent() {
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.9, 4096));
    // not playing
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    assert!(rms(&out) == 0.0);
    assert_eq!(h.frame(), 0, "clock does not advance while paused");
}

#[test]
fn params_applied_through_handle() {
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.8, 8192));
    h.play();
    // Mute the master via the command channel.
    h.set_param(2, mparam::GAIN, 0.0);
    h.flush();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    for _ in 0..40 {
        rt.process(&mut out, BLOCK);
    }
    assert!(rms(&out) < 1e-3, "master gain 0 via handle should mute, got {}", rms(&out));
}

#[test]
fn underrun_emits_silence_not_garbage() {
    let (mut h, mut rt, src) = one_source_engine();
    // Feed less than a block; the rest underruns to silence (finite, not garbage).
    h.push_pcm(src, &frames_dc(0.5, 10));
    h.play();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    assert!(all_finite(&out));
    // First 10 frames have signal; the rest are silent.
    let l = left(&out);
    assert!((l[0] - 0.5).abs() < 1e-6);
    assert!(l[20..].iter().all(|&x| x == 0.0), "underrun region is silent");
}

#[test]
fn rcu_hot_swap_takes_effect_next_block() {
    // Start with a master gain of 1, render; then publish a graph with gain 0 and confirm
    // the audio thread picks it up. Reaping the old graph happens on the control thread.
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.5, 65536));
    h.play();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    assert!(rms(&out) > 0.4, "before swap: audible");

    // Build a replacement graph that mutes (master gain 0). New ring for its source.
    let (prod2, cons2) = spsc::<[f32; CHANNELS]>(8192);
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(RingSource::new(cons2))),
            NodeEntry::new(2, Box::new(MasterNode::new(0.0, false))),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let g2 = desc.compile(SR, BLOCK).unwrap();
    // Feed the new source so it isn't underrunning, then swap.
    let mut p2 = prod2;
    for _ in 0..4096 {
        p2.push([0.5, 0.5]);
    }
    h.publish_graph(g2);

    // A couple more blocks let the swap + gain settle.
    for _ in 0..40 {
        rt.process(&mut out, BLOCK);
    }
    assert!(rms(&out) < 1e-3, "after swap to gain-0 graph: silent, got {}", rms(&out));
    h.reap(); // free the retired graph on the control thread
}
