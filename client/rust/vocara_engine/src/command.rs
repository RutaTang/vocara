//! Parameter command channel (control → audio).
//!
//! The control thread coalesces duplicate `(node, param)` updates (a slider drag firing
//! many events collapses to its latest value) and flushes them into a wait-free ring. The
//! audio thread drains a bounded number per block and applies them — no allocation, no lock.

use crate::graph::{Graph, NodeId};
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
    pending: Vec<(u64, f32)>, // key = (node << 16) | param  ->  latest value
}

impl CommandSender {
    /// Record the latest value for `(node, param)` (coalescing in place).
    pub fn set(&mut self, node: NodeId, param: u16, value: f32) {
        let key = ((node as u64) << 16) | param as u64;
        if let Some(e) = self.pending.iter_mut().find(|(k, _)| *k == key) {
            e.1 = value;
        } else {
            self.pending.push((key, value));
        }
    }

    /// Push pending values into the ring; keeps any that don't fit for the next flush
    /// (never blocks).
    pub fn flush(&mut self) {
        while let Some(&(key, value)) = self.pending.last() {
            let cmd = ParamCmd {
                node: (key >> 16) as NodeId,
                param: (key & 0xffff) as u16,
                value,
            };
            if self.prod.push(cmd) {
                self.pending.pop();
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

    pub fn pending(&self) -> usize {
        self.cons.len()
    }
}

pub fn command_channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let (prod, cons) = spsc(capacity);
    (
        CommandSender {
            prod,
            pending: Vec::new(),
        },
        CommandReceiver { cons },
    )
}
