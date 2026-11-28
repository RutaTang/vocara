//! Port of `song_practice_structural_edit_test.dart` (the view model on the
//! in-memory store): imports, the structural-edit mutex, persist before
//! delete, split and merge, duplicates, separation, the remote-applied
//! reload, take numbering, the save chain and its failures. (Its separation
//! cases are `separation`'s; its sync-cursor cases, `lyrics_cursor`'s.)

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_services::audio_dsp::write_wav_mono;
use vocara_services::engine::media::MAX_PLAYBACK_DURATION;
use vocara_store::sync::sync_engine::RemoteApplied;

use super::*;

fn reference(id: &str, path: &str) -> AudioTrack {
    AudioTrack::new(id, AudioTrackKind::Reference, "Original", at()).with_audio(
        path,
        0,
        1000,
        Vec::new(),
    )
}

fn with_reference(path: &str, separation_source: Option<&str>) -> SongPractice {
    let mut p = seed_practice(None);
    p.reference = Some(Arc::new(reference("ref", path)));
    p.separation_source = separation_source.map(str::to_string);
    p
}

fn set_position(s: &mut Screen, ms: u64) {
    s.at_ms(ms);
}

fn note(id: &str, at_ms: i64, end: Option<i64>) -> PracticeNote {
    PracticeNote {
        id: id.into(),
        position_ms: at_ms,
        end_ms: end,
        content: id.into(),
        created_at: at(),
        updated_at: at(),
    }
}

fn marker(id: &str, at_ms: i64) -> TechniqueMarker {
    TechniqueMarker {
        id: id.into(),
        position_ms: at_ms,
        end_ms: None,
        technique_id: "t1".into(),
        technique_name: "Twang".into(),
        created_at: at(),
        updated_at: at(),
    }
}

fn paths(p: &SongPractice) -> Vec<String> {
    p.vocal_tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(|c| c.audio_file_path.clone()))
        .collect()
}

// --- Imports kept consistent ---------------------------------------------------------

#[test]
fn a_screen_already_gone_imports_nothing_more() {
    // A drop of several files carried on importing after Back.
    let (f, mut s) = Fixture::open(Setup::default());
    let before = f.repo.stored().vocal_tracks.len();
    s.vm.dispose();
    let a = s.call(|vm| vm.add_vocal_take("/src/late.wav"));
    let b = s.call(|vm| vm.add_accompaniment("/src/late.wav"));
    let c = s.call(|vm| vm.add_reference("/src/late.wav"));
    let d = s.call(|vm| vm.add_clip_to_take("single", "/src/late.wav", None));
    s.settle();
    for call in [a, b, c] {
        assert_eq!(s.reply(call), Some(&Reply::Done(call)));
    }
    assert_eq!(
        s.reply(d),
        Some(&Reply::ClipAdded {
            call: d,
            end_ms: None
        })
    );
    assert_eq!(f.files.imports.load(Ordering::SeqCst), 0);
    assert_eq!(f.repo.stored().vocal_tracks.len(), before);
}

#[test]
fn an_imported_take_leaves_the_selection_recs_target_alone() {
    // Selected, a guide vocal imported to sing along with became the take
    // the next recording landed on, muted for the whole of it.
    let (_f, mut s) = Fixture::open(Setup::default());
    let before = s.vm.selected_track_id().map(str::to_string);
    let target = s.vm.record_target_id();
    let call = s.call(|vm| vm.add_vocal_take("/src/guide.wav"));
    s.await_reply(call);
    s.settle();
    assert_eq!(s.vm.selected_track_id().map(str::to_string), before);
    assert_eq!(s.vm.record_target_id(), target);
}

#[cfg(unix)]
#[test]
fn a_lyrics_file_that_is_a_pipe_or_a_device_is_refused_at_once() {
    // Their length reads 0, so the size cap passed and the read went on
    // without end: a pipe held the import open, /dev/zero ate memory.
    let dir = tempfile::tempdir().expect("a temp dir");
    let pipe = dir.path().join("pipe.lrc");
    let made = std::process::Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    let zero = dir.path().join("zero.lrc");
    std::os::unix::fs::symlink("/dev/zero", &zero).expect("a link");
    let (_f, mut s) = Fixture::open(Setup::default());
    let before = s.vm.lyrics().to_vec();
    for path in [&pipe, &zero] {
        let call = s.call(|vm| vm.import_lyrics_from_file(&path.to_string_lossy()));
        s.await_reply(call);
    }
    assert_eq!(s.toasts.len(), 2);
    assert_eq!(s.vm.lyrics(), before.as_slice());
}

#[test]
fn lyrics_that_repeat_their_lines_past_reason_are_refused_not_expanded() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let crafted = format!("{}{}", "[0:00]".repeat(40000), "x".repeat(100000));
    let call = s.call(|vm| vm.apply_edited_lyrics(&crafted));
    assert!(!s.landed(call));
    assert_eq!(s.toasts.len(), 1);
    assert!(s.toasts[0].contains("too many times"), "{:?}", s.toasts);
}

