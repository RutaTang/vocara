//! Port of `technique_practice_take_test.dart`.
//!
//! The ephemeral practice take lives in the READ view only: the edit view
//! has no practice recorder, no Stop control and no Reference/Practice
//! switch. These tests pin the seams between the two — a take must never
//! leak into (or be stranded by) an edit session.
//!
//! Left to the detail screen's port: the Edit button disabled while a take
//! records, and a tap on it while a start pends opening no editor (the
//! screen checks `can_edit` at tap time).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vocara_services::engine::EngineBindings;
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::playback_engine::PlaybackEngine;

use super::support::*;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, JobKind, TechniqueAnalysisSubject, TechniqueDetailViewModel,
};

#[test]
fn edit_is_refused_while_a_practice_take_is_recording_or_starting() {
    // Regression: Edit stayed live during a practice take. The edit view has
    // no practice Stop control, its Save refuses while the mic is hot (every
    // attempt toasted "Couldn't save"), and Cancel's draft discard silently
    // aborted the take — the user's only way out threw the take away.
    let s = Setup::new(twang(ID, vec![]));
    let mut rig = s.loaded();
    assert!(rig.vm.can_edit());

    rig.call_settled(|vm| vm.start_practice());
    assert!(rig.vm.is_recording_practice());
    assert!(
        !rig.vm.can_edit(),
        "stop the take first: the editor has no practice controls"
    );

    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.can_edit());

    // A start still pending behind its waits: the tap itself must still be
    // refused, or the take arms inside the editor with no Stop.
    s.recorder.st().starting = true;
    assert!(!rig.vm.can_edit());
    s.recorder.st().starting = false;
    assert!(rig.vm.can_edit());
    assert_eq!(s.repo.st().base_saves, 0);
}

#[test]
fn opening_the_editor_points_the_analysis_back_at_the_reference_even_when_the_practice_analysis_lands_after_it(
) {
    // Regression: the edit view's example card has no Reference/Practice
    // switch, yet begin_edit kept the practice subject — the card showed the
    // practice take's analysis, and playback/scrub (which only drive the
    // reference) no longer moved it.
    let s = Setup::new(twang(ID, vec![example("M0", "/tmp/ref-a.wav")])).analysis(Outcome::Fresh);
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    rig.call_settled(|vm| vm.stop_practice());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );

    let same_as_reference = |vm: &TechniqueDetailViewModel| {
        Arc::ptr_eq(
            &AnalysisSource::selected_metrics(vm).unwrap(),
            &vm.reference_analysis().unwrap().metrics,
        )
    };
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
    assert!(same_as_reference(&rig.vm));
    rig.call(|vm| vm.end_edit());

    // Edit tapped while a take's analysis is still running: the take landing
    // afterwards must not flip the open editor over to it either.
    rig.hold(JobKind::AnalyzePractice);
    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    rig.call_settled(|vm| vm.stop_practice());
    assert!(rig.vm.practice_analysis_loading());
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.release(JobKind::AnalyzePractice);
    assert!(
        rig.vm.practice_analysis().is_some(),
        "the take itself is kept"
    );
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Reference
    );
    assert!(same_as_reference(&rig.vm));
}

/// An engine's native half that opens (and counts its destroys).
#[derive(Default)]
struct StubEngine {
    destroyed: AtomicUsize,
}

impl EngineBindings for StubEngine {
    fn destroy(&self) {
        self.destroyed.fetch_add(1, Ordering::SeqCst);
    }

    fn sample_rate(&self) -> f64 {
        48000.0
    }
}

#[test]
fn a_page_whose_engine_failed_to_open_builds_a_new_one_for_the_next_take_and_keeps_one_that_opened()
{
    // An engine that failed to open was kept for the page's life: every take
    // failed with "reconnect an audio device" even after the user did.
    let dud = Arc::new(GraphAudioPlaybackService::with_bindings(
        Arc::new(StubEngine::default()),
        None,
    ));
    PlaybackEngine::dispose(&*dud); // no output device: it did not open
    assert!(!dud.is_available());
    let native = Arc::new(StubEngine::default());
    let opened = Arc::new(GraphAudioPlaybackService::with_bindings(
        native.clone(),
        None,
    ));
    let engines = Arc::new(Mutex::new(VecDeque::from([dud.clone(), opened.clone()])));
    let made = Arc::new(AtomicUsize::new(0));

    let s = Setup::new(twang(ID, vec![]));
    let mut deps = s.deps();
    deps.create_engine = {
        let engines = engines.clone();
        let made = made.clone();
        Arc::new(move || {
            made.fetch_add(1, Ordering::SeqCst);
            engines.lock().unwrap().pop_front().unwrap()
        })
    };
    let (mut vm, _load) = TechniqueDetailViewModel::new(deps);

    assert!(Arc::ptr_eq(&vm.acquire_engine(), &dud));
    assert!(Arc::ptr_eq(&vm.acquire_engine(), &opened));
    assert_eq!(Arc::strong_count(&dud), 1, "the dud is let go");
    assert!(Arc::ptr_eq(&vm.acquire_engine(), &opened));
    assert_eq!(made.load(Ordering::SeqCst), 2);

    // The page's engine goes with the page.
    for job in vm.dispose().jobs() {
        drop(job.run());
    }
    assert_eq!(native.destroyed.load(Ordering::SeqCst), 1);
    assert!(!opened.is_available());
}
