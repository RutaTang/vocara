//! Pure, unit-testable buffer-layout logic for the VPIO mic element (`vpio.rs`).
//!
//! VPIO negotiates its own client stream format on initialize, and the mic element can come back
//! NON-INTERLEAVED (`kAudioFormatFlagIsNonInterleaved`): then `AudioUnitRender` wants an
//! `AudioBufferList` with one single-channel buffer per channel, and rejects the one interleaved
//! buffer the input callback used to hand it every time — a mic that renders nothing, flagged
//! lost after the failure streak. [`CaptureLayout`] says which list to build; [`downmix_to_mono`]
//! turns whatever the render filled into the mono stream the AEC consumes, through the
//! [`InputChannelMix`] that also serves the cpal input path (`device::InputCell`), so a
//! multi-channel mic is heard the same way on both; [`DeadCaptureStreak`] decides when cycles
//! that captured nothing mean the mic is gone. All are RT-safe (no alloc / lock / syscall;
//! bounded work).

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

/// Mix one render's capture down to the mono mic signal into `out`; returns the frames written
/// (at most `frames` and `out.len()`). `bufs` are the list's buffers in order, each cut to the
/// samples its `mDataByteSize` reports. Which channels make the mono, and how, is `mix`'s job
/// under `mode` (see [`InputChannelMix`]); this only reads the channels each frame delivered.
///
/// * Interleaved: the channel count is read off what came back, `samples / frames`, not the
///   format: VPIO often fills a buffer it described as stereo with mono, and reading mono samples
///   as L/R pairs halves the frame count (the monitored voice an octave up).
/// * Non-interleaved: the buffers that delivered anything are the channels, in order; a channel
///   left empty is skipped rather than read as silence.
pub fn downmix_to_mono(
    layout: CaptureLayout,
    bufs: &[&[f32]],
    frames: usize,
    out: &mut [f32],
    mix: &mut InputChannelMix,
    mode: u32,
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
            *o = mix.mix(mode, &buf[i * ch..i * ch + ch]);
        }
        return n;
    }
    let mut planes: [&[f32]; MAX_CAPTURE_CH] = [&[]; MAX_CAPTURE_CH];
    let mut live = 0usize;
    let mut n = frames;
    for buf in bufs.iter().take(MAX_CAPTURE_CH).filter(|b| !b.is_empty()) {
        planes[live] = buf;
        live += 1;
        n = n.min(buf.len());
    }
    if live == 0 {
        return 0;
    }
    let mut frame = [0.0f32; MAX_CAPTURE_CH];
    for (i, o) in out[..n].iter_mut().enumerate() {
        for (c, plane) in planes[..live].iter().enumerate() {
            frame[c] = plane[i];
        }
        *o = mix.mix(mode, &frame[..live]);
    }
    n
}

/// `ve_set_input_channel`: the mono mic signal is a level-weighted mix of the device's inputs
/// (see [`InputChannelMix`]).
pub const INPUT_CHANNEL_AUTO: u32 = 0;
/// `ve_set_input_channel`: the mono mic signal is input 1 alone.
pub const INPUT_CHANNEL_1: u32 = 1;
/// `ve_set_input_channel`: the mono mic signal is input 2 alone (input 1 on a device without one).
pub const INPUT_CHANNEL_2: u32 = 2;

/// Rise time constant of the per-channel level the Auto weights come from (s).
const LEVEL_RISE_S: f64 = 0.15;
/// Fall time constant of that level (s). Longer than the rise so a breath or an instrumental
/// break does not hand the weight back to an input idling at its noise floor: the next phrase
/// would then start at the even split's -6 dB and glide up.
const LEVEL_FALL_S: f64 = 1.0;
/// Time constant the weights glide toward their target with (s): short enough that a voice
/// arriving on an input is at full level within tens of milliseconds, long enough that no sample
/// moves further than the voice's own slope (the target can jump within a few samples when the
/// other input sits at its noise floor), and that a change of choice is a glide, not a click.
const WEIGHT_GLIDE_S: f64 = 0.01;
/// Bias added to every level so it never decays into subnormals and the Auto split is always
/// defined (digital silence on every input splits evenly). Far below any real noise floor.
const LEVEL_FLOOR: f32 = 1.0e-18;