#[test]
fn a_file_just_over_the_length_limit_says_so() {
    // Rounded down, a file just over the limit read as exactly it.
    let (_f, mut s) = Fixture::open(Setup {
        probe: Some(Arc::new(|_: &str| {
            Ok(Some(MAX_PLAYBACK_DURATION + Duration::from_millis(400)))
        })),
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_vocal_take("/src/long.wav"));
    s.await_reply(call);
    let over = MAX_PLAYBACK_DURATION.as_secs() / 60 + 1;
    assert_eq!(s.toasts.len(), 1);
    assert!(s.toasts[0].starts_with(&format!("That file is {over} minutes long.")));
}

/// The length check reads the file (an iCloud placeholder blocks until it
/// downloads): it runs on the import's worker, so the call that starts the
/// import returns at once.
#[test]
fn the_length_check_runs_off_the_ui_thread() {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let (_f, mut s) = Fixture::open(Setup {
        probe: Some(Arc::new(move |_: &str| {
            let _ = guard(&wait).recv_timeout(Duration::from_secs(10));
            Ok(Some(MAX_PLAYBACK_DURATION + Duration::from_secs(60)))
        })),
        ..Setup::default()
    });
    let started = std::time::Instant::now();
    let call = s.call(|vm| vm.add_vocal_take("/src/placeholder.wav"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the call waited on the probe"
    );
    release.send(()).expect("the probe waits");
    s.await_reply(call);
    assert_eq!(s.toasts.len(), 1);
    assert!(s.toasts[0].contains("minutes long"), "{:?}", s.toasts);
}

#[test]
fn a_file_that_cannot_be_read_says_so() {
    let (f, mut s) = Fixture::open(Setup::default());
    *guard(&f.files.fail_next_read) = true;
    let call = s.call(|vm| vm.add_vocal_take("/src/gone.wav"));
    s.await_reply(call);
    assert_eq!(s.toasts, ["The file couldn't be read."]);
}

#[test]
fn a_failed_write_does_not_blame_the_file() {
    // Every file-system error read as the source's: a full disk too.
    let dir = tempfile::tempdir().expect("a temp dir");
    let source = dir.path().join("take.wav");
    std::fs::write(&source, [1u8, 2]).expect("a source");
    let (f, mut s) = Fixture::open(Setup::default());
    *guard(&f.files.fail_next_read) = true;
    let call = s.call(|vm| vm.add_vocal_take(&source.to_string_lossy()));
    s.await_reply(call);
    assert_eq!(s.toasts.len(), 1);
    assert_ne!(s.toasts[0], "The file couldn't be read.");
}

// --- Parallel paths kept consistent --------------------------------------------------

#[test]
fn a_failed_lyric_write_takes_the_engine_back_with_the_model() {
    // An import's save failed while a lyric stamp had moved the model on, so
    // it was not reverted; the stamp's failure then reverted the model to the
    // disk's, without the import's lane, but the engine kept playing it.
    let (f, mut s) = Fixture::open(Setup::default());
    let gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&gate));
    let importing = s.call(|vm| vm.add_vocal_take("/src/new.wav"));
    s.pump_until(|_| f.repo.s().in_flight > 0);
    s.act(|vm| vm.stamp_next_lyric());
    {
        let mut r = f.repo.s();
        r.fail_saves = true;
        r.save_gate = None;
    }
    gate.open();
    s.await_reply(importing);
    s.settle();
    assert!(!paths(s.practice()).contains(&"/import/0.wav".to_string()));
    assert!(!guard(&f.engine.last_paths).contains(&"/import/0.wav".to_string()));
}

#[test]
fn deleting_the_originals_clip_removes_the_original() {
    // It left an empty backing lane nothing could fill, its kept original
    // still set.
    let (_f, mut s) = Fixture::open(Setup {
        practice: with_reference("/pool/orig.wav", Some("/pool/orig.wav")),
        ..Setup::default()
    });
    let clip_id = s.track("ref").clips[0].id.clone();
    s.act(|vm| vm.delete_clip("ref", &clip_id));
    s.settle();
    assert!(s.practice().reference.is_none());
    assert!(s.practice().separation_source.is_none());
}

#[test]
fn a_new_backing_keeps_the_file_the_original_still_plays() {
    // The kept original IS the Original lane's file: deleted with the
    // replaced backing, only the pool's own refusal had saved it.
    let (f, mut s) = Fixture::open(Setup {
        practice: with_reference("/work/orig.wav", Some("/work/orig.wav")),
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_accompaniment("/src/backing.wav"));
    s.await_reply(call);
    s.settle();
    assert_eq!(
        s.practice().reference.as_ref().expect("the original").clips[0].audio_file_path,
        "/work/orig.wav"
    );
    assert!(!f.events().contains(&"delete:/work/orig.wav".to_string()));
}

#[test]
fn a_failed_empty_take_gives_the_selection_back() {
    // It fell to the first lane, losing the selected bus, unlike a REC start
    // that drops its empty take.
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.mixer_do(|m, h| m.create_bus(h, "")));
    s.settle();
    let bus = s.vm.selected_bus_id().map(str::to_string);
    assert!(bus.is_some());
    f.repo.s().fail_next_save = true;
    let takes = s.practice().vocal_tracks.len();
    s.vm.create_empty_vocal_take(super::super::edits::EmptyTakeFor::NewTake);
    let fx = s.vm.finish();
    s.run(fx);
    s.settle();
    assert_eq!(s.practice().vocal_tracks.len(), takes, "reverted");
    assert_eq!(s.vm.selected_bus_id().map(str::to_string), bus);
}

#[test]
fn lyrics_dropped_during_an_edit_are_taken_as_pasted_ones_are() {
    let (f, mut s) = Fixture::open(Setup::default());
    let gate = Gate::new();
    *guard(&f.edits.duplicate_gate) = Some(Arc::clone(&gate));
    s.act(|vm| vm.duplicate_track("single"));
    assert!(s.vm.busy());
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("song.txt");
    std::fs::write(&file, "first line\nsecond line").expect("lyrics");
    let call = s.call(|vm| vm.import_lyrics_from_file(&file.to_string_lossy()));
    s.await_reply(call);
    let words: Vec<&str> = s.vm.lyrics().iter().map(|l| l.text.as_str()).collect();
    assert_eq!(words, ["first line", "second line"]);
    gate.open();
    s.settle();
}

