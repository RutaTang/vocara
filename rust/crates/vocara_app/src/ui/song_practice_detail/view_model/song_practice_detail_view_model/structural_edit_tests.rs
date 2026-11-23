//! The cases of `song_practice_structural_edit_test.dart` the first port
//! (`tests/structural.rs`) left out, on the same in-memory screen
//! ([`Fixture`]): a merge's note shift read from the audio, a note editor
//! against a split's clamp, the separation flow (edits landing while it
//! runs, Cancel once its save runs, its failures and what they take back,
//! the mixer state of the lane it re-kinds, its import discipline), a
//! duplicate of the take as it is once copied, the timeline after a failed
//! save, reloads after a release and after a save that could not revert,
//! an import's file after a revert, and times kept by text across a lyrics
//! edit.
//!
//! (The file's lyrics sync-cursor cases are the view model's take and lyrics
//! flow tests'.)

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_store::sync::sync_engine::RemoteApplied;

use super::tests::*;
use super::{Effect, SongPracticeDetailViewModel};

/// The Original: a reference lane holding the full mix.
fn original() -> AudioTrack {
    AudioTrack::new("ref", AudioTrackKind::Reference, "Original", at()).with_audio(
        "/pool/original.wav",
        0,
        1000,
        Vec::new(),
    )
}

/// The seed practice with [`original`] as its reference (Dart's
/// `pumpVm(reference: ...)`).
fn with_original() -> SongPractice {
    SongPractice {
        reference: Some(Arc::new(original())),
        ..seed_practice(None)
    }
}

/// A screen over [practice] whose separation parks on [gate].
fn separating(practice: SongPractice, gate: &Arc<Gate>) -> (Fixture, Screen) {
    let (f, s) = Fixture::open(Setup {
        practice,
        separation: Arc::new(GatedSeparation {
            gate: Arc::clone(gate),
            cancels: AtomicUsize::new(0),
        }),
        ..Setup::default()
    });
    assert!(s.vm.separation_available(), "the probe answered");
    (f, s)
}

fn remote(id: &str) -> RemoteApplied {
    RemoteApplied {
        rows: Vec::new(),
        practice_ids: [id.to_string()].into_iter().collect(),
        technique_ids: Default::default(),
    }
}

/// Keeps answering what lands for [for_ms]: the screen's turns after an
/// event the test cannot observe (a re-read parked on its gate).
fn pump_for(s: &mut Screen, for_ms: u64) {
    let until = Instant::now() + Duration::from_millis(for_ms);
    s.pump_until(|_| Instant::now() >= until);
}

/// Imports [source] as the accompaniment and lets it land.
fn add_accompaniment(s: &mut Screen, source: &str) {
    let call = s.call(|vm| vm.add_accompaniment(source));
    s.await_reply(call);
    s.settle();
}

fn display_name(p: &SongPractice, id: &str) -> String {
    p.vocal_tracks
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.display_name.clone())
        .expect("the take")
}

// --- Persist-before-delete invariant -------------------------------------------------

#[test]
fn the_bottom_takes_notes_follow_its_audio_in_the_joined_file() {
    // The shift was the top clip's stored length, which a clip measured by an
    // earlier build holds up to 10 ms short of its audio.
    let dir = tempfile::tempdir().unwrap();
    let top_wav = dir.path().join("top.wav");
    std::fs::write(
        &top_wav,
        vocara_services::audio_dsp::write_wav_mono(&vec![0.0f64; 59232], 48000), // 1234 ms
    )
    .unwrap();
    let mut takes = mergeable_takes();
    takes[0].clips[0].audio_file_path = top_wav.to_string_lossy().into_owned();
    takes[0].clips[0].duration_ms = 1230;
    takes[1].notes = vec![PracticeNote {
        id: "n".into(),
        position_ms: 100,
        end_ms: None,
        content: "n".into(),
        created_at: at(),
        updated_at: at(),
    }];
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(takes)),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();

    let tracks = &s.practice().vocal_tracks;
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].notes[0].position_ms, 1334);
}

