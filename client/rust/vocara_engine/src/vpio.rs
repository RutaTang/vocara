//! macOS VoiceProcessingIO (VPIO) AudioUnit: a single I/O unit that captures the mic (input
//! element 1) and — in duplex mode — renders the engine mix to the speaker (output element 0).
//! Running both in one unit unlocks Apple's built-in Acoustic Echo Cancellation: the playback we
//! push through element 0 is the reference VPIO subtracts from the mic, so the backing track does
//! not bleed into the recorded vocal (the karaoke goal). It also replaces the broken two-AUHAL
//! cpal in+out split, which fed the input stream silence on macOS.
//!
//! VPIO negotiates its own client stream format on initialize: the mic element is often delivered
//! as **mono, non-interleaved** float even when we request stereo interleaved. We therefore do NOT
//! assume a layout — we read back the effective ASBD after `AudioUnitInitialize` and make both
//! callbacks adapt (channel count, interleaving, rate). Assuming stereo-interleaved when VPIO
//! actually hands us mono made `feed` read consecutive mono samples as L/R pairs, halving the
//! frame count → the monitored voice played back an octave high (the "helium voice" bug); a
//! mismatched `AudioBufferList` made `AudioUnitRender` fail outright → zero captured frames.
//!
//! The `AudioComponentInstance` is pinned to the [`crate::device::AudioHost`] thread (never sent
//! across threads). The render callbacks run on CoreAudio's real-time thread and are RT-safe:
//! no alloc / lock / syscall (the one-shot setup `eprintln` diagnostics fire once, then never
//! again); a panic is contained by `catch_unwind`. macOS-only.
#![cfg(target_vendor = "apple")]

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use objc2_audio_toolbox::{
    kAUVoiceIOProperty_BypassVoiceProcessing, kAUVoiceIOProperty_VoiceProcessingEnableAGC,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioOutputUnitProperty_SetInputCallback, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output, kAudioUnitSubType_VoiceProcessingIO, kAudioUnitType_Output,
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioUnit, AudioUnitGetProperty,
    AudioUnitInitialize, AudioUnitRender, AudioUnitRenderActionFlags, AudioUnitSetProperty,
    AudioUnitUninitialize,
};
use objc2_audio_toolbox::{AudioOutputUnitStart, AudioOutputUnitStop};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, AudioBuffer, AudioBufferList, AudioStreamBasicDescription,
    AudioTimeStamp,
};

use crate::aec::Aec;
use crate::capture_layout::{
    downmix_to_mono, CaptureLayout, DeadCaptureStreak, InputChannelMix, MAX_CAPTURE_CH,
};
use crate::device::InputCell;
use crate::dsp::capture_cleanup::HighPass;
use crate::live::RtCell;
use crate::ring::{spsc, Consumer, Producer};
use crate::ring_sync::{baseline_cap, classify_duplex_rates, DuplexRateCheck, RefRingSync};
use crate::sample::{frames_at_engine_rate, CHANNELS, ENGINE_SR};

const VPIO_RATE: f64 = ENGINE_SR; // 48_000.0 — requested rate; verified against the effective ASBD.
const REQ_CH: u32 = CHANNELS as u32; // 2 — what we request; VPIO may negotiate fewer (e.g. mono).
/// Buffer-allocation ceiling: VPIO never delivers more than this many frames per callback.
const MAX_FRAMES: usize = 4096;
/// Requested max frames per VPIO slice. SMALL on purpose: a 4096-frame slice makes the speaker→mic
/// echo round-trip ~170 ms, far too long for the AEC to converge over (and a singer sings in time
/// with the backing, so an unconverged filter cancels the VOICE). Forcing small slices shrinks the
/// round-trip to ~20 ms so the adaptive filter + double-talk detector actually work.
const VPIO_MAX_SLICE: u32 = 512;
/// Max channels we provision buffers / scratch for (mic + speaker are mono or stereo).
const MAX_CH: usize = MAX_CAPTURE_CH;
/// Far-end reference ring capacity (mono samples). Comfortably spans several IO buffers of backlog.
const REF_RING_CAP: usize = 1 << 15; // 32768
/// Echo-path length our AEC models. Covers the macOS duplex round-trip (~170 ms) + reverb tail.
const AEC_TAIL_MS: f32 = 250.0;
/// The AEC's own algorithmic latency: one internal block (256 samples), added to the IO round-trip.
const AEC_BLOCK_FRAMES: u32 = 256;
/// Consecutive dead capture cycles (after the unit's first capture) before we flag the input device
/// as lost. At the ~10 ms VPIO slice that is ~200 ms of dead capture — a real disconnect, not a
/// one-off transient.
const INPUT_LOST_STREAK: u32 = 20;
/// How long a unit that has not delivered its first frame may fail or come back empty before it
/// is flagged lost: comfortably longer than a Bluetooth headset's switch to its headset profile,
/// so a warming-up mic is never a "disconnected" one (see [`DeadCaptureStreak`]). Deliberately
/// longer than a take arm's ~2 s ready check too: an arm whose mic never delivers is refused by
/// that check ("not delivering") before this runs out, and a shorter allowance would raise a
/// loss under a slow-but-healthy warm-up that the take's first poll then stopped. This covers a
/// unit opened with no arm waiting on it (a selected mic that came back mid-session).
const INPUT_WARMUP_SECS: f64 = 2.5;

/// The warm-up allowance in frames of the mic element at `rate` (what each dead cycle counts).
fn input_warmup_frames(rate: f64) -> u64 {
    (rate * INPUT_WARMUP_SECS) as u64
}

/// Estimated record round-trip latency (frames @ ENGINE_SR): output IO (buffer + safety + device
/// latency) + acoustic flight + input IO + the AEC block. The captured vocal lands this far behind
/// the engine playhead, so recorded takes are shifted earlier by it to sit on the beat the singer
/// heard. Written when the mic opens; read by the FFI. 0 until the mic has opened once.
pub(crate) static RECORD_LATENCY_FRAMES: AtomicU32 = AtomicU32::new(0);

/// Diagnostic: whether the current duplex session runs with the AEC BYPASSED because the input
/// element negotiated a non-engine rate (see [`DuplexRateCheck::BypassAec`]). Reset on every
/// (re)start. Mic capture and playback keep working in that mode; only echo cancellation is off,
/// so the backing track bleeds into speaker recordings exactly as it did before AEC existed.
pub(crate) static AEC_BYPASSED: AtomicBool = AtomicBool::new(false);

/// Default capture (record) gain (+12 dB). The built-in mic has no AGC and records quiet; +12 dB
/// lands a typical sung take around -14 dBFS with peaks near -6 — the tracking headroom pro
/// practice calls for — instead of the old +24 dB that pushed every loud note into a ceiling.
/// The capture path is deliberately STATIC and LINEAR (40 Hz high-pass + this pure scale; no
/// gate/expander/AGC, no soft knee — see `dsp::capture_cleanup`), so the boost also amplifies
/// inter-phrase mic hiss — accepted. Recording is 32-bit float, so even a fader pushed far past
/// this default stores over-0 dBFS peaks losslessly; playback is tamed by the master limiter.
/// User-adjustable at runtime via [`INPUT_GAIN`].
///
/// NOTE: the backing track is no longer auto-ducked while recording — the singer sets the backing
/// level themselves on the mixer. (Removed; the speaker now plays the backing at its mixer level
/// during a take, so the AEC reference and the take's bleed reflect exactly that.)
const DEFAULT_INPUT_GAIN: f32 = 3.981; // 10^(12/20)

