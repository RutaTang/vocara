//! The live engine end to end (offline-driven): ring-fed source streaming, transport,
//! coalesced params through the handle, and an RCU graph hot-swap mid-run.

mod common;
use common::*;

use std::collections::HashMap;

use vocara_engine::dsp::master::{param as mparam, MasterNode};
use vocara_engine::live::{spawn, CMD_RING_CAPACITY};
use vocara_engine::ring::spsc;
use vocara_engine::source::RingSource;
use vocara_engine::{Edge, GraphDesc, NodeEntry, CHANNELS};

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn frames_dc(value: f32, n: usize) -> Vec<[f32; CHANNELS]> {
    vec![[value, value]; n]
}

/// Build a one-source live engine: RingSource(1) -> Master(2).
fn one_source_engine() -> (
    vocara_engine::live::EngineHandle,
    vocara_engine::live::RtProcessor,
    u32,
) {
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
    assert!(
        (rms(&out) - 0.5).abs() < 1e-3,
        "streamed DC 0.5, got {}",
        rms(&out)
    );
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
    assert_eq!(
        h.frame(),
        big as i64,
        "clock advanced by the full large block"
    );
    assert!(
        (rms(&out) - 0.25).abs() < 1e-3,
        "streamed DC across the large block, got {}",
        rms(&out)
    );
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
    assert!(
        rms(&out) < 1e-3,
        "master gain 0 via handle should mute, got {}",
        rms(&out)
    );
}

#[test]
fn reap_drains_overflowed_param_storm_without_explicit_flush() {
    // A parameter storm larger than the command ring: after the single flush that overflows,
    // the leftovers must still reach the audio thread via the control-plane hot path (reap(),
    // which the FFI frame-position getter polls during playback) — with NO further
    // user-driven flush. Before the reap() hook, the tail of the storm sat pending forever
    // and those nodes silently ran at engine defaults.
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
    let (_mic_prod, mic_cons) = spsc::<[f32; CHANNELS]>(256);
    // Command ring of only 16 slots so the storm below overflows it.
    let (mut h, mut rt) = spawn(g, producers, mic_cons, 16);

    h.push_pcm(1, &frames_dc(0.8, 65536));
    h.play();

    // Storm: 32 padding commands (unknown params; RingSource ignores them) followed by the
    // one that matters — muting the master. One flush fits only the first 16.
    for p in 0..32u16 {
        h.set_param(1, 100 + p, 0.0);
    }
    h.set_param(2, mparam::GAIN, 0.0);
    h.flush();

    // Playback loop: render + poll (reap) — exactly what the app does while playing. The
    // master mute at the very END of the storm must arrive and take effect.
    let mut out = vec![0.0; BLOCK * CHANNELS];
    for _ in 0..40 {
        rt.process(&mut out, BLOCK);
        h.reap();
    }
    assert!(
        rms(&out) < 1e-3,
        "tail of an overflowed param storm never delivered: master still audible at {}",
        rms(&out)
    );
}

#[test]
fn a_post_commit_parameter_storm_reaches_the_graph_within_a_few_blocks() {
    // After every commit the host re-pushes each live parameter of the rebuilt graph, ~58
    // per clip, each as set + flush. With the old 1024-slot ring drained 64 per block, a
    // 40-clip project's tail took ~0.4 s to land (its tracks dry and centred meanwhile), and
    // the overflow beyond the ring waited for reap() — which nothing calls while paused.
    // No reap here: the storm must land on its own flushes. Measured against a lone mute,
    // since the master's gain smoother takes its own blocks to reach silence.
    fn blocks_until_muted(storm_clips: u16) -> usize {
        let (prod, cons) = spsc::<[f32; CHANNELS]>(1 << 16);
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
        let (_mic_prod, mic_cons) = spsc::<[f32; CHANNELS]>(256);
        let (mut h, mut rt) = spawn(g, producers, mic_cons, CMD_RING_CAPACITY);
        h.push_pcm(1, &frames_dc(0.8, 65536));
        h.play();
        // One block first, so the master is live in both runs: a smoother that has not been
        // heard yet snaps to its first target, which would make the lone mute instant.
        let mut out = vec![0.0; BLOCK * CHANNELS];
        rt.process(&mut out, BLOCK);
        for clip in 0..storm_clips {
            for p in 0..58u16 {
                // Unknown params (RingSource ignores them): padding in the storm's shape.
                h.set_param(1, 100 + clip * 58 + p, 0.0);
                h.flush();
            }
        }
        h.set_param(2, mparam::GAIN, 0.0); // the storm's LAST command
        h.flush();
        for blocks in 1..=400 {
            rt.process(&mut out, BLOCK);
            if rms(&out) < 1e-3 {
                return blocks;
            }
        }
        usize::MAX
    }
    let lone = blocks_until_muted(0);
    let storm = blocks_until_muted(40);
    assert!(
        storm <= lone + 3,
        "the storm's tail took {storm} blocks to take effect (a lone command: {lone}; \
         usize::MAX = never)"
    );
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
    assert!(
        l[20..].iter().all(|&x| x == 0.0),
        "underrun region is silent"
    );
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
    assert!(
        rms(&out) < 1e-3,
        "after swap to gain-0 graph: silent, got {}",
        rms(&out)
    );
    h.reap(); // free the retired graph on the control thread
}

