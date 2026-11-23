//! The separation cases of `song_practice_structural_edit_test.dart` that
//! `structural` leaves: edits landing while the stems are made or saved,
//! Cancel through the run, the separated backing's import discipline, a
//! commit save that fails (what is deleted, what the engine and the
//! selection are left on), and the take-back beside another save ("a save
//! failing beside another"). The separation runs on [`GatedSeparation`],
//! parked until the test lets the stems in.

use std::sync::atomic::{AtomicBool, Ordering};

use vocara_model::models::audio_fx::TrackFx;

use super::*;

/// The Original lane: the full song, in the reference slot.
fn original() -> AudioTrack {
    AudioTrack::new("ref", AudioTrackKind::Reference, "Original", at()).with_audio(
        "/pool/original.wav",
        0,
        1000,
        Vec::new(),
    )
}

fn seeded_with(reference: Option<AudioTrack>) -> SongPractice {
    SongPractice {
        reference: reference.map(Arc::new),
        ..seed_practice(None)
    }
}

/// A screen over `practice` whose separation parks on `gate`, its probe
/// landed (Dart's wait for `separationAvailable`).
fn open(practice: SongPractice, gate: &Arc<Gate>) -> (Fixture, Screen) {
    let (f, s) = Fixture::open(Setup {
        practice,
        separation: Arc::new(GatedSeparation {
            gate: Arc::clone(gate),
            cancels: std::sync::atomic::AtomicUsize::new(0),
        }),
        ..Setup::default()
    });
    assert!(s.vm.separation_available());
    (f, s)
}

/// [`open`] with the instrumental's analysis parked on `analysis` and
/// `reached` set once it is (Dart's `_SignalingWaveforms`).
fn open_signaling(
    practice: SongPractice,
    gate: &Arc<Gate>,
    reached: &Arc<AtomicBool>,
    analysis: &Arc<Gate>,
) -> (Fixture, Screen) {
    let (reached, analysis) = (Arc::clone(reached), Arc::clone(analysis));
    let (f, s) = Fixture::open(Setup {
        practice,
        separation: Arc::new(GatedSeparation {
            gate: Arc::clone(gate),
            cancels: std::sync::atomic::AtomicUsize::new(0),
        }),
        analyze: Arc::new(move |_: &str| {
            reached.store(true, Ordering::SeqCst);
            analysis.wait();
            Ok(WaveformResult {
                peaks: Vec::new(),
                duration_ms: 1000,
            })
        }),
        ..Setup::default()
    });
    assert!(s.vm.separation_available());
    (f, s)
}

/// Dart's `await` on an edit made while the separation runs: what it
/// started has landed, the separation (parked) aside.
fn edit_landed(s: &mut Screen) {
    s.pump_until(|s| s.in_flight == 1);
}

// --- Edits and Cancel during the run ---------------------------------------------------------

// Regression guard: separation is the longest operation in the app, and the
// swap rebuilt the Reference lane from the snapshot taken before it started
// — so a rename or a note added to that lane while it ran was overwritten
// (and the note tombstoned) the moment the stems landed.
#[test]
fn a_rename_and_a_note_landing_during_a_separation_survive_it() {
    let gate = Gate::new();
    let (_f, mut s) = open(seeded_with(Some(original())), &gate);
    s.act(|vm| vm.separate_backing("ref")); // parked on the stems
    s.act(|vm| vm.rename_track("ref", "Original renamed"));
    edit_landed(&mut s);
    s.act(|vm| vm.add_note("ref", 100, None, "note during separation"));
    edit_landed(&mut s);
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
        p.accompaniment.as_ref().map(|a| a.display_name.as_str()),
        Some("Original renamed (Instrumental)")
    );
}