/// Turns one frame of a mic's channels into the ONE mono signal everything downstream hears — the
/// monitor, the AEC, the meter and the take — under the input-channel mode.
///
/// * Input 1 / Input 2: that channel alone (Input 2 on a device without one is Input 1).
/// * Auto: each channel weighted by its share of the summed level (mean square, smoothed — see
///   [`LEVEL_RISE_S`] / [`LEVEL_FALL_S`]). An input idling at its noise floor holds a share of
///   about a millionth next to a sung voice, so a mic on either input records at full level; two
///   live inputs blend in proportion to their levels, and equal ones average. Only the first
///   [`MAX_CAPTURE_CH`] channels of a wider device are weighed.
///
/// The weights glide toward their target ([`WEIGHT_GLIDE_S`]) and always sum to 1. A 1-channel
/// device passes through untouched. RT-safe: no alloc / lock / syscall, a few multiply-adds per
/// channel per frame.
pub struct InputChannelMix {
    /// Per-sample one-pole coefficients for the level's rise and fall, and the weights' glide.
    rise: f32,
    fall: f32,
    glide: f32,
    /// Smoothed mean-square level per channel (the Auto weights' basis), tracked in every mode so
    /// a switch to Auto starts from a settled estimate.
    level: [f32; MAX_CAPTURE_CH],
    /// The gain each channel enters the mono with.
    weight: [f32; MAX_CAPTURE_CH],
    /// Whether a frame has been mixed: an explicit choice applies from the first frame instead of
    /// gliding in from the even split the weights start at.
    primed: bool,
}

impl InputChannelMix {
    /// A mixer for a mic element running at `rate` Hz.
    pub fn new(rate: f64) -> InputChannelMix {
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            48_000.0
        };
        let coef = |secs: f64| (1.0 - (-1.0 / (secs * rate)).exp()) as f32;
        InputChannelMix {
            rise: coef(LEVEL_RISE_S),
            fall: coef(LEVEL_FALL_S),
            glide: coef(WEIGHT_GLIDE_S),
            level: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            weight: [1.0 / MAX_CAPTURE_CH as f32; MAX_CAPTURE_CH],
            primed: false,
        }
    }

    /// One frame's channel samples (`frame`, the device's channels in order) to the mono signal
    /// under `mode`; an unknown mode is Auto.
    pub fn mix(&mut self, mode: u32, frame: &[f32]) -> f32 {
        let ch = frame.len().min(MAX_CAPTURE_CH);
        if ch <= 1 {
            return frame.first().copied().unwrap_or(0.0);
        }
        let mut total = 0.0f32;
        for (level, &x) in self.level[..ch].iter_mut().zip(frame) {
            let p = x * x + LEVEL_FLOOR;
            let a = if p > *level { self.rise } else { self.fall };
            *level += a * (p - *level);
            total += *level;
        }
        let mut target = [0.0f32; MAX_CAPTURE_CH];
        match mode {
            INPUT_CHANNEL_1 => target[0] = 1.0,
            INPUT_CHANNEL_2 => target[1] = 1.0,
            _ => {
                for (t, &level) in target[..ch].iter_mut().zip(&self.level) {
                    *t = level / total;
                }
            }
        }
        if !self.primed {
            self.primed = true;
            if matches!(mode, INPUT_CHANNEL_1 | INPUT_CHANNEL_2) {
                self.weight = target;
            }
        }
        let mut y = 0.0f32;
        for (c, (w, t)) in self.weight.iter_mut().zip(target).enumerate() {
            *w += self.glide * (t - *w);
            if c < ch {
                y += *w * frame[c];
            }
        }
        y
    }
}

