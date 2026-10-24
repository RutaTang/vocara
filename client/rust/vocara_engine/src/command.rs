//! Parameter command channel (control → audio).
//!
//! The control thread coalesces duplicate `(node, param)` updates (a slider drag firing
//! many events collapses to its latest value) and flushes them into a wait-free ring. The
//! audio thread drains a bounded number per block and applies them — no allocation, no lock.

use std::collections::VecDeque;

use crate::graph::{Graph, NodeId};
use crate::graph_cell::GraphCell;
use crate::ring::{spsc, Consumer, Producer};

/// A single parameter change. POD so it crosses the ring as a value (no Drop).
#[derive(Clone, Copy, Default)]
pub struct ParamCmd {
    pub node: NodeId,
    pub param: u16,
    pub value: f32,
}

/// Control-thread half: coalesces then flushes into the ring.
pub struct CommandSender {
    prod: Producer<ParamCmd>,
    /// key = (node << 16) | param  ->  latest value. A deque so an overflow flush drains
    /// FIFO from the front in O(1) per command (a post-rebuild storm can queue far more
    /// than the ring capacity).
    pending: VecDeque<(u64, f32)>,
}

impl CommandSender {
    /// Record the latest value for `(node, param)` (coalescing in place).
    pub fn set(&mut self, node: NodeId, param: u16, value: f32) {
        let key = ((node as u64) << 16) | param as u64;
        if let Some(e) = self.pending.iter_mut().find(|(k, _)| *k == key) {
            e.1 = value;
        } else {
            self.pending.push_back((key, value));
        }
    }

    /// Push pending values into the ring in FIFO order; keeps any that don't fit for the
    /// next flush (never blocks). Draining from the FRONT preserves submission order across
    /// a ring overflow — draining from the back would starve exactly the OLDEST commands
    /// (the first tracks' settings) until the user happened to touch them again.
    pub fn flush(&mut self) {
        while let Some(&(key, value)) = self.pending.front() {
            let cmd = ParamCmd {
                node: (key >> 16) as NodeId,
                param: (key & 0xffff) as u16,
                value,
            };
            if self.prod.push(cmd) {
                self.pending.pop_front();
            } else {
                break; // ring full; retry next flush
            }
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// Audio-thread half: drains and applies.
pub struct CommandReceiver {
    cons: Consumer<ParamCmd>,
}

impl CommandReceiver {
    /// Apply up to `max` queued commands to `graph`. RT-safe (bounded, wait-free).
    pub fn apply(&mut self, graph: &mut Graph, max: usize) {
        for _ in 0..max {
            match self.cons.pop() {
                Some(c) => graph.set_param(c.node, c.param, c.value),
                None => break,
            }
        }
    }

    /// Apply up to `max` queued commands, each to the graph CURRENT when it is popped. A
    /// command pushed after `publish_graph` (a commit's parameter storm) must reach the new
    /// graph, never the retired one a block loaded before the publish landed: the pop's
    /// Acquire pairs with the push, which the publish precedes, so this load sees the new
    /// pointer. RT-safe (bounded, wait-free).
    pub fn apply_current(&mut self, cell: &GraphCell, max: usize) {
        for _ in 0..max {
            match self.cons.pop() {
                // SAFETY: the reaper's one-block grace epoch keeps any graph loaded during
                // this block alive until it ends; the reference is not kept past the call.
                Some(c) => unsafe { &mut *cell.load() }.set_param(c.node, c.param, c.value),
                None => break,
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.cons.len()
    }
}

pub fn command_channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let (prod, cons) = spsc(capacity);
    (
        CommandSender {
            prod,
            pending: VecDeque::new(),
        },
        CommandReceiver { cons },
    )
}
