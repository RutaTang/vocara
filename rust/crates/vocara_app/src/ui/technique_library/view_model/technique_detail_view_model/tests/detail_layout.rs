//! Port of the view-model cases of `technique_detail_layout_test.dart`, and
//! the view-model assertions of its widget tests.
//!
//! Left to the detail screen's port: the viewer and editor layouts (no
//! overflow, placeholders, the header's buttons and their edge, Examples
//! play-only in the read view), the spinner and "This technique could not be
//! loaded." over a failed load, the drop zone handing even unsupported
//! files to `add_media`, the download button, "Give the technique a name."
//! (the screen's check before `save`), and Save, Cancel and every field
//! disabled while `saving` (and while an example take Save finalizes is
//! importing).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_services::audio_exporter::AudioExporter;
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::engine::media::MediaError;
use vocara_services::local_storage_service::LocalStorageService;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn technique(media: Vec<TechniqueMedia>) -> Technique {
    Technique {
        aliases: vec!["Edge".into()],
        tags: vec!["EVT".into(), "Voice quality".into()],
        summary: "Bright carrying quality.".into(),
        description: "A long description. ".repeat(20),
        cues: "Quack like a duck.".into(),
        media,
        ..Technique::new("t1", "Twang", june20(), june20())
    }
}

fn m1() -> TechniqueMedia {
    TechniqueMedia {
        path: "/tmp/a.m4a".into(),
        ..example("m1", "")
    }
}

fn with_summary(rig: &Rig, summary: &str) -> Technique {
    Technique {
        summary: summary.into(),
        ..rig.vm.technique().unwrap().clone()
    }
}

#[test]
fn a_failed_load_reports_its_error_not_an_endless_spinner() {
    // Regression: the screen watches only the view model, and the load's
    // failure path notified nothing but the (unwatched) command — the
    // spinner, keyed on load.running, was never redrawn away.
    let s = Setup::new(technique(vec![]));
    s.repo.st().unreadable = true;
    let mut rig = s.rig();
    // The read fails only once the screen is up and showing the spinner.
    let read = rig.run_one(JobKind::Load);
    assert!(rig.vm.load().running());
    let before = rig.vm.revision();

    rig.land(read);
    assert!(rig.vm.load().error());
    assert!(!rig.vm.load().running());
    assert!(rig.vm.technique().is_none());
    assert!(rig.vm.revision() > before, "the screen hears it");
}

#[test]
fn dropping_only_unsupported_files_reports_the_skip() {
    // Regression guard: the drop zone used to filter unsupported files out
    // BEFORE calling the view model, so a drop of only unsupported files did
    // nothing at all.
    let s = Setup::new(technique(vec![]));
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.add_media(vec!["/tmp/notes.pdf".into()]));

    assert!(rig.vm.effective_media().is_empty());
    assert_eq!(
        rig.vm.recording_notice(),
        Some("No supported audio or video files in the drop."),
        "a drop that imports nothing must say so"
    );
}

#[test]
fn an_example_can_be_exported_from_the_read_view_too() {
    // A recorded example was reachable only by digging through the app's
    // container, its file named by a hash.
    let dir = tempfile::tempdir().unwrap();
    let source = write_file(dir.path(), "take.wav", &[1, 2, 3, 4]);
    let dest = dir.path().join("out").join("exported.wav");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let dest = dest.to_string_lossy().into_owned();
    let suggested = Arc::new(Mutex::new(Vec::<String>::new()));
    let media = vec![TechniqueMedia {
        label: "Recording 1".into(),
        ..example("m1", &source)
    }];
    let s = Setup::new(technique(media));
    let exporter = {
        let suggested = suggested.clone();
        let dest = dest.clone();
        AudioExporter::new(
            Arc::new(AudioFileService::new(Arc::new(
                LocalStorageService::with_root(dir.path().join("app")),
            ))),
            move |name| {
                suggested.lock().unwrap().push(name.to_string());
                Ok(Some(dest.clone()))
            },
        )
        .with_finalizer(s.finalizer.clone())
    };
    let s = s.exporter(exporter);
    let mut rig = s.loaded();

    rig.call_settled(|vm| vm.export_media("m1"));
    assert_eq!(*suggested.lock().unwrap(), ["Twang - Recording 1.wav"]);
    assert_eq!(std::fs::read(&dest).unwrap(), [1, 2, 3, 4]);
    assert_eq!(
        rig.vm.recording_notice(),
        Some(format!("Exported to {dest}").as_str())
    );
}

#[test]
fn a_video_examples_export_says_video_not_audio() {
    let media = vec![TechniqueMedia {
        kind: TechniqueMediaKind::Video,
        ..example("v1", "/nonexistent/vocara/clip.mp4")
    }];
    let s = Setup::new(technique(media));
    let exporter = AudioExporter::new(
        Arc::new(AudioFileService::new(Arc::new(
            LocalStorageService::with_root(s.root().join("app")),
        ))),
        |_| panic!("a missing file opens no panel"),
    )
    .with_finalizer(s.finalizer.clone());
    let s = s.exporter(exporter);
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.export_media("v1"));
    assert_eq!(
        rig.vm.recording_notice(),
        Some("The video file for this example is missing.")
    );
}

