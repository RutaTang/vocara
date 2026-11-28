//! Whether an OPEN mic stream is still delivering: the audio host's judgement before it keeps a
//! stream for a new arm. A stream the host keeps is trusted by every ready check — the applied
//! watermark and the running bit both say "up" — so a unit that stopped delivering (a route or
//! device change, a Bluetooth profile switch, a unit that never came alive) used to be kept for
//! every later take, each of which then waited out its ready timeout on a mic nothing would
//! ever restart. Judged stalled, it is reopened instead.
//!
//! The host samples the stream's emitted-frame counter on a slow tick while a stream is open and
//! again at each input sync (never per callback; the callback's only part is the dead-capture
//! flag it raises with one atomic store, see `InputCell::mark_dead`). A stream is stalled when:
//!
//! * its callback declared the capture dead (a streak of failed or empty renders after it had
//!   delivered, or a warm-up that ran out without one frame — the same rule that raises
//!   `EVENT_INPUT_LOST`); or
//! * it delivered, and its counter has not moved for [`STALL_QUIET`] (a unit that simply stops
//!   calling back raises nothing at all); or
//! * it has never delivered and has been open longer than [`INPUT_WARMUP`], the allowance a
//!   Bluetooth headset gets to switch to its headset profile.
//!
//! Pure and platform-free, so the decision is tested without hardware.

use std::time::{Duration, Instant};

/// How long a stream that delivered may go without a new frame before it counts as stalled.
/// A healthy unit delivers every IO cycle (~10 ms), so this is dozens of missed cycles.
pub const STALL_QUIET: Duration = Duration::from_millis(500);

/// How long a stream may stay silent from its open before it counts as stalled: longer than a
/// Bluetooth headset's switch to its headset profile, so a warming-up mic is not reopened (which
/// would only restart its warm-up). The VPIO callback's own dead-capture warm-up is the same
/// allowance, in frames.
pub const INPUT_WARMUP: Duration = Duration::from_millis(2500);

/// How often the host samples an open stream's frame counter.
pub const HEALTH_TICK: Duration = Duration::from_millis(250);

/// Why a stream is judged stalled (it reads as the reason in the reopen's diagnostics line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stall {
    /// The callback declared the capture dead.
    DeadCapture,
    /// Delivered, then no new frame for this long.
    Quiet(Duration),
    /// Open this long without one frame.
    NeverDelivered(Duration),
}

impl Stall {
    pub fn describe(&self) -> String {
        match self {
            Stall::DeadCapture => "the capture went dead".to_string(),
            Stall::Quiet(d) => format!("no new frames for {} ms", d.as_millis()),
            Stall::NeverDelivered(d) => {
                format!("no frame since it opened {} ms ago", d.as_millis())
            }
        }
    }
}

/// The open stream's delivery record (see the module doc). `None` fields: no stream open.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputHealth {
    open: Option<Open>,
}

#[derive(Clone, Copy, Debug)]
struct Open {
    opened_at: Instant,
    /// The counter at the last observation.
    frames: u64,
    /// When an observation last saw the counter move (the open, before the first frame).
    moved_at: Instant,
    delivered: bool,
    /// The stall already reported (so the host logs a stall once, not every tick).
    reported: bool,
}

/// What an observation saw change, for the host to log once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The stream delivered its first frame.
    FirstFrames,
    /// The stream stopped delivering.
    Stalled(Stall),
    /// A stalled stream delivered again.
    Recovered,
}

impl InputHealth {
    pub const fn new() -> InputHealth {
        InputHealth { open: None }
    }

    /// A stream just opened; `frames` is the counter now.
    pub fn opened(&mut self, frames: u64, now: Instant) {
        self.open = Some(Open {
            opened_at: now,
            frames,
            moved_at: now,
            delivered: false,
            reported: false,
        });
    }

    /// The stream closed.
    pub fn closed(&mut self) {
        self.open = None;
    }

    /// Sample the counter; returns what changed since the last sample, if anything worth a line.
    /// `dead`: the callback declared the capture dead.
    pub fn observe(&mut self, frames: u64, dead: bool, now: Instant) -> Option<Change> {
        let o = self.open.as_mut()?;
        if frames != o.frames {
            o.frames = frames;
            o.moved_at = now;
            let first = !o.delivered;
            o.delivered = true;
            if o.reported && !dead {
                o.reported = false;
                return Some(Change::Recovered);
            }
            if first {
                return Some(Change::FirstFrames);
            }
            return None;
        }
        let stall = self.stall(dead, now)?;
        let o = self.open.as_mut()?;
        if o.reported {
            return None;
        }
        o.reported = true;
        Some(Change::Stalled(stall))
    }