// --- Import transcode ----------------------------------------------------------------

#[test]
fn a_compressed_import_is_transcoded_once_and_the_compressed_copy_deleted() {
    let (f, mut s) = Fixture::open(Setup::default());
    *guard(&f.edits.transcode_to) = Some("/edit/transcoded.wav".into());
    let call = s.call(|vm| vm.add_vocal_take("/src/song.mp3"));
    s.await_reply(call);
    assert_eq!(*guard(&f.edits.streamable_calls), ["/import/0.wav"]);
    // The compressed copy is redundant once the WAV is the track.
    assert!(f.events().contains(&"delete:/import/0.wav".to_string()));
    let added = s.practice().vocal_tracks.last().cloned().expect("a take");
    assert_eq!(added.clips[0].audio_file_path, "/edit/transcoded.wav");
}

#[test]
fn an_already_wav_import_passes_through_untouched() {
    let (f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.add_vocal_take("/src/take.wav"));
    s.await_reply(call);
    assert_eq!(*guard(&f.edits.streamable_calls), ["/import/0.wav"]);
    assert!(!f.events().iter().any(|e| e.starts_with("delete:")));
    let added = s.practice().vocal_tracks.last().cloned().expect("a take");
    assert_eq!(added.clips[0].audio_file_path, "/import/0.wav");
}

#[test]
fn a_failed_transcode_rolls_the_import_back_like_any_failed_step() {
    let (f, mut s) = Fixture::open(Setup::default());
    *guard(&f.edits.fail_transcode) = true;
    let before = s.practice().vocal_tracks.len();
    let call = s.call(|vm| vm.add_vocal_take("/src/song.mp3"));
    s.await_reply(call);
    assert!(f.events().contains(&"delete:/import/0.wav".to_string()));
    assert_eq!(s.practice().vocal_tracks.len(), before);
    assert!(s.vm.error_message().is_some());
}

#[test]
fn a_failed_step_after_the_import_deletes_the_sandbox_copy() {
    // The import succeeded, the waveform analysis throws: the copy used to
    // stay while nothing referenced it.
    let (f, mut s) = Fixture::open(Setup {
        analyze: throwing_analyze(),
        ..Setup::default()
    });
    let before = s.practice().vocal_tracks.len();
    let call = s.call(|vm| vm.add_vocal_take("/src/a.wav"));
    s.await_reply(call);
    assert!(f.events().contains(&"delete:/import/0.wav".to_string()));
    assert_eq!(s.practice().vocal_tracks.len(), before, "nothing lands");
    assert!(s.vm.error_message().is_some(), "the failure is surfaced");
    assert_eq!(s.toasts, ["Something went wrong."]);
}

// --- The structural-edit mutex -------------------------------------------------------

#[test]
fn an_import_over_the_playback_cap_is_refused_before_anything_is_copied() {
    let (f, mut s) = Fixture::open(Setup {
        probe: Some(Arc::new(|_: &str| Ok(Some(Duration::from_secs(2 * 3600))))),
        ..Setup::default()
    });
    let before = s.practice().vocal_tracks.len();
    let first = s.practice().vocal_tracks[0].id.clone();
    // One after another, as a drop of several files imports them (the
    // check runs on the import's worker, under the edit gate).
    let a = s.call(|vm| vm.add_vocal_take("/src/very_long.mp3"));
    s.await_reply(a);
    let b = s.call(|vm| vm.add_accompaniment("/src/very_long.mp3"));
    s.await_reply(b);
    let c = s.call(|vm| vm.add_clip_to_take(&first, "/src/x.mp3", None));
    s.await_reply(c);
    // Refused at the admission gate: no copy, no analysis, no save.
    assert!(f.events().is_empty());
    assert_eq!(s.practice().vocal_tracks.len(), before);
    assert!(s.practice().accompaniment.is_none());
    assert_eq!(s.toasts.len(), 3);
    assert!(s.toasts[0].contains("limit is 60 minutes"));
}

#[test]
fn a_probe_that_cannot_run_admits_the_import() {
    let (_f, mut s) = Fixture::open(Setup {
        probe: Some(Arc::new(|_: &str| panic!("no ffi"))),
        ..Setup::default()
    });
    let before = s.practice().vocal_tracks.len();
    let call = s.call(|vm| vm.add_vocal_take("/src/a.wav"));
    s.await_reply(call);
    assert_eq!(s.practice().vocal_tracks.len(), before + 1);
}

#[test]
fn an_overlapping_structural_action_is_rejected_while_the_gate_is_held() {
    let (f, mut s) = Fixture::open(Setup::default());
    let before = s.practice().vocal_tracks.len();
    let a = s.call(|vm| vm.add_vocal_take("/src/a.wav")); // holds the gate
    let b = s.call(|vm| vm.add_vocal_take("/src/b.wav")); // sees busy → no-op
    s.await_reply(a);
    s.await_reply(b);
    s.settle();
    assert_eq!(s.practice().vocal_tracks.len(), before + 1);
    assert_eq!(f.events().iter().filter(|e| *e == "save").count(), 1);
}

