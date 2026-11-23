//! The library view model: tag facets, filtering, and its generation guards
//! (`technique_library_layout_test.dart`'s view-model case,
//! `list_read_failure_create_test.dart`, `list_remote_refresh_test.dart`
//! and `technique_media_pool_guard_test.dart`'s library case).
//!
//! The Dart tests held a read open with a completer. Here a held read is an
//! effect that has run (it read the list then) and whose [`Done`] lands
//! later.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vocara_model::models::technique::Technique;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::audio_file_service::AudioFileError;
use vocara_services::technique_media_service::TechniqueMediaFiles;
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::sync_engine::RemoteApplied;
use vocara_store::sync::sync_hashes::Loaded;
// `IndexMap<String, String>`, the base hashes' type, without naming
// `indexmap` (not a dependency of the app).
use vocara_store::sync::sync_row::FieldClocks as BaseHashes;

use super::*;
use crate::app::library::Library;

/// A remote page carrying `rows`, touching `practices` and `techniques`.
pub(crate) fn remote(
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

/// A technique list a test edits as another device (or this one) would.
#[derive(Default)]
pub(crate) struct FakeRepo {
    pub techniques: Mutex<Vec<Technique>>,
    /// Reads fail while set.
    pub unreadable: AtomicBool,
    /// Reads still to fail before they succeed.
    pub fail_reads: AtomicUsize,
    pub reads: AtomicUsize,
}

impl FakeRepo {
    pub fn with(techniques: Vec<Technique>) -> Arc<Self> {
        Arc::new(Self {
            techniques: Mutex::new(techniques),
            ..Self::default()
        })
    }

    pub fn set(&self, techniques: Vec<Technique>) {
        *self.techniques.lock().unwrap() = techniques;
    }

    pub fn ids(&self) -> Vec<String> {
        self.techniques
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.id.clone())
            .collect()
    }
}

fn unsupported<T>() -> vocara_store::Result<T> {
    Err(vocara_store::Error::State("unsupported".into()))
}

impl TechniqueRepository for FakeRepo {
    fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let failing = self
            .fail_reads
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing || self.unreadable.load(Ordering::SeqCst) {
            return Err(vocara_store::Error::State("simulated read failure".into()));
        }
        Ok(self.techniques.lock().unwrap().clone())
    }

    fn get_technique(&self, id: &str) -> vocara_store::Result<Technique> {
        self.techniques
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| vocara_store::Error::State("not found".into()))
    }

    fn load_technique(&self, _id: &str) -> vocara_store::Result<Loaded<Technique>> {
        unsupported()
    }

    /// Stores a new technique first, as the list reads newest first.
    fn save_technique(&self, technique: &Technique) -> vocara_store::Result<Technique> {
        self.techniques.lock().unwrap().insert(0, technique.clone());
        Ok(technique.clone())
    }

    fn save_technique_with_base(
        &self,
        _technique: &Technique,
        _base_hashes: &BaseHashes,
    ) -> vocara_store::Result<Loaded<Technique>> {
        unsupported()
    }

    /// Forgets the technique, so a later reload sees the library the view
    /// model was left with.
    fn delete_technique(&self, id: &str) -> vocara_store::Result<()> {
        self.techniques.lock().unwrap().retain(|t| t.id != id);
        Ok(())
    }
}

/// Media files whose cleanups are counted and can fail.
#[derive(Default)]
pub(crate) struct FakeMedia {
    pub cleanups: Mutex<Vec<String>>,
    pub fail: bool,
}

impl TechniqueMediaFiles for FakeMedia {
    fn import_media(
        &self,
        _technique_id: &str,
        _source_path: &str,
    ) -> Result<String, AudioFileError> {
        Err(AudioFileError::Format("unused".into()))
    }

    fn delete_media(&self, _path: &str) -> Result<(), AudioFileError> {
        Ok(())
    }

    fn delete_technique_media(&self, technique_id: &str) -> Result<(), AudioFileError> {
        self.cleanups.lock().unwrap().push(technique_id.to_string());
        if self.fail {
            return Err(AudioFileError::Io(std::io::Error::other(
                "injected delete failure",
            )));
        }
        Ok(())
    }
}

/// `_t(id, name, tags)`: a technique with a summary long enough to fill a
/// card.
pub(crate) fn technique(id: &str, name: &str, tags: &[&str]) -> Technique {
    let now = DateTime::local_ymd(2026, 6, 20);
    let mut t = Technique::new(id, name, now, now);
    t.tags = tags.iter().map(|s| s.to_string()).collect();
    t.summary = format!("A summary for {name} that is reasonably long to fill the card.");
    t
}

