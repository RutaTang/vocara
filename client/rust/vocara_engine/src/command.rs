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
    /// The graph it was sent for (see [`CommandSender::begin_epoch`]).
    pub epoch: u32,
}

/// Control-thread half: coalesces then flushes into the ring.
pub struct CommandSender {
    prod: Producer<ParamCmd>,
    /// key = (node << 16) | param  ->  latest value. A deque so an overflow flush drains
    /// FIFO from the front in O(1) per command (a post-rebuild storm can queue far more
    /// than the ring capacity).
    pending: VecDeque<(u64, f32)>,
    /// The graph epoch every command sent now is stamped with.
    epoch: u32,
}

impl CommandSender {
    /// Start a new graph epoch (call as a new graph is published, and stamp the graph with
    /// the returned value): commands still pending here or in the ring were sent for the
    /// graph being replaced. They address nodes by id, and the new graph can number a
    /// different track under the same id, so the audio thread drops them instead of
    /// applying them on top of the values the new graph was committed with.
    pub fn begin_epoch(&mut self) -> u32 {
        self.pending.clear();
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

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
                epoch: self.epoch,
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
    /// Apply up to `max` queued commands to `graph` (those sent for it: see
    /// [`CommandSender::begin_epoch`]). RT-safe (bounded, wait-free).
    pub fn apply(&mut self, graph: &mut Graph, max: usize) {
        for _ in 0..max {
            match self.cons.pop() {
                Some(c) => {
                    if c.epoch == graph.epoch() {
                        graph.set_param(c.node, c.param, c.value);
                    }
                }
                None => break,
            }
        }
    }

    /// Apply up to `max` queued commands, each to the graph CURRENT when it is popped. A
    /// command pushed after `publish_graph` (a commit's parameter storm) must reach the new
    /// graph, never the retired one a block loaded before the publish landed: the pop's
    /// Acquire pairs with the push, which the publish precedes, so this load sees the new
    /// pointer. A command sent for an EARLIER graph (still queued when the publish landed)
    /// is dropped: see [`CommandSender::begin_epoch`]. RT-safe (bounded, wait-free).
    pub fn apply_current(&mut self, cell: &GraphCell, max: usize) {
        for _ in 0..max {
            match self.cons.pop() {
                Some(c) => {
                    // SAFETY: the reaper's one-block grace epoch keeps any graph loaded
                    // during this block alive until it ends; the reference is not kept
                    // past the call.
                    let g = unsafe { &mut *cell.load() };
                    if c.epoch == g.epoch() {
                        g.set_param(c.node, c.param, c.value);
                    }
                }
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
            epoch: 0,
        },
        CommandReceiver { cons },
    )
}