/// Runtime record/input gain (linear bits), set from the mixer's mic-input fader.
pub(crate) static INPUT_GAIN: AtomicU32 = AtomicU32::new(DEFAULT_INPUT_GAIN.to_bits());

/// Set the record/input gain (linear).
pub(crate) fn set_input_gain(linear: f32) {
    INPUT_GAIN.store(linear.max(0.0).to_bits(), Ordering::Relaxed);
}

const ELEM_OUTPUT: u32 = 0; // speaker (AEC reference lives here)
const ELEM_INPUT: u32 = 1; // microphone

const VP_AGC_ENABLE_AGC: u32 = kAUVoiceIOProperty_VoiceProcessingEnableAGC;

/// Engine-rate stereo packed-float32 interleaved ASBD (what we *request*).
fn engine_asbd() -> AudioStreamBasicDescription {
    let bytes_per_frame = REQ_CH * 4;
    AudioStreamBasicDescription {
        mSampleRate: VPIO_RATE,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: REQ_CH,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

/// Effective layout of one element, read back from the unit after initialize. The output
/// callback follows the buffer list VPIO hands it; the input callback BUILDS the list it renders
/// into, so it needs the negotiated interleaving too (see [`CaptureLayout`]).
#[derive(Clone, Copy)]
struct Layout {
    channels: usize,
    rate: f64,
    /// `kAudioFormatFlagIsNonInterleaved` was NOT set: one buffer of interleaved frames.
    interleaved: bool,
}

impl Layout {
    fn from(asbd: &AudioStreamBasicDescription) -> Layout {
        Layout {
            channels: (asbd.mChannelsPerFrame as usize).max(1),
            rate: if asbd.mSampleRate > 0.0 {
                asbd.mSampleRate
            } else {
                VPIO_RATE
            },
            interleaved: asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0,
        }
    }

    fn capture(&self) -> CaptureLayout {
        CaptureLayout::new(self.channels, !self.interleaved)
    }
}

/// A multi-buffer `AudioBufferList` (the C type is flexibly-sized; Rust needs an explicit array).
/// Layout-compatible prefix with `AudioBufferList`, so a `*mut AblN` casts to `*mut AudioBufferList`.
#[repr(C)]
struct AblN {
    m_number_buffers: u32,
    m_buffers: [AudioBuffer; MAX_CH],
}

fn zero_buffer() -> AudioBuffer {
    AudioBuffer {
        mNumberChannels: 0,
        mDataByteSize: 0,
        mData: std::ptr::null_mut(),
    }
}

/// Input-callback context: pulls the captured + echo-cancelled mic frames and forwards them to
/// `InputCell` as interleaved f32. Owns the lent descriptor + an interleave scratch (no RT alloc).
struct InCtx {
    input: Arc<InputCell>,
    /// The input-stream generation this unit was built with. The cell denies a feed once a newer
    /// stream has claimed it, which is how a callback surviving a failed `AudioOutputUnitStop`
    /// stops racing its replacement (see [`crate::live::RtCell`]).
    epoch: u64,
    unit: AudioUnit,
    abl: AblN,
    /// Buffer we SUPPLY to `AudioUnitRender` (VPIO copies the captured mic into it). Supplying our
    /// own buffer is the robust AUHAL input pattern; the lend pattern (NULL mData) returned success
    /// here but never set mData, so `feed` never ran. Non-interleaved captures use one
    /// `MAX_FRAMES` plane of it per channel.
    render_buf: Vec<f32>,
    /// The clean mono voice handed to `InputCell` (`MAX_FRAMES*MAX_CH`).
    scratch: Vec<f32>,
    /// Custom acoustic echo canceller: removes the backing-track echo (loudspeaker→mic) from the raw
    /// mic while preserving the singing voice. VPIO's own AEC is bypassed (it ducked the sung voice).
    aec: Aec,
    /// Far-end reference (the rendered backing, mono) produced by `output_cb`.
    ref_cons: Consumer<f32>,
    /// Keeps the reference ring's depth healthy across input glitches (see `ring_sync.rs`):
    /// a backlog left by missed pops would delay the reference past the AEC's modeled
    /// round-trip and the filter could never re-converge.
    ref_sync: RefRingSync,
    /// AEC bypass (set once before start, read-only on the RT callback): the input element
    /// negotiated a non-engine rate, so far-end pairing is impossible (see
    /// [`DuplexRateCheck::BypassAec`]). The mic passes through the same high-pass + gain path;
    /// all far-end ring/`ref_sync` work is skipped (`output_cb` leaves the ring unpumped).
    aec_bypass: bool,
    /// The mic's channels to the ONE mono signal the AEC and everything after it hear, under the
    /// input cell's channel mode (`ve_set_input_channel`). Built for the negotiated rate.
    mix: InputChannelMix,
    /// Mono AEC scratch (mic-in / far-end / clean-out), `MAX_FRAMES` each.
    mic_mono: Vec<f32>,
    far_mono: Vec<f32>,
    clean_mono: Vec<f32>,
    /// Post-AEC 40 Hz high-pass (DC/rumble removal), applied before the record gain. The only
    /// processing between the AEC and the fan-out besides the gain — static on purpose.
    hp: HighPass,
    layout: Layout,
    /// Shared device-event flags (polled by the host via `ve_poll_device_event`). We raise
    /// EVENT_INPUT_LOST on a sustained render-failure streak so a mid-session mic disconnect is
    /// observable, matching the cpal `err_cb` path (the VPIO unit itself has no error callback).
    event: Arc<AtomicU32>,
    /// Dead capture cycles toward EVENT_INPUT_LOST: a warm-up allowance before the unit's first
    /// capture, the streak rule after it (see [`DeadCaptureStreak`]).
    dead: DeadCaptureStreak,
}

// SAFETY: built/moved on the AudioHost thread before the unit starts; afterwards only the input
// callback thread touches it.
unsafe impl Send for InCtx {}

/// Output-callback context (duplex only): renders the engine graph into VPIO's output buffer (the
/// AEC reference). Owns an interleaved render scratch it then writes into VPIO's real layout.
struct OutCtx {
    rt: Arc<RtCell>,
    /// The output-stream generation this unit was built with; see [`InCtx::epoch`].
    epoch: u64,
    /// Interleaved stereo render scratch (`MAX_FRAMES * CHANNELS`).
    scratch: Vec<f32>,
    /// Mono downmix of the rendered backing, pushed to the AEC far-end ring (the echo reference).
    ref_prod: Producer<f32>,
    /// Whether to pump `ref_prod` (set once before start, read-only on the RT callback).
    /// `false` when the AEC is bypassed: the input side never pops then, so pushing would
    /// only grow the ring to capacity for nothing.
    push_ref: bool,
    layout: Layout,
}

// SAFETY: built/moved on the AudioHost thread before the unit starts; afterwards only the output
// callback thread touches it.
unsafe impl Send for OutCtx {}

/// A live VoiceProcessingIO unit. Drop stops + disposes it (RAII), mirroring `DeviceHandle`.
pub struct Vpio {
    unit: AudioUnit,
    /// Callback refcons: the callbacks hold their raw addresses, so these must outlive the
    /// unit. `Option` so a failed stop can leak them instead of freeing memory a surviving
    /// callback still dereferences (see [`Drop`]).
    _in: Option<Box<InCtx>>,
    _out: Option<Box<OutCtx>>,
    started: bool,
}

// SAFETY: constructed, started, stopped and dropped only on the single AudioHost thread.
unsafe impl Send for Vpio {}

impl Vpio {
    /// Create + initialize + start a VPIO unit feeding `input` (mic) and, when `with_output`,
    /// rendering `rt` to the speaker through element 0 (enables AEC). `device_uid` selects the
    /// input device: None = system default; a UID that cannot be resolved or bound is an ERROR,
    /// not a silent fallback — the caller (`device.rs::open_mic`) owns retrying on the default and
    /// telling the user. MUST run on the AudioHost thread.
    pub fn start(
        device_uid: Option<&str>,
        input: Arc<InputCell>,
        rt: Arc<RtCell>,
        with_output: bool,
        event: Arc<AtomicU32>,
    ) -> Result<Vpio, String> {
        // Resolve the selected input device BEFORE building anything. An unresolvable UID must
        // FAIL the open: the host only learns of a fallback when `start` returns Err (device.rs
        // `open_mic` → `or_else`), and only then does it publish an empty actual-device and raise
        // `EVENT_INPUT_FELL_BACK`. Proceeding on the system default instead would record the wrong
        // mic while the UI still names the selected one. Matches the non-Apple path, which already
        // errors with "input device {uid} not found".
        let bound_dev = match device_uid {
            Some(uid) => match device_id_for_uid(uid) {
                Some(id) => Some(id),
                None => return Err(format!("input device {uid} not found")),
            },
            None => None,
        };
        unsafe {
            let mut desc = AudioComponentDescription {
                componentType: kAudioUnitType_Output,
                componentSubType: kAudioUnitSubType_VoiceProcessingIO,
                componentManufacturer: kAudioUnitManufacturer_Apple,
                componentFlags: 0,
                componentFlagsMask: 0,
            };
            let comp = AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&mut desc));
            if comp.is_null() {
                return Err("VPIO component not found".into());
            }

            let mut unit: AudioUnit = std::ptr::null_mut();
            let st = AudioComponentInstanceNew(comp, NonNull::from(&mut unit));
            if st != 0 || unit.is_null() {
                return Err(format!("AudioComponentInstanceNew failed: {st}"));
            }
            let bail = |unit: AudioUnit, msg: String| -> Result<Vpio, String> {
                AudioComponentInstanceDispose(unit);
                Err(msg)
            };

            // Shrink the IO buffer BEFORE initialize (low latency + keeps the echo in AEC range),
            // and publish the record round-trip latency so recorded takes can be time-aligned.
            // Measure on the device the VPIO unit will actually bind to (`device_uid` below), not
            // the system defaults — a non-default mic has its own buffer/latency/safety offsets.
            // The IO-only figure (no AEC block) also bounds the ref-ring baseline below: an
            // enshrined ring depth past it would make the echo non-causal (AEC dead for the
            // session), so the baseline cap is derived from it, not from a buffer-size constant.
            let io_round_trip = tune_io_and_round_trip(device_uid);
            RECORD_LATENCY_FRAMES.store(io_round_trip + AEC_BLOCK_FRAMES, Ordering::Relaxed);
            AEC_BYPASSED.store(false, Ordering::Relaxed);

            let on: u32 = 1;
            let off: u32 = 0;
            // Enable input element (1); enable/disable output element (0) per mode.
            if let Err(e) = set_u32(
                unit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Input,
                ELEM_INPUT,
                on,
            ) {
                return bail(unit, e);
            }
            let out_flag = if with_output { on } else { off };
            if let Err(e) = set_u32(
                unit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Output,
                ELEM_OUTPUT,
                out_flag,
            ) {
                return bail(unit, e);
            }

            // Bind the selected input device. A REJECTED bind fails the open for the same reason
            // an unresolvable UID does (see `bound_dev` above): the caller owns the default-device
            // fallback and the user-facing notice, and a swallowed failure here captures from
            // another device entirely with nothing anywhere reporting it. Kept AFTER EnableIO and
            // BEFORE the `set_asbd` calls — setting CurrentDevice resets the stream formats.
            if let Some(dev_id) = bound_dev {
                if let Err(e) = set_u32(
                    unit,
                    kAudioOutputUnitProperty_CurrentDevice,
                    kAudioUnitScope_Global,
                    0,
                    dev_id,
                ) {
                    return bail(unit, e);
                }
            }

            // Request engine-rate stereo f32. VPIO may renegotiate (mono / non-interleaved); we read
            // the effective format back after initialize and adapt the callbacks to it.
            // Input: scope Output of element 1 (the format VPIO delivers to us).
            // Output: scope Input of element 0 (the format we deliver to VPIO).
            let asbd = engine_asbd();
            let _ = set_asbd(unit, kAudioUnitScope_Output, ELEM_INPUT, &asbd);
            if with_output {
                let _ = set_asbd(unit, kAudioUnitScope_Input, ELEM_OUTPUT, &asbd);
            }

            let _ = set_u32(
                unit,
                kAudioUnitProperty_MaximumFramesPerSlice,
                kAudioUnitScope_Global,
                0,
                VPIO_MAX_SLICE,
            );

            // BYPASS VPIO's voice processing (Bypass = 1): its AEC/NS is voice-call-tuned and ducked
            // the sung voice during the continuous double-talk of singing over music. We take the RAW
            // mic and run our own echo canceller (crate::aec) instead. AGC off too (moot under bypass).
            let _ = set_u32(
                unit,
                kAUVoiceIOProperty_BypassVoiceProcessing,
                kAudioUnitScope_Global,
                0,
                on,
            );
            let _ = set_u32(unit, VP_AGC_ENABLE_AGC, kAudioUnitScope_Global, 0, off);

            // Far-end reference ring (rendered backing, mono) + our AEC. `output_cb` produces the
            // reference; `input_cb` cancels it from the raw mic.
            let (ref_prod, ref_cons) = spsc::<f32>(REF_RING_CAP);
            let aec = Aec::new(VPIO_RATE as f32, AEC_TAIL_MS);

            // Pinned contexts (their addresses are the callback refcons). Layouts are placeholders
            // until we read the negotiated formats after initialize (no callback fires before start).
            let placeholder = Layout {
                channels: REQ_CH as usize,
                rate: VPIO_RATE,
                interleaved: true,
            };
            // Retire every older callback before this unit can deliver or render a frame.
            let in_epoch = input.claim();
            let out_epoch = rt.claim();
            let mut in_ctx = Box::new(InCtx {
                input,
                epoch: in_epoch,
                unit,
                abl: AblN {
                    m_number_buffers: 1,
                    m_buffers: [zero_buffer(), zero_buffer()],
                },
                render_buf: vec![0.0; MAX_FRAMES * MAX_CH],
                scratch: vec![0.0; MAX_FRAMES * MAX_CH],
                aec,
                ref_cons,
                ref_sync: RefRingSync::new(baseline_cap(io_round_trip, MAX_FRAMES)),
                aec_bypass: false,
                mix: InputChannelMix::new(VPIO_RATE),
                mic_mono: vec![0.0; MAX_FRAMES],
                far_mono: vec![0.0; MAX_FRAMES],
                clean_mono: vec![0.0; MAX_FRAMES],
                hp: HighPass::new(),
                layout: placeholder,
                event,
                dead: DeadCaptureStreak::new(INPUT_LOST_STREAK, input_warmup_frames(VPIO_RATE)),
            });
            let in_ptr = (&mut *in_ctx as *mut InCtx) as *mut c_void;

            let mut out_ctx = if with_output {
                Some(Box::new(OutCtx {
                    rt,
                    epoch: out_epoch,
                    scratch: vec![0.0; MAX_FRAMES * CHANNELS],
                    ref_prod,
                    push_ref: true,
                    layout: placeholder,
                }))
            } else {
                None
            };

            // Input callback: fires after VPIO captured + echo-cancelled the mic.
            let in_cb = AURenderCallbackStruct {
                inputProc: Some(input_cb),
                inputProcRefCon: in_ptr,
            };
            if let Err(e) = set_struct(
                unit,
                kAudioOutputUnitProperty_SetInputCallback,
                kAudioUnitScope_Global,
                0,
                &in_cb,
            ) {
                return bail(unit, e);
            }

            // Output render callback (duplex only): supplies the engine mix as the AEC reference.
            if let Some(oc) = out_ctx.as_mut() {
                let out_ptr = (&mut **oc as *mut OutCtx) as *mut c_void;
                let out_cb = AURenderCallbackStruct {
                    inputProc: Some(output_cb),
                    inputProcRefCon: out_ptr,
                };
                if let Err(e) = set_struct(
                    unit,
                    kAudioUnitProperty_SetRenderCallback,
                    kAudioUnitScope_Input,
                    ELEM_OUTPUT,
                    &out_cb,
                ) {
                    return bail(unit, e);
                }
            }

            let st = AudioUnitInitialize(unit);
            if st != 0 {
                return bail(unit, format!("AudioUnitInitialize failed: {st}"));
            }

            // Read the negotiated formats and adapt. The input layout also configures InputCell's
            // downmix/resample so the captured rate + channel count are honored (fixes helium voice).
            let in_layout = get_asbd(unit, kAudioUnitScope_Output, ELEM_INPUT)
                .map(|a| Layout::from(&a))
                .unwrap_or(placeholder);
            in_ctx.layout = in_layout;
            in_ctx.mix = InputChannelMix::new(in_layout.rate);
            // Dead cycles count the mic element's own frames, at the rate it negotiated.
            in_ctx.dead =
                DeadCaptureStreak::new(INPUT_LOST_STREAK, input_warmup_frames(in_layout.rate));
            // input_cb always hands InputCell the clean MONO voice (its own mix has already taken
            // the device's channels down), so configure for one channel at the captured rate.
            // Reconfigure retires every older generation and mints the one bound to this format —
            // adopt it, or this unit's own callbacks would be denied as zombies. No callback has
            // fired yet (start below).
            in_ctx.epoch = in_ctx.input.reconfigure(in_layout.rate, 1);
            if let Some(oc) = out_ctx.as_mut() {
                let out_layout = get_asbd(unit, kAudioUnitScope_Input, ELEM_OUTPUT)
                    .map(|a| Layout::from(&a))
                    .unwrap_or(placeholder);
                oc.layout = out_layout;
                // Duplex sanity: the far-end ring is pushed at the output element's rate and
                // popped at the input element's, and `output_cb` writes engine-rate samples
                // without resampling — so the AEC needs both negotiated rates at VPIO_RATE.
                match classify_duplex_rates(in_layout.rate, out_layout.rate, VPIO_RATE) {
                    DuplexRateCheck::AecOn => {}
                    DuplexRateCheck::BypassAec => {
                        // The mic element negotiated VPIO's internal voice rate: far-end
                        // pairing is impossible (the ring would drift without bound), but
                        // capture itself is fine — `reconfigure` above already set the
                        // resampler to the negotiated rate. Run the duplex with the AEC
                        // BYPASSED instead of failing the whole mic: a working mic with
                        // echo bleed always beats no mic at all. One-shot setup log
                        // (host thread, not the RT callback).
                        in_ctx.aec_bypass = true;
                        oc.push_ref = false; // leave the ref ring unpumped so it can't grow
                                             // Without the AEC in the capture path its block latency no longer
                                             // applies to recorded takes.
                        RECORD_LATENCY_FRAMES.store(io_round_trip, Ordering::Relaxed);
                        AEC_BYPASSED.store(true, Ordering::Relaxed);
                        // Tell the UI, not just stderr: the user should know
                        // speaker playback may bleed into the take.
                        in_ctx
                            .event
                            .fetch_or(crate::device::EVENT_AEC_BYPASSED, Ordering::Release);
                        eprintln!(
                            "vocara: VPIO negotiated input {} Hz vs output {} Hz — running WITHOUT echo cancellation (mic + playback intact; expect backing bleed on speakers)",
                            in_layout.rate, out_layout.rate
                        );
                    }
                    DuplexRateCheck::Refuse => {
                        // A non-engine OUTPUT rate corrupts playback itself (pitched audio),
                        // which bypassing the AEC cannot save. Refuse; the caller falls back
                        // (default input, then a plain cpal output without a mic).
                        AudioUnitUninitialize(unit);
                        return bail(
                            unit,
                            format!(
                                "VPIO negotiated an unusable output rate: input {} Hz, output {} Hz, expected {} Hz",
                                in_layout.rate, out_layout.rate, VPIO_RATE
                            ),
                        );
                    }
                }
            }

            let st = AudioOutputUnitStart(unit);
            if st != 0 {
                AudioUnitUninitialize(unit);
                return bail(unit, format!("AudioOutputUnitStart failed: {st}"));
            }

            Ok(Vpio {
                unit,
                _in: Some(in_ctx),
                _out: out_ctx,
                started: true,
            })
        }
    }

    /// Stop + uninitialize + dispose. Idempotent. MUST run on the AudioHost thread.
    ///
    /// Returns false when the stop FAILED, i.e. the unit may still be running its callbacks. The
    /// unit is then deliberately left alive (not uninitialized, not disposed) — see [`Drop`].
    fn stop(&mut self) -> bool {
        unsafe {
            if !self.unit.is_null() {
                if self.started {
                    let st = AudioOutputUnitStop(self.unit);
                    if st != 0 {
                        // Stop is the synchronization point that joins in-flight render
                        // callbacks; if it failed they may keep running after this handle is
                        // gone. Latch it so the audio host never lowers the renderer gate on
                        // the strength of this teardown — retired graphs then wait for their
                        // grace epoch (the pre-gate behavior) instead of being freed under a
                        // possibly-live callback.
                        crate::device::note_stream_teardown_failure();
                        // Leave the unit initialized and undisposed: uninitializing or disposing
                        // it while a callback may be inside frees the refcon contexts out from
                        // under it, which is a use-after-free, not merely a data race. The
                        // callbacks themselves are already harmless — the cells retire them by
                        // epoch — so a leaked, silently-rendering unit is the cheap outcome. It
                        // is also bounded: this needs CoreAudio's stop to fail, which is not a
                        // path a session takes repeatedly.
                        return false;
                    }
                    AudioUnitUninitialize(self.unit);
                    self.started = false;
                }
                AudioComponentInstanceDispose(self.unit);
                self.unit = std::ptr::null_mut();
            }
            true
        }
    }
}