#[test]
fn no_renderer_frees_retired_graphs_immediately() {
    // With no stream callback running, the block counter never advances, so the grace epoch
    // never elapses — every commit would park one more full graph (unbounded while the output
    // device is lost). The audio host lowers the renderer gate whenever no stream is up; reap
    // must then free retirees immediately instead of waiting for an epoch that never comes.
    let (mut h, _rt, _src) = one_source_engine();
    let build = || {
        let (_p, cons) = spsc::<[f32; CHANNELS]>(64);
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(RingSource::new(cons))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        desc.compile(SR, BLOCK).unwrap()
    };
    // Gate defaults to true (a renderer may be running): the retiree parks for its epoch.
    h.publish_graph(build());
    assert_eq!(
        h.retired_count(),
        1,
        "with the gate up the grace epoch applies"
    );
    // The host reports all streams down: the parked graph AND the fresh retiree free at once.
    *h.renderer_gate().lock().unwrap() = false;
    h.publish_graph(build());
    assert_eq!(
        h.retired_count(),
        0,
        "with the gate down retirees free immediately"
    );
    // And a raised gate restores the grace-epoch behavior.
    *h.renderer_gate().lock().unwrap() = true;
    h.publish_graph(build());
    assert_eq!(
        h.retired_count(),
        1,
        "raising the gate re-arms the grace epoch"
    );
}

#[test]
fn a_parameter_sent_after_a_publish_reaches_the_new_graph() {
    // The render loaded the graph pointer, THEN drained the command ring onto it. A commit
    // whose publish landed while a callback worked through a backlog (the tail of an earlier
    // commit's parameter storm) had its own follow-up parameters popped in that same drain
    // and applied to the graph it had just retired: the new graph never got them.
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;
    use vocara_engine::dsp::biquad::EqNode;
    use vocara_engine::source::BufferSource;

    // DC 0.8 (10 s) -> EQ (3) -> master (2).
    let graph = || {
        GraphDesc {
            nodes: vec![
                NodeEntry::new(
                    1,
                    Box::new(BufferSource::new(vec![0.8; 480_000 * CHANNELS], 0)),
                ),
                NodeEntry::new(3, Box::new(EqNode::default_strip())),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 3), Edge::simple(3, 2)],
            master: 2,
        }
        .compile(SR, BLOCK)
        .unwrap()
    };
    let (_mic, mic_cons) = spsc::<[f32; CHANNELS]>(256);
    let (mut h, mut rt) = spawn(graph(), HashMap::new(), mic_cons, CMD_RING_CAPACITY);
    h.play();
    let level = Arc::new(AtomicU32::new(0));
    let blocks = Arc::new(AtomicU64::new(0));
    let starting = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    // Device-callback cadence: one block per ~0.5 ms, each announcing its start.
    let device = {
        let (level, blocks, starting, stop) = (
            level.clone(),
            blocks.clone(),
            starting.clone(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            let mut out = vec![0.0f32; BLOCK * CHANNELS];
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_micros(500));
                starting.fetch_add(1, Ordering::Release);
                rt.process(&mut out, BLOCK);
                level.store(rms(&out).to_bits(), Ordering::Release);
                blocks.fetch_add(1, Ordering::Release);
            }
        })
    };
    let rounds = 30;
    let mut lost = 0;
    for round in 0..rounds {
        let next = graph();
        // A backlog for the CURRENT graph: level-neutral EQ retunes (0 dB bands), then
        // parameters the master ignores, for ring occupancy.
        for k in 0..12u16 {
            h.set_param(
                3,
                ((k % 6) << 3) | (k / 6),
                300.0 + f32::from(k) + round as f32,
            );
        }
        for k in 12..100u16 {
            h.set_param(2, 100 + k, 0.0);
        }
        h.flush();
        // The commit publishes just as the next callback starts draining that backlog, and
        // its follow-up silences the NEW graph's master.
        let s0 = starting.load(Ordering::Acquire);
        while starting.load(Ordering::Acquire) == s0 {
            std::hint::spin_loop();
        }
        h.publish_graph(next);
        h.set_param(2, mparam::GAIN, 0.0);
        h.flush();
        let b0 = blocks.load(Ordering::Acquire);
        while blocks.load(Ordering::Acquire) < b0 + 60 {
            std::hint::spin_loop();
        }
        if f32::from_bits(level.load(Ordering::Acquire)) > 0.1 {
            lost += 1;
        }
        h.reap();
    }
    stop.store(true, Ordering::Relaxed);
    device.join().unwrap();
    assert_eq!(
        lost, 0,
        "{lost}/{rounds} post-publish parameters hit the retired graph"
    );
}

