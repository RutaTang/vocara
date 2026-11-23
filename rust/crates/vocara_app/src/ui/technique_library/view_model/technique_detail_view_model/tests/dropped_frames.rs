//! Port of `technique_recording_dropped_frames_test.dart`: a take whose
//! recording ring overflowed finalizes cleanly but has silent holes; it is
//! kept, and never presented as gapless.

use std::path::Path;

use vocara_model::models::technique::Technique;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn setup() -> Setup {
    Setup::new(twang(ID, vec![]))
}

fn notice(rig: &Rig) -> String {
    rig.vm.recording_notice().unwrap_or_default().to_string()
}

#[test]
fn a_clean_practice_stop_with_ring_overflow_surfaces_the_dropped_frames_notice() {
    // Regression: stop() reports dropped_frames but the view model used to
    // read only `clean` — a time-compressed take (ring overflow while the
    // writer stalled) was presented as gapless.
    let s = setup();
    let mut rig = s.loaded();
    s.recorder.st().dropped_frames = 4800; // 100 ms at the 48 kHz ring
    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    assert!(rig.vm.is_recording_practice());

    rig.call_settled(|vm| vm.stop_practice());
    assert!(notice(&rig).contains("dropped about 100 ms"));
    assert!(notice(&rig).contains("small gaps"));
}

#[test]
fn a_clean_practice_stop_without_overflow_surfaces_no_notice() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    rig.call_settled(|vm| vm.stop_practice());
    assert_eq!(rig.vm.recording_notice(), None);
}

#[test]
fn a_clean_example_stop_with_ring_overflow_surfaces_the_notice_and_keeps_the_clip() {
    let s = setup();
    let mut rig = s.loaded();
    s.recorder.st().dropped_frames = 9600; // 200 ms at the 48 kHz ring
    rig.vm.begin_media_draft();
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    assert!(rig.vm.is_recording_example());

    rig.call_settled(|vm| vm.stop_example_recording());
    assert!(notice(&rig).contains("dropped about 200 ms"));
    assert!(notice(&rig).contains("small gaps"));
    // Most of the take is fine, so it is staged as usual — the notice only
    // stops it from masquerading as gapless.
    assert_eq!(rig.vm.effective_media().len(), 1);
}

#[test]
fn a_clean_example_stop_without_overflow_surfaces_no_notice() {
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.call_settled(|vm| vm.start_example_recording());
    rig.call_settled(|vm| vm.stop_example_recording());
    assert_eq!(rig.vm.recording_notice(), None);
    assert_eq!(rig.vm.effective_media().len(), 1);
}

#[test]
fn dispose_during_an_airborne_save_never_deletes_the_staged_files_the_commit_is_persisting() {
    // The route can pop while save() waits on the repository. dispose() used
    // to delete every staged-addition file unconditionally — the commit then
    // landed rows pointing at nothing.
    let s = setup();
    let mut rig = s.loaded();

    // Stage a REAL recorded example file into the draft.
    rig.vm.begin_media_draft();
    rig.call_settled(|vm| vm.start_example_recording());
    rig.call_settled(|vm| vm.stop_example_recording());
    let staged = rig.vm.effective_media()[0].path.clone();
    assert!(Path::new(&staged).exists());

    // Save goes airborne, then the screen is torn down mid-wait.
    rig.hold(JobKind::Commit);
    let kept = Technique {
        summary: "kept".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.take_answers();
    rig.call(|vm| vm.save(kept));
    assert!(rig.vm.saving());
    rig.dispose();
    // Let any (wrong) fire-and-forget deletions run before asserting.
    rig.settle();
    assert!(
        Path::new(&staged).exists(),
        "dispose must not delete files the airborne commit references"
    );

    rig.release(JobKind::Commit);
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert_eq!(s.repo.technique().media[0].path, staged);
    assert!(
        Path::new(&staged).exists(),
        "the committed row must keep pointing at a real file"
    );
}
