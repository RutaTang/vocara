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

/// Time constant of the per-channel level Auto reads (s).
const LEVEL_TAU_S: f64 = 0.01;
/// Rise time constant of each channel's noise-floor estimate (s); it falls at once. A level has to
/// stand [`LIVE_OVER_FLOOR`] above its floor to count as live, so a steady hum or hiss — present
/// from the start or appearing later — stops counting within about a second...
const FLOOR_RISE_S: f64 = 10.0;
/// ...and three times slower on the picked input once it has dipped to near its floor (within
/// [`DIP_OVER_FLOOR`]) and come back while picked — as a voice does at every consonant and breath,
/// and a hum never does, however its level ripples around the live line — so a note held for
/// seconds keeps counting, but a hum that took the pick in a silence does not.
const PICKED_FLOOR_RISE_S: f64 = 30.0;
/// How near its floor a level has to fall to count as a dip (power ratio: 3 dB).
const DIP_OVER_FLOOR: f32 = 2.0;
/// How long after the mixer starts the floor follows the level both ways (s): a hum present from
/// the first frame is the floor from the start.
const FLOOR_WARMUP_S: f64 = 0.1;
/// A live channel stands this far above its floor (power ratio: 10 dB)...
const LIVE_OVER_FLOOR: f32 = 10.0;
/// ...and above this level (mean square: -60 dBFS).
const LIVE_MIN: f32 = 1.0e-6;
/// Time constant of each channel's evidence of use (s): the share of recent time it was live. A
/// singer's syllables keep it high across their gaps; a pop, a knock or a hum that stops counting
/// leaves a dent that fades.
const EVIDENCE_S: f64 = 4.0;
/// Evidence a channel needs to be picked first, with no other input in use (about 0.3 s of
/// singing)...
const PICK_MIN: f32 = 0.05;
/// ...while every other input has at most this share of its evidence.
const DOMINANT_SHARE: f32 = 0.5;
/// How much more evidence another input needs to take the pick while the picked one is in use...
const SWITCH_MARGIN: f32 = 0.3;
/// ...and once it has gone quiet (a mic moved to the other input).
const SWITCH_MARGIN_QUIET: f32 = 0.1;
/// Evidence on every input, none of them dominant, that makes them all in use (two sources, a
/// stereo mic): blend evenly. A strummed guitar is live about a quarter of the time.
const BLEND_MIN: f32 = 0.2;
/// How long the picked input counts as in use after it was last live, at full evidence (s): it
/// spans a breath or a held note, so a burst on the other input then is left out. Shorter at
/// lower evidence, down to a fifth.
const HANGOVER_S: f64 = 1.5;
/// Sustained activity on another input (leaky: one up per live frame, half down per quiet one)
/// that blends it in while the picked input is quiet (s): a mic moved there is never cut, a pop
/// is over long before.
const RUN_MIN_S: f64 = 0.25;
/// Time constant of the correlation that tells an even blend to flip input 2 (s): a stereo pair
/// with one channel's polarity reversed cancelled itself out. Long, so two different sources —
/// beating against each other — average out near zero instead of flipping each other.
const CORR_TAU_S: f64 = 0.5;
/// Correlation below which the blend flips input 2, and above which it stops.
const FLIP_BELOW: f32 = -0.7;
const UNFLIP_ABOVE: f32 = -0.2;
/// Time constant the weights glide toward their target with (s): a change of input — Auto's or
/// the user's — is a glide, not a click.
const WEIGHT_GLIDE_S: f64 = 0.01;
/// Bias added to every level so it never decays into subnormals. Far below any real noise floor.
const LEVEL_FLOOR: f32 = 1.0e-18;
/// Largest magnitude taken as a sample (+24 dBFS). Past it — or not finite — it is garbage from a
/// driver or virtual device, and silence: squared, one 2e19 overflowed the level for good.
const SAMPLE_MAX: f32 = 16.0;

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
/// * Auto: the input in use. An input is live while its level (mean square over [`LEVEL_TAU_S`])
///   stands above [`LIVE_MIN`] and [`LIVE_OVER_FLOOR`] above its own noise floor, so a steady hum
///   or noise never counts for long; its evidence is the share of recent time it was live
///   ([`EVIDENCE_S`]). The pick follows the evidence: the first input with [`PICK_MIN`] while the
///   others have at most [`DOMINANT_SHARE`] of it; another input once it has [`SWITCH_MARGIN`]
///   more ([`SWITCH_MARGIN_QUIET`] once the picked one has gone quiet); an even blend while every
///   input has [`BLEND_MIN`] and none dominates (two sources, a stereo mic — with input 2 flipped
///   if the pair is in opposite polarity). While the picked input is in use — live within its
///   [`HANGOVER_S`], across breaths and held notes — the other input is left out: a pop, a thump
///   or a knocked guitar there never reaches the take. Once it is quiet, another input with
///   [`RUN_MIN_S`] of sustained activity is blended in, so a mic moved there is never cut for
///   longer than that while the evidence catches up. A hum that took the pick in a silence stops
///   counting within about a second, and the voice takes the pick back.
///
///   The price of deciding on seconds of evidence: the first quarter second or so of singing, a
///   mic moved to the other input for a couple of seconds, and a singer joining a guitar already
///   on the other input after about a second, all blend evenly (6 dB down) before Auto settles —
///   and with two sources it settles on the one in clearly more use. Input 1 / Input 2 are for
///   two sources. Only the first [`MAX_CAPTURE_CH`] channels of a wider device are weighed.
///
/// A sample past [`SAMPLE_MAX`] or not finite is silence. The weights glide toward their target
/// ([`WEIGHT_GLIDE_S`]). A 1-channel device passes through untouched. RT-safe: no alloc / lock /
/// syscall, a few dozen operations per frame.
pub struct InputChannelMix {
    /// Per-sample one-pole coefficients.
    level_coef: f32,
    floor_coef: f32,
    picked_floor_coef: f32,
    evidence_coef: f32,
    corr_coef: f32,
    glide: f32,
    /// Durations in frames; `warmup` counts down.
    warmup: u32,
    hangover: f32,
    run_min: f32,
    /// Per channel: smoothed level and noise floor, evidence of use, frames since last live, and
    /// sustained activity — tracked in every mode so a switch to Auto starts from settled ones.
    level: [f32; MAX_CAPTURE_CH],
    floor: [f32; MAX_CAPTURE_CH],
    evidence: [f32; MAX_CAPTURE_CH],
    quiet: [u32; MAX_CAPTURE_CH],
    run: [f32; MAX_CAPTURE_CH],
    /// Slow powers and cross power of inputs 1 and 2, and whether the even blend flips input 2.
    power: [f32; 2],
    cross: f32,
    flip: bool,
    /// Auto's pick; whether the picked input has dipped and come back since it was picked (see
    /// [`PICKED_FLOOR_RISE_S`]); and which inputs have dipped since they were last live.
    picked: Pick,
    proven: bool,
    dipped: [bool; MAX_CAPTURE_CH],
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
        let frames = |secs: f64| (secs * rate) as f32;
        InputChannelMix {
            level_coef: coef(LEVEL_TAU_S),
            floor_coef: coef(FLOOR_RISE_S),
            picked_floor_coef: coef(PICKED_FLOOR_RISE_S),
            evidence_coef: coef(EVIDENCE_S),
            corr_coef: coef(CORR_TAU_S),
            glide: coef(WEIGHT_GLIDE_S),
            warmup: frames(FLOOR_WARMUP_S) as u32,
            hangover: frames(HANGOVER_S),
            run_min: frames(RUN_MIN_S),
            level: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            floor: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            evidence: [0.0; MAX_CAPTURE_CH],
            quiet: [u32::MAX; MAX_CAPTURE_CH],
            run: [0.0; MAX_CAPTURE_CH],
            power: [LEVEL_FLOOR; 2],
            cross: 0.0,
            flip: false,
            picked: Pick::Even,
            proven: false,
            dipped: [false; MAX_CAPTURE_CH],
            weight: [1.0 / MAX_CAPTURE_CH as f32; MAX_CAPTURE_CH],
            primed: false,
        }
    }

    /// One frame's channel samples (`frame`, the device's channels in order) to the mono signal
    /// under `mode`; an unknown mode is Auto.
    pub fn mix(&mut self, mode: u32, frame: &[f32]) -> f32 {
        let ch = frame.len().min(MAX_CAPTURE_CH);
        let mut x = [0.0f32; MAX_CAPTURE_CH];
        for (x, &s) in x.iter_mut().zip(&frame[..ch]) {
            *x = if s.is_finite() && s.abs() <= SAMPLE_MAX {
                s
            } else {
                0.0
            };
        }
        if ch <= 1 {
            return x[0];
        }
        let live = self.track(&x[..ch]);
        let auto = self.auto_pick(&live[..ch]);
        let mut target = [0.0f32; MAX_CAPTURE_CH];
        match mode {
            INPUT_CHANNEL_1 => target[0] = 1.0,
            INPUT_CHANNEL_2 => target[1] = 1.0,
            _ => match auto {
                Pick::One(c) => target[c] = 1.0,
                Pick::Even => {
                    target[..ch].fill(1.0 / ch as f32);
                    if self.flip {
                        target[1] = -target[1];
                    }
                }
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
            // Snapped once there (a 0.001 dB step): gliding on toward exactly 0 sat at a
            // subnormal, which is slow where nothing flushes those to zero (the cpal input path),
            // and toward 1 it stalled a rounding step short.
            if (t - *w).abs() < 1.0e-4 {
                *w = t;
            }
            if c < ch {
                y += *w * x[c];
            }
        }
        y
    }

    /// Advance every channel's level, floor, evidence and activity by one frame, and the pair's
    /// correlation; returns which channels are live.
    fn track(&mut self, x: &[f32]) -> [bool; MAX_CAPTURE_CH] {
        let warming = self.warmup > 0;
        self.warmup = self.warmup.saturating_sub(1);
        let mut live = [false; MAX_CAPTURE_CH];
        for (c, &s) in x.iter().enumerate() {
            let level = &mut self.level[c];
            *level += self.level_coef * (s * s + LEVEL_FLOOR - *level);
            let floor = &mut self.floor[c];
            if warming || *level < *floor {
                *floor = *level;
            } else {
                let rise = if self.proven && self.picked == Pick::One(c) {
                    self.picked_floor_coef
                } else {
                    self.floor_coef
                };
                *floor += rise * (*level - *floor);
            }
            live[c] = *level > LIVE_MIN && *level > LIVE_OVER_FLOOR * *floor;
            if *level < DIP_OVER_FLOOR * *floor {
                self.dipped[c] = true;
            } else if live[c] && self.dipped[c] {
                self.dipped[c] = false;
                if self.picked == Pick::One(c) {
                    self.proven = true;
                }
            }
            let hit = if live[c] { 1.0 } else { 0.0 };
            self.evidence[c] += self.evidence_coef * (hit - self.evidence[c]);
            if live[c] {
                self.quiet[c] = 0;
                self.run[c] = (self.run[c] + 1.0).min(2.0 * self.run_min);
            } else {
                self.quiet[c] = self.quiet[c].saturating_add(1);
                self.run[c] = (self.run[c] - 0.5).max(0.0);
            }
        }
        for (p, &s) in self.power.iter_mut().zip(x) {
            *p += self.corr_coef * (s * s + LEVEL_FLOOR - *p);
        }
        self.cross += self.corr_coef * (x[0] * x[1] - self.cross);
        if live[0] && live[1] {
            let corr = self.cross / (self.power[0] * self.power[1]).sqrt();
            if corr < FLIP_BELOW {
                self.flip = true;
            } else if corr > UNFLIP_ABOVE {
                self.flip = false;
            }
        }
        live
    }

    /// Whether channel `c` is in use: live within its hangover, which shrinks with its evidence.
    fn in_use(&self, c: usize) -> bool {
        let scale = (self.evidence[c] / 0.5).clamp(0.2, 1.0);
        (self.quiet[c] as f32) < self.hangover * scale
    }

    /// Advance Auto's pick by one frame (see the type doc); returns what Auto mixes this frame.
    fn auto_pick(&mut self, live: &[bool]) -> Pick {
        let ch = live.len();
        let e = &self.evidence[..ch];
        let strongest = (0..ch).max_by(|&a, &b| e[a].total_cmp(&e[b])).unwrap_or(0);
        // Dominant and blended are exclusive, so the pick never flips between them frame by frame.
        let dominant = (0..ch).all(|c| c == strongest || e[c] <= DOMINANT_SHARE * e[strongest]);
        let blended = !dominant && e.iter().all(|&v| v >= BLEND_MIN);
        match self.picked {
            Pick::Even => {
                if e[strongest] >= PICK_MIN && dominant {
                    self.picked = Pick::One(strongest);
                    self.proven = false;
                }
            }
            Pick::One(a) => {
                let margin = if self.in_use(a) {
                    SWITCH_MARGIN
                } else {
                    SWITCH_MARGIN_QUIET
                };
                if blended {
                    self.picked = Pick::Even;
                } else if strongest != a && e[strongest] >= e[a] + margin {
                    self.picked = Pick::One(strongest);
                    self.proven = false;
                }
            }
        }
        match self.picked {
            Pick::One(a)
                if !self.in_use(a) && (0..ch).any(|b| b != a && self.run[b] >= self.run_min) =>
            {
                Pick::Even
            }
            p => p,
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

    // Marker samples, small and exact in f32: past `SAMPLE_MAX` a sample is garbage, and silence.
    fn left(i: usize) -> f32 {
        i as f32 / 1024.0
    }

    fn right(i: usize) -> f32 {
        0.5 + i as f32 / 1024.0
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

    /// A singer from `from` to `to` (s): syllables of 0.22-0.34 s split by 40-90 ms consonant
    /// gaps, in 3 s phrases with 0.45 s breaths, about `rms_db` dBFS, a vibrato — deterministic,
    /// and dipping the way a voice does (a steady tone never would, and Auto tells them apart).
    struct Singer {
        env: Vec<f32>,
        f0: f64,
        rms: f64,
        phase: f64,
        vib: f64,
        last: f32,
    }

    impl Singer {
        fn new(len: usize, from: f64, to: f64, rms_db: f64, f0: f64, seed: u32) -> Singer {
            let mut rng = Floor(seed);
            let mut unit = || 0.5 + rng.next() as f64 / 1.1e-4; // 0..1
            let mut env = vec![0.0f32; len];
            let mut t = from;
            while t < to {
                let phrase_end = (t + 3.0).min(to);
                let mut s = t;
                while s < phrase_end {
                    let end = (s + 0.22 + 0.12 * unit()).min(phrase_end);
                    let level = 10f64.powf((-3.0 + 6.0 * unit()) / 20.0) as f32;
                    let (a, b) = (at(s).min(len), at(end).min(len));
                    let ramp = at(0.008) as f32;
                    for (k, e) in env[a..b].iter_mut().enumerate() {
                        let edge = (k.min(b - a - 1 - k)) as f32 / ramp;
                        *e = level * edge.min(1.0);
                    }
                    s = end + 0.04 + 0.05 * unit();
                }
                t = phrase_end + 0.45;
            }
            Singer {
                env,
                f0,
                rms: 10f64.powf(rms_db / 20.0),
                phase: 0.0,
                vib: 0.0,
                last: 0.0,
            }
        }

        /// The sample [`Singer::at`] returned last.
        fn at_last(&self) -> f32 {
            self.last
        }

        /// The voice at sample `n` (call in order, once per sample).
        fn at(&mut self, n: usize) -> f32 {
            use std::f64::consts::TAU;
            self.vib += TAU * 5.5 / SR;
            self.phase += TAU * self.f0 * 2f64.powf(50.0 / 1200.0 * self.vib.sin()) / SR;
            let mut s = 0.0;
            for h in 1..=6 {
                s += (h as f64 * self.phase).sin() / h as f64;
            }
            self.last =
                (self.env.get(n).copied().unwrap_or(0.0) as f64 * self.rms * s / 0.9) as f32;
            self.last
        }
    }

    /// Runs `secs` of Auto over `frame(n) -> ([input 1, input 2], wanted)`; returns the take and
    /// what it should have been.
    fn auto_run(
        secs: f64,
        mut frame: impl FnMut(usize) -> ([f32; 2], f32),
    ) -> (Vec<f32>, Vec<f32>) {
        let mut mix = InputChannelMix::new(SR);
        let (mut out, mut want) = (Vec::new(), Vec::new());
        for n in 0..at(secs) {
            let (f, w) = frame(n);
            out.push(mix.mix(INPUT_CHANNEL_AUTO, &f));
            want.push(w);
        }
        (out, want)
    }

    /// The take's error against what it should have been over `span` (s), in dB re the latter.
    fn off_over(out: &[f32], want: &[f32], span: std::ops::Range<f64>) -> f64 {
        let r = at(span.start)..at(span.end).min(out.len());
        let err: Vec<f32> = out[r.clone()]
            .iter()
            .zip(&want[r.clone()])
            .map(|(y, w)| y - w)
            .collect();
        db_off(&err, &want[r])
    }

    /// The lowest gain the wanted signal gets over any 0.5 s window of `span` (s) it sounds in.
    fn min_gain_over(out: &[f32], want: &[f32], span: std::ops::Range<f64>) -> f64 {
        let mut lowest = f64::INFINITY;
        let mut a = at(span.start);
        while a + at(0.5) <= at(span.end).min(out.len()) {
            let w = &want[a..a + at(0.5)];
            let ww: f64 = w.iter().map(|&v| (v as f64).powi(2)).sum();
            if ww / w.len() as f64 > 1e-6 {
                let yw: f64 = out[a..a + at(0.5)]
                    .iter()
                    .zip(w)
                    .map(|(&y, &v)| y as f64 * v as f64)
                    .sum();
                lowest = lowest.min(yw / ww);
            }
            a += at(0.5);
        }
        lowest
    }

    /// Auto: a pop, a thump or a knocked guitar on the idle input — in a phrase or a breath —
    /// never reaches the take nor ducks the voice. Each input's share of the level put the voice
    /// 12 dB down for two seconds after a 30 ms pop; a pick that blended at every breath let one
    /// pop in eight through.
    #[test]
    fn a_burst_on_the_idle_input_does_not_reach_the_take() {
        let secs = 30.0;
        let mut singer = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
        let mut floor = Floor(5);
        let mut room = Floor(6);
        let mut when = Floor(9);
        let bursts: Vec<(usize, u32)> = (0..45)
            .map(|k| (at(2.0 + 27.0 * (0.5 + when.next() as f64 / 1.1e-4)), k % 3))
            .collect();
        let (out, want) = auto_run(secs, |n| {
            let v = singer.at(n) + 0.25 * room.next();
            let mut other = floor.next();
            for &(p, kind) in &bursts {
                if n < p {
                    continue;
                }
                let t = (n - p) as f64 / SR;
                other += match kind {
                    0 if t < 0.03 => sine(1000.0, 0.5, n),        // pop, -6 dBFS
                    1 if t < 0.2 => sine(60.0, 0.25, n),          // thump, -12 dBFS
                    2 => sine(110.0, 0.3 * (-t / 0.15).exp(), n), // knock, -10 dBFS
                    _ => 0.0,
                };
            }
            ([other, v], v)
        });
        let off = off_over(&out, &want, 1.0..secs);
        assert!(off <= -40.0, "the bursts reached the take at {off:.1} dB");
    }

    /// A hum appearing on the idle input in a silence never takes the voice: the steady hum that
    /// took the pick there kept it for good, its floor frozen, while every consonant gap kept the
    /// voice from winning it back.
    #[test]
    fn a_hum_appearing_in_a_silence_does_not_take_the_voice() {
        let secs = 20.0;
        let mut singer = Singer::new(at(secs), 6.0, secs, -20.0, 196.0, 3);
        let mut floor = Floor(5);
        let mut room = Floor(6);
        let (out, want) = auto_run(secs, |n| {
            let hum = if n >= at(4.0) {
                sine(50.0, 0.0032, n)
            } else {
                0.0
            }; // -50 dBFS
            (
                [floor.next() + hum, singer.at(n) + 0.25 * room.next()],
                singer.at_last(),
            )
        });
        let worst = min_gain_over(&out, &want, 6.5..secs);
        assert!(worst >= 0.45, "the voice fell to {worst:.2} once singing");
        let off = off_over(&out, &want, 10.0..secs);
        assert!(off <= -30.0, "the hum is still in the take at {off:.1} dB");
    }

    /// A noise floor stepping up on the idle input mid-phrase is left out.
    #[test]
    fn a_noise_step_on_the_idle_input_is_left_out() {
        let secs = 20.0;
        let mut singer = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
        let (mut quiet, mut loud) = (Floor(5), Floor(7));
        let mut room = Floor(6);
        let (out, want) = auto_run(secs, |n| {
            let idle = if n >= at(8.0) {
                56.0 * loud.next()
            } else {
                quiet.next()
            }; // -55 dBFS
            ([idle, singer.at(n) + 0.25 * room.next()], singer.at_last())
        });
        let off = off_over(&out, &want, 1.0..secs);
        assert!(off <= -40.0, "the noise reached the take at {off:.1} dB");
    }

    /// Auto holds its pick through a pause, even over an idle input humming louder than the mic's
    /// own floor: the next phrase is at full level from its first syllable, the hum left out.
    #[test]
    fn the_pick_holds_through_a_pause_over_a_humming_idle_input() {
        let secs = 14.0;
        let mut first = Singer::new(at(secs), 0.3, 4.0, -20.0, 196.0, 3);
        let mut second = Singer::new(at(secs), 9.0, secs, -20.0, 196.0, 4);
        let mut room = Floor(6);
        let (out, want) = auto_run(secs, |n| {
            let v = first.at(n) + second.at(n);
            ([sine(50.0, 0.0032, n), v + 0.25 * room.next()], v)
        });
        let off = off_over(&out, &want, 9.0..secs);
        assert!(off <= -40.0, "the second phrase is off by {off:.1} dB");
    }

    /// Auto follows a mic moved to the other input, and never cuts it once it sings there: an even
    /// blend while the evidence turns, then that input alone.
    #[test]
    fn a_mic_moved_to_the_other_input_is_followed() {
        let secs = 24.0;
        let mut before = Singer::new(at(secs), 0.5, 10.0, -20.0, 196.0, 3);
        let mut after = Singer::new(at(secs), 12.0, secs, -20.0, 196.0, 4);
        let (mut room1, mut room2) = (Floor(5), Floor(6));
        let (out, want) = auto_run(secs, |n| {
            let (a, b) = (before.at(n), after.at(n));
            ([b + 0.25 * room1.next(), a + 0.25 * room2.next()], a + b)
        });
        let worst = min_gain_over(&out, &want, 12.5..secs);
        assert!(worst >= 0.45, "the moved mic fell to {worst:.2}");
        let off = off_over(&out, &want, 17.0..secs);
        assert!(off <= -40.0, "on its new input, off by {off:.1} dB");
    }

    /// Two singers, one on each input, blend evenly — and when one stops, the other is alone again
    /// within seconds. Each input's share of the level instead put the quieter one 14 dB down.
    #[test]
    fn two_inputs_in_use_blend_evenly() {
        let secs = 24.0;
        let mut a = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
        let mut b = Singer::new(at(secs), 0.5, 12.0, -14.0, 262.0, 4);
        let (out, want) = auto_run(secs, |n| {
            let (x, y) = (a.at(n), b.at(n));
            ([x, y], if n < at(12.0) { 0.5 * (x + y) } else { x })
        });
        let off = off_over(&out, &want, 4.0..12.0);
        assert!(off <= -40.0, "the blend is off the even mix by {off:.1} dB");
        let off = off_over(&out, &want, 17.0..secs);
        assert!(off <= -40.0, "alone again, off by {off:.1} dB");
    }

    /// A singer joining a guitar already on the other input is heard within seconds.
    #[test]
    fn a_singer_joining_a_live_guitar_is_heard() {
        let secs = 20.0;
        let mut singer = Singer::new(at(secs), 6.0, secs, -20.0, 196.0, 3);
        let mut room = Floor(6);
        let (out, want) = auto_run(secs, |n| {
            let strum = (n % at(0.5)) as f64 / SR;
            let guitar = sine(147.0, 0.14 * (-strum / 0.3).exp(), n);
            (
                [guitar, singer.at(n) + 0.25 * room.next()],
                singer.at_last(),
            )
        });
        let worst = min_gain_over(&out, &want, 10.0..secs);
        assert!(worst >= 0.45, "the singer fell to {worst:.2}");
    }

    /// A stereo mic with one channel's polarity reversed: the even blend flips it back instead of
    /// cancelling the voice out.
    #[test]
    fn a_stereo_pair_in_opposite_polarity_does_not_cancel() {
        let secs = 8.0;
        let mut singer = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
        let (out, want) = auto_run(secs, |n| {
            let v = singer.at(n);
            ([v, -v], v)
        });
        let worst = min_gain_over(&out, &want, 2.0..secs);
        assert!(worst >= 0.95, "the pair cancelled to {worst:.2}");
    }

    /// A NaN, an infinity or a finite but absurd sample is silence, and the mic carries on in
    /// every mode — the recursive level kept one NaN for good, and squared, a finite 2e19 made
    /// one: every later output was lost.
    #[test]
    fn a_garbage_sample_does_not_silence_the_mic() {
        for (mode, live) in [
            (INPUT_CHANNEL_AUTO, 1),
            (INPUT_CHANNEL_1, 0),
            (INPUT_CHANNEL_2, 1),
        ] {
            let secs = 6.0;
            let mut singer = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
            let mut floor = Floor(23);
            let mut mix = InputChannelMix::new(SR);
            let (mut voice, mut out) = (Vec::new(), Vec::new());
            for n in 0..at(secs) {
                let v = singer.at(n);
                let mut frame = [floor.next(), floor.next()];
                frame[live] += v;
                if n == at(2.0) {
                    frame[1 - live] = f32::NAN;
                }
                if n == at(2.1) {
                    frame[1 - live] = 2.0e19;
                }
                if n == at(2.2) {
                    frame[live] = f32::INFINITY;
                }
                let y = mix.mix(mode, &frame);
                assert!(y.is_finite(), "mode {mode}: frame {n} is {y}");
                if n >= at(2.5) {
                    voice.push(v);
                    out.push(y);
                }
            }
            let off = db_off(
                &out.iter()
                    .zip(&voice)
                    .map(|(y, v)| y - v)
                    .collect::<Vec<_>>(),
                &voice,
            );
            assert!(off <= -40.0, "mode {mode}: off by {off:.1} dB after");
        }
    }

    /// A finite but absurd sample poisons nothing: squared, a 2e19 overflowed the input's level
    /// to infinity and then NaN for good, and a mic moved onto that input was never followed.
    #[test]
    fn a_garbage_sample_does_not_poison_an_input() {
        let secs = 12.0;
        let mut before = Singer::new(at(secs), 0.5, 3.0, -20.0, 196.0, 3);
        let mut after = Singer::new(at(secs), 4.0, secs, -20.0, 196.0, 4);
        let (mut room1, mut room2) = (Floor(5), Floor(6));
        let (out, want) = auto_run(secs, |n| {
            let (a, b) = (before.at(n), after.at(n));
            let input1 = if n == at(1.0) {
                2.0e19
            } else {
                b + 0.25 * room1.next()
            };
            ([input1, a + 0.25 * room2.next()], a + b)
        });
        let off = off_over(&out, &want, 8.0..secs);
        assert!(off <= -40.0, "the mic moved onto it is off by {off:.1} dB");
    }

    /// Once picked, the weights sit exactly on their target: the take is the picked input,
    /// sample for sample. Gliding on toward 0 sat at a subnormal (slow where nothing flushes those),
    /// and toward 1 a rounding step short.
    #[test]
    fn the_weights_settle_exactly_on_their_target() {
        let secs = 4.0;
        let mut singer = Singer::new(at(secs), 0.5, secs, -20.0, 196.0, 3);
        let mut floor = Floor(5);
        let mut mix = InputChannelMix::new(SR);
        for n in 0..at(secs) {
            let frame = [floor.next(), singer.at(n)];
            let y = mix.mix(INPUT_CHANNEL_AUTO, &frame);
            if n >= at(3.0) {
                assert_eq!(y, frame[1], "frame {n}");
            }
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
