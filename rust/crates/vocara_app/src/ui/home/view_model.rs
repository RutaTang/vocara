//! Port of `ui/home/view_model/home_view_model.dart`: the home screen's view
//! model, the collection of song practices.
//!
//! Dart's three `Command`s are [`Command0`] / [`Command1`] here. Starting one
//! hands back an [`Effect`] (the repository call, not yet run) which the
//! screen runs off the UI thread ([`Effect::run`]); its outcome comes back as
//! a [`Landing`] the view model lands ([`HomeViewModel::land`]) — the part of
//! each Dart action after its `await`, then the command's `finally`. Dart's
//! listeners on the commands (`_replayMissed`, run as each command notifies)
//! are the [`Effect`] a landing may hand back: the load a finished command
//! owes.
//!
//! Dart's async actions ran their synchronous start at `execute` (a load
//! snapshots the list epoch there); the rest ran when the repository
//! answered. The same split holds here: the epoch is taken when a load
//! starts, the landing compares it when the load lands.

use std::sync::Arc;

use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::song_practice::SongPractice;
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::sync_engine::RemoteApplied;

use crate::util::command::{Command, Command0, Command1, Outcome, Run};

/// Work the view model asks the screen to run off the UI thread: a started
/// command's repository call.
pub enum Effect {
    /// The list read (`load`).
    Load(Run<Vec<PracticeSummary>, String>),
    /// The create (`createProject`).
    Create(Run<SongPractice, String>),
    /// The delete of practice `id` (`deleteProject`).
    Delete { id: String, run: Run<(), String> },
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::Load(_) => f.write_str("Effect::Load"),
            Effect::Create(_) => f.write_str("Effect::Create"),
            Effect::Delete { id, .. } => write!(f, "Effect::Delete({id})"),
        }
    }
}

impl Effect {
    /// Runs the repository call on the current thread (blocking: the screen
    /// runs it on a worker thread) and returns its outcome to land.
    pub fn run(self) -> Landing {
        match self {
            Effect::Load(run) => Landing::Loaded(run.run()),
            Effect::Create(run) => Landing::Created(run.run()),
            Effect::Delete { id, run } => Landing::Deleted {
                id,
                outcome: run.run(),
            },
        }
    }
}

/// A command's outcome, back from the worker thread.
#[derive(Debug, Clone)]
pub enum Landing {
    Loaded(Outcome<Vec<PracticeSummary>, String>),
    Created(Outcome<SongPractice, String>),
    Deleted {
        id: String,
        outcome: Outcome<(), String>,
    },
}

/// The home screen's view model (see the module docs).
pub struct HomeViewModel {
    /// Loads the practice list (started once on creation).
    load: Command0<Vec<PracticeSummary>>,

    /// Creates a new practice from a title; the result carries the created
    /// practice.
    create_project: Command1<SongPractice, String>,

    /// Deletes a practice by id.
    delete_project: Command1<(), String>,

    projects: Vec<PracticeSummary>,

    /// Set in [`HomeViewModel::dispose`]. Every command lands AFTER its
    /// repository call, so the page can be gone by then (leaving home while
    /// the first list read — or a remote-applied reload — is still in
    /// flight). Same guard every other view model in the app carries. (The
    /// router already drops a late result for a page that has left the
    /// stack; this keeps the view model's own word on it.)
    disposed: bool,

    /// A remote change, a return from a practice, or a list read overtaken
    /// by a create or delete, that a fresh load still owes the cards.
    missed_remote: bool,

    /// Bumped by a create or delete changing the list: a load that read it
    /// before them would land over theirs (a deleted card back on the list).
    list_epoch: u64,

    /// The [`list_epoch`](Self::list_epoch) the running load started at.
    load_epoch: u64,

    /// Whether a read of the list has ever succeeded. Until one has, a
    /// create leaves the list alone: added to a list never read, the new
    /// card alone replaced the read error, and the rest of the library
    /// looked lost (the error shows only over an empty list). Not even while
    /// a read is still running, which may fail yet; the read the create owes
    /// brings the card.
    listed: bool,
}

impl std::fmt::Debug for HomeViewModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeViewModel")
            .field("projects", &self.projects.len())
            .field("loading", &self.load.running())
            .field("missed_remote", &self.missed_remote)
            .field("list_epoch", &self.list_epoch)
            .field("listed", &self.listed)
            .finish_non_exhaustive()
    }
}

impl HomeViewModel {
    /// The view model over `repository`, and the first list read it starts
    /// at once (Dart's `load = Command0(_load)..execute()`). Remote-applied
    /// events reach it through [`HomeViewModel::on_remote_applied`].
    pub fn new(repository: Arc<dyn SongPracticeRepository>) -> (Self, Effect) {
        let list = Arc::clone(&repository);
        let create = Arc::clone(&repository);
        let delete = repository;
        let mut vm = HomeViewModel {
            load: Command0::new(move || list.list_practice_summaries().map_err(|e| e.to_string())),
            create_project: Command1::new(move |title: String| {
                create.create_practice(&title).map_err(|e| e.to_string())
            }),
            delete_project: Command1::new(move |id: String| {
                delete.delete_practice(&id).map_err(|e| e.to_string())
            }),
            projects: Vec::new(),
            disposed: false,
            missed_remote: false,
            list_epoch: 0,
            load_epoch: 0,
            listed: false,
        };
        let first = vm
            .execute_load()
            .expect("nothing runs before the first load");
        (vm, first)
    }

