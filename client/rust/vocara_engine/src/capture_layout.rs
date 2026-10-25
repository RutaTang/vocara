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

/// `ve_set_input_channel`: the mono mic signal is the input carrying the mic, or an even blend of
/// comparable live inputs (see [`InputChannelMix`]).
pub const INPUT_CHANNEL_AUTO: u32 = 0;
/// `ve_set_input_channel`: the mono mic signal is input 1 alone.
pub const INPUT_CHANNEL_1: u32 = 1;
/// `ve_set_input_channel`: the mono mic signal is input 2 alone (input 1 on a device without one).
pub const INPUT_CHANNEL_2: u32 = 2;

/// Time constant of the per-channel level Auto decides on (s): short, so a burst on the other
/// input is out of the decision within tens of milliseconds of ending.
const LEVEL_TAU_S: f64 = 0.01;
/// Rise time constant of each channel's noise-floor estimate (s); it falls at once. A level has to
/// stand [`LIVE_OVER_FLOOR`] above its floor to count as live, so a steady hum or hiss on an
/// empty input is never taken for a mic.
const FLOOR_RISE_S: f64 = 20.0;
/// How long after the mixer starts the floor follows the level both ways (s): a hum present from
/// the first frame is the floor from the start, not a live signal for the seconds the slow rise
/// would take to reach it.
const FLOOR_WARMUP_S: f64 = 0.1;
/// A live channel stands this far above its floor (power ratio: 10 dB)...
const LIVE_OVER_FLOOR: f32 = 10.0;
/// ...and above this level (mean square: -60 dBFS).
const LIVE_MIN: f32 = 1.0e-6;
/// A live channel this far above every other live one (power ratio: 12 dB) is THE input; closer,
/// both are in use, and Auto blends them evenly.
const DOMINANCE: f32 = 15.85;
/// How long a new pick must hold before Auto takes it while the input it has picked carries
/// nothing (s): a mic moved to the other input is followed this soon...
const SWITCH_SOON_S: f64 = 0.15;
/// ...and while the picked input is still live (s): a pop, a bumped mic or a knocked guitar on
/// the other input is over before this, and never reaches the take.
const SWITCH_LATE_S: f64 = 0.4;
/// Time constant the weights glide toward their target with (s): short enough that a voice
/// arriving on an input is at full level within tens of milliseconds, long enough that a change
/// of input — Auto's or the user's — is a glide, not a click.
const WEIGHT_GLIDE_S: f64 = 0.01;
/// Bias added to every level so it never decays into subnormals. Far below any real noise floor.
const LEVEL_FLOOR: f32 = 1.0e-18;

/// What Auto takes the mono from: the device's inputs blended evenly, or one of them alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Even,
    One(usize),
}

