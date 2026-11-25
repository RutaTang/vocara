//! Port of `client/test/recording_finalizer_test.dart`.
//!
//! The quit-time finalize budget. Both quit paths (the close button's guard
//! and the Cmd+Q exit request) wait on `finalize_all` before letting the app
//! go, so an unbounded wait here is the one failure macOS resolves by
//! force-killing the app — which finalizes nothing at all.
//!
//! Dart's completers become [`Gate`]s, its unawaited futures threads. Where
//! Dart registered a hold or a landing synchronously before its next line,
//! the helpers here return only once the registration is made.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vocara_services::recording_finalizer::{Finalizer, RecordingFinalizer};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The tests here time a quit's budgets in tens of milliseconds, so they run
/// one at a time: side by side (cargo's default), their threads' sleeps and
/// wake-ups on a loaded CI runner overran those budgets with nothing wrong.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A one-shot signal (Dart's `Completer<void>`): [`Gate::wait`] returns once
/// [`Gate::open`] ran.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn open(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }

    fn wait(&self) {
        let mut open = self.0 .0.lock().unwrap();
        while !*open {
            open = self.0 .1.wait(open).unwrap();
        }
    }
}

/// A gate that opens when the test ends (Dart's `addTearDown` completing
/// `never`), so the threads parked on it finish.
#[derive(Default)]
struct Never(Gate);

impl Never {
    fn gate(&self) -> Gate {
        self.0.clone()
    }
}

impl Drop for Never {
    fn drop(&mut self) {
        self.0.open();
    }
}

fn shared(f: RecordingFinalizer) -> Arc<RecordingFinalizer> {
    Arc::new(f)
}

/// `finalizer.hold(work)` on a thread of its own, returned once the hold is
/// registered (Dart registers it synchronously, before the caller's next
/// line).
fn spawn_hold<T: Send + 'static>(
    f: &Arc<RecordingFinalizer>,
    work: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    let (tx, rx) = mpsc::channel();
    let f = Arc::clone(f);
    let handle = thread::spawn(move || {
        f.hold(move || {
            tx.send(()).unwrap();
            work()
        })
    });
    rx.recv().unwrap();
    handle
}

/// `finalizer.landing(id, work)` on a thread of its own, returned once the
/// take is listed.
fn spawn_landing<T: Send + 'static>(
    f: &Arc<RecordingFinalizer>,
    practice_id: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    let (tx, rx) = mpsc::channel();
    let f = Arc::clone(f);
    let handle = thread::spawn(move || {
        f.landing(practice_id, move || {
            tx.send(()).unwrap();
            work()
        })
    });
    rx.recv().unwrap();
    handle
}

/// A quit run under way on a thread of its own.
struct Quit {
    done: Arc<AtomicBool>,
    ended: mpsc::Receiver<()>,
}

impl Quit {
    fn start(f: &Arc<RecordingFinalizer>) -> Quit {
        let (tx, ended) = mpsc::channel();
        let done = Arc::new(AtomicBool::new(false));
        let (f, flag) = (Arc::clone(f), Arc::clone(&done));
        thread::spawn(move || {
            f.finalize_all();
            flag.store(true, Ordering::SeqCst);
            let _ = tx.send(());
        });
        Quit { done, ended }
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// `quit.timeout(limit)`: the test standing in for macOS's own watchdog.
    fn join_within(self, limit: Duration) {
        self.ended
            .recv_timeout(limit)
            .expect("the quit outlived the watchdog");
    }
}

/// `finalizer.finalizeAll().timeout(limit)`.
fn finalize_all_within(f: &Arc<RecordingFinalizer>, limit: Duration) {
    Quit::start(f).join_within(limit);
}

/// A finalizer that counts its calls, standing in for Dart's
/// `finishesWithoutWaiting`: a run whose budget is spent returns before
/// calling a single finalizer, so a probe registered before the earlier run
/// (and called by it) is not called again. A wall-clock bound on that failed
/// in Dart whenever the test isolate stalled, with nothing wrong.
fn probe(f: &RecordingFinalizer) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    f.register(
        &Finalizer::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        false,
    );
    calls
}