fn vm_over(repo: &Arc<FakeRepo>) -> (TechniqueLibraryViewModel, Option<Effect>) {
    TechniqueLibraryViewModel::new(repo.clone(), Arc::new(FakeMedia::default()))
}

/// Runs `effect` to the end — a delete's media cleanup too — landing all it
/// reports and running every load those landings start.
pub(crate) fn settle(vm: &mut TechniqueLibraryViewModel, effect: Option<Effect>) {
    let mut queue: VecDeque<Effect> = effect.into_iter().collect();
    while let Some(effect) = queue.pop_front() {
        let (done, cleanup) = effect.run();
        queue.extend(vm.land(done));
        if let Some(cleanup) = cleanup {
            queue.extend(vm.land(cleanup.run()));
        }
    }
}

fn names(vm: &TechniqueLibraryViewModel) -> Vec<&str> {
    vm.all().iter().map(|t| t.name.as_str()).collect()
}

fn ids(list: &[&Technique]) -> Vec<String> {
    list.iter().map(|t| t.id.clone()).collect()
}

fn all_ids(vm: &TechniqueLibraryViewModel) -> Vec<String> {
    vm.all().iter().map(|t| t.id.clone()).collect()
}

#[test]
fn the_load_starts_at_once_and_fills_the_list() {
    let repo = FakeRepo::with(vec![technique("1", "Twang", &["EVT"])]);
    let (mut vm, load) = vm_over(&repo);
    assert!(vm.load().running(), "loading from the start");
    settle(&mut vm, load);
    assert!(vm.load().completed());
    assert_eq!(names(&vm), ["Twang"]);
}