#[test]
fn a_split_whose_save_fails_keeps_the_halves_the_model_still_plays() {
    // A failed save does not revert a model an edit moved on since: the
    // split halves stayed in it, their files were deleted anyway, and the
    // rename's own save then failed on them.
    let (f, mut s) = Fixture::open(Setup::default());
    set_position(&mut s, 500);
    let gate = Gate::new();
    {
        let mut r = f.repo.s();
        r.save_gate = Some(Arc::clone(&gate));
        r.fail_next_save = true;
    }
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.pump_until(|_| f.repo.s().in_flight > 0);
    s.act(|vm| vm.rename_track("multi", "Renamed")); // moves the model on
    f.repo.s().save_gate = None;
    gate.open();
    s.settle();
    assert!(
        !f.events().iter().any(|e| e.starts_with("delete:/edit/")),
        "the halves are still in the model"
    );
    let stored = paths(&f.repo.stored());
    assert!(stored.contains(&"/edit/first.wav".to_string()));
    assert!(stored.contains(&"/edit/second.wav".to_string()));
}

#[test]
fn deleting_the_original_lets_go_of_the_original_kept_for_re_extraction() {
    let mut p = seed_practice(None);
    p.reference = Some(Arc::new(reference("orig", "/pool/orig.wav")));
    p.separation_source = Some("/audio/p1/source.wav".into());
    let (f, mut s) = Fixture::open(Setup {
        practice: p,
        ..Setup::default()
    });
    s.act(|vm| vm.delete_track("orig"));
    s.settle();
    assert!(f.repo.stored().reference.is_none());
    assert!(f.repo.stored().separation_source.is_none());
    assert!(f
        .events()
        .contains(&"delete:/audio/p1/source.wav".to_string()));
}

#[test]
fn rec_pressed_while_an_edit_runs_says_why_it_does_nothing() {
    // Every structural edit refused in that window said so; REC alone was
    // silent, and read as a broken button.
    let gate = Gate::new();
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    assert!(s.vm.busy(), "the merge holds the busy gate");
    s.act(|vm| vm.recording_do(|r, h| r.start_recording(h)));
    assert_eq!(s.toasts, ["Wait for the current edit to finish."]);
    assert!(!s.vm.recording().is_starting_take());
    gate.open();
    s.settle();
}

// --- An editor opened before a split or merge --------------------------------------

#[test]
fn an_editor_still_edits_and_deletes_the_moved_note_after_a_merge() {
    let mut takes = mergeable_takes();
    takes[1].notes = vec![note("n1", 100, None), note("n2", 300, None)];
    takes[1].technique_markers = vec![marker("m1", 200)];
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(takes)),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    s.act(|vm| vm.update_note_content("bottom", "n1", "sharp here"));
    s.act(|vm| vm.delete_note("bottom", "n2"));
    s.act(|vm| vm.delete_technique_marker("bottom", "m1"));
    s.settle();
    let stored = f.repo.stored();
    assert_eq!(stored.vocal_tracks.len(), 1);
    let merged = &stored.vocal_tracks[0];
    let contents: Vec<&str> = merged.notes.iter().map(|n| n.content.as_str()).collect();
    assert_eq!(contents, ["sharp here"]);
    assert!(merged.technique_markers.is_empty());
}

#[test]
fn an_editor_still_edits_the_moved_note_after_a_split() {
    let mut single = take("single", "Take 1", "/pool/take1.wav");
    single.notes = vec![note("n1", 100, None), note("n2", 800, None)];
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![single])),
        ..Setup::default()
    });
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    s.act(|vm| vm.update_note_content("single", "n2", "sharp here"));
    s.act(|vm| vm.update_note_content("single", "n1", "held"));
    s.settle();
    let stored = f.repo.stored();
    assert_eq!(stored.vocal_tracks.len(), 2);
    assert_eq!(stored.vocal_tracks[0].notes[0].content, "held");
    assert_eq!(stored.vocal_tracks[1].notes[0].content, "sharp here");
}

#[test]
fn deleting_a_clip_keeps_the_notes_another_clip_still_covers() {
    let lane = AudioTrack {
        clips: vec![
            clip("orig", "/pool/o.wav", 0, 3000),
            clip("punch", "/pool/p.wav", 1000, 3000),
        ],
        notes: vec![
            note("under-both", 1500, None),
            note("punch-only", 3500, None),
            note("range", 2000, Some(3800)),
        ],
        ..AudioTrack::new("lane", AudioTrackKind::Vocal, "Take 1", at())
    };
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![lane])),
        ..Setup::default()
    });
    s.act(|vm| vm.delete_clip("lane", "punch"));
    s.settle();
    let lane = s.track("lane");
    // 1000..3000 is still the original's; only 3000..4000 falls silent.
    let ids: Vec<&str> = lane.notes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["under-both", "range"]);
    assert_eq!(lane.notes[1].end_ms, Some(3000), "clamped at the gap");
}