    /// Whether the open stream is stalled as of the last sample and `now` (see the module doc).
    /// `None` when healthy, warming up, or no stream is open.
    pub fn stall(&self, dead: bool, now: Instant) -> Option<Stall> {
        let o = self.open.as_ref()?;
        if dead {
            return Some(Stall::DeadCapture);
        }
        if o.delivered {
            let quiet = now.saturating_duration_since(o.moved_at);
            (quiet >= STALL_QUIET).then_some(Stall::Quiet(quiet))
        } else {
            let open = now.saturating_duration_since(o.opened_at);
            (open >= INPUT_WARMUP).then_some(Stall::NeverDelivered(open))
        }
    }

    /// Whether the open stream has delivered since it opened.
    pub fn delivered(&self) -> bool {
        self.open.is_some_and(|o| o.delivered)
    }

    /// How long the open stream has been open, as of `now`.
    pub fn open_for(&self, now: Instant) -> Option<Duration> {
        self.open
            .map(|o| now.saturating_duration_since(o.opened_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_open_is_never_stalled() {
        let h = InputHealth::new();
        assert_eq!(h.stall(true, Instant::now()), None);
    }

    #[test]
    fn a_delivering_stream_is_healthy_until_its_counter_stands_still_for_the_quiet_limit() {
        let t0 = Instant::now();
        let mut h = InputHealth::new();
        h.opened(100, t0);
        assert_eq!(
            h.observe(612, false, t0 + ms(250)),
            Some(Change::FirstFrames)
        );
        assert_eq!(h.observe(1124, false, t0 + ms(500)), None);
        // Standing still, but not yet for the limit: kept.
        assert_eq!(h.observe(1124, false, t0 + ms(750)), None);
        assert_eq!(h.stall(false, t0 + ms(999)), None);
        // The limit after the last move: stalled, reported once.
        assert_eq!(
            h.observe(1124, false, t0 + ms(1000)),
            Some(Change::Stalled(Stall::Quiet(ms(500))))
        );
        assert_eq!(h.observe(1124, false, t0 + ms(1250)), None);
        assert_eq!(h.stall(false, t0 + ms(1250)), Some(Stall::Quiet(ms(750))));
        // It comes back: healthy again, and said so.
        assert_eq!(
            h.observe(2000, false, t0 + ms(1500)),
            Some(Change::Recovered)
        );
        assert_eq!(h.stall(false, t0 + ms(1600)), None);
    }

    #[test]
    fn a_stream_warming_up_is_not_stalled_until_the_warm_up_runs_out() {
        // A Bluetooth headset switching profiles delivers nothing for a while: reopening it
        // inside its allowance would only restart the switch.
        let t0 = Instant::now();
        let mut h = InputHealth::new();
        h.opened(0, t0);
        for t in (250..2500).step_by(250) {
            assert_eq!(h.observe(0, false, t0 + ms(t)), None, "at {t} ms");
        }
        assert_eq!(
            h.observe(0, false, t0 + ms(2500)),
            Some(Change::Stalled(Stall::NeverDelivered(ms(2500))))
        );
        assert!(!h.delivered());
    }

    #[test]
    fn a_dead_capture_is_stalled_at_once_whatever_the_counter() {
        let t0 = Instant::now();
        let mut h = InputHealth::new();
        h.opened(0, t0);
        assert_eq!(h.stall(true, t0), Some(Stall::DeadCapture));
        assert_eq!(
            h.observe(0, true, t0 + ms(10)),
            Some(Change::Stalled(Stall::DeadCapture))
        );
    }

    #[test]
    fn a_reopen_starts_a_fresh_record() {
        let t0 = Instant::now();
        let mut h = InputHealth::new();
        h.opened(0, t0);
        h.observe(480, false, t0 + ms(250));
        assert!(h.stall(false, t0 + ms(2000)).is_some());
        // Reopened at 2 s: the new stream gets its own warm-up, from its own open.
        h.opened(480, t0 + ms(2000));
        assert!(!h.delivered());
        assert_eq!(h.stall(false, t0 + ms(4000)), None);
        assert_eq!(h.open_for(t0 + ms(2600)), Some(ms(600)));
        h.closed();
        assert_eq!(h.stall(true, t0 + ms(9000)), None);
        assert_eq!(h.open_for(t0 + ms(9000)), None);
    }

    #[test]
    fn each_reason_reads_plainly() {
        assert_eq!(Stall::DeadCapture.describe(), "the capture went dead");
        assert_eq!(Stall::Quiet(ms(600)).describe(), "no new frames for 600 ms");
        assert_eq!(
            Stall::NeverDelivered(ms(2600)).describe(),
            "no frame since it opened 2600 ms ago"
        );
    }
}