impl Drop for Vpio {
    fn drop(&mut self) {
        // Stop (joins the RT callback) BEFORE the contexts are freed: no callback runs afterward.
        if !self.stop() {
            // The stop failed, so a callback may still be inside. Its refcons are the addresses
            // of `_in` / `_out`, which this drop is about to free — leak them instead. `stop`
            // left the unit alive for the same reason, and both cells have already retired this
            // generation, so what leaks renders silence and feeds nothing.
            if let Some(inp) = self._in.take() {
                std::mem::forget(inp);
            }
            if let Some(out) = self._out.take() {
                std::mem::forget(out);
            }
        }
    }
}

// ---- property helpers (non-RT; setup only) ----------------------------------------------------

unsafe fn set_u32(unit: AudioUnit, id: u32, scope: u32, elem: u32, v: u32) -> Result<(), String> {
    let st = AudioUnitSetProperty(
        unit,
        id,
        scope,
        elem,
        &v as *const u32 as *const c_void,
        std::mem::size_of::<u32>() as u32,
    );
    if st == 0 {
        Ok(())
    } else {
        Err(format!(
            "SetProperty {id} (scope {scope}, elem {elem}) failed: {st}"
        ))
    }
}

unsafe fn set_asbd(
    unit: AudioUnit,
    scope: u32,
    elem: u32,
    asbd: &AudioStreamBasicDescription,
) -> Result<(), String> {
    let st = AudioUnitSetProperty(
        unit,
        kAudioUnitProperty_StreamFormat,
        scope,
        elem,
        asbd as *const _ as *const c_void,
        std::mem::size_of::<AudioStreamBasicDescription>() as u32,
    );
    if st == 0 {
        Ok(())
    } else {
        Err(format!(
            "StreamFormat (scope {scope}, elem {elem}) failed: {st}"
        ))
    }
}

