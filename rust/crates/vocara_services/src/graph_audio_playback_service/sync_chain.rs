//! The graph syncs' serializer (Dart's `SyncChain`).

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

type Job = Box<dyn FnOnce() + Send>;

/// Serializes graph syncs: two rebuilds must never interleave over one
/// engine, because a rebuild releases the service between its file decodes,
/// and a second one would stage into the index maps the first is still
/// swapping.
///
/// It also has to accept a job ENQUEUED FROM INSIDE a running job: that is
/// how a degraded commit queues its one rebuild. That works because
/// [`SyncChain::add`] only appends to the queue and never waits: an inner add
/// lands after every job already queued, instead of waiting on a job that is
/// waiting on its caller.
///
/// Jobs run one at a time, in the order they were added, on a worker thread
/// the chain starts when a job arrives at an idle queue and that ends once the
/// queue is empty again (so an idle chain holds no thread). A job that panics
/// reports the panic to its own ticket and the chain goes on with the next:
/// one failed sync never blocks the ones behind it, as Dart's chain absorbed a
/// failed future.
///
/// Cloning gives another handle to the same queue (what a job holds to queue
/// its successor).
#[derive(Clone, Default)]
pub struct SyncChain {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    queue: Mutex<Queue>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// A drain loop is running (on the worker, or inline after a failed spawn).
    draining: bool,
}

impl SyncChain {
    pub fn new() -> SyncChain {
        SyncChain::default()
    }

    /// Runs `job` once everything already queued has settled. The ticket
    /// carries `job`'s own outcome (its value, or its panic); the chain
    /// absorbs a panic so the jobs behind it still run. Never blocks on the
    /// queue: called from inside a running job, the new one lands after it.
    pub fn add<R, F>(&self, job: F) -> SyncTicket<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let outcome = catch_unwind(AssertUnwindSafe(job));
            // A dropped ticket (a caller that does not wait) ignores it.
            let _ = tx.send(outcome);
        });
        let start = {
            let mut q = self.inner.lock();
            q.jobs.push_back(job);
            !std::mem::replace(&mut q.draining, true)
        };
        if start {
            let inner = Arc::clone(&self.inner);
            let spawned = thread::Builder::new()
                .name("vocara-sync-chain".into())
                .spawn(move || inner.drain());
            if spawned.is_err() {
                // No thread to be had: run the queue here rather than leave a
                // ticket waiting forever. The order is the same.
                self.inner.drain();
            }
        }
        SyncTicket { rx }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the queued jobs in order until the queue is empty.
    fn drain(&self) {
        loop {
            let job = {
                let mut q = self.lock();
                match q.jobs.pop_front() {
                    Some(job) => job,
                    None => {
                        q.draining = false;
                        return;
                    }
                }
            };
            // Each job catches its own panic (see `add`), so the loop goes on.
            job();
        }
    }
}

/// A queued job's outcome, to wait for (Dart's `Future` from `add`). Dropping
/// it leaves the job queued; only its result goes unread.
pub struct SyncTicket<R> {
    rx: mpsc::Receiver<thread::Result<R>>,
}

impl<R> SyncTicket<R> {
    /// Blocks until the job ran: its value, or the payload of its panic.
    pub fn wait(self) -> thread::Result<R> {
        self.rx
            .recv()
            .unwrap_or_else(|_| Err(Box::new("the sync chain dropped the job")))
    }
}

/// A panic payload's message (`panic!` with a literal or a formatted string),
/// or a placeholder for any other payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic with a non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    // Port of `test/sync_chain_test.dart`.
    //
    // Graph rebuilds decode files with the service released, so two of them
    // running at once would stage into the index maps the other is swapping.
    // SyncChain is the single-file queue that prevents it, and, crucially,
    // the queue a running sync can push its OWN follow-up onto: a commit that
    // degraded a streaming track to silence queues exactly one rebuild from
    // inside the sync, and that must land after the commit instead of
    // deadlocking on it.

    use std::time::Duration;

    use super::*;

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn push(log: &Mutex<Vec<String>>, entry: &str) {
        log.lock().unwrap().push(entry.to_string());
    }

    #[test]
    fn jobs_run_one_at_a_time_in_the_order_they_were_queued() {
        let chain = SyncChain::new();
        let log = log();
        let job = |name: &'static str| {
            let log = Arc::clone(&log);
            move || {
                push(&log, &format!("{name}:start"));
                thread::sleep(Duration::from_millis(5));
                push(&log, &format!("{name}:end"));
            }
        };
        let a = chain.add(job("a"));
        let b = chain.add(job("b"));
        a.wait().unwrap();
        b.wait().unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["a:start", "a:end", "b:start", "b:end"]
        );
    }

    #[test]
    fn a_job_may_queue_another_from_inside_itself() {
        // The degraded-commit retry: the running sync withholds the committed
        // signature and queues the rebuild that will re-probe the file.
        // Nothing else ever calls sync for it (play, seek and fader moves all
        // bypass the graph rebuild), so if this deadlocked or was dropped, the
        // track would stay silent for the rest of the session.
        let chain = SyncChain::new();
        let log = log();
        let commit = {
            let (chain, log) = (chain.clone(), Arc::clone(&log));
            move || {
                push(&log, "commit:start");
                // Fire-and-forget from INSIDE the running job.
                let rebuild_log = Arc::clone(&log);
                drop(chain.add(move || push(&rebuild_log, "rebuild")));
                thread::sleep(Duration::from_millis(5));
                push(&log, "commit:end");
            }
        };
        chain.add(commit).wait().unwrap();
        // Let the queued rebuild run.
        chain.add(|| ()).wait().unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            ["commit:start", "commit:end", "rebuild"]
        );
    }

    #[test]
    fn a_failing_job_reports_to_its_caller_without_poisoning_the_queue() {
        let chain = SyncChain::new();
        let ran = log();
        let failed = {
            let ran = Arc::clone(&ran);
            chain.add(move || {
                push(&ran, "boom");
                panic!("commit rejected");
            })
        };
        let after = {
            let ran = Arc::clone(&ran);
            chain.add(move || push(&ran, "after"))
        };
        let payload = failed.wait().unwrap_err();
        assert_eq!(panic_message(&*payload), "commit rejected");
        after.wait().unwrap();
        assert_eq!(*ran.lock().unwrap(), ["boom", "after"]);
    }

    #[test]
    fn an_idle_chain_starts_again_for_the_next_job() {
        let chain = SyncChain::new();
        assert_eq!(chain.add(|| 1).wait().unwrap(), 1);
        // The worker ended with the empty queue; a later job starts another.
        thread::sleep(Duration::from_millis(5));
        assert_eq!(chain.add(|| 2).wait().unwrap(), 2);
    }

    #[test]
    fn a_dropped_ticket_still_runs_its_job() {
        let chain = SyncChain::new();
        let ran = log();
        {
            let ran = Arc::clone(&ran);
            drop(chain.add(move || push(&ran, "fire-and-forget")));
        }
        chain.add(|| ()).wait().unwrap();
        assert_eq!(*ran.lock().unwrap(), ["fire-and-forget"]);
    }

    #[test]
    fn panic_messages_read_both_payload_kinds() {
        let literal = catch_unwind(|| panic!("plain")).unwrap_err();
        assert_eq!(panic_message(&*literal), "plain");
        let formatted = catch_unwind(|| panic!("n = {}", 3)).unwrap_err();
        assert_eq!(panic_message(&*formatted), "n = 3");
        let other = catch_unwind(|| std::panic::panic_any(7_u8)).unwrap_err();
        assert_eq!(panic_message(&*other), "a panic with a non-string payload");
    }
}