/// Waits until [calls] reaches [n]: a quit run started on another thread has
/// called its finalizers (Dart's `finalizeAll()` calls them synchronously,
/// before the caller's next line).
fn until_called(calls: &AtomicUsize, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while calls.load(Ordering::SeqCst) < n {
        assert!(
            Instant::now() < deadline,
            "the quit never called its finalizers"
        );
        thread::sleep(ms(1));
    }
}

#[test]
fn a_wedged_finalizer_cannot_hold_the_quit_and_does_not_starve_the_rest() {
    let _serial = serial();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(30))
            .with_total_budget(ms(500)),
    );
    let never = Never::default();
    let ran = Arc::new(AtomicUsize::new(0));
    let wedge = never.gate();
    f.register(&Finalizer::new(move || wedge.wait()), false);
    let counted = Arc::clone(&ran);
    f.register(
        &Finalizer::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
        false,
    );

    // The outer guard is the test's stand-in for macOS's own watchdog.
    finalize_all_within(&f, Duration::from_secs(2));

    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the wedged callback must not starve the next one"
    );
}

#[test]
fn a_stuck_progress_reading_holds_neither_the_saves_nor_a_quit_they_finished_in_time() {
    let _serial = serial();
    // Read first, the reading held every save back for its whole limit, and
    // the quit after they were done.
    let never = Never::default();
    let stuck = never.gate();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(Duration::from_secs(1))
            .with_total_budget(Duration::from_secs(3))
            .with_progress(move || {
                stuck.wait();
                0
            }),
    );
    let clock = Instant::now();
    let started = Arc::new(Mutex::new(None));
    let at = Arc::clone(&started);
    f.register(
        &Finalizer::new(move || *at.lock().unwrap() = Some(clock.elapsed())),
        false,
    );
    finalize_all_within(&f, Duration::from_secs(5));
    let started = started.lock().unwrap().expect("the finalizer ran");
    assert!(started < ms(500), "{started:?}");
    assert!(clock.elapsed() < RecordingFinalizer::READING_LIMIT);
}

#[test]
fn a_quit_whose_finalizers_all_settled_leaves_the_next_one_its_full_budget() {
    let _serial = serial();
    // A fresh run waits up to the total (150 ms) for the 20 ms save; a spent
    // one waits nothing. The per-finalizer window is wide, so only the total
    // tells them apart.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(300))
            .with_total_budget(ms(150)),
    );
    f.register(&Finalizer::new(|| {}), false);
    f.finalize_all();
    thread::sleep(ms(200));
    let landed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&landed);
    let save = spawn_hold(&f, move || {
        thread::sleep(ms(20));
        flag.store(true, Ordering::SeqCst);
    });
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        landed.load(Ordering::SeqCst),
        "the earlier quit spent its budget"
    );
    save.join().unwrap();
}

#[test]
fn a_take_armed_while_a_cut_off_quit_waited_is_still_finalized_by_the_exit_request() {
    let _serial = serial();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(50))
            .with_total_budget(ms(80)),
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait());
    let close = Quit::start(&f); // the close button's run
    thread::sleep(ms(10));
    let stopped = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&stopped);
    f.register(
        &Finalizer::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            thread::sleep(ms(20));
        }),
        true,
    );
    close.join_within(Duration::from_secs(2));
    thread::sleep(ms(40)); // spent
    finalize_all_within(&f, Duration::from_secs(2));
    assert_eq!(
        stopped.load(Ordering::SeqCst),
        1,
        "the exit request's run skipped the new take"
    );
}