#[test]
fn the_instrumental_sits_where_the_original_lane_does() {
    // Built without a start, it landed at 0 under an Original dragged right:
    // played early against it and against every take sung to it.
    let gate = Gate::new();
    let mut moved = original();
    moved.clips[0].start_ms = 5000;
    let (_f, mut s) = open(seeded_with(Some(moved)), &gate);
    s.act(|vm| vm.separate_backing("ref"));
    gate.open();
    s.settle();
    let p = s.practice().clone();
    let start = |t: Option<&Arc<AudioTrack>>| t.and_then(|t| t.primary_clip()).map(|c| c.start_ms);
    assert_eq!(start(p.reference.as_ref()), Some(5000));
    assert_eq!(start(p.accompaniment.as_ref()), Some(5000));
}

// Cancel stays on screen while the stems are imported and analyzed (the
// whole visible run on a cached song), but the service had nothing left to
// cancel and the backing was applied anyway.
#[test]
fn cancel_after_the_stems_arrive_keeps_the_backing_as_it_was() {
    let gate = Gate::new();
    let reached = Arc::new(AtomicBool::new(false));
    let analysis = Gate::new();
    let (f, mut s) = open_signaling(seeded_with(Some(original())), &gate, &reached, &analysis);
    let backing = s.practice().accompaniment.as_ref().map(|a| a.id.clone());

    s.act(|vm| vm.separate_backing("ref"));
    gate.open(); // the stems are in
    s.pump_until(|_| reached.load(Ordering::SeqCst)); // the instrumental is being analyzed
    assert!(s.vm.is_separating(), "Cancel is still on screen");
    s.vm.cancel_separation();
    analysis.open();
    s.settle();

    assert_eq!(
        s.practice().accompaniment.as_ref().map(|a| a.id.clone()),
        backing
    );
    assert_eq!(s.practice().separation_source, None);
    assert!(!f.events().iter().any(|e| e == "save"));
    assert!(!s.vm.is_separating());
}

#[test]
fn once_the_backing_is_being_saved_cancel_is_no_longer_offered() {
    // Pressed during the commit save, Cancel was ignored: the overlay still
    // offered it, then the backing was replaced anyway.
    let gate = Gate::new();
    let reached = Arc::new(AtomicBool::new(false));
    let analysis = Gate::new();
    let (f, mut s) = open_signaling(seeded_with(Some(original())), &gate, &reached, &analysis);
    let save_gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&save_gate));
    s.act(|vm| vm.separate_backing("ref"));
    assert!(s.vm.can_cancel_separation());
    gate.open();
    s.pump_until(|_| reached.load(Ordering::SeqCst));
    analysis.open();
    s.pump_until(|_| f.repo.s().in_flight > 0);
    assert!(s.vm.is_separating());
    assert!(!s.vm.can_cancel_separation());
    f.repo.s().save_gate = None;
    save_gate.open();
    s.settle();
    assert!(s.practice().accompaniment.is_some());
}

// --- The selection and the lane through a failed save ----------------------------------------

