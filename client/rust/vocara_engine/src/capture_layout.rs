//! Pure, unit-testable buffer-layout logic for the VPIO mic element (`vpio.rs`).
//!
//! VPIO negotiates its own client stream format on initialize, and the mic element can come back
//! NON-INTERLEAVED (`kAudioFormatFlagIsNonInterleaved`): then `AudioUnitRender` wants an
//! `AudioBufferList` with one single-channel buffer per channel, and rejects the one interleaved
//! buffer the input callback used to hand it every time — a mic that renders nothing, flagged
//! lost after the failure streak. [`CaptureLayout`] says which list to build; [`downmix_to_mono`]
//! turns whatever the render filled into the mono stream the AEC consumes. Both are RT-safe (no
//! alloc / lock / syscall; bounded work).

/// Most channels the mic path provisions buffers for (mic captures are mono or stereo).
pub const MAX_CAPTURE_CH: usize = 2;

/// How the mic element lays its frames out in the `AudioBufferList` it renders into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureLayout {
    /// Channels per frame (clamped to 1..=MAX_CAPTURE_CH).
    pub channels: usize,
    /// One buffer holding interleaved frames, or one buffer per channel.
    pub interleaved: bool,
}

impl CaptureLayout {
    /// From the negotiated format: `mChannelsPerFrame` and whether `mFormatFlags` carries
    /// `kAudioFormatFlagIsNonInterleaved`. Mono is the same list either way.
    pub fn new(channels_per_frame: usize, non_interleaved: bool) -> CaptureLayout {
        let channels = channels_per_frame.clamp(1, MAX_CAPTURE_CH);
        CaptureLayout {
            channels,
            interleaved: !non_interleaved || channels == 1,
        }
    }

    /// Buffers the `AudioBufferList` must carry.
    pub fn buffers(&self) -> usize {
        if self.interleaved {
            1
        } else {
            self.channels
        }
    }

    /// Channels in each of those buffers (`mNumberChannels`).
    pub fn channels_per_buffer(&self) -> usize {
        if self.interleaved {
            self.channels
        } else {
            1
        }
    }
}