#[test]
fn a_take_rearmed_on_the_same_screen_during_the_wait_is_finalized_too() {
    let _serial = serial();
    // The mic controller registers the SAME finalizer for every take: reusing
    // the first call's run, the exit request's run never stopped take 2.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(50))
            .with_total_budget(ms(80)),
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait());
    let takes = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let slot: Arc<OnceLock<Finalizer>> = Arc::new(OnceLock::new());
    let finalize_on_quit = Finalizer::new({
        let (f, slot, takes, stops) = (
            Arc::clone(&f),
            Arc::clone(&slot),
            Arc::clone(&takes),
            Arc::clone(&stops),
        );
        move || {
            if takes.load(Ordering::SeqCst) > stops.load(Ordering::SeqCst) {
                // stopTake()
                f.unregister(slot.get().unwrap());
                stops.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    slot.set(finalize_on_quit.clone()).unwrap();
    let arm = || {
        takes.fetch_add(1, Ordering::SeqCst);
        f.register(&finalize_on_quit, true);
    };

    arm();
    let close = Quit::start(&f);
    until_called(&stops, 1); // the close button's run stopped take 1
    thread::sleep(ms(10));
    arm(); // REC again while the close button's run waits
    close.join_within(Duration::from_secs(2));
    thread::sleep(ms(40)); // spent
    finalize_all_within(&f, Duration::from_secs(2));
    assert_eq!(stops.load(Ordering::SeqCst), 2, "take 2 was never stopped");
}

#[test]
fn a_take_armed_during_the_wait_and_stopped_by_the_user_keeps_its_own_budget() {
    let _serial = serial();
    // Its stop-and-save carries on under waits not marked starts_work; with
    // the take's own finalizer gone, the exit request's run found the budget
    // spent and exited mid-save.
    // The take's own budget is the per-finalizer window (300 ms, for the
    // 50 ms save); the spent total (60 ms) would give it nothing.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(300))
            .with_total_budget(ms(60)),
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait()); // a wedged save
    let started = probe(&f);
    let close = Quit::start(&f);
    until_called(&started, 1);
    thread::sleep(ms(10));
    let finalize_on_quit = Finalizer::new(|| {});
    f.register(&finalize_on_quit, true); // REC
    thread::sleep(ms(30));
    // The user's Stop: the take's finalizer goes, its save holds the quit.
    f.unregister(&finalize_on_quit);
    let saved = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saved);
    let save = spawn_hold(&f, move || {
        thread::sleep(ms(50));
        flag.store(true, Ordering::SeqCst);
    });
    close.join_within(Duration::from_secs(2));
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(saved.load(Ordering::SeqCst), "take 2's save at exit");
    save.join().unwrap();
}

#[test]
fn a_take_stopped_during_the_wait_with_nothing_to_save_earns_no_budget() {
    let _serial = serial();
    // A practice take holds nothing once stopped: counted as uncounted work
    // anyway, it gave the exit request's run a full wait on nothing.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(100))
            .with_total_budget(ms(60)),
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait()); // a wedged save
    let calls = probe(&f);
    let close = Quit::start(&f);
    thread::sleep(ms(10));
    let practice_take = Finalizer::new(|| {});
    f.register(&practice_take, true); // REC
    f.unregister(&practice_take); // Stop: nothing to save
    close.join_within(Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the close button's run");

    finalize_all_within(&f, Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the budget is spent");
}

#[test]
fn a_stops_tail_added_during_the_wait_is_waited_on_by_the_next_run() {
    let _serial = serial();
    // Reusing the first call's run, the exit request's run waited only on the
    // tails that existed then, and exited mid-save of the later one.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(200))
            .with_total_budget(ms(600)),
    );
    // A stable finalizer waiting on a growing set, like the mic controller's.
    let tails: Arc<Mutex<Vec<Gate>>> = Arc::default();
    let hold_for_stops = Finalizer::new({
        let tails = Arc::clone(&tails);
        move || {
            let snapshot = tails.lock().unwrap().clone();
            for tail in snapshot {
                tail.wait();
            }
        }
    });
    let first = Gate::default(); // take 1's save, slow
    tails.lock().unwrap().push(first.clone());
    f.register(&hold_for_stops, false);
    let close = Quit::start(&f);
    thread::sleep(ms(10));
    let saved = Arc::new(AtomicBool::new(false));
    let second = Gate::default();
    tails.lock().unwrap().push(second.clone());
    let later = {
        let (saved, second) = (Arc::clone(&saved), second.clone());
        thread::spawn(move || {
            thread::sleep(ms(300));
            saved.store(true, Ordering::SeqCst);
            second.open();
        })
    };
    close.join_within(Duration::from_secs(2)); // cut off at 200 ms
    first.open();
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        saved.load(Ordering::SeqCst),
        "the exit request's run missed the tail"
    );
    later.join().unwrap();
}