/// Consecutive dead mic cycles (a failed `AudioUnitRender`, or a success that delivered nothing)
/// toward an input-lost verdict. The VPIO unit has no error callback, so a sustained streak is the
/// only way a mid-session disconnect reaches the host's device-event poll.
///
/// Two allowances, because a unit that has not captured its first frame yet is not dead in the
/// same way. Before that it is warming up — a Bluetooth headset switching to its headset profile
/// can fail or come back empty for a second or two — and the ~200 ms streak rule reported a loss
/// that was still latched when the take being armed started recording from the now-delivering
/// mic, whose first poll stopped it. So the warm-up gets its own, generous allowance measured in
/// FRAMES (cycle sizes vary with the device): a unit, a reopened one included, that has delivered
/// nothing by then is reported lost once. After the first capture, `limit` dead cycles in a row
/// are a loss.
pub struct DeadCaptureStreak {
    /// Dead cycles in a row that make a delivering mic lost.
    limit: u32,
    streak: u32,
    /// Dead frames a unit may spend before its first capture (see the type doc).
    warmup_frames: u64,
    /// Dead frames spent so far before the first capture.
    warmup: u64,
    /// Whether the unit has captured anything yet.
    delivered: bool,
}

impl DeadCaptureStreak {
    pub const fn new(limit: u32, warmup_frames: u64) -> DeadCaptureStreak {
        DeadCaptureStreak {
            limit,
            streak: 0,
            warmup_frames,
            warmup: 0,
            delivered: false,
        }
    }

    /// A cycle that captured frames: the unit is alive and the streak starts over.
    pub fn on_capture(&mut self) {
        self.delivered = true;
        self.streak = 0;
    }