#[test]
fn a_too_long_analysis_failure_surfaces_its_cause() {
    // Regression: the decode-length cap refuses with user-presentable copy,
    // but the generic catch used to reduce it to the anonymous "couldn't
    // analyze" state — the message must reach the failure hint.
    let media = TechniqueMedia {
        path: "/tmp/hours-long.m4a".into(),
        ..example("m1", "")
    };
    let s =
        Setup::new(technique(vec![media.clone()])).analysis(Outcome::Refuse(MediaError::TooLong {
            max_duration: Duration::from_secs(30 * 60),
        }));
    let mut rig = s.loaded();
    assert_eq!(rig.vm.reference_media().unwrap().path, media.path);

    rig.call_settled(|vm| vm.ensure_reference_analysis());

    assert!(rig.vm.reference_analysis_failed());
    assert!(rig
        .vm
        .reference_analysis_failure_message()
        .unwrap()
        .contains("30 minutes"));
}

#[test]
fn save_is_a_no_op_while_a_save_is_already_in_flight() {
    // Regression: a double-click on Save used to run two commits against the
    // same stale base hashes. The second call must not reach the repository.
    let s = Setup::new(technique(vec![]));
    let mut rig = s.loaded();
    rig.hold(JobKind::Commit);

    let edited = with_summary(&rig, "changed");
    rig.call(|vm| vm.save(edited.clone()));
    // The flag flips synchronously, so a re-entrant call sees it at once.
    assert!(rig.vm.saving());
    rig.call(|vm| vm.save(edited));
    assert_eq!(
        rig.take_answers(),
        [Answer::Saved(false)],
        "second save is a no-op"
    );
    assert_eq!(rig.queued(JobKind::Commit), 1, "one repository write only");

    rig.release(JobKind::Commit);
    assert_eq!(
        rig.take_answers(),
        [Answer::Saved(true)],
        "the original save still persists"
    );
    assert!(!rig.vm.saving(), "flag clears when the save resolves");
    assert_eq!(s.repo.st().base_saves, 1);
}

#[test]
fn media_mutations_during_an_in_flight_save_are_no_ops() {
    // Regression: the commit snapshots effective_media when it starts, so a
    // clip removed/renamed/added during the wait used to be silently thrown
    // away (or worse, un-happen) when the save landed and cleared the draft.
    let s = Setup::new(technique(vec![m1()]));
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.hold(JobKind::Commit);
    let edited = with_summary(&rig, "changed");
    rig.call(|vm| vm.save(edited));
    assert!(rig.vm.saving());

    // Every mutation entry point must refuse while the commit is airborne.
    rig.call(|vm| vm.remove_media("m1"));
    rig.vm.rename_media("m1", "sneaky caption");
    rig.call(|vm| vm.add_media(vec!["/tmp/never-imported.wav".into()]));
    assert!(!rig.vm.has_media_draft(), "nothing staged mid-save");
    assert!(!rig.vm.importing_media(), "no import launched mid-save");
    assert!(!rig.vm.can_record_example(), "no take can arm mid-save");
    assert!(!rig.vm.can_record_practice());

    rig.release(JobKind::Commit);
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    // The saved result matches the pre-mutation state: the clip survives,
    // unrenamed.
    let stored = s.repo.technique();
    assert_eq!(stored.media.len(), 1);
    assert_eq!(stored.media[0].id, "m1");
    assert!(stored.media[0].label.is_empty());
}

#[test]
fn the_reverse_races_are_refused_too_no_second_save_and_no_draft_discard_while_the_commit_is_airborne(
) {
    let s = Setup::new(technique(vec![m1()]));
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    // Stage a rename so the draft is non-empty when the save launches: the
    // discard below would clear it if the guard failed.
    rig.vm.rename_media("m1", "kept label");
    assert!(rig.vm.has_media_draft());
    rig.hold(JobKind::Commit);
    let edited = with_summary(&rig, "changed");
    rig.call(|vm| vm.save(edited));
    assert!(rig.vm.saving());

    // A racing second save must refuse against the same base…
    let racer = with_summary(&rig, "racer");
    rig.call(|vm| vm.save(racer));
    // …and a direct discard (Cancel is disabled, but the method is callable)
    // must be a no-op while the commit is persisting the draft's outcome.
    rig.call(|vm| vm.discard_media_draft());
    assert_eq!(
        rig.take_answers(),
        [Answer::Saved(false), Answer::DraftDiscarded(false)]
    );
    assert!(
        rig.vm.has_media_draft(),
        "the airborne save owns the draft; discard must not clear it"
    );

    rig.release(JobKind::Commit);
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    // Exactly the FIRST save persisted, with the staged rename applied.
    let stored = s.repo.technique();
    assert_eq!(stored.summary, "changed");
    assert_eq!(stored.media[0].label, "kept label");
    assert!(!rig.vm.has_media_draft(), "the save cleared its draft");
}