#[test]
fn a_staged_graph_goes_live_with_its_parameters_already_in_place() {
    // A commit's graph used to be published first and its live values pushed after it, so
    // the audio thread rendered the build defaults — and every smoother ramped away from
    // them — for the blocks that re-push took to land. Staged, the values land on the graph
    // itself, the live graph keeps playing meanwhile, and the first block it renders after
    // going live already carries them.
    use vocara_engine::BufferSource;
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.5, 8192));
    h.play();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);

    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(BufferSource::new(vec![0.5; 16_384], 0))),
            NodeEntry::new(2, Box::new(MasterNode::unity())), // build default: unity
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    h.stage_graph(desc.compile(SR, BLOCK).unwrap());
    h.set_param(2, mparam::GAIN, 0.25); // the host's post-commit re-push
    h.flush();

    rt.process(&mut out, BLOCK);
    assert!(
        out.iter().all(|&x| (x - 0.5).abs() < 1e-6),
        "the live graph keeps playing, untouched, while the new one is staged"
    );

    assert!(h.publish_staged());
    rt.process(&mut out, BLOCK);
    let worst = out
        .iter()
        .map(|&x| (x - 0.125).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 1e-6,
        "the first live block must already play the staged gain (worst error {worst})"
    );
    assert!(!h.publish_staged(), "nothing left to publish");
}

#[test]
fn commands_queued_for_the_old_graph_never_reach_the_new_one() {
    // A storm of live values for the old graph (a fast-path sync's re-push) is still in the
    // command ring — the audio thread drains a bounded number per block — when the next
    // commit publishes. Those commands address nodes by id, and a rebuilt graph can number a
    // different lane under the same id: applied there after the commit's own staged values,
    // the old lane's fader landed on the new lane, while the UI showed the right one.
    use vocara_engine::BufferSource;
    for capacity in [CMD_RING_CAPACITY, 256] {
        // CMD_RING_CAPACITY: the stale command waits in the ring; 256: in the sender's
        // overflow queue, which a reap flushes later.
        let (prod, cons) = spsc::<[f32; CHANNELS]>(8192);
        let old = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(RingSource::new(cons))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        let mut producers = HashMap::new();
        producers.insert(1u32, prod);
        let (_mic_prod, mic_cons) = spsc::<[f32; CHANNELS]>(256);
        let (mut h, mut rt) = spawn(
            old.compile(SR, BLOCK).unwrap(),
            producers,
            mic_cons,
            capacity,
        );
        h.push_pcm(1, &frames_dc(0.5, 8192));
        h.play();
        let mut out = vec![0.0; BLOCK * CHANNELS];
        for i in 0..1500u32 {
            h.set_param(100_000 + i, 0, 0.0); // the rest of the storm (no such nodes)
        }
        h.set_param(2, mparam::GAIN, 0.1); // the old lane's fader, queued last
        h.flush();
        rt.process(&mut out, BLOCK); // drains only part of it

        let new = GraphDesc {
            nodes: vec![
                NodeEntry::new(1, Box::new(BufferSource::new(vec![0.5; 32_768], 0))),
                NodeEntry::new(2, Box::new(MasterNode::unity())),
            ],
            edges: vec![Edge::simple(1, 2)],
            master: 2,
        };
        h.stage_graph(new.compile(SR, BLOCK).unwrap());
        h.set_param(2, mparam::GAIN, 0.8); // the new lane's own value
        h.flush();
        assert!(h.publish_staged());
        for _ in 0..16 {
            h.reap();
            rt.process(&mut out, BLOCK);
        }
        let worst = out.iter().map(|&x| (x - 0.4).abs()).fold(0.0f32, f32::max);
        assert!(
            worst < 1e-4,
            "ring {capacity}: the new graph must keep its own gain (worst error {worst})"
        );
    }
}

