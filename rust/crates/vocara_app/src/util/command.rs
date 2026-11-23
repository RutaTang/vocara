//! Port of `utils/command.dart`: a command runs an async action and exposes
//! its running/result state to the UI (Flutter's app-architecture guide
//! pattern), which renders spinners and surfaces errors from it.
//!
//! The Dart `Command` was a `ChangeNotifier` that awaited its action. Here the
//! state is plain data the owning view model holds; [`Command0::execute`] /
//! [`Command1::execute`] start a run and hand back a [`Run`] (the action, not
//! yet run), which the screen runs off the UI thread ([`Run::into_task`]) and
//! whose outcome comes back through [`Command::finish`]. "Notify listeners"
//! is "the state changed; iced redraws".

use std::sync::Arc;

use iced::Task;

use super::{run_caught, ActionError};

/// The outcome of one run: the action's value or why it failed.
pub type Outcome<T, E> = Result<T, ActionError<E>>;

/// A command's running/result state (Dart's abstract `Command<T>`).
#[derive(Debug)]
pub struct Command<T, E = String> {
    running: bool,
    result: Option<Outcome<T, E>>,
}

impl<T, E> Default for Command<T, E> {
    fn default() -> Self {
        Self {
            running: false,
            result: None,
        }
    }
}

impl<T, E> Command<T, E> {
    /// Whether a run is in flight.
    pub fn running(&self) -> bool {
        self.running
    }

    /// The last run's outcome; `None` while running and before the first run.
    pub fn result(&self) -> Option<&Outcome<T, E>> {
        self.result.as_ref()
    }

    /// Whether the last run finished with an error.
    pub fn error(&self) -> bool {
        matches!(self.result, Some(Err(_)))
    }

    /// Whether the last run finished successfully.
    pub fn completed(&self) -> bool {
        matches!(self.result, Some(Ok(_)))
    }

    /// Clears the last result (e.g. after the UI has shown the error once).
    pub fn clear_result(&mut self) {
        self.result = None;
    }

    /// Starts a run: `false`, and nothing changes, while a run is in flight
    /// (the guard against re-entrancy); otherwise marks the command running
    /// and clears the last result.
    fn begin(&mut self) -> bool {
        if self.running {
            return false;
        }
        self.running = true;
        self.result = None;
        true
    }

    /// Lands a run's outcome and clears [`running`](Self::running) — the
    /// Dart `finally`. An outcome for a command that is not running (a late
    /// result after the owner reset itself) is still recorded, as Dart's
    /// `finally` ran whatever happened.
    pub fn finish(&mut self, outcome: Outcome<T, E>) {
        self.result = Some(outcome);
        self.running = false;
    }
}

/// One accepted run's action, ready to run on a worker thread.
pub struct Run<T, E> {
    action: Box<dyn FnOnce() -> Result<T, E> + Send>,
}

impl<T: Send + 'static, E: Send + 'static> Run<T, E> {
    /// Runs the action on the current thread. The contract is that actions
    /// return a `Result` and never panic; be defensive anyway: a panicking
    /// action would otherwise leave the command running forever (neither
    /// error nor completed, so the UI shows nothing) and surface far from its
    /// cause. It becomes an observable error outcome instead.
    pub fn run(self) -> Outcome<T, E> {
        run_caught(self.action)
    }

    /// Runs the action on a blocking thread and maps its outcome to a
    /// message, which the screen hands to [`Command::finish`].
    pub fn into_task<M: Send + 'static>(
        self,
        on_done: impl FnOnce(Outcome<T, E>) -> M + Send + 'static,
    ) -> Task<M> {
        Task::perform(super::blocking(move || self.run()), move |joined| {
            on_done(match joined {
                Ok(outcome) => outcome,
                Err(message) => Err(ActionError::Panicked(message)),
            })
        })
    }
}

/// A [`Command`] that accepts no arguments.
pub struct Command0<T, E = String> {
    state: Command<T, E>,
    action: Arc<dyn Fn() -> Result<T, E> + Send + Sync>,
}

impl<T: Send + 'static, E: Send + 'static> Command0<T, E> {
    pub fn new(action: impl Fn() -> Result<T, E> + Send + Sync + 'static) -> Self {
        Self {
            state: Command::default(),
            action: Arc::new(action),
        }
    }

