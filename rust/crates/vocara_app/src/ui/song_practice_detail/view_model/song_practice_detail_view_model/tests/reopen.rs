//! Reopening a practice while what the last screen on it left is still
//! landing (the edit cases of `song_practice_take_flow_test.dart`'s reopen
//! group), and the detail side of `home_reopen_test.dart` (a practice that
//! cannot be loaded still has a way back: the load ends, with nothing to
//! show).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

/// The first screen renames take 1 with its save parked, then leaves.
fn rename_and_leave(f: &Fixture, first: &mut Screen) -> Arc<Gate> {
    let gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&gate));
    first.act(|vm| vm.rename_track("single", "Lead"));
    first.pump_until(|_| f.repo.s().in_flight > 0);
    first.vm.dispose();
    f.repo.s().save_gate = None;
    gate
}

#[test]
fn a_practice_reopened_while_an_edit_is_still_saving_shows_the_edit() {
    // An edit's save outlives the screen the same way a take's finalize does
    // (rename, then Back while the disk is slow): the reopened screen read
    // the practice from before the edit, and its next save then merged
    // against that stale base.
    let (f, mut first) = Fixture::open(Setup::default());
    let gate = rename_and_leave(&f, &mut first);
    let mut reopened = f.screen(Parts::default());
    thread::sleep(Duration::from_millis(20));
    gate.open();
    first.settle();
    reopened.settle();
    assert_eq!(
        reopened.track("single").display_name,
        "Lead",
        "the edit it was reopened on"
    );
    assert!(!reopened.vm.catching_up());
}

#[test]
fn a_reopen_behind_a_stuck_save_opens_at_once_and_shows_the_edit_once_it_lands() {
    // Waiting out every landing save held each reopen behind a wedged one for
    // the finalizer's whole budget, only to load the old state anyway.
    let (f, mut first) = Fixture::open(Setup::default());
    let gate = rename_and_leave(&f, &mut first);
    let started = Instant::now();
    let mut reopened = f.screen(Parts::default());
    reopened.pump_until(|s| s.vm.practice().is_some());
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "opened at once"
    );
    assert_eq!(
        reopened.track("single").display_name,
        "Take 1",
        "the save has not landed yet"
    );
    // An edit made on this read would merge against a base from before the
    // landing save, and lose either itself or the rename.
    assert!(reopened.vm.catching_up(), "takes no edits yet");
    assert!(reopened.vm.busy(), "the gate refuses guarded edits");

    gate.open();
    first.settle();
    reopened.settle();
    assert_eq!(
        reopened.track("single").display_name,
        "Lead",
        "the landed edit shows without a reopen"
    );
    assert!(!reopened.vm.catching_up());
    assert!(!reopened.vm.busy(), "edits open once caught up");
}

#[test]
fn a_reopen_catches_up_on_a_save_that_lands_after_the_finalizer_budget() {
    // The budget caps a QUIT's wait; capped here too, the screen re-read
    // before the save landed and never again, showing the old name for good
    // while its next save fought the landed one.
    let (f, mut first) = Fixture::open(Setup {
        finalizer: Some(Arc::new(
            RecordingFinalizer::for_testing().with_per_finalizer_budget(Duration::from_millis(50)),
        )),
        ..Setup::default()
    });
    let gate = rename_and_leave(&f, &mut first);
    let mut reopened = f.screen(Parts::default());
    reopened.pump_until(|s| s.vm.practice().is_some());
    // Well past the budget, and past the reopen's grace.
    let until = Instant::now() + Duration::from_millis(400);
    reopened.pump_until(|_| Instant::now() >= until);
    assert!(reopened.vm.catching_up(), "still landing");
    gate.open();
    first.settle();
    reopened.settle();
    assert_eq!(reopened.track("single").display_name, "Lead");
}

#[test]
fn a_practice_that_cannot_be_loaded_ends_its_load_with_nothing_to_show() {
    // home_reopen_test: the screen then shows "Could not load this practice."
    // under a way back, rather than a spinner for good.
    let events = Arc::new(Mutex::new(Vec::new()));
    let repo = FakeRepo::new(seed_practice(None), Arc::clone(&events));
    repo.s().fail_next_load = true;
    let f = Fixture {
        events,
        repo,
        files: FakeFiles::new(Arc::new(Mutex::new(Vec::new()))),
        edits: Arc::new(FakeEdits::default()),
        engine: Arc::new(FakeEngine::default()),
        techs: Arc::new(FakeTechRepo::default()),
        finalizer: Arc::new(RecordingFinalizer::for_testing()),
    };
    let mut s = f.screen(Parts::default());
    assert!(s.vm.load_running());
    s.settle();
    assert!(!s.vm.load_running());
    assert!(s.vm.practice().is_none());
    assert!(s.vm.load_error().is_some());
}

#[test]
fn a_remote_change_landing_during_the_load_is_loaded_after_it() {
    // list_remote_refresh_test (detail side): a change applied while the
    // first read was out is owed once the load is done.
    let gate = Gate::new();
    let (f, mut s) = Fixture::open(Setup {
        first_load: Some(Arc::clone(&gate)),
        ..Setup::default()
    });
    s.pump_until(|_| f.repo.s().stale_load_gate.is_none());
    let mut stored = f.repo.stored();
    stored.title = "Renamed elsewhere".into();
    f.repo.s().stored = Some(stored);
    s.act(|vm| {
        vm.on_remote_applied(&vocara_store::sync::sync_engine::RemoteApplied {
            rows: Vec::new(),
            practice_ids: ["p1".to_string()].into_iter().collect(),
            technique_ids: Default::default(),
        })
    });
    gate.open();
    s.settle();
    assert_eq!(s.practice().title, "Renamed elsewhere");
}
