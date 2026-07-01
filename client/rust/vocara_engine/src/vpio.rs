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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use objc2_audio_toolbox::{
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioOutputUnitProperty_SetInputCallback, kAudioUnitProperty_MaximumFramesPerSlice,
    kAudioUnitProperty_SetRenderCallback, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global,
    kAudioUnitManufacturer_Apple, kAudioUnitScope_Input, kAudioUnitScope_Output,
    kAudioUnitSubType_VoiceProcessingIO, kAudioUnitType_Output,
    kAUVoiceIOProperty_BypassVoiceProcessing,
    kAUVoiceIOProperty_VoiceProcessingEnableAGC, AURenderCallbackStruct, AudioComponentDescription,
    AudioComponentFindNext, AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioUnit,
    AudioUnitGetProperty, AudioUnitInitialize, AudioUnitRender, AudioUnitRenderActionFlags,
    AudioUnitSetProperty, AudioUnitUninitialize,
};
use objc2_audio_toolbox::{AudioOutputUnitStart, AudioOutputUnitStop};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
};

use crate::aec::Aec;
use crate::device::InputCell;
use crate::live::RtCell;
use crate::ring::{spsc, Consumer, Producer};
use crate::sample::{CHANNELS, ENGINE_SR};

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
const MAX_CH: usize = 2;
/// Far-end reference ring capacity (mono samples). Comfortably spans several IO buffers of backlog.
const REF_RING_CAP: usize = 1 << 15; // 32768
/// Echo-path length our AEC models. Covers the macOS duplex round-trip (~170 ms) + reverb tail.
const AEC_TAIL_MS: f32 = 250.0;
/// The AEC's own algorithmic latency: one internal block (256 samples), added to the IO round-trip.
const AEC_BLOCK_FRAMES: u32 = 256;
/// Consecutive `AudioUnitRender` failures before we flag the input device as lost. At the ~10 ms
/// VPIO slice that is ~200 ms of dead capture — a real disconnect, not a one-off transient.
const INPUT_LOST_STREAK: u32 = 20;

/// Estimated record round-trip latency (frames @ ENGINE_SR): output IO (buffer + safety + device
/// latency) + acoustic flight + input IO + the AEC block. The captured vocal lands this far behind
/// the engine playhead, so recorded takes are shifted earlier by it to sit on the beat the singer
/// heard. Written when the mic opens; read by the FFI. 0 until the mic has opened once.
pub(crate) static RECORD_LATENCY_FRAMES: AtomicU32 = AtomicU32::new(0);

/// Default capture (record) gain (~+24 dB). The built-in mic has no AGC and records quiet; with the
/// expander pulling the noise floor down, this boost lands mostly on the voice. User-adjustable at
/// runtime via [`INPUT_GAIN`]; paired with a soft ceiling so peaks saturate gently, not hard-clip.
///
/// NOTE: the backing track is no longer auto-ducked while recording — the singer sets the backing
/// level themselves on the mixer. (Removed; the speaker now plays the backing at its mixer level
/// during a take, so the AEC reference and the take's bleed reflect exactly that.)
const DEFAULT_INPUT_GAIN: f32 = 15.85;

/// Runtime record/input gain (linear bits), set from the mixer's mic-input fader.
pub(crate) static INPUT_GAIN: AtomicU32 = AtomicU32::new(DEFAULT_INPUT_GAIN.to_bits());

/// Set the record/input gain (linear).
pub(crate) fn set_input_gain(linear: f32) {
    INPUT_GAIN.store(linear.max(0.0).to_bits(), Ordering::Relaxed);
}