#[test]
fn the_edit_surface_is_frozen_while_the_save_is_in_flight() {
    // Save and Cancel (and every field) are disabled on `saving`: no double
    // commit, no Discard deleting files the commit references, no text typed
    // into a form already read.
    let s = Setup::new(technique(vec![]));
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    assert!(!rig.vm.saving());
    rig.hold(JobKind::Commit);
    let t = rig.vm.technique().unwrap().clone();
    rig.call(|vm| vm.save(t));
    assert!(rig.vm.saving());

    rig.release(JobKind::Commit);
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert!(!rig.vm.saving());
    assert_eq!(s.repo.st().base_saves, 1);
}

#[test]
fn an_import_abandoned_by_cancel_clears_its_busy_state_for_the_next_edit_session() {
    // Regression: the abandoned branch reset importing_media without
    // notifying, so the screen kept the import spinner and the disabled
    // Save/add/Record it last saw — into the NEXT edit session.
    let s = Setup::new(technique(vec![])).media(FakeMedia::sandbox());
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.hold(JobKind::Import);
    rig.call(|vm| vm.add_media(vec!["/tmp/dropped.wav".into()]));
    assert!(rig.vm.importing_media());
    // Cancel, then Edit again while the copy is still parked.
    rig.call_settled(|vm| vm.discard_media_draft());
    rig.call_settled(|vm| vm.end_edit());
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let before = rig.vm.revision();

    rig.release(JobKind::Import);
    assert!(!rig.vm.importing_media());
    assert!(
        rig.vm.revision() > before,
        "the screen must hear that the import is over"
    );
    assert!(
        rig.vm.effective_media().is_empty(),
        "the orphan is not staged"
    );
    assert_eq!(s.media.deleted(), ["/sandbox/dropped.wav"]);
}

#[test]
fn a_staged_clip_is_listed_once_while_the_save_commits_and_once_again_after_the_save_fails() {
    // Regression: the commit's optimistic aggregate already carries the
    // staged additions while the draft still holds them, so
    // effective_media listed every staged clip twice for the whole commit.
    let s = Setup::new(technique(vec![])).media(FakeMedia::sandbox());
    s.repo.st().fail_next_save = true;
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.add_media(vec!["/tmp/clip.wav".into()]));
    let staged = rig.vm.effective_media().pop().unwrap();
    let ids =
        |rig: &Rig| -> Vec<String> { rig.vm.effective_media().into_iter().map(|m| m.id).collect() };

    rig.hold(JobKind::Commit);
    let t = rig.vm.technique().unwrap().clone();
    rig.call(|vm| vm.save(t));
    assert!(rig.vm.saving());
    assert_eq!(ids(&rig), [staged.id.as_str()]);
    rig.release(JobKind::Commit);
    assert_eq!(rig.take_answers(), [Answer::Saved(false)]);
    // The failed save reverted the aggregate; the clip is still staged — and
    // still listed exactly once — for the retry.
    assert_eq!(ids(&rig), [staged.id.as_str()]);
    assert!(rig.vm.has_media_draft());

    let t = rig.vm.technique().unwrap().clone();
    rig.call_settled(|vm| vm.save(t));
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert_eq!(ids(&rig), [staged.id.as_str()]);
    assert!(!rig.vm.has_media_draft());
    let stored: Vec<String> = s.repo.technique().media.into_iter().map(|m| m.id).collect();
    assert_eq!(stored, [staged.id]);
}

#[test]
fn save_while_an_example_take_records_keeps_the_edit_frozen_until_the_take_and_the_edits_are_committed(
) {
    // Regression: Save stopped the take and waited on its import with Cancel
    // still live. Cancel → Discard in that window tore the form down, and
    // the resuming Save then dereferenced the disposed controllers (or, with
    // the dialog still open when the import landed, committed anyway).
    let s = Setup::new(Technique {
        summary: String::new(),
        description: String::new(),
        cues: String::new(),
        ..technique(vec![])
    })
    .media(FakeMedia::sandbox());
    // A take that touches no disk.
    s.recorder.st().mode = StartMode::At("/tmp/vocara_no_such_take_dir/take.wav".into());
    let mut rig = s.loaded();
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    let revised = Technique {
        name: "Twang, revised".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call_settled(|vm| vm.start_example_recording());
    assert_eq!(rig.take_answers(), [Answer::ExampleStart(None)]);
    assert!(rig.vm.is_recording_example());

    // Save: the take stops and its import parks.
    rig.hold(JobKind::Import);
    rig.call_settled(|vm| vm.save(revised));
    assert!(!rig.vm.is_recording_example(), "Save released the mic");
    assert!(rig.vm.saving_example(), "the take is still importing");
    assert!(
        rig.vm.saving(),
        "Cancel and the form are frozen: a Discard now would tear the form \
         down under the save"
    );
    assert!(rig.take_answers().is_empty());

    // The import lands, then the commit: the take AND the typed edit
    // persist, and the editor closes.
    rig.release(JobKind::Import);
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    let stored = s.repo.technique();
    assert_eq!(stored.name, "Twang, revised");
    let paths: Vec<&str> = stored.media.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, ["/sandbox/take.wav"]);
    assert!(!Path::new("/tmp/vocara_no_such_take_dir").exists());
}
