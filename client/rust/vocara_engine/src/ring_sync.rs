//! Pure, unit-testable sync logic for the VPIO far-end reference ring (`vpio.rs`).
//!
//! In the duplex AEC path the output callback pushes one mono reference sample per rendered
//! frame and the input callback pops one per captured frame — a strict 1:1 flow. Any input-side
//! glitch that skips pops (a failed `AudioUnitRender`, an empty capture, or the callback not
//! firing at all) leaves a *permanent* backlog in the ring: the popped reference then lags the
//! mic by the backlog, the echo becomes non-causal relative to the reference stream, and the
//! adaptive filter in `aec.rs` can never re-converge. [`RefRingSync`] decides how many samples
//! the input callback must discard to keep the ring at its healthy depth.
//!
//! Trimming errs on the safe side: discarding too much only ADDS modeled delay (the filter
//! covers it within its tail), while trimming too little leaves the echo non-causal forever.
//! All methods are RT-safe (no alloc / lock / syscall; bounded work).

/// Consecutive above-baseline successful callbacks required before a silent-gap backlog is
/// trimmed. Normal duplex phase jitter makes the depth fluctuate and dip back to the baseline
/// within a couple of cycles; a real backlog (the callback did not fire for a while) never dips.
const GAP_WINDOW: u32 = 8;

/// Tracks the reference ring's healthy depth and returns discard counts for the input callback.
pub struct RefRingSync {
    /// Healthy ring depth (samples), recorded right after the first successful capture's pops.
    baseline: Option<usize>,
    /// Upper bound applied to the recorded baseline. An enshrined baseline of `d` samples keeps
    /// the popped far-end permanently `d` samples stale, shifting the echo the AEC must model
    /// from the IO round trip `R` down to `R - d`: at `d >= R` the echo is non-causal and the
    /// filter can NEVER cancel it, so this bound must stay well below the modeled round trip —
    /// derive it with [`baseline_cap`], not from an arbitrary buffer-size constant.
    max_baseline: usize,
    /// Whether the previous callback failed (mirrors the caller's error streak).
    failing: bool,
    /// Consecutive successful callbacks whose depth stayed above the baseline.
    excess_run: u32,
    /// Minimum excess over the baseline observed during the current above-baseline run.
    min_excess: usize,
}

impl RefRingSync {
    pub const fn new(max_baseline: usize) -> RefRingSync {
        RefRingSync {
            baseline: None,
            max_baseline,
            failing: false,
            excess_run: 0,
            min_excess: usize::MAX,
        }
    }

    /// The callback fired but captured nothing (render failure or an empty success). The output
    /// side still pushed this cycle's reference, so the caller must discard `frames` samples to
    /// keep push/pop 1:1. Returns the discard count.
    pub fn on_failure(&mut self, frames: usize) -> usize {
        self.failing = true;
        frames
    }

    /// A successful capture; `depth` is the ring depth right after the normal 1:1 pops. Returns
    /// how many EXTRA samples to discard so the ring returns to its healthy baseline depth.
    ///
    /// MEASUREMENT CONTRACT: derive `depth` from a SINGLE ring-length snapshot taken at
    /// callback entry (`entry_len.saturating_sub(pops)`), never from a live `len()` read after
    /// the pops. The producer runs concurrently (distinct input/output devices = distinct IO
    /// threads), so a push racing a post-pop read inflates the reading; if that race aligns
    /// for a full window the inflation gets trimmed as if it were backlog and the next cycle
    /// underruns — recurring zeros injected into the far-end stream. An entry snapshot can
    /// only UNDER-estimate the depth (pushes after the snapshot are not counted), which at
    /// worst delays a legitimate trim by one cycle.
    pub fn on_success(&mut self, depth: usize) -> usize {
        let Some(base) = self.baseline else {
            // First successful capture: the settled depth is the healthy steady state (it
            // reflects the output/input callback phase, which the AEC's converged round-trip
            // estimate absorbs) — capped so a late first capture doesn't bake in a backlog.
            let base = depth.min(self.max_baseline);
            self.baseline = Some(base);
            self.failing = false;
            return depth - base;
        };
        let excess = depth.saturating_sub(base);
        if self.failing {
            // Recovery from a failure streak: everything above the baseline is backlog from
            // cycles the input side missed. Trim it all at once (over-trimming is safe).
            self.failing = false;
            self.excess_run = 0;
            self.min_excess = usize::MAX;
            return excess;
        }
        // Silent-gap detection: the callback simply did not fire for a while (no error was ever
        // reported), leaving a standing backlog. Trim only the MINIMUM excess observed over a
        // full window of consecutive above-baseline callbacks, so normal phase jitter (which
        // dips back to the baseline and resets the window) is never shed.
        if excess == 0 {
            self.excess_run = 0;
            self.min_excess = usize::MAX;
            return 0;
        }
        self.excess_run += 1;
        self.min_excess = self.min_excess.min(excess);
        if self.excess_run >= GAP_WINDOW {
            let trim = self.min_excess;
            self.excess_run = 0;
            self.min_excess = usize::MAX;
            trim
        } else {
            0
        }
    }
}