/// Read the effective stream format of one element (after initialize).
unsafe fn get_asbd(unit: AudioUnit, scope: u32, elem: u32) -> Option<AudioStreamBasicDescription> {
    let mut asbd: AudioStreamBasicDescription = std::mem::zeroed();
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let st = AudioUnitGetProperty(
        unit,
        kAudioUnitProperty_StreamFormat,
        scope,
        elem,
        NonNull::new(&mut asbd as *mut _ as *mut c_void)?,
        NonNull::from(&mut size),
    );
    if st == 0 {
        Some(asbd)
    } else {
        None
    }
}

unsafe fn set_struct(
    unit: AudioUnit,
    id: u32,
    scope: u32,
    elem: u32,
    cb: &AURenderCallbackStruct,
) -> Result<(), String> {
    let st = AudioUnitSetProperty(
        unit,
        id,
        scope,
        elem,
        cb as *const _ as *const c_void,
        std::mem::size_of::<AURenderCallbackStruct>() as u32,
    );
    if st == 0 {
        Ok(())
    } else {
        Err(format!("SetProperty {id} (callback) failed: {st}"))
    }
}

// ---- RT-safe callbacks ------------------------------------------------------------------------

/// Input callback (element 1): pull the captured mic frames via `AudioUnitRender` into buffers we
/// supply, laid out as VPIO negotiated (one interleaved buffer, or one plane per channel), cancel
/// the echo, then forward interleaved f32 to `InputCell`. Handles mono/stereo,
/// interleaved/non-interleaved.
unsafe extern "C-unwind" fn input_cb(
    in_ref: NonNull<c_void>,
    flags: NonNull<AudioUnitRenderActionFlags>,
    ts: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    _io: *mut AudioBufferList,
) -> i32 {
    let res = catch_unwind(AssertUnwindSafe(|| {
        // Flush-to-zero for this callback thread: the AEC's FFT/adaptive-filter feedback and the
        // expander state would otherwise hit denormal slowdowns (FPCR/MXCSR is per-thread).
        let _fz = crate::dsp::denormals::FlushGuard::new();
        let ctx = &mut *(in_ref.as_ptr() as *mut InCtx);
        let frames = (frames as usize).min(MAX_FRAMES);
        // Single ring-length snapshot for this callback's depth accounting (see the measurement
        // contract on `RefRingSync::on_success`): the output callback pushes concurrently, so a
        // live `len()` read after the pops can be inflated by a racing push — sustained over a
        // trim window that inflation would be shed as if it were backlog, and the next cycle
        // would underrun (zeros injected into the far-end stream). The snapshot can only
        // UNDER-estimate (safe: a real backlog is then trimmed one cycle later).
        let ref_len_entry = ctx.ref_cons.len();
        // The list the negotiated format calls for (the room we provide). The mic is frequently
        // mono even when the format claims stereo, so the real count is read back after render.
        // A NON-INTERLEAVED format needs one single-channel buffer per channel: handed the one
        // interleaved buffer instead, the render fails every cycle and the mic is dead.
        let cap = ctx.layout.capture();
        let nbuf = cap.buffers();
        let per_buf = cap.channels_per_buffer() * frames; // samples each buffer has room for

        // SUPPLY our own buffers (mData non-null). The lend pattern (NULL mData) returned st=0
        // with mData still NULL on this unit → `feed` never ran (the black-meter / 0-frames bug).
        // Buffer b lives at render_buf[b * MAX_FRAMES..]: a plane holds at most MAX_FRAMES
        // samples, and a single interleaved buffer starts at 0 with room for MAX_FRAMES * MAX_CH.
        ctx.abl.m_number_buffers = nbuf as u32;
        for b in 0..nbuf {
            ctx.abl.m_buffers[b] = AudioBuffer {
                mNumberChannels: cap.channels_per_buffer() as u32,
                mDataByteSize: (per_buf * 4) as u32,
                mData: ctx.render_buf[b * MAX_FRAMES..].as_mut_ptr() as *mut c_void,
            };
        }

        let abl_ptr = (&mut ctx.abl as *mut AblN) as *mut AudioBufferList;
        let st = AudioUnitRender(
            ctx.unit,
            flags.as_ptr(),
            ts,
            ELEM_INPUT,
            frames as u32,
            NonNull::new_unchecked(abl_ptr),
        );

        // What each buffer actually came back with (its mDataByteSize, never past the room we
        // gave it). `downmix_to_mono` reads the real channel count off these sizes.
        let mut got: [&[f32]; MAX_CH] = [&[]; MAX_CH];
        let mut got_samples = 0usize;
        for (b, g) in got.iter_mut().enumerate().take(nbuf) {
            let len = (ctx.abl.m_buffers[b].mDataByteSize as usize / std::mem::size_of::<f32>())
                .min(per_buf);
            let start = b * MAX_FRAMES;
            *g = &ctx.render_buf[start..start + len];
            got_samples += len;
        }

        if st != 0 {
            // Sustained AudioUnitRender failure means the input device went away (hot-unplug /
            // default-device change): the VPIO unit has no error callback, so flag it for the
            // host's device-event poll (it rebuilds), matching build_input's cpal err_cb.
            // RT-safe: a plain counter plus one atomic OR fired once at the threshold.
            if ctx.dead.on_dead(frames) {
                ctx.event
                    .fetch_or(crate::device::EVENT_INPUT_LOST, Ordering::Release);
            }
            // The output callback still pushed this cycle's reference: consume (discard) it so
            // push/pop stay 1:1 and the ring never accumulates a permanent backlog (which would
            // delay the reference past the AEC's modeled round-trip — a non-causal echo the
            // filter can never re-converge on). RT-safe: bounded wait-free pops. Skipped when
            // the AEC is bypassed (the ring is unpumped; there is no 1:1 flow to keep).
            if !ctx.aec_bypass {
                for _ in 0..ctx.ref_sync.on_failure(frames) {
                    let _ = ctx.ref_cons.pop();
                }
            }
            return st;
        }
        if got_samples == 0 {
            // A render that SUCCEEDS (st==0) but delivers zero frames is also a dead mic: the
            // device is alive yet its capture pipeline hands us nothing (a stalled default-device
            // change, or a unit that never delivers at all). Accumulate toward EVENT_INPUT_LOST
            // just like a hard render failure — otherwise these empty successes reset the streak
            // and the loss is never flagged. RT-safe: a plain counter plus one atomic OR.
            if ctx.dead.on_dead(frames) {
                ctx.event
                    .fetch_or(crate::device::EVENT_INPUT_LOST, Ordering::Release);
            }
            // Same 1:1 discard as the render-failure path above: an empty success also skipped
            // the normal pops while the output side kept pushing.
            if !ctx.aec_bypass {
                for _ in 0..ctx.ref_sync.on_failure(frames) {
                    let _ = ctx.ref_cons.pop();
                }
            }
            return 0;
        }
        // Reset only on a real, non-empty capture (a healthy mic delivers frames every cycle).
        ctx.dead.on_capture();

        // Mix the RAW mic down to its mono signal (the chosen input, or the one Auto picked).
        // `n` = captured frames.
        let mode = ctx.input.channel_mode();
        let n = downmix_to_mono(
            cap,
            &got[..nbuf],
            frames,
            &mut ctx.mic_mono[..frames],
            &mut ctx.mix,
            mode,
        );
        if ctx.aec_bypass {
            // AEC bypassed (mismatched element rates): no far-end pairing exists, the ref
            // ring is unpumped. The raw mic takes the same high-pass + gain path below.
            ctx.clean_mono[..n].copy_from_slice(&ctx.mic_mono[..n]);
        } else {
            // Overflow guard ONLY: dropping reference samples warps the far-end stream and
            // wrecks the adaptive filter (the previous per-block drain caused continuous
            // mis-adaptation). Normal duplex is 1:1 so the ring stays shallow; only if it
            // nears capacity do we shed the oldest. This guard reads the LIVE length (it
            // protects the real ring); the shed count feeds the snapshot accounting below.
            let mut shed = 0usize;
            while ctx.ref_cons.len() + MAX_FRAMES > REF_RING_CAP {
                let _ = ctx.ref_cons.pop();
                shed += 1;
            }
            // Pull the matching far-end reference (the backing we sent to the speaker,
            // mono; zero-filled if the ring momentarily lags).
            for i in 0..n {
                ctx.far_mono[i] = ctx.ref_cons.pop().unwrap_or(0.0);
            }
            // Trim any standing backlog left by cycles where this callback did not run at
            // all (a silent device stall never reports an error): `ref_sync` compares the
            // post-pop depth against its recorded healthy baseline and returns the extra
            // samples to discard. The depth is derived from the entry snapshot — NOT a live
            // `len()`, which a concurrently racing push can inflate into spurious trims
            // (see the measurement contract on `on_success`). Bounded by the ring capacity;
            // RT-safe (wait-free pops).
            let depth = ref_len_entry.saturating_sub(shed + n);
            for _ in 0..ctx.ref_sync.on_success(depth) {
                let _ = ctx.ref_cons.pop();
            }
            // Cancel the loudspeaker echo from the mic, preserving the singing voice.
            ctx.aec.process(
                &ctx.mic_mono[..n],
                &ctx.far_mono[..n],
                &mut ctx.clean_mono[..n],
            );
        }
        // High-pass (DC/rumble) the clean mono voice and feed it as the cell's one channel, which
        // applies the (user-adjustable) record gain after its mix — a pure linear scale (the
        // float take stores over-0 dBFS losslessly; the master limiter guards playback).
        for i in 0..n {
            ctx.scratch[i] = ctx.hp.process(ctx.clean_mono[i]);
        }
        let gain = f32::from_bits(INPUT_GAIN.load(Ordering::Relaxed));
        // A denied feed drops the block rather than racing a second producer into rings that
        // have exactly one producer by construction.
        ctx.input
            .try_feed_gained(ctx.epoch, &ctx.scratch[..n], gain);
        0
    }));
    res.unwrap_or(-1)
}