#[test]
fn a_separation_whose_save_fails_leaves_the_selection_on_the_original() {
    // The separation flips the backing flag silently while it saves; that
    // save's selection check judged the backing lane by the instrumental's
    // slot (empty here) and moved the selection off the Original on screen,
    // and the failure path put only the flag back.
    let gate = Gate::new();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    s.act(|vm| vm.set_play_reference(true));
    s.act(|vm| vm.select_track("ref"));
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
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    s.act(|vm| vm.set_play_reference(true));
    s.act(|vm| vm.select_track("ref"));
    s.act(|vm| vm.separate_backing("ref"));
    // The failed save's revert re-reads the store: park it there.
    let reread = Gate::new();
    {
        let mut r = f.repo.s();
        r.fail_next_save = true;
        r.load_gate = Some(Arc::clone(&reread));
    }
    gate.open();
    s.pump_until(|_| f.repo.s().loads_parked == 1);
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
    let (_f, mut s) = open(seed_practice(None), &gate);
    let call = s.call(|vm| vm.add_accompaniment("/src/full-mix.wav"));
    s.await_reply(call);
    s.settle();
    let backing = s
        .practice()
        .accompaniment
        .as_ref()
        .map(|a| a.id.clone())
        .expect("the full mix");

    s.act(|vm| vm.mixer_do(|m, h| m.set_track_volume_live(h, &backing, 0.42)));
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_pan_live(h, &backing, -0.6)));
    s.act(|vm| vm.mixer_do(|m, h| m.toggle_mute(h, &backing)));
    s.settle();
    s.act(|vm| {
        vm.mixer_do(|m, h| {
            m.set_track_fx_live(
                h,
                &backing,
                TrackFx {
                    eq_on: true,
                    ..TrackFx::flat()
                },
            )
        })
    });
    s.act(|vm| vm.set_track_color(&backing, Some(0xFF00_FF00)));
    s.settle();

    s.act(|vm| vm.separate_backing(&backing));
    gate.open();
    s.settle();

    let reference = s.practice().reference.clone().expect("the Original");
    assert_eq!(reference.id, backing, "the same lane, re-kinded");
    assert!((reference.volume - 0.42).abs() < 1e-9);
    assert!((reference.pan - -0.6).abs() < 1e-9);
    assert!(reference.muted);
    assert_eq!(reference.fx.as_ref().map(|fx| fx.eq_on), Some(true));
    assert_eq!(reference.color_value, Some(0xFF00_FF00));
}

// --- The separated backing's import discipline -----------------------------------------------

// Regression guard: the separation success path imported the instrumental
// stem directly — skipping `ensure_streamable_wav` — so the separated backing
// (a 44.1 kHz sidecar WAV) bypassed the disk-streaming path and was fully
// re-decoded into RAM at every project open, exactly the failure mode the
// import discipline exists to prevent.
#[test]
fn the_separated_instrumental_goes_through_the_same_import_discipline_as_every_import() {
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    *guard(&f.edits.transcode_to) = Some("/edit/inst48k.wav".into());

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    assert_eq!(*guard(&f.edits.streamable_calls), ["/import/0.wav"]);
    assert_eq!(
        s.practice().accompaniment.as_ref().map(|a| a
            .clips
            .iter()
            .map(|c| c.audio_file_path.clone())
            .collect::<Vec<_>>()),
        Some(vec!["/edit/inst48k.wav".to_string()]),
        "the committed backing must be the streamable WAV"
    );
    assert!(
        f.events().contains(&"delete:/import/0.wav".to_string()),
        "the non-streamable import copy is redundant once transcoded"
    );
}

#[test]
fn a_failed_save_after_separation_deletes_the_transcoded_wav_and_the_import_copy() {
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    *guard(&f.edits.transcode_to) = Some("/edit/inst48k.wav".into());
    f.repo.s().fail_saves = true;

    s.act(|vm| vm.separate_backing("ref"));
    s.settle();

    let events = f.events();
    assert!(events.contains(&"delete:/edit/inst48k.wav".to_string()));
    assert!(events.contains(&"delete:/import/0.wav".to_string()));
    assert!(s.practice().accompaniment.is_none(), "nothing kept live");
}

#[test]
fn a_failed_save_after_separation_tells_the_engine_the_backing_toggle_came_back() {
    // The flow flips the toggle to the instrumental silently before its
    // save, and the save's failure path resyncs the engine under that flipped
    // flag. Restoring the flag silently afterwards left the toggle showing
    // Original while the engine played the accompaniment side and kept the
    // original muted.
    let gate = Gate::new();
    gate.open();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    s.act(|vm| vm.mixer_do(|m, h| m.set_play_reference(h, true))); // listening to the original
    let ref_clip = s.practice().reference.as_ref().expect("the Original").clips[0]
        .id
        .clone();
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

// --- A save failing beside another ------------------------------------------------------------

/// Runs the separation of `lane` with its save held, makes `edit` while it
/// is held, then lets the save fail.
fn fail_separation_around(
    f: &Fixture,
    s: &mut Screen,
    gate: &Gate,
    lane: &str,
    edit: impl FnOnce(&mut SongPracticeDetailViewModel) -> Vec<Effect>,
) {
    let save_gate = Gate::new();
    {
        let mut r = f.repo.s();
        r.save_gate = Some(Arc::clone(&save_gate));
        r.fail_at = vec![r.save_no + 1];
    }
    s.act(|vm| vm.separate_backing(lane));
    gate.open();
    s.pump_until(|_| f.repo.s().in_flight > 0);
    assert!(s.practice().accompaniment.is_some(), "its save runs");
    s.act(edit);
    f.repo.s().save_gate = None;
    save_gate.open();
    s.settle();
}

fn name_of(p: &SongPractice, id: &str) -> String {
    p.vocal_tracks
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.display_name.clone())
        .unwrap_or_default()
}

