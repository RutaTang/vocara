//! The app-wide throttle for heavy analysis computes (port of
//! `lib/utils/analysis_gate.dart`).
//!
//! Dart's gate is an async semaphore on one isolate. The Rust services run
//! analyses on worker threads (`spawn_blocking`), so this gate is a blocking
//! one: [`AnalysisGate::run`] blocks the calling worker thread while it waits
//! its turn, with the same FIFO order and the same slot hand-off. It has no
//! GUI or async-runtime dependency.

use std::collections::{HashSet, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

struct GateState {
    running: usize,
    next_ticket: u64,
    /// Tickets waiting for a slot, oldest first.
    waiters: VecDeque<u64>,
    /// Tickets handed the slot of a job that finished.
    granted: HashSet<u64>,
}

/// App-wide throttle for heavy analysis computes.
///
/// One clip analysis legally peaks at hundreds of MB (10-minute cap, see
/// MediaAnalysisService); nothing used to stop different surfaces — a
/// selected take, a lane aggregate, a technique example — from launching
/// several at once and stacking those peaks. Every heavy compute funnels
/// through [`AnalysisGate::run`]; excess callers WAIT (FIFO) instead of
/// stacking memory.
///
/// The limit of 2 bounds the worst legal stack at roughly one gigabyte
/// while keeping one background compute rolling behind the interactive one.
/// Playback staging decodes are deliberately NOT gated: they are
/// latency-critical, bounded by their own budgets, and starving playback
/// behind an analysis queue would be a worse trade.
pub struct AnalysisGate {
    limit: usize,
    state: Mutex<GateState>,
    wake: Condvar,
}

impl AnalysisGate {
    /// A gate admitting [limit] computes at once (must be > 0: a gate must
    /// admit someone).
    pub fn new(limit: usize) -> AnalysisGate {
        assert!(limit > 0, "a gate must admit someone");
        AnalysisGate {
            limit,
            state: Mutex::new(GateState {
                running: 0,
                next_ticket: 0,
                waiters: VecDeque::new(),
                granted: HashSet::new(),
            }),
            wake: Condvar::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of computes currently inside the gate (for tests/diagnostics).
    pub fn running(&self) -> usize {
        self.lock().running
    }

    /// Number of computes queued behind the gate (for tests/diagnostics).
    pub fn queued(&self) -> usize {
        self.lock().waiters.len()
    }

    /// Runs [work] once a slot is free, blocking the caller until then. The
    /// slot is freed when [work] returns or panics.
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        {
            let mut s = self.lock();
            if s.running < self.limit {
                s.running += 1;
            } else {
                // Woken with the slot of the job that finished, never counted free in
                // between: counted free, a caller reaching here before the waiter
                // resumed took it, and the waiter queued again behind everyone after it
                // (a selected take waited out the lane merges queued after it).
                let ticket = s.next_ticket;
                s.next_ticket += 1;
                s.waiters.push_back(ticket);
                while !s.granted.remove(&ticket) {
                    s = self.wake.wait(s).unwrap_or_else(|e| e.into_inner());
                }
            }
        }
        let _slot = Slot(self);
        work()
    }

    fn release(&self) {
        let mut s = self.lock();
        match s.waiters.pop_front() {
            Some(next) => {
                s.granted.insert(next);
                drop(s);
                self.wake.notify_all();
            }
            None => s.running -= 1,
        }
    }
}

/// Frees its gate's slot on drop (Dart's `finally`): a job that panics does
/// not wedge the gate.
struct Slot<'a>(&'a AnalysisGate);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The single shared gate every analysis service routes through.
pub fn analysis_gate() -> &'static AnalysisGate {
    static GATE: OnceLock<AnalysisGate> = OnceLock::new();
    GATE.get_or_init(|| AnalysisGate::new(2))
}

#[cfg(test)]
mod tests {
    //! Port of the AnalysisGate group of `client/test/analysis_gate_test.dart`,
    //! on threads; every wait is for a condition, never for a time.
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn until(cond: impl Fn() -> bool) {
        while !cond() {
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn admits_up_to_the_limit_and_queues_the_rest_fifo() {
        let gate = Arc::new(AnalysisGate::new(2));
        let started = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(Mutex::new(Vec::new()));
        let mut releases = Vec::new();
        let mut jobs = Vec::new();
        for n in 1..=3 {
            let (tx, rx) = mpsc::channel::<()>();
            releases.push(tx);
            let (g, job_started, job_done) = (gate.clone(), started.clone(), done.clone());
            jobs.push(thread::spawn(move || {
                g.run(|| {
                    job_started.lock().unwrap().push(n);
                    rx.recv().unwrap();
                    job_done.lock().unwrap().push(n);
                })
            }));
            // Each job reaches the gate (and, if admitted, starts) before the
            // next one is started.
            until(|| {
                gate.running() + gate.queued() == n && started.lock().unwrap().len() == n.min(2)
            });
        }
        assert_eq!(gate.running(), 2, "two admitted");
        assert_eq!(gate.queued(), 1, "the third waits");
        assert_eq!(
            *started.lock().unwrap(),
            [1, 2],
            "job 3 must not have STARTED"
        );

        releases[0].send(()).unwrap();
        until(|| started.lock().unwrap().len() == 3);
        assert_eq!(gate.running(), 2, "job 3 admitted after a slot freed");

        // Released in turn (Dart's microtasks run them in that order).
        releases[1].send(()).unwrap();
        until(|| done.lock().unwrap().len() == 2);
        releases[2].send(()).unwrap();
        for j in jobs {
            j.join().unwrap();
        }
        assert_eq!(*done.lock().unwrap(), [1, 2, 3]);
        assert_eq!(gate.running(), 0);
    }

    #[test]
    fn a_waiter_is_not_overtaken_by_a_caller_arriving_as_it_wakes() {
        let gate = Arc::new(AnalysisGate::new(1));
        let order = Arc::new(Mutex::new(Vec::new()));
        let (release, released) = mpsc::channel::<()>();
        let looping = {
            let (gate, order) = (gate.clone(), order.clone());
            thread::spawn(move || {
                for i in 0..4 {
                    gate.run(|| {
                        if i == 0 {
                            released.recv().unwrap();
                        }
                        order.lock().unwrap().push(format!("L{i}"));
                    });
                }
            })
        };
        until(|| gate.running() == 1);
        let waiter = {
            let (gate, order) = (gate.clone(), order.clone());
            thread::spawn(move || gate.run(|| order.lock().unwrap().push("W".to_string())))
        };
        until(|| gate.queued() == 1);
        release.send(()).unwrap();
        looping.join().unwrap();
        waiter.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["L0", "W", "L1", "L2", "L3"]);
        assert_eq!(gate.running(), 0);
    }

    #[test]
    fn a_panicking_job_frees_its_slot() {
        let gate = Arc::new(AnalysisGate::new(1));
        let g = gate.clone();
        let result = thread::spawn(move || g.run(|| panic!("boom"))).join();
        assert!(result.is_err());
        // The gate must not be wedged.
        assert_eq!(gate.run(|| 42), 42);
        assert_eq!(gate.running(), 0);
    }
}