/// Apply the capture gain with a soft knee: quiet/moderate audio stays linear (analysis-safe) and
/// only peaks above the knee saturate smoothly toward ±1 (no hard clipping). RT-safe.
#[inline]
fn makeup(x: f32, gain: f32) -> f32 {
    const KNEE: f32 = 0.7;
    let y = x * gain;
    let a = y.abs();
    if a <= KNEE {
        y
    } else {
        y.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

/// Post-AEC capture cleanup, applied BEFORE the makeup gain so the boost doesn't amplify the noise
/// floor: a ~80 Hz high-pass (kills low rumble) + an adaptive downward expander. The expander tracks
/// the running noise floor and pulls the signal down BETWEEN phrases (where the mic hiss + residual
/// backing sit), but never fully gates (partial −18 dB floor) and uses fast-attack / slow-release,
/// so the singing voice is never chopped. RT-safe (no alloc/lock; denormal-guarded by the caller's
/// flush-to-zero).
struct Cleanup {
    hp_x: f32,
    hp_y: f32,
    env: f32,
    nf: f32,
    gain: f32,
}

impl Cleanup {
    fn new() -> Self {
        Cleanup { hp_x: 0.0, hp_y: 0.0, env: 0.0, nf: 1.0e-4, gain: 1.0 }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        // One-pole high-pass ~80 Hz @ 48 kHz.
        const HP_A: f32 = 0.9896;
        self.hp_y = HP_A * (self.hp_y + x - self.hp_x);
        self.hp_x = x;
        let s = self.hp_y;

        // Fast-attack / ~5 ms-release peak envelope.
        const ENV_REL: f32 = 0.9959;
        let a = s.abs();
        self.env = a.max(self.env * ENV_REL);

        // Adaptive noise floor: drop to the envelope instantly, creep up very slowly (~1 s) so it
        // settles at the hiss/residual level and the voice always sits well above it.
        const NF_RISE: f32 = 2.1e-5;
        if self.env < self.nf {
            self.nf = self.env;
        } else {
            self.nf += (self.env - self.nf) * NF_RISE;
        }

        // Downward expander: full gain when the envelope is comfortably above the floor, easing to a
        // partial floor (never silent) when it approaches the noise floor.
        const OPEN_MARGIN: f32 = 4.0; // open ~+12 dB above the noise floor
        const GATE_FLOOR: f32 = 0.12; // ≈ −18 dB, the most it ever pulls down
        let thresh = (self.nf * OPEN_MARGIN).max(1.0e-6);
        let target = if self.env >= thresh {
            1.0
        } else {
            let r = self.env / thresh; // 0..1
            (r * r).max(GATE_FLOOR)
        };

        // Asymmetric gain slew: ~2 ms attack (don't clip onsets), ~200 ms release (don't chop tails).
        const ATTACK: f32 = 0.0103;
        const RELEASE: f32 = 1.04e-4;
        let slew = if target > self.gain { ATTACK } else { RELEASE };
        self.gain += (target - self.gain) * slew;

        s * self.gain
    }
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

/// Effective layout of one element, read back from the unit after initialize. (The callbacks
/// supply/handle interleaving at runtime, so only the channel count + rate are kept.)
#[derive(Clone, Copy)]
struct Layout {
    channels: usize,
    rate: f64,
}

impl Layout {
    fn from(asbd: &AudioStreamBasicDescription) -> Layout {
        Layout {
            channels: (asbd.mChannelsPerFrame as usize).max(1),
            rate: if asbd.mSampleRate > 0.0 { asbd.mSampleRate } else { VPIO_RATE },
        }
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
    unit: AudioUnit,
    abl: AblN,
    /// Buffer we SUPPLY to `AudioUnitRender` (VPIO copies the captured mic into it). Supplying our
    /// own buffer is the robust AUHAL input pattern; the lend pattern (NULL mData) returned success
    /// here but never set mData, so `feed` never ran.
    render_buf: Vec<f32>,
    /// Interleaved-stereo scratch handed to `InputCell` (mono is duplicated up; `MAX_FRAMES*MAX_CH`).
    scratch: Vec<f32>,
    /// Custom acoustic echo canceller: removes the backing-track echo (loudspeaker→mic) from the raw
    /// mic while preserving the singing voice. VPIO's own AEC is bypassed (it ducked the sung voice).
    aec: Aec,
    /// Far-end reference (the rendered backing, mono) produced by `output_cb`.
    ref_cons: Consumer<f32>,
    /// Mono AEC scratch (mic-in / far-end / clean-out), `MAX_FRAMES` each.
    mic_mono: Vec<f32>,
    far_mono: Vec<f32>,
    clean_mono: Vec<f32>,
    /// Post-AEC high-pass + adaptive expander, applied before the makeup gain.
    cleanup: Cleanup,
    layout: Layout,
    /// Shared device-event flags (polled by the host via `ve_poll_device_event`). We raise
    /// EVENT_INPUT_LOST on a sustained render-failure streak so a mid-session mic disconnect is
    /// observable, matching the cpal `err_cb` path (the VPIO unit itself has no error callback).
    event: Arc<AtomicU32>,
    /// Consecutive `AudioUnitRender` failures, reset on any success.
    err_streak: u32,
}

// SAFETY: built/moved on the AudioHost thread before the unit starts; afterwards only the input
// callback thread touches it.
unsafe impl Send for InCtx {}

/// Output-callback context (duplex only): renders the engine graph into VPIO's output buffer (the
/// AEC reference). Owns an interleaved render scratch it then writes into VPIO's real layout.
struct OutCtx {
    rt: Arc<RtCell>,
    /// Interleaved stereo render scratch (`MAX_FRAMES * CHANNELS`).
    scratch: Vec<f32>,
    /// Mono downmix of the rendered backing, pushed to the AEC far-end ring (the echo reference).
    ref_prod: Producer<f32>,
    layout: Layout,
}

// SAFETY: built/moved on the AudioHost thread before the unit starts; afterwards only the output
// callback thread touches it.
unsafe impl Send for OutCtx {}

/// A live VoiceProcessingIO unit. Drop stops + disposes it (RAII), mirroring `DeviceHandle`.
pub struct Vpio {
    unit: AudioUnit,
    _in: Box<InCtx>,
    _out: Option<Box<OutCtx>>,
    started: bool,
}

// SAFETY: constructed, started, stopped and dropped only on the single AudioHost thread.
unsafe impl Send for Vpio {}

impl Vpio {
    /// Create + initialize + start a VPIO unit feeding `input` (mic) and, when `with_output`,
    /// rendering `rt` to the speaker through element 0 (enables AEC). `device_uid` selects the
    /// input device (None / unresolvable → system default). MUST run on the AudioHost thread.
    pub fn start(
        device_uid: Option<&str>,
        input: Arc<InputCell>,
        rt: Arc<RtCell>,
        with_output: bool,
        event: Arc<AtomicU32>,
    ) -> Result<Vpio, String> {
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
            let round_trip = tune_io_and_round_trip(device_uid) + AEC_BLOCK_FRAMES;
            RECORD_LATENCY_FRAMES.store(round_trip, Ordering::Relaxed);

            let on: u32 = 1;
            let off: u32 = 0;
            // Enable input element (1); enable/disable output element (0) per mode.
            if let Err(e) = set_u32(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, ELEM_INPUT, on) {
                return bail(unit, e);
            }
            let out_flag = if with_output { on } else { off };
            if let Err(e) = set_u32(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Output, ELEM_OUTPUT, out_flag) {
                return bail(unit, e);
            }

            // Bind the selected input device (best-effort; default device on failure).
            if let Some(uid) = device_uid {
                if let Some(dev_id) = device_id_for_uid(uid) {
                    let _ = set_u32(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, dev_id);
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

            let _ = set_u32(unit, kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitScope_Global, 0, VPIO_MAX_SLICE);

            // BYPASS VPIO's voice processing (Bypass = 1): its AEC/NS is voice-call-tuned and ducked
            // the sung voice during the continuous double-talk of singing over music. We take the RAW
            // mic and run our own echo canceller (crate::aec) instead. AGC off too (moot under bypass).
            let _ = set_u32(unit, kAUVoiceIOProperty_BypassVoiceProcessing, kAudioUnitScope_Global, 0, on);
            let _ = set_u32(unit, VP_AGC_ENABLE_AGC, kAudioUnitScope_Global, 0, off);

            // Far-end reference ring (rendered backing, mono) + our AEC. `output_cb` produces the
            // reference; `input_cb` cancels it from the raw mic.
            let (ref_prod, ref_cons) = spsc::<f32>(REF_RING_CAP);
            let aec = Aec::new(VPIO_RATE as f32, AEC_TAIL_MS);

            // Pinned contexts (their addresses are the callback refcons). Layouts are placeholders
            // until we read the negotiated formats after initialize (no callback fires before start).
            let placeholder = Layout { channels: REQ_CH as usize, rate: VPIO_RATE };
            let mut in_ctx = Box::new(InCtx {
                input,
                unit,
                abl: AblN { m_number_buffers: 1, m_buffers: [zero_buffer(), zero_buffer()] },
                render_buf: vec![0.0; MAX_FRAMES * MAX_CH],
                scratch: vec![0.0; MAX_FRAMES * MAX_CH],
                aec,
                ref_cons,
                mic_mono: vec![0.0; MAX_FRAMES],
                far_mono: vec![0.0; MAX_FRAMES],
                clean_mono: vec![0.0; MAX_FRAMES],
                cleanup: Cleanup::new(),
                layout: placeholder,
                event,
                err_streak: 0,
            });
            let in_ptr = (&mut *in_ctx as *mut InCtx) as *mut c_void;

            let mut out_ctx = if with_output {
                Some(Box::new(OutCtx {
                    rt,
                    scratch: vec![0.0; MAX_FRAMES * CHANNELS],
                    ref_prod,
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
            if let Err(e) = set_struct(unit, kAudioOutputUnitProperty_SetInputCallback, kAudioUnitScope_Global, 0, &in_cb) {
                return bail(unit, e);
            }

            // Output render callback (duplex only): supplies the engine mix as the AEC reference.
            if let Some(oc) = out_ctx.as_mut() {
                let out_ptr = (&mut **oc as *mut OutCtx) as *mut c_void;
                let out_cb = AURenderCallbackStruct {
                    inputProc: Some(output_cb),
                    inputProcRefCon: out_ptr,
                };
                if let Err(e) = set_struct(unit, kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, ELEM_OUTPUT, &out_cb) {
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
            // input_cb always hands InputCell interleaved STEREO (mono captures are duplicated up),
            // so configure for identity stereo at the captured rate.
            in_ctx.input.reconfigure(in_layout.rate, CHANNELS);
            if let Some(oc) = out_ctx.as_mut() {
                let out_layout = get_asbd(unit, kAudioUnitScope_Input, ELEM_OUTPUT)
                    .map(|a| Layout::from(&a))
                    .unwrap_or(placeholder);
                oc.layout = out_layout;
            }

            let st = AudioOutputUnitStart(unit);
            if st != 0 {
                AudioUnitUninitialize(unit);
                return bail(unit, format!("AudioOutputUnitStart failed: {st}"));
            }

            Ok(Vpio { unit, _in: in_ctx, _out: out_ctx, started: true })
        }
    }

    /// Stop + uninitialize + dispose. Idempotent. MUST run on the AudioHost thread.
    fn stop(&mut self) {
        unsafe {
            if !self.unit.is_null() {
                if self.started {
                    AudioOutputUnitStop(self.unit);
                    AudioUnitUninitialize(self.unit);
                    self.started = false;
                }
                AudioComponentInstanceDispose(self.unit);
                self.unit = std::ptr::null_mut();
            }
        }
    }
}

impl Drop for Vpio {
    fn drop(&mut self) {
        // Stop (joins the RT callback) BEFORE the contexts are freed: no callback runs afterward.
        self.stop();
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
        Err(format!("SetProperty {id} (scope {scope}, elem {elem}) failed: {st}"))
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
        Err(format!("StreamFormat (scope {scope}, elem {elem}) failed: {st}"))
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

/// Input callback (element 1): pull the captured + echo-cancelled mic frames via `AudioUnitRender`
/// into a descriptor matching VPIO's negotiated layout (it lends its buffers via NULL `mData`),
/// then forward interleaved f32 to `InputCell`. Handles mono/stereo, interleaved/non-interleaved.
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
        // Channels the negotiated format advertises (the room we provide). The mic is frequently
        // mono even when the format claims stereo, so we read the real count back after render.
        let req_ch = ctx.layout.channels.clamp(1, MAX_CH);

        // SUPPLY our own interleaved buffer (mData non-null). The lend pattern (NULL mData) returned
        // st=0 with mData still NULL on this unit → `feed` never ran (the black-meter / 0-frames bug).
        let cap_samples = frames * req_ch;
        ctx.abl.m_number_buffers = 1;
        ctx.abl.m_buffers[0] = AudioBuffer {
            mNumberChannels: req_ch as u32,
            mDataByteSize: (cap_samples * 4) as u32,
            mData: ctx.render_buf.as_mut_ptr() as *mut c_void,
        };

        let abl_ptr = (&mut ctx.abl as *mut AblN) as *mut AudioBufferList;
        let st = AudioUnitRender(
            ctx.unit,
            flags.as_ptr(),
            ts,
            ELEM_INPUT,
            frames as u32,
            NonNull::new_unchecked(abl_ptr),
        );

        // Actual captured channels = samples / frames (VPIO reports stereo but often delivers mono;
        // reading 1 mono sample-per-frame as an L/R pair is exactly the "helium voice" bug).
        let got_bytes = (ctx.abl.m_buffers[0].mDataByteSize as usize).min(cap_samples * 4);
        let got_samples = got_bytes / std::mem::size_of::<f32>();
        let act_ch = if frames > 0 { (got_samples / frames).clamp(1, MAX_CH) } else { 1 };

        if st != 0 {
            // Sustained AudioUnitRender failure means the input device went away (hot-unplug /
            // default-device change): the VPIO unit has no error callback, so flag it for the
            // host's device-event poll (it rebuilds), matching build_input's cpal err_cb.
            // RT-safe: a plain counter plus one atomic OR fired once at the threshold.
            ctx.err_streak = ctx.err_streak.saturating_add(1);
            if ctx.err_streak == INPUT_LOST_STREAK {
                ctx.event
                    .fetch_or(crate::device::EVENT_INPUT_LOST, Ordering::Release);
            }
            return st;
        }
        if got_samples == 0 {
            // A render that SUCCEEDS (st==0) but delivers zero frames is also a dead mic: the
            // device is alive yet its capture pipeline hands us nothing (a stalled default-device
            // change, or a device that never produces data). Accumulate toward EVENT_INPUT_LOST
            // just like a hard render failure — otherwise these empty successes reset the streak
            // and the loss is never flagged. RT-safe: a plain counter plus one atomic OR.
            ctx.err_streak = ctx.err_streak.saturating_add(1);
            if ctx.err_streak == INPUT_LOST_STREAK {
                ctx.event
                    .fetch_or(crate::device::EVENT_INPUT_LOST, Ordering::Release);
            }
            return 0;
        }
        // Reset only on a real, non-empty capture (a healthy mic delivers frames every cycle).
        ctx.err_streak = 0;

        // Downmix the RAW mic to mono and pull the matching far-end reference (the backing we sent
        // to the speaker, mono; zero-filled if the ring momentarily lags). `n` = captured frames.
        let n = (got_samples / act_ch).min(frames);
        // Overflow guard ONLY: dropping reference samples warps the far-end stream and wrecks the
        // adaptive filter (the previous per-block drain caused continuous mis-adaptation). Normal
        // duplex is 1:1 so the ring stays shallow; only if it nears capacity do we shed the oldest.
        while ctx.ref_cons.len() + MAX_FRAMES > REF_RING_CAP {
            let _ = ctx.ref_cons.pop();
        }
        for i in 0..n {
            ctx.mic_mono[i] = if act_ch == 1 {
                ctx.render_buf[i]
            } else {
                0.5 * (ctx.render_buf[i * act_ch] + ctx.render_buf[i * act_ch + 1])
            };
            ctx.far_mono[i] = ctx.ref_cons.pop().unwrap_or(0.0);
        }
        // Cancel the loudspeaker echo from the mic, preserving the singing voice.
        ctx.aec
            .process(&ctx.mic_mono[..n], &ctx.far_mono[..n], &mut ctx.clean_mono[..n]);
        // Clean up (high-pass + expander) then apply the (user-adjustable) record gain, and
        // duplicate the clean mono voice up to the interleaved-stereo feed.
        let gain = f32::from_bits(INPUT_GAIN.load(Ordering::Relaxed));
        for i in 0..n {
            let s = makeup(ctx.cleanup.process(ctx.clean_mono[i]), gain);
            ctx.scratch[2 * i] = s;
            ctx.scratch[2 * i + 1] = s;
        }
        ctx.input.feed_rt(&ctx.scratch[..n * CHANNELS]);
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
        ctx.rt.render_playback(scratch, frames);
        // The backing plays at its mixer level during recording (no auto-duck) — the singer lowers
        // it themselves if they want less bleed. The ref ring + speaker see the same signal, so the
        // AEC stays consistent (it cancels whatever the speaker actually plays).
        // Publish the mono backing to the AEC far-end ring (the echo reference for input_cb). Drops
        // on a full ring (back-pressure; input_cb keeps it drained 1:1 every duplex cycle).
        for i in 0..frames {
            let _ = ctx.ref_prod.push(0.5 * (scratch[2 * i] + scratch[2 * i + 1]));
        }
        // Self-monitor: add the singer's voice to the OUTPUT (headphones) — AFTER the reference is
        // captured, so the voice never enters the AEC reference. Silent at monitor gain 0 (the
        // default, e.g. on a speaker where it would feed back); the mixer's monitor fader raises it.
        ctx.rt.mix_monitor(scratch, frames);

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

/// Request a small IO buffer on the device(s) the VPIO unit binds (cuts monitor/record latency and
/// keeps the speaker→mic echo inside the AEC's filter), and return the estimated record round-trip
/// in frames: ACOUSTIC flight + each leg's (buffer + safety offset + hardware latency). The AEC's
/// own block latency is added by the caller. Used to time-align recorded takes on the timeline.
///
/// `device_uid` is the mic the caller is about to bind (via `kAudioOutputUnitProperty_CurrentDevice`
/// below). When it resolves, VPIO uses that SINGLE device for both the input and output legs (a
/// communication endpoint), so both scopes are measured on it — a non-default mic has its own
/// buffer/latency/safety, and measuring the system defaults instead misaligns takes. When it does
/// NOT resolve, VPIO falls back to the system default INPUT for capture and default OUTPUT for
/// playback (two distinct devices), so each leg is measured on its own default, as before.
fn tune_io_and_round_trip(device_uid: Option<&str>) -> u32 {
    use objc2_core_audio::{
        kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyLatency,
        kAudioDevicePropertySafetyOffset, kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    };
    const WANT_FRAMES: u32 = 256;
    /// Speaker→mic acoustic flight on a laptop (~0.3 m ≈ 1.3 ms).
    const ACOUSTIC_FRAMES: u32 = 64;
    let mut round_trip = ACOUSTIC_FRAMES;
    unsafe {
        // Resolve the device the VPIO unit will actually bind. Some(dev) → one duplex device serves
        // both legs; None → measure each leg on its respective system default (the unbound case).
        let bound = device_uid.and_then(device_id_for_uid);
        let legs: [(Option<u32>, u32); 2] = match bound {
            Some(dev) => [
                (Some(dev), kAudioObjectPropertyScopeInput),
                (Some(dev), kAudioObjectPropertyScopeOutput),
            ],
            None => [
                (
                    dev_get_u32(
                        kAudioObjectSystemObject as u32,
                        kAudioHardwarePropertyDefaultInputDevice,
                        kAudioObjectPropertyScopeGlobal,
                    ),
                    kAudioObjectPropertyScopeInput,
                ),
                (
                    dev_get_u32(
                        kAudioObjectSystemObject as u32,
                        kAudioHardwarePropertyDefaultOutputDevice,
                        kAudioObjectPropertyScopeGlobal,
                    ),
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
            let buf = dev_get_u32(dev, kAudioDevicePropertyBufferFrameSize, kAudioObjectPropertyScopeGlobal).unwrap_or(WANT_FRAMES);
            let latency = dev_get_u32(dev, kAudioDevicePropertyLatency, lscope).unwrap_or(0);
            let safety = dev_get_u32(dev, kAudioDevicePropertySafetyOffset, lscope).unwrap_or(0);
            round_trip += buf + latency + safety;
        }
    }
    round_trip
}

// ---- UID → AudioDeviceID via the HAL ----------------------------------------------------------

/// Resolve a CoreAudio device UID to an `AudioDeviceID`. `None` on any failure (caller falls back
/// to the unit's default device). Not RT — called only during `start`.
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
