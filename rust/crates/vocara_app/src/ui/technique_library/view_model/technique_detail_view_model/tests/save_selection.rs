//! Port of `technique_save_selection_test.dart`: a save resolves every
//! staged clip through the content-addressed pool, so it comes back under a
//! DIFFERENT path with the same id; the selection (by id) and the analysis
//! caches (by path) must follow it.

use vocara_model::util::dart_path;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

/// A persisted example M1 already under its pool path, a repository that
/// pools what it saves, and the same canned analysis for every call.
fn setup() -> (Setup, String) {
    let s = Setup::new(twang(ID, vec![])).analysis(Outcome::Fixed(canned_analysis()));
    let pool = s.root().join("assets").to_string_lossy().into_owned();
    s.repo.st().technique.as_mut().unwrap().media =
        vec![example("M1", &dart_path::join(&pool, "M1.wav"))];
    s.repo.st().save_mode = SaveMode::Pool(pool.clone());
    (s, pool)
}

fn record_example(rig: &mut Rig) {
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    rig.call_settled(|vm| vm.stop_example_recording());
    assert!(matches!(
        rig.take_answers()[..],
        [Answer::ExampleStop(Some(_))]
    ));
}

fn save(rig: &mut Rig) -> Answer {
    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    rig.last_answer()
}

#[test]
fn save_keeps_the_just_recorded_example_selected_and_carries_its_analysis_across_the_path_rewrite()
{
    let (s, _) = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    record_example(&mut rig);

    // The just-recorded clip is selected + expanded, and its analysis is
    // computed under the sandbox path it was staged at.
    let recorded = rig.vm.effective_media().pop().unwrap();
    assert_eq!(rig.vm.reference_media().unwrap().id, recorded.id);
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    let computed = rig.vm.reference_analysis().expect("computed");
    let calls_before_save = s.analysis.calls();

    assert_eq!(save(&mut rig), Answer::Saved(true));

    // The commit rewrote the clip's path to its pool path; the selection and
    // the expansion must follow the clip (by id), not fall back to the first
    // example, and the analysis computed under the sandbox path must remain
    // reachable under the new one.
    let reference = rig.vm.reference_media().unwrap();
    assert_eq!(reference.id, recorded.id);
    assert_ne!(reference.path, recorded.path);
    assert_eq!(rig.vm.expanded_example_id(), Some(reference.id));
    assert_eq!(rig.vm.reference_analysis(), Some(computed), "the same one");
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(s.analysis.calls(), calls_before_save);
}

#[test]
fn a_save_whose_reread_failed_carries_the_analysis_to_the_pool_path_once_the_owed_reload_lands() {
    let (s, pool) = setup();
    s.repo.st().save_mode = SaveMode::PoolRereadFails(pool);
    let mut rig = s.loaded();
    rig.vm.begin_edit();
    rig.vm.begin_media_draft();
    record_example(&mut rig);
    let recorded = rig.vm.effective_media().pop().unwrap();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    let computed = rig.vm.reference_analysis().expect("computed");
    let calls = s.analysis.calls();

    assert_eq!(save(&mut rig), Answer::Saved(true));
    assert_eq!(
        rig.vm.reference_media().unwrap().path,
        recorded.path,
        "as sent"
    );
    rig.call_settled(|vm| vm.end_edit());

    let reference = rig.vm.reference_media().unwrap();
    assert_eq!(reference.id, recorded.id);
    assert_ne!(reference.path, recorded.path, "pooled");
    assert_eq!(rig.vm.reference_analysis(), Some(computed));
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(s.analysis.calls(), calls);
}

#[test]
fn an_example_whose_bytes_land_after_its_analysis_failed_is_analyzed_once_a_remote_reload_brings_its_pool_path(
) {
    let (s, pool) = setup();
    // Not on this device yet: the repository's placeholder.
    s.repo.st().technique.as_mut().unwrap().media = vec![example("M9", "asset://A9")];
    s.analysis.st().outcome = Outcome::Nothing;
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(rig.vm.reference_analysis_failed());

    // The bytes land (an import of the same file elsewhere re-points the
    // row), and a remote edit reloads the technique.
    s.analysis.st().outcome = Outcome::Fixed(canned_analysis());
    let pooled = dart_path::join(&pool, "M9.wav");
    s.repo.st().technique.as_mut().unwrap().media = vec![example("M9", &pooled)];
    rig.call_settled(|vm| vm.on_remote_applied(&remote(ID)));
    assert_eq!(rig.vm.reference_media().unwrap().path, pooled);
    assert!(!rig.vm.reference_analysis_failed());
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(rig.vm.reference_analysis().is_some());
}

#[test]
fn save_carries_the_analysis_failed_latch_across_the_path_rewrite() {
    // The pool copy is byte-identical, so a recompute is doomed.
    let (s, _) = setup();
    s.analysis.st().outcome = Outcome::Nothing;
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    record_example(&mut rig);
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(rig.vm.reference_analysis_failed());
    let calls_before_save = s.analysis.calls();

    assert_eq!(save(&mut rig), Answer::Saved(true));

    assert!(rig.vm.reference_analysis_failed());
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(s.analysis.calls(), calls_before_save);
}

#[test]
fn a_save_landing_while_the_staged_clip_is_still_analyzed_does_not_start_a_second_analysis_of_it() {
    // Regression: the in-flight analysis kept its completion keyed on the
    // staged (sandbox) path, so the saved example read as not loading under
    // its pool path — the view started a second full decode of the same
    // bytes, and the first one parked a dead entry in the shared cache.
    let (s, _) = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    record_example(&mut rig);
    rig.hold(JobKind::AnalyzeReference);
    rig.call(|vm| vm.ensure_reference_analysis());
    assert!(rig.vm.reference_analysis_loading());

    assert_eq!(save(&mut rig), Answer::Saved(true));
    assert!(rig.vm.reference_analysis_loading(), "the same analysis");
    rig.call(|vm| vm.ensure_reference_analysis()); // the screen's post-frame call
    assert_eq!(rig.queued(JobKind::AnalyzeReference), 1);

    rig.release(JobKind::AnalyzeReference);
    assert!(!rig.vm.reference_analysis_loading());
    assert_eq!(rig.vm.reference_analysis(), Some(canned_or_fixed(&s)));
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(s.analysis.calls(), 1);
}

/// The analysis the fake hands out every call.
fn canned_or_fixed(s: &Setup) -> vocara_model::models::technique_analysis::TechniqueAnalysis {
    match &s.analysis.st().outcome {
        Outcome::Fixed(a) => a.clone(),
        _ => panic!("a fixed analysis"),
    }
}