#[test]
fn a_non_finite_monitor_reverb_value_never_reaches_the_output() {
    // `f32::clamp` passes NaN through: a NaN wet made the monitor reverb's output NaN for
    // good, and the monitor is ADDED to the playback mix even at gain 0 (NaN * 0 = NaN) — so
    // every sample after it was NaN, monitoring off, valid values restored or not.
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.5, 8192));
    h.play();
    h.set_monitor_gain(0.0); // monitoring OFF
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    h.set_monitor_reverb(0.6, 0.5, f32::NAN);
    rt.process(&mut out, BLOCK);
    h.set_monitor_reverb(f32::INFINITY, f32::NEG_INFINITY, 0.2);
    rt.process(&mut out, BLOCK);
    h.set_monitor_gain(f32::INFINITY);
    for _ in 0..20 {
        rt.process(&mut out, BLOCK);
    }
    let bad = out.iter().filter(|s| !s.is_finite()).count();
    assert_eq!(bad, 0, "{bad} of {} samples non-finite", out.len());
    assert!(
        (rms(&out) - 0.5).abs() < 1e-3,
        "playback untouched, got {}",
        rms(&out)
    );
}

#[test]
fn a_non_finite_parameter_never_reaches_a_node() {
    // One infinite master gain: the smoother ran to infinity, and the next finite target
    // turned it into NaN (inf + (x - inf) * c) for the rest of the graph's life.
    let (mut h, mut rt, src) = one_source_engine();
    h.push_pcm(src, &frames_dc(0.5, 8192));
    h.play();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    rt.process(&mut out, BLOCK);
    for bad in [f32::INFINITY, f32::NAN, f32::NEG_INFINITY] {
        h.set_param(2, mparam::GAIN, bad);
        h.flush();
        rt.process(&mut out, BLOCK);
    }
    h.set_param(2, mparam::GAIN, 0.5);
    h.flush();
    for _ in 0..40 {
        rt.process(&mut out, BLOCK);
    }
    assert!(
        out.iter().all(|s| s.is_finite()),
        "master output non-finite"
    );
    assert!(
        (rms(&out) - 0.25).abs() < 1e-3,
        "gain 0.5 applied, got {}",
        rms(&out)
    );

    // The graph boundary drops it too (the staged-commit path sets params on the graph
    // directly, and the command ring lands on it through the same call).
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(
                1,
                Box::new(vocara_engine::BufferSource::new(vec![0.5; 2 * BLOCK], 0)),
            ),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    g.set_param(2, mparam::GAIN, f32::NAN);
    let mut out = vec![0.0; BLOCK * CHANNELS];
    g.render(BLOCK, 0, &mut out);
    assert!(
        out.iter().all(|&s| s == 0.5),
        "graph-level NaN reached the node"
    );
}

#[test]
fn a_pause_right_after_a_seek_keeps_the_seek_under_a_concurrent_render() {
    // pause() folds the clock back to what was heard of the run before. When the render
    // applied the seek between that read and the clock swap, the swap put the pre-seek
    // position back over it: paused where the seek had left, while the UI showed the seek.
    // About a third of these rounds lost it. A render that plays on from the seek before
    // the pause lands may leave it later, never earlier.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use vocara_engine::live::RtCell;
    use vocara_engine::source::BufferSource;

    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(
                1,
                Box::new(BufferSource::new(vec![0.1; 1_000_000 * CHANNELS], 0)),
            ),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2)],
        master: 2,
    };
    let (_mic, mic_cons) = spsc::<[f32; CHANNELS]>(256);
    let (h, rt) = spawn(
        desc.compile(SR, 64).unwrap(),
        HashMap::new(),
        mic_cons,
        CMD_RING_CAPACITY,
    );
    let cell = RtCell::new(rt);
    let stop = Arc::new(AtomicBool::new(false));
    let render = {
        let (cell, stop) = (Arc::clone(&cell), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut out = vec![0.0f32; 64 * CHANNELS];
            while !stop.load(Ordering::Relaxed) {
                // 48 kHz identity output: what renders is what is heard.
                cell.render_offline(&mut out, 64);
                cell.transport().sync_consumed_to_rendered();
            }
        })
    };
    let t = cell.transport();
    let heard_on = |frames: u64| {
        let from = t.consumed_frames();
        while t.consumed_frames() < from + frames {
            std::hint::spin_loop();
        }
    };
    let mut lost = Vec::new();
    for round in 0..5_000 {
        let target: i64 = if round % 2 == 0 { 600_000 } else { 100_000 };
        h.seek(target - 50_000);
        h.play();
        heard_on(256);
        h.seek(target);
        h.pause();
        heard_on(256); // the render applies anything still pending
        if h.frame() < target {
            lost.push((round, target, h.frame()));
        }
    }
    stop.store(true, Ordering::Relaxed);
    render.join().unwrap();
    assert!(
        lost.is_empty(),
        "{} of 5000 seeks lost to the pause after them; first: {:?}",
        lost.len(),
        &lost[..lost.len().min(5)]
    );
}
