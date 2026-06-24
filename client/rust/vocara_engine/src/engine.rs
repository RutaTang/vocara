//! Transport + the render entry point shared by the offline harness and the live cpal
//! callback. One sample-counter clock drives every source; play/pause/seek/loop are
//! expressed against it. Loop wrap happens inside a render call by splitting the block at
//! the loop boundary, so the output is sample-contiguous across the seam (no gap, no
//! re-trigger). The reverb tail is *not* reset on wrap (it rings through); only `seek`
//! resets node state so a tail doesn't bleed across a jump.

use crate::graph::Graph;
use crate::sample::{Smp, CHANNELS};

/// Transport state, expressed in timeline samples.
#[derive(Clone, Copy, Debug, Default)]
pub struct Transport {
    pub playing: bool,
    pub frame: i64,
    pub loop_enabled: bool,
    pub loop_start: i64,
    pub loop_end: i64,
}

pub struct Engine {
    graph: Graph,
    transport: Transport,
    sr: f64,
    max_block: usize,
}

impl Engine {
    pub fn new(graph: Graph, sr: f64, max_block: usize) -> Self {
        Engine {
            graph,
            transport: Transport::default(),
            sr,
            max_block,
        }
    }

    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    pub fn sample_rate(&self) -> f64 {
        self.sr
    }

    pub fn frame(&self) -> i64 {
        self.transport.frame
    }

    pub fn play(&mut self) {
        self.transport.playing = true;
    }

    pub fn pause(&mut self) {
        self.transport.playing = false;
    }

    /// Jump the playhead. Resets node state so reverb/filter tails don't bleed across the
    /// discontinuity (unlike a loop wrap, which preserves the tail).
    pub fn seek(&mut self, frame: i64) {
        self.transport.frame = frame.max(0);
        self.graph.reset();
    }

    pub fn set_loop(&mut self, enabled: bool, start: i64, end: i64) {
        self.transport.loop_enabled = enabled;
        self.transport.loop_start = start.max(0);
        self.transport.loop_end = end.max(0);
    }

    /// Render `frames` of interleaved stereo into `out`. Advances the clock; splits the
    /// block at the loop boundary for a gapless wrap; chunks to `max_block`.
    pub fn render(&mut self, frames: usize, out: &mut [Smp]) {
        let total = frames * CHANNELS;
        if !self.transport.playing {
            for s in &mut out[..total] {
                *s = 0.0;
            }
            return;
        }

        let t = &mut self.transport;
        let mut done = 0;
        while done < frames {
            if t.loop_enabled && t.loop_end > t.loop_start && t.frame >= t.loop_end {
                t.frame = t.loop_start;
            }
            let mut chunk = (frames - done).min(self.max_block);
            if t.loop_enabled && t.loop_end > t.loop_start {
                let to_end = (t.loop_end - t.frame).max(0) as usize;
                if to_end == 0 {
                    // Degenerate loop region; stop advancing to avoid a busy loop.
                    for s in &mut out[done * CHANNELS..total] {
                        *s = 0.0;
                    }
                    return;
                }
                chunk = chunk.min(to_end);
            }
            let s = done * CHANNELS;
            let e = (done + chunk) * CHANNELS;
            self.graph.render(chunk, t.frame, &mut out[s..e]);
            t.frame += chunk as i64;
            done += chunk;
        }
    }
}
