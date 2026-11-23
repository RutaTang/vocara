//! Port of `technique_practice_take_generation_test.dart`: a practice take
//! is judged against the reference example it ARMED under; a take whose
//! reference changed is discarded, and one whose reference held is kept.

use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_services::engine::media::MediaError;

use super::support::*;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, JobKind, TechniqueAnalysisSubject,
};

/// Two examples, E1 and E2, a fixed analysis, the recorder.
fn setup() -> Setup {
    let s = Setup::new(twang(ID, vec![])).analysis(Outcome::Fixed(canned_analysis()));
    let root = s.root().to_path_buf();
    s.repo.st().technique.as_mut().unwrap().media = ["E1", "E2"]
        .iter()
        .map(|id| example(id, &root.join(format!("{id}.wav")).to_string_lossy()))
        .collect();
    s
}

fn loaded(s: &Setup) -> Rig {
    let rig = s.loaded();
    assert_eq!(rig.vm.reference_media().unwrap().id, "E1");
    rig
}

fn start(rig: &mut Rig) {
    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
}

#[test]
fn switching_examples_while_the_take_records_is_refused_and_the_take_is_judged_against_the_example_it_armed_under(
) {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    assert!(rig.vm.is_recording_practice());

    // Mid-take the user opens the second example's card (the cards stay live
    // while recording). It used to switch the reference, and the take
    // recorded against E1 was then either paired with E2 (the generation was
    // snapshotted at Stop) or thrown away at Stop: refused instead.
    rig.call_settled(|vm| vm.toggle_example_expanded("E2"));
    assert_eq!(rig.vm.reference_media().unwrap().id, "E1");
    assert!(rig
        .vm
        .recording_notice()
        .unwrap()
        .contains("Stop the practice take"));
    // Nor can its own card fold away the Stop it holds.
    rig.call_settled(|vm| vm.toggle_example_expanded("E1"));
    assert_eq!(rig.vm.expanded_example_id().as_deref(), Some("E1"));
    assert!(rig.vm.is_recording_practice());

    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis().is_some());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert_eq!(rig.vm.reference_media().unwrap().id, "E1");
}

#[test]
fn a_take_whose_reference_held_from_arm_to_stop_is_analyzed_clearing_the_previous_take_mid_recording_does_not_void_it(
) {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis().is_some());

    start(&mut rig);
    // Discards the first take (the UI hides Clear while recording; the view
    // model must still keep the take being recorded, whose reference never
    // changed).
    rig.vm.clear_practice();
    rig.call_settled(|vm| vm.stop_practice());

    assert!(rig.vm.practice_analysis().is_some());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert_eq!(rig.vm.recording_notice(), None);
}

#[test]
fn a_reference_switched_while_the_start_is_still_pending_is_the_one_the_take_is_recorded_against() {
    let s = setup();
    let mut rig = loaded(&s);
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    assert!(rig.vm.starting());

    rig.call(|vm| vm.toggle_example_expanded("E2")); // before capture begins
    rig.release(JobKind::StartRecording);
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    assert!(rig.vm.is_recording_practice());
    rig.call_settled(|vm| vm.stop_practice());

    assert_eq!(rig.vm.reference_media().unwrap().id, "E2");
    assert!(rig.vm.practice_analysis().is_some(), "it is kept");
}

#[test]
fn a_reference_switched_while_the_take_is_being_analyzed_still_discards_it() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.hold(JobKind::AnalyzePractice);
    rig.call(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis_loading());
    rig.call(|vm| vm.toggle_example_expanded("E2"));
    rig.release(JobKind::AnalyzePractice);

    assert!(rig.vm.practice_analysis().is_none());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
}

// The page opens on the first example with no example ever tapped, so the
// reference is the first one by fallback, not by selection. Every media
// re-normalization then "selected" it and read that as a reference change:
// the take recorded against it was dropped although its reference never
// moved.

#[test]
fn edit_then_cancel_keeps_a_take_recorded_against_the_default_example() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis().is_some());

    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.discard_media_draft());
    rig.call_settled(|vm| vm.end_edit());

    assert_eq!(rig.vm.reference_media().unwrap().id, "E1");
    assert!(rig.vm.practice_analysis().is_some());
}

