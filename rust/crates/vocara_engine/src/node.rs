//! The graph node contract — the durable, engine-agnostic seam.
//!
//! Every processing unit (file source, EQ, compressor, reverb, gain/pan, sum, master)
//! implements [`Node`]. All mutable DSP state is owned by the node and sized once in
//! [`Node::prepare`]; [`Node::process`] runs on the audio thread and must not allocate,
//! lock, block, or panic.

use crate::sample::Smp;

/// The per-block I/O handed to a node. `inputs` and `outputs` are distinct allocations
/// (the renderer never aliases an input buffer as an output), so there is no `&`/`&mut`
/// overlap. Each buffer is interleaved stereo of length `frames * CHANNELS`.
pub struct ProcessIo<'a> {
    /// One slice per input port. Empty for a source. For a fan-in port the renderer has
    /// already summed all incoming edges into this single buffer.
    pub inputs: &'a [&'a [Smp]],
    /// One slice per output port; the node writes its result here.
    pub outputs: &'a mut [&'a mut [Smp]],
    /// Number of frames to process this block; always `1..=max_block`.
    pub frames: usize,
    /// Absolute sample index of `outputs[*][0]` on the shared timeline.
    pub tl: i64,
    /// Device sample rate.
    pub sr: f64,
}

impl<'a> ProcessIo<'a> {
    /// Convenience: the single primary input port, or `None` for a source.
    #[inline]
    pub fn in0(&self) -> Option<&[Smp]> {
        self.inputs.first().copied()
    }
}

/// A processing node in the DAG. `Send` so the compiled graph can be moved onto the audio
/// thread; the borrow checker then proves no control-thread aliasing of live state.
pub trait Node: Send {
    /// Allocate / size all internal state for the given rate and worst-case block. Runs on
    /// the control thread before the node is ever processed.
    fn prepare(&mut self, sample_rate: f64, max_block: usize);

    /// Process one block. Real-time: no alloc, no lock, no syscall, no panic.
    fn process(&mut self, io: &mut ProcessIo);

    /// Apply a single parameter change (target only — smoothing happens in `process`).
    /// O(1); never recomputes more than the addressed parameter.
    fn set_param(&mut self, _param: u16, _value: f32) {}

    /// Reported processing latency in samples, for plugin-delay compensation. 0 for
    /// EQ/gain/sum; non-zero only if a node adds true look-ahead.
    fn latency(&self) -> usize {
        0
    }

    /// Clear histories / delay lines (used on seek so tails don't bleed across a jump).
    fn reset(&mut self) {}

    /// Take over the signal state of `old`, the node that ran under this one's id in the graph
    /// a rebuild replaces: a reverb's tank and a delay's line, filter histories, envelopes. A
    /// rebuilt graph's nodes otherwise started from silence, so every rebuild during playback
    /// (a clip nudged, a send added) cut the reverb tail dead, clicked every EQ'd track and
    /// jumped every compressor. The parameters stay this node's own (the rebuild pushed them).
    /// Called on the audio thread, at the first block the new graph renders: swap or copy
    /// state, never allocate. `old` may be another kind of node; then nothing is taken. The
    /// default carries nothing (a node with no signal state, or a source).
    fn carry_state(&mut self, _old: &mut dyn Node) {}

    /// The node as [`Any`](std::any::Any), so [`carry_state`](Self::carry_state) can tell
    /// whether `old` is its own kind. `None` for a node that carries nothing.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Called once per block while the transport is STOPPED (the node is not processed).
    /// RT-safe. Default: nothing.
    fn idle(&mut self) {}

    /// Number of input ports (0 for a source).
    fn num_in(&self) -> usize {
        1
    }

    /// Number of output ports.
    fn num_out(&self) -> usize {
        1
    }
}

/// `old` as a `T`, when it is one: the check every [`Node::carry_state`] starts with.
#[inline]
pub fn same_kind<T: 'static>(old: &mut dyn Node) -> Option<&mut T> {
    old.as_any_mut()?.downcast_mut::<T>()
}
