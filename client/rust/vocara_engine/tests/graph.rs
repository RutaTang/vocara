//! Graph runtime: compilation, fan-in summation, cycle rejection, and the shared reverb
//! send-bus topology rendering end to end.

mod common;
use common::*;

use vocara_engine::dsp::biquad::{BandSpec, EqNode};
use vocara_engine::dsp::gain_pan::GainPanNode;
use vocara_engine::dsp::master::MasterNode;
use vocara_engine::dsp::reverb::ReverbNode;
use vocara_engine::graph::CompileError;
use vocara_engine::{BufferSource, Edge, GraphDesc, NodeEntry, CHANNELS};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn dc(value: f32, frames: usize) -> Vec<f32> {
    vec![value; frames * CHANNELS]
}

#[test]
fn source_through_eq_to_master() {
    // src(1) -> eq(2) -> master(3, unity)
    let src = BufferSource::new(dc(0.25, 1000), 0);
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(src)),
            NodeEntry::new(2, Box::new(EqNode::default_strip())), // flat = identity
            NodeEntry::new(3, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2), Edge::simple(2, 3)],
        master: 3,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    let mut out = vec![0.0; 256 * CHANNELS];
    g.render(256, 0, &mut out);
    // Flat EQ + unity gain => master ≈ source DC.
    assert!(
        (rms(&out) - 0.25).abs() < 1e-3,
        "expected ~0.25, got {}",
        rms(&out)
    );
}

#[test]
fn fan_in_sums_sources() {
    // src1(0.5) + src2(0.3) both -> master => 0.8.
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(BufferSource::new(dc(0.5, 500), 0))),
            NodeEntry::new(2, Box::new(BufferSource::new(dc(0.3, 500), 0))),
            NodeEntry::new(9, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 9), Edge::simple(2, 9)],
        master: 9,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    let mut out = vec![0.0; 128 * CHANNELS];
    g.render(128, 0, &mut out);
    for &s in &out {
        assert!((s - 0.8).abs() < 1e-4, "fan-in sum should be 0.8, got {s}");
    }
}

#[test]
fn cycle_is_rejected_at_compile() {
    // 1 -> 2 -> 1 is a cycle.
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(GainPanNode::unity())),
            NodeEntry::new(2, Box::new(GainPanNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2), Edge::simple(2, 1)],
        master: 2,
    };
    assert_eq!(desc.compile(SR, BLOCK).err(), Some(CompileError::Cycle));
}

#[test]
fn out_of_range_from_port_is_rejected_at_compile() {
    // Every node has exactly one output port, so from_port > 0 can never be valid. It must
    // fail compile: in release the renderer silently read port 0's buffer instead (only a
    // debug_assert caught the mismatch before).
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(GainPanNode::unity())),
            NodeEntry::new(2, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge {
            from: 1,
            from_port: 1,
            to: 2,
            to_port: 0,
        }],
        master: 2,
    };
    assert_eq!(desc.compile(SR, BLOCK).err(), Some(CompileError::BadEdge));
}

#[test]
fn missing_master_is_rejected() {
    let desc = GraphDesc {
        nodes: vec![NodeEntry::new(1, Box::new(GainPanNode::unity()))],
        edges: vec![],
        master: 99,
    };
    assert_eq!(
        desc.compile(SR, BLOCK).err(),
        Some(CompileError::MissingMaster)
    );
}

#[test]
fn shared_reverb_send_bus_renders() {
    // Two vocal tracks, each: src -> gain -> master (dry) and gain -> reverb (send).
    // One shared reverb bus -> master. Fan-in at the reverb (2) and the master (3).
    //   src1(1) -> g1(2) ; src2(3) -> g2(4) ; reverb(5) ; master(6)
    let v1 = to_stereo(&sine_mono(330.0, SR, 4000, 0.4));
    let v2 = to_stereo(&sine_mono(440.0, SR, 4000, 0.4));
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(BufferSource::new(v1, 0))),
            NodeEntry::new(2, Box::new(GainPanNode::new(1.0, -0.3))),
            NodeEntry::new(3, Box::new(BufferSource::new(v2, 0))),
            NodeEntry::new(4, Box::new(GainPanNode::new(1.0, 0.3))),
            NodeEntry::new(5, Box::new(ReverbNode::default_bus())),
            NodeEntry::new(6, Box::new(MasterNode::unity())),
        ],
        edges: vec![
            Edge::simple(1, 2),
            Edge::simple(3, 4),
            Edge::simple(2, 6), // dry
            Edge::simple(4, 6), // dry
            Edge::simple(2, 5), // send
            Edge::simple(4, 5), // send
            Edge::simple(5, 6), // reverb return
        ],
        master: 6,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    let mut out = vec![0.0; BLOCK * CHANNELS];
    // Render several blocks so the reverb builds up.
    for b in 0..16 {
        g.render(BLOCK, (b * BLOCK) as i64, &mut out);
    }
    assert!(all_finite(&out), "shared-bus graph must stay finite");
    assert!(
        rms(&out) > 1e-3,
        "expected audible output from the bus graph"
    );
}