/// Upper bound for the recorded baseline depth, derived from the modeled IO round trip.
///
/// An enshrined baseline of `d` samples keeps the popped far-end permanently `d` samples
/// stale, which shifts the echo the AEC must model from `io_round_trip` down to
/// `io_round_trip - d`: at `d >= io_round_trip` the echo is non-causal and the filter can
/// NEVER cancel it. Capping at HALF the round trip keeps at least half the modeled delay as
/// causal margin while still admitting the normal ~one-buffer startup phase offset.
/// Over-capping errs safe (trimming below the true healthy depth only ADDS modeled delay,
/// covered by the filter tail). `hard_max` bounds degenerate round trips (e.g. an 8 kHz HFP
/// route whose buffer legs scale to tens of thousands of engine frames).
pub fn baseline_cap(io_round_trip_frames: u32, hard_max: usize) -> usize {
    ((io_round_trip_frames / 2) as usize).min(hard_max)
}

/// How `Vpio::start` must react to the duplex element rates negotiated at initialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplexRateCheck {
    /// Both elements negotiated the engine rate: full duplex with echo cancellation.
    AecOn,
    /// The OUTPUT element is at the engine rate but the INPUT element is not (VPIO
    /// occasionally negotiates its internal voice rate, e.g. 24 kHz, on the mic element).
    /// The far-end ring would then be pushed and popped at different rates — the reference
    /// drifts without bound and the AEC can never converge — but capture itself is fine
    /// (`InputCell` resamples any rate), so the duplex runs with the AEC BYPASSED rather
    /// than trading a working mic (with echo bleed) for no mic at all.
    BypassAec,
    /// The OUTPUT element did not negotiate the engine rate: `output_cb` writes engine-rate
    /// samples without resampling, so playback itself would be pitched/corrupted — bypassing
    /// the AEC cannot save it. Refuse to start; the caller falls back (default input, then a
    /// plain cpal output without a mic).
    Refuse,
}