#[test]
fn a_moved_clip_takes_the_notes_and_markers_on_its_audio_with_it() {
    // The clip moved and its notes stayed where they were, off its audio.
    let lane = AudioTrack {
        clips: vec![
            clip("orig", "/pool/o.wav", 0, 3000),
            clip("punch", "/pool/p.wav", 1000, 1000),
        ],
        notes: vec![
            note("before", 500, None),
            // Under the punch-in: heard is the punch, so it rides it.
            note("on-punch", 1500, None),
            // Starts on the punch, ends on the original's audio after it:
            // decided by its start.
            note("from-punch", 1800, Some(2600)),
            note("after", 2500, None),
        ],
        technique_markers: vec![marker("m-punch", 1200), marker("m-orig", 2200)],
        ..AudioTrack::new("lane", AudioTrackKind::Vocal, "Take 1", at())
    };
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![lane])),
        ..Setup::default()
    });
    // The drag passes over "after" on its way and back: it stays put.
    s.vm.move_clip_start_live("lane", "punch", 2000);
    s.vm.move_clip_start_live("lane", "punch", 1300);
    let live = s.track("lane");
    let at_of = |id: &str| {
        live.notes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.position_ms)
    };
    assert_eq!(
        at_of("on-punch"),
        Some(1800),
        "the live drag shows it moving"
    );
    assert_eq!(at_of("after"), Some(2500));
    s.act(|vm| vm.commit_track_move());
    s.settle();
    let stored = f.repo.stored();
    let lane = stored
        .vocal_tracks
        .iter()
        .find(|t| t.id == "lane")
        .expect("the lane");
    let notes: Vec<(&str, i64, Option<i64>)> = lane
        .notes
        .iter()
        .map(|n| (n.id.as_str(), n.position_ms, n.end_ms))
        .collect();
    assert_eq!(
        notes,
        [
            ("before", 500, None),
            ("on-punch", 1800, None),
            ("from-punch", 2100, Some(2900)),
            ("after", 2500, None),
        ]
    );
    let markers: Vec<(&str, i64)> = lane
        .technique_markers
        .iter()
        .map(|m| (m.id.as_str(), m.position_ms))
        .collect();
    assert_eq!(markers, [("m-punch", 1500), ("m-orig", 2200)]);
    // The next drag starts from where this one left everything.
    s.vm.move_clip_start_live("lane", "punch", 1400);
    s.act(|vm| vm.commit_track_move());
    s.settle();
    assert_eq!(s.track("lane").notes[1].position_ms, 1900);
}

// --- Persist before delete -----------------------------------------------------------

#[test]
fn delete_clip_saves_first_and_deletes_the_clip_audio_only_after() {
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.delete_clip("multi", "clip-a"));
    s.settle();
    assert_eq!(f.events(), ["save", "delete:/pool/clipA.wav"]);
}

#[test]
fn delete_clip_on_a_failed_save_deletes_nothing_and_reverts() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.delete_clip("multi", "clip-a"));
    s.settle();
    assert_eq!(f.events(), ["save-failed"]);
    assert_eq!(s.track("multi").clips.len(), 2, "reverted to the disk");
}

#[test]
fn delete_track_saves_first_and_deletes_every_clip_file_only_after() {
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.delete_track("multi"));
    s.settle();
    assert_eq!(
        f.events(),
        ["save", "delete:/pool/clipA.wav", "delete:/pool/clipB.wav"]
    );
}

#[test]
fn delete_track_on_a_failed_save_keeps_every_clip_file() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.delete_track("multi"));
    s.settle();
    assert_eq!(f.events(), ["save-failed"]);
    assert!(s.vm.track_by_id("multi").is_some());
}

#[test]
fn split_saves_first_then_deletes_only_the_original_file() {
    let (f, mut s) = Fixture::open(Setup::default());
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    assert_eq!(f.events(), ["save", "delete:/pool/take1.wav"]);
    assert_eq!(s.toasts, ["Take split."]);
}

#[test]
fn split_on_a_failed_save_keeps_the_original_and_cleans_up_the_halves() {
    let (f, mut s) = Fixture::open(Setup::default());
    set_position(&mut s, 500);
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    assert_eq!(
        f.events(),
        [
            "save-failed",
            "delete:/edit/first.wav",
            "delete:/edit/second.wav"
        ]
    );
    assert_eq!(
        s.track("single").clips[0].audio_file_path,
        "/pool/take1.wav"
    );
}

#[test]
fn merge_saves_first_then_deletes_only_the_two_source_files() {
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    assert_eq!(
        f.events(),
        ["save", "delete:/pool/top.wav", "delete:/pool/bottom.wav"]
    );
    assert_eq!(s.toasts, ["Takes merged."]);
}

#[test]
fn a_selected_bottom_take_follows_its_audio_into_the_merged_lane() {
    // ... and back out of it when the save fails.
    for fail in [false, true] {
        let (f, mut s) = Fixture::open(Setup {
            practice: seed_practice(Some(mergeable_takes())),
            ..Setup::default()
        });
        let bottom_clip = s.practice().vocal_tracks[1].clips[0].id.clone();
        s.act(|vm| vm.select_clip("bottom", &bottom_clip));
        assert_eq!(s.vm.record_target_id().as_deref(), Some("bottom"));
        f.repo.s().fail_saves = fail;
        s.act(|vm| vm.merge_track_down("top"));
        s.settle();
        if fail {
            assert_eq!(s.vm.selected_track_id(), Some("bottom"));
            assert_eq!(s.vm.selected_clip_id(), Some(bottom_clip.as_str()));
        } else {
            assert_eq!(s.vm.selected_track_id(), Some("top"), "not the backing");
            let tracks = &s.practice().vocal_tracks;
            assert_eq!(tracks.len(), 1);
            assert_eq!(tracks[0].clips.len(), 1);
            let merged = tracks[0].clips[0].id.clone();
            assert_eq!(s.vm.selected_clip_id(), Some(merged.as_str()));
            assert_eq!(s.vm.record_target_id().as_deref(), Some("top"));
        }
        f.repo.s().fail_saves = false;
    }
}

#[test]
fn merge_on_a_failed_save_keeps_the_sources_and_cleans_up_the_output() {
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        ..Setup::default()
    });
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    assert_eq!(f.events(), ["save-failed", "delete:/edit/merged.wav"]);
    assert_eq!(s.practice().vocal_tracks.len(), 2);
}

#[test]
fn a_split_or_merge_whose_analysis_fails_deletes_the_new_files() {
    let (f, mut s) = Fixture::open(Setup {
        analyze: throwing_analyze(),
        ..Setup::default()
    });
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    assert_eq!(
        f.events(),
        ["delete:/edit/first.wav", "delete:/edit/second.wav"]
    );
    assert!(!s.vm.busy());
    assert_eq!(s.toasts, ["Something went wrong."]);

    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        analyze: throwing_analyze(),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    assert_eq!(f.events(), ["delete:/edit/merged.wav"]);
    assert!(!s.vm.busy());
}

