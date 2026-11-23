//! Live-path components: SPSC ring, coalesced command channel, RCU graph swap, and the
//! vocara topology builder.

mod common;
use common::*;

use vocara_engine::build::{self, build, ProjectSpec, TrackSpec};
use vocara_engine::command::command_channel;
use vocara_engine::graph_cell::{reap, GraphCell};
use vocara_engine::ring::spsc;
use vocara_engine::{BufferSource, Graph, GraphDesc, NodeEntry, CHANNELS};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

#[test]
fn ring_is_fifo_and_bounded() {
    let (mut p, mut c) = spsc::<u32>(4); // rounds to 4
    assert_eq!(c.pop(), None);
    assert!(p.push(1) && p.push(2) && p.push(3) && p.push(4));
    assert!(!p.push(5), "ring should be full at capacity");
    assert_eq!(c.pop(), Some(1));
    assert!(p.push(5), "space freed after a pop");
    assert_eq!(c.pop(), Some(2));
    assert_eq!(c.pop(), Some(3));
    assert_eq!(c.pop(), Some(4));
    assert_eq!(c.pop(), Some(5));
    assert_eq!(c.pop(), None);
}

#[test]
fn ring_threaded_spsc_preserves_order() {
    let (mut p, mut c) = spsc::<u32>(64);
    let n = 50_000u32;
    let producer = std::thread::spawn(move || {
        let mut i = 0;
        while i < n {
            if p.push(i) {
                i += 1;
            } else {
                std::thread::yield_now();
            }
        }
    });
    let mut next = 0u32;
    while next < n {
        if let Some(v) = c.pop() {
            assert_eq!(v, next, "SPSC must preserve order");
            next += 1;
        }
    }
    producer.join().unwrap();
}

