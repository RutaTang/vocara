//! Port of `utils/busy_gate.dart`: a screen's single busy/error latch for
//! long-running actions.
//!
//! [`BusyGate::busy`] is deliberately ONE flag per view model, not one per
//! action:
//!  * it renders as the one transport spinner and is polled by the
//!    integration tests as their settle condition;
//!  * it is the cross-action structural-edit MUTEX — actions guard on `busy`
//!    before touching the model, so an import can never interleave with a
//!    recording finalize (lost-update races).
//!
//! Owning view models must therefore create exactly one instance and lend it
//! to every controller that runs guarded work; a second instance would
//! silently split the mutex.
//!
//! Dart's `run` awaited the action. Here [`BusyGate::run`] takes the latch and
//! hands back the action as a [`BusyRun`] (or `None` when the gate is taken:
//! Dart's `false`); the screen runs it off the UI thread and lands its outcome
//! with [`BusyGate::finish`] — the Dart `catch`/`finally`. Dart's `onError`
//! callback becomes `finish`'s return value: the error comes back to the
//! owner, which surfaces it (a toast) its own way at that point. Dart's
//! `dispose` guard (no notify after the owner is gone) is the router's job
//! here: a late outcome for a page that is gone never reaches its gate.

use std::sync::Arc;

use iced::Task;

use super::{run_caught, ActionError};

/// An action the gate runs: it fails with `E`.
pub type BusyAction<E> = Box<dyn FnOnce() -> Result<(), E> + Send>;

/// Wraps every [`BusyGate::run`]'s action unless the run opts out
/// ([`BusyGate::run_unwrapped`]): how the owner keeps track of ALL its
/// guarded work at once, whichever controller started it. The song practice
/// screen lists each one as landing on its practice, so a screen reopened on
/// it meanwhile waits for it before taking edits.
pub type Around<E> = Arc<dyn Fn(BusyAction<E>) -> Result<(), E> + Send + Sync>;

/// The outcome of one guarded action.
pub type BusyOutcome<E> = Result<(), ActionError<E>>;

/// The busy/error latch (see the module docs).
pub struct BusyGate<E = String> {
    around: Option<Around<E>>,
    busy: bool,
    error_message: Option<String>,
}

impl<E> Default for BusyGate<E> {
    fn default() -> Self {
        Self {
            around: None,
            busy: false,
            error_message: None,
        }
    }
}

impl<E: std::fmt::Display + Send + 'static> BusyGate<E> {
    /// A gate with no `around` wrapper.
    pub fn new() -> Self {
        Self::default()
    }

    /// A gate whose runs are wrapped by `around` (see [`Around`]).
    pub fn with_around(around: Around<E>) -> Self {
        Self {
            around: Some(around),
            ..Self::default()
        }
    }

    /// Whether an action holds the gate.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// The raw text of the last run's error (cleared when a run starts).
    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    /// Takes the gate for `action`: sets [`busy`](Self::busy) and clears
    /// [`error_message`](Self::error_message), and hands back the action
    /// (wrapped by `around`, if any) to run.
    ///
    /// ATOMIC: when an action already holds the gate, the call does nothing
    /// and returns `None` — the mutex must hold even for a caller whose own
    /// pre-guard raced the latch (two overlapping runs would let the first
    /// finisher clear `busy` while the second still mutates, re-opening the
    /// gate to a third). `Some` means the action will run, even if it then
    /// fails: the error is surfaced through `error_message` and
    /// [`finish`](Self::finish)'s return, not this one. No caller may start a
    /// run from inside another run's action: it would be rejected, silently
    /// skipping its work.
    pub fn run(
        &mut self,
        action: impl FnOnce() -> Result<(), E> + Send + 'static,
    ) -> Option<BusyRun<E>> {
        self.take(Box::new(action), true)
    }

    /// [`run`](Self::run) without the `around` wrapper (Dart's
    /// `wrapped: false`).
    pub fn run_unwrapped(
        &mut self,
        action: impl FnOnce() -> Result<(), E> + Send + 'static,
    ) -> Option<BusyRun<E>> {
        self.take(Box::new(action), false)
    }

    fn take(&mut self, action: BusyAction<E>, wrapped: bool) -> Option<BusyRun<E>> {
        if self.busy {
            return None;
        }
        self.busy = true;
        self.error_message = None;
        let job: BusyAction<E> = match (&self.around, wrapped) {
            (Some(around), true) => {
                let around = Arc::clone(around);
                Box::new(move || around(action))
            }
            _ => action,
        };
        Some(BusyRun { job })
    }

    /// Lands a run's outcome: a failure is kept in `error_message` and
    /// returned (Dart's `onError`) so the owner can surface a friendly
    /// message; `busy` clears either way (the Dart `finally`).
    pub fn finish(&mut self, outcome: BusyOutcome<E>) -> Option<ActionError<E>> {
        self.busy = false;
        match outcome {
            Ok(()) => None,
            Err(error) => {
                self.error_message = Some(error.to_string());
                Some(error)
            }
        }
    }
}

