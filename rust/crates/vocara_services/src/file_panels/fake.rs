//! The test panels: while a test holds a [`FakeFilePanels`], every panel
//! the process asks for (on any thread) is recorded and answered by it
//! instead of the platform. Tests that hold one run one at a time
//! ([`FakeFilePanels::install`] waits for the previous one to be dropped),
//! so tests in parallel cannot answer each other's panels.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{Kind, Panel, Request};

#[derive(Debug, Default)]
struct State {
    requests: Vec<Request>,
    answers: VecDeque<Option<Vec<PathBuf>>>,
}

/// The fake in place, if a test holds one.
static INSTALLED: Mutex<Option<Arc<Mutex<State>>>> = Mutex::new(None);

/// Held by the test whose fake is in place.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The installed fake's answer for a panel of `kind` set up as `panel`
/// (recorded), or `None` when no fake is installed and the platform's
/// panel is to run.
pub(super) fn answer(kind: Kind, panel: &Panel) -> Option<Option<Vec<PathBuf>>> {
    let state = lock(&INSTALLED).clone()?;
    let mut state = lock(&state);
    state.requests.push(Request {
        kind,
        panel: panel.clone(),
    });
    Some(state.answers.pop_front().flatten())
}

/// Answers every panel while it is held (see the module docs). A panel with
/// no answer queued is cancelled.
#[derive(Debug)]
pub struct FakeFilePanels {
    state: Arc<Mutex<State>>,
    _serial: MutexGuard<'static, ()>,
}

impl FakeFilePanels {
    /// Puts a fake in place of the platform's panels until it is dropped,
    /// once the one before it (another test's) has been dropped.
    pub fn install() -> Self {
        let serial = lock(&SERIAL);
        let state = Arc::new(Mutex::new(State::default()));
        *lock(&INSTALLED) = Some(Arc::clone(&state));
        FakeFilePanels {
            state,
            _serial: serial,
        }
    }

    /// The next panel picks (or saves to) `paths`: a save panel or
    /// `pick_file` takes the first.
    pub fn pick(&self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) {
        let paths = paths.into_iter().map(Into::into).collect();
        lock(&self.state).answers.push_back(Some(paths));
    }

    /// The next panel is cancelled (as is any with nothing queued).
    pub fn cancel(&self) {
        lock(&self.state).answers.push_back(None);
    }

    /// The panels asked for so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }
}

impl Drop for FakeFilePanels {
    fn drop(&mut self) {
        *lock(&INSTALLED) = None;
    }
}