    /// The practices the cards show, newest first.
    pub fn projects(&self) -> &[PracticeSummary] {
        &self.projects
    }

    /// The list read's state (`vm.load`).
    pub fn load(&self) -> &Command<Vec<PracticeSummary>> {
        &self.load
    }

    /// The create's state (`vm.createProject`).
    pub fn create_project(&self) -> &Command<SongPractice> {
        &self.create_project
    }

    /// The delete's state (`vm.deleteProject`).
    pub fn delete_project(&self) -> &Command<()> {
        &self.delete_project
    }

    /// `load.execute()`: starts a list read (Retry, and every reload); `None`
    /// while one runs (the command drops a call made while it runs).
    pub fn execute_load(&mut self) -> Option<Effect> {
        let run = self.load.execute()?;
        self.load_epoch = self.list_epoch;
        Some(Effect::Load(run))
    }

    /// `createProject.execute(title)`; `None` while a create runs.
    pub fn execute_create(&mut self, title: String) -> Option<Effect> {
        self.create_project.execute(title).map(Effect::Create)
    }

    /// `deleteProject.execute(id)`; `None` while a delete runs.
    pub fn execute_delete(&mut self, id: String) -> Option<Effect> {
        let run = self.delete_project.execute(id.clone())?;
        Some(Effect::Delete { id, run })
    }

    /// Freshness-only reload when sync applied remote changes to any
    /// practice — its own row (a new/renamed/deleted practice) or a child row
    /// the cards count (a take, a note, a backing track), which ships without
    /// touching the practices table. Held while any command runs and loaded
    /// after it (see [`HomeViewModel::replay_missed`]): dropped, a rename
    /// landing during a load kept its old title until some later reload.
    /// (The technique library's pattern.)
    pub fn on_remote_applied(&mut self, event: &RemoteApplied) -> Option<Effect> {
        if event.practice_ids.is_empty() {
            return None;
        }
        self.missed_remote = true;
        self.replay_missed()
    }

    /// Loads again on return from a practice, for what was changed there. A
    /// load already running read the list before those changes, so another
    /// is owed after it: re-run directly, the command dropped it as
    /// re-entry.
    pub fn refresh(&mut self) -> Option<Effect> {
        self.missed_remote = true;
        self.replay_missed()
    }

    fn busy(&self) -> bool {
        self.load.running() || self.create_project.running() || self.delete_project.running()
    }

    /// Loads again for what [`missed_remote`](Self::missed_remote) owes, once
    /// no command runs (each command asks again as it finishes).
    fn replay_missed(&mut self) -> Option<Effect> {
        if self.disposed || !self.missed_remote || self.busy() {
            return None;
        }
        self.missed_remote = false;
        self.execute_load()
    }

    /// Lands a command's outcome — the rest of its Dart action, then the
    /// command's `finally` — and returns the load the finished command owes,
    /// if any (Dart's `_replayMissed`, run as the command notified).
    pub fn land(&mut self, landing: Landing) -> Option<Effect> {
        match landing {
            Landing::Loaded(outcome) => {
                if self.load_epoch != self.list_epoch {
                    // Read before a create or delete landed: owe a fresh one
                    // instead.
                    self.missed_remote = true;
                } else if let Ok(projects) = &outcome {
                    self.listed = true;
                    self.projects = projects.clone();
                }
                self.load.finish(outcome);
            }
            Landing::Created(outcome) => {
                if let Ok(practice) = &outcome {
                    self.list_epoch += 1;
                    if self.listed {
                        self.projects.insert(0, PracticeSummary::of(practice));
                    }
                }
                self.create_project.finish(outcome);
            }
            Landing::Deleted { id, outcome } => {
                if outcome.is_ok() {
                    self.list_epoch += 1;
                    self.projects.retain(|p| p.id != id);
                }
                self.delete_project.finish(outcome);
            }
        }
        self.replay_missed()
    }

    /// `createProject.clearResult()` (after the screen surfaced its error);
    /// like every command notification, it may start the load owed.
    pub fn clear_create_result(&mut self) -> Option<Effect> {
        self.create_project.clear_result();
        self.replay_missed()
    }

    /// `deleteProject.clearResult()`; see
    /// [`HomeViewModel::clear_create_result`].
    pub fn clear_delete_result(&mut self) -> Option<Effect> {
        self.delete_project.clear_result();
        self.replay_missed()
    }

    /// The screen is gone: nothing more is started (the remote-applied
    /// subscription ends with the page).
    pub fn dispose(&mut self) {
        self.disposed = true;
    }
}

#[cfg(test)]
mod tests;