#[test]
fn tag_facets_fold_case_count_each_technique_once_and_sort() {
    let repo = FakeRepo::with(vec![
        technique("1", "Twang", &["EVT", "twang", "evt"]),
        technique("2", "Curbing", &["cvt", "Evt"]),
        technique("3", "Mixed voice", &[]),
        technique("4", "Belt", &["Belt", "BELT"]),
    ]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    let facets = vm.tag_facets();
    assert_eq!(
        facets,
        vec![
            TagFacet {
                tag: "Belt".into(),
                count: 1
            },
            TagFacet {
                tag: "cvt".into(),
                count: 1
            },
            // The first-seen casing, each technique once.
            TagFacet {
                tag: "EVT".into(),
                count: 2
            },
            TagFacet {
                tag: "twang".into(),
                count: 1
            },
        ]
    );
    assert_eq!(vm.untagged_count(), 1);
}

#[test]
fn the_filter_matches_tags_case_insensitively_and_untagged_alone() {
    let repo = FakeRepo::with(vec![
        technique("1", "Twang", &["EVT"]),
        technique("2", "Curbing", &["CVT"]),
        technique("3", "Mixed voice", &[]),
    ]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    assert_eq!(ids(&vm.filtered()), ["1", "2", "3"]);

    vm.set_tag(Some("evt".into()));
    assert_eq!(ids(&vm.filtered()), ["1"]);
    assert!(vm.is_tag_active("EVT") && !vm.is_tag_active("CVT"));

    vm.set_tag(Some(UNTAGGED_FILTER.into()));
    assert_eq!(ids(&vm.filtered()), ["3"]);
    // "Untagged" is no tag a card carries.
    assert!(!vm.is_tag_active(UNTAGGED_FILTER));

    vm.set_tag(None);
    assert!(!vm.is_tag_active("EVT"));
}

#[test]
fn the_search_reads_names_summaries_aliases_and_tags_trimmed_and_folded() {
    let mut twang = technique("1", "Twang", &["Quality"]);
    twang.aliases = vec!["Edge".into()];
    let mut curbing = technique("2", "Curbing", &["CVT"]);
    curbing.summary = "A held-back, rounded sound.".into();
    let repo = FakeRepo::with(vec![twang, curbing]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    for (q, want) in [
        ("  twa ", vec!["1"]),
        ("EDGE", vec!["1"]),
        ("qual", vec!["1"]),
        ("rounded", vec!["2"]),
        ("cvt", vec!["2"]),
        ("   ", vec!["1", "2"]),
        ("nothing", vec![]),
    ] {
        vm.set_query(q);
        assert_eq!(ids(&vm.filtered()), want, "{q:?}");
        assert_eq!(vm.query(), q);
    }
    // The tag filter and the search combine.
    vm.set_query("curb");
    vm.set_tag(Some("quality".into()));
    assert!(vm.filtered().is_empty());
}

// technique_library_layout_test: the reload drops a filter that can no
// longer match (so the grid is never stuck empty with its filter gone), but
// the delete only pruned the list — deleting the sole EVT technique left
// "No techniques match this filter." under a ghost EVT filter.
#[test]
fn deleting_the_last_technique_a_filter_matches_drops_the_filter_as_a_reload_would() {
    let repo = FakeRepo::with(vec![
        technique("1", "Twang", &["EVT"]),
        technique("2", "Belting", &["EVT"]),
        technique("3", "Curbing", &["CVT"]),
        technique("4", "Mixed voice", &[]),
    ]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    assert_eq!(vm.all().len(), 4);

    // A filter that still matches something survives the delete…
    vm.set_tag(Some("evt".into()));
    let delete = vm.execute_delete("1");
    settle(&mut vm, delete);
    assert_eq!(vm.active_tag(), Some("evt"));
    assert_eq!(ids(&vm.filtered()), ["2"]);

    // …the last match going drops it, back to the whole library.
    let delete = vm.execute_delete("2");
    settle(&mut vm, delete);
    assert_eq!(vm.active_tag(), None);
    assert_eq!(ids(&vm.filtered()), ["3", "4"]);

    // Same for "Untagged" once no untagged technique is left.
    vm.set_tag(Some(UNTAGGED_FILTER.into()));
    assert_eq!(ids(&vm.filtered()), ["4"]);
    let delete = vm.execute_delete("4");
    settle(&mut vm, delete);
    assert_eq!(vm.active_tag(), None);
    assert_eq!(ids(&vm.filtered()), ["3"]);
    assert_eq!(repo.ids(), ["3"], "the deletes themselves ran");
}

#[test]
fn a_reload_drops_a_filter_whose_tag_is_gone() {
    let repo = FakeRepo::with(vec![technique("1", "Twang", &["EVT"])]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    vm.set_tag(Some("EVT".into()));
    repo.set(vec![technique("1", "Twang", &["CVT"])]);
    let reload = vm.refresh();
    settle(&mut vm, reload);
    assert_eq!(vm.active_tag(), None);
}

#[test]
fn a_create_names_the_technique_and_puts_it_first() {
    let repo = FakeRepo::with(vec![technique("1", "Twang", &[])]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);

    let create = vm.execute_create("  Belt  ");
    assert!(vm.create_technique().running());
    // One at a time: a second is dropped as re-entry.
    assert!(vm.execute_create("Again").is_none());
    settle(&mut vm, create);
    let Some(Ok(created)) = vm.create_technique().result() else {
        panic!("created")
    };
    assert_eq!(created.name, "Belt");
    assert_eq!(created.created_at, created.updated_at);
    assert_eq!(names(&vm), ["Belt", "Twang"]);

    // A blank name is "New technique".
    let create = vm.execute_create("   ");
    settle(&mut vm, create);
    assert_eq!(names(&vm)[0], DEFAULT_NAME);
}

// list_read_failure_create_test: Home and the technique library show their
// read error only over an empty list. A create after a failed read added its
// card to the list never read: the error gave way to that one card, every
// later read failed too, and the rest of the library looked lost.
#[test]
fn the_library_keeps_its_read_error_over_a_technique_created_after_it() {
    let kept = Technique::new(
        "T1",
        "Kept",
        DateTime::local_ymd(2026, 6, 20),
        DateTime::local_ymd(2026, 6, 20),
    );
    let repo = FakeRepo::with(vec![kept]);
    repo.unreadable.store(true, Ordering::SeqCst);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    assert!(vm.load().error());

    let create = vm.execute_create("New");
    settle(&mut vm, create);
    assert!(vm.create_technique().completed());
    assert!(vm.all().is_empty(), "the error still shows");

    repo.unreadable.store(false, Ordering::SeqCst);
    let reload = vm.refresh();
    settle(&mut vm, reload);
    assert_eq!(names(&vm), ["New", "Kept"]);
}

// The guard also let a create through while the first read still ran (its
// error not yet known): that read and the one the create owed then failed,
// and the one new card stood in for the whole library.
#[test]
fn the_library_shows_no_lone_new_card_when_the_read_it_overtook_fails() {
    let repo = FakeRepo::with(Vec::new());
    repo.unreadable.store(true, Ordering::SeqCst);
    let (mut vm, held) = vm_over(&repo);
    assert!(vm.load().running());
    let create = vm.execute_create("New");
    settle(&mut vm, create);
    settle(&mut vm, held);
    assert!(vm.load().error());
    assert!(vm.all().is_empty(), "the error still shows");
    assert_eq!(repo.reads.load(Ordering::SeqCst), 2, "the owed read ran");
}

// list_remote_refresh_test: skipped while any command ran and forgotten, the
// card kept its old name until some later reload.
#[test]
fn a_remote_change_landing_during_a_load_is_loaded_after_it() {
    let repo = FakeRepo::with(vec![technique("a", "Twang", &[])]);
    let (mut vm, load) = vm_over(&repo);
    // The first read happens now; it lands later.
    let (first, _) = load.expect("the first load").run();
    assert!(vm.load().running());
    repo.set(vec![technique("a", "Twang (renamed)", &[])]);
    assert!(
        vm.on_remote_applied(&remote(&[], &[], &["a"])).is_none(),
        "not while the load runs"
    );
    let owed = vm.land(first);
    settle(&mut vm, owed);
    assert_eq!(names(&vm), ["Twang (renamed)"]);
}

#[test]
fn a_load_read_before_a_delete_does_not_bring_the_card_back() {
    let repo = FakeRepo::with(vec![technique("a", "A", &[]), technique("b", "B", &[])]);
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    assert_eq!(all_ids(&vm), ["a", "b"]);
    // A remote event's reload reads the list, and is held there.
    let reload = vm.on_remote_applied(&remote(&[], &[], &["b"]));
    let (held, _) = reload.expect("a reload").run();
    assert!(vm.load().running());
    let delete = vm.execute_delete("a");
    settle(&mut vm, delete);
    assert_eq!(all_ids(&vm), ["b"]);
    // The older read lands.
    let owed = vm.land(held);
    settle(&mut vm, owed);
    assert_eq!(all_ids(&vm), ["b"]);
}

// The return's load was dropped as re-entry, and the running one had read
// the list before the delete made on that page: the card stayed.
#[test]
fn a_return_from_a_technique_page_while_a_load_runs_loads_again_after_it() {
    let repo = FakeRepo::with(vec![technique("a", "A", &[]), technique("b", "B", &[])]);
    let (mut vm, load) = vm_over(&repo);
    let (held, _) = load.expect("the first load").run();
    assert!(vm.load().running());
    repo.set(vec![technique("b", "B", &[])]); // deleted on its page
    assert!(vm.refresh().is_none(), "owed, not dropped");
    let owed = vm.land(held);
    settle(&mut vm, owed);
    assert_eq!(all_ids(&vm), ["b"]);
}

// The library's cards count example clips, which live in child rows: a
// remote page carrying only those must refresh the list too.
#[test]
fn the_library_reloads_when_a_remote_page_carries_only_a_techniques_examples() {
    let repo = FakeRepo::with(Vec::new());
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    assert_eq!(repo.reads.load(Ordering::SeqCst), 1);

    let reload = vm.on_remote_applied(&remote(&[("technique_media", "M1")], &[], &["T1"]));
    settle(&mut vm, reload);
    assert_eq!(repo.reads.load(Ordering::SeqCst), 2);

    // A page touching no technique leaves the library alone.
    let reload = vm.on_remote_applied(&remote(&[("tracks", "TR1")], &["P1"], &[]));
    assert!(reload.is_none());
    assert_eq!(repo.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn a_disposed_view_model_loads_nothing_more() {
    let repo = FakeRepo::with(Vec::new());
    let (mut vm, load) = vm_over(&repo);
    settle(&mut vm, load);
    vm.dispose();
    assert!(vm.refresh().is_none());
    assert!(vm.on_remote_applied(&remote(&[], &[], &["T1"])).is_none());
    assert_eq!(repo.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_delete_runs_until_its_media_is_cleaned_up() {
    let repo = FakeRepo::with(vec![technique("a", "A", &[]), technique("b", "B", &[])]);
    let media = Arc::new(FakeMedia::default());
    let (mut vm, load) = TechniqueLibraryViewModel::new(repo.clone(), media.clone());
    settle(&mut vm, load);
    let (deleted, cleanup) = vm.execute_delete("a").expect("a delete").run();
    assert!(vm.land(deleted).is_none());
    // The card is gone while its media is still being removed, and the
    // delete still runs: a second waits.
    assert_eq!(all_ids(&vm), ["b"]);
    assert!(vm.delete_technique().running());
    assert!(vm.execute_delete("b").is_none());
    let cleanup = cleanup.expect("a cleanup follows");
    assert!(vm.land(cleanup.run()).is_none());
    assert!(vm.delete_technique().completed());
    assert_eq!(*media.cleanups.lock().unwrap(), ["a"]);
}

#[test]
fn a_failed_delete_keeps_the_card_and_cleans_up_nothing() {
    struct Refusing(Arc<FakeRepo>);
    impl TechniqueRepository for Refusing {
        fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
            self.0.list_techniques()
        }
        fn get_technique(&self, id: &str) -> vocara_store::Result<Technique> {
            self.0.get_technique(id)
        }
        fn load_technique(&self, id: &str) -> vocara_store::Result<Loaded<Technique>> {
            self.0.load_technique(id)
        }
        fn save_technique(&self, t: &Technique) -> vocara_store::Result<Technique> {
            self.0.save_technique(t)
        }
        fn save_technique_with_base(
            &self,
            t: &Technique,
            base: &BaseHashes,
        ) -> vocara_store::Result<Loaded<Technique>> {
            self.0.save_technique_with_base(t, base)
        }
        fn delete_technique(&self, _id: &str) -> vocara_store::Result<()> {
            Err(vocara_store::Error::State("refused".into()))
        }
    }
    let inner = FakeRepo::with(vec![technique("a", "A", &[])]);
    let media = Arc::new(FakeMedia::default());
    let (mut vm, load) = TechniqueLibraryViewModel::new(Arc::new(Refusing(inner)), media.clone());
    settle(&mut vm, load);
    let delete = vm.execute_delete("a");
    settle(&mut vm, delete);
    assert!(vm.delete_technique().error());
    assert_eq!(all_ids(&vm), ["a"]);
    assert!(media.cleanups.lock().unwrap().is_empty());
}

// technique_media_pool_guard_test: the list said "Couldn't delete the
// technique." for one it had deleted, when its files could not all go.
#[test]
fn a_technique_whose_files_could_not_all_be_deleted_is_still_deleted() {
    let library = Library::in_memory();
    let repo = library.technique_repo.clone();
    let now = DateTime::now();
    let id = vocara_model::util::id::new_id();
    repo.save_technique(&Technique::new(id.clone(), "Twang", now, now))
        .expect("saved");
    let media = Arc::new(FakeMedia {
        fail: true,
        ..FakeMedia::default()
    });
    let (mut vm, load) = TechniqueLibraryViewModel::new(repo.clone(), media.clone());
    settle(&mut vm, load);
    let delete = vm.execute_delete(id.clone());
    settle(&mut vm, delete);
    assert!(vm.delete_technique().completed());
    assert!(repo.get_technique(&id).is_err(), "the delete itself ran");
    assert_eq!(*media.cleanups.lock().unwrap(), [id]);
    assert!(vm.all().is_empty());
}

#[test]
fn a_load_that_panicked_is_an_error_and_owes_nothing() {
    struct Panicking;
    impl TechniqueRepository for Panicking {
        fn list_techniques(&self) -> vocara_store::Result<Vec<Technique>> {
            panic!("boom")
        }
        fn get_technique(&self, _: &str) -> vocara_store::Result<Technique> {
            unsupported()
        }
        fn load_technique(&self, _: &str) -> vocara_store::Result<Loaded<Technique>> {
            unsupported()
        }
        fn save_technique(&self, t: &Technique) -> vocara_store::Result<Technique> {
            Ok(t.clone())
        }
        fn save_technique_with_base(
            &self,
            _: &Technique,
            _: &BaseHashes,
        ) -> vocara_store::Result<Loaded<Technique>> {
            unsupported()
        }
        fn delete_technique(&self, _: &str) -> vocara_store::Result<()> {
            Ok(())
        }
    }
    let (mut vm, load) =
        TechniqueLibraryViewModel::new(Arc::new(Panicking), Arc::new(FakeMedia::default()));
    let (held, _) = load.expect("the first load").run();
    // A create lands meanwhile; a returned read would now be owed again,
    // but a panicked one never got past its await.
    let create = vm.execute_create("New");
    settle(&mut vm, create);
    assert!(vm.land(held).is_none());
    assert!(vm.load().error());
}