#[test]
fn a_notes_text_saved_from_the_copy_its_editor_opened_with_keeps_the_clamp_a_split_gave_it() {
    let (_f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.add_note("single", 100, Some(900), "range"));
    s.settle();
    let opened = s.track("single").notes[0].clone(); // what the editor holds
    s.at_ms(500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    let clamped = s.track("single").notes[0].clone();
    assert_ne!(clamped.end_ms, Some(900), "precondition: split clamped");

    s.act(|vm| vm.update_note_content("single", &opened.id, "edited"));
    s.settle();

    let note = s.track("single").notes[0].clone();
    assert_eq!(note.content, "edited");
    assert_eq!(note.end_ms, clamped.end_ms);
    assert_eq!(note.position_ms, clamped.position_ms);
}

// --- Separation ----------------------------------------------------------------------

// Separation is the longest operation in the app, and the swap rebuilt the
// Reference lane from the snapshot taken before it started — so a rename or
// a note added to that lane while it ran was overwritten (and the note
// tombstoned) the moment the stems landed.
#[test]
fn a_rename_and_a_note_landing_during_a_separation_survive_it() {
    let gate = Gate::new();
    let (_f, mut s) = separating(with_original(), &gate);

    s.act(|vm| vm.separate_backing("ref"));
    s.act(|vm| vm.rename_track("ref", "Original renamed"));
    s.act(|vm| vm.add_note("ref", 100, None, "note during separation"));
    gate.open();
    s.settle();

    let p = s.practice().clone();
    let reference = p.reference.as_ref().expect("the Original");
    assert_eq!(
        reference.display_name, "Original renamed",
        "the rename must not be overwritten by the swap"
    );
    let notes: Vec<&str> = reference.notes.iter().map(|n| n.content.as_str()).collect();
    assert_eq!(
        notes,
        ["note during separation"],
        "a note added mid-separation must not be dropped"
    );
    // The instrumental is named from the lane's CURRENT name.
    assert_eq!(
        p.accompaniment
            .as_ref()
            .expect("the instrumental")
            .display_name,
        "Original renamed (Instrumental)"
    );
}

#[test]
fn once_the_backing_is_being_saved_cancel_is_no_longer_offered() {
    // Pressed during the commit save, Cancel was ignored: the overlay still
    // offered it, then the backing was replaced anyway.
    let gate = Gate::new();
    let (f, mut s) = separating(with_original(), &gate);
    let save_gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&save_gate));

    s.act(|vm| vm.separate_backing("ref"));
    assert!(s.vm.can_cancel_separation());
    gate.open();
    s.pump_until(|_| f.repo.s().in_flight > 0);
    assert!(s.vm.is_separating());
    assert!(!s.vm.can_cancel_separation());

    f.repo.s().save_gate = None;
    save_gate.open();
    s.settle();
    assert!(s.practice().accompaniment.is_some());
}

#[test]
fn a_separation_whose_save_fails_leaves_the_selection_on_the_original() {
    // The separation flips the backing flag silently while it saves; that
    // save's selection check judged the backing lane by the instrumental's
    // slot (empty here) and moved the selection off the Original on screen,
    // and the failure path put only the flag back.
    let gate = Gate::new();
    let (f, mut s) = separating(with_original(), &gate);
    s.act(|vm| vm.set_play_reference(true));
    s.act(|vm| vm.select_track("ref"));
    s.settle();

    s.act(|vm| vm.separate_backing("ref"));
    f.repo.s().fail_next_save = true;
    gate.open();
    s.settle();

    assert!(s.vm.mixer().play_reference());
    assert_eq!(s.vm.selected_track_id(), Some("ref"));
}

#[test]
fn a_lane_picked_while_a_failed_separation_saves_stays_picked() {
    let gate = Gate::new();
    let (f, mut s) = separating(with_original(), &gate);
    s.act(|vm| vm.set_play_reference(true));
    s.act(|vm| vm.select_track("ref"));
    s.settle();

    s.act(|vm| vm.separate_backing("ref"));
    let saves = f.repo.s().save_no;
    // The failed save's revert re-reads the store: park it there.
    let reread = Gate::new();
    {
        let mut r = f.repo.s();
        r.fail_next_save = true;
        r.load_gate = Some(Arc::clone(&reread));
    }
    gate.open();
    s.pump_until(|_| f.repo.s().save_no > saves);
    pump_for(&mut s, 100); // the failure lands; its re-read parks
    let take = s.practice().vocal_tracks[0].id.clone();
    s.act(|vm| vm.select_track(&take)); // the user picks a lane meanwhile
    f.repo.s().load_gate = None;
    reread.open();
    s.settle();

    assert_eq!(s.vm.selected_track_id(), Some(take.as_str()));
}