#[test]
fn split_puts_the_surviving_half_in_a_new_clip_row_selection_following() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let original = s.track("single").clips[0].id.clone();
    s.act(|vm| vm.select_clip("single", &original));
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    let first = s.track("single").clips[0].id.clone();
    assert_ne!(first, original, "a fresh clip id");
    assert_eq!(s.vm.selected_clip_id(), Some(first.as_str()));
}

#[test]
fn the_bottom_takes_notes_follow_its_audio_in_the_joined_file() {
    // The shift was the top clip's stored length, which a clip measured by an
    // earlier build holds up to 10 ms short of its audio.
    let dir = tempfile::tempdir().expect("a temp dir");
    let top_wav = dir.path().join("top.wav");
    let silence = vec![0.0f64; 59232]; // 1234 ms at 48 kHz
    std::fs::write(&top_wav, write_wav_mono(&silence, 48000)).expect("the top take");
    let mut takes = mergeable_takes();
    takes[0].clips[0].audio_file_path = top_wav.to_string_lossy().into_owned();
    takes[0].clips[0].duration_ms = 1230;
    takes[1].notes = vec![note("n", 100, None)];
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(takes)),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    let merged = &s.practice().vocal_tracks;
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].notes.len(), 1);
    assert_eq!(merged[0].notes[0].position_ms, 1334);
}

#[test]
fn merge_puts_the_merged_audio_in_a_new_clip_row_selection_following() {
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        ..Setup::default()
    });
    let top_clip = s.track("top").clips[0].id.clone();
    s.act(|vm| vm.select_clip("top", &top_clip));
    s.act(|vm| vm.merge_track_down("top"));
    s.settle();
    let merged = s.track("top").clips[0].clone();
    assert_ne!(merged.id, top_clip);
    assert_eq!(merged.audio_file_path, "/edit/merged.wav");
    assert_eq!(s.vm.selected_clip_id(), Some(merged.id.as_str()));
}

#[test]
fn a_rename_and_a_note_landing_during_a_split_survive_it() {
    let gate = Gate::new();
    let (f, mut s) = Fixture::open(Setup {
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.act(|vm| vm.rename_track("single", "Renamed"));
    s.act(|vm| vm.add_note("single", 100, None, "mid-split"));
    s.pump_until(|_| f.repo.s().save_no >= 2);
    gate.open();
    s.settle();
    let first = s.track("single");
    assert_eq!(first.display_name, "Renamed");
    assert_eq!(first.notes.len(), 1);
    assert_eq!(first.notes[0].content, "mid-split");
}

#[test]
fn a_notes_text_saved_from_the_copy_its_editor_opened_with_keeps_the_clamp_a_split_gave_it() {
    let (_f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.add_note("single", 100, Some(900), "range"));
    s.settle();
    let opened = s.track("single").notes[0].clone(); // what the editor holds
    set_position(&mut s, 500);
    s.act(|vm| vm.split_track_at_playhead("single"));
    s.settle();
    let notes = s.track("single").notes.clone();
    assert_eq!(notes.len(), 1);
    let clamped = notes[0].clone();
    assert_ne!(clamped.end_ms, Some(900), "precondition: split clamped");

    s.act(|vm| vm.update_note_content("single", &opened.id, "edited"));
    s.settle();
    let notes = s.track("single").notes.clone();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].content, "edited");
    assert_eq!(notes[0].end_ms, clamped.end_ms);
    assert_eq!(notes[0].position_ms, clamped.position_ms);
}

#[test]
fn a_rename_and_a_note_landing_during_a_merge_survive_it() {
    let gate = Gate::new();
    let (f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(mergeable_takes())),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    s.act(|vm| vm.merge_track_down("top"));
    s.act(|vm| vm.rename_track("top", "Renamed"));
    s.act(|vm| vm.add_note("bottom", 100, None, "on the bottom"));
    s.pump_until(|_| f.repo.s().save_no >= 2);
    gate.open();
    s.settle();
    let merged = s.track("top");
    assert_eq!(merged.display_name, "Renamed");
    assert_eq!(merged.notes.len(), 1);
    assert_eq!(merged.notes[0].content, "on the bottom");
    assert_eq!(
        merged.notes[0].position_ms, 1100,
        "shifted onto the joined audio"
    );
}

// --- Duplicates and imports on failed saves -----------------------------------------

#[test]
fn add_vocal_take_on_a_failed_save_deletes_the_imported_file() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    let call = s.call(|vm| vm.add_vocal_take("/src/a.wav"));
    s.await_reply(call);
    s.settle();
    assert_eq!(f.events(), ["save-failed", "delete:/import/0.wav"]);
}

#[test]
fn add_accompaniment_deletes_the_replaced_backing_only_after_the_save() {
    let mut p = seed_practice(None);
    p.accompaniment = Some(Arc::new(
        AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Old", at()).with_audio(
            "/pool/old.wav",
            0,
            1000,
            Vec::new(),
        ),
    ));
    let (f, mut s) = Fixture::open(Setup {
        practice: p,
        ..Setup::default()
    });
    let call = s.call(|vm| vm.add_accompaniment("/src/new.wav"));
    s.await_reply(call);
    assert_eq!(f.events(), ["save", "delete:/pool/old.wav"]);

    f.clear_events();
    f.repo.s().fail_saves = true;
    let call = s.call(|vm| vm.add_accompaniment("/src/newer.wav"));
    s.await_reply(call);
    assert_eq!(f.events(), ["save-failed", "delete:/import/1.wav"]);
}

