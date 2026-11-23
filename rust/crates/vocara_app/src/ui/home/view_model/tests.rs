//! The home view model: listing, create, delete, and the reload guards
//! (`list_remote_refresh_test.dart`'s and `list_read_failure_create_test.dart`'s
//! Home cases, over fakes as they were; the basics over the real repository).

use std::sync::Arc;

use super::*;
use crate::app::library::Library;
use crate::ui::home::testing::{remote_applied, summary, FakePractices};

fn titles(vm: &HomeViewModel) -> Vec<&str> {
    vm.projects().iter().map(|p| p.title.as_str()).collect()
}

fn ids(vm: &HomeViewModel) -> Vec<&str> {
    vm.projects().iter().map(|p| p.id.as_str()).collect()
}

/// Runs `effect` now and lands it, and so on for every load it owes, until
/// nothing more runs.
fn settle(vm: &mut HomeViewModel, effect: Option<Effect>) {
    let mut next = effect;
    while let Some(effect) = next {
        next = vm.land(effect.run());
    }
}

/// A view model whose first read has landed.
fn loaded(repo: &Arc<FakePractices>) -> HomeViewModel {
    let (mut vm, first) = HomeViewModel::new(repo.clone());
    settle(&mut vm, Some(first));
    vm
}

fn remote_change() -> RemoteApplied {
    remote_applied(&[], &["a"], &[])
}

// ---------------------------------------------------------------- basics

#[test]
fn it_lists_creates_and_deletes_through_the_real_repository() {
    let library = Library::in_memory();
    let repo = library.song_repo.clone();
    repo.create_practice("First").expect("create");
    repo.create_practice("Second").expect("create");

    let (mut vm, first) = HomeViewModel::new(repo.clone());
    assert!(vm.load().running(), "the first read starts on creation");
    assert!(vm.projects().is_empty());
    settle(&mut vm, Some(first));
    assert!(vm.load().completed());
    assert_eq!(titles(&vm), ["Second", "First"]);

    // A create lands its card on top at once.
    let create = vm.execute_create("Third".into());
    assert!(vm.create_project().running());
    settle(&mut vm, create);
    let Some(Ok(created)) = vm.create_project().result() else {
        panic!("created")
    };
    assert_eq!(created.title, "Third");
    assert_eq!(titles(&vm), ["Third", "Second", "First"]);

    // A delete takes its card off.
    let second = vm.projects()[1].id.clone();
    let delete = vm.execute_delete(second);
    settle(&mut vm, delete);
    assert!(vm.delete_project().completed());
    assert_eq!(titles(&vm), ["Third", "First"]);

    // And a fresh read agrees.
    let reload = vm.refresh();
    assert!(reload.is_some());
    settle(&mut vm, reload);
    assert_eq!(titles(&vm), ["Third", "First"]);
}

#[test]
fn a_command_drops_a_call_made_while_it_runs() {
    let repo = FakePractices::with(vec![summary("a", "A")]);
    let (mut vm, first) = HomeViewModel::new(repo.clone());
    assert!(vm.execute_load().is_none(), "the first read still runs");
    let delete = vm.execute_delete("a".into());
    assert!(delete.is_some());
    assert!(vm.execute_delete("a".into()).is_none());
    let create = vm.execute_create("New".into());
    assert!(create.is_some());
    assert!(vm.execute_create("Other".into()).is_none());
    settle(&mut vm, Some(first));
    settle(&mut vm, delete);
    settle(&mut vm, create);
    assert_eq!(repo.state().deletes, 1);
    assert_eq!(repo.state().creates, 1);
}

#[test]
fn a_failed_create_or_delete_keeps_the_list_and_reports_its_error() {
    let repo = FakePractices::with(vec![summary("a", "A")]);
    let mut vm = loaded(&repo);
    repo.state().uncreatable = true;
    repo.state().undeletable = true;
    let create = vm.execute_create("New".into());
    settle(&mut vm, create);
    assert!(vm.create_project().error());
    let delete = vm.execute_delete("a".into());
    settle(&mut vm, delete);
    assert!(vm.delete_project().error());
    assert_eq!(ids(&vm), ["a"]);
    assert!(vm.clear_create_result().is_none());
    assert!(vm.clear_delete_result().is_none());
    assert!(vm.create_project().result().is_none());
    assert!(vm.delete_project().result().is_none());
}

#[test]
fn nothing_more_starts_once_disposed() {
    let repo = FakePractices::with(vec![summary("a", "A")]);
    let mut vm = loaded(&repo);
    vm.dispose();
    assert!(vm.refresh().is_none());
    assert!(vm.on_remote_applied(&remote_change()).is_none());
}

// ------------------------------------------- list_remote_refresh_test (Home)
//
// Home keeps its list fresh as the library does. It had none of the
// library's guards: a remote rename landing during a load was dropped, a
// read taken before a delete brought the card back, and the reload on
// return was dropped behind a running load.