#[test]
fn set_param_reaches_node() {
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(BufferSource::new(dc(0.5, 500), 0))),
            NodeEntry::new(
                2,
                Box::new(EqNode::new(&[BandSpec::peaking(1000.0, 1.0, 0.0)])),
            ),
            NodeEntry::new(3, Box::new(MasterNode::unity())),
        ],
        edges: vec![Edge::simple(1, 2), Edge::simple(2, 3)],
        master: 3,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    // Set the gain/pan master gain to 0 via param GAIN(0) and expect silence after settle.
    g.set_param(3, 0, 0.0);
    let mut out = vec![0.0; BLOCK * CHANNELS];
    for _ in 0..20 {
        g.render(BLOCK, 0, &mut out);
    }
    assert!(
        rms(&out) < 1e-3,
        "master gain 0 should mute, got {}",
        rms(&out)
    );
}

/// A node with two DISTINCT input ports: it writes `in0 - in1`, so a renderer
/// that fed port 1 the same buffer as port 0 (or nothing at all) is visible in
/// the output rather than merely suspected.
struct DifferenceNode {
    ports: usize,
}

impl vocara_engine::node::Node for DifferenceNode {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn process(&mut self, io: &mut vocara_engine::node::ProcessIo) {
        let n = io.frames * CHANNELS;
        let a = io.inputs[0];
        let b = io.inputs[1];
        let out = &mut io.outputs[0];
        for i in 0..n {
            out[i] = a[i] - b[i];
        }
    }

    fn num_in(&self) -> usize {
        self.ports
    }
}

#[test]
fn every_declared_input_port_reaches_the_node() {
    // src1(0.5) -> port 0, src2(0.3) -> port 1 of a 2-input node -> master.
    // The renderer used to build a one-element input array and truncate the
    // slice to it, so port 1 silently never arrived: this rendered 0.5, not
    // the 0.2 the node's own contract asks for.
    let desc = GraphDesc {
        nodes: vec![
            NodeEntry::new(1, Box::new(BufferSource::new(dc(0.5, 500), 0))),
            NodeEntry::new(2, Box::new(BufferSource::new(dc(0.3, 500), 0))),
            NodeEntry::new(3, Box::new(DifferenceNode { ports: 2 })),
            NodeEntry::new(4, Box::new(MasterNode::unity())),
        ],
        edges: vec![
            Edge {
                from: 1,
                from_port: 0,
                to: 3,
                to_port: 0,
            },
            Edge {
                from: 2,
                from_port: 0,
                to: 3,
                to_port: 1,
            },
            Edge::simple(3, 4),
        ],
        master: 4,
    };
    let mut g = desc.compile(SR, BLOCK).unwrap();
    let mut out = vec![0.0; 128 * CHANNELS];
    g.render(128, 0, &mut out);
    for &s in &out {
        assert!(
            (s - 0.2).abs() < 1e-4,
            "port 1 must reach the node: expected 0.2, got {s}"
        );
    }
}

#[test]
fn more_input_ports_than_the_renderer_can_hand_over_is_rejected() {
    // The bound exists because the renderer's port array is on the stack (the
    // audio thread allocates nothing per block). Exceeding it must fail at
    // compile, where the old graph keeps playing — never at render, and never
    // by quietly dropping the extra ports.
    let desc = GraphDesc {
        nodes: vec![NodeEntry::new(
            7,
            Box::new(DifferenceNode {
                ports: vocara_engine::graph::MAX_IN + 1,
            }),
        )],
        edges: vec![],
        master: 7,
    };
    assert_eq!(
        desc.compile(SR, BLOCK).err(),
        Some(CompileError::TooManyInputs(7))
    );
}
