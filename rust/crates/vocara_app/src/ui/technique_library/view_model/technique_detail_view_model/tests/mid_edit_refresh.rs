//! Port of `technique_mid_edit_refresh_test.dart`: a remote change landing
//! WHILE the technique editor is open must not advance the base hashes the
//! next save merges against.
//!
//! The editor seeds its text controllers once, when Edit is tapped, and
//! never re-seeds them; Save then submits all eight text fields from those
//! controllers. If a freshness reload moved the base to the remote row
//! meanwhile, `desired != base` holds even though the user typed nothing —
//! which is exactly the condition the repository's keep-theirs branch needs
//! to be FALSE. A later save (adding an example clip, say) would then write
//! the stale text back over the remote change under a fresh stamp.
//!
//! The screen's part of the editor is modelled as it worked: Edit is
//! `begin_media_draft` + `begin_edit` with the form seeded from the
//! technique as of the tap; Save sends that seeded technique; Cancel is
//! `discard_media_draft` + `end_edit`. Left to the detail screen's port: a
//! double-click on Save not landing on the Delete that takes its place.

use vocara_model::models::technique::Technique;

use super::support::*;
use crate::ui::technique_library::view_model::technique_detail_view_model::{Answer, JobKind};

fn technique() -> Technique {
    Technique {
        summary: "local summary".into(),
        description: "A description.".into(),
        cues: "Quack like a duck.".into(),
        ..Technique::new("t1", "Twang", june20(), june20())
    }
}

fn setup() -> Setup {
    let s = Setup::new(technique()).media(FakeMedia::pool());
    s.repo.st().stored_hashes = hashes(&[("t1", "BASE-HASH")]);
    s
}

/// Sync applying a remote edit of this technique: the stored row and its
/// hash both move on, and the event reaches the page.
fn remote_edit(s: &Setup, rig: &mut Rig) {
    s.repo
        .remote_edit(|t| t.summary = "remote summary".into(), "REMOTE-HASH");
    rig.call_settled(|vm| vm.on_remote_applied(&remote("t1")));
}

fn summary(rig: &Rig) -> String {
    rig.vm.technique().unwrap().summary.clone()
}

/// Edit tapped: the form is seeded from the technique as of the tap.
fn edit(rig: &mut Rig) -> Technique {
    rig.vm.begin_media_draft();
    rig.vm.begin_edit();
    rig.vm.technique().unwrap().clone()
}

#[test]
fn a_remote_change_landing_during_the_first_load_is_loaded_after_it() {
    // Skipped while the page's first read was out, and never replayed: the
    // rename stayed invisible, through Edit and Cancel too.
    let s = setup();
    let mut rig = s.rig();
    // The first read reads the store, then parks before it answers.
    let first_read = rig.run_one(JobKind::Load);
    assert!(rig.vm.load().running());
    remote_edit(&s, &mut rig);
    rig.land(first_read);
    rig.settle();
    assert_eq!(summary(&rig), "remote summary");
}

#[test]
fn a_remote_change_mid_edit_does_not_advance_the_save_base() {
    let s = setup();
    let mut rig = s.loaded();
    let seeded = edit(&mut rig);

    remote_edit(&s, &mut rig);
    // The form's controllers still hold the pre-remote text, so the
    // aggregate and base they will be saved against must not have moved
    // either.
    assert_eq!(summary(&rig), "local summary");

    rig.call_settled(|vm| vm.save(seeded));
    let st = s.repo.st();
    assert_eq!(st.saved_with_base.len(), 1);
    assert_eq!(
        st.saved_with_base[0]["t1"], "BASE-HASH",
        "the save must merge against the snapshot the form was seeded from, \
         or keep-theirs can never fire"
    );
}

#[test]
fn a_reload_skipped_during_the_edit_is_replayed_on_cancel() {
    let s = setup();
    let mut rig = s.loaded();
    edit(&mut rig);
    remote_edit(&s, &mut rig);
    assert_eq!(summary(&rig), "local summary");

    // Leaving edit without saving: pinning the base was only ever about the
    // open form, so the read view must not be left showing stale content
    // with nothing pending to correct it.
    rig.call_settled(|vm| vm.discard_media_draft());
    assert_eq!(rig.take_answers(), [Answer::DraftDiscarded(true)]);
    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(summary(&rig), "remote summary");
}

#[test]
fn edit_tapped_before_the_reload_repaints_seeds_the_form_from_the_reloaded_technique() {
    // Regression: the header's Edit carried the technique of its last build.
    // A click landing after a reload but before the next frame seeded the
    // form from the pre-reload text while the base hashes had already moved
    // to the remote row, so a Save with nothing typed wrote the old text back
    // over the remote change.
    let s = setup();
    let mut rig = s.loaded();
    remote_edit(&s, &mut rig);
    assert_eq!(summary(&rig), "remote summary");

    let seeded = edit(&mut rig);
    rig.call_settled(|vm| vm.save(seeded));
    let st = s.repo.st();
    assert_eq!(st.saved.len(), 1);
    assert_eq!(st.saved[0].summary, "remote summary");
    assert_eq!(st.saved_with_base[0]["t1"], "REMOTE-HASH");
}