#[test]
fn a_takes_own_stop_and_save_earns_the_quit_no_second_budget() {
    let _serial = serial();
    // Stopping a take registers its tail and its save's hold as it goes;
    // counted as new work, they gave the exit request's run a full budget and
    // the quit ran past its total with nothing moving.
    // Budgets far enough apart that the right total (450 ms) and a second
    // full budget (800 ms) both sit ~200 ms from the bound: 70 ms of slack
    // failed on a loaded runner.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(400))
            .with_total_budget(ms(450)),
    );
    let never = Never::default();
    let slot: Arc<OnceLock<Finalizer>> = Arc::new(OnceLock::new());
    let finalize_on_quit = Finalizer::new({
        let (f, slot, never) = (Arc::clone(&f), Arc::clone(&slot), never.gate());
        move || {
            f.unregister(slot.get().unwrap());
            let stalled = never.clone();
            spawn_hold(&f, move || stalled.wait()); // the stalled save
            never.wait();
        }
    });
    slot.set(finalize_on_quit.clone()).unwrap();
    f.register(&finalize_on_quit, true);
    let sw = Instant::now();
    finalize_all_within(&f, Duration::from_secs(2));
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(sw.elapsed() < ms(650), "{:?}", sw.elapsed());
}

#[test]
fn a_quit_the_app_survived_leaves_the_next_one_a_fresh_budget() {
    let _serial = serial();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(30))
            .with_total_budget(ms(40))
            .with_progress_ceiling(ms(60)),
    );
    let never = Never::default();
    let wedge = never.gate();
    f.register(&Finalizer::new(move || wedge.wait()), false);
    finalize_all_within(&f, Duration::from_secs(2));
    thread::sleep(ms(150));
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    f.register(
        &Finalizer::new(move || flag.store(true, Ordering::SeqCst)),
        false,
    );
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        ran.load(Ordering::SeqCst),
        "the old quit left the budget spent for good"
    );
}

#[test]
fn several_wedged_finalizers_cost_the_quit_one_budget_not_one_each() {
    let _serial = serial();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(100))
            .with_total_budget(Duration::from_secs(5)),
    );
    let parked = Never::default();
    let entered = Arc::new(AtomicUsize::new(0));
    for _ in 0..5 {
        let (entered, parked) = (Arc::clone(&entered), parked.gate());
        f.register(
            &Finalizer::new(move || {
                entered.fetch_add(1, Ordering::SeqCst);
                parked.wait();
            }),
            false,
        );
    }

    let sw = Instant::now();
    finalize_all_within(&f, Duration::from_secs(2));

    assert_eq!(entered.load(Ordering::SeqCst), 5, "every finalizer runs");
    // Five budgets one after another would be 500 ms.
    assert!(sw.elapsed() < ms(400), "{:?}", sw.elapsed());
}

/// A finalizer for testing whose progress reads [copied].
/// The windows the copy tests run under. A save is read as stalled when its
/// reading has not moved for a whole [`WINDOW`], so the copier moves far more
/// often than that ([`STEP`]) and copies for longer than the [`TOTAL`] budget
/// ([`COPY`]), which is what the tests are about. The margins are wide on
/// purpose: on a loaded runner a 20 ms sleep overslept a 40 ms window, and a
/// copy still under way read as a stall.
const WINDOW: Duration = Duration::from_millis(150);
const TOTAL: Duration = Duration::from_millis(200);
const COPY: Duration = Duration::from_millis(400);
const STEP: Duration = Duration::from_millis(5);

