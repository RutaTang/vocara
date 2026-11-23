//! Port of the view-model cases of `technique_example_identity_test.dart`
//! (the repository and model cases are ported with the store), end to end
//! on the real drift repository.
//!
//! Names: an imported clip is copied into the sandbox under a generated id
//! and pooled under its content hash, and an uncaptioned example falls back
//! to its file's basename — so a dropped "My Twang Demo.wav" read as
//! "01M5Q7…T68.wav" while staged and as a 64-hex hash once saved.
//!
//! Identity: two examples with identical bytes resolve to the SAME pooled
//! file. Selection and expansion were keyed by path, so both cards opened
//! with the analysis panel and a playhead-linked player, and the second
//! example could never be selected on its own. Left to the detail screen's
//! port: only the selected example's card shows the analysis.

use std::collections::HashSet;
use std::sync::Arc;

use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::db::{AppDatabase, AssetStore};
use vocara_store::repositories::{TechniqueRepository, TechniqueRepositoryDrift};
use vocara_store::sync::hlc::HlcClock;
use vocara_store::sync::sync_writer::SyncWriter;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, TechniqueDetailDeps, TechniqueDetailViewModel,
};

struct Store {
    tmp: tempfile::TempDir,
    repo: Arc<TechniqueRepositoryDrift>,
    technique_id: String,
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
        let repo = Arc::new(TechniqueRepositoryDrift::new(db, writer, assets));
        let technique_id = new_id();
        let now = DateTime::local_ymd(2026, 7, 1);
        repo.save_technique(&Technique::new(technique_id.clone(), "Twang", now, now))
            .unwrap();
        Store {
            tmp,
            repo,
            technique_id,
        }
    }

    fn source(&self, name: &str, bytes: &[u8]) -> String {
        write_file(&self.tmp.path().join("src"), name, bytes)
    }

    /// The page over the store, loaded; the real media service shares the
    /// store's root.
    fn rig(&self) -> Rig {
        let s = Setup::new(twang(&self.technique_id, vec![]));
        let deps = TechniqueDetailDeps {
            repository: self.repo.clone(),
            media: Arc::new(FakeMedia::real(self.tmp.path())),
            ..s.deps()
        };
        let (vm, next) = TechniqueDetailViewModel::new(deps);
        let mut rig = Rig::from_parts(vm, next);
        rig.settle();
        assert!(rig.vm.technique().is_some());
        rig
    }
}

fn labels(rig: &Rig) -> Vec<String> {
    rig.vm
        .effective_media()
        .iter()
        .map(TechniqueMedia::display_label)
        .collect()
}

fn save(rig: &mut Rig) -> Answer {
    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    rig.last_answer()
}

#[test]
fn a_dropped_clip_keeps_its_file_name_through_the_save() {
    let store = Store::new();
    let mut rig = store.rig();
    rig.vm.begin_edit();
    rig.vm.begin_media_draft();
    let dropped = store.source("My Twang Demo.wav", &[1, 2, 3]);
    rig.call_settled(|vm| vm.add_media(vec![dropped]));

    assert_eq!(labels(&rig), ["My Twang Demo"]);
    assert_eq!(save(&mut rig), Answer::Saved(true));
    assert_eq!(labels(&rig), ["My Twang Demo"]);

    let stored = store.repo.get_technique(&store.technique_id).unwrap();
    assert_eq!(stored.media[0].label, "My Twang Demo");
}

#[test]
fn recorded_examples_are_captioned_recording_n_counting_up_across_edit_sessions() {
    let store = Store::new();
    let mut rig = store.rig();
    let record_one = |rig: &mut Rig| {
        rig.call_settled(|vm| vm.start_example_recording());
        assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
        rig.call_settled(|vm| vm.stop_example_recording());
        assert!(matches!(
            rig.take_answers()[..],
            [Answer::ExampleStop(Some(_))]
        ));
    };

    rig.vm.begin_edit();
    rig.vm.begin_media_draft();
    record_one(&mut rig);
    record_one(&mut rig);
    assert_eq!(save(&mut rig), Answer::Saved(true));
    rig.call_settled(|vm| vm.end_edit());

    rig.vm.begin_edit();
    rig.vm.begin_media_draft();
    record_one(&mut rig);
    assert_eq!(labels(&rig), ["Recording 1", "Recording 2", "Recording 3"]);
}

#[test]
fn two_byte_identical_examples_select_independently() {
    let store = Store::new();
    let ids = [new_id(), new_id()];
    let current = store.repo.get_technique(&store.technique_id).unwrap();
    let media = ids
        .iter()
        .enumerate()
        .map(|(i, id)| TechniqueMedia {
            label: format!("Take {i}"),
            ..example(id, &store.source(&format!("take {i}.wav"), &[9, 9, 9]))
        })
        .collect();
    store
        .repo
        .save_technique(&Technique { media, ..current })
        .unwrap();
    let mut rig = store.rig();
    // The pool dedups identical bytes: one file, two examples.
    let paths: HashSet<String> = rig
        .vm
        .technique()
        .unwrap()
        .media
        .iter()
        .map(|m| m.path.clone())
        .collect();
    assert_eq!(paths.len(), 1);

    assert_eq!(rig.vm.reference_media().unwrap().id, ids[0]);
    assert_eq!(
        rig.vm.expanded_example_id().as_deref(),
        Some(ids[0].as_str())
    );

    // Expanding the second card selects IT, not the first sharing its file.
    rig.call_settled(|vm| vm.toggle_example_expanded(&ids[1]));
    assert_eq!(rig.vm.reference_media().unwrap().id, ids[1]);
    assert_eq!(
        rig.vm.expanded_example_id().as_deref(),
        Some(ids[1].as_str())
    );
}
