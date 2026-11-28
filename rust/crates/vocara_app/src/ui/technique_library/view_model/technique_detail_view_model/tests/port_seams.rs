//! The Rust port's own seams: the recorder's device events delivered by
//! the tick (Dart's controller callbacks), the quit bridge (Dart's
//! finalizer called `quitFinalize` on the UI isolate), the playhead and
//! scrub signals, the synthetic analysis track, lost and repeated job
//! reports — and `deep_link_back_test.dart`'s detail part (Delete answers,
//! and the screen navigates back on it).

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_services::mic_recording_controller::{MicEvent, AEC_BYPASSED_NOTICE};

use super::support::*;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, JobKind, TechniqueAnalysisSubject,
};

fn setup() -> Setup {
    Setup::new(twang(ID, vec![example("E1", "/tmp/e1.wav")])).analysis(Outcome::Fresh)
}

#[test]
fn an_input_lost_mid_practice_stops_the_take_and_says_so() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    assert!(rig.vm.next_tick_due().is_some(), "the recorder ticks");
    s.recorder.st().events.push(MicEvent::InputDeviceLost);

    rig.call_settled(|vm| vm.tick());
    assert!(!rig.vm.is_recording_practice());
    assert_eq!(
        rig.vm.recording_notice(),
        Some("Recording stopped: the input device was disconnected.")
    );
    assert!(
        rig.vm.practice_analysis().is_some(),
        "what was captured is kept"
    );
}

#[test]
fn an_example_at_its_length_limit_is_stopped_and_staged() {
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    s.recorder.st().events.push(MicEvent::RecordLimitReached);

    rig.call_settled(|vm| vm.tick());
    assert!(!rig.vm.is_recording_example());
    assert_eq!(
        rig.vm.recording_notice(),
        Some(
            "Recording stopped at the six-hour maximum length. \
             Everything up to that point was saved."
        )
    );
    assert_eq!(rig.vm.effective_media().len(), 2, "the take is staged");
    assert!(
        rig.take_answers().is_empty(),
        "a stop nobody called answers nobody"
    );
}

#[test]
fn a_practice_take_at_its_length_limit_is_told_nothing_of_saving() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    s.recorder.st().events.push(MicEvent::RecordLimitReached);
    rig.call_settled(|vm| vm.tick());
    assert_eq!(
        rig.vm.recording_notice(),
        Some("Recording stopped at the six-hour maximum length.")
    );
}

/// A write failing mid-take is reported by the tick, not at Stop: the take
/// stops there, what landed is kept (the recorder's partial is readable),
/// and the page says why it stopped and that the example may be incomplete.
#[test]
fn an_example_whose_write_failed_is_stopped_at_once_and_what_landed_is_kept() {
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    s.recorder.st().clean = false;
    s.recorder.st().events.push(MicEvent::RecordWriteFailed);

    rig.call(|vm| vm.tick());
    assert!(!rig.vm.is_recording_example(), "stopped on the tick");
    assert_eq!(
        rig.vm.recording_notice(),
        Some(
            "Recording stopped: the example could not be written to disk, which may be \
             full. What was recorded up to then is being saved."
        )
    );
    rig.settle();
    assert_eq!(
        rig.vm.recording_notice(),
        Some("Recording hit a disk error. The example may be incomplete.")
    );
    assert_eq!(rig.vm.effective_media().len(), 2, "what landed is staged");
}

#[test]
fn a_practice_take_whose_write_failed_is_stopped_at_once() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    s.recorder.st().clean = false;
    s.recorder.st().events.push(MicEvent::RecordWriteFailed);
    rig.call(|vm| vm.tick());
    assert!(!rig.vm.is_recording_practice(), "stopped on the tick");
    assert_eq!(
        rig.vm.recording_notice(),
        Some("Recording stopped: the take could not be written to disk, which may be full.")
    );
}

#[test]
fn a_degraded_input_is_said_and_the_take_goes_on() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    let before = rig.vm.revision();
    s.recorder
        .st()
        .events
        .push(MicEvent::DeviceNotice(AEC_BYPASSED_NOTICE));
    rig.call_settled(|vm| vm.tick());
    assert_eq!(rig.vm.recording_notice(), Some(AEC_BYPASSED_NOTICE));
    assert!(rig.vm.revision() > before);
    assert!(rig.vm.is_recording_practice());
}