fn copying(copied: &Arc<AtomicI64>) -> Arc<RecordingFinalizer> {
    let copied = Arc::clone(copied);
    shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(WINDOW)
            .with_total_budget(TOTAL)
            .with_progress_ceiling(Duration::from_secs(5))
            .with_progress(move || copied.load(Ordering::SeqCst)),
    )
}

/// Copies for [`COPY`] of wall-clock time, a megabyte more on disk every
/// [`STEP`] (a late wake-up costs steps, never the copy's length), calling
/// [at_step] with each step's index.
fn copy_for(copied: &AtomicI64, mut at_step: impl FnMut(usize)) {
    let until = Instant::now() + COPY;
    let mut i = 0;
    while Instant::now() < until {
        thread::sleep(STEP);
        copied.fetch_add(1 << 20, Ordering::SeqCst);
        at_step(i);
        i += 1;
    }
}

#[test]
fn a_save_queued_behind_a_long_copy_lands_before_the_quit_exits() {
    let _serial = serial();
    // One after another, the copy used up the total budget and the mute
    // queued behind it was never waited on: it needs only milliseconds, but
    // only once the copy is done.
    let copied = Arc::new(AtomicI64::new(0));
    let f = copying(&copied);
    let copy_done = Gate::default();
    let copy = {
        let (copied, done) = (Arc::clone(&copied), copy_done.clone());
        spawn_hold(&f, move || {
            copy_for(&copied, |_| {});
            done.open();
        })
    };
    let muted = Arc::new(AtomicBool::new(false));
    let mute = {
        let muted = Arc::clone(&muted);
        spawn_hold(&f, move || {
            copy_done.wait();
            thread::sleep(ms(10)); // write
            muted.store(true, Ordering::SeqCst);
        })
    };
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        muted.load(Ordering::SeqCst),
        "the quit exited under the queued save"
    );
    copy.join().unwrap();
    mute.join().unwrap();
}

#[test]
fn a_copy_keeps_the_quit_waiting_for_the_save_it_belongs_to_then_a_wedged_one_is_cut_off() {
    let _serial = serial();
    // The copy is evidence for the save it belongs to, which the quit waits
    // on alongside the wedged one; once it lands nothing moves, and the
    // wedged save is cut off one budget later.
    let copied = Arc::new(AtomicI64::new(0));
    let f = copying(&copied);
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait());
    let landed = Arc::new(AtomicBool::new(false));
    let copy = {
        let (copied, landed) = (Arc::clone(&copied), Arc::clone(&landed));
        spawn_hold(&f, move || {
            copy_for(&copied, |_| {});
            landed.store(true, Ordering::SeqCst);
        })
    };
    let sw = Instant::now();
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        landed.load(Ordering::SeqCst),
        "the copying save was not waited on"
    );
    // The copy, then one window more for the wedge: well under the join's
    // two seconds, which only a quit that never cut the wedge would reach.
    assert!(
        sw.elapsed() < ms(1500),
        "the wedge is cut: {:?}",
        sw.elapsed()
    );
    copy.join().unwrap();
}

#[test]
fn the_exit_request_after_a_close_button_quit_does_not_wait_it_all_over_again() {
    let _serial = serial();
    // The close button runs the quit, then the window's close raises an exit
    // request that runs it again for whatever is still pending.
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(100))
            .with_total_budget(ms(150)),
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait());
    let calls = probe(&f);
    finalize_all_within(&f, Duration::from_secs(2));
    thread::sleep(ms(60));
    finalize_all_within(&f, Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "budget already spent");
}

#[test]
fn a_held_save_still_copying_past_its_budget_keeps_the_quit_waiting() {
    let _serial = serial();
    // A Save pooling a large example (a 1 GB video copies for well over the
    // 5 s budget) is working, not wedged: cutting it off lost the Save.
    let copied = Arc::new(AtomicI64::new(0));
    let f = copying(&copied);
    let landed = Arc::new(AtomicBool::new(false));
    let save = {
        let (copied, landed) = (Arc::clone(&copied), Arc::clone(&landed));
        spawn_hold(&f, move || {
            copy_for(&copied, |_| {});
            landed.store(true, Ordering::SeqCst);
        })
    };
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        landed.load(Ordering::SeqCst),
        "the quit cut off a save still copying"
    );
    save.join().unwrap();
}

