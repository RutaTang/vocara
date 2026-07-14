//! The DAG graph runtime: compile a declarative description into a topo-ordered schedule,
//! then render it block-by-block over per-node buffers. Fan-in (the shared reverb send
//! bus) is a real vertex — the renderer sums every incoming edge into a node's input buffer
//! before processing. Cycles are rejected at compile time so the audio thread never sees
//! one; node recursion (reverb delay lines) lives *inside* a node, keeping the graph
//! acyclic.

use std::collections::{HashMap, VecDeque};

use crate::node::{Node, ProcessIo};
use crate::sample::{add_into, clear, Smp, CHANNELS};

pub type NodeId = u32;

/// One node plus its stable id.
pub struct NodeEntry {
    pub id: NodeId,
    pub node: Box<dyn Node>,
}

impl NodeEntry {
    pub fn new(id: NodeId, node: Box<dyn Node>) -> Self {
        NodeEntry { id, node }
    }
}

/// A directed connection from one node's output port to another's input port.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub from: NodeId,
    pub from_port: usize,
    pub to: NodeId,
    pub to_port: usize,
}

impl Edge {
    /// Port-0 to port-0 connection (the common case).
    pub fn simple(from: NodeId, to: NodeId) -> Self {
        Edge {
            from,
            from_port: 0,
            to,
            to_port: 0,
        }
    }
}

/// Declarative graph: nodes, edges, and which node feeds the device.
pub struct GraphDesc {
    pub nodes: Vec<NodeEntry>,
    pub edges: Vec<Edge>,
    pub master: NodeId,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The edges form a cycle (rejected; the old graph keeps playing).
    Cycle,
    /// `master` is not among the nodes.
    MissingMaster,
    /// An edge references an unknown node id.
    BadEdge,
    /// A node has more than one output port (unsupported; fan-out is done via edges).
    MultiOutput(NodeId),
}

struct ScheduledNode {
    id: NodeId,
    node: Box<dyn Node>,
    /// Per input port: the source `(schedule index, output port)`s summed into that port.
    in_edges: Vec<Vec<(usize, usize)>>,
    /// One scratch buffer per input port (the fan-in sum lands here).
    in_scratch: Vec<Box<[Smp]>>,
    /// Exactly one output buffer (fan-out is multiple edges reading it).
    output: Box<[Smp]>,
}

/// A compiled, ready-to-render graph. Owns all nodes + buffers; allocation happens only
/// here (at compile), never during `render`.
pub struct Graph {
    schedule: Vec<ScheduledNode>,
    master_idx: usize,
    index: HashMap<NodeId, usize>,
    sr: f64,
    max_block: usize,
}

impl GraphDesc {
    /// Compile to a renderable [`Graph`]: validate, topo-sort (Kahn) with cycle rejection,
    /// allocate buffers, and `prepare()` every node at `sr` / `max_block`.
    pub fn compile(self, sr: f64, max_block: usize) -> Result<Graph, CompileError> {
        let GraphDesc {
            nodes,
            edges,
            master,
        } = self;
        let n = nodes.len();

        let ids: Vec<NodeId> = nodes.iter().map(|e| e.id).collect();
        let id_to_pos: HashMap<NodeId, usize> =
            ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        if !id_to_pos.contains_key(&master) {
            return Err(CompileError::MissingMaster);
        }

        let mut taken: Vec<Option<Box<dyn Node>>> =
            nodes.into_iter().map(|e| Some(e.node)).collect();
        let num_in: Vec<usize> = taken
            .iter()
            .map(|o| o.as_ref().unwrap().num_in())
            .collect();
        for (pos, o) in taken.iter().enumerate() {
            if o.as_ref().unwrap().num_out() != 1 {
                return Err(CompileError::MultiOutput(ids[pos]));
            }
        }

        // Build successor lists + in-degree for Kahn's algorithm.
        let mut indeg = vec![0usize; n];
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in &edges {
            let from = *id_to_pos.get(&e.from).ok_or(CompileError::BadEdge)?;
            let to = *id_to_pos.get(&e.to).ok_or(CompileError::BadEdge)?;
            // Every node has exactly one output port (enforced above), so any from_port > 0 is
            // out of range. Reject it here: in release the renderer would otherwise silently
            // read port 0's buffer (only a debug_assert used to catch it).
            if e.from_port != 0 {
                return Err(CompileError::BadEdge);
            }
            indeg[to] += 1;
            succ[from].push(to);
        }

        let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);
        while let Some(p) = queue.pop_front() {
            order.push(p);
            for &s in &succ[p] {
                indeg[s] -= 1;
                if indeg[s] == 0 {
                    queue.push_back(s);
                }
            }
        }
        if order.len() != n {
            return Err(CompileError::Cycle);
        }