    /// A cycle of `frames` that captured nothing. `true` exactly once per streak (or once for the
    /// warm-up), on the cycle that crosses its allowance — the caller raises the loss then, not
    /// on every cycle after.
    pub fn on_dead(&mut self, frames: usize) -> bool {
        if !self.delivered {
            let before = self.warmup;
            self.warmup = before.saturating_add(frames as u64);
            return before < self.warmup_frames && self.warmup >= self.warmup_frames;
        }
        self.streak = self.streak.saturating_add(1);
        self.streak == self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::{
        downmix_to_mono, CaptureLayout, DeadCaptureStreak, InputChannelMix, INPUT_CHANNEL_1,
        INPUT_CHANNEL_2, INPUT_CHANNEL_AUTO,
    };

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

    /// `downmix_to_mono` with a fresh mixer at 48 kHz.
    fn downmix(layout: CaptureLayout, bufs: &[&[f32]], out: &mut [f32], mode: u32) -> usize {
        let mut mix = InputChannelMix::new(48_000.0);
        downmix_to_mono(layout, bufs, FRAMES, out, &mut mix, mode)
    }

    /// Non-interleaved stereo: L and R arrive in separate buffers, plane 1 being input 1. Read as
    /// one interleaved buffer instead, buffer 0's consecutive LEFT samples would pair up as L/R —
    /// half the frames, each mixing two different instants, and no right channel.
    #[test]
    fn non_interleaved_stereo_reads_the_two_planes() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES).map(right).collect();
        let planar = CaptureLayout::new(2, true);
        let mut out = [f32::NAN; FRAMES];
        assert_eq!(
            downmix(planar, &[&l, &r], &mut out, INPUT_CHANNEL_1),
            FRAMES
        );
        for (i, &o) in out.iter().enumerate() {
            assert_eq!(o, left(i), "input 1, frame {i}");
        }
        assert_eq!(
            downmix(planar, &[&l, &r], &mut out, INPUT_CHANNEL_2),
            FRAMES
        );
        for (i, &o) in out.iter().enumerate() {
            assert_eq!(o, right(i), "input 2, frame {i}");
        }
    }

    /// A short render: the frame count is what the shortest delivering plane holds.
    #[test]
    fn a_short_plane_bounds_the_frames() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES - 3).map(right).collect();
        let mut out = [0.0f32; FRAMES];
        let n = downmix(
            CaptureLayout::new(2, true),
            &[&l, &r],
            &mut out,
            INPUT_CHANNEL_2,
        );
        assert_eq!(n, FRAMES - 3);
        assert_eq!(out[n - 1], right(n - 1));
    }

    /// A plane the render left empty is skipped, not mixed in as silence (which would drop the
    /// captured voice by 6 dB), and nothing delivered means nothing written.
    #[test]
    fn an_empty_plane_is_skipped() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let mut out = [0.0f32; FRAMES];
        let planar = CaptureLayout::new(2, true);
        let n = downmix(planar, &[&l, &[]], &mut out, INPUT_CHANNEL_AUTO);
        assert_eq!(n, FRAMES);
        assert_eq!(out[5], left(5));
        assert_eq!(
            downmix(planar, &[&[], &[]], &mut out, INPUT_CHANNEL_AUTO),
            0
        );
    }

    /// Interleaved stereo reads each L/R pair as inputs 1 and 2; a buffer described as stereo that
    /// came back holding one sample per frame is mono, read sample by sample.
    #[test]
    fn interleaved_reads_its_channel_count_off_the_delivered_size() {
        let lr: Vec<f32> = (0..FRAMES).flat_map(|i| [left(i), right(i)]).collect();
        let mut out = [0.0f32; FRAMES];
        let packed = CaptureLayout::new(2, false);
        assert_eq!(downmix(packed, &[&lr], &mut out, INPUT_CHANNEL_2), FRAMES);
        assert_eq!(out[3], right(3));

        let mono: Vec<f32> = (0..FRAMES).map(left).collect();
        assert_eq!(downmix(packed, &[&mono], &mut out, INPUT_CHANNEL_2), FRAMES);
        assert_eq!(out[6], left(6));
    }

    /// `out` bounds the write even when the render claims more.
    #[test]
    fn the_output_slice_bounds_the_frames() {
        let l: Vec<f32> = (0..FRAMES).map(left).collect();
        let r: Vec<f32> = (0..FRAMES).map(right).collect();
        let mut out = [0.0f32; 4];
        let mut mix = InputChannelMix::new(48_000.0);
        let planar = CaptureLayout::new(2, true);
        let n = downmix_to_mono(
            planar,
            &[&l, &r],
            FRAMES,
            &mut out,
            &mut mix,
            INPUT_CHANNEL_1,
        );
        assert_eq!(n, 4);
    }

    // ---- the input-channel mix -------------------------------------------------------------

    const SR: f64 = 48_000.0;

    /// A sung-level tone (RMS -20 dBFS) at `hz`, sample `n`.
    fn tone(hz: f64, n: usize) -> f32 {
        (0.1 * std::f64::consts::SQRT_2 * (std::f64::consts::TAU * hz * n as f64 / SR).sin()) as f32
    }

    /// A deterministic noise floor at about -90 dBFS RMS: an input with nothing plugged in.
    struct Floor(u32);
    impl Floor {
        fn next(&mut self) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            ((x as f32 / u32::MAX as f32) * 2.0 - 1.0) * 5.5e-5
        }
    }

    fn rms(s: &[f32]) -> f64 {
        (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
    }

    /// Auto: a mic on either input of a 2-input interface records at full level — the idle input
    /// at its noise floor gets essentially no weight. The averaging downmix put such a mic 6 dB
    /// down with the idle input's noise mixed in, and the cpal path kept input 1 alone (silence
    /// for a mic on input 2).
    #[test]
    fn auto_takes_a_mic_on_either_input_at_full_level() {
        for live in 0..2 {
            let mut mix = InputChannelMix::new(SR);
            let mut floor = Floor(7);
            let (mut voice, mut out) = (Vec::new(), Vec::new());
            for n in 0..SR as usize {
                let v = tone(220.0, n);
                let mut frame = [floor.next(), floor.next()];
                frame[live] = v;
                let y = mix.mix(INPUT_CHANNEL_AUTO, &frame);
                if n >= SR as usize / 2 {
                    voice.push(v);
                    out.push(y);
                }
            }
            let db = 20.0 * (rms(&out) / rms(&voice)).log10();
            assert!(db.abs() <= 0.5, "mic on input {}: {db:+.2} dB", live + 1);
        }
    }

    /// Auto: equal channels give their average — the same signal on both inputs comes through
    /// as itself, and two different signals at the same level blend half and half.
    #[test]
    fn auto_gives_equal_channels_their_average() {
        let mut mix = InputChannelMix::new(SR);
        for n in 0..SR as usize / 4 {
            let v = tone(220.0, n);
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[v, v]);
            assert!((y - v).abs() <= 1e-6, "frame {n}: {y} vs {v}");
        }
        let mut mix = InputChannelMix::new(SR);
        let (mut err, mut avg) = (Vec::new(), Vec::new());
        for n in 0..SR as usize {
            let (a, b) = (tone(220.0, n), tone(330.0, n));
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[a, b]);
            if n >= SR as usize / 2 {
                err.push(y - 0.5 * (a + b));
                avg.push(0.5 * (a + b));
            }
        }
        let off = 20.0 * (rms(&err) / rms(&avg)).log10();
        assert!(off <= -26.0, "the blend is off the average by {off:.1} dB");
    }

    /// Auto: a voice starting on input 2 after a long silence (both inputs at their noise floor,
    /// so the weights sit at an even split) glides to full level without a step — no sample moves
    /// further than the voice's own steepest slope — and gets there within 100 ms.
    #[test]
    fn a_voice_starting_on_input_2_glides_in_without_a_step() {
        for phase in [0.0, 0.25] {
            let mut mix = InputChannelMix::new(SR);
            let mut floor = Floor(11);
            let onset = SR as usize;
            let (mut slope, mut step) = (0.0f32, 0.0f32);
            let (mut prev_x, mut prev_y) = (0.0f32, 0.0f32);
            let (mut voice, mut out) = (Vec::new(), Vec::new());
            for n in 0..(onset + SR as usize / 2) {
                let v = if n >= onset {
                    // A tone starting at `phase` of its cycle (0.25: at its crest, a hard onset).
                    let k = n - onset + (phase * SR / 220.0) as usize;
                    tone(220.0, k)
                } else {
                    0.0
                };
                let x = v + floor.next();
                let y = mix.mix(INPUT_CHANNEL_AUTO, &[floor.next(), x]);
                if n > onset - 10 {
                    slope = slope.max((x - prev_x).abs());
                    step = step.max((y - prev_y).abs());
                }
                if n >= onset + SR as usize / 10 {
                    voice.push(v);
                    out.push(y);
                }
                (prev_x, prev_y) = (x, y);
            }
            assert!(
                step <= slope * 1.0001,
                "phase {phase}: the output moved {step} in a sample, the voice at most {slope}"
            );
            let db = 20.0 * (rms(&out) / rms(&voice)).log10();
            assert!(db.abs() <= 0.5, "phase {phase}: {db:+.2} dB 100 ms in");
        }
    }

    /// Input 1 and Input 2 pick their channel from the first frame, whatever the other carries.
    #[test]
    fn explicit_inputs_pick_their_channel() {
        let mut one = InputChannelMix::new(SR);
        let mut two = InputChannelMix::new(SR);
        for n in 0..1000 {
            let (a, b) = (tone(220.0, n), 0.5 * tone(330.0, n));
            assert_eq!(one.mix(INPUT_CHANNEL_1, &[a, b]), a, "input 1, frame {n}");
            assert_eq!(two.mix(INPUT_CHANNEL_2, &[a, b]), b, "input 2, frame {n}");
        }
        // A wider device: Input 2 is still the second channel.
        let mut wide = InputChannelMix::new(SR);
        assert_eq!(wide.mix(INPUT_CHANNEL_2, &[0.1, 0.2, 0.3, 0.4]), 0.2);
    }

    /// A 1-channel device is untouched in every mode; Input 2 there means input 1.
    #[test]
    fn a_mono_device_is_untouched() {
        for mode in [INPUT_CHANNEL_AUTO, INPUT_CHANNEL_1, INPUT_CHANNEL_2] {
            let mut mix = InputChannelMix::new(SR);
            for n in 0..1000 {
                let v = tone(220.0, n);
                assert_eq!(mix.mix(mode, &[v]), v, "mode {mode}, frame {n}");
            }
        }
    }

    /// Changing the choice mid-stream glides from one input to the other instead of stepping
    /// between their levels (a click in the monitor and the take).
    #[test]
    fn switching_inputs_glides_instead_of_stepping() {
        let mut mix = InputChannelMix::new(SR);
        let mut prev = mix.mix(INPUT_CHANNEL_1, &[0.5, -0.5]);
        assert_eq!(prev, 0.5);
        let mut biggest = 0.0f32;
        for _ in 0..SR as usize / 10 {
            let y = mix.mix(INPUT_CHANNEL_2, &[0.5, -0.5]);
            biggest = biggest.max((y - prev).abs());
            prev = y;
        }
        assert!(biggest < 0.01, "a {biggest} step on the switch");
        assert!((prev + 0.5).abs() < 1e-3, "settled at {prev}, not input 2");
    }

    const LIMIT: u32 = 20;
    /// A 512-frame cycle, and a warm-up allowance of 250 of them (2.67 s at 48 kHz).
    const CYCLE: usize = 512;
    const WARMUP: u64 = 250 * CYCLE as u64;

    /// A unit that has not captured its first frame is warming up: counted by the streak rule,
    /// the warm-up latched EVENT_INPUT_LOST under the take being armed, and the take's first poll
    /// then stopped it once the mic was delivering. Inside its allowance it is not lost.
    #[test]
    fn a_unit_warming_up_is_not_lost_within_its_allowance() {
        let mut s = DeadCaptureStreak::new(LIMIT, WARMUP);
        for cycle in 1..250 {
            assert!(
                !s.on_dead(CYCLE),
                "warm-up cycle {cycle} reported the mic lost"
            );
        }
        // Delivering now, and later dying: the ordinary streak rule applies.
        s.on_capture();
        for _ in 1..LIMIT {
            assert!(!s.on_dead(CYCLE));
        }
        assert!(s.on_dead(CYCLE));
    }

    /// A unit — a reopened one included — that never delivers is reported lost once its warm-up
    /// allowance has passed, and only once: before, nothing ever reported it, and the technique
    /// page, which has no frame-counter stall watch, never learned the mic was dead.
    #[test]
    fn a_unit_that_never_delivers_is_lost_once_its_warm_up_has_passed() {
        let mut s = DeadCaptureStreak::new(LIMIT, WARMUP);
        let reports: Vec<usize> = (1..=1000).filter(|_| s.on_dead(CYCLE)).collect();
        assert_eq!(
            reports,
            vec![250],
            "lost exactly once, on the cycle that crosses the allowance"
        );
    }

    /// Once the unit has delivered, the limit-th dead cycle in a row reports the loss, once.
    #[test]
    fn a_streak_after_the_first_capture_reports_the_loss_once() {
        let mut s = DeadCaptureStreak::new(LIMIT, WARMUP);
        s.on_capture();
        for cycle in 1..LIMIT {
            assert!(!s.on_dead(CYCLE), "cycle {cycle} is under the limit");
        }
        assert!(s.on_dead(CYCLE), "the limit-th dead cycle reports the loss");
        for _ in 0..LIMIT {
            assert!(!s.on_dead(CYCLE), "a loss is reported once per streak");
        }
    }

    /// A capture inside a streak starts it over: transient failures never add up to a loss.
    #[test]
    fn a_capture_starts_the_streak_over() {
        let mut s = DeadCaptureStreak::new(LIMIT, WARMUP);
        s.on_capture();
        for _ in 0..5 {
            for _ in 1..LIMIT {
                assert!(!s.on_dead(CYCLE));
            }
            s.on_capture();
        }
    }
}
