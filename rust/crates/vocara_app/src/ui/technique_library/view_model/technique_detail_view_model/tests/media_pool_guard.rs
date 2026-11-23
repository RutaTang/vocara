//! Port of the view-model cases of `technique_media_pool_guard_test.dart`,
//! on the real drift repository and asset store (the service's own cases are
//! ported with `TechniqueMediaService`, the library's with the library view
//! model).

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path;
use vocara_model::util::id::new_id;
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::repositories::{TechniqueRepository, TechniqueRepositoryDrift};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, TechniqueDetailDeps, TechniqueDetailViewModel,
};

/// The store over a temp root the media service shares.
struct Store {
    tmp: tempfile::TempDir,
    assets: Arc<AssetStore>,
    repo: Arc<TechniqueRepositoryDrift>,
}

impl Store {
    fn new() -> Store {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(AppDatabase::open_in_memory().unwrap());
        let writer = Arc::new(SyncWriter::new(
            db.clone(),
            Arc::new(HlcClock::new(format!("{:0<26}", "NODE"))),
        ));
        let assets = Arc::new(AssetStore::new(db.clone(), tmp.path(), writer.clone()));
        let repo = Arc::new(TechniqueRepositoryDrift::new(db, writer, assets.clone()));
        Store { tmp, assets, repo }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn src(&self, name: &str, bytes: &[u8]) -> String {
        write_file(self.root(), name, bytes)
    }

    /// Saves technique [id] with one example [media_id] imported from a
    /// file of [bytes].
    fn save_with(&self, id: &str, media_id: &str, name: &str, bytes: &[u8]) {
        let now = DateTime::local_ymd(2026, 7, 1);
        let t = Technique {
            media: vec![example(media_id, &self.src(name, bytes))],
            ..Technique::new(id, format!("T {id}"), now, now)
        };
        self.repo.save_technique(&t).unwrap();
    }

    /// The page on technique [id] over [media], loaded.
    fn rig(&self, id: &str, media: FakeMedia) -> (Rig, Arc<FakeMedia>) {
        let media = Arc::new(media);
        let s = Setup::new(twang(id, vec![]));
        let deps = TechniqueDetailDeps {
            repository: self.repo.clone(),
            media: media.clone(),
            ..s.deps()
        };
        let (vm, next) = TechniqueDetailViewModel::new(deps);
        let mut rig = Rig::from_parts(vm, next);
        rig.settle();
        assert!(rig.vm.technique().is_some());
        (rig, media)
    }

    fn failing_deletes(&self) -> FakeMedia {
        let mut media = FakeMedia::real(self.root());
        media.fail_deletes = true;
        media
    }
}

#[test]
fn dispose_time_draft_cleanup_failure_is_contained() {
    // dispose() fire-and-forgets the staged drafts' deletes. A failure that
    // escaped would be a crash report from a best-effort cleanup: it is
    // contained and logged.
    let store = Store::new();
    let id = new_id();
    store.save_with(&id, &new_id(), "keep.wav", &[1, 2]);
    let (mut rig, media) = store.rig(&id, store.failing_deletes());
    let draft = store.src("draft.wav", &[3, 4]);
    rig.call_settled(|vm| vm.add_media(vec![draft]));
    assert_eq!(rig.vm.effective_media().len(), 2);

    rig.dispose();
    rig.settle();
    assert_eq!(media.deletes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_delete_of_a_staged_clip_does_not_break_cancel_half_way() {
    // One IO error stopped the loop with the draft already cleared: no
    // notify, the other files left behind, and the error thrown out of Cancel
    // before the screen could leave edit mode.
    let store = Store::new();
    let id = new_id();
    store.save_with(&id, &new_id(), "keep.wav", &[1, 2]);
    let (mut rig, media) = store.rig(&id, store.failing_deletes());
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let (a, b) = (store.src("a.wav", &[3]), store.src("b.wav", &[4]));
    rig.call_settled(|vm| vm.add_media(vec![a, b]));
    assert!(rig.vm.has_media_draft());
    let before = rig.vm.revision();

    rig.call_settled(|vm| vm.discard_media_draft());

    assert_eq!(rig.take_answers(), [Answer::DraftDiscarded(true)]);
    assert_eq!(media.deletes.load(Ordering::SeqCst), 2);
    assert!(!rig.vm.has_media_draft());
    assert!(rig.vm.revision() > before);
}

#[test]
fn a_technique_whose_files_could_not_all_be_deleted_is_still_deleted() {
    // Thrown, the cleanup's error escaped unhandled and the page stayed on
    // the deleted technique.
    let store = Store::new();
    let id = new_id();
    store.save_with(&id, &new_id(), "keep.wav", &[1, 2]);
    let (mut rig, _) = store.rig(&id, store.failing_deletes());
    rig.call_settled(|vm| vm.delete());
    assert_eq!(rig.take_answers(), [Answer::Deleted(true)]);
    assert!(store.repo.get_technique(&id).is_err());
}

#[test]
fn a_lone_clip_refused_for_its_size_says_so() {
    let store = Store::new();
    let id = new_id();
    store.save_with(&id, &new_id(), "keep.wav", &[1, 2]);
    let mut media = FakeMedia::real(store.root());
    media.refuse = Some("This file is too large to import.".into());
    let (mut rig, _) = store.rig(&id, media);
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let long = store.src("long.wav", &[1]);
    rig.call_settled(|vm| vm.add_media(vec![long]));
    assert_eq!(
        rig.vm.recording_notice(),
        Some("This file is too large to import.")
    );

    let (two, three) = (store.src("long2.wav", &[1]), store.src("long3.wav", &[1]));
    rig.call_settled(|vm| vm.add_media(vec![two, three]));
    assert_eq!(
        rig.vm.recording_notice(),
        Some("2 files couldn't be imported.")
    );
}

#[test]
fn removing_a_persisted_example_never_reclaims_shared_pool_bytes() {
    // The end-to-end removed-paths flow: two techniques whose examples dedup
    // to the SAME content hash (one pooled file, one asset row).
    let store = Store::new();
    let (t1, m1) = (new_id(), new_id());
    let t2 = new_id();
    store.save_with(&t1, &m1, "one.wav", &[7, 7, 7]);
    store.save_with(&t2, &new_id(), "two.wav", &[7, 7, 7]);

    let (mut rig, _) = store.rig(&t1, FakeMedia::real(store.root()));
    let pool_path = rig.vm.technique().unwrap().media[0].path.clone();
    let pool_root = store.root().join("assets").to_string_lossy().into_owned();
    assert!(dart_path::is_within(&pool_root, &pool_path));

    // Remove the persisted example and save: the removed-paths cleanup calls
    // delete_media(pool_path) — the guard must leave the pooled bytes alone.
    rig.call(|vm| vm.remove_media(&m1));
    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert!(rig.vm.technique().unwrap().media.is_empty());
    assert!(Path::new(&pool_path).exists());

    // The second technique referencing the same hash still resolves.
    let second = store.repo.get_technique(&t2).unwrap();
    assert_eq!(second.media[0].path, pool_path);
    assert!(Path::new(&pool_path).exists());

    // Pool bytes are reclaimed ONLY by the hash-refcount GC: with t2 still
    // referencing the hash, a sweep keeps the file in place.
    assert_eq!(store.assets.sweep_unreferenced(Duration::ZERO).unwrap(), 0);
    assert!(Path::new(&pool_path).exists());

    // ...and once nothing references the hash, that same sweep reclaims it.
    store.repo.delete_technique(&t2).unwrap();
    assert_eq!(store.assets.sweep_unreferenced(Duration::ZERO).unwrap(), 1);
    assert!(!Path::new(&pool_path).exists());
}
