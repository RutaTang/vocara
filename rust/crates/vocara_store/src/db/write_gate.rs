//! The app-wide store write gate (port of `lib/data/db/write_gate.dart`).

use std::sync::{Condvar, Mutex, MutexGuard};

/// The app-wide store write gate: a shared/exclusive mutex serializing
/// ordinary WRITERS against store-wide MAINTENANCE.
///
/// Why it exists: a save's import+commit sequence is not atomic — the asset
/// row/pool file land first (`AssetStore::import_file`), the clip/media rows
/// that reference them commit later, with other work in between. A GC pass
/// running in that window (repair library between user actions) observes
/// the asset as unreferenced and tombstones it, or trashes the freshly
/// placed pool file / in-flight `.part`. The gate closes that window
/// structurally:
///
///  * [`WriteGate::shared`] — every save/import path (the repositories' save
///    cores, and the asset import itself as a belt-and-braces for any other
///    caller) runs under the shared side; any number may overlap.
///  * [`WriteGate::exclusive`] — repair library and the boot sweeps run
///    exclusively: they wait for every in-flight shared section to finish and
///    hold off new ones while they run.
///
/// Reader preference, by design: a NEW shared section only waits for a
/// RUNNING exclusive, never a queued one. That makes nested shared
/// acquisition safe (a save holding the gate while the import re-enters it
/// can never deadlock behind a queued repair). The cost is theoretical
/// writer starvation under a continuous stream of saves — acceptable because
/// maintenance is rare and user-triggered while saves are short.
///
/// NOT re-entrant on the exclusive side: an exclusive section must never call
/// [`WriteGate::exclusive`] (or [`WriteGate::shared`]) again — Persistence
/// keeps repair/sweep internals gate-free for exactly that reason.
///
/// Blocking: a section waits on the calling thread, which is never the UI
/// thread (the store runs on blocking tasks).
#[derive(Debug, Default)]
pub struct WriteGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct GateState {
    /// Number of shared sections currently running.
    shared: usize,
    /// Whether an exclusive section is RUNNING right now (not merely
    /// queued).
    exclusive_running: bool,
    /// The ticket the next exclusive caller takes (call order).
    next_ticket: u64,
    /// The ticket allowed to run next: exclusive sections are serialized
    /// among themselves in call order.
    serving: u64,
}

