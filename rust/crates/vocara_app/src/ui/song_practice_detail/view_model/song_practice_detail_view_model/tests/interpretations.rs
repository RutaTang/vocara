//! The interpretations controller wired into the view model (the view-model
//! cases of `song_practice_take_flow_test.dart` that need no recording, and
//! of `interpretations_controller_test.dart`'s save and selection wiring):
//! changes shown before their save lands, numbering, takes filed under the
//! interpretation they were made in, and a delete under the busy gate.

use std::sync::Arc;

use vocara_model::models::song_practice::SongPractice;

use super::*;

/// Two interpretations, take 1 under the first.
fn two_versions() -> SongPractice {
    let mut t1 = take("take1", "Take 1", "/pool/take1.wav");
    t1.version_id = Some("v1".into());
    let mut p = seed_practice(Some(vec![t1]));
    p.versions = vec![
        version("v1", "Interpretation 1"),
        version("v2", "Interpretation 2"),
    ];
    p.active_version_id = Some("v1".into());
    p
}

fn active(s: &Screen) -> Option<String> {
    s.vm.practice()
        .and_then(|p| p.active_version().map(|v| v.id.clone()))
}

#[test]
fn a_switch_is_shown_before_its_save_lands() {
    let (f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        ..Setup::default()
    });
    let gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&gate));
    let revision = s.vm.revision();
    s.act(|vm| vm.versions_do(|v, h| v.set_active("v2", h)));
    assert_eq!(
        active(&s).as_deref(),
        Some("v2"),
        "the save is still parked"
    );
    assert_ne!(s.vm.revision(), revision);
    f.repo.s().save_gate = None;
    gate.open();
    s.settle();
    assert_eq!(f.repo.stored().active_version_id.as_deref(), Some("v2"));
}

#[test]
fn a_new_one_is_shown_before_its_save_lands_numbered_past_the_names_taken() {
    let (f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        ..Setup::default()
    });
    let gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&gate));
    s.act(|vm| vm.versions_do(|v, h| v.create("", h)));
    let name =
        s.vm.practice()
            .and_then(|p| p.active_version().map(|v| v.name.clone()));
    assert_eq!(name.as_deref(), Some("Interpretation 3"));
    f.repo.s().save_gate = None;
    gate.open();
    s.settle();
}

#[test]
fn a_new_interpretation_is_not_named_after_one_that_exists() {
    // Counting them named a second "Interpretation 2" once the first of two
    // was deleted.
    let (_f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        ..Setup::default()
    });
    s.act(|vm| vm.versions_do(|v, h| v.delete("v1", h)));
    s.settle();
    let names = |s: &Screen| -> Vec<String> {
        s.practice()
            .versions
            .iter()
            .map(|v| v.name.clone())
            .collect()
    };
    assert_eq!(names(&s), ["Interpretation 2"]);
    assert_eq!(
        InterpretationsController::next_default_name(s.vm.practice()),
        "Interpretation 3"
    );
    s.act(|vm| vm.versions_do(|v, h| v.create("", h)));
    s.settle();
    assert_eq!(names(&s), ["Interpretation 2", "Interpretation 3"]);
}

#[test]
fn a_delete_drops_its_takes_files_after_the_save_and_hands_the_gate_back() {
    let (f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        ..Setup::default()
    });
    s.act(|vm| vm.versions_do(|v, h| v.delete("v1", h)));
    assert!(s.vm.busy(), "the delete holds the busy gate throughout");
    s.settle();
    assert!(!s.vm.busy());
    assert_eq!(f.events(), ["save", "delete:/pool/take1.wav"]);
    assert!(s.vm.track_by_id("take1").is_none());
}

#[test]
fn an_imported_take_lands_in_the_interpretation_it_was_dropped_into() {
    // A switch while it imports must not carry the take along.
    let gate = Gate::new();
    let (_f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_vocal_take("/src/new.wav"));
    s.act(|vm| vm.versions_do(|v, h| v.set_active("v2", h)));
    gate.open();
    s.await_reply(call);
    s.settle();
    let added = s
        .practice()
        .vocal_tracks
        .iter()
        .find(|t| t.id != "take1")
        .cloned()
        .expect("the import");
    assert_eq!(added.version_id.as_deref(), Some("v1"));
}

#[test]
fn an_imported_take_lands_in_the_interpretation_on_screen_when_the_id_does_not_resolve() {
    let mut p = two_versions();
    p.active_version_id = Some("gone".into());
    let (_f, mut s) = Fixture::open(Setup {
        practice: p,
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_vocal_take("/src/new.wav"));
    s.await_reply(call);
    let added = s
        .practice()
        .vocal_tracks
        .iter()
        .find(|t| t.id != "take1")
        .cloned()
        .expect("the import");
    assert_eq!(added.version_id.as_deref(), Some("v1"), "the one on screen");
}

#[test]
fn recs_empty_take_lands_in_the_interpretation_on_screen() {
    let mut p = two_versions();
    p.active_version_id = Some("gone".into());
    let (_f, mut s) = Fixture::open(Setup {
        practice: p,
        ..Setup::default()
    });
    s.act(|vm| vm.new_take());
    s.settle();
    let added = s
        .practice()
        .vocal_tracks
        .iter()
        .find(|t| t.id != "take1")
        .cloned()
        .expect("the empty take");
    assert_eq!(added.version_id.as_deref(), Some("v1"));
    assert!(added.clips.is_empty());
    assert_eq!(s.vm.selected_track_id(), Some(added.id.as_str()));
}

#[test]
fn a_delete_refused_while_an_import_runs_says_why() {
    let gate = Gate::new();
    let (_f, mut s) = Fixture::open(Setup {
        practice: two_versions(),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_vocal_take("/src/new.wav"));
    s.act(|vm| vm.versions_do(|v, h| v.delete("v1", h)));
    assert_eq!(s.toasts, ["Wait for the current edit to finish."]);
    gate.open();
    s.await_reply(call);
    s.settle();
    assert_eq!(s.practice().versions.len(), 2);
}