/// Downmix one render's capture to mono into `out`; returns the frames written (at most `frames`
/// and `out.len()`). `bufs` are the list's buffers in order, each cut to the samples its
/// `mDataByteSize` reports.
///
/// * Interleaved: the channel count is read off what came back, `samples / frames`, not the
///   format: VPIO often fills a buffer it described as stereo with mono, and reading mono samples
///   as L/R pairs halves the frame count (the monitored voice an octave up).
/// * Non-interleaved: frame `i` of every buffer that delivered anything is averaged; a channel
///   left empty is skipped rather than averaged in as silence.
pub fn downmix_to_mono(
    layout: CaptureLayout,
    bufs: &[&[f32]],
    frames: usize,
    out: &mut [f32],
) -> usize {
    let frames = frames.min(out.len());
    if frames == 0 {
        return 0;
    }
    if layout.interleaved {
        let Some(buf) = bufs.first() else {
            return 0;
        };
        let ch = (buf.len() / frames).clamp(1, MAX_CAPTURE_CH);
        let n = (buf.len() / ch).min(frames);
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = if ch == 1 {
                buf[i]
            } else {
                0.5 * (buf[i * ch] + buf[i * ch + 1])
            };
        }
        return n;
    }
    let mut live = 0usize;
    let mut n = frames;
    for buf in bufs.iter().take(MAX_CAPTURE_CH).filter(|b| !b.is_empty()) {
        live += 1;
        n = n.min(buf.len());
    }
    if live == 0 {
        return 0;
    }
    let scale = 1.0 / live as f32;
    for (i, o) in out[..n].iter_mut().enumerate() {
        let mut s = 0.0;
        for buf in bufs.iter().take(MAX_CAPTURE_CH).filter(|b| !b.is_empty()) {
            s += buf[i];
        }
        *o = s * scale;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::{downmix_to_mono, CaptureLayout};

    const FRAMES: usize = 8;

    fn left(i: usize) -> f32 {
        i as f32
    }

    fn right(i: usize) -> f32 {
        100.0 + i as f32
    }

    /// The negotiated flags decide the list: non-interleaved stereo needs two single-channel
    /// buffers (the callback used to offer one two-channel buffer, which the render rejects).
    #[test]
    fn the_negotiated_format_decides_the_buffer_list() {
        let planar = CaptureLayout::new(2, true);
        assert_eq!((planar.buffers(), planar.channels_per_buffer()), (2, 1));
        let packed = CaptureLayout::new(2, false);
        assert_eq!((packed.buffers(), packed.channels_per_buffer()), (1, 2));
        // Mono is one single-channel buffer whichever way the flag reads.
        let mono = CaptureLayout::new(1, true);
        assert_eq!((mono.buffers(), mono.channels_per_buffer()), (1, 1));
        // A wider device is clamped to what the path provisions.
        assert_eq!(CaptureLayout::new(4, true).buffers(), 2);
    }

    /// Non-interleaved stereo: L and R arrive in separate buffers and every frame is their mean.
    /// Read as one interleaved buffer instead, buffer 0's consecutive LEFT samples would pair up
    /// as L/R — half the frames, each averaging two different instants, and no right channel.
    #[test]
    fn non_interleaved_stereo_averages_the_two_planes() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES).map(right).collect();
        let mut out = [f32::NAN; FRAMES];
        let n = downmix_to_mono(CaptureLayout::new(2, true), &[&l, &r], FRAMES, &mut out);
        assert_eq!(n, FRAMES);
        for (i, &o) in out.iter().enumerate() {
            assert_eq!(o, 0.5 * (left(i) + right(i)), "frame {i}");
        }
    }

    /// A short render: the frame count is what the shortest delivering plane holds.
    #[test]
    fn a_short_plane_bounds_the_frames() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES - 3).map(right).collect();
        let mut out = [0.0f32; FRAMES];
        let n = downmix_to_mono(CaptureLayout::new(2, true), &[&l, &r], FRAMES, &mut out);
        assert_eq!(n, FRAMES - 3);
        assert_eq!(out[n - 1], 0.5 * (left(n - 1) + right(n - 1)));
    }

    /// A plane the render left empty is skipped, not averaged in as silence (which would drop the
    /// captured voice by 6 dB), and nothing delivered means nothing written.
    #[test]
    fn an_empty_plane_is_skipped() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let mut out = [0.0f32; FRAMES];
        let n = downmix_to_mono(CaptureLayout::new(2, true), &[&l, &[]], FRAMES, &mut out);
        assert_eq!(n, FRAMES);
        assert_eq!(out[5], left(5));
        assert_eq!(
            downmix_to_mono(CaptureLayout::new(2, true), &[&[], &[]], FRAMES, &mut out),
            0
        );
    }

    /// Interleaved stereo averages each L/R pair; a buffer described as stereo that came back
    /// holding one sample per frame is mono, read sample by sample.
    #[test]
    fn interleaved_reads_its_channel_count_off_the_delivered_size() {
        let lr: Vec<f32> = (0..FRAMES).flat_map(|i| [left(i), right(i)]).collect();
        let mut out = [0.0f32; FRAMES];
        let packed = CaptureLayout::new(2, false);
        assert_eq!(downmix_to_mono(packed, &[&lr], FRAMES, &mut out), FRAMES);
        assert_eq!(out[3], 0.5 * (left(3) + right(3)));

        let mono: Vec<f32> = (0..FRAMES).map(left).collect();
        assert_eq!(downmix_to_mono(packed, &[&mono], FRAMES, &mut out), FRAMES);
        assert_eq!(out[6], left(6));
    }

    /// `out` bounds the write even when the render claims more.
    #[test]
    fn the_output_slice_bounds_the_frames() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES).map(right).collect();
        let mut out = [0.0f32; 4];
        let n = downmix_to_mono(CaptureLayout::new(2, true), &[&l, &r], FRAMES, &mut out);
        assert_eq!(n, 4);
    }
}
