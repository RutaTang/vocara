//! Builds the vocara signal graph from a high-level spec.
//!
//! Per vocal track:  Source → EQ → Comp → Gain → Pan → Master (dry)
//!                                         └─ Send → [shared Reverb bus] → Master
//! Backing/reference: Source → Gain → Pan → Master   (no EQ/Comp/Send — clean A/B)
//!
//! The send taps **post-gain / pre-pan**, so the wet signal reaches the one shared reverb
//! centered and gets its stereo image from the reverb's width. Node ids are stable and
//! derived from the track id, so the control plane can address any node and a future graph
//! rebuild can migrate state by id.

use crate::dsp::biquad::EqNode;
use crate::dsp::compressor::CompressorNode;
use crate::dsp::deesser::DeEsserNode;
use crate::dsp::delay::DelayNode;
use crate::dsp::doubler::DoublerNode;
use crate::dsp::gain_pan::PanNode;
use crate::dsp::gate::GateNode;
use crate::dsp::limiter::LimiterNode;
use crate::dsp::master::MasterNode;
use crate::dsp::reverb::ReverbNode;
use crate::dsp::saturation::SaturationNode;
use crate::graph::{Edge, GraphDesc, NodeEntry, NodeId};
use crate::node::Node;

pub const REVERB_ID: NodeId = NodeId::MAX - 1;
pub const MASTER_ID: NodeId = NodeId::MAX;

const PER_TRACK: NodeId = 32;

/// Max post-fader aux sends per track / bus (Logic-style).
pub const MAX_SENDS: usize = 4;
/// Node-id offset of the first aux send within a track/bus's slot band.
const SEND_OFFSET: NodeId = 16;

pub fn source_id(track: NodeId) -> NodeId {
    track * PER_TRACK
}
pub fn eq_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 1
}
pub fn comp_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 2
}
pub fn gain_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 3
}
pub fn pan_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 4
}
pub fn send_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 5
}
// Extra insert plugins (FxKind tags 2..7). Fixed offsets so node ids never move
// when the chain is reordered (only the edges change).
pub fn deesser_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 6
}
pub fn gate_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 7
}
pub fn delay_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 8
}
pub fn saturation_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 9
}
pub fn doubler_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 10
}
pub fn limiter_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 11
}
pub fn reverb_id(track: NodeId) -> NodeId {
    track * PER_TRACK + 12
}
/// Aux send node `i` (0..MAX_SENDS) for a track — a post-fader gain into a bus.
pub fn track_send_id(track: NodeId, i: usize) -> NodeId {
    track * PER_TRACK + SEND_OFFSET + i as NodeId
}

/// A reverb used as a series insert: full dry + a wet blend (off = wet 0), not a
/// send-bus return (which is fully wet, no dry).
fn reverb_insert() -> ReverbNode {
    ReverbNode::new(0.5, 0.5, 0.0, 1.0, 1.0)
}

/// Builds the node id + boxed DSP node for a track insert `tag` (FxKind index).
fn track_insert(
    tid: NodeId,
    tag: u8,
    comp_meter: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
) -> (NodeId, Box<dyn Node>) {
    match tag {
        1 => (
            comp_id(tid),
            Box::new(CompressorNode::default_vocal().with_meter(comp_meter)),
        ),
        2 => (deesser_id(tid), Box::new(DeEsserNode::new())),
        3 => (gate_id(tid), Box::new(GateNode::new())),
        4 => (delay_id(tid), Box::new(DelayNode::new())),
        5 => (saturation_id(tid), Box::new(SaturationNode::new())),
        6 => (doubler_id(tid), Box::new(DoublerNode::new())),
        7 => (limiter_id(tid), Box::new(LimiterNode::new())),
        8 => (reverb_id(tid), Box::new(reverb_insert())),
        _ => (eq_id(tid), Box::new(EqNode::default_strip())),
    }
}

// --- FX buses (DAW group/aux tracks) ----------------------------------------
//
// Buses live in a high node-id band, below MASTER/REVERB and clear of any
// track's `track * PER_TRACK` ids. Each bus uses 5 nodes: a summing input
// (where routed tracks/buses fan in), EQ, comp, gain (the bus fader), and a
// reverb send.
const BUS_STRIDE: NodeId = 32;
// Below MASTER (MAX) / REVERB (MAX - 1), with room for each bus's slot band
// (inserts + aux sends) to grow upward without colliding.
const BUS_TOP: NodeId = NodeId::MAX - 256;