#[test]
fn duplicate_track_on_a_failed_save_deletes_the_copied_files() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.duplicate_track("single"));
    s.settle();
    assert!(f.events().contains(&"delete:/edit/dup0.wav".to_string()));
    let ids: Vec<&str> = s
        .practice()
        .vocal_tracks
        .iter()
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(ids, ["single", "multi"]);
}

#[test]
fn duplicate_track_gives_the_copy_fresh_clip_note_and_marker_ids() {
    let mut single = take("single", "Take 1", "/pool/take1.wav");
    single.notes = vec![note("n1", 100, None)];
    single.technique_markers = vec![marker("m1", 200)];
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![single])),
        ..Setup::default()
    });
    s.act(|vm| vm.duplicate_track("single"));
    s.settle();
    assert_eq!(s.toasts, ["Take duplicated."]);
    let tracks = s.practice().vocal_tracks.clone();
    assert_eq!(tracks.len(), 2);
    let copy = &tracks[1];
    assert_eq!(copy.display_name, "Take 1 copy");
    assert_ne!(copy.clips[0].id, tracks[0].clips[0].id);
    assert_eq!(copy.clips[0].audio_file_path, "/edit/dup0.wav");
    assert_ne!(copy.notes[0].id, "n1");
    assert_ne!(copy.technique_markers[0].id, "m1");
}

#[test]
fn a_duplicate_is_of_the_take_as_it_is_once_the_files_are_copied() {
    // Built from the snapshot taken before the copies, it missed a rename and
    // a clip drag made while they ran.
    let (f, mut s) = Fixture::open(Setup::default());
    let gate = Gate::new();
    *guard(&f.edits.duplicate_gate) = Some(Arc::clone(&gate));
    s.act(|vm| vm.duplicate_track("multi"));
    s.act(|vm| vm.rename_track("multi", "Verse"));
    s.vm.move_clip_start_live("multi", "clip-b", 700);
    s.act(|vm| vm.commit_track_move());
    // Dart awaited both saves before the copies ran.
    s.pump_until(|_| f.repo.s().save_no >= 2);
    gate.open();
    s.settle();
    let copy = s.practice().vocal_tracks.last().cloned().expect("the copy");
    assert_eq!(copy.display_name, "Verse copy");
    let starts: Vec<i64> = copy.clips.iter().map(|c| c.start_ms).collect();
    assert_eq!(starts, [0, 700]);
}

#[test]
fn duplicate_track_that_fails_part_way_discards_the_copies_that_landed() {
    let (f, mut s) = Fixture::open(Setup::default());
    *guard(&f.edits.fail_duplicate_at) = Some(1);
    s.act(|vm| vm.duplicate_track("multi"));
    s.settle();
    assert_eq!(f.events(), ["delete:/edit/dup0.wav"]);
    assert!(!s.vm.busy());
    assert_eq!(s.practice().vocal_tracks.len(), 2);
    assert_eq!(s.toasts, ["Something went wrong."]);
}

// --- The remote-applied reload -------------------------------------------------------

fn remote(id: &str) -> RemoteApplied {
    RemoteApplied {
        rows: Vec::new(),
        practice_ids: [id.to_string()].into_iter().collect(),
        technique_ids: Default::default(),
    }
}

#[test]
fn a_take_that_arrived_from_another_device_is_inside_the_timeline() {
    let (f, mut s) = Fixture::open(Setup::default());
    let mut stored = f.repo.stored();
    let mut far = take("far", "Far", "/pool/far.wav");
    far.clips[0].start_ms = 60_000;
    stored.vocal_tracks.push(Arc::new(far));
    f.repo.s().stored = Some(stored);
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();
    assert!(s.vm.track_by_id("far").is_some());
    assert!(s.practice().timeline_duration_ms >= 61_000);
}

#[test]
fn a_failed_save_keeps_the_timeline_over_every_lane() {
    // A failed save reverts to the stored aggregate, whose timeline need not
    // cover its takes (one recovered or synced past it): grown on load, it
    // shrank back on the revert and cut the take off at the right edge.
    let (f, mut s) = Fixture::open(Setup::default());
    let mut stored = f.repo.stored();
    let mut late = take("late", "Take 3 (recovered)", "/pool/late.wav");
    late.clips[0].start_ms = 60_000;
    late.clips[0].duration_ms = 5000;
    stored.timeline_duration_ms = 1500;
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
fn a_reload_for_another_practice_reads_nothing() {
    let (f, mut s) = Fixture::open(Setup::default());
    let loads = f.repo.s().loads;
    s.act(|vm| vm.on_remote_applied(&remote("other")));
    s.settle();
    assert_eq!(f.repo.s().loads, loads);
}

#[test]
fn a_live_fader_edit_made_while_the_reload_reads_survives_it() {
    let (f, mut s) = Fixture::open(Setup::default());
    let gate = Gate::new();
    f.repo.s().load_gate = Some(Arc::clone(&gate));
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_volume_live(h, "single", 0.25)));
    f.repo.s().load_gate = None;
    gate.open();
    s.settle();
    assert_eq!(s.track("single").volume, 0.25);
}

#[test]
fn a_fader_held_still_when_a_reload_arrives_keeps_its_value() {
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.mixer_do(|m, h| m.set_track_volume_live(h, "single", 0.4)));
    let loads = f.repo.s().loads;
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();
    assert_eq!(
        f.repo.s().loads,
        loads,
        "an unsaved change skips the reload"
    );
    assert_eq!(s.track("single").volume, 0.4);
}

