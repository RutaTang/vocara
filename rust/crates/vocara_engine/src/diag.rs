//! The engine's diagnostics lines: what the audio host and the VoiceProcessingIO glue decided
//! about the microphone (an input sync's outcome, a unit's negotiated format, a failed stop),
//! queued for the host app to copy into its own diagnostics log (`ve_take_diagnostic`). The
//! engine cannot reach that log itself, and stderr is not one.
//!
//! The queue is process-wide on purpose: two engines fighting over the one VoiceProcessingIO
//! unit a process gets are diagnosed from ONE interleaved record, each line tagged with the
//! engine that wrote it ([`next_engine_tag`]).
//!
//! Lines are written on state changes and failures only: control and host threads, never an
//! audio callback (the queue takes a lock and allocates). They carry no file paths and no
//! device UIDs — a UID can carry a serial number or a Bluetooth address; a device is described
//! by its role (selected, system default) and transport instead. The queue is bounded: past
//! [`QUEUE_CAP`] the oldest lines go, and the next line taken says how many.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::ring_sync::DuplexRateCheck;

/// Lines kept for the host to take; past it the oldest are dropped (and counted).
pub const QUEUE_CAP: usize = 256;

/// A bounded FIFO of diagnostics lines (see the module doc).
#[derive(Default)]
pub struct DiagQueue {
    lines: VecDeque<String>,
    dropped: u64,
}

impl DiagQueue {
    pub const fn new() -> DiagQueue {
        DiagQueue {
            lines: VecDeque::new(),
            dropped: 0,
        }
    }

    /// Append `line`, dropping the oldest when full.
    pub fn push(&mut self, line: String) {
        if self.lines.len() >= QUEUE_CAP {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    /// The oldest line, or `None` when empty. After an overflow the first line taken says how
    /// many were dropped.
    pub fn take(&mut self) -> Option<String> {
        if self.dropped > 0 {
            let n = std::mem::take(&mut self.dropped);
            return Some(format!("{n} older engine diagnostics lines were dropped"));
        }
        self.lines.pop_front()
    }
}

static QUEUE: Mutex<DiagQueue> = Mutex::new(DiagQueue::new());

/// Queue `msg` from engine `tag` (0 = not tied to one engine). Control and host threads only.
pub fn note(tag: u64, msg: &str) {
    let line = if tag == 0 {
        msg.to_string()
    } else {
        format!("engine {tag}: {msg}")
    };
    QUEUE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(line);
}

/// The oldest queued line, if any (`ve_take_diagnostic`).
pub fn take() -> Option<String> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner).take()
}

static NEXT_ENGINE_TAG: AtomicU64 = AtomicU64::new(1);

/// A process-unique tag for a new engine, starting at 1: what its lines are prefixed with, and
/// whose VoiceProcessingIO unit is whose.
pub fn next_engine_tag() -> u64 {
    NEXT_ENGINE_TAG.fetch_add(1, Ordering::Relaxed)
}

/// Replace every occurrence of `uid` in `text` with a neutral name: the open errors name the
/// device they could not find, and a UID can identify hardware.
pub fn scrub_uid(text: &str, uid: Option<&str>) -> String {
    match uid {
        Some(u) if !u.is_empty() => text.replace(u, "<selected input>"),
        _ => text.to_string(),
    }
}

/// One element of a negotiated stream format, as the verdict line describes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElementFormat {
    pub rate: f64,
    pub channels: usize,
    pub interleaved: bool,
}

impl ElementFormat {
    fn describe(&self) -> String {
        format!(
            "{} Hz {} ch {}",
            self.rate.round() as i64,
            self.channels,
            if self.interleaved || self.channels <= 1 {
                "interleaved"
            } else {
                "planar"
            }
        )
    }
}

/// The VoiceProcessingIO unit's format verdict, once it initialized: what each element
/// negotiated and what the duplex check made of it. `output` is `None` for an input-only unit.
/// `device` names the role of the bound mic ("selected", "system default") and `transport`
/// how it is attached, never its UID.
pub fn vpio_verdict(
    input: ElementFormat,
    output: Option<ElementFormat>,
    check: Option<DuplexRateCheck>,
    device: &str,
    transport: &str,
) -> String {
    let verdict = match check {
        None | Some(DuplexRateCheck::AecOn) => "echo cancellation on",
        Some(DuplexRateCheck::BypassAec) => "echo cancellation BYPASSED (input rate differs)",
        Some(DuplexRateCheck::Refuse) => "REFUSED (output not at the engine rate)",
    };
    let out = output.map_or_else(|| "none".to_string(), |o| o.describe());
    format!(
        "vpio format: input {}, output {out}: {verdict}; mic {device} ({transport})",
        input.describe()
    )
}

/// What one input sync (the host converging the mic to the desired state) did.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncOutcome {
    /// No stream was open; one opened as `gen`. `fell_back`: the selected input could not be
    /// opened and the system default was.
    Opened { gen: u64, fell_back: bool },
    /// The open stream was kept as is.
    Kept { gen: u64 },
    /// The open stream `from` was torn down and reopened as `to`, because of `why`.
    Reopened { from: u64, to: u64, why: String },
    /// The open (after any teardown) failed; nothing is open now.
    OpenFailed { err: String },
    /// The stream `gen` was closed: nothing wants the mic.
    Closed { gen: u64 },
    /// Nothing wanted, nothing open.
    Idle,
}