pub fn bus_sum_id(bus: NodeId) -> NodeId {
    BUS_TOP - bus * BUS_STRIDE
}
pub fn bus_eq_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 1
}
pub fn bus_comp_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 2
}
pub fn bus_gain_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 3
}
pub fn bus_send_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 4
}
pub fn bus_pan_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 5
}
pub fn bus_deesser_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 6
}
pub fn bus_gate_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 7
}
pub fn bus_delay_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 8
}
pub fn bus_saturation_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 9
}
pub fn bus_doubler_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 10
}
pub fn bus_limiter_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 11
}
pub fn bus_reverb_id(bus: NodeId) -> NodeId {
    bus_sum_id(bus) + 12
}
/// Aux send node `i` for a bus — a post-fader gain into another bus.
pub fn bus_aux_send_id(bus: NodeId, i: usize) -> NodeId {
    bus_sum_id(bus) + SEND_OFFSET + i as NodeId
}

/// Builds the node id + boxed DSP node for a bus insert `tag` (FxKind index).
fn bus_insert(
    bid: NodeId,
    tag: u8,
    comp_meter: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
) -> (NodeId, Box<dyn Node>) {
    match tag {
        1 => (
            bus_comp_id(bid),
            Box::new(CompressorNode::default_vocal().with_meter(comp_meter)),
        ),
        2 => (bus_deesser_id(bid), Box::new(DeEsserNode::new())),
        3 => (bus_gate_id(bid), Box::new(GateNode::new())),
        4 => (bus_delay_id(bid), Box::new(DelayNode::new())),
        5 => (bus_saturation_id(bid), Box::new(SaturationNode::new())),
        6 => (bus_doubler_id(bid), Box::new(DoublerNode::new())),
        7 => (bus_limiter_id(bid), Box::new(LimiterNode::new())),
        8 => (bus_reverb_id(bid), Box::new(reverb_insert())),
        _ => (bus_eq_id(bid), Box::new(EqNode::default_strip())),
    }
}

/// Resolves an output routing (Some(bus) → that bus's input, None → master).
fn output_target(output: Option<NodeId>) -> NodeId {
    match output {
        Some(b) => bus_sum_id(b),
        None => MASTER_ID,
    }
}

/// One track and its mix/effect settings. `source` is any source node (a [`BufferSource`]
/// offline, a ring-fed source live).
///
/// [`BufferSource`]: crate::source::BufferSource
pub struct TrackSpec {
    pub id: NodeId,
    pub source: Box<dyn Node>,
    /// Vocals get the EQ → Comp insert chain and a reverb send; backing/reference don't.
    pub vocal: bool,
    pub gain: f32,
    pub pan: f32,
    /// Reverb send level in dB (vocals). Use a very low value (e.g. -120) for "send off".
    pub send_db: f32,
    /// Output routing: Some(bus id) feeds an FX bus, None feeds the master directly.
    pub bus: Option<NodeId>,
    /// Insert-chain order as `FxKind` tags (0 = EQ, 1 = Comp). The chain is wired
    /// `source → chain[0] → … → gain` in this order; only listed inserts exist.
    pub chain: Vec<u8>,
    /// Post-fader aux sends: (target bus index, linear level). Each taps the
    /// fader output into a bus, independent of the main output routing.
    pub sends: Vec<(NodeId, f32)>,
    /// Optional gain-reduction meter the vocal compressor publishes to (for a UI meter).
    pub comp_meter: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
}

impl TrackSpec {
    pub fn vocal(id: NodeId, source: Box<dyn Node>) -> Self {
        TrackSpec {
            id,
            source,
            vocal: true,
            gain: 1.0,
            pan: 0.0,
            send_db: -120.0,
            bus: None,
            chain: vec![0, 1], // EQ → Comp
            sends: Vec::new(),
            comp_meter: None,
        }
    }
    pub fn backing(id: NodeId, source: Box<dyn Node>) -> Self {
        TrackSpec {
            id,
            source,
            vocal: false,
            gain: 1.0,
            pan: 0.0,
            send_db: -120.0,
            bus: None,
            chain: Vec::new(),
            sends: Vec::new(),
            comp_meter: None,
        }
    }
}

/// One FX bus (DAW group/aux): a summing input, an EQ → Comp insert chain, a
/// fader, and a reverb send. Tracks (or other buses) route their output into it.
pub struct BusSpec {
    /// 0-based bus index (its node ids derive from this).
    pub id: NodeId,
    pub gain: f32,
    /// Reverb send level in dB (-120 = off).
    pub send_db: f32,
    /// Output routing: Some(bus id) feeds another bus, None feeds the master.
    pub output: Option<NodeId>,
    /// Insert-chain order (`FxKind` tags), like [`TrackSpec::chain`].
    pub chain: Vec<u8>,
    /// Post-fader aux sends: (target bus index, linear level).
    pub sends: Vec<(NodeId, f32)>,
    pub comp_meter: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
}