#[test]
fn separating_a_backing_that_is_not_yet_the_reference_keeps_the_lanes_mixer_state() {
    // The ordinary first separation: the full mix sits in the ACCOMPANIMENT
    // slot, so the swap has to rebuild it under the reference kind. Only the
    // kind changes — it is the same lane, and everything the user set on it
    // has to survive. Volume and the colour tint were carried; pan, mute,
    // solo and the insert chain were silently reset.
    let gate = Gate::new();
    let (_f, mut s) = separating(seed_practice(None), &gate);
    add_accompaniment(&mut s, "/src/full-mix.wav");
    let backing_id = s.practice().accompaniment.as_ref().unwrap().id.clone();

    s.act(|vm| vm.mixer_do(|m, h| m.set_track_volume_live(h, &backing_id, 0.42)));
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_pan_live(h, &backing_id, -0.6)));
    s.act(|vm| vm.mixer_do(|m, h| m.toggle_mute(h, &backing_id)));
    s.act(|vm| {
        vm.mixer_do(|m, h| {
            m.set_track_fx_live(
                h,
                &backing_id,
                TrackFx {
                    eq_on: true,
                    ..TrackFx::flat()
                },
            )
        })
    });
    s.act(|vm| vm.set_track_color(&backing_id, Some(0xFF00FF00)));
    s.settle();

    s.act(|vm| vm.separate_backing(&backing_id));
    gate.open();
    s.settle();

    let reference = s.practice().reference.clone().expect("the Original");
    assert_eq!(reference.id, backing_id, "the same lane, re-kinded");
    assert!((reference.volume - 0.42).abs() <= 1e-9);
    assert!((reference.pan - -0.6).abs() <= 1e-9);
    assert!(reference.muted);
    assert!(reference.fx.as_ref().is_some_and(|fx| fx.eq_on));
    assert_eq!(reference.color_value, Some(0xFF00FF00));
}

// Regression guard: the separation success path imported the instrumental
// stem directly — skipping the transcode — so the separated backing (a
// 44.1 kHz sidecar WAV) bypassed the disk-streaming path and was fully
// re-decoded into RAM at every project open, exactly the failure mode the
// import discipline exists to prevent.
#[test]
fn the_separated_instrumental_goes_through_the_same_import_discipline_as_every_other_import() {
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = separating(with_original(), &gate);
    *guard(&f.edits.transcode_to) = Some("/edit/inst48k.wav".into());

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    assert_eq!(*guard(&f.edits.streamable_calls), ["/import/0.wav"]);
    assert_eq!(
        s.practice().accompaniment.as_ref().unwrap().clips[0].audio_file_path,
        "/edit/inst48k.wav",
        "the committed backing must be the streamable WAV"
    );
    assert!(
        f.events().contains(&"delete:/import/0.wav".to_string()),
        "the non-streamable import copy is redundant once transcoded"
    );
}

#[test]
fn a_failed_save_after_separation_deletes_the_transcoded_wav_and_the_import_copy_and_keeps_nothing_live(
) {
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = separating(with_original(), &gate);
    *guard(&f.edits.transcode_to) = Some("/edit/inst48k.wav".into());
    f.repo.s().fail_saves = true;

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    let events = f.events();
    assert!(events.contains(&"delete:/edit/inst48k.wav".to_string()));
    assert!(events.contains(&"delete:/import/0.wav".to_string()));
    assert!(s.practice().accompaniment.is_none());
}

#[test]
fn a_failed_save_after_separation_tells_the_engine_the_backing_toggle_came_back() {
    // The flow flips the toggle to the instrumental silently before its
    // save, and the save's failure path resyncs the engine under that
    // flipped flag. Restoring the flag silently afterwards left the toggle
    // showing Original while the engine played the accompaniment side and
    // kept the original muted.
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = separating(with_original(), &gate);
    s.act(|vm| vm.set_play_reference(true)); // listening to the original
    s.settle();
    let ref_clip = s.practice().reference.as_ref().unwrap().clips[0].id.clone();
    assert_eq!(guard(&f.engine.muted).get(&ref_clip), Some(&false));
    f.repo.s().fail_saves = true;

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    assert!(s.vm.mixer().play_reference());
    assert_eq!(
        guard(&f.engine.muted).get(&ref_clip),
        Some(&false),
        "the original must play"
    );
}

