//! A buffer-backed source node: owns interleaved-stereo PCM placed at `start_frame` on the
//! shared timeline, emitting silence before/after its span. This is the deterministic
//! offline counterpart of the live ring-fed source (which reads decoded PCM from an SPSC
//! ring); both place audio against the one sample-counter clock, so multi-source sync is
//! structural — there is no per-source drift correction.

use std::sync::Arc;

use crate::node::{Node, ProcessIo};
use crate::ring::Consumer;
use crate::sample::{Smp, CHANNELS};

pub struct BufferSource {
    pcm: Arc<[Smp]>, // interleaved stereo
    start_frame: i64,
    len_frames: i64,
}

impl BufferSource {
    pub fn new(pcm: Vec<Smp>, start_frame: i64) -> Self {
        BufferSource::from_arc(pcm.into(), start_frame)
    }

    /// Build from shared PCM (no copy) — lets several graph rebuilds share one decode.
    pub fn from_arc(pcm: Arc<[Smp]>, start_frame: i64) -> Self {
        let len_frames = (pcm.len() / CHANNELS) as i64;
        BufferSource {
            pcm,
            start_frame,
            len_frames,
        }
    }

    /// Build from a mono buffer by duplicating to both channels.
    pub fn from_mono(mono: &[Smp], start_frame: i64) -> Self {
        let mut pcm = vec![0.0; mono.len() * CHANNELS];
        for (i, &s) in mono.iter().enumerate() {
            pcm[CHANNELS * i] = s;
            pcm[CHANNELS * i + 1] = s;
        }
        BufferSource::new(pcm, start_frame)
    }

    pub fn len_frames(&self) -> i64 {
        self.len_frames
    }
}

impl Node for BufferSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let out = &mut *io.outputs[0];
        for i in 0..io.frames {
            let local = io.tl + i as i64 - self.start_frame;
            if local >= 0 && local < self.len_frames {
                let p = local as usize * CHANNELS;
                out[CHANNELS * i] = self.pcm[p];
                out[CHANNELS * i + 1] = self.pcm[p + 1];
            } else {
                out[CHANNELS * i] = 0.0;
                out[CHANNELS * i + 1] = 0.0;
            }
        }
    }
}

/// The live streaming source: pops decoded interleaved-stereo frames from an SPSC ring fed
/// by a decoder/feeder thread. On underrun it emits silence and bumps a counter (a 1-2 ms
/// fade at the discontinuity would be added for production; the counter is the health
/// signal the UI reads). Sequential streaming — timeline placement/seek are handled by the
/// feeder refilling the ring from the right file offset.
pub struct RingSource {
    rx: Consumer<[Smp; CHANNELS]>,
    xrun: u64,
}

impl RingSource {
    pub fn new(rx: Consumer<[Smp; CHANNELS]>) -> Self {
        RingSource { rx, xrun: 0 }
    }

    /// Underruns observed so far (ring empty when audio was needed).
    pub fn xruns(&self) -> u64 {
        self.xrun
    }
}

impl Node for RingSource {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn num_in(&self) -> usize {
        0
    }

    fn process(&mut self, io: &mut ProcessIo) {
        let out = &mut *io.outputs[0];
        for i in 0..io.frames {
            match self.rx.pop() {
                Some(f) => {
                    out[CHANNELS * i] = f[0];
                    out[CHANNELS * i + 1] = f[1];
                }
                None => {
                    out[CHANNELS * i] = 0.0;
                    out[CHANNELS * i + 1] = 0.0;
                    self.xrun = self.xrun.wrapping_add(1);
                }
            }
        }
    }
}