/// The whole project: tracks, FX buses, the one shared reverb, and the master.
pub struct ProjectSpec {
    pub tracks: Vec<TrackSpec>,
    pub buses: Vec<BusSpec>,
    pub master_gain: f32,
    pub master_limit: bool,
    pub reverb: ReverbNode,
}

impl ProjectSpec {
    pub fn new(tracks: Vec<TrackSpec>) -> Self {
        ProjectSpec {
            tracks,
            buses: Vec::new(),
            master_gain: 1.0,
            master_limit: true,
            reverb: ReverbNode::default_bus(),
        }
    }
}

/// Assemble the [`GraphDesc`]. The shared reverb and master always exist; a reverb with no
/// active sends simply processes silence.
pub fn build(spec: ProjectSpec) -> GraphDesc {
    let ProjectSpec {
        tracks,
        buses,
        master_gain,
        master_limit,
        reverb,
    } = spec;

    let mut nodes: Vec<NodeEntry> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    nodes.push(NodeEntry::new(REVERB_ID, Box::new(reverb)));
    nodes.push(NodeEntry::new(
        MASTER_ID,
        Box::new(MasterNode::new(master_gain, master_limit)),
    ));
    edges.push(Edge::simple(REVERB_ID, MASTER_ID)); // reverb return → master

    // FX buses: Sum → [inserts, incl. a reverb send tap] → Gain → Pan → output.
    for b in &buses {
        let sum = bus_sum_id(b.id);
        let gain = bus_gain_id(b.id);
        let pan = bus_pan_id(b.id);
        nodes.push(NodeEntry::new(sum, Box::new(MasterNode::new(1.0, false))));
        nodes.push(NodeEntry::new(gain, Box::new(MasterNode::new(b.gain, false))));
        nodes.push(NodeEntry::new(pan, Box::new(PanNode::new(0.0))));
        // Insert chain in configured order: sum → inserts → gain.
        let mut prev = sum;
        for &tag in &b.chain {
            let (id, node) = bus_insert(b.id, tag, b.comp_meter.clone());
            nodes.push(NodeEntry::new(id, node));
            edges.push(Edge::simple(prev, id));
            prev = id;
        }
        edges.push(Edge::simple(prev, gain));
        edges.push(Edge::simple(gain, pan));
        edges.push(Edge::simple(pan, output_target(b.output))); // bus out → master / bus
        // Post-fader aux sends from this bus into other buses.
        for (i, &(target, level)) in b.sends.iter().enumerate() {
            let s = bus_aux_send_id(b.id, i);
            nodes.push(NodeEntry::new(s, Box::new(MasterNode::new(level, false))));
            edges.push(Edge::simple(gain, s));
            edges.push(Edge::simple(s, bus_sum_id(target)));
        }
    }

    for t in tracks {
        let tid = t.id;
        let src = source_id(tid);
        let gain = gain_id(tid);
        let pan = pan_id(tid);

        nodes.push(NodeEntry::new(src, t.source));
        nodes.push(NodeEntry::new(gain, Box::new(MasterNode::new(t.gain, false))));
        nodes.push(NodeEntry::new(pan, Box::new(PanNode::new(t.pan))));
        edges.push(Edge::simple(pan, output_target(t.bus))); // dry → master / bus

        if t.vocal {
            // Insert chain in configured order: source → inserts → gain.
            let mut prev = src;
            for &tag in &t.chain {
                let (id, node) = track_insert(tid, tag, t.comp_meter.clone());
                nodes.push(NodeEntry::new(id, node));
                edges.push(Edge::simple(prev, id));
                prev = id;
            }
            edges.push(Edge::simple(prev, gain));
            edges.push(Edge::simple(gain, pan));
        } else {
            edges.push(Edge::simple(src, gain));
            edges.push(Edge::simple(gain, pan));
        }
        // Post-fader aux sends: tap the fader output into target buses.
        for (i, &(target, level)) in t.sends.iter().enumerate() {
            let s = track_send_id(tid, i);
            nodes.push(NodeEntry::new(s, Box::new(MasterNode::new(level, false))));
            edges.push(Edge::simple(gain, s));
            edges.push(Edge::simple(s, bus_sum_id(target)));
        }
    }

    GraphDesc {
        nodes,
        edges,
        master: MASTER_ID,
    }
}