#[test]
fn a_save_still_copying_is_not_cut_off_when_another_copy_lands() {
    let _serial = serial();
    // The reading is the staging files in flight, so it DROPS when another
    // import finishes and leaves staging: that is progress, not a stall.
    let copied = Arc::new(AtomicI64::new(100 << 20)); // another import's 100 MB already in staging
    let f = copying(&copied);
    let landed = Arc::new(AtomicBool::new(false));
    let save = {
        let (copied, landed) = (Arc::clone(&copied), Arc::clone(&landed));
        spawn_hold(&f, move || {
            let other = Arc::clone(&copied);
            copy_for(&copied, |i| {
                if i == 3 {
                    other.fetch_sub(100 << 20, Ordering::SeqCst); // the other import landed
                }
            });
            landed.store(true, Ordering::SeqCst);
        })
    };
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(
        landed.load(Ordering::SeqCst),
        "the drop was read as a stalled save"
    );
    save.join().unwrap();
}

#[test]
fn settled_waits_out_a_landing_past_the_budget_and_one_listed_while_it_waits() {
    let _serial = serial();
    let f = shared(RecordingFinalizer::for_testing().with_per_finalizer_budget(ms(20)));
    let first = Gate::default();
    let second = Gate::default();
    let first_landing = {
        let first = first.clone();
        spawn_landing(&f, "p", move || first.wait())
    };
    let done = Arc::new(AtomicBool::new(false));
    let settled = {
        let (f, done) = (Arc::clone(&f), Arc::clone(&done));
        thread::spawn(move || {
            f.settled("p");
            done.store(true, Ordering::SeqCst);
        })
    };
    thread::sleep(ms(60));
    assert!(!done.load(Ordering::SeqCst), "no budget caps it");

    // landing() hands the finalize's own error back to its caller.
    let second_landing = {
        let second = second.clone();
        spawn_landing(&f, "p", move || -> Result<(), String> {
            second.wait();
            Err("a failed finalize still settles".into())
        })
    };
    first.open();
    thread::sleep(ms(20));
    assert!(
        !done.load(Ordering::SeqCst),
        "the take listed meanwhile still lands"
    );

    second.open();
    settled.join().unwrap();
    assert!(second_landing.join().unwrap().is_err());
    first_landing.join().unwrap();
    assert!(!f.is_landing("p"));
}

#[test]
fn a_held_save_that_stops_progressing_is_still_cut_off() {
    let _serial = serial();
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(40))
            .with_total_budget(ms(60))
            .with_progress_ceiling(Duration::from_secs(5))
            .with_progress(|| 7), // never moves
    );
    let never = Never::default();
    let wedge = never.gate();
    spawn_hold(&f, move || wedge.wait());
    let sw = Instant::now();
    finalize_all_within(&f, Duration::from_secs(2));
    assert!(sw.elapsed() < ms(500), "{:?}", sw.elapsed());
}

#[test]
fn a_held_save_keeps_the_quit_waiting_until_it_settles_and_only_that_long() {
    let _serial = serial();
    let f = shared(RecordingFinalizer::for_testing());
    let commit = Gate::default();
    let saved = {
        let commit = commit.clone();
        spawn_hold(&f, move || {
            commit.wait();
            7
        })
    };
    assert!(f.has_active(), "held from the first call");

    let quit = Quit::start(&f);
    thread::sleep(ms(50));
    assert!(!quit.is_done(), "the quit waits for the airborne save");

    commit.open();
    quit.join_within(Duration::from_secs(2));
    assert_eq!(
        saved.join().unwrap(),
        7,
        "the caller still gets the save's result"
    );
    assert!(!f.has_active());
}

