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
/// stand [`VOICED_OVER_FLOOR`] above its floor to count as voiced, so a steady hum or hiss — present
/// from the start or appearing later — stops counting within about a second.
const FLOOR_RISE_S: f64 = 10.0;
/// How near its floor a level has to fall to count as a dip (power ratio: 3 dB)...
const DIP_OVER_FLOOR: f64 = 2.0;
/// ...and how long after its last dip the picked input's floor holds still, once it has dipped and
/// come back while picked (s): a voice dips at every consonant and breath, so a legato phrase or a
/// note held for seconds keeps counting as voiced.
const PICKED_DIP_HOLD_S: f64 = 20.0;
/// Before that, the picked input's floor rises this much slower than [`FLOOR_RISE_S`], and so does
/// a newcomer's blended in beside it: a first phrase sung legato keeps its pick, or takes it from a
/// hum that took it in a silence. Such a hum leads no longer than the voice takes to start — a
/// held level stops leading once another input peaks above it.
const PICKED_FLOOR_RISE_S: f64 = 30.0;
/// How long after its last dip the picked input, standing clear of its floor but no longer voiced,
/// still sounds (s): it leads while no input that led within its hangover peaked above it, and sets
/// the level the others are judged against. A first phrase held legato keeps a quieter talkback
/// out; a hum that took the pick in a silence has stopped counting by then.
const SOUNDING_HOLD_S: f64 = 10.0;
/// How long after the mixer starts the floor follows the level both ways (s): a hum present from
/// the first frame is the floor from the start.
const FLOOR_WARMUP_S: f64 = 0.1;
/// A voiced channel stands this far above its floor (power ratio: 10 dB)...
const VOICED_OVER_FLOOR: f64 = 10.0;
/// ...and above this level (mean square: -60 dBFS).
const VOICED_MIN: f64 = 1.0e-6;
/// An input leads while it sounds — voiced, or for the picked one, standing clear of its floor
/// (within [`DIP_OVER_FLOOR`], see [`SOUNDING_HOLD_S`]), so a held note keeps leading — and is
/// within this of the loudest input sounding (power ratio: 10 dB). Only a leading input gathers
/// evidence, keeps its pick in use, or builds sustained activity: a second mic hearing the singer
/// from across the room, crosstalk, a talkback or a hum under the voice never leads while the voice
/// sounds.
const LEAD_WITHIN: f64 = 10.0;
/// Time constant of each input's evidence of use (s): the share of recent time it led.
const EVIDENCE_S: f64 = 4.0;
/// Evidence an input needs to be picked first (about 0.2 s of singing).
const PICK_MIN: f64 = 0.05;
/// Evidence ratios (weakest / strongest) at which an even blend begins and ends; between them
/// the pick stands. The weakest needs [`BLEND_MIN`] too.
const BLEND_ENTER: f64 = 0.6;
const BLEND_LEAVE: f64 = 0.4;
const BLEND_MIN: f64 = 0.2;
/// No change of pick but the first comes sooner than this after the last (s): two sources trading
/// the lead flipped the pick — and the echo path the canceller had settled on — twice a second.
const PICK_DWELL_S: f64 = 1.0;
/// How much more evidence another input needs to take the pick while the picked one is in use...
const SWITCH_MARGIN: f64 = 0.3;
/// ...and once it has gone quiet (a mic moved to the other input).
const SWITCH_MARGIN_QUIET: f64 = 0.1;
/// How long the picked input counts as in use after it last led, at full evidence (s): it spans a
/// breath, so a burst on the other input then is left out. Shorter at lower evidence, down to a
/// fifth.
const HANGOVER_S: f64 = 1.5;
/// Sustained leading on another input (leaky: one up per leading frame, half down per other) that
/// blends it in once the picked input is out of use (s): a mic moved there is never cut longer, a
/// pop is over long before...
const RUN_MIN_S: f64 = 0.25;
/// ...and that blends it in even while the picked input is in use (s): a singer joining a guitar
/// already on the other input. A knock or a thump leads for well under half this.
const JOIN_RUN_S: f64 = 1.0;
/// An input standing clear of its floor this far below another leading one is out of use, whatever
/// its hangover (power ratio: 20 dB): a hum that took the pick in a silence gives way the moment
/// the voice starts. A knock over a held note is nowhere near this far above it — and in a breath
/// the voice falls to its floor, so its hangover still keeps a pop or a talkback out of the take.
const OUTCLASSED_BY: f64 = 100.0;
/// Fall time constant of the peak level that outclassing is judged against (s): a voice in a
/// consonant gap keeps its last syllable's peak, so a burst then does not outclass it.
const PEAK_FALL_S: f64 = 0.5;
/// Time constant of the correlation that tells an even blend to flip input 2 (s): a stereo pair
/// with one channel's polarity reversed cancelled itself out. Long, so two different sources —
/// beating against each other — average out near zero instead of flipping each other.
const CORR_TAU_S: f64 = 0.5;
/// Correlation below which the blend flips input 2, and above which it stops.
const FLIP_BELOW: f64 = -0.7;
const UNFLIP_ABOVE: f64 = -0.2;
/// Time constant the weights glide toward their target with (s): a change of input — Auto's or
/// the user's — is a glide, not a click.
const WEIGHT_GLIDE_S: f64 = 0.01;
/// Bias added to every level so it never decays into subnormals. Far below any real noise floor.
const LEVEL_FLOOR: f64 = 1.0e-18;
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
/// * Auto: the input in use. An input is voiced while its level (mean square over
///   [`LEVEL_TAU_S`]) stands above [`VOICED_MIN`] and [`VOICED_OVER_FLOOR`] above its own noise
///   floor, so a steady hum or noise never counts for long, and leads while it is also within
///   [`LEAD_WITHIN`] of the loudest input sounding, so bleed, crosstalk, a talkback or a hum under
///   the voice never counts while the voice sounds. Its evidence is the share of recent time it
///   led ([`EVIDENCE_S`]). The pick follows the evidence, never sooner than [`PICK_DWELL_S`] after
///   its last change: the first input with [`PICK_MIN`]; another input once it has
///   [`SWITCH_MARGIN`] more ([`SWITCH_MARGIN_QUIET`] once the picked one is out of use); an even
///   blend between [`BLEND_ENTER`] and [`BLEND_LEAVE`] of evidence (two sources, a stereo mic —
///   with input 2 flipped if the pair is in opposite polarity).
///
///   While the picked input is in use — it led within its [`HANGOVER_S`], across breaths and
///   consonants, and a held note keeps leading ([`PICKED_DIP_HOLD_S`]) — another input is left
///   out: a pop, a thump or a knocked guitar there never reaches the take. Only another input
///   leading on for [`JOIN_RUN_S`] is blended in then (a singer joining a guitar). Out of use — or
///   outclassed by [`OUTCLASSED_BY`], as a hum that took the pick in a silence is by the voice —
///   the picked input lets another input leading on for [`RUN_MIN_S`] blend in, and hands it the
///   pick after [`PICK_DWELL_S`].
///
///   The price of deciding on seconds of evidence: the first fifth of a second or so of singing
///   blends evenly (6 dB down); a voice after a hum that took the pick in a silence, or a mic
///   re-plugged into the other input, is cut for about [`RUN_MIN_S`] and blends for a second or
///   two (a quiet singer within [`LEAD_WITHIN`] of the hum, starting legato a second or so after
///   it, is cut for a second and blends for several); a mic moved without a pause, a singer
///   joining a guitar already on the other input, or one starting right after a talkback, is cut
///   for up to about [`JOIN_RUN_S`] (it looks like a breath until then — telling a knock's ringing
///   from a first syllable any sooner would cost the burst protection) and then blends for a
///   second or two; and a source sounding more than [`LEAD_WITHIN`] above a quiet singer blends in
///   until it stops counting, about a second for a hum. With two sources Auto settles on the one
///   in clearly more use — Input 1 / Input 2 are for two sources. Only the first
///   [`MAX_CAPTURE_CH`] channels of a wider device are weighed.
///
/// A sample past [`SAMPLE_MAX`] or not finite is silence. The weights glide toward their target
/// ([`WEIGHT_GLIDE_S`]). A 1-channel device passes through untouched. RT-safe: no alloc / lock /
/// syscall, a few dozen operations per frame.
pub struct InputChannelMix {
    /// Per-sample one-pole coefficients.
    level_coef: f64,
    floor_coef: f64,
    picked_floor_coef: f64,
    evidence_coef: f64,
    peak_coef: f64,
    corr_coef: f64,
    glide: f32,
    /// Durations in frames; `warmup` counts down.
    warmup: u32,
    dip_hold: u32,
    sounding_hold: u32,
    dwell: u32,
    hangover: f32,
    run_min: f32,
    join_run: f32,
    /// Per channel: smoothed level and noise floor, evidence of use, frames since it last led and
    /// since it last dipped, and sustained leading — tracked in every mode so a switch to Auto
    /// starts from settled ones.
    level: [f64; MAX_CAPTURE_CH],
    floor: [f64; MAX_CAPTURE_CH],
    peak: [f64; MAX_CAPTURE_CH],
    evidence: [f64; MAX_CAPTURE_CH],
    quiet: [u32; MAX_CAPTURE_CH],
    since_dip: [u32; MAX_CAPTURE_CH],
    run: [f32; MAX_CAPTURE_CH],
    /// Which inputs led this frame.
    leading: [bool; MAX_CAPTURE_CH],
    /// Slow powers and cross power of inputs 1 and 2, and whether the even blend flips input 2.
    power: [f64; 2],
    cross: f64,
    flip: bool,
    /// Per input, frames it has been out of use.
    out_of_use: [u32; MAX_CAPTURE_CH],
    /// Auto's pick; frames since it last changed; frames an even blend for a newcomer still holds;
    /// whether the picked input has dipped since it was picked, and come back from it (see
    /// [`PICKED_DIP_HOLD_S`]).
    picked: Pick,
    since_change: u32,
    even_hold: u32,
    dipped_since_pick: bool,
    proven: bool,
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
        let coef = |secs: f64| 1.0 - (-1.0 / (secs * rate)).exp();
        let frames = |secs: f64| (secs * rate) as f32;
        InputChannelMix {
            level_coef: coef(LEVEL_TAU_S),
            floor_coef: coef(FLOOR_RISE_S),
            picked_floor_coef: coef(PICKED_FLOOR_RISE_S),
            evidence_coef: coef(EVIDENCE_S),
            peak_coef: coef(PEAK_FALL_S),
            corr_coef: coef(CORR_TAU_S),
            glide: coef(WEIGHT_GLIDE_S) as f32,
            warmup: frames(FLOOR_WARMUP_S) as u32,
            dip_hold: frames(PICKED_DIP_HOLD_S) as u32,
            sounding_hold: frames(SOUNDING_HOLD_S) as u32,
            dwell: frames(PICK_DWELL_S) as u32,
            hangover: frames(HANGOVER_S),
            run_min: frames(RUN_MIN_S),
            join_run: frames(JOIN_RUN_S),
            level: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            floor: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            peak: [LEVEL_FLOOR; MAX_CAPTURE_CH],
            evidence: [0.0; MAX_CAPTURE_CH],
            quiet: [u32::MAX; MAX_CAPTURE_CH],
            since_dip: [u32::MAX; MAX_CAPTURE_CH],
            run: [0.0; MAX_CAPTURE_CH],
            leading: [false; MAX_CAPTURE_CH],
            power: [LEVEL_FLOOR; 2],
            cross: 0.0,
            flip: false,
            picked: Pick::Even,
            since_change: u32::MAX,
            out_of_use: [0; MAX_CAPTURE_CH],
            even_hold: 0,
            dipped_since_pick: false,
            proven: false,
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
        self.track(&x[..ch]);
        let auto = self.auto_pick(ch);
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
    /// correlation; sets which channels lead.
    fn track(&mut self, x: &[f32]) {
        let ch = x.len();
        let warming = self.warmup > 0;
        self.warmup = self.warmup.saturating_sub(1);
        let mut voiced = [false; MAX_CAPTURE_CH];
        for (c, &s) in x.iter().enumerate() {
            let s = f64::from(s);
            let level = &mut self.level[c];
            *level += self.level_coef * (s * s + LEVEL_FLOOR - *level);
            let peak = &mut self.peak[c];
            *peak = if *level > *peak {
                *level
            } else {
                *peak + self.peak_coef * (*level - *peak)
            };
            let floor = &mut self.floor[c];
            let picked = self.picked == Pick::One(c);
            if warming || *level < *floor {
                *floor = *level;
            } else if !(picked && self.proven && self.since_dip[c] < self.dip_hold) {
                let blended_in = self.even_hold > 0 && self.run[c] >= self.run_min;
                let rise = if picked || blended_in {
                    self.picked_floor_coef
                } else {
                    self.floor_coef
                };
                *floor += rise * (*level - *floor);
            }
            voiced[c] = *level > VOICED_MIN && *level > VOICED_OVER_FLOOR * *floor;
            if *level < DIP_OVER_FLOOR * *floor {
                self.since_dip[c] = 0;
                if picked {
                    self.dipped_since_pick = true;
                }
            } else {
                // Back from a dip taken while picked — not the silence its onset rose from, which
                // a hum has too: the input dips the way a voice does.
                if voiced[c] && picked && self.dipped_since_pick {
                    self.proven = true;
                }
                self.since_dip[c] = self.since_dip[c].saturating_add(1);
            }
        }
        // The loudest input sounding: a voiced one, or the picked one while it stands clear of its
        // floor (a held note).
        let mut sounding = voiced;
        if let Pick::One(p) = self.picked {
            if p < ch
                && self.level[p] > DIP_OVER_FLOOR * self.floor[p]
                && self.since_dip[p] < self.sounding_hold
            {
                sounding[p] = true;
            }
        }
        let loudest = (0..ch)
            .filter(|&c| sounding[c])
            .map(|c| self.level[c])
            .fold(0.0f64, f64::max);
        // A held note leads only while no input that led within its hangover peaked above it:
        // merely standing clear of its floor kept a picked hum leading in every consonant gap of
        // a louder voice — or beside a quiet one — its use and evidence renewed for as long as it
        // sounded.
        let held = match self.picked {
            Pick::One(p) if sounding[p] && !voiced[p] => (0..ch)
                .all(|b| {
                    b == p || self.quiet[b] as f32 >= self.hangover || self.peak[b] <= self.level[p]
                })
                .then_some(p),
            _ => None,
        };
        for (c, &voice) in voiced[..ch].iter().enumerate() {
            let lead = (voice || held == Some(c)) && self.level[c] * LEAD_WITHIN >= loudest;
            self.leading[c] = lead;
            let hit = if lead { 1.0 } else { 0.0 };
            self.evidence[c] += self.evidence_coef * (hit - self.evidence[c]);
            // Snapped below any meaning: decaying on, it went subnormal within the hour, slow where
            // nothing flushes those (the cpal input path).
            if self.evidence[c] < 1.0e-30 {
                self.evidence[c] = 0.0;
            }
            if lead {
                self.quiet[c] = 0;
                self.run[c] = (self.run[c] + 1.0).min(2.0 * self.join_run);
            } else {
                self.quiet[c] = self.quiet[c].saturating_add(1);
                self.run[c] = (self.run[c] - 0.5).max(0.0);
            }
        }
        for (p, &s) in self.power.iter_mut().zip(x) {
            let s = f64::from(s);
            *p += self.corr_coef * (s * s + LEVEL_FLOOR - *p);
        }
        self.cross += self.corr_coef * (f64::from(x[0]) * f64::from(x[1]) - self.cross);
        if self.cross.abs() < 1.0e-30 {
            self.cross = 0.0;
        }
        // Judged whenever both carry signal, voiced or not: a legato pair stops being voiced
        // mid-note, and the flip went stale with it.
        if self.level[0] > VOICED_MIN && self.level[1] > VOICED_MIN {
            let corr = self.cross / (self.power[0] * self.power[1]).sqrt();
            if corr < FLIP_BELOW {
                self.flip = true;
            } else if corr > UNFLIP_ABOVE {
                self.flip = false;
            }
        }
        self.since_change = self.since_change.saturating_add(1);
    }

    /// Whether channel `c` is in use: it led within its hangover, which shrinks with its evidence,
    /// and no leading input outclasses it.
    fn in_use(&self, c: usize) -> bool {
        let scale = (self.evidence[c] / 0.5).clamp(0.2, 1.0) as f32;
        let outclassed = self.level[c] > DIP_OVER_FLOOR * self.floor[c]
            && (0..self.level.len())
                .any(|b| b != c && self.leading[b] && self.level[b] > OUTCLASSED_BY * self.peak[c]);
        !outclassed && (self.quiet[c] as f32) < self.hangover * scale
    }

    fn set_pick(&mut self, pick: Pick) {
        self.picked = pick;
        self.since_change = 0;
        self.even_hold = 0;
        self.dipped_since_pick = false;
        self.proven = false;
    }

    /// Advance Auto's pick by one frame (see the type doc); returns what Auto mixes this frame.
    fn auto_pick(&mut self, ch: usize) -> Pick {
        for c in 0..ch {
            self.out_of_use[c] = if self.in_use(c) {
                0
            } else {
                self.out_of_use[c].saturating_add(1)
            };
        }
        let e = self.evidence;
        let e = &e[..ch];
        let strongest = (0..ch).max_by(|&a, &b| e[a].total_cmp(&e[b])).unwrap_or(0);
        let weakest = e.iter().copied().fold(f64::INFINITY, f64::min);
        let ratio = if e[strongest] > 0.0 {
            weakest / e[strongest]
        } else {
            1.0
        };
        let dwell = self.dwell;
        // An input leading on beside one out of use for a whole dwell: hand it the pick rather
        // than wait seconds for the evidence (a moved mic, a voice after a hum that took the pick
        // in a silence, one of two sources falling silent). Ahead of the blend, which stale
        // evidence of the silent one would otherwise trip first.
        let heir = |gone: usize| {
            (self.out_of_use[gone] >= dwell)
                .then(|| (0..ch).find(|&b| b != gone && self.run[b] >= self.run_min))
                .flatten()
        };
        if self.since_change >= self.dwell {
            match self.picked {
                Pick::Even => {
                    if let Some(b) = (0..ch).find_map(heir) {
                        self.set_pick(Pick::One(b));
                    } else if e[strongest] >= PICK_MIN && ratio <= BLEND_LEAVE {
                        self.set_pick(Pick::One(strongest));
                    }
                }
                Pick::One(a) => {
                    let margin = if self.in_use(a) {
                        SWITCH_MARGIN
                    } else {
                        SWITCH_MARGIN_QUIET
                    };
                    let all_in_use = (0..ch).all(|c| self.in_use(c));
                    if let Some(b) = heir(a) {
                        self.set_pick(Pick::One(b));
                    } else if all_in_use && ratio >= BLEND_ENTER && weakest >= BLEND_MIN {
                        self.set_pick(Pick::Even);
                    } else if strongest != a && e[strongest] >= e[a] + margin {
                        self.set_pick(Pick::One(strongest));
                    }
                }
            }
        }
        match self.picked {
            Pick::One(a) => {
                let newcomer = (0..ch)
                    .filter(|&b| b != a)
                    .map(|b| self.run[b])
                    .fold(0.0f32, f32::max);
                let need = if self.in_use(a) {
                    self.join_run
                } else {
                    self.run_min
                };
                // Held for a dwell once it starts: a newcomer's leaky run hovering about the line
                // toggled the blend on and off with every strum. Dropped once no newcomer has led
                // for [`RUN_MIN_S`]: a hum blending in just as it stopped counting stayed in the
                // take for the whole dwell.
                if newcomer >= need {
                    self.even_hold = self.dwell;
                } else if (0..ch).all(|b| b == a || self.quiet[b] as f32 >= self.run_min) {
                    self.even_hold = 0;
                } else {
                    self.even_hold = self.even_hold.saturating_sub(1);
                }
                if self.even_hold > 0 {
                    Pick::Even
                } else {
                    Pick::One(a)
                }
            }
            Pick::Even => Pick::Even,
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
            Singer::phrased(len, from, to, rms_db, f0, seed, 3.0, false)
        }

        /// A singer in `phrase`-second phrases, legato (one note each, no consonant gaps) or not.
        #[allow(clippy::too_many_arguments)]
        fn phrased(
            len: usize,
            from: f64,
            to: f64,
            rms_db: f64,
            f0: f64,
            seed: u32,
            phrase: f64,
            legato: bool,
        ) -> Singer {
            let mut rng = Floor(seed);
            let mut unit = || 0.5 + rng.next() as f64 / 1.1e-4; // 0..1
            let mut env = vec![0.0f32; len];
            let mut t = from;
            while t < to {
                let phrase_end = (t + phrase).min(to);
                let mut s = t;
                while s < phrase_end {
                    let end = if legato {
                        phrase_end
                    } else {
                        (s + 0.22 + 0.12 * unit()).min(phrase_end)
                    };
                    let level = 10f64.powf((-3.0 + 6.0 * unit()) / 20.0) as f32;
                    let (a, b) = (at(s).min(len), at(end).min(len));
                    let ramp = at(0.008) as f32;
                    for (k, e) in env[a..b].iter_mut().enumerate() {
                        let edge = (k.min(b - a - 1 - k)) as f32 / ramp;
                        *e = level * edge.min(1.0);
                    }
                    s = end + 0.04 + 0.05 * unit();
                }
                t = phrase_end + if legato { 1.0 } else { 0.45 };
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

    /// A legato voice after a hum that took the pick in a silence gets the take within seconds,
    /// however soon or late after the hum it starts: the hum's held level kept it leading for as
    /// long as it stood clear of its floor, while the voice, its floor rising unpicked, stopped
    /// counting a second into each note — cut or blended for up to 24 s.
    #[test]
    fn a_legato_voice_after_a_hum_takes_the_pick() {
        for (hum_db, voice_db, delay) in [
            (-50.0f64, -20.0, 1.0),
            (-50.0, -20.0, 3.0),
            (-50.0, -20.0, 12.0),
            (-45.0, -35.0, 3.0),
        ] {
            let start = 4.0 + delay;
            let secs = start + 20.0;
            let mut singer = Singer::phrased(at(secs), start, secs, voice_db, 196.0, 3, 6.0, true);
            let (mut floor, mut room) = (Floor(5), Floor(6));
            let hum = 10f64.powf(hum_db / 20.0);
            let (out, want) = auto_run(secs, |n| {
                let h = if n >= at(4.0) {
                    sine(50.0, hum, n)
                } else {
                    0.0
                };
                (
                    [floor.next() + h, singer.at(n) + 0.25 * room.next()],
                    singer.at_last(),
                )
            });
            let off = off_over(&out, &want, start + 3.0..secs);
            assert!(
                off <= -30.0,
                "hum {hum_db}, voice {voice_db} {delay} s later: off by {off:.1} dB"
            );
        }
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

    /// Nothing Auto keeps decays into subnormals, slow where nothing flushes those (the cpal input
    /// path): an input's evidence did within the hour after it last led, and so did the pair's
    /// cross power.
    #[test]
    fn decayed_state_never_goes_subnormal() {
        let mut mix = InputChannelMix::new(SR);
        // Just above the smallest normal f64: a second of decay takes either below it.
        mix.evidence.fill(2.5e-308);
        mix.cross = 2.5e-308;
        for _ in 0..at(2.0) {
            mix.mix(INPUT_CHANNEL_AUTO, &[0.0, 0.0]);
        }
        for v in mix.evidence[..2].iter().chain([&mix.cross]) {
            assert!(*v == 0.0 || v.is_normal(), "{v:e} is subnormal");
        }
    }

    /// Auto follows a mic moved to the other input, and never cuts it once it sings there: an even
    /// blend for a second or so, then that input alone — the stale evidence of the input it left
    /// held the blend, 6 dB down, for four seconds.
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
        let off = off_over(&out, &want, 14.0..secs);
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

    /// Legato phrases keep their pick over a talkback on the other input: a note held for
    /// seconds stopped counting once its floor rose to meet it, and anything intermittent there,
    /// even at -55 dBFS, took the take.
    #[test]
    fn legato_phrases_keep_their_pick_over_a_talkback() {
        let secs = 40.0;
        let mut singer = Singer::phrased(at(secs), 1.0, secs, -20.0, 220.0, 3, 8.0, true);
        let mut talk = Singer::phrased(at(secs), 1.7, secs, -45.0, 120.0, 4, 2.0, false);
        let (mut room1, mut room2) = (Floor(5), Floor(6));
        let (out, want) = auto_run(secs, |n| {
            let (v, t) = (singer.at(n), talk.at(n));
            ([t + 0.25 * room1.next(), v + 0.25 * room2.next()], v)
        });
        let worst = min_gain_over(&out, &want, 3.0..secs);
        assert!(worst >= 0.9, "the singer fell to {worst:.2}");
    }

    /// A second mic hearing the singer from across the room, or crosstalk onto the other input,
    /// never holds the take in an even blend: counted at any level, it put the voice 6 dB down
    /// with that mic's room and a comb filter mixed in, all session.
    #[test]
    fn a_mic_hearing_the_singer_from_afar_is_left_out() {
        for (bleed_db, delay) in [(-18.0f64, 96usize), (-50.0, 0)] {
            let secs = 20.0;
            let mut singer = Singer::new(at(secs), 1.0, secs, -12.0, 220.0, 3);
            let mut room = Floor(5);
            let mut past = vec![0.0f32; 128];
            let gain = 10f64.powf(bleed_db / 20.0) as f32;
            let (out, want) = auto_run(secs, |n| {
                let v = singer.at(n);
                past[n % 128] = v;
                let far = gain * past[(n + 128 - delay) % 128];
                ([far + 4.0 * room.next(), v], v)
            });
            let off = off_over(&out, &want, 3.0..secs);
            assert!(off <= -40.0, "{bleed_db} dB away: off by {off:.1} dB");
        }
    }

    /// Two sources trading the lead hold the pick still: blending and unblending on every strum
    /// flipped it — and the echo path the canceller had settled on — twice a second.
    #[test]
    fn two_sources_do_not_flap_the_pick() {
        let secs = 30.0;
        let mut singer = Singer::new(at(secs), 1.0, secs, -20.0, 220.0, 3);
        let mut mix = InputChannelMix::new(SR);
        let (mut changes, mut last) = (0, mix.picked);
        for n in 0..at(secs) {
            let strum = (n % at(0.5)) as f64 / SR;
            let guitar = sine(147.0, 0.14 * (-strum / 0.25).exp(), n);
            mix.mix(INPUT_CHANNEL_AUTO, &[guitar, singer.at(n)]);
            if mix.picked != last {
                changes += 1;
                last = mix.picked;
            }
        }
        assert!(changes <= 3, "{changes} pick changes in {secs} s");
    }

    /// A hum appearing on the idle input early in the singing is left out of the take — counted
    /// for its first second, it reached the blend threshold and joined the take mid-phrase.
    #[test]
    fn a_hum_appearing_early_in_the_singing_is_left_out() {
        let secs = 14.0;
        let mut singer = Singer::new(at(secs), 2.0, secs, -20.0, 196.0, 3);
        let mut floor = Floor(5);
        let mut room = Floor(6);
        let (out, want) = auto_run(secs, |n| {
            let hum = if n >= at(3.0) {
                sine(50.0, 0.0032, n)
            } else {
                0.0
            }; // -50 dBFS
            (
                [floor.next() + hum, singer.at(n) + 0.25 * room.next()],
                singer.at_last(),
            )
        });
        let off = off_over(&out, &want, 3.0..secs);
        assert!(off <= -40.0, "the hum reached the take at {off:.1} dB");
    }

    /// A legato stereo pair in opposite polarity stays flipped through its notes: judged only
    /// while both inputs counted as voiced, the flip went stale once a held note stopped counting.
    #[test]
    fn a_legato_pair_in_opposite_polarity_stays_flipped() {
        let secs = 20.0;
        let mut singer = Singer::phrased(at(secs), 1.0, secs, -20.0, 220.0, 3, 6.0, true);
        let (out, want) = auto_run(secs, |n| {
            let v = singer.at(n);
            ([-v, v], v)
        });
        let worst = min_gain_over(&out, &want, 2.0..secs).abs();
        assert!(worst >= 0.95, "the pair cancelled to {worst:.2}");
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