#[test]
fn a_failed_separation_of_the_original_keeps_an_edit_made_while_its_save_ran() {
    // The failure path put back the whole model from before the separation,
    // dropping a rename made while its save ran. Taken back after that save,
    // the rename's save queued behind it had already sent the separation.
    let gate = Gate::new();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    fail_separation_around(&f, &mut s, &gate, "ref", |vm| {
        vm.rename_track("single", "renamed")
    });

    assert!(s.practice().accompaniment.is_none());
    assert_eq!(name_of(s.practice(), "single"), "renamed");
    let stored = f.repo.stored();
    assert!(stored.accompaniment.is_none());
    assert_eq!(stored.separation_source, None);
    assert_eq!(name_of(&stored, "single"), "renamed");
}

#[test]
fn a_failed_separation_keeps_an_edit_to_the_original_lane_itself() {
    let gate = Gate::new();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    fail_separation_around(&f, &mut s, &gate, "ref", |vm| {
        vm.rename_track("ref", "My original")
    });

    let shown = s.practice().reference.clone().expect("the Original");
    assert_eq!(shown.display_name, "My original");
    let stored = f.repo.stored();
    assert_eq!(
        stored.reference.as_ref().map(|r| r.display_name.as_str()),
        Some("My original")
    );
    assert!(stored.accompaniment.is_none());
}

#[test]
fn a_failed_separation_puts_a_moved_original_back_in_the_backing_slot_edited() {
    // The ordinary first separation moves the full mix from the backing slot
    // to the reference one: taken back, it returns there.
    let gate = Gate::new();
    let (f, mut s) = open(seed_practice(None), &gate);
    let call = s.call(|vm| vm.add_accompaniment("/src/full-mix.wav"));
    s.await_reply(call);
    s.settle();
    let backing = s
        .practice()
        .accompaniment
        .as_ref()
        .map(|a| a.id.clone())
        .expect("the full mix");
    let renamed = backing.clone();
    fail_separation_around(&f, &mut s, &gate, &backing, move |vm| {
        vm.rename_track(&renamed, "Full mix")
    });

    for p in [s.practice().clone(), f.repo.stored()] {
        assert!(p.reference.is_none());
        let acc = p.accompaniment.as_ref().expect("the full mix");
        assert_eq!(acc.id, backing);
        assert_eq!(acc.kind, AudioTrackKind::Accompaniment);
        assert_eq!(acc.display_name, "Full mix");
    }
}

#[test]
fn a_taken_back_separation_leaves_the_engine_without_it() {
    // With no revert (the reload failed too), the take-back changed the
    // lanes after the failed save's resync: the engine kept the
    // instrumental, whose file the rollback then deleted.
    let gate = Gate::new();
    let (f, mut s) = open(seeded_with(Some(original())), &gate);
    {
        let mut r = f.repo.s();
        r.fail_at = vec![r.save_no + 1];
        r.fail_next_load = true;
    }
    s.act(|vm| vm.separate_backing("ref"));
    gate.open();
    s.settle();

    assert!(s.practice().accompaniment.is_none());
    assert!(!guard(&f.engine.last_paths).contains(&"/import/0.wav".to_string()));
}
