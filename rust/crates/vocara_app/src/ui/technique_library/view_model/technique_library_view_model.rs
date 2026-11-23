//! Port of `ui/technique_library/view_model/technique_library_view_model.dart`:
//! the technique library home — a FLAT pool of techniques browsed by a tag
//! filter plus free-text search. There is no collection layer: tags
//! (including framework tags like "EVT"/"CVT") are the only organizing axis.
//!
//! The Dart view model awaited its repository inside three commands. Here
//! each command's run is handed back as an [`Effect`] the screen runs off
//! the UI thread ([`Effect::run`]); what it reports comes back as a
//! [`Done`], which [`TechniqueLibraryViewModel::land`] applies — the code
//! after the Dart `await`, then the command's `finally` (its running flag
//! cleared), then the listener every command had (`_replayMissed`). The
//! generation guards keep their exact meaning:
//!
//! - `list_epoch` is bumped by a create or delete changing the list, so a
//!   load that read it before them is not applied over theirs; the read it
//!   lost is owed instead (`missed_remote`) and replayed once no command
//!   runs.
//! - `listed`: until a read of the list has succeeded, a create leaves the
//!   list alone (the read error shows only over an empty list).
//! - `disposed`: a view model whose page is gone replays nothing.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::technique::Technique;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_string;
use vocara_model::util::id::new_id;
use vocara_services::audio_file_service::AudioFileError;
use vocara_services::technique_media_service::TechniqueMediaFiles;
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_engine::RemoteApplied;

use crate::util::command::{Command, Command0, Command1, Outcome, Run};
use crate::util::{run_caught, ActionError};

/// A tag plus how many techniques carry it — the facets the tag filter
/// lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFacet {
    pub tag: String,
    pub count: usize,
}

/// The sentinel tag used to filter techniques that have no tags at all.
pub const UNTAGGED_FILTER: &str = "\u{0}untagged";

/// The name a technique created with a blank name gets.
pub const DEFAULT_NAME: &str = "New technique";

/// Work a view model method started, to run off the UI thread.
pub enum Effect {
    /// A read of the list (`load`), started at list generation `epoch`.
    Load {
        epoch: u64,
        run: Run<Vec<Technique>, String>,
    },
    /// The save of a new technique (`createTechnique`).
    Create(Run<Technique, String>),
    /// A technique's delete (`deleteTechnique`): its rows, then — once they
    /// are gone — its media, a [`MediaCleanup`] that runs even when the
    /// page has gone meanwhile, as the Dart method ran on.
    Delete {
        id: String,
        run: Run<(), String>,
        media: Arc<dyn TechniqueMediaFiles>,
    },
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::Load { epoch, .. } => f.debug_struct("Load").field("epoch", epoch).finish(),
            Effect::Create(_) => f.write_str("Create"),
            Effect::Delete { id, .. } => f.debug_struct("Delete").field("id", id).finish(),
        }
    }
}

impl Effect {
    /// Runs the effect's work on this thread (it blocks): what it reports,
    /// and the step that follows it, if any (a delete's media cleanup, once
    /// its rows are gone).
    pub fn run(self) -> (Done, Option<MediaCleanup>) {
        match self {
            Effect::Load { epoch, run } => (
                Done::Loaded {
                    epoch,
                    outcome: run.run(),
                },
                None,
            ),
            Effect::Create(run) => (Done::Created(Box::new(run.run())), None),
            Effect::Delete { id, run, media } => {
                let outcome = run.run();
                let cleanup = outcome.is_ok().then(|| MediaCleanup {
                    id: id.clone(),
                    media,
                });
                (Done::Deleted { id, outcome }, cleanup)
            }
        }
    }

    /// What the effect reports when its work never came back (the worker
    /// was lost): a failed run, so its command does not stay running.
    pub fn lost(&self, why: &str) -> Done {
        fn lost<T>(why: &str) -> Outcome<T, String> {
            Err(ActionError::Panicked(why.to_string()))
        }
        match self {
            Effect::Load { epoch, .. } => Done::Loaded {
                epoch: *epoch,
                outcome: lost(why),
            },
            Effect::Create(_) => Done::Created(Box::new(lost(why))),
            Effect::Delete { id, .. } => Done::Deleted {
                id: id.clone(),
                outcome: lost(why),
            },
        }
    }
}

/// A deleted technique's media cleanup: the second step of
/// [`Effect::Delete`].
pub struct MediaCleanup {
    id: String,
    media: Arc<dyn TechniqueMediaFiles>,
}