/// Output render callback (element 0, duplex only): render the engine graph into an interleaved
/// stereo scratch, then write it into VPIO's output buffer in whatever layout it negotiated (the
/// AEC reference). Handles mono/stereo, interleaved/non-interleaved.
unsafe extern "C-unwind" fn output_cb(
    in_ref: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _ts: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    io: *mut AudioBufferList,
) -> i32 {
    let res = catch_unwind(AssertUnwindSafe(|| {
        // Flush-to-zero for this callback thread (the monitor reverb's feedback would denormal).
        let _fz = crate::dsp::denormals::FlushGuard::new();
        if io.is_null() {
            return 0;
        }
        let abl = &mut *io;
        let nbuf = abl.mNumberBuffers as usize;
        if nbuf == 0 {
            return 0;
        }
        let ctx = &mut *(in_ref.as_ptr() as *mut OutCtx);
        let frames = (frames as usize).min(MAX_FRAMES);

        // Render the engine to interleaved stereo scratch.
        let need = frames * CHANNELS;
        let scratch = &mut ctx.scratch[..need];
        // Playback only: the VPIO output is the AEC reference, which must equal the backing track
        // (no monitored voice) or echo cancellation breaks down.
        // A denial means this callback was retired by a newer output stream (or a zombie is
        // still inside the processor). Everything downstream is then skipped, not just the
        // render: silence goes to the device, the consumed cursor is NOT stamped (it would move
        // the playhead over audio nobody produced), and no silence is pushed into the AEC
        // reference ring, which input_cb pairs 1:1 with real far-end audio.
        if ctx.rt.try_render_playback(ctx.epoch, scratch, frames) {
            // Identity delivery (VPIO renders at the engine rate straight into the
            // element buffer): the consumed cursor equals the render counter.
            ctx.rt.transport().sync_consumed_to_rendered();
            // The backing plays at its mixer level during recording (no auto-duck) — the singer
            // lowers it themselves if they want less bleed. The ref ring + speaker see the same
            // signal, so the AEC stays consistent (it cancels whatever the speaker actually
            // plays). Publish the mono backing to the AEC far-end ring (the echo reference for
            // input_cb). Drops on a full ring (back-pressure; input_cb keeps it drained 1:1
            // every duplex cycle). Skipped when the AEC is bypassed (mismatched element rates):
            // the input side never pops then, so pushing would only fill the ring for nothing.
            if ctx.push_ref {
                for i in 0..frames {
                    let _ = ctx
                        .ref_prod
                        .push(0.5 * (scratch[2 * i] + scratch[2 * i + 1]));
                }
            }
            // Self-monitor: add the singer's voice to the OUTPUT (headphones) — AFTER the
            // reference is captured, so the voice never enters the AEC reference. Silent at
            // monitor gain 0 (the default, e.g. on a speaker where it would feed back); the
            // mixer's monitor fader raises it.
            ctx.rt.try_mix_monitor(ctx.epoch, scratch, frames);
        } else {
            scratch.fill(0.0);
        }

        let bufs = std::slice::from_raw_parts_mut(abl.mBuffers.as_mut_ptr(), nbuf);
        if nbuf == 1 {
            // Interleaved (one buffer, possibly mono or stereo).
            let buf = &mut bufs[0];
            if buf.mData.is_null() {
                return 0;
            }
            let oc = (buf.mNumberChannels as usize).max(1);
            let cap = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
            let dst = std::slice::from_raw_parts_mut(buf.mData as *mut f32, cap);
            let f = (cap / oc).min(frames);
            for i in 0..f {
                let l = scratch[i * CHANNELS];
                let r = scratch[i * CHANNELS + 1];
                if oc == 1 {
                    dst[i] = 0.5 * (l + r);
                } else {
                    dst[i * oc] = l;
                    dst[i * oc + 1] = r;
                    for c in 2..oc {
                        dst[i * oc + c] = 0.0;
                    }
                }
            }
            for s in &mut dst[f * oc..] {
                *s = 0.0;
            }
        } else {
            // Non-interleaved planar: scatter L/R into separate channel buffers.
            for (ch, buf) in bufs.iter_mut().enumerate() {
                if buf.mData.is_null() {
                    continue;
                }
                let cap = buf.mDataByteSize as usize / std::mem::size_of::<f32>();
                let dst = std::slice::from_raw_parts_mut(buf.mData as *mut f32, cap);
                let f = cap.min(frames);
                for i in 0..f {
                    dst[i] = if ch == 0 {
                        scratch[i * CHANNELS]
                    } else {
                        scratch[i * CHANNELS + 1]
                    };
                }
                for s in &mut dst[f..] {
                    *s = 0.0;
                }
            }
        }
        0
    }));
    res.unwrap_or(-1)
}

