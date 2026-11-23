//! Port of `technique_sibling_paths_test.dart`: sibling paths of the
//! technique page kept in line with each other — a quit's example commit
//! held like a Save, a remote change owed to a failed start or a failed
//! first load, and the practice start's pending state.

use std::path::Path;

use vocara_model::models::technique::Technique;
use vocara_services::mic_recording_controller::{MicRecordingFailure, MicRecordingStart};

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn setup() -> Setup {
    let s = Setup::new(twang(ID, vec![]));
    // Like resolveOrImport: every media file must still be readable.
    s.repo.st().require_files = true;
    s
}

#[test]
fn a_failed_practice_start_replays_a_remote_change_it_held_off() {
    // The reload owed was never run: the page kept the old text, and an edit
    // seeded from it saved over the change.
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    assert!(rig.vm.starting_practice());
    let before = s.repo.st().loads;
    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));
    assert_eq!(s.repo.st().loads, before, "held while the start pends");

    s.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Failed(
        MicRecordingFailure::PermissionDenied,
    ));
    rig.release(JobKind::StartRecording);
    assert!(matches!(
        rig.take_answers()[..],
        [Answer::PracticeStart(Some(_))]
    ));
    assert_eq!(s.repo.st().loads, before + 1);
    assert!(!rig.vm.starting_practice());
}

#[test]
fn a_quit_committing_an_example_is_held_like_a_save() {
    // Neither Save nor Cancel was refused meanwhile: Save ran a second commit
    // on the same base, and Cancel deleted the clip being committed.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    rig.hold(JobKind::Commit);
    rig.call_settled(|vm| vm.quit_finalize());
    assert_eq!(
        rig.queued(JobKind::Commit),
        1,
        "the quit-time commit is in flight"
    );
    assert!(rig.vm.saving());
    let staged = rig.vm.effective_media().pop().unwrap();

    let typed = Technique {
        summary: "typed".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call(|vm| vm.save(typed));
    // Held off, and says so: the screen keeps the edit open on the draft.
    rig.call(|vm| vm.discard_media_draft());
    assert_eq!(
        rig.take_answers(),
        [Answer::Saved(false), Answer::DraftDiscarded(false)]
    );
    assert_eq!(rig.queued(JobKind::Commit), 1, "no second commit");
    assert!(Path::new(&staged.path).exists());

    rig.release(JobKind::Commit);
    assert!(!rig.vm.saving());
    assert_eq!(s.repo.technique().media.len(), 1);
}

#[test]
fn a_remote_change_after_a_failed_first_load_reads_again() {
    // It was owed to the load, which had already ended: the page stayed on
    // "could not be loaded" through the change.
    let s = setup();
    s.repo.st().load_failures = 1;
    let mut rig = s.rig();
    rig.settle();
    assert!(!rig.vm.load().running());
    assert!(rig.vm.technique().is_none());
    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));
    assert_eq!(rig.vm.technique().unwrap().name, "Twang");
}

#[test]
fn an_example_added_in_this_edit_reads_as_unsaved() {
    // Its Remove said "Cancel the edit to keep it", but it is deleted at
    // once.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.start_example_recording());
    rig.call_settled(|vm| vm.stop_example_recording());
    let added = rig.vm.effective_media().pop().unwrap();
    assert!(rig.vm.is_unsaved_media(&added.id));
    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    assert_eq!(rig.last_answer(), Answer::Saved(true));
    let saved = rig.vm.technique().unwrap().media[0].id.clone();
    assert!(!rig.vm.is_unsaved_media(&saved));
}

#[test]
fn a_practice_start_that_throws_is_said_as_a_failure_the_button_let_go() {
    // Nothing was told after the throw: the button stayed on "Starting the
    // microphone…", disabled, and the error went unhandled.
    let s = setup();
    let mut rig = s.loaded();
    s.recorder.st().mode = StartMode::Throw;
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    let before = rig.vm.revision();
    rig.release(JobKind::StartRecording);
    assert_eq!(
        rig.take_answers(),
        [Answer::PracticeStart(Some(
            "Couldn't create the recording file. Check your free disk space.".into()
        ))]
    );
    assert!(rig.vm.revision() > before, "the screen hears it");
    assert!(!rig.vm.starting_practice());
    assert!(rig.vm.can_record_practice());
}

#[test]
fn a_cancelled_import_copies_no_more_of_its_files() {
    // Cancelled during the first of four, all four were still copied (each
    // up to gigabytes), only to be deleted.
    let s = setup().media(FakeMedia::pool());
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.hold(JobKind::Import);
    rig.call(|vm| {
        vm.add_media(vec![
            "/a/1.wav".into(),
            "/a/2.wav".into(),
            "/a/3.wav".into(),
            "/a/4.wav".into(),
        ])
    });
    assert_eq!(rig.queued(JobKind::Import), 1, "the first copy is out");
    rig.call_settled(|vm| vm.discard_media_draft());
    rig.release(JobKind::Import);
    assert_eq!(s.media.imports.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(rig.vm.effective_media().is_empty());
}
