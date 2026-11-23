//! Test support for the home screen: a practice repository whose list a test
//! edits as another device or this one would (the Dart tests' `_Practices`,
//! `_HeldPracticeRepo`, `_FakeRepo`), summaries, remote-applied events, and a
//! runner that drives tasks to completion.
//!
//! The Dart fakes held a read open on a `Completer` to make it land after
//! something else. Here an [`Effect`](super::view_model::Effect) or a task
//! is a value: a test holds it, and runs it (the read happens then) or lands
//! its outcome (the read returns then) when it chooses.

use std::sync::{Arc, Mutex, MutexGuard};

use iced::Task;
use indexmap::IndexMap;
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::dart_date_time::DateTime;
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::sync_engine::RemoteApplied;
use vocara_store::sync::sync_hashes::Loaded;
use vocara_store::{Error, Result};

/// A remote page that touched `practices` and `techniques`, carrying `rows`.
pub(crate) fn remote_applied(
    rows: &[(&str, &str)],
    practices: &[&str],
    techniques: &[&str],
) -> RemoteApplied {
    RemoteApplied {
        rows: rows
            .iter()
            .map(|(t, id)| (t.to_string(), id.to_string()))
            .collect(),
        practice_ids: practices.iter().map(|s| s.to_string()).collect(),
        technique_ids: techniques.iter().map(|s| s.to_string()).collect(),
    }
}

/// A summary as the Dart tests built them: no takes, no backing, no notes.
pub(crate) fn summary(id: &str, title: &str) -> PracticeSummary {
    PracticeSummary {
        id: id.to_string(),
        title: title.to_string(),
        updated_at: DateTime::local_ymd(2026, 1, 1),
        take_count: 0,
        has_accompaniment: false,
        note_count: 0,
    }
}

/// What a [`FakePractices`] holds, and what it was asked.
#[derive(Debug, Default)]
pub(crate) struct State {
    /// The stored list, newest first.
    pub practices: Vec<PracticeSummary>,
    /// Whether a list read fails.
    pub unreadable: bool,
    /// Whether a create fails.
    pub uncreatable: bool,
    /// Whether a delete fails.
    pub undeletable: bool,
    pub summary_reads: usize,
    pub creates: usize,
    pub deletes: usize,
}

/// A practice repository over a list a test edits.
#[derive(Debug, Default)]
pub(crate) struct FakePractices {
    state: Mutex<State>,
}

impl FakePractices {
    pub(crate) fn with(practices: Vec<PracticeSummary>) -> Arc<Self> {
        let repo = Self::default();
        repo.state().practices = practices;
        Arc::new(repo)
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SongPracticeRepository for FakePractices {
    fn list_practices(&self) -> Result<Vec<SongPractice>> {
        Ok(Vec::new())
    }

    fn list_practice_summaries(&self) -> Result<Vec<PracticeSummary>> {
        let mut s = self.state();
        s.summary_reads += 1;
        if s.unreadable {
            return Err(Error::State("read".into()));
        }
        Ok(s.practices.clone())
    }

    fn get_practice(&self, _id: &str) -> Result<SongPractice> {
        Err(Error::State("not found".into()))
    }

    fn load_practice(&self, _id: &str) -> Result<Loaded<SongPractice>> {
        Err(Error::State("not found".into()))
    }

    fn create_practice(&self, title: &str) -> Result<SongPractice> {
        let mut s = self.state();
        s.creates += 1;
        if s.uncreatable {
            return Err(Error::State("create".into()));
        }
        let at = DateTime::local_ymd(2026, 6, 20);
        let p = SongPractice::new(format!("P{}", 100 + s.creates), title, at, at);
        s.practices.insert(0, PracticeSummary::of(&p));
        Ok(p)
    }

    fn save_practice(&self, practice: &SongPractice) -> Result<SongPractice> {
        Ok(practice.clone())
    }

    fn save_practice_with_base(
        &self,
        _practice: &SongPractice,
        _base_hashes: &IndexMap<String, String>,
    ) -> Result<Loaded<SongPractice>> {
        Err(Error::State("unsupported".into()))
    }

    fn delete_practice(&self, id: &str) -> Result<()> {
        let mut s = self.state();
        s.deletes += 1;
        if s.undeletable {
            return Err(Error::State("delete".into()));
        }
        s.practices.retain(|p| p.id != id);
        Ok(())
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a test runtime")
}

/// Runs `task` to completion on a tokio runtime (its blocking work on the
/// runtime's blocking pool, as in the app) and returns the messages it
/// produced. Widget operations (a focus) are dropped.
pub(crate) fn run_task<M: Send + 'static>(task: Task<M>) -> Vec<M> {
    use iced::futures::StreamExt;
    use iced_test::runtime::Action;
    let Some(stream) = iced_test::runtime::task::into_stream(task) else {
        return Vec::new();
    };
    runtime().block_on(
        stream
            .filter_map(|action| async move {
                match action {
                    Action::Output(m) => Some(m),
                    _ => None,
                }
            })
            .collect(),
    )
}

/// Runs `task` on a thread of its own and hands each message over as it is
/// produced: for a task with a part that waits on the test.
pub(crate) fn spawn_task<M: Send + 'static>(task: Task<M>) -> std::sync::mpsc::Receiver<M> {
    use iced::futures::StreamExt;
    use iced_test::runtime::Action;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Some(stream) = iced_test::runtime::task::into_stream(task) else {
            return;
        };
        runtime().block_on(stream.for_each(move |action| {
            if let Action::Output(m) = action {
                let _ = sender.send(m);
            }
            async {}
        }));
    });
    receiver
}