#[test]
fn a_text_only_save_keeps_a_take_recorded_against_the_default_example() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());

    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let edited = Technique {
        summary: "edited".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call_settled(|vm| vm.save(edited));
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    rig.call_settled(|vm| vm.end_edit());

    assert!(rig.vm.practice_analysis().is_some());
}

#[test]
fn a_remote_change_to_the_technique_keeps_a_take_whose_reference_it_left_alone() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());

    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));

    assert_eq!(rig.vm.reference_media().unwrap().id, "E1");
    assert!(rig.vm.practice_analysis().is_some());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
}

#[test]
fn a_remote_change_during_the_take_replayed_at_stop_does_not_discard_the_take_being_analyzed() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));
    rig.call_settled(|vm| vm.stop_practice());

    assert_eq!(s.analysis.calls(), 1);
    assert!(rig.vm.practice_analysis().is_some());
}

#[test]
fn a_remote_change_that_removes_the_reference_still_drops_the_take() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());

    s.repo.st().technique.as_mut().unwrap().media.remove(0);
    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));

    assert_eq!(rig.vm.reference_media().unwrap().id, "E2");
    assert!(rig.vm.practice_analysis().is_none());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
}

#[test]
fn a_re_recorded_take_shows_nothing_of_the_previous_one_while_it_analyzes() {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis().is_some());

    start(&mut rig);
    rig.hold(JobKind::AnalyzePractice);
    rig.call_settled(|vm| vm.stop_practice());
    // The old take's gap table and quality warning showed under "Analyzing
    // your take…" as if they were the new one's.
    assert!(rig.vm.practice_analysis_loading());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    assert!(rig.vm.practice_analysis().is_none());
    assert!(AnalysisSource::selected_metrics(&rig.vm).is_none());

    rig.release(JobKind::AnalyzePractice);
    assert!(rig.vm.practice_analysis().is_some());
}

#[test]
fn a_take_dropped_while_it_analyzes_frees_record_at_once_and_its_analysis_neither_runs_on_nor_touches_the_next_take(
) {
    let s = setup();
    let mut rig = loaded(&s);
    start(&mut rig);
    rig.hold(JobKind::AnalyzePractice);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis_loading());

    rig.call_settled(|vm| vm.toggle_example_expanded("E2"));
    // "Analyzing…" stood for a take that no longer existed, with Record
    // disabled until the discarded pass had run to its end.
    assert!(!rig.vm.practice_analysis_loading());
    assert!(rig.vm.can_record_practice());

    // A new take arms before the old pass is over, and the old pass ends
    // mid-take.
    start(&mut rig);
    let old_pass = rig.run_one(JobKind::AnalyzePractice);
    rig.land(old_pass);
    assert_eq!(s.analysis.st().skipped.len(), 1, "dropped, not analyzed");
    assert!(rig.vm.is_recording_practice());

    // The new take kept its temp file through the old pass's cleanup.
    rig.release(JobKind::AnalyzePractice);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis().is_some());
    assert_eq!(rig.vm.reference_media().unwrap().id, "E2");
    assert!(!rig.vm.practice_analysis_loading());
}

#[test]
fn a_take_that_fails_for_no_stated_cause_does_not_repeat_an_earlier_takes() {
    let s = setup();
    let mut rig = loaded(&s);
    s.analysis.st().throw_next = Some(MediaError::TooLong {
        max_duration: Duration::from_secs(10 * 60),
    });
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis_failed());
    assert!(rig.vm.practice_analysis_failure_message().is_some());

    s.analysis.st().panic_next = true; // an isolate that died
    start(&mut rig);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis_failed());
    assert_eq!(rig.vm.practice_analysis_failure_message(), None);
}

#[test]
fn example_players_are_paused_before_the_mic_opens() {
    // Also `technique_example_player_test.dart`'s "starting a practice take
    // pauses every example player".
    let s = setup();
    let mut rig = loaded(&s);
    rig.hold(JobKind::StartRecording);
    rig.call(|vm| vm.start_practice());
    assert!(rig.vm.starting());
    // Once the start resolved, capture was already running: a clip still
    // playing on speakers was in the take's opening.
    assert_eq!(rig.pauses, 1);
    rig.release(JobKind::StartRecording);
    assert_eq!(rig.pauses, 1);
    assert!(rig.vm.mic_live(), "and the players refuse to play into it");
}