/// An accepted run's action, ready to run on a worker thread.
pub struct BusyRun<E> {
    job: BusyAction<E>,
}

impl<E: Send + 'static> BusyRun<E> {
    /// Runs the action on the current thread; a panic becomes
    /// [`ActionError::Panicked`] (Dart's caught throw).
    pub fn run(self) -> BusyOutcome<E> {
        run_caught(self.job)
    }

    /// Runs the action on a blocking thread and maps its outcome to a
    /// message, which the screen hands to [`BusyGate::finish`].
    pub fn into_task<M: Send + 'static>(
        self,
        on_done: impl FnOnce(BusyOutcome<E>) -> M + Send + 'static,
    ) -> Task<M> {
        Task::perform(super::blocking(move || self.run()), move |joined| {
            on_done(match joined {
                Ok(outcome) => outcome,
                Err(message) => Err(ActionError::Panicked(message)),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn runs_the_action_under_the_busy_latch_and_reports_acceptance() {
        let mut gate: BusyGate = BusyGate::new();
        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = Arc::clone(&ran);
        let run = gate.run(move || {
            ran2.store(true, Ordering::SeqCst);
            Ok(())
        });
        let run = run.expect("accepted");
        assert!(gate.busy());
        let error = gate.finish(run.run());
        assert!(ran.load(Ordering::SeqCst));
        assert!(!gate.busy());
        assert!(error.is_none());
    }

    #[test]
    fn is_atomic_a_second_concurrent_run_is_rejected_and_never_executes() {
        let mut gate: BusyGate = BusyGate::new();
        let first = gate.run(|| Ok(())).expect("accepted");
        // The latch is set before the action runs at all.
        assert!(gate.busy());

        let second_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&second_ran);
        let second = gate.run(move || {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(second.is_none(), "the overlapping run must be rejected");
        assert!(!second_ran.load(Ordering::SeqCst));
        assert!(
            gate.busy(),
            "a rejected run must not clear the first run's latch"
        );

        assert!(gate.finish(first.run()).is_none());
        assert!(!gate.busy());
    }

    #[test]
    fn accepts_a_new_run_once_the_previous_one_finished() {
        let mut gate: BusyGate = BusyGate::new();
        let run = gate.run(|| Ok(())).expect("first");
        gate.finish(run.run());
        let run = gate.run(|| Ok(())).expect("second");
        gate.finish(run.run());
    }

    #[test]
    fn a_failing_action_still_counts_as_ran_and_the_gate_reopens() {
        let mut gate: BusyGate = BusyGate::new();
        let mut errors = Vec::new();
        let run = gate
            .run(|| Err("StateError: boom".to_string()))
            .expect("accepted");
        if let Some(e) = gate.finish(run.run()) {
            errors.push(e);
        }
        assert!(!gate.busy());
        assert!(gate.error_message().unwrap().contains("boom"));
        assert_eq!(errors.len(), 1);
        // The error does not wedge the gate shut.
        assert!(gate.run(|| Ok(())).is_some());
    }

    #[test]
    fn a_panicking_action_is_surfaced_like_a_throw() {
        let mut gate: BusyGate = BusyGate::new();
        let run = gate.run(|| panic!("boom")).expect("accepted");
        let error = gate.finish(run.run());
        assert!(!gate.busy());
        assert!(matches!(error, Some(ActionError::Panicked(m)) if m == "boom"));
        assert_eq!(gate.error_message(), Some("boom"));
    }

    #[test]
    fn a_new_run_clears_the_last_error() {
        let mut gate: BusyGate = BusyGate::new();
        let run = gate.run(|| Err("bad".to_string())).unwrap();
        gate.finish(run.run());
        let _run = gate.run(|| Ok(())).unwrap();
        assert_eq!(gate.error_message(), None);
    }

    #[test]
    fn around_wraps_runs_unless_they_opt_out() {
        let wrapped = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wrapped);
        let mut gate: BusyGate = BusyGate::with_around(Arc::new(move |action| {
            counter.fetch_add(1, Ordering::SeqCst);
            action()
        }));
        let run = gate.run(|| Ok(())).unwrap();
        gate.finish(run.run());
        assert_eq!(wrapped.load(Ordering::SeqCst), 1);
        let run = gate.run_unwrapped(|| Ok(())).unwrap();
        gate.finish(run.run());
        assert_eq!(wrapped.load(Ordering::SeqCst), 1, "opted out");
    }
}