#[test]
fn a_duplicate_is_of_the_take_as_it_is_once_the_files_are_copied() {
    // Built from the snapshot taken before the copies, it missed a rename and
    // a clip drag made while they ran.
    let (f, mut s) = Fixture::open(Setup::default());
    let copies = Gate::new();
    *guard(&f.edits.duplicate_gate) = Some(Arc::clone(&copies));

    s.act(|vm| vm.duplicate_track("multi"));
    s.act(|vm| vm.rename_track("multi", "Verse"));
    s.vm.move_clip_start_live("multi", "clip-b", 700);
    s.act(|vm| vm.commit_track_move());
    copies.open();
    s.settle();

    let copy = s.practice().vocal_tracks.last().unwrap().clone();
    assert_eq!(copy.display_name, "Verse copy");
    let starts: Vec<i64> = copy.clips.iter().map(|c| c.start_ms).collect();
    assert_eq!(starts, [0, 700]);
}

// --- Remote-applied reload -----------------------------------------------------------

#[test]
fn a_failed_save_keeps_the_timeline_over_every_lane() {
    // A failed save reverts to the stored aggregate, whose timeline need not
    // cover its takes (one recovered or synced past it): grown on load, it
    // shrank back on the revert and cut the take off at the right edge.
    let (f, mut s) = Fixture::open(Setup::default());
    let mut stored = f.repo.stored();
    stored.timeline_duration_ms = 1500;
    let mut late = take("late", "Take 3 (recovered)", "/pool/late.wav");
    late.clips[0].start_ms = 60_000;
    late.clips[0].duration_ms = 5000;
    stored.vocal_tracks.push(Arc::new(late));
    {
        let mut r = f.repo.s();
        r.stored = Some(stored);
        r.fail_saves = true;
    }

    s.act(|vm| vm.rename_track("single", "renamed"));
    s.settle();

    let p = s.practice();
    assert!(p.vocal_tracks.iter().any(|t| t.id == "late"));
    assert!(p.timeline_duration_ms >= p.content_end_ms());
}

#[test]
fn a_reload_after_the_release_shows_the_store() {
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_volume_live(h, "single", 0.3)));
    s.act(|vm| vm.mixer_do(|m, _| m.commit_track_volume()));
    s.settle();

    let mut stored = f.repo.stored();
    stored.title = "Renamed elsewhere".into();
    f.repo.s().stored = Some(stored);
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();

    assert_eq!(s.practice().title, "Renamed elsewhere");
}

// --- A failed save that could not revert ---------------------------------------------
//
// With no store read to revert to, the failed change stays on screen, a
// remote reload holds off for it, and the next save writes it with its own.

#[test]
fn a_change_whose_save_could_not_revert_is_kept_through_a_reload_and_retried_by_the_next_save() {
    let (f, mut s) = Fixture::open(Setup::default());
    {
        let mut r = f.repo.s();
        r.fail_next_save = true;
        r.fail_next_load = true;
    }
    s.act(|vm| vm.rename_track("single", "kept"));
    s.settle();
    let mut stored = f.repo.stored();
    stored.title = "Renamed elsewhere".into();
    f.repo.s().stored = Some(stored);
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();
    assert_eq!(display_name(s.practice(), "single"), "kept");

    s.act(|vm| vm.rename_track("multi", "next"));
    s.settle();

    let stored = f.repo.stored();
    assert_eq!(display_name(&stored, "single"), "kept");
    assert_eq!(display_name(&stored, "multi"), "next");
}

#[test]
fn an_accompaniment_import_a_failed_save_could_not_revert_keeps_its_file() {
    // The import's rollback deleted the file the lane still on screen
    // played: the next save stored a lane whose audio was gone.
    let (f, mut s) = Fixture::open(Setup::default());
    {
        let mut r = f.repo.s();
        r.fail_next_save = true;
        r.fail_next_load = true;
    }
    add_accompaniment(&mut s, "/src/backing.wav");
    let path = s
        .practice()
        .accompaniment
        .as_ref()
        .expect("kept on screen")
        .clips[0]
        .audio_file_path
        .clone();
    assert!(!f.events().contains(&format!("delete:{path}")));

    s.act(|vm| vm.rename_track("single", "next"));
    s.settle();

    assert_eq!(
        f.repo.stored().accompaniment.as_ref().unwrap().clips[0].audio_file_path,
        path
    );
}