#[test]
fn outside_an_edit_session_a_remote_change_still_refreshes() {
    let s = setup();
    let mut rig = s.loaded();
    remote_edit(&s, &mut rig);
    assert_eq!(summary(&rig), "remote summary");
}

#[test]
fn a_reload_skipped_during_a_practice_take_is_replayed_when_the_take_stops() {
    // Regression: only end_edit replayed a skipped reload, and a practice
    // take holds no edit session — the read view stayed stale after Stop with
    // nothing pending to correct it, and a later Edit seeded the form from
    // the stale text (whose save then overwrote the remote change).
    let s = setup();
    let mut rig = s.loaded();
    rig.call_settled(|vm| vm.start_practice());
    assert_eq!(rig.take_answers(), [Answer::PracticeStart(None)]);
    remote_edit(&s, &mut rig);
    assert_eq!(summary(&rig), "local summary", "held by the take");

    rig.call_settled(|vm| vm.stop_practice());
    assert_eq!(summary(&rig), "remote summary");
}

#[test]
fn a_reload_skipped_while_an_import_outlives_its_edit_is_replayed_when_the_import_ends() {
    // Cancel's replay found the abandoned copy still running and skipped
    // again; the copy's end replayed nothing, so the read view kept the old
    // text and the next edit, seeded from it, wrote it over the remote
    // change.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_edit();
    rig.hold(JobKind::Import);
    rig.call(|vm| vm.add_media(vec!["/src/a large take.wav".into()]));
    assert!(rig.vm.importing_media());
    remote_edit(&s, &mut rig);
    // Cancel, Discard: the copy is still running.
    rig.call_settled(|vm| vm.discard_media_draft());
    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(summary(&rig), "local summary", "held by the copy");

    rig.release(JobKind::Import);
    assert!(!rig.vm.importing_media());
    assert_eq!(summary(&rig), "remote summary");
}

fn concurrent_description(sent: Technique) -> Technique {
    Technique {
        description: "a concurrent description".into(),
        ..sent
    }
}

#[test]
fn a_save_whose_reread_failed_reloads_the_store_once_the_edit_ends() {
    // The commit stood in what it sent for the re-read: the read view kept
    // that, not the store, which held a concurrent writer's row (and pool
    // paths for clips the editor still knew by their sandbox ones), until
    // some later remote change happened to reload it.
    let s = setup();
    let mut rig = s.loaded();
    s.repo.st().save_mode = SaveMode::CommitWithoutReread(concurrent_description);

    rig.vm.begin_edit();
    let edited = Technique {
        summary: "edited".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call_settled(|vm| vm.save(edited));
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    assert_eq!(
        rig.vm.technique().unwrap().description,
        "A description.",
        "as sent"
    );
    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(summary(&rig), "edited");
    assert_eq!(
        rig.vm.technique().unwrap().description,
        "a concurrent description"
    );
    assert!(!rig.vm.has_media_draft());
}

#[test]
fn an_owed_reload_that_fails_stays_owed() {
    // Cleared before the reload ran, the debt went with a read that failed
    // as the save's own had: the read view kept what the save sent.
    let s = setup();
    let mut rig = s.loaded();
    s.repo.st().save_mode = SaveMode::CommitWithoutReread(concurrent_description);

    rig.vm.begin_edit();
    let edited = Technique {
        summary: "edited".into(),
        ..rig.vm.technique().unwrap().clone()
    };
    rig.call_settled(|vm| vm.save(edited));
    assert_eq!(rig.take_answers(), [Answer::Saved(true)]);
    s.repo.st().load_failures = 1;
    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(
        rig.vm.technique().unwrap().description,
        "A description.",
        "not yet"
    );

    rig.vm.begin_edit();
    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(
        rig.vm.technique().unwrap().description,
        "a concurrent description"
    );
}

#[test]
fn a_skipped_reload_is_still_owed_after_edit_reopens() {
    // begin_edit cleared the miss: Edit reopened while the abandoned copy
    // still ran dropped the remote change, so neither the copy's end nor the
    // second Cancel replayed it and the read view kept the old text.
    let s = setup();
    let mut rig = s.loaded();
    rig.vm.begin_edit();
    rig.hold(JobKind::Import);
    rig.call(|vm| vm.add_media(vec!["/src/a large take.wav".into()]));
    remote_edit(&s, &mut rig);
    rig.call_settled(|vm| vm.discard_media_draft());
    rig.call_settled(|vm| vm.end_edit());
    rig.vm.begin_edit(); // Edit again before the copy ends
    rig.release(JobKind::Import);
    assert_eq!(summary(&rig), "local summary", "held by the edit");

    rig.call_settled(|vm| vm.end_edit());
    assert_eq!(summary(&rig), "remote summary");
}