impl std::fmt::Debug for MediaCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaCleanup")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl MediaCleanup {
    /// Removes the technique's media directory (it blocks). Best effort:
    /// the row is deleted and the card gone, so a failed cleanup must not
    /// report the delete as failed. What it leaves is an orphan for the boot
    /// sweep; the failure is logged by its type only (the message can hold
    /// paths).
    pub fn run(self) -> Done {
        let media = self.media;
        let id = self.id;
        if let Err(e) = run_caught(|| media.delete_technique_media(&id)) {
            let kind = match &e {
                ActionError::Failed(e) => dart_type_name(e),
                ActionError::Panicked(_) => "panic",
            };
            DiagnosticsLog::instance().log(
                "technique",
                &format!("media cleanup after delete failed: {kind}"),
            );
        }
        Done::MediaCleaned { id }
    }

    /// What the cleanup reports when its worker was lost: done all the same
    /// (it is best effort), so the delete does not stay running.
    pub fn lost(&self) -> Done {
        Done::MediaCleaned {
            id: self.id.clone(),
        }
    }
}

/// The Dart `runtimeType` a media failure stood for.
fn dart_type_name(e: &AudioFileError) -> &'static str {
    match e {
        AudioFileError::InvalidId(_) => "ArgumentError",
        AudioFileError::Format(_) => "FormatException",
        AudioFileError::Io(_) => "FileSystemException",
    }
}

/// What an [`Effect`] reported.
#[derive(Debug, Clone, PartialEq)]
pub enum Done {
    /// The list read started at `epoch`.
    Loaded {
        epoch: u64,
        outcome: Outcome<Vec<Technique>, String>,
    },
    /// The new technique's save.
    Created(Box<Outcome<Technique, String>>),
    /// The rows of technique `id` deleted (or not).
    Deleted {
        id: String,
        outcome: Outcome<(), String>,
    },
    /// The media cleanup after technique `id`'s delete finished.
    MediaCleaned { id: String },
}

/// ViewModel for the technique library home (see the module docs).
pub struct TechniqueLibraryViewModel {
    media: Arc<dyn TechniqueMediaFiles>,

    /// Loads all techniques (runs once on creation; re-run on return from
    /// detail).
    load: Command0<Vec<Technique>>,
    /// Creates a named technique; its result carries the created technique.
    create_technique: Command1<Technique, String>,
    /// Deletes a technique by id (also removes its media files).
    delete_technique: Command1<(), String>,

    all: Vec<Technique>,
    /// The active tag filter, or `None` for "All". May be
    /// [`UNTAGGED_FILTER`].
    active_tag: Option<String>,
    query: String,

    /// A remote change, or a list read overtaken by a create or delete, that
    /// a fresh load still owes the cards. Skipped and forgotten, a remote
    /// rename that landed during a load kept its old name until some later
    /// reload.
    missed_remote: bool,
    /// Bumped by a create or delete changing the list: a load that read it
    /// before them would land over theirs (a deleted card back on the grid).
    list_epoch: u64,
    /// Whether a read of the list has ever succeeded. Until one has, a
    /// create leaves the list alone after a failed read: added to a list
    /// never read, the new card alone replaced the read error, and the rest
    /// of the library looked lost (the error shows only over an empty list).
    listed: bool,
    /// Set in [`dispose`](Self::dispose): a view model whose page is gone
    /// starts no more loads.
    disposed: bool,
}