/// The input sync's line: `input sync seq S want on|off stream none|gen G -> outcome`. `None`
/// for an idle sync, which changes nothing and would only be noise.
pub fn sync_line(
    seq: u64,
    want: bool,
    stream: Option<u64>,
    outcome: &SyncOutcome,
) -> Option<String> {
    let stream = stream.map_or_else(|| "none".to_string(), |g| format!("gen {g}"));
    let done = match outcome {
        SyncOutcome::Idle => return None,
        SyncOutcome::Opened { gen, fell_back } => {
            if *fell_back {
                format!("opened gen {gen} (selected input unavailable; on the system default)")
            } else {
                format!("opened gen {gen}")
            }
        }
        SyncOutcome::Kept { gen } => format!("kept gen {gen}"),
        SyncOutcome::Reopened { from, to, why } => {
            format!("reopened gen {from} as gen {to} ({why})")
        }
        SyncOutcome::OpenFailed { err } => format!("open failed: {err}"),
        SyncOutcome::Closed { gen } => format!("closed gen {gen}"),
    };
    Some(format!(
        "input sync seq {seq} want {} stream {stream} -> {done}",
        if want { "on" } else { "off" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_is_fifo_bounded_and_says_how_many_it_dropped() {
        let mut q = DiagQueue::new();
        for i in 0..QUEUE_CAP + 3 {
            q.push(format!("line {i}"));
        }
        assert_eq!(
            q.take().as_deref(),
            Some("3 older engine diagnostics lines were dropped")
        );
        assert_eq!(q.take().as_deref(), Some("line 3"));
        let rest = std::iter::from_fn(|| q.take()).count();
        assert_eq!(rest, QUEUE_CAP - 1);
        assert_eq!(q.take(), None);
    }

    #[test]
    fn engine_tags_are_unique_and_never_zero() {
        let a = next_engine_tag();
        let b = next_engine_tag();
        assert!(a >= 1 && b > a);
    }

    #[test]
    fn a_device_uid_never_reaches_a_line() {
        let err = "input device AC-12-34-56-78-9A:input not found";
        assert_eq!(
            scrub_uid(err, Some("AC-12-34-56-78-9A:input")),
            "input device <selected input> not found"
        );
        assert_eq!(scrub_uid(err, None), err);
        assert_eq!(scrub_uid(err, Some("")), err);
    }

    #[test]
    fn the_format_verdict_names_both_elements_and_the_duplex_check() {
        let mono = ElementFormat {
            rate: 48_000.0,
            channels: 1,
            interleaved: false,
        };
        let stereo = ElementFormat {
            rate: 48_000.0,
            channels: 2,
            interleaved: false,
        };
        assert_eq!(
            vpio_verdict(
                mono,
                Some(stereo),
                Some(DuplexRateCheck::AecOn),
                "selected",
                "usb"
            ),
            "vpio format: input 48000 Hz 1 ch interleaved, output 48000 Hz 2 ch planar: \
             echo cancellation on; mic selected (usb)"
        );
        let voice = ElementFormat {
            rate: 24_000.0,
            ..mono
        };
        assert_eq!(
            vpio_verdict(
                voice,
                Some(stereo),
                Some(DuplexRateCheck::BypassAec),
                "system default",
                "bluetooth"
            ),
            "vpio format: input 24000 Hz 1 ch interleaved, output 48000 Hz 2 ch planar: \
             echo cancellation BYPASSED (input rate differs); mic system default (bluetooth)"
        );
        let odd_out = ElementFormat {
            rate: 44_100.0,
            channels: 2,
            interleaved: true,
        };
        assert!(vpio_verdict(
            mono,
            Some(odd_out),
            Some(DuplexRateCheck::Refuse),
            "selected",
            "built-in"
        )
        .contains("output 44100 Hz 2 ch interleaved: REFUSED"));
        assert!(vpio_verdict(mono, None, None, "selected", "usb").contains("output none"));
    }

    #[test]
    fn each_sync_outcome_reads_as_one_line() {
        let line = |want, stream, o: SyncOutcome| sync_line(7, want, stream, &o);
        assert_eq!(
            line(
                true,
                None,
                SyncOutcome::Opened {
                    gen: 4,
                    fell_back: false
                }
            )
            .as_deref(),
            Some("input sync seq 7 want on stream none -> opened gen 4")
        );
        assert_eq!(
            line(
                true,
                None,
                SyncOutcome::Opened {
                    gen: 4,
                    fell_back: true
                }
            )
            .as_deref(),
            Some(
                "input sync seq 7 want on stream none -> opened gen 4 \
                 (selected input unavailable; on the system default)"
            )
        );
        assert_eq!(
            line(true, Some(3), SyncOutcome::Kept { gen: 3 }).as_deref(),
            Some("input sync seq 7 want on stream gen 3 -> kept gen 3")
        );
        assert_eq!(
            line(
                true,
                Some(3),
                SyncOutcome::Reopened {
                    from: 3,
                    to: 4,
                    why: "no frames for 600 ms".into()
                }
            )
            .as_deref(),
            Some("input sync seq 7 want on stream gen 3 -> reopened gen 3 as gen 4 (no frames for 600 ms)")
        );
        assert_eq!(
            line(
                true,
                None,
                SyncOutcome::OpenFailed {
                    err: "AudioUnitInitialize failed: -10875".into()
                }
            )
            .as_deref(),
            Some("input sync seq 7 want on stream none -> open failed: AudioUnitInitialize failed: -10875")
        );
        assert_eq!(
            line(false, Some(3), SyncOutcome::Closed { gen: 3 }).as_deref(),
            Some("input sync seq 7 want off stream gen 3 -> closed gen 3")
        );
        assert_eq!(line(false, None, SyncOutcome::Idle), None);
    }
}