/// Turns one frame of a mic's channels into the ONE mono signal everything downstream hears — the
/// monitor, the AEC, the meter and the take — under the input-channel mode.
///
/// * Input 1 / Input 2: that channel alone (Input 2 on a device without one is Input 1).
/// * Auto: the input carrying the mic. An input is live while its level (mean square over
///   [`LEVEL_TAU_S`]) stands above [`LIVE_MIN`] and [`LIVE_OVER_FLOOR`] above its own noise floor.
///   One live input, or one [`DOMINANCE`] above the others, is picked alone, so a mic on either
///   input of a 2-input interface records at full level with the idle input's noise left out.
///   Comparable live inputs (a stereo mic, two sources) blend evenly. Nothing live (a pause) keeps
///   the pick. A new pick is taken only once it has held [`SWITCH_LATE_S`] while the picked input
///   is still live — a pop, a thump or a knocked guitar on the other input never reaches the take
///   or ducks the voice — or [`SWITCH_SOON_S`] while it carries nothing, the inputs blending evenly
///   meanwhile so a mic moved to the other input is never cut. A blend following each input's
///   share of the level instead ducked the voice by 12 dB for seconds after a pop on the idle
///   input. The price: a second source joining a live one is left out for [`SWITCH_LATE_S`] —
///   Input 1 / Input 2 are for two sources. Only the first [`MAX_CAPTURE_CH`] channels of a wider
///   device are weighed.
///
/// A non-finite sample is silence. The weights glide toward their target ([`WEIGHT_GLIDE_S`]) and
/// always sum to 1. A 1-channel device passes through untouched. RT-safe: no alloc / lock /
/// syscall, a few multiply-adds per channel per frame.
pub struct InputChannelMix {
    /// Per-sample one-pole coefficients: the level, the floor's rise, the weights' glide.
    level_coef: f32,
    floor_coef: f32,
    glide: f32,
    /// [`FLOOR_WARMUP_S`], [`SWITCH_SOON_S`] and [`SWITCH_LATE_S`] in frames; `warmup` counts down.
    warmup: u32,
    soon: u32,
    late: u32,
    /// Smoothed mean-square level and noise floor per channel, tracked in every mode so a switch to
    /// Auto starts from a settled estimate.
    level: [f32; MAX_CAPTURE_CH],
    floor: [f32; MAX_CAPTURE_CH],
    /// Auto's pick, the pick it is considering instead, and for how many frames it has held.
    picked: Pick,
    pending: Pick,
    pending_frames: u32,
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
        let frames = |secs: f64| (secs * rate).round() as u32;
        InputChannelMix {
            level_coef: coef(LEVEL_TAU_S),
            floor_coef: coef(FLOOR_RISE_S),
            glide: coef(WEIGHT_GLIDE_S),
            warmup: frames(FLOOR_WARMUP_S),
            soon: frames(SWITCH_SOON_S),
            late: frames(SWITCH_LATE_S),
            level: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            floor: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            picked: Pick::Even,
            pending: Pick::Even,
            pending_frames: 0,
            weight: [1.0 / MAX_CAPTURE_CH as f32; MAX_CAPTURE_CH],
            primed: false,
        }
    }

    /// One frame's channel samples (`frame`, the device's channels in order) to the mono signal
    /// under `mode`; an unknown mode is Auto.
    pub fn mix(&mut self, mode: u32, frame: &[f32]) -> f32 {
        let ch = frame.len().min(MAX_CAPTURE_CH);
        // A driver or virtual device passing garbage through: in the recursive level a NaN stayed
        // NaN for good, and so did every later output — a silent mic until the stream reopened.
        let mut x = [0.0f32; MAX_CAPTURE_CH];
        for (x, &s) in x.iter_mut().zip(&frame[..ch]) {
            *x = if s.is_finite() { s } else { 0.0 };
        }
        if ch <= 1 {
            return x[0];
        }
        let live = self.track(&x[..ch], self.picked);
        let auto = self.auto_pick(&live[..ch]);
        let mut target = [0.0f32; MAX_CAPTURE_CH];
        match mode {
            INPUT_CHANNEL_1 => target[0] = 1.0,
            INPUT_CHANNEL_2 => target[1] = 1.0,
            _ => match auto {
                Pick::One(c) => target[c] = 1.0,
                Pick::Even => target[..ch].fill(1.0 / ch as f32),
            },
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
                y += *w * x[c];
            }
        }
        y
    }

    /// Advance every channel's level and floor by one frame; returns which channels are live.
    /// The `picked` input's floor only falls: a note held for seconds is as steady as a hum, and
    /// with its floor rising to meet it the voice stopped counting as live mid-note — and a knock
    /// on the other input then took the take over.
    fn track(&mut self, x: &[f32], picked: Pick) -> [bool; MAX_CAPTURE_CH] {
        let warming = self.warmup > 0;
        self.warmup = self.warmup.saturating_sub(1);
        let mut live = [false; MAX_CAPTURE_CH];
        for (c, &s) in x.iter().enumerate() {
            let level = &mut self.level[c];
            *level += self.level_coef * (s * s + LEVEL_FLOOR - *level);
            let floor = &mut self.floor[c];
            if warming || *level < *floor {
                *floor = *level;
            } else if picked != Pick::One(c) {
                *floor += self.floor_coef * (*level - *floor);
            }
            live[c] = *level > LIVE_MIN && *level > LIVE_OVER_FLOOR * *floor;
        }
        live
    }

    /// Advance Auto's pick by one frame (see the type doc); returns what Auto mixes this frame.
    fn auto_pick(&mut self, live: &[bool]) -> Pick {
        let loudest = (0..live.len())
            .filter(|&c| live[c])
            .max_by(|&a, &b| self.level[a].total_cmp(&self.level[b]));
        let candidate = loudest.map(|l| {
            let dominant = (0..live.len())
                .all(|c| c == l || !live[c] || self.level[l] >= DOMINANCE * self.level[c]);
            if dominant {
                Pick::One(l)
            } else {
                Pick::Even
            }
        });
        let picked_live = matches!(self.picked, Pick::One(c) if live[c]);
        match candidate {
            Some(p) if p != self.picked => {
                if p != self.pending {
                    self.pending = p;
                    self.pending_frames = 0;
                }
                self.pending_frames += 1;
                let hold = if picked_live { self.late } else { self.soon };
                if self.pending_frames >= hold {
                    self.picked = p;
                }
            }
            _ => {
                self.pending = self.picked;
                self.pending_frames = 0;
            }
        }
        // Considering another pick while the picked input carries nothing (a mic moved to the
        // other input): blend, so the mic is not cut while Auto makes up its mind.
        if self.pending != self.picked && !picked_live {
            Pick::Even
        } else {
            self.picked
        }
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

    /// Auto: a mic on either input of a 2-input interface records at full level, the idle input's
    /// noise left out. The averaging downmix put such a mic 6 dB down with the idle input's noise
    /// mixed in, and the cpal path kept input 1 alone (silence for a mic on input 2).
    #[test]
    fn auto_takes_a_mic_on_either_input_at_full_level() {
        for live in 0..2 {
            let mut mix = InputChannelMix::new(SR);
            let mut floor = Floor(7);
            let onset = SR as usize / 2; // the stream opens, then the singer starts
            let (mut voice, mut out) = (Vec::new(), Vec::new());
            for n in 0..2 * SR as usize {
                let v = if n >= onset { tone(220.0, n) } else { 0.0 };
                let mut frame = [floor.next(), floor.next()];
                frame[live] += v;
                let y = mix.mix(INPUT_CHANNEL_AUTO, &frame);
                if n >= onset + SR as usize / 2 {
                    voice.push(v);
                    out.push(y);
                }
            }
            let err: Vec<f32> = out.iter().zip(&voice).map(|(y, v)| y - v).collect();
            let db = 20.0 * (rms(&out) / rms(&voice)).log10();
            assert!(db.abs() <= 0.1, "mic on input {}: {db:+.2} dB", live + 1);
            let left = 20.0 * (rms(&err) / rms(&voice)).log10();
            assert!(
                left <= -40.0,
                "input {}: the rest at {left:.1} dB",
                live + 1
            );
        }
    }

    /// Auto: a voice already sounding when the stream opens is as steady as a floor until it
    /// pauses, so it blends evenly until then — and is picked at its first breath.
    #[test]
    fn a_voice_sounding_from_the_first_frame_is_picked_at_its_first_pause() {
        let mut mix = InputChannelMix::new(SR);
        let mut floor = Floor(3);
        let gap = SR as usize..SR as usize + SR as usize / 5;
        let (mut voice, mut out) = (Vec::new(), Vec::new());
        for n in 0..3 * SR as usize {
            let v = if gap.contains(&n) {
                0.0
            } else {
                tone(220.0, n)
            };
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[floor.next(), v + floor.next()]);
            if n >= gap.end + SR as usize / 2 {
                voice.push(v);
                out.push(y);
            }
        }
        let db = 20.0 * (rms(&out) / rms(&voice)).log10();
        assert!(db.abs() <= 0.1, "after the pause: {db:+.2} dB");
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
    /// further than the voice's own steepest slope — and gets there within 200 ms.
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
                if n >= onset + SR as usize / 5 {
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
            assert!(db.abs() <= 0.5, "phase {phase}: {db:+.2} dB 200 ms in");
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

    /// Seconds to frames at [`SR`].
    fn at(secs: f64) -> usize {
        (secs * SR) as usize
    }

    /// A sine at `hz` with RMS `rms`, sample `n`.
    fn sine(hz: f64, rms: f64, n: usize) -> f32 {
        (rms * std::f64::consts::SQRT_2 * (std::f64::consts::TAU * hz * n as f64 / SR).sin()) as f32
    }

    /// 20·log10 of `err`'s RMS against `reference`'s.
    fn db_off(err: &[f32], reference: &[f32]) -> f64 {
        20.0 * (rms(err) / rms(reference)).log10()
    }

    /// Auto: a pop, a thump or a knocked guitar on the idle input never reaches the take nor
    /// ducks the voice. Each input's share of the level put the voice 12 dB down for two seconds
    /// after a 30 ms pop.
    #[test]
    fn a_burst_on_the_idle_input_does_not_duck_the_voice() {
        let mut mix = InputChannelMix::new(SR);
        let mut floor = Floor(5);
        let pop = at(1.0)..at(1.03); // -6 dBFS
        let thump = at(2.0)..at(2.2); // -12 dBFS
        let knock = at(3.0)..at(3.35); // -3 dBFS, a guitar knocked and ringing
        let (mut voice, mut err) = (Vec::new(), Vec::new());
        for n in 0..at(4.5) {
            let v = if n >= at(0.3) { tone(220.0, n) } else { 0.0 };
            let mut other = floor.next();
            if pop.contains(&n) {
                other += sine(1000.0, 0.5, n);
            }
            if thump.contains(&n) {
                other += sine(60.0, 0.25, n);
            }
            if knock.contains(&n) {
                other += sine(110.0, 0.7, n);
            }
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[v, other]);
            if n >= at(0.6) {
                voice.push(v);
                err.push(y - v);
            }
        }
        let off = db_off(&err, &voice);
        assert!(off <= -40.0, "the bursts reached the take at {off:.1} dB");
    }

    /// Auto holds its pick through a pause, even over an idle input humming louder than the mic's
    /// own floor: the next phrase is at full level from its first syllable, the hum left out.
    #[test]
    fn the_pick_holds_through_a_pause_over_a_humming_idle_input() {
        let mut mix = InputChannelMix::new(SR);
        let mut floor = Floor(13);
        let sung = |n: usize| (at(0.3)..at(1.3)).contains(&n) || n >= at(4.3);
        let (mut voice, mut err) = (Vec::new(), Vec::new());
        for n in 0..at(5.3) {
            let v = if sung(n) { tone(220.0, n) } else { 0.0 };
            let hum = sine(50.0, 0.0032, n); // -50 dBFS
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[hum, v + floor.next()]);
            if n >= at(4.3) {
                voice.push(v);
                err.push(y - v);
            }
        }
        let off = db_off(&err, &voice);
        assert!(off <= -40.0, "the second phrase is off by {off:.1} dB");
    }

    /// Auto follows a mic moved to the other input, and never cuts it while deciding: an even
    /// blend first, then that input alone.
    #[test]
    fn a_mic_moved_to_the_other_input_is_followed() {
        let mut mix = InputChannelMix::new(SR);
        let mut floor = Floor(17);
        let moved = at(1.8);
        let (mut deciding, mut deciding_voice) = (Vec::new(), Vec::new());
        let (mut voice, mut err) = (Vec::new(), Vec::new());
        for n in 0..at(3.0) {
            let v = tone(220.0, n);
            let frame = if n < at(1.3) && n >= at(0.3) {
                [v, floor.next()]
            } else if n >= moved {
                [floor.next(), v]
            } else {
                [floor.next(), floor.next()]
            };
            let y = mix.mix(INPUT_CHANNEL_AUTO, &frame);
            if (moved..moved + at(0.15)).contains(&n) {
                deciding.push(y);
                deciding_voice.push(v);
            }
            if n >= moved + at(0.25) {
                voice.push(v);
                err.push(y - v);
            }
        }
        let while_deciding = 20.0 * (rms(&deciding) / rms(&deciding_voice)).log10();
        assert!(
            while_deciding >= -6.5,
            "cut while deciding: {while_deciding:.1} dB"
        );
        let off = db_off(&err, &voice);
        assert!(off <= -40.0, "on its new input, off by {off:.1} dB");
    }

    /// Auto: two live inputs at comparable levels — a voice and a guitar 6 dB above it — blend
    /// evenly and hold still, where each one's share of the level put the voice 14 dB down. Once
    /// the guitar stops, the voice is alone again within a quarter second.
    #[test]
    fn comparable_live_inputs_blend_evenly() {
        let mut mix = InputChannelMix::new(SR);
        let mut floor = Floor(19);
        let guitar_ends = at(2.0);
        let (mut blend, mut blend_err) = (Vec::new(), Vec::new());
        let (mut voice, mut err) = (Vec::new(), Vec::new());
        for n in 0..at(3.0) {
            let v = if n >= at(0.3) { tone(220.0, n) } else { 0.0 };
            let g = if (at(0.3)..guitar_ends).contains(&n) {
                sine(330.0, 0.2, n)
            } else {
                floor.next()
            };
            let y = mix.mix(INPUT_CHANNEL_AUTO, &[v, g]);
            if (at(0.8)..guitar_ends).contains(&n) {
                blend.push(0.5 * (v + g));
                blend_err.push(y - 0.5 * (v + g));
            }
            if n >= guitar_ends + at(0.25) {
                voice.push(v);
                err.push(y - v);
            }
        }
        let off = db_off(&blend_err, &blend);
        assert!(off <= -40.0, "the blend is off the even mix by {off:.1} dB");
        let off = db_off(&err, &voice);
        assert!(off <= -40.0, "after the guitar, off by {off:.1} dB");
    }

    /// A NaN or infinite sample is silence, and the mic carries on in every mode: in the recursive
    /// level one stayed NaN for good, and every later output with it.
    #[test]
    fn a_non_finite_sample_does_not_silence_the_mic() {
        for (mode, live) in [
            (INPUT_CHANNEL_AUTO, 1),
            (INPUT_CHANNEL_1, 0),
            (INPUT_CHANNEL_2, 1),
        ] {
            let mut mix = InputChannelMix::new(SR);
            let mut floor = Floor(23);
            let (mut voice, mut out) = (Vec::new(), Vec::new());
            for n in 0..at(2.0) {
                let v = if n >= at(0.3) { tone(220.0, n) } else { 0.0 };
                let mut frame = [floor.next(), floor.next()];
                frame[live] += v;
                if n == at(1.0) {
                    frame[1 - live] = f32::NAN;
                }
                if n == at(1.2) {
                    frame[live] = f32::INFINITY;
                }
                let y = mix.mix(mode, &frame);
                assert!(y.is_finite(), "mode {mode}: frame {n} is {y}");
                if n >= at(1.3) {
                    voice.push(v);
                    out.push(y);
                }
            }
            let db = 20.0 * (rms(&out) / rms(&voice)).log10();
            assert!(db.abs() <= 0.1, "mode {mode}: {db:+.2} dB after");
        }
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