impl std::fmt::Debug for TechniqueLibraryViewModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TechniqueLibraryViewModel")
            .field("techniques", &self.all.len())
            .field("active_tag", &self.active_tag)
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl TechniqueLibraryViewModel {
    /// The view model, and the load it starts at once (Dart's
    /// `load = Command0(_load)..execute()`).
    pub fn new(
        repository: Arc<dyn TechniqueRepository>,
        media: Arc<dyn TechniqueMediaFiles>,
    ) -> (Self, Option<Effect>) {
        let list = Arc::clone(&repository);
        let save = Arc::clone(&repository);
        let delete = repository;
        let mut vm = Self {
            media,
            load: Command0::new(move || list.list_techniques().map_err(|e| e.to_string())),
            create_technique: Command1::new(move |name: String| {
                let now = DateTime::now();
                let trimmed = dart_string::trim(&name);
                let technique = Technique::new(
                    new_id(),
                    if trimmed.is_empty() {
                        DEFAULT_NAME
                    } else {
                        trimmed
                    },
                    now,
                    now,
                );
                save.save_technique(&technique).map_err(|e| e.to_string())
            }),
            delete_technique: Command1::new(move |id: String| {
                delete.delete_technique(&id).map_err(|e| e.to_string())
            }),
            all: Vec::new(),
            active_tag: None,
            query: String::new(),
            missed_remote: false,
            list_epoch: 0,
            listed: false,
            disposed: false,
        };
        let load = vm.execute_load();
        (vm, load)
    }

    /// The load command's state.
    pub fn load(&self) -> &Command<Vec<Technique>> {
        &self.load
    }

    /// The create command's state.
    pub fn create_technique(&self) -> &Command<Technique> {
        &self.create_technique
    }

    /// The delete command's state.
    pub fn delete_technique(&self) -> &Command<()> {
        &self.delete_technique
    }

    /// Every technique, as last loaded (and edited by creates and deletes).
    pub fn all(&self) -> &[Technique] {
        &self.all
    }

    /// The active tag filter, or `None` for "All". May be
    /// [`UNTAGGED_FILTER`].
    pub fn active_tag(&self) -> Option<&str> {
        self.active_tag.as_deref()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether a command runs (a reload waits for all three).
    fn busy(&self) -> bool {
        self.load.running() || self.create_technique.running() || self.delete_technique.running()
    }

    pub fn set_tag(&mut self, tag: Option<String>) {
        if self.active_tag == tag {
            return;
        }
        self.active_tag = tag;
    }

    /// Whether `tag` is the active filter, compared case-insensitively (tags
    /// are folded by case, so the active value may differ in casing from a
    /// card's tag string).
    pub fn is_tag_active(&self, tag: &str) -> bool {
        match self.active_tag.as_deref() {
            None | Some(UNTAGGED_FILTER) => false,
            Some(active) => fold(active) == fold(tag),
        }
    }

    pub fn set_query(&mut self, q: impl Into<String>) {
        let q = q.into();
        if self.query == q {
            return;
        }
        self.query = q;
    }

    /// All tags in the library with their counts, alphabetically. Used for
    /// the tag filter. Tags are folded case-insensitively (so "EVT" and
    /// "evt" are one entry), keeping the first-seen casing as the display
    /// label — consistent with how `parseCommaList` dedupes tags within a
    /// single technique.
    pub fn tag_facets(&self) -> Vec<TagFacet> {
        // Lowercased tag -> its facet (shown casing, count).
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut facets: Vec<TagFacet> = Vec::new();
        for t in &self.all {
            let mut seen: Vec<String> = Vec::new();
            for tag in &t.tags {
                let key = fold(tag);
                if seen.contains(&key) {
                    continue; // count each technique once per tag
                }
                seen.push(key.clone());
                match index.get(&key) {
                    Some(&i) => facets[i].count += 1,
                    None => {
                        index.insert(key, facets.len());
                        facets.push(TagFacet {
                            tag: tag.clone(),
                            count: 1,
                        });
                    }
                }
            }
        }
        facets.sort_by(|a, b| compare_utf16(&fold(&a.tag), &fold(&b.tag)));
        facets
    }

    /// How many techniques carry no tags (for the optional "Untagged"
    /// entry).
    pub fn untagged_count(&self) -> usize {
        self.all.iter().filter(|t| t.tags.is_empty()).count()
    }

    /// Techniques matching the active tag filter and search query.
    pub fn filtered(&self) -> Vec<&Technique> {
        let q = fold(dart_string::trim(&self.query));
        let active_lower = self.active_tag.as_deref().map(fold);
        let matches_tag = |t: &Technique| match self.active_tag.as_deref() {
            None => true,
            Some(UNTAGGED_FILTER) => t.tags.is_empty(),
            Some(_) => t.tags.iter().any(|tag| Some(fold(tag)) == active_lower),
        };
        let matches_query = |t: &Technique| {
            q.is_empty()
                || fold(&t.name).contains(&q)
                || fold(&t.summary).contains(&q)
                || t.aliases.iter().any(|a| fold(a).contains(&q))
                || t.tags.iter().any(|tag| fold(tag).contains(&q))
        };
        self.all
            .iter()
            .filter(|t| matches_tag(t) && matches_query(t))
            .collect()
    }

    /// `load.execute()`: a read of the list, unless one runs already (the
    /// command drops a re-entry).
    pub fn execute_load(&mut self) -> Option<Effect> {
        let epoch = self.list_epoch;
        self.load.execute().map(|run| Effect::Load { epoch, run })
    }

    /// `createTechnique.execute(name)`: saves a technique named `name`
    /// (trimmed; [`DEFAULT_NAME`] when blank), unless a create runs already.
    pub fn execute_create(&mut self, name: impl Into<String>) -> Option<Effect> {
        self.create_technique
            .execute(name.into())
            .map(Effect::Create)
    }

    /// `deleteTechnique.execute(id)`: deletes technique `id` and then its
    /// media, unless a delete runs already.
    pub fn execute_delete(&mut self, id: impl Into<String>) -> Option<Effect> {
        let id = id.into();
        let media = Arc::clone(&self.media);
        self.delete_technique
            .execute(id.clone())
            .map(|run| Effect::Delete { id, run, media })
    }

    /// Loads again on return from a technique's page, for its edits and
    /// deletes. A load already running read the list before them, so
    /// another is owed after it: re-run directly, the command dropped it as
    /// re-entry and that older read put a deleted card back on the grid.
    pub fn refresh(&mut self) -> Option<Effect> {
        self.missed_remote = true;
        self.replay_missed()
    }

    /// Freshness-only reload when sync applied remote changes to any
    /// technique: its own row, or only its example clips (which the cards
    /// count, and which ship without touching the techniques table). Skipped
    /// while any command runs — a skipped reload can never lose data
    /// (three-way saves merge against base hashes) — and owed until none
    /// does.
    pub fn on_remote_applied(&mut self, event: &RemoteApplied) -> Option<Effect> {
        if event.technique_ids.is_empty() {
            return None;
        }
        self.missed_remote = true;
        self.replay_missed()
    }

    /// Loads again for what `missed_remote` owes, once no command runs (each
    /// command asks again as it finishes).
    fn replay_missed(&mut self) -> Option<Effect> {
        if self.disposed || !self.missed_remote || self.busy() {
            return None;
        }
        self.missed_remote = false;
        self.execute_load()
    }

    /// Applies what an effect reported: the rest of the Dart method after
    /// its `await`, then — when its command's run is over — the command's
    /// `finally` and the replay of a load owed meanwhile. Returns the load
    /// that replay started, if any.
    pub fn land(&mut self, done: Done) -> Option<Effect> {
        match done {
            Done::Loaded { epoch, outcome } => {
                // A run that panicked never got past its `await` (Dart's
                // throw), so only a returned result is weighed.
                if !matches!(outcome, Err(ActionError::Panicked(_))) {
                    if epoch != self.list_epoch {
                        // Read before a create or delete landed: owe a fresh
                        // one instead.
                        self.missed_remote = true;
                    } else if let Ok(list) = &outcome {
                        self.listed = true;
                        self.all = list.clone();
                        self.drop_stale_tag_filter();
                    }
                }
                self.load.finish(outcome);
            }
            Done::Created(outcome) => {
                if let Ok(technique) = outcome.as_ref() {
                    self.list_epoch += 1;
                    if self.listed {
                        self.all.insert(0, technique.clone());
                    }
                }
                self.create_technique.finish(*outcome);
            }
            Done::Deleted { id, outcome } => {
                if outcome.is_err() {
                    self.delete_technique.finish(outcome);
                    return self.replay_missed();
                }
                self.list_epoch += 1;
                self.all.retain(|t| t.id != id);
                self.drop_stale_tag_filter();
                // Still running: its media cleanup follows.
                return None;
            }
            Done::MediaCleaned { .. } => {
                // The cleanup follows only a delete whose rows went.
                self.delete_technique.finish(Ok(()));
            }
        }
        self.replay_missed()
    }

    /// Drops an active filter that can no longer match after edits/deletes,
    /// so we never get stuck on an empty grid with the filter gone from the
    /// list. Covers both a tag that no longer exists (case-insensitive) and
    /// the "Untagged" filter once every technique has a tag. Both paths that
    /// can shrink the list run it — a delete prunes the list without a
    /// reload, so deleting the last technique a filter matches would
    /// otherwise strand it. (A create only adds, so it can never make a
    /// filter stale.)
    fn drop_stale_tag_filter(&mut self) {
        let Some(active) = self.active_tag.as_deref() else {
            return;
        };
        let stale = if active == UNTAGGED_FILTER {
            self.all.iter().all(|t| !t.tags.is_empty())
        } else {
            let active_lower = fold(active);
            !self
                .all
                .iter()
                .any(|t| t.tags.iter().any(|tag| fold(tag) == active_lower))
        };
        if stale {
            self.active_tag = None;
        }
    }

    /// The page is gone: no more loads start.
    pub fn dispose(&mut self) {
        self.disposed = true;
    }
}

/// Dart's `String.toLowerCase()` (the language-independent Unicode
/// mapping).
fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Dart's `String.compareTo`: lexicographic over UTF-16 code units.
fn compare_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests;
