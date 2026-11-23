//! Port of `technique_quit_finalize_test.dart`: a quit while a take is in
//! flight. The fake recorder bypasses the finalizer registry, so the tests
//! drive the quit path with `quit_finalize` (Dart's `@visibleForTesting`
//! `quitFinalize`); its post-stop tail is held on the real stop-finalize
//! gate, over a private finalizer whose `finalize_all` a thread runs as the
//! exit request would.
//!
//! The widget test (a quit while Save finalizes a recording example) is
//! ported through the view model: the screen's Save is `save`.

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::TechniqueMediaKind;
use vocara_services::recording_finalizer::RecordingFinalizer;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn setup() -> Setup {
    Setup::new(twang(ID, vec![]))
}

/// Runs the exit request's `finalize_all` on a thread; the receiver hears
/// when it returns.
fn exit_request(finalizer: &Arc<RecordingFinalizer>) -> mpsc::Receiver<()> {
    let (done, heard) = mpsc::channel();
    let finalizer = Arc::clone(finalizer);
    thread::spawn(move || {
        finalizer.finalize_all();
        let _ = done.send(());
    });
    heard
}

/// Whether the exit request is still waiting after a moment.
fn still_waiting(heard: &mpsc::Receiver<()>) -> bool {
    heard.recv_timeout(Duration::from_millis(100)).is_err()
}

#[test]
fn quit_finalize_persists_an_in_flight_example_recording() {
    // Regression: the quit path imported the clip and staged it only into
    // the draft, so after the process exit no row referenced the file and the
    // next boot's staging sweep trashed the only copy.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    assert!(rig.vm.is_recording_example());

    rig.call_settled(|vm| vm.quit_finalize());

    assert!(!rig.vm.is_recording_example());
    let stored = s.repo.technique();
    assert_eq!(
        stored.media.len(),
        1,
        "the captured clip must be committed, not just staged"
    );
    let persisted = &stored.media[0];
    assert_eq!(persisted.kind, TechniqueMediaKind::Audio);
    assert!(Path::new(&persisted.path).exists());
    // The commit adopted the clip: it is no longer double-listed as both a
    // persisted row and a draft addition.
    assert_eq!(rig.vm.effective_media().len(), 1);
    assert_eq!(rig.vm.reference_media().unwrap().id, persisted.id);
    assert!(!rig.vm.saving());
}

#[test]
fn the_quits_own_commit_of_an_example_holds_the_quit_until_it_lands() {
    // Stopping unregisters the take's finalizer: unheld, a close-button run
    // cut off mid-stage or mid-commit left the exit request's run nothing to
    // wait on, and the clip — a temp WAV boot recovery never scans — was
    // lost.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.call_settled(|vm| vm.start_example_recording());
    rig.hold(JobKind::Commit);
    rig.call_settled(|vm| vm.quit_finalize());
    assert!(
        s.finalizer.has_active(),
        "the stage and commit hold the quit"
    );

    let heard = exit_request(&s.finalizer);
    assert!(still_waiting(&heard), "the exit request's run waits for it");

    rig.release(JobKind::Commit);
    heard
        .recv_timeout(Duration::from_secs(5))
        .expect("the exit request returns once the clip landed");
    assert_eq!(s.repo.technique().media.len(), 1, "the clip landed");
    assert!(!s.finalizer.has_active());
}

#[test]
fn quit_finalize_of_a_practice_take_persists_nothing() {
    // Ephemeral by design.
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    assert!(rig.vm.is_recording_practice());
    let path = s.recorder.st().path.clone().unwrap();

    rig.call_settled(|vm| vm.quit_finalize());

    assert!(!rig.vm.is_recording_practice());
    assert_eq!(s.repo.st().base_saves, 0);
    assert!(s.repo.technique().media.is_empty());
    assert!(
        !Path::new(&path).parent().unwrap().exists(),
        "its temp folder is cleaned up"
    );
}

#[test]
fn a_quit_while_a_plain_save_commits_waits_for_the_commit() {
    // Regression: only a Save that finalized a recording held the quit. A
    // text or media edit's Save followed by Cmd+Q exited mid-commit — most
    // plausibly during the pool import of a large staged clip — and the edit
    // was gone, its staged files swept as orphans at the next boot.
    let s = setup();
    let mut rig = s.loaded();
    rig.hold(JobKind::Commit);
    rig.vm.begin_edit();
    let typed = Technique {
        summary: "typed".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call_settled(|vm| vm.save(typed));
    assert_eq!(rig.queued(JobKind::Commit), 1, "the commit is in flight");

    let heard = exit_request(&s.finalizer);
    assert!(still_waiting(&heard), "the quit must wait for the save");

    rig.release(JobKind::Commit);
    heard.recv_timeout(Duration::from_secs(5)).expect("quit");
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert_eq!(s.repo.technique().summary, "typed", "the edit landed");
    assert!(!s.finalizer.has_active());
}

#[test]
fn a_quit_while_save_is_finalizing_a_recording_example_waits_for_the_take_to_commit() {
    // Regression: Save stopped the take — which unregisters the armed-window
    // quit finalizer — and only then imported and committed it, with nothing
    // holding the quit. A quit in that window exited mid-save, and the take
    // survived only as a temp WAV that boot recovery never scans.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.start_example_recording());
    assert!(rig.vm.is_recording_example());

    rig.hold(JobKind::Commit);
    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    assert_eq!(
        rig.queued(JobKind::Commit),
        1,
        "imported; the commit is in flight"
    );

    let heard = exit_request(&s.finalizer);
    assert!(
        still_waiting(&heard),
        "the quit gate must cover the stop-to-commit window"
    );

    rig.release(JobKind::Commit);
    heard.recv_timeout(Duration::from_secs(5)).expect("quit");
    let stored = s.repo.technique();
    assert_eq!(stored.media.len(), 1, "the take committed");
    assert!(Path::new(&stored.media[0].path).exists());
    assert!(!s.finalizer.has_active());
}