// ---- IO buffer tuning + latency reporting -----------------------------------------------------

unsafe fn dev_get_u32(dev: u32, sel: u32, scope: u32) -> Option<u32> {
    use objc2_core_audio::{
        kAudioObjectPropertyElementMain, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    };
    let addr = AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut v: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let st = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut v as *mut u32 as *mut c_void)?,
    );
    if st == 0 {
        Some(v)
    } else {
        None
    }
}

unsafe fn dev_get_f64(dev: u32, sel: u32, scope: u32) -> Option<f64> {
    use objc2_core_audio::{
        kAudioObjectPropertyElementMain, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    };
    let addr = AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut v: f64 = 0.0;
    let mut size = std::mem::size_of::<f64>() as u32;
    let st = AudioObjectGetPropertyData(
        dev,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new(&mut v as *mut f64 as *mut c_void)?,
    );
    if st == 0 {
        Some(v)
    } else {
        None
    }
}

unsafe fn dev_set_u32(dev: u32, sel: u32, scope: u32, v: u32) -> i32 {
    use objc2_core_audio::{
        kAudioObjectPropertyElementMain, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    };
    let addr = AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    match NonNull::new(&v as *const u32 as *mut c_void) {
        Some(data) => AudioObjectSetPropertyData(
            dev,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            std::mem::size_of::<u32>() as u32,
            data,
        ),
        None => -1,
    }
}