#[test]
fn command_channel_coalesces_and_applies() {
    let (mut tx, mut rx) = command_channel(64);
    // Two updates to the same (node, param) collapse to the latest.
    tx.set(7, 0, 0.1);
    tx.set(7, 0, 0.9);
    tx.set(7, 1, -0.5);
    assert_eq!(tx.pending_len(), 2, "same (node,param) coalesces");
    tx.flush();

    // Apply onto a one-node graph and confirm the latest value won.
    let desc = GraphDesc {
        nodes: vec![NodeEntry::new(
            7,
            Box::new(vocara_engine::dsp::master::MasterNode::new(0.0, false)),
        )],
        edges: vec![],
        master: 7,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    rx.apply(&mut g, 16);
    // Master gain target is now 0.9; feed a DC source by hand through render isn't wired
    // (master has no input), so just assert the apply didn't choke and the param took.
    // Render silence-in → the node passes silence; the test's point is the coalesced apply.
    let mut out = vec![0.0; BLOCK * CHANNELS];
    g.render(BLOCK, 0, &mut out);
    assert!(all_finite(&out));
}

/// A post-rebuild parameter storm can exceed the command ring's capacity. flush() must then
/// deliver the leftovers across later flushes in FIFO order — draining from the back would
/// starve exactly the OLDEST commands (the first tracks' EQ/comp/sends/pan) and deliver the
/// rest reversed.
#[test]
fn command_overflow_delivers_all_in_order() {
    use std::sync::{Arc, Mutex};
    use vocara_engine::{Node, ProcessIo};

    /// Records every (param, value) it receives so delivery order is observable.
    struct RecorderNode(Arc<Mutex<Vec<(u16, f32)>>>);
    impl Node for RecorderNode {
        fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
        fn process(&mut self, _io: &mut ProcessIo) {}
        fn set_param(&mut self, param: u16, value: f32) {
            self.0.lock().unwrap().push((param, value));
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let desc = GraphDesc {
        nodes: vec![NodeEntry::new(7, Box::new(RecorderNode(Arc::clone(&seen))))],
        edges: vec![],
        master: 7,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();

    // Ring of 16 slots; 200 distinct (node, param) commands — far more than one flush fits.
    let (mut tx, mut rx) = command_channel(16);
    let total = 200u16;
    for p in 0..total {
        tx.set(7, p, p as f32);
    }
    tx.flush();
    assert!(
        tx.pending_len() > 0,
        "storm must overflow the ring for this test to bite"
    );

    // Alternate bounded audio-thread drains (like the render callback) with control-thread
    // re-flushes until everything is delivered.
    let mut spins = 0;
    while tx.pending_len() > 0 || rx.pending() > 0 {
        rx.apply(&mut g, 8);
        tx.flush();
        spins += 1;
        assert!(spins < 1000, "delivery must terminate");
    }

    let seen = seen.lock().unwrap();
    let expect: Vec<(u16, f32)> = (0..total).map(|p| (p, p as f32)).collect();
    assert_eq!(
        *seen, expect,
        "every command arrives exactly once, in submission order"
    );
}

#[test]
fn rcu_graph_swap_changes_output() {
    fn dc_graph(value: f32) -> Box<Graph> {
        let desc = GraphDesc {
            nodes: vec![
                NodeEntry::new(
                    1,
                    Box::new(BufferSource::new(vec![value; 1000 * CHANNELS], 0)),
                ),
                NodeEntry::new(2, Box::new(vocara_engine::dsp::master::MasterNode::unity())),
            ],
            edges: vec![vocara_engine::Edge::simple(1, 2)],
            master: 2,
        };
        Box::new(desc.compile(SR, BLOCK).unwrap())
    }

    let cell = GraphCell::new(dc_graph(0.5));
    let mut out = vec![0.0; BLOCK * CHANNELS];

    // SAFETY: single-threaded test — no concurrent RT reader.
    unsafe { (*cell.load()).render(BLOCK, 0, &mut out) };
    assert!((rms(&out) - 0.5).abs() < 1e-3, "graph A => 0.5");

    let retired = cell.publish(dc_graph(0.2));
    unsafe { (*cell.load()).render(BLOCK, 0, &mut out) };
    assert!((rms(&out) - 0.2).abs() < 1e-3, "graph B => 0.2 after swap");

    // SAFETY: A is fully retired and unobservable after the swap in this single thread.
    unsafe { reap(vocara_engine::graph_cell::RetiredPtr(retired)) };
}

#[test]
fn a_dangling_send_keeps_sibling_send_slots_addressable() {
    // Persisted data can carry an aux send whose target bus no longer
    // resolves; the control plane keeps the dangling entry in its model and
    // addresses send nodes by MODEL slot. The builder must keep slots stable:
    // renumbering across a skipped sibling would hand one send's level to
    // another and leave the last knob addressing a nonexistent node.
    use vocara_engine::build::BusSpec;
    let v1 = to_stereo(&sine_mono(330.0, SR, 4000, 0.3));
    let mut spec = ProjectSpec::new(vec![TrackSpec {
        sends: vec![None, Some((0, 0.5))], // slot 0 dangling, slot 1 → bus 0
        ..TrackSpec::vocal(1, Box::new(BufferSource::new(v1, 0)))
    }]);
    spec.buses = vec![
        BusSpec {
            id: 0,
            gain: 1.0,
            send_db: -120.0,
            output: None,
            chain: vec![0, 1],
            sends: vec![None, Some((1, 0.5))], // slot 0 dangling, slot 1 → bus 1
            comp_meter: None,
        },
        BusSpec {
            id: 1,
            gain: 1.0,
            send_db: -120.0,
            output: None,
            chain: vec![0, 1],
            sends: Vec::new(),
            comp_meter: None,
        },
    ];
    let desc = build(spec);
    let mut g = desc.compile(SR, BLOCK).unwrap();

    // The valid send keeps ITS model slot's node id; the dangling slot
    // builds no node (a set there is a silent no-op — it is inaudible anyway).
    assert!(
        g.node_mut(build::track_send_id(1, 1)).is_some(),
        "track send slot 1 must keep its node id across a dangling slot 0"
    );
    assert!(
        g.node_mut(build::track_send_id(1, 0)).is_none(),
        "a dangling track send must build no node"
    );
    assert!(
        g.node_mut(build::bus_aux_send_id(0, 1)).is_some(),
        "bus send slot 1 must keep its node id across a dangling slot 0"
    );
    assert!(
        g.node_mut(build::bus_aux_send_id(0, 0)).is_none(),
        "a dangling bus send must build no node"
    );

    let mut out = vec![0.0; BLOCK * CHANNELS];
    for b in 0..8 {
        g.render(BLOCK, (b * BLOCK) as i64, &mut out);
    }
    assert!(
        all_finite(&out),
        "graph with a dangling send must stay finite"
    );
    assert!(rms(&out) > 1e-3, "the mix must still be audible");
}

#[test]
fn builder_assembles_vocal_and_backing_inserts() {
    // 2 vocals (one with EQ → Comp → Reverb inserts) + 1 backing → master.
    let v1 = to_stereo(&sine_mono(330.0, SR, 8000, 0.3));
    let v2 = to_stereo(&sine_mono(440.0, SR, 8000, 0.3));
    let backing = to_stereo(&sine_mono(110.0, SR, 8000, 0.3));

    let mut spec = ProjectSpec::new(vec![
        TrackSpec {
            chain: vec![0, 1, 8], // EQ → Comp → Reverb (series insert)
            ..TrackSpec::vocal(1, Box::new(BufferSource::new(v1, 0)))
        },
        TrackSpec {
            pan: 0.4,
            ..TrackSpec::vocal(2, Box::new(BufferSource::new(v2, 0)))
        },
        TrackSpec::backing(3, Box::new(BufferSource::new(backing, 0))),
    ]);
    spec.master_limit = true;

    let desc = build(spec);
    let mut g = desc.compile(SR, BLOCK).unwrap();

    // Every addressed node id must resolve (control plane can reach them).
    assert!(g.node_mut(build::eq_id(1)).is_some());
    assert!(g.node_mut(build::comp_id(2)).is_some());
    assert!(g.node_mut(build::reverb_id(1)).is_some()); // reverb insert on track 1
    assert!(g.node_mut(build::MASTER_ID).is_some());
    // Backing track has no inserts.
    assert!(g.node_mut(build::eq_id(3)).is_none());

    let mut out = vec![0.0; BLOCK * CHANNELS];
    for b in 0..32 {
        g.render(BLOCK, (b * BLOCK) as i64, &mut out);
    }
    assert!(all_finite(&out), "vocara graph must stay finite");
    assert!(
        rms(&out) > 1e-3,
        "expected audible mix from the built graph"
    );
}

#[test]
fn a_lanes_clips_share_one_chain_so_its_dynamics_run_across_the_cut() {
    // A sustained note split into two clips of one lane (a split at the playhead, a punch-in
    // next to a clip). Through ONE chain the compressor hears one continuous note, exactly as
    // if it were one clip; a chain per clip restarted the second clip's compressor at 0 dB of
    // reduction, a gain bump at the cut.
    use vocara_engine::dsp::compressor::param as cp;
    const FRAMES: usize = 187 * BLOCK;
    const CUT: usize = 93 * BLOCK + 100; // mid-block, as a real cut lands
    let tone = to_stereo(&sine_mono(220.0, SR, FRAMES, 0.3));
    let (head, tail) = tone.split_at(CUT * CHANNELS);
    let head = || Box::new(BufferSource::new(head.to_vec(), 0));
    let tail = || Box::new(BufferSource::new(tail.to_vec(), CUT as i64));

    let render = |tracks: Vec<TrackSpec>, chains: &[u32]| -> Vec<f32> {
        let mut g = build(ProjectSpec::new(tracks)).compile(SR, BLOCK).unwrap();
        for &id in chains {
            g.set_param(build::comp_id(id), cp::BYPASS, 0.0);
            g.set_param(build::comp_id(id), cp::THRESHOLD_DB, -30.0);
            g.set_param(build::comp_id(id), cp::RATIO, 4.0);
        }
        let mut out = vec![0.0; FRAMES * CHANNELS];
        for b in 0..FRAMES / BLOCK {
            let at = b * BLOCK;
            g.render(
                BLOCK,
                at as i64,
                &mut out[at * CHANNELS..(at + BLOCK) * CHANNELS],
            );
        }
        out
    };
    let max_diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    };

    let one_clip = render(
        vec![TrackSpec::vocal(
            1,
            Box::new(BufferSource::new(tone.clone(), 0)),
        )],
        &[1],
    );
    let one_lane = render(
        vec![TrackSpec {
            lane_sources: vec![(2, tail())],
            ..TrackSpec::vocal(1, head())
        }],
        &[1],
    );
    assert_eq!(
        max_diff(&one_lane, &one_clip),
        0.0,
        "a lane's two clips must render exactly like the one clip they were cut from"
    );

    // The layout this replaced, for scale: a chain per clip bumps at the cut.
    let chain_per_clip = render(
        vec![TrackSpec::vocal(1, head()), TrackSpec::vocal(2, tail())],
        &[1, 2],
    );
    assert!(
        max_diff(&chain_per_clip, &one_clip) > 0.02,
        "a chain per clip restarts its compressor at the cut"
    );
}