/// Classify the negotiated duplex element rates (see [`DuplexRateCheck`]). A non-finite or
/// non-positive input rate refuses defensively: the capture resampler cannot be configured
/// from it either. (In practice `Layout::from` never yields one.)
pub fn classify_duplex_rates(in_rate: f64, out_rate: f64, expected: f64) -> DuplexRateCheck {
    let at_engine_rate = |r: f64| r.is_finite() && (r - expected).abs() < 0.5;
    if !at_engine_rate(out_rate) || !in_rate.is_finite() || in_rate <= 0.0 {
        DuplexRateCheck::Refuse
    } else if at_engine_rate(in_rate) {
        DuplexRateCheck::AecOn
    } else {
        DuplexRateCheck::BypassAec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature model of the real callback flow: `depth` mirrors the ring, the sync tracker
    /// returns discards exactly as `vpio::input_cb` applies them.
    struct Model {
        sync: RefRingSync,
        depth: usize,
    }

    impl Model {
        fn new(max_baseline: usize) -> Model {
            Model {
                sync: RefRingSync::new(max_baseline),
                depth: 0,
            }
        }

        /// One duplex cycle where the input callback runs and captures normally.
        fn cycle_ok(&mut self, frames: usize) {
            self.depth += frames; // output_cb pushes
            self.depth -= frames.min(self.depth); // input_cb pops 1:1 (zero-fill when short)
            let trim = self.sync.on_success(self.depth);
            self.depth -= trim.min(self.depth);
        }

        /// One duplex cycle where the input callback fires but captures nothing.
        fn cycle_failed(&mut self, frames: usize) {
            self.depth += frames;
            let discard = self.sync.on_failure(frames);
            self.depth -= discard.min(self.depth);
        }

        /// One duplex cycle where the input callback does not fire at all (silent gap).
        fn cycle_gap(&mut self, frames: usize) {
            self.depth += frames;
        }
    }

    const F: usize = 480; // frames per cycle

    #[test]
    fn normal_flow_never_trims() {
        let mut m = Model::new(4096);
        m.depth = 512; // standing phase offset between the callbacks
        for _ in 0..100 {
            m.cycle_ok(F);
        }
        assert_eq!(
            m.depth, 512,
            "steady 1:1 flow must keep the baseline depth untouched"
        );
    }

    #[test]
    fn failed_callbacks_keep_push_pop_one_to_one() {
        let mut m = Model::new(4096);
        m.depth = 512;
        m.cycle_ok(F); // records the baseline
        for _ in 0..5 {
            m.cycle_failed(F); // render failure: discard keeps the flow 1:1
        }
        m.cycle_ok(F);
        assert_eq!(
            m.depth, 512,
            "failure-with-callback must not accumulate backlog"
        );
    }

    #[test]
    fn failure_streak_recovery_trims_gap_backlog_to_baseline() {
        let mut m = Model::new(4096);
        m.depth = 512;
        m.cycle_ok(F);
        // A mixed outage: some cycles the callback fails, some it does not fire at all.
        m.cycle_failed(F);
        m.cycle_gap(F);
        m.cycle_gap(F);
        m.cycle_failed(F);
        assert!(m.depth > 512, "the silent cycles left a backlog");
        m.cycle_ok(F); // recovery: trims straight back to the baseline
        assert_eq!(
            m.depth, 512,
            "recovery from a failure streak must restore the baseline"
        );
    }

    #[test]
    fn silent_gap_without_errors_recovers_within_the_window() {
        let mut m = Model::new(4096);
        m.depth = 512;
        m.cycle_ok(F);
        for _ in 0..3 {
            m.cycle_gap(F); // the callback never fires — no failure is ever reported
        }
        assert_eq!(m.depth, 512 + 3 * F);
        for _ in 0..(GAP_WINDOW as usize + 1) {
            m.cycle_ok(F);
        }
        assert_eq!(
            m.depth, 512,
            "a persistent backlog must be trimmed back to the baseline"
        );
    }

    #[test]
    fn phase_jitter_that_dips_to_baseline_is_never_trimmed() {
        let mut s = RefRingSync::new(4096);
        assert_eq!(s.on_success(512), 0); // records baseline = 512
        for _ in 0..100 {
            // Output-ahead phase: the depth reads one buffer above the baseline for a few
            // cycles, then realigns BEFORE a full window elapses — the dip resets the window,
            // so healthy jitter is never shed (shedding warps the far-end stream).
            for _ in 0..GAP_WINDOW - 1 {
                assert_eq!(s.on_success(512 + F), 0, "jitter must not trim");
            }
            assert_eq!(s.on_success(512), 0);
        }
    }

    #[test]
    fn late_first_capture_is_capped_below_the_causal_round_trip() {
        // The output side ran ~1 s before the input callback ever fired. The enshrined
        // baseline keeps the far-end permanently that many samples stale, so it must stay
        // WELL below the modeled IO round trip — at or past it the echo is non-causal and
        // the AEC is dead for the whole session (the old MAX_FRAMES = 4096 cap allowed
        // exactly that: 4096 >> a ~1024-frame round trip).
        let io_round_trip: u32 = 1024; // typical: 2 legs x (256 buf + safety + latency) + flight
        let cap = baseline_cap(io_round_trip, 4096);
        let mut m = Model::new(cap);
        for _ in 0..100 {
            m.cycle_gap(F);
        }
        m.cycle_ok(F);
        assert_eq!(
            m.depth, cap,
            "the startup backlog must be capped at the causal bound"
        );
        assert!(
            (m.depth as u32) < io_round_trip,
            "the enshrined baseline must leave the echo causal: depth {} >= round trip {}",
            m.depth,
            io_round_trip
        );
        for _ in 0..200 {
            m.cycle_ok(F);
        }
        assert_eq!(
            m.depth, cap,
            "and the capped depth is the stable baseline afterwards"
        );
    }

    #[test]
    fn baseline_cap_leaves_causal_margin_and_bounds_degenerate_round_trips() {
        assert_eq!(baseline_cap(1024, 4096), 512, "half the round trip");
        assert_eq!(
            baseline_cap(0, 4096),
            0,
            "no round trip, no admissible backlog"
        );
        // An 8 kHz HFP route scales its buffer legs to tens of thousands of engine frames;
        // the hard ceiling keeps the baseline inside the provisioned scratch either way.
        assert_eq!(baseline_cap(60_000, 4096), 4096);
    }

    #[test]
    fn concurrent_push_racing_the_measurement_never_trims_with_an_entry_snapshot() {
        // Cross-thread duplex (distinct input/output devices): the output callback can push
        // between the input callback's pops and its depth read. Model a stretch where 96
        // samples of the next cycle's push consistently land in that window:
        //  - a live post-pop `len()` read (the pre-fix caller) sees the inflation, trims it
        //    as if it were backlog, and the next cycle underruns — recurring zeros injected
        //    into the far-end stream (this arm guards that the model reproduces the bug);
        //  - the entry-snapshot measurement `entry_len - pops` (what `vpio::input_cb` feeds
        //    now) never sees the racing push, so nothing is ever trimmed.
        for (entry_snapshot, expect_corruption) in [(false, true), (true, false)] {
            let mut sync = RefRingSync::new(4096);
            let mut depth = 0usize; // the real ring
            let mut trims = 0usize;
            let mut zeros = 0usize; // underrun zero-fills handed to the AEC
            let mut cycle = |depth: &mut usize, this_push: usize, racing: usize| {
                *depth += this_push; // this cycle's push (landed before the callback)
                let entry = *depth;
                let got = F.min(*depth); // the normal 1:1 pops
                zeros += F - got;
                *depth -= got;
                *depth += racing; // head of the NEXT push, racing the measurement
                let measured = if entry_snapshot {
                    entry.saturating_sub(F)
                } else {
                    *depth
                };
                let trim = sync.on_success(measured);
                trims += trim;
                *depth -= trim.min(*depth);
            };
            // Clean warm-up (no race): records baseline 0.
            for _ in 0..20 {
                cycle(&mut depth, F, 0);
            }
            // 30-cycle race stretch: 96 samples of each NEXT push land mid-measurement, the
            // rest (F - 96) arrives before the following callback. The first racy cycle's
            // own push landed in full (only its successor races).
            cycle(&mut depth, F, 96);
            for _ in 0..29 {
                cycle(&mut depth, F - 96, 96);
            }
            if expect_corruption {
                assert!(
                    trims > 0 && zeros > 0,
                    "the live-len model must reproduce the race bug (trims={trims}, zeros={zeros})"
                );
            } else {
                assert_eq!(trims, 0, "an entry snapshot must never trim race inflation");
                assert_eq!(zeros, 0, "and must never provoke underrun zero-fills");
            }
        }
    }

    #[test]
    fn duplex_rate_classification() {
        use DuplexRateCheck::*;
        assert_eq!(classify_duplex_rates(48_000.0, 48_000.0, 48_000.0), AecOn);
        assert_eq!(
            classify_duplex_rates(48_000.0000001, 48_000.0, 48_000.0),
            AecOn
        );
        // VPIO occasionally negotiates its internal voice rate on the mic element: capture
        // still works (InputCell resamples any rate), so the duplex must run with the AEC
        // bypassed — never trade a working mic (echo bleed) for no mic at all.
        assert_eq!(
            classify_duplex_rates(24_000.0, 48_000.0, 48_000.0),
            BypassAec
        );
        // A mismatched OUTPUT element corrupts playback itself (output_cb writes engine-rate
        // samples without resampling): refuse, the caller falls back to a cpal output.
        assert_eq!(classify_duplex_rates(48_000.0, 44_100.0, 48_000.0), Refuse);
        assert_eq!(classify_duplex_rates(44_100.0, 44_100.0, 48_000.0), Refuse);
        assert_eq!(classify_duplex_rates(f64::NAN, 48_000.0, 48_000.0), Refuse);
        assert_eq!(classify_duplex_rates(24_000.0, f64::NAN, 48_000.0), Refuse);
    }
}