/// Whether `dev` can actually play audio: probes `kAudioDevicePropertyStreams` on the OUTPUT scope.
/// A failed query or an empty stream list both mean "not an output device" (e.g. an input-only USB
/// mic), so the output leg must be attributed to the default output device instead. Not RT — called
/// during `start`, and by [`device_serves_output`].
unsafe fn has_output_streams(dev: u32) -> bool {
    use objc2_core_audio::{
        kAudioDevicePropertyStreams, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeOutput, AudioObjectGetPropertyDataSize,
        AudioObjectPropertyAddress,
    };
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreams,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size: u32 = 0;
    let st = AudioObjectGetPropertyDataSize(
        dev,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
    );
    st == 0 && size > 0
}

/// Whether the device with `uid` is present right now and has output streams: a bound mic that
/// serves the duplex unit's output leg too. Two HAL reads for that one device (UID to device, then
/// its output stream list), so the device-event poll can ask afresh every tick. Not RT.
pub(crate) fn device_serves_output(uid: &str) -> bool {
    // SAFETY: a plain HAL property query on a device id the HAL just resolved.
    device_id_for_uid(uid).is_some_and(|dev| unsafe { has_output_streams(dev) })
}

/// Request a small IO buffer on the device(s) the VPIO unit binds (cuts monitor/record latency and
/// keeps the speaker→mic echo inside the AEC's filter), and return the estimated record round-trip
/// in frames @ `ENGINE_SR`: ACOUSTIC flight + each leg's (buffer + safety offset + hardware
/// latency), the per-device counts scaled from the device's nominal rate to engine-rate frames.
/// The AEC's own block latency is added by the caller. Used to time-align recorded takes.
///
/// `device_uid` is the mic the caller is about to bind (via `kAudioOutputUnitProperty_CurrentDevice`
/// below). Decision logic per leg:
/// - UID resolves to a DUPLEX device (it has output streams, e.g. a USB interface or headset):
///   VPIO uses that SINGLE device for both legs (a communication endpoint), so both scopes are
///   measured on it — a non-default mic has its own buffer/latency/safety, and measuring the
///   system defaults instead misaligns takes.
/// - UID resolves to an INPUT-ONLY mic (e.g. a USB mic played through the built-in speakers):
///   VPIO still renders through the system default OUTPUT device, so the output leg is measured
///   (and its IO buffer tuned) on that default. Measuring the output scope on the input-only mic
///   instead would silently drop the speaker's real buffer + latency + safety offset — hundreds
///   of frames on MacBook speakers — because those queries fail and fall back to 0.
/// - UID does NOT resolve: VPIO falls back to the system default INPUT for capture and default
///   OUTPUT for playback (two distinct devices), so each leg is measured on its own default.
fn tune_io_and_round_trip(device_uid: Option<&str>) -> u32 {
    use objc2_core_audio::{
        kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyLatency,
        kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
        kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
        kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    };
    const WANT_FRAMES: u32 = 256;
    /// Speaker→mic acoustic flight on a laptop (~0.3 m ≈ 1.3 ms).
    const ACOUSTIC_FRAMES: u32 = 64;
    let mut round_trip = ACOUSTIC_FRAMES;
    unsafe {
        // Resolve the device the VPIO unit will actually bind, then assign each leg per the doc
        // comment above: a duplex bound device serves BOTH legs; an input-only bound mic serves
        // only the input leg (VPIO plays through the default OUTPUT device, which must still be
        // tuned and measured); unbound falls back to the system defaults for both legs.
        let default_dev = |sel: u32| {
            dev_get_u32(
                kAudioObjectSystemObject as u32,
                sel,
                kAudioObjectPropertyScopeGlobal,
            )
        };
        let bound = device_uid.and_then(device_id_for_uid);
        let legs: [(Option<u32>, u32); 2] = match bound {
            Some(dev) if has_output_streams(dev) => [
                (Some(dev), kAudioObjectPropertyScopeInput),
                (Some(dev), kAudioObjectPropertyScopeOutput),
            ],
            Some(dev) => [
                (Some(dev), kAudioObjectPropertyScopeInput),
                (
                    default_dev(kAudioHardwarePropertyDefaultOutputDevice),
                    kAudioObjectPropertyScopeOutput,
                ),
            ],
            None => [
                (
                    default_dev(kAudioHardwarePropertyDefaultInputDevice),
                    kAudioObjectPropertyScopeInput,
                ),
                (
                    default_dev(kAudioHardwarePropertyDefaultOutputDevice),
                    kAudioObjectPropertyScopeOutput,
                ),
            ],
        };
        for (dev, lscope) in legs {
            let Some(dev) = dev else {
                continue;
            };
            let _ = dev_set_u32(
                dev,
                kAudioDevicePropertyBufferFrameSize,
                kAudioObjectPropertyScopeGlobal,
                WANT_FRAMES,
            );
            let buf = dev_get_u32(
                dev,
                kAudioDevicePropertyBufferFrameSize,
                kAudioObjectPropertyScopeGlobal,
            )
            .unwrap_or(WANT_FRAMES);
            let latency = dev_get_u32(dev, kAudioDevicePropertyLatency, lscope).unwrap_or(0);
            let safety = dev_get_u32(dev, kAudioDevicePropertySafetyOffset, lscope).unwrap_or(0);
            // BufferFrameSize/Latency/SafetyOffset are reported in frames at the DEVICE's
            // nominal rate, but RECORD_LATENCY_FRAMES is published (and consumed by Dart) as
            // frames @ ENGINE_SR — scale each leg before summing, or a 44.1/96 kHz device
            // skews the take alignment by its rate ratio.
            let dev_rate = dev_get_f64(
                dev,
                kAudioDevicePropertyNominalSampleRate,
                kAudioObjectPropertyScopeGlobal,
            )
            .unwrap_or(ENGINE_SR);
            round_trip += frames_at_engine_rate(buf + latency + safety, dev_rate);
        }
    }
    round_trip
}