#[test]
fn a_quit_waiting_on_the_page_is_answered_by_the_tick_once_its_take_is_cleaned_up() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    let take_dir = std::path::Path::new(&s.recorder.st().path.clone().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();

    // The controller's quit finalize, on a finalizer thread.
    let quit = Arc::clone(&rig.vm.quit);
    let waiting = thread::spawn(move || quit.request_and_wait());
    while !rig.vm.quit.pending() {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(rig.vm.next_tick_due().is_some(), "the tick is due at once");

    rig.hold(JobKind::DeleteTake);
    rig.call(|vm| vm.tick());
    assert!(!rig.vm.is_recording_practice());
    thread::sleep(Duration::from_millis(50));
    assert!(
        !waiting.is_finished(),
        "answered once the take is cleaned up"
    );

    rig.release(JobKind::DeleteTake);
    waiting.join().unwrap();
    assert!(!take_dir.exists());
    assert_eq!(
        s.repo.st().base_saves,
        0,
        "a practice take persists nothing"
    );
}

#[test]
fn a_quit_waiting_on_a_page_that_goes_away_is_answered() {
    let s = setup();
    let mut rig = s.loaded();
    let quit = Arc::clone(&rig.vm.quit);
    let waiting = thread::spawn(move || quit.request_and_wait());
    while !rig.vm.quit.pending() {
        thread::sleep(Duration::from_millis(1));
    }
    rig.dispose();
    waiting.join().unwrap();
    // And a later one at once.
    rig.vm.quit.request_and_wait();
}

#[test]
fn a_scrub_moves_the_playhead_and_the_reference_player() {
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    let duration = rig.vm.reference_analysis().unwrap().duration_ms;
    assert_eq!(duration, 1000);

    rig.call(|vm| vm.seek(Duration::from_millis(400)));
    assert_eq!(rig.vm.position(), Duration::from_millis(400));
    // Clamped to the subject; a repeat is a request of its own.
    rig.call(|vm| vm.seek(Duration::from_secs(5)));
    rig.call(|vm| vm.seek(Duration::from_secs(5)));
    assert_eq!(
        rig.seeks,
        [
            Duration::from_millis(400),
            Duration::from_millis(1000),
            Duration::from_millis(1000)
        ]
    );

    // The player drives the playhead while the reference shows.
    rig.vm
        .set_playhead_from_playback(Duration::from_millis(250));
    assert_eq!(rig.vm.position(), Duration::from_millis(250));

    // A practice take has no player to move.
    rig.call_settled(|vm| vm.start_practice());
    rig.vm
        .set_playhead_from_playback(Duration::from_millis(100));
    assert_eq!(
        rig.vm.position(),
        Duration::from_millis(250),
        "not while recording"
    );
    rig.call_settled(|vm| vm.stop_practice());
    assert_eq!(
        rig.vm.analysis_subject(),
        TechniqueAnalysisSubject::Practice
    );
    rig.call(|vm| vm.seek(Duration::from_millis(300)));
    assert_eq!(rig.seeks.len(), 3);
    assert_eq!(rig.vm.position(), Duration::from_millis(300));
}

#[test]
fn the_analysis_views_read_a_synthetic_track_of_the_subject() {
    let s = setup();
    let mut rig = s.loaded();
    assert!(rig.vm.selected_track().is_none());
    rig.call_settled(|vm| vm.ensure_reference_analysis());
    let track = rig.vm.selected_track().unwrap();
    assert_eq!(track.display_name, "Reference");
    assert_eq!(track.duration_ms(), 1000);
    assert_eq!(rig.vm.analysis_start_ms(), 0);
    assert_eq!(rig.vm.analysis_duration_ms(), 1000);
    // No clearly-voiced frame: the playhead opens mid-clip.
    assert_eq!(rig.vm.position(), Duration::from_millis(500));

    rig.call_settled(|vm| vm.start_practice());
    assert!(rig.vm.mic_live());
    rig.call_settled(|vm| vm.stop_practice());
    assert_eq!(
        rig.vm.selected_track().unwrap().display_name,
        "Your practice"
    );
}

#[test]
fn a_lost_commit_reverts_and_answers_not_saved() {
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::Commit);
    let edited = Technique {
        summary: "lost".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    let next = rig.vm.save(edited);
    let job = next.jobs().pop().unwrap();
    assert_eq!(job.kind(), JobKind::Commit);
    assert_eq!(rig.vm.technique().unwrap().summary, "lost", "optimistic");

    let lost = job.lost("worker lost");
    rig.land(lost.clone());
    assert_eq!(rig.take_answers(), [Answer::Saved(false)]);
    assert_eq!(rig.vm.technique().unwrap().summary, "", "reverted");
    assert!(!rig.vm.saving());

    // A report lands once.
    rig.land(lost);
    assert!(rig.take_answers().is_empty());
    assert!(!rig.vm.has_pending_jobs());
}

#[test]
fn delete_answers_and_the_technique_is_gone() {
    // deep_link_back_test.dart: Delete lands on the library — the screen
    // navigates back on the answer.
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.delete());
    assert_eq!(rig.take_answers(), [Answer::Deleted(true)]);
    assert_eq!(s.repo.st().deletes, 1);
    assert!(s.repo.st().technique.is_none());
}