#[test]
fn a_failed_held_save_reaches_its_caller_never_the_quit() {
    let _serial = serial();
    let f = shared(RecordingFinalizer::for_testing());
    let commit = Gate::default();
    let saved = {
        let commit = commit.clone();
        spawn_hold(&f, move || -> Result<(), String> {
            commit.wait();
            Err("disk full".into())
        })
    };
    let quit = Quit::start(&f);
    commit.open();

    assert!(saved.join().unwrap().is_err());
    quit.join_within(Duration::from_secs(2)); // completes normally
    assert!(!f.has_active());
}

#[test]
fn a_panicking_held_save_still_releases_the_quit() {
    let _serial = serial();
    // Rust's form of a held save that throws: the panic reaches its caller,
    // and the hold is gone with it.
    let f = shared(RecordingFinalizer::for_testing());
    let held = {
        let f = Arc::clone(&f);
        thread::spawn(move || f.hold(|| panic!("disk full")))
    };
    assert!(held.join().is_err());
    assert!(!f.has_active());
    finalize_all_within(&f, Duration::from_secs(2));
}

#[test]
fn the_normal_path_still_runs_every_finalizer_to_completion() {
    let _serial = serial();
    let f = shared(RecordingFinalizer::for_testing());
    let ran = Arc::new(AtomicUsize::new(0));
    for fails in [false, true, false] {
        let ran = Arc::clone(&ran);
        f.register(
            &Finalizer::new(move || {
                ran.fetch_add(1, Ordering::SeqCst);
                if fails {
                    panic!("one failure never blocks the rest");
                }
            }),
            false,
        );
    }

    f.finalize_all();

    assert_eq!(ran.load(Ordering::SeqCst), 3);
}

#[test]
fn a_reopened_practice_waits_for_its_landing_take_but_not_for_a_wedged_one() {
    let _serial = serial();
    // A budget of 300 ms (Dart: 30) so the landed take is seen to return
    // well inside it, not at its end, on a loaded machine too.
    let f = shared(RecordingFinalizer::for_testing().with_per_finalizer_budget(ms(300)));
    let take = Gate::default();
    let never = Never::default();
    let taking = {
        let take = take.clone();
        spawn_landing(&f, "p1", move || take.wait())
    };
    let (tx, landed) = mpsc::channel();
    {
        let f = Arc::clone(&f);
        thread::spawn(move || {
            f.landed("p1");
            let _ = tx.send(Instant::now());
        });
    }
    thread::sleep(ms(5));
    assert!(landed.try_recv().is_err(), "its take is still landing");
    let completed = Instant::now();
    take.open();
    let at = landed
        .recv_timeout(Duration::from_secs(2))
        .expect("landed once its take did");
    assert!(
        at.duration_since(completed) < ms(150),
        "landed waited out its budget instead of the take"
    );
    taking.join().unwrap();

    let wedge = never.gate();
    spawn_landing(&f, "p1", move || wedge.wait());
    // The outer guard is the test's stand-in for a screen that never opens.
    let (tx, waited) = mpsc::channel();
    {
        let f = Arc::clone(&f);
        thread::spawn(move || {
            f.landed("p1");
            f.landed("other");
            let _ = tx.send(());
        });
    }
    waited
        .recv_timeout(Duration::from_secs(2))
        .expect("a wedged landing held the reopened screen");
}

#[test]
fn landed_waits_only_for_the_takes_landing_when_it_was_asked() {
    let _serial = serial();
    // Unlike settled: a reopened screen shows what landed by the time it
    // opened, and a take stopped after that is not its to wait for.
    let f = shared(RecordingFinalizer::for_testing());
    let first = Gate::default();
    let later = Never::default();
    let first_landing = {
        let first = first.clone();
        spawn_landing(&f, "p", move || first.wait())
    };
    let (asking, asked) = mpsc::channel();
    let (tx, landed) = mpsc::channel();
    {
        let f = Arc::clone(&f);
        thread::spawn(move || {
            asking.send(()).unwrap();
            f.landed("p");
            let _ = tx.send(());
        });
    }
    asked.recv().unwrap();
    thread::sleep(ms(20)); // into its wait
    let wedge = later.gate();
    spawn_landing(&f, "p", move || wedge.wait());
    first.open();
    landed
        .recv_timeout(Duration::from_secs(2))
        .expect("landed waited on a take listed after it was asked");
    first_landing.join().unwrap();
    assert!(f.is_landing("p"), "the later take is still landing");
}