impl WriteGate {
    /// A fresh, open gate.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        // Sections run outside the lock, so a poisoned lock only means a
        // panic between two lines of bookkeeping that cannot panic: carry on.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, GateState>) -> MutexGuard<'a, GateState> {
        self.changed.wait(guard).unwrap_or_else(|e| e.into_inner())
    }

    /// Runs [op] under the shared side of the gate. Overlaps freely with
    /// other shared sections (including nested ones); waits only while an
    /// exclusive section is actively running.
    pub fn shared<T>(&self, op: impl FnOnce() -> T) -> T {
        {
            let mut s = self.lock();
            while s.exclusive_running {
                s = self.wait(s);
            }
            s.shared += 1;
        }
        // Released however op ends, a panic included.
        struct Release<'a>(&'a WriteGate);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let mut s = self.0.lock();
                s.shared -= 1;
                if s.shared == 0 {
                    self.0.changed.notify_all();
                }
            }
        }
        let _release = Release(self);
        op()
    }

    /// Runs [op] exclusively: after every in-flight shared section has
    /// finished, with new shared sections held off until [op] completes.
    /// Exclusive sections are serialized among themselves in call order.
    pub fn exclusive<T>(&self, op: impl FnOnce() -> T) -> T {
        {
            let mut s = self.lock();
            let ticket = s.next_ticket;
            s.next_ticket += 1;
            while s.serving != ticket {
                s = self.wait(s);
            }
            // Wait for the shared side to drain. Re-check after every
            // wake-up: a shared section may have slipped in while this one
            // was waking (reader preference admits it), so only a zero-count
            // observation under the lock may proceed.
            while s.shared > 0 {
                s = self.wait(s);
            }
            s.exclusive_running = true;
        }
        // The gate must survive a failed (or panicking) run.
        struct Release<'a>(&'a WriteGate);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                let mut s = self.0.lock();
                s.exclusive_running = false;
                s.serving += 1;
                self.0.changed.notify_all();
            }
        }
        let _release = Release(self);
        op()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::thread;
    use std::time::Duration;

    type Log = Arc<StdMutex<Vec<&'static str>>>;

    fn log() -> Log {
        Arc::new(StdMutex::new(Vec::new()))
    }

    fn push(log: &Log, entry: &'static str) {
        log.lock().unwrap().push(entry);
    }

    fn snapshot(log: &Log) -> Vec<&'static str> {
        log.lock().unwrap().clone()
    }

    #[test]
    fn exclusive_waits_for_every_in_flight_shared_section() {
        let gate = Arc::new(WriteGate::new());
        let log = log();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();

        let s = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.shared(|| {
                    push(&log, "shared-start");
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    push(&log, "shared-end");
                })
            })
        };
        started_rx.recv().unwrap();
        let x = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.exclusive(|| push(&log, "exclusive")))
        };

        // Give the exclusive every chance to (incorrectly) jump the queue.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(snapshot(&log), ["shared-start"]);

        release_tx.send(()).unwrap();
        s.join().unwrap();
        x.join().unwrap();
        assert_eq!(snapshot(&log), ["shared-start", "shared-end", "exclusive"]);
    }

    #[test]
    fn shared_sections_overlap_freely() {
        let gate = Arc::new(WriteGate::new());
        let (a_tx, a_rx) = mpsc::channel::<()>();
        let (b_tx, b_rx) = mpsc::channel::<()>();
        // Each section finishes only once the OTHER one has started:
        // completes only if the two genuinely overlap.
        let a = {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.shared(|| {
                    a_tx.send(()).unwrap();
                    b_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            })
        };
        let b = {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.shared(|| {
                    b_tx.send(()).unwrap();
                    a_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                })
            })
        };
        a.join().unwrap();
        b.join().unwrap();
    }

    #[test]
    fn nested_shared_acquisition_cannot_deadlock_behind_a_queued_exclusive() {
        let gate = Arc::new(WriteGate::new());
        let log = log();
        let (started_tx, started_rx) = mpsc::channel::<()>();

        let outer = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.shared(|| {
                    push(&log, "outer-start");
                    started_tx.send(()).unwrap();
                    // Let the exclusive queue up mid-section, then re-enter
                    // the gate the way the import does inside a save.
                    thread::sleep(Duration::from_millis(10));
                    gate.shared(|| push(&log, "nested"));
                    push(&log, "outer-end");
                })
            })
        };
        started_rx.recv().unwrap();
        let excl = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.exclusive(|| push(&log, "exclusive")))
        };

        outer.join().unwrap();
        excl.join().unwrap();
        assert_eq!(
            snapshot(&log),
            ["outer-start", "nested", "outer-end", "exclusive"]
        );
    }

    #[test]
    fn a_shared_section_starting_during_a_running_exclusive_waits() {
        let gate = Arc::new(WriteGate::new());
        let log = log();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();

        let x = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.exclusive(|| {
                    push(&log, "exclusive-start");
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    push(&log, "exclusive-end");
                })
            })
        };
        // Let the exclusive actually start before requesting the shared side.
        started_rx.recv().unwrap();
        let s = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.shared(|| push(&log, "shared")))
        };
        thread::sleep(Duration::from_millis(20));
        assert_eq!(snapshot(&log), ["exclusive-start"]);

        release_tx.send(()).unwrap();
        x.join().unwrap();
        s.join().unwrap();
        assert_eq!(
            snapshot(&log),
            ["exclusive-start", "exclusive-end", "shared"]
        );
    }

    #[test]
    fn exclusive_sections_are_serialized_in_call_order() {
        let gate = Arc::new(WriteGate::new());
        let log = log();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();

        let x1 = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || {
                gate.exclusive(|| {
                    push(&log, "x1-start");
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    push(&log, "x1-end");
                })
            })
        };
        started_rx.recv().unwrap();
        let x2 = {
            let (gate, log) = (gate.clone(), log.clone());
            thread::spawn(move || gate.exclusive(|| push(&log, "x2")))
        };

        thread::sleep(Duration::from_millis(20));
        assert_eq!(snapshot(&log), ["x1-start"]);

        release_tx.send(()).unwrap();
        x1.join().unwrap();
        x2.join().unwrap();
        assert_eq!(snapshot(&log), ["x1-start", "x1-end", "x2"]);
    }

    #[test]
    fn queued_exclusive_sections_run_in_the_order_they_were_called() {
        let gate = Arc::new(WriteGate::new());
        let log = log();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let hold = {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.exclusive(|| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
            })
        };
        started_rx.recv().unwrap();
        let mut queued = Vec::new();
        for name in ["a", "b", "c"] {
            let (gate, log) = (gate.clone(), log.clone());
            queued.push(thread::spawn(move || gate.exclusive(|| push(&log, name))));
            // Each takes its ticket before the next is spawned.
            thread::sleep(Duration::from_millis(10));
        }
        release_tx.send(()).unwrap();
        hold.join().unwrap();
        for q in queued {
            q.join().unwrap();
        }
        assert_eq!(snapshot(&log), ["a", "b", "c"]);
    }

    #[test]
    fn errors_propagate_and_never_wedge_the_gate() {
        let gate = WriteGate::new();
        let shared: Result<(), &str> = gate.shared(|| Err("shared boom"));
        assert_eq!(shared, Err("shared boom"));
        let exclusive: Result<(), &str> = gate.exclusive(|| Err("exclusive boom"));
        assert_eq!(exclusive, Err("exclusive boom"));
        // A panicking section releases the gate too.
        let gate = Arc::new(gate);
        let g = gate.clone();
        assert!(thread::spawn(move || g.exclusive(|| panic!("boom")))
            .join()
            .is_err());
        let g = gate.clone();
        assert!(thread::spawn(move || g.shared(|| panic!("boom")))
            .join()
            .is_err());
        // Both sides still work after the failures.
        assert_eq!(gate.shared(|| 1), 1);
        assert_eq!(gate.exclusive(|| 2), 2);
    }
}