        // pos -> schedule index.
        let mut sched_of_pos = vec![0usize; n];
        for (si, &p) in order.iter().enumerate() {
            sched_of_pos[p] = si;
        }

        // Per scheduled node, the summed input edges by destination port.
        let mut in_edges_by_sched: Vec<Vec<Vec<(usize, usize)>>> = order
            .iter()
            .map(|&p| vec![Vec::new(); num_in[p]])
            .collect();
        for e in &edges {
            let fp = id_to_pos[&e.from];
            let tp = id_to_pos[&e.to];
            let fs = sched_of_pos[fp];
            let ts = sched_of_pos[tp];
            if e.to_port >= in_edges_by_sched[ts].len() {
                return Err(CompileError::BadEdge);
            }
            in_edges_by_sched[ts][e.to_port].push((fs, e.from_port));
        }

        let buf = || vec![0.0; max_block * CHANNELS].into_boxed_slice();
        let mut schedule: Vec<ScheduledNode> = Vec::with_capacity(n);
        for (si, &p) in order.iter().enumerate() {
            let mut node = taken[p].take().unwrap();
            node.prepare(sr, max_block);
            let in_edges = std::mem::take(&mut in_edges_by_sched[si]);
            let in_scratch = (0..num_in[p]).map(|_| buf()).collect();
            schedule.push(ScheduledNode {
                id: ids[p],
                node,
                in_edges,
                in_scratch,
                output: buf(),
            });
        }

        let index: HashMap<NodeId, usize> =
            schedule.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
        let master_idx = sched_of_pos[id_to_pos[&master]];

        Ok(Graph {
            schedule,
            master_idx,
            index,
            sr,
            max_block,
        })
    }
}

impl Graph {
    /// Render one block into `out` (interleaved stereo master, length `frames * CHANNELS`).
    pub fn render(&mut self, frames: usize, tl: i64, out: &mut [Smp]) {
        debug_assert!(frames <= self.max_block);
        let count = self.schedule.len();
        for i in 0..count {
            let (done, rest) = self.schedule.split_at_mut(i);
            // Disjoint field borrows so gather (writes in_scratch) and process (writes
            // output, reads in_scratch) don't collide.
            let ScheduledNode {
                node,
                in_edges,
                in_scratch,
                output,
                ..
            } = &mut rest[0];

            // Fan-in: sum every incoming edge into the destination port's scratch buffer.
            for (port, sources) in in_edges.iter().enumerate() {
                let scratch = &mut in_scratch[port];
                clear(scratch, frames);
                for &(src_idx, src_port) in sources {
                    debug_assert_eq!(src_port, 0);
                    add_into(scratch, &done[src_idx].output, frames);
                }
            }

            let num_in = in_edges.len();
            let inputs_storage: [&[Smp]; 1] = if num_in >= 1 {
                [&in_scratch[0][..]]
            } else {
                [&[][..]]
            };
            let inputs: &[&[Smp]] = &inputs_storage[..num_in.min(1)];
            let mut out_storage: [&mut [Smp]; 1] = [&mut output[..]];
            let mut io = ProcessIo {
                inputs,
                outputs: &mut out_storage[..],
                frames,
                tl,
                sr: self.sr,
            };
            node.process(&mut io);
        }

        let n = frames * CHANNELS;
        out[..n].copy_from_slice(&self.schedule[self.master_idx].output[..n]);
    }

    /// Apply a parameter change to a node by id (control plane / command ring).
    pub fn set_param(&mut self, node: NodeId, param: u16, value: f32) {
        if let Some(&i) = self.index.get(&node) {
            self.schedule[i].node.set_param(param, value);
        }
    }

    /// Clear every node's internal state (on seek, so tails don't bleed across the jump).
    pub fn reset(&mut self) {
        for s in &mut self.schedule {
            s.node.reset();
        }
    }

    /// Borrow a node mutably by id (tests / metering).
    pub fn node_mut(&mut self, node: NodeId) -> Option<&mut (dyn Node + '_)> {
        match self.index.get(&node) {
            Some(&i) => Some(self.schedule[i].node.as_mut()),
            None => None,
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sr
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }
}