#[test]
fn an_import_a_revert_took_back_still_drops_its_file() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_next_save = true;

    add_accompaniment(&mut s, "/src/backing.wav");

    assert!(s.practice().accompaniment.is_none());
    assert!(f.events().contains(&"delete:/import/0.wav".to_string()));
}

// --- A save failing beside another ---------------------------------------------------

/// Runs the separation of [lane] with its save held, makes [edit] while it
/// is held, then lets the save fail.
fn fail_separation_around(
    f: &Fixture,
    s: &mut Screen,
    gate: &Arc<Gate>,
    lane: &str,
    edit: impl FnOnce(&mut SongPracticeDetailViewModel) -> Vec<Effect>,
) {
    let save_gate = Gate::new();
    {
        let mut r = f.repo.s();
        r.save_gate = Some(Arc::clone(&save_gate));
        r.fail_next_save = true;
    }
    s.act(|vm| vm.separate_backing(lane));
    gate.open();
    s.pump_until(|_| f.repo.s().in_flight > 0);
    assert!(
        s.practice().accompaniment.is_some(),
        "its save runs with the instrumental in"
    );
    s.act(edit);
    f.repo.s().save_gate = None;
    save_gate.open();
    s.settle();
}

#[test]
fn a_failed_separation_keeps_an_edit_made_to_the_original_lane_itself() {
    let gate = Gate::new();
    let (f, mut s) = separating(with_original(), &gate);
    fail_separation_around(&f, &mut s, &gate, "ref", |vm| {
        vm.rename_track("ref", "My original")
    });

    assert_eq!(
        s.practice().reference.as_ref().unwrap().display_name,
        "My original"
    );
    let stored = f.repo.stored();
    assert_eq!(
        stored.reference.as_ref().unwrap().display_name,
        "My original"
    );
    assert!(stored.accompaniment.is_none());
}

#[test]
fn a_failed_separation_puts_a_moved_original_back_in_the_backing_slot_edited() {
    // The ordinary first separation moves the full mix from the backing slot
    // to the reference one: taken back, it returns there.
    let gate = Gate::new();
    let (f, mut s) = separating(seed_practice(None), &gate);
    add_accompaniment(&mut s, "/src/full-mix.wav");
    let backing_id = s.practice().accompaniment.as_ref().unwrap().id.clone();
    let id = backing_id.clone();
    fail_separation_around(&f, &mut s, &gate, &backing_id, move |vm| {
        vm.rename_track(&id, "Full mix")
    });

    for p in [s.practice().clone(), f.repo.stored()] {
        assert!(p.reference.is_none());
        let backing = p.accompaniment.as_ref().expect("the full mix is back");
        assert_eq!(backing.id, backing_id);
        assert_eq!(backing.kind, AudioTrackKind::Accompaniment);
        assert_eq!(backing.display_name, "Full mix");
    }
}

#[test]
fn a_taken_back_separation_leaves_the_engine_without_it() {
    // With no revert (the reload failed too), the take-back changed the lanes
    // after the failed save's resync: the engine kept the instrumental, whose
    // file the rollback then deleted.
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = separating(with_original(), &gate);
    {
        let mut r = f.repo.s();
        r.fail_next_save = true;
        r.fail_next_load = true;
    }

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    assert!(s.practice().accompaniment.is_none());
    assert!(!guard(&f.engine.last_paths).contains(&"/import/0.wav".to_string()));
}

// --- Editing timed lyrics ------------------------------------------------------------
//
// Times were matched to the edited text by line text alone, first come first
// served.

#[test]
fn a_fixed_typo_in_the_first_chorus_leaves_the_second_its_own_time() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics("[00:30.00]chorus\n[00:40.00]verse\n[01:30.00]chorus"));
    assert!(s.landed(call));
    let times =
        |s: &Screen| -> Vec<Option<i64>> { s.vm.lyrics().iter().map(|l| l.time_ms).collect() };

    let call = s.call(|vm| vm.set_lyrics_text("chorus!\nverse\nchorus"));
    assert!(s.landed(call));
    assert_eq!(times(&s), [None, Some(40000), Some(90000)]);

    // The first chorus deleted.
    let call = s.call(|vm| vm.set_lyrics_text("verse\nchorus"));
    assert!(s.landed(call));
    assert_eq!(times(&s), [Some(40000), Some(90000)]);
}