// ---- UID → AudioDeviceID via the HAL ----------------------------------------------------------

/// Resolve a CoreAudio device UID to an `AudioDeviceID`. `None` on any failure, including the
/// ordinary "the device is gone" case. `start` treats that as an error rather than binding the
/// default behind the user's back. Not RT — called only during `start`.
fn device_id_for_uid(uid: &str) -> Option<u32> {
    use objc2_core_audio::{
        kAudioHardwarePropertyTranslateUIDToDevice, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioObjectGetPropertyData,
        AudioObjectPropertyAddress,
    };
    use objc2_core_foundation::CFString;

    unsafe {
        let cf = CFString::from_str(uid);
        let cf_ptr: *const c_void = (&*cf as *const CFString) as *const c_void;

        let addr = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyTranslateUIDToDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut out_id: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let st = AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&addr),
            std::mem::size_of::<*const c_void>() as u32,
            &cf_ptr as *const _ as *const c_void,
            NonNull::from(&mut size),
            NonNull::new(&mut out_id as *mut u32 as *mut c_void)?,
        );
        if st == 0 && out_id != 0 {
            Some(out_id)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::master::MasterNode;
    use crate::graph::{Edge, GraphDesc, NodeEntry};
    use crate::live::spawn;
    use crate::sample::Smp;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    const BLOCK: usize = 128;

    /// The minimum shared state `Vpio::start` needs. No audio hardware is touched: the device
    /// resolution this test exercises happens before any AudioUnit is created.
    fn cells() -> (Arc<InputCell>, Arc<RtCell>, Arc<AtomicU32>) {
        let desc = GraphDesc {
            nodes: vec![NodeEntry::new(1, Box::new(MasterNode::unity()))],
            edges: Vec::<Edge>::new(),
            master: 1,
        };
        let g = desc.compile(ENGINE_SR, BLOCK).unwrap();
        let (_mic_prod, mic_cons) = spsc::<[Smp; CHANNELS]>(256);
        let (_h, rt) = spawn(g, HashMap::new(), mic_cons, 256);
        let (mic_prod, _mic_rx) = spsc::<[Smp; CHANNELS]>(256);
        let (cap_prod, _cap_rx) = spsc::<[Smp; CHANNELS]>(8);
        let (rec_prod, _rec_rx) = spsc::<[Smp; CHANNELS]>(8);
        let event = Arc::new(AtomicU32::new(0));
        let input = InputCell::new(
            mic_prod,
            cap_prod,
            rec_prod,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1.0f64.to_bits())),
            Arc::new(AtomicU64::new(0)),
            Arc::clone(&event),
        );
        (input, RtCell::new(rt), event)
    }

    /// The whole point of failing here: the host learns a mic fell back ONLY from an `Err` out of
    /// `start` (device.rs `open_mic` → `or_else`), and only then publishes an empty actual-device
    /// and raises EVENT_INPUT_FELL_BACK. A best-effort bind captured from the system default while
    /// the UI kept naming the selected mic. Asserting the exact message keeps the test honest on a
    /// machine with no mic, where a regressed `start` would fail later for an unrelated reason.
    #[test]
    fn an_unresolvable_input_uid_fails_the_open_instead_of_binding_the_default() {
        let (input, rt, event) = cells();
        let err = Vpio::start(Some("vocara-no-such-device"), input, rt, false, event)
            .err()
            .expect("an unresolvable UID must not open on the system default");
        assert_eq!(err, "input device vocara-no-such-device not found");
    }
}