    /// Starts a run: `None` while one is in flight (re-entrancy guard).
    pub fn execute(&mut self) -> Option<Run<T, E>> {
        if !self.state.begin() {
            return None;
        }
        let action = Arc::clone(&self.action);
        Some(Run {
            action: Box::new(move || action()),
        })
    }
}

impl<T, E> std::ops::Deref for Command0<T, E> {
    type Target = Command<T, E>;
    fn deref(&self) -> &Command<T, E> {
        &self.state
    }
}

impl<T, E> std::ops::DerefMut for Command0<T, E> {
    fn deref_mut(&mut self) -> &mut Command<T, E> {
        &mut self.state
    }
}

/// A [`Command`] that accepts a single argument.
pub struct Command1<T, A, E = String> {
    state: Command<T, E>,
    action: Arc<dyn Fn(A) -> Result<T, E> + Send + Sync>,
}

impl<T: Send + 'static, A: Send + 'static, E: Send + 'static> Command1<T, A, E> {
    pub fn new(action: impl Fn(A) -> Result<T, E> + Send + Sync + 'static) -> Self {
        Self {
            state: Command::default(),
            action: Arc::new(action),
        }
    }

    /// Starts a run with `argument`: `None` while one is in flight.
    pub fn execute(&mut self, argument: A) -> Option<Run<T, E>> {
        if !self.state.begin() {
            return None;
        }
        let action = Arc::clone(&self.action);
        Some(Run {
            action: Box::new(move || action(argument)),
        })
    }
}

impl<T, A, E> std::ops::Deref for Command1<T, A, E> {
    type Target = Command<T, E>;
    fn deref(&self) -> &Command<T, E> {
        &self.state
    }
}

impl<T, A, E> std::ops::DerefMut for Command1<T, A, E> {
    fn deref_mut(&mut self) -> &mut Command<T, E> {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_marks_running_then_lands_its_result() {
        let mut cmd: Command0<i32> = Command0::new(|| Ok(7));
        assert!(!cmd.running() && cmd.result().is_none());
        let run = cmd.execute().expect("accepted");
        assert!(cmd.running());
        assert!(!cmd.completed() && !cmd.error());
        let outcome = run.run();
        cmd.finish(outcome);
        assert!(!cmd.running());
        assert!(cmd.completed());
        assert_eq!(cmd.result(), Some(&Ok(7)));
    }

    #[test]
    fn a_second_execute_while_running_is_ignored() {
        let mut cmd: Command1<i32, i32> = Command1::new(|a| Ok(a * 2));
        let first = cmd.execute(2).expect("accepted");
        assert!(cmd.execute(5).is_none(), "re-entrancy guard");
        cmd.finish(first.run());
        assert_eq!(cmd.result(), Some(&Ok(4)));
        assert!(cmd.execute(5).is_some(), "open again once finished");
    }

    #[test]
    fn a_new_run_clears_the_last_result() {
        let mut cmd: Command0<(), String> = Command0::new(|| Err("nope".to_string()));
        let run = cmd.execute().unwrap();
        cmd.finish(run.run());
        assert!(cmd.error());
        let _run = cmd.execute().unwrap();
        assert!(cmd.result().is_none());
    }

    #[test]
    fn a_panicking_action_becomes_an_observable_error() {
        let mut cmd: Command0<(), String> = Command0::new(|| panic!("boom"));
        let run = cmd.execute().unwrap();
        cmd.finish(run.run());
        assert!(cmd.error());
        assert_eq!(
            cmd.result(),
            Some(&Err(ActionError::Panicked("boom".to_string())))
        );
        assert!(!cmd.running());
    }

    #[test]
    fn clear_result_forgets_the_outcome() {
        let mut cmd: Command0<i32> = Command0::new(|| Ok(1));
        let run = cmd.execute().unwrap();
        cmd.finish(run.run());
        cmd.clear_result();
        assert!(!cmd.completed() && !cmd.error() && cmd.result().is_none());
    }

    #[tokio::test]
    async fn into_task_runs_on_a_blocking_thread() {
        let mut cmd: Command0<i32> = Command0::new(|| Ok(3));
        let run = cmd.execute().unwrap();
        let outcome = crate::util::blocking(move || run.run()).await.unwrap();
        cmd.finish(outcome);
        assert_eq!(cmd.result(), Some(&Ok(3)));
    }
}
