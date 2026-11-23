//! Port of `technique_pending_start_test.dart`.
//!
//! A recording start pends behind the mic permission prompt and the mic open
//! (up to the ready timeout). The recorder's starting flag flips at the tap,
//! but the screen only sees what the view model notifies: these tests pin
//! that the pending window is visible to it, and that nothing the take
//! depends on can change in it.
//!
//! Left to the detail screen's port: Save's button disabled while an example
//! start pends (here: `saving_example` holds then, which the screen's
//! save-blocked reads), and the record row saying "Starting the microphone…"
//! rather than "Saving example…" (here: `starting` holds then).

use vocara_services::mic_recording_controller::{MicRecordingFailure, MicRecordingStart};

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn setup() -> Setup {
    Setup::new(twang(ID, vec![example("M0", "/tmp/example-0.wav")]))
}

#[test]
fn a_pending_practice_start_is_announced_and_the_card_holding_its_stop_cannot_collapse_under_it() {
    // Regression: the starting flag flipped without a notify, so during the
    // permission prompt the card still looked collapsible — collapsing it
    // armed the take with its Stop and meter hidden while the mic was hot.
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::StartRecording);
    let before = rig.vm.revision();

    rig.call(|vm| vm.start_practice());
    assert!(s.recorder.st().starting);
    assert_eq!(
        rig.vm.revision() - before,
        1,
        "the screen must see the pending start"
    );

    rig.call(|vm| vm.toggle_example_expanded("M0"));
    assert_eq!(
        rig.vm.expanded_example_id().as_deref(),
        Some("M0"),
        "the take arms into it"
    );

    rig.release(JobKind::StartRecording);
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    assert!(rig.vm.is_recording_practice());
    assert_eq!(rig.vm.expanded_example_id().as_deref(), Some("M0"));
}

#[test]
fn a_start_that_fails_after_pending_is_announced_too() {
    // The failure path cleared the starting flag silently: whatever the
    // pending start had disabled stayed disabled until something else
    // notified.
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    let before = rig.vm.revision();

    s.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Failed(
        MicRecordingFailure::PermissionDenied,
    ));
    rig.release(JobKind::StartRecording);
    assert!(matches!(rig.last_answer(), Answer::PracticeStart(Some(_))));
    assert!(!s.recorder.st().starting);
    assert!(rig.vm.revision() > before);
}

#[test]
fn save_is_blocked_while_an_example_recording_start_is_pending() {
    // Regression: Save looked live through the permission prompt, and a tap
    // there toasted "Couldn't save" (the save refuses a pending start). The
    // screen's save-blocked is `saving || saving_example || importing_media`.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let blocked =
        |rig: &Rig| rig.vm.saving() || rig.vm.saving_example() || rig.vm.importing_media();
    assert!(!blocked(&rig));

    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_example_recording());
    assert!(blocked(&rig));

    s.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Invalidated);
    rig.release(JobKind::StartRecording);
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    assert!(!blocked(&rig));
}

#[test]
fn a_pending_example_start_reads_as_starting() {
    // Regression: the record row took its busy state from saving_example,
    // which also covers a pending start, so through the permission prompt
    // and the mic open it said "Saving example…" before anything was
    // recorded. The row reads `starting` for its "Starting the microphone…".
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_example_recording());
    assert!(rig.vm.starting());
    assert!(!rig.vm.is_recording_example());

    s.recorder.st().mode = StartMode::Resolve(MicRecordingStart::Invalidated);
    rig.release(JobKind::StartRecording);
    assert!(!rig.vm.starting());
}
