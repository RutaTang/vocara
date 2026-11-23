//! Port of `technique_reference_analysis_queue_test.dart`: reference
//! analyses queue behind the app-wide analysis gate (two slots, first come
//! first served). One the user moved away from while it waited — another
//! example selected, the screen left — must be dropped when the gate admits
//! it, not decoded in full ahead of the analyses still wanted.
//!
//! The gate's admission is the job running: a held job has not been
//! admitted.

use std::time::Duration;

use vocara_services::engine::media::MediaError;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::JobKind;

/// Examples A, B and C at `/tmp/<name>.<ext>`, or, [synced], held as refs
/// to bytes not on this device yet.
fn setup(ext: &str, synced: bool) -> Setup {
    let media = ["a", "b", "c"]
        .iter()
        .map(|name| {
            let path = if synced {
                format!("asset://{}", name.repeat(64))
            } else {
                format!("/tmp/{name}.{ext}")
            };
            example(&name.to_uppercase(), &path)
        })
        .collect();
    Setup::new(twang(ID, media))
}

fn decoded(s: &Setup) -> Vec<String> {
    s.analysis.st().decoded.clone()
}

#[test]
fn clicking_through_examples_decodes_only_the_one_still_selected_when_the_gate_admits() {
    // Regression: every example clicked queued a full decode + DSP pass, and
    // the one finally selected (and a practice take's analysis after it)
    // waited behind references nobody was looking at any more.
    let s = setup("wav", false);
    let mut rig = s.loaded();
    rig.hold(JobKind::AnalyzeReference);

    rig.call(|vm| vm.ensure_reference_analysis()); // A, the default selection
    rig.call(|vm| vm.select_example("B"));
    rig.call(|vm| vm.select_example("C"));
    rig.release(JobKind::AnalyzeReference);

    assert_eq!(decoded(&s), ["/tmp/c.wav"]);
    rig.call(|vm| vm.select_example("A"));
    assert!(
        !rig.vm.reference_analysis_failed(),
        "a skipped analysis must not latch the example as failed"
    );
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert_eq!(decoded(&s), ["/tmp/c.wav", "/tmp/a.wav"]);
}

#[test]
fn an_analysis_still_queued_when_the_screen_goes_away_is_never_decoded() {
    let s = setup("wav", false);
    let mut rig = s.loaded();
    rig.hold(JobKind::AnalyzeReference);
    rig.call(|vm| vm.ensure_reference_analysis());
    rig.dispose();
    rig.release(JobKind::AnalyzeReference);

    assert!(decoded(&s).is_empty());
}

#[test]
fn an_example_whose_analysis_failed_for_no_stated_cause_can_be_analyzed_again() {
    // The failure was latched for the life of the page.
    let s = setup("wav", false);
    let mut rig = s.loaded();

    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(rig.vm.reference_analysis_failed());
    assert!(rig.vm.can_retry_reference_analysis());
    rig.call_settled(|vm| vm.ensure_reference_analysis()); // latched: not asked again
    assert_eq!(decoded(&s), ["/tmp/a.wav"]);

    rig.call_settled(|vm| vm.retry_reference_analysis());
    assert_eq!(decoded(&s), ["/tmp/a.wav", "/tmp/a.wav"]);
}

#[test]
fn an_example_in_a_format_the_engine_cannot_decode_says_so_is_never_decoded_and_offers_no_retry() {
    // It plays through the media player; analyzed, it failed with no stated
    // cause and a "Try again" that could never work, each tap a full decode.
    let s = setup("opus", false);
    let mut rig = s.loaded();

    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(decoded(&s).is_empty());
    assert!(rig.vm.reference_analysis_failed());
    assert!(rig
        .vm
        .reference_analysis_failure_message()
        .unwrap()
        .contains("can't be analyzed"));
    assert!(!rig.vm.can_retry_reference_analysis());
}

#[test]
fn an_example_whose_file_has_not_reached_this_device_says_so_is_never_decoded_and_offers_no_retry()
{
    // Decoded, its asset ref failed to open: "The file could not be read.
    // Check the disk", a retry that could never work, under a player saying
    // the file isn't on this device.
    let s = setup("wav", true);
    let mut rig = s.loaded();

    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(decoded(&s).is_empty());
    assert!(rig.vm.reference_analysis_failed());
    assert!(rig
        .vm
        .reference_analysis_failure_message()
        .unwrap()
        .contains("isn't on this device"));
    assert!(!rig.vm.can_retry_reference_analysis());
}

#[test]
fn an_example_whose_file_could_not_be_read_can_be_tried_again() {
    // Its stated cause hid the retry, while its words said to try again.
    let s = setup("wav", false).analysis(Outcome::Refuse(MediaError::Read));
    let mut rig = s.loaded();

    rig.call_settled(|vm| vm.ensure_reference_analysis());
    assert!(rig
        .vm
        .reference_analysis_failure_message()
        .unwrap()
        .contains("could not be read"));
    assert!(rig.vm.can_retry_reference_analysis());

    s.analysis.st().outcome = Outcome::Refuse(MediaError::TooLong {
        max_duration: Duration::from_secs(10 * 60),
    });
    rig.call_settled(|vm| vm.retry_reference_analysis());
    assert_eq!(decoded(&s).len(), 2);
    assert!(!rig.vm.can_retry_reference_analysis(), "too long stays");
}