#[test]
fn a_reload_after_a_failed_saves_revert_shows_the_store() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_next_save = true;
    s.act(|vm| vm.rename_track("single", "Lost"));
    s.settle();
    assert_eq!(s.track("single").display_name, "Take 1");
    let mut stored = f.repo.stored();
    stored.title = "Retitled elsewhere".into();
    f.repo.s().stored = Some(stored);
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();
    assert_eq!(s.practice().title, "Retitled elsewhere");
}

#[test]
fn and_a_reload_after_the_release_shows_the_store() {
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

// --- Take numbering ------------------------------------------------------------------

#[test]
fn a_new_take_numbers_past_a_recovered_take() {
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![
            take("t1", "Take 1", "/pool/1.wav"),
            take("t3", "Take 3 (recovered)", "/pool/3.wav"),
        ])),
        ..Setup::default()
    });
    s.act(|vm| vm.new_take());
    s.settle();
    let names: Vec<String> = s
        .practice()
        .vocal_tracks
        .iter()
        .map(|t| t.display_name.clone())
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("Take 4"));
}

// --- Save serialization --------------------------------------------------------------

#[test]
fn two_rapid_ungated_saves_never_overlap() {
    let (f, mut s) = Fixture::open(Setup::default());
    s.act(|vm| vm.rename_track("single", "A"));
    s.act(|vm| vm.rename_track("single", "B"));
    s.settle();
    assert_eq!(f.repo.s().max_in_flight, 1);
    assert_eq!(f.repo.stored().vocal_tracks[0].display_name, "B");
}

#[test]
fn an_edit_made_while_a_save_is_failing_in_flight_survives() {
    // Edit A's save is in flight (its model already sent) when edit B
    // changes the live model and queues save B. Save A then fails; its
    // revert reloads the disk's model, and over B's model it would have
    // saved the PRE-EDIT state: edit B lost on disk with only A's failure
    // said. (When both edits land before the save sends its model, the
    // failed write held both, and the revert with its toast is the designed
    // failure: not what this pins.)
    let (f, mut s) = Fixture::open(Setup::default());
    let gate = Gate::new();
    {
        let mut r = f.repo.s();
        r.save_gate = Some(Arc::clone(&gate));
        r.fail_next_save = true;
    }
    s.act(|vm| vm.rename_track("single", "A")); // this save fails
                                                // Save A has sent its model and is inside the store's write: THEN edit B
                                                // arrives, the window the guard protects.
    s.pump_until(|_| f.repo.s().in_flight > 0);
    s.act(|vm| vm.rename_track("single", "B")); // queued behind, must survive
    f.repo.s().save_gate = None;
    gate.open();
    s.settle();

    assert_eq!(f.events(), ["save-failed", "save"]);
    let name = |p: &SongPractice| {
        p.vocal_tracks
            .iter()
            .find(|t| t.id == "single")
            .map(|t| t.display_name.clone())
    };
    assert_eq!(
        name(&f.repo.stored()).as_deref(),
        Some("B"),
        "edit B must reach the store despite edit A's failed save"
    );
    assert_eq!(
        name(s.practice()).as_deref(),
        Some("B"),
        "the screen must keep showing the surviving edit"
    );
    assert!(s
        .toasts
        .contains(&"Couldn't save your changes.".to_string()));
}

// --- A failed save that could not revert ---------------------------------------------
//
// With no store read to revert to, the failed change stays on screen, a
// remote reload holds off for it, and the next save writes it with its own.

fn display_name(p: &SongPractice, id: &str) -> String {
    p.vocal_tracks
        .iter()
        .find(|t| t.id == id)
        .map(|t| t.display_name.clone())
        .expect("the take")
}

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
    assert_eq!(
        display_name(s.practice(), "single"),
        "kept",
        "nothing to revert to"
    );
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
fn a_lyric_stamp_a_failed_save_could_not_revert_is_kept_for_the_next_tap() {
    // Replaced by a later reload, the failed stamp went without the sync
    // cursor moving back, and the next tap stamped the line after it.
    fn tap_at(s: &mut Screen, ms: u64) {
        set_position(s, ms);
        s.act(|vm| vm.stamp_next_lyric());
        s.settle();
    }
    let (f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.set_lyrics_text("a\nb\nc\nd"));
    s.await_reply(call);
    tap_at(&mut s, 1000);
    {
        let mut r = f.repo.s();
        r.fail_at = vec![r.save_no + 1];
        r.fail_next_load = true;
    }
    tap_at(&mut s, 2000);
    s.act(|vm| vm.on_remote_applied(&remote("p1")));
    s.settle();
    tap_at(&mut s, 3000);
    let times: Vec<Option<i64>> = f.repo.stored().lyrics.iter().map(|l| l.time_ms).collect();
    assert_eq!(times, [Some(1000), Some(2000), Some(3000), None]);
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
    let call = s.call(|vm| vm.add_accompaniment("/src/backing.wav"));
    s.await_reply(call);
    s.settle();
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
        f.repo
            .stored()
            .accompaniment
            .as_ref()
            .expect("stored")
            .clips[0]
            .audio_file_path,
        path
    );
}

#[test]
fn while_an_import_a_revert_took_back_still_drops_its_file() {
    let (f, mut s) = Fixture::open(Setup::default());
    {
        let mut r = f.repo.s();
        r.fail_at = vec![r.save_no + 1];
    }
    let call = s.call(|vm| vm.add_accompaniment("/src/backing.wav"));
    s.await_reply(call);
    s.settle();
    assert!(s.practice().accompaniment.is_none());
    assert!(f.events().contains(&"delete:/import/0.wav".to_string()));
}
