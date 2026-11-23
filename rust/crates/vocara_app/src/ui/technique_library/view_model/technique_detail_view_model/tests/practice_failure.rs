//! Port of `technique_practice_failure_test.dart`'s view-model assertions.
//!
//! Left to the detail screen's port (the analysis section): the failed
//! take's panel saying "too long", the Reference / Your practice switch
//! showing it again, and the practice button reading "Starting the
//! microphone…" (here: `starting_practice`) instead of "Record practice".

use std::time::Duration;

use vocara_services::engine::media::MediaError;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, JobKind, TechniqueAnalysisSubject,
};

/// The reference analyzes fine; the practice take (> 10 min) trips the cap.
fn setup() -> Setup {
    let s = Setup::new(twang("T1", vec![])).analysis(Outcome::Fresh);
    let root = s.root().to_path_buf();
    s.repo.st().technique.as_mut().unwrap().media =
        vec![example("E1", &root.join("E1.wav").to_string_lossy())];
    s.analysis.st().refuse_paths = Some((
        "practice_take".into(),
        MediaError::TooLong {
            max_duration: Duration::from_secs(10 * 60),
        },
    ));
    s.recorder.st().mode = StartMode::In(root);
    s
}

#[test]
fn a_failed_practice_analysis_is_shown_on_its_own_panel() {
    // A take the analysis refuses (here: past the 10-minute cap) used to snap
    // the panel back to the reference, with the practice segment disabled:
    // the cause was kept but shown nowhere.
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    rig.vm.set_analysis_dim(8); // Range
    assert!(rig.vm.can_record_practice(), "Record practice");

    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    rig.call_settled(|vm| vm.stop_practice());

    // The failed take stays the subject, and its panel says why.
    assert!(rig.vm.practice_analysis_failed());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert!(rig
        .vm
        .practice_analysis_failure_message()
        .unwrap()
        .contains("too long"));

    // The user can leave it and come back to read it again.
    rig.vm
        .show_analysis_subject(TechniqueAnalysisSubject::Reference);
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
    assert!(!rig.vm.reference_analysis_failed());
    rig.vm
        .show_analysis_subject(TechniqueAnalysisSubject::Practice);
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert!(rig
        .vm
        .practice_analysis_failure_message()
        .unwrap()
        .contains("too long"));
}

#[test]
fn the_practice_button_says_the_mic_is_starting_and_takes_no_taps() {
    // It said "Record practice" through the permission prompt and the mic's
    // opening, and every tap did nothing, unlike the example's record row.
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    assert!(rig.vm.starting_practice());
    assert!(!rig.vm.can_record_practice(), "a tap does nothing");
    rig.call(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    assert_eq!(rig.queued(JobKind::StartRecording), 1);

    rig.release(JobKind::StartRecording);
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    assert!(!rig.vm.starting_practice());
    rig.call_settled(|vm| vm.stop_practice_skipping_analysis());
    assert!(!rig.vm.is_recording_practice());
}