fn held_repo() -> (Arc<FakePractices>, HomeViewModel) {
    let repo = FakePractices::with(vec![summary("a", "Old"), summary("b", "B")]);
    let vm = loaded(&repo);
    (repo, vm)
}

#[test]
fn a_remote_change_landing_during_a_load_is_loaded_after_it() {
    let (repo, mut vm) = held_repo();
    let held = vm.refresh().expect("a load");
    assert!(vm.load().running());
    let read = held.run(); // read now, returned when let go
    repo.state().practices = vec![summary("a", "New"), summary("b", "B")];
    assert!(
        vm.on_remote_applied(&remote_change()).is_none(),
        "held while the load runs"
    );
    let owed = vm.land(read);
    assert!(owed.is_some(), "the remote change is loaded after it");
    settle(&mut vm, owed);
    assert_eq!(titles(&vm), ["New", "B"]);
}

#[test]
fn a_load_read_before_a_delete_does_not_bring_the_card_back() {
    let (_repo, mut vm) = held_repo();
    let held = vm.on_remote_applied(&remote_change()).expect("a load");
    assert!(vm.load().running());
    let read = held.run(); // still holds "a"
    let delete = vm.execute_delete("a".into());
    settle(&mut vm, delete);
    assert_eq!(ids(&vm), ["b"]);
    // The older read lands.
    let owed = vm.land(read);
    assert_eq!(ids(&vm), ["b"]);
    settle(&mut vm, owed);
    assert_eq!(ids(&vm), ["b"]);
}

#[test]
fn a_return_from_a_practice_while_a_load_runs_loads_again_after_it() {
    let (repo, mut vm) = held_repo();
    let held = vm.on_remote_applied(&remote_change()).expect("a load");
    let read = held.run();
    repo.state().practices = vec![summary("a", "Renamed there"), summary("b", "B")];
    assert!(vm.refresh().is_none(), "dropped as re-entry, but owed");
    let owed = vm.land(read);
    settle(&mut vm, owed);
    assert_eq!(titles(&vm), ["Renamed there", "B"]);
}

// Regression: Home filtered on the practices TABLE, and a take recorded on
// another device ships only tracks/clips/notes rows — the card's take and
// note counts never refreshed.
#[test]
fn home_reloads_when_a_remote_page_carries_only_a_practices_takes_and_notes() {
    let repo = FakePractices::with(Vec::new());
    let mut vm = loaded(&repo);
    assert_eq!(repo.state().summary_reads, 1);

    let reload = vm.on_remote_applied(&remote_applied(
        &[("tracks", "TR1"), ("practice_notes", "N1")],
        &["P1"],
        &[],
    ));
    settle(&mut vm, reload);
    assert_eq!(repo.state().summary_reads, 2);

    // A page touching no practice at all still leaves Home alone.
    let reload = vm.on_remote_applied(&remote_applied(&[("technique_media", "M1")], &[], &["T1"]));
    assert!(reload.is_none());
    assert_eq!(repo.state().summary_reads, 2);
}

// --------------------------------- list_read_failure_create_test (Home)
//
// Home shows its read error only over an empty list. A create after a failed
// read added its card to the list never read: the error gave way to that one
// card, every later read failed too, and the rest of the library looked lost.

fn unreadable() -> Arc<FakePractices> {
    let repo = FakePractices::with(vec![PracticeSummary {
        updated_at: vocara_model::util::dart_date_time::DateTime::local_ymd(2026, 6, 20),
        ..summary("P1", "Kept")
    }]);
    repo.state().unreadable = true;
    repo
}

#[test]
fn home_keeps_its_read_error_over_a_practice_created_after_it() {
    let repo = unreadable();
    let mut vm = loaded(&repo);
    assert!(vm.load().error());

    let create = vm.execute_create("New".into());
    settle(&mut vm, create);
    assert!(matches!(vm.create_project().result(), Some(Ok(_))));
    assert!(vm.projects().is_empty(), "the error still shows");

    repo.state().unreadable = false;
    let reload = vm.refresh();
    settle(&mut vm, reload);
    assert_eq!(titles(&vm), ["New", "Kept"]);
}

// The guard also let a create through while the first read still ran (its
// error not yet known): that read and the one the create owed then failed,
// and the one new card stood in for the whole library.
#[test]
fn home_shows_no_lone_new_card_when_the_read_it_overtook_fails() {
    let repo = unreadable();
    let (mut vm, first) = HomeViewModel::new(repo.clone());
    assert!(vm.load().running());
    let create = vm.execute_create("New".into());
    settle(&mut vm, create);
    settle(&mut vm, Some(first));
    assert!(vm.load().error());
    assert!(vm.projects().is_empty(), "the error still shows");
    assert_eq!(repo.state().summary_reads, 2, "the create owed a read");
}