#[test]
fn settled_within_gives_up_at_its_limit_and_answers_once_the_take_lands() {
    let _serial = serial();
    // Dart's settled(id).timeout(grace), which the song screen waits on.
    let f = shared(RecordingFinalizer::for_testing());
    assert!(f.settled_within("p", ms(10)), "nothing landing");
    let take = Gate::default();
    let taking = {
        let take = take.clone();
        spawn_landing(&f, "p", move || take.wait())
    };
    assert!(f.is_landing("p"));
    assert!(!f.settled_within("p", ms(30)));
    take.open();
    assert!(f.settled_within("p", Duration::from_secs(2)));
    taking.join().unwrap();
}

#[test]
fn the_guard_forms_hold_and_list_until_dropped() {
    let _serial = serial();
    // A save handed off to another thread is held from the hand-off on.
    let f = shared(RecordingFinalizer::for_testing());
    let hold = f.begin_hold();
    let landing = f.begin_landing("p");
    assert!(f.has_active());
    assert!(f.is_landing("p"));
    let quit = Quit::start(&f);
    thread::sleep(ms(30));
    assert!(!quit.is_done(), "the quit waits for the handed-off save");
    let worker = thread::spawn(move || {
        drop(landing);
        drop(hold);
    });
    quit.join_within(Duration::from_secs(2));
    worker.join().unwrap();
    assert!(!f.has_active());
    assert!(!f.is_landing("p"));
}

#[test]
fn concurrent_quits_share_one_run() {
    let _serial = serial();
    // A close click racing a Cmd+Q exit request: the second waits on the
    // first's run instead of calling the finalizers again.
    let f = shared(RecordingFinalizer::for_testing());
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Gate::default();
    let entered = Gate::default();
    {
        let (calls, release, entered) = (Arc::clone(&calls), release.clone(), entered.clone());
        f.register(
            &Finalizer::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                entered.open();
                release.wait();
            }),
            false,
        );
    }
    let close = Quit::start(&f);
    entered.wait();
    let exit = Quit::start(&f);
    thread::sleep(ms(30));
    assert!(!exit.is_done(), "the exit request waits on the close's run");
    release.open();
    close.join_within(Duration::from_secs(2));
    exit.join_within(Duration::from_secs(2));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// The quit's progress reading looks at an export's destination, which can sit
// on a network share that stopped answering: the quit waited on it, and so
// did every save behind it.
fn a_progress_reading_that_misbehaves_neither_holds_nor_stops_the_quit(
    reading: impl Fn() -> i64 + Send + Sync + 'static,
) {
    let f = shared(
        RecordingFinalizer::for_testing()
            .with_per_finalizer_budget(ms(50))
            .with_total_budget(ms(80))
            .with_progress(reading),
    );
    let saved = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saved);
    f.register(
        &Finalizer::new(move || flag.store(true, Ordering::SeqCst)),
        true,
    );
    let clock = Instant::now();
    f.finalize_all();
    assert!(saved.load(Ordering::SeqCst));
    assert!(
        clock.elapsed() < RecordingFinalizer::READING_LIMIT * 3,
        "one bounded reading, then the finalizers"
    );
}

#[test]
fn a_progress_reading_that_never_answers_neither_holds_nor_stops_the_quit() {
    let _serial = serial();
    let never = Never::default();
    let stuck = never.gate();
    a_progress_reading_that_misbehaves_neither_holds_nor_stops_the_quit(move || {
        stuck.wait();
        0
    });
}

#[test]
fn a_progress_reading_that_throws_neither_holds_nor_stops_the_quit() {
    let _serial = serial();
    a_progress_reading_that_misbehaves_neither_holds_nor_stops_the_quit(|| panic!("gone"));
}
