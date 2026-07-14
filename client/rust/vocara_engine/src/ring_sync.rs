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
    /// Upper bound applied to the recorded baseline: a first capture that fires late (after the
    /// output side already ran for a while) must not enshrine that startup backlog as "healthy".
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

/// Whether the negotiated duplex element rates keep the reference ring drift-free: the input
/// and output elements must run at the SAME rate (the ring is pushed/popped 1:1 in samples),
/// and both must match the engine rate (the output callback writes engine-rate samples without
/// resampling). Any mismatch drifts the far-end reference continuously — the backlog grows (or
/// drains) without bound and the AEC can never converge — so `Vpio::start` must refuse it.
pub fn duplex_rates_ok(in_rate: f64, out_rate: f64, expected: f64) -> bool {
    let ok = |r: f64| r.is_finite() && (r - expected).abs() < 0.5;
    ok(in_rate) && ok(out_rate)
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
            Model { sync: RefRingSync::new(max_baseline), depth: 0 }
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
        assert_eq!(m.depth, 512, "steady 1:1 flow must keep the baseline depth untouched");
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
        assert_eq!(m.depth, 512, "failure-with-callback must not accumulate backlog");
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
        assert_eq!(m.depth, 512, "recovery from a failure streak must restore the baseline");
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
        assert_eq!(m.depth, 512, "a persistent backlog must be trimmed back to the baseline");
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
    fn late_first_capture_is_capped_and_trimmed() {
        let mut m = Model::new(4096);
        // The output side ran ~1 s before the input callback ever fired.
        for _ in 0..100 {
            m.cycle_gap(F);
        }
        m.cycle_ok(F);
        assert_eq!(m.depth, 4096, "the startup backlog must be capped at max_baseline");
        for _ in 0..200 {
            m.cycle_ok(F);
        }
        assert_eq!(m.depth, 4096, "and the capped depth is the stable baseline afterwards");
    }

    #[test]
    fn duplex_rate_check() {
        assert!(duplex_rates_ok(48_000.0, 48_000.0, 48_000.0));
        assert!(duplex_rates_ok(48_000.0000001, 48_000.0, 48_000.0));
        // VPIO occasionally negotiates its internal voice rate on the mic element.
        assert!(!duplex_rates_ok(24_000.0, 48_000.0, 48_000.0));
        // Same-but-wrong on both elements still breaks playback (engine renders at 48 kHz).
        assert!(!duplex_rates_ok(44_100.0, 44_100.0, 48_000.0));
        assert!(!duplex_rates_ok(f64::NAN, 48_000.0, 48_000.0));
    }
}
