//! The sync-cursor cases of `song_practice_structural_edit_test.dart` that
//! the `lyrics` module leaves: the "lyrics sync cursor" group's failed
//! click, lyrics dropped in Sync mode and new words from another device, and
//! the groups
//! "the sync cursor follows the words under it" and "the sync cursor through
//! failed and queued writes".
//!
//! Each call is a turn of its own, as each tap is. A write a call starts is
//! in flight until the screen pumps its answer, so Dart's "the write is in
//! flight" holds between two calls with nothing pumped in between; where
//! Dart waited for the store to be holding the write (`waitInFlight`), the
//! store holds it at its door until the test has made its moves.

use vocara_store::sync::sync_engine::RemoteApplied;

use super::*;

fn words(s: &Screen) -> Vec<String> {
    s.vm.lyrics().iter().map(|l| l.text.clone()).collect()
}

fn time(s: &Screen, line: usize) -> Option<i64> {
    s.vm.lyrics()[line].time_ms
}

fn stored_time(f: &Fixture, line: usize) -> Option<i64> {
    f.repo.stored().lyrics[line].time_ms
}

/// Dart's `await vm.setLyricsText(text)`: whether it landed.
fn set_text(s: &mut Screen, text: &str) -> bool {
    let call = s.call(|vm| vm.set_lyrics_text(text));
    s.landed(call)
}

/// Dart's `vm.importLyrics(content)`, not awaited.
fn import(s: &mut Screen, content: &str) -> Call {
    s.call(|vm| vm.import_lyrics(content))
}

fn sync_mode(s: &mut Screen) {
    s.vm.set_lyrics_mode(LyricsMode::Sync);
}

/// Space, not awaited.
fn tap(s: &mut Screen) {
    s.act(|vm| vm.stamp_next_lyric());
}

/// Dart's `await vm.stampNextLyric()`.
fn stamp(s: &mut Screen) {
    tap(s);
    s.settle();
}

/// `text` in Sync mode, its first `taps` lines stamped at 1 s.
fn stamped(s: &mut Screen, text: &str, taps: usize) {
    assert!(set_text(s, text));
    sync_mode(s);
    s.at_ms(1000);
    for _ in 0..taps {
        stamp(s);
    }
}

/// The store keeps the next writes at its door until the returned gate
/// opens (see [`release`]).
fn hold_writes(f: &Fixture) -> Arc<Gate> {
    let gate = Gate::new();
    f.repo.s().save_gate = Some(Arc::clone(&gate));
    gate
}

/// Dart's `waitInFlight`: a write is in the store.
fn wait_in_flight(f: &Fixture, s: &mut Screen) {
    s.pump_until(|_| f.repo.s().in_flight == 1);
    assert_eq!(
        f.repo.s().in_flight,
        1,
        "probe setup: a write should be in flight"
    );
}

/// Lets the held writes, and every later one, through.
fn release(f: &Fixture, gate: &Gate) {
    f.repo.s().save_gate = None;
    gate.open();
}

// --- Lyrics sync cursor --------------------------------------------------------------------

#[test]
fn a_click_whose_write_fails_leaves_the_cursor_on_that_line() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "one\ntwo\nthree"));
    sync_mode(&mut s);
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.stamp_lyric_at(1));
    s.settle();
    assert_eq!(time(&s, 1), None);
    assert_eq!(s.vm.sync_index(), 1);
}

#[test]
fn lyrics_dropped_in_sync_mode_restart_the_cursor() {
    let (_f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 5);
    assert_eq!(s.vm.sync_index(), 5);
    let call = import(&mut s, "x\ny");
    s.await_reply(call);
    assert_eq!(s.vm.sync_index(), 0);
    s.at_ms(2000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(2000), "Space did nothing");
}

#[test]
fn new_words_from_another_device_restart_the_cursor_new_times_do_not() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd", 2);
    assert_eq!(s.vm.sync_index(), 2);

    let reload_with = |s: &mut Screen, lines: Vec<LyricLine>| {
        let mut stored = f.repo.stored();
        stored.lyrics = lines.clone();
        f.repo.s().stored = Some(stored);
        let event = RemoteApplied {
            rows: Vec::new(),
            practice_ids: ["p1".to_string()].into_iter().collect(),
            technique_ids: Default::default(),
        };
        s.act(|vm| vm.on_remote_applied(&event));
        s.pump_until(|s| s.vm.lyrics() == lines.as_slice());
        s.settle();
        assert_eq!(s.vm.lyrics(), lines.as_slice());
    };

    // A peer stamped line d: the same words, so the cursor stays on c.
    let mut lines = s.vm.lyrics().to_vec();
    lines[3].time_ms = Some(9000);
    reload_with(&mut s, lines);
    assert_eq!(s.vm.sync_index(), 2);
    // A peer cleared every time: start over on the first words, as a clear
    // here does — left on c, the next tap timed c with a and b untimed.
    let cleared =
        s.vm.lyrics()
            .iter()
            .map(|l| LyricLine {
                time_ms: None,
                ..l.clone()
            })
            .collect();
    reload_with(&mut s, cleared);
    assert_eq!(s.vm.sync_index(), 0);
    s.at_ms(1500);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1500));
    assert_eq!(s.vm.sync_index(), 1);
    // A peer replaced the words: the cursor's place in the old ones means
    // nothing.
    reload_with(
        &mut s,
        vec![LyricLine::new("x", None), LyricLine::new("y", None)],
    );
    assert_eq!(s.vm.sync_index(), 0);
}

// --- The sync cursor follows the words under it ------------------------------------------

#[test]
fn the_same_words_re_imported_without_their_times_restart_the_cursor() {
    let (_f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    assert_eq!(s.vm.sync_index(), 3);
    // In Sync mode the user drops the same plain lyrics again: every time is
    // gone.
    let call = import(&mut s, "a\nb\nc\nd\ne\nf");
    assert!(s.landed(call));
    assert!(s.vm.lyrics().iter().all(|l| l.time_ms.is_none()));
    s.at_ms(5000);
    stamp(&mut s);
    assert_eq!(
        time(&s, 0),
        Some(5000),
        "the cursor stayed on d while a-c had lost their times"
    );
}

#[test]
fn a_failed_same_words_re_import_read_mid_save_restores_the_cursor() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    assert_eq!(s.vm.sync_index(), 3);
    f.repo.s().fail_saves = true;
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf");
    // The import's notify rebuilds the Sync view on the next frame, which
    // reads the cursor while the write is still in flight.
    assert_eq!(s.vm.sync_index(), 0);
    assert!(!s.landed(landed));
    f.repo.s().fail_saves = false;
    assert!(s.vm.lyrics()[..3].iter().all(|l| l.time_ms == Some(1000)));
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1000), "Space re-stamped line a");
    assert_eq!(time(&s, 3), Some(7000), "d never got the tap");
}

#[test]
fn a_tap_landed_while_a_failed_import_was_in_flight_keeps_its_cursor() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc\nd"));
    sync_mode(&mut s);
    // A transient failure: the import's write fails, the tap's lands.
    f.repo.s().fail_next_save = true;
    let landed = import(&mut s, "[00:01.00]a\n[00:02.00]b\n[00:03.00]c\n[00:04.00]d");
    // Every line timed: the cursor starts over on the first words.
    assert_eq!(s.vm.sync_index(), 0);
    // The user re-times from the top while the import's write is in flight.
    s.at_ms(1500);
    tap(&mut s);
    assert_eq!(s.vm.sync_index(), 1);
    assert!(!s.landed(landed));
    s.settle();
    assert_eq!(time(&s, 0), Some(1500));
    assert_eq!(
        s.vm.sync_index(),
        1,
        "the failed import re-seeded over the tap"
    );
    s.at_ms(2500);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1500), "Space re-stamped line a");
    assert_eq!(time(&s, 1), Some(2500), "b never got the tap");
}

#[test]
fn a_click_landed_while_a_failed_import_was_in_flight_keeps_its_cursor() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc\nd\ne"));
    sync_mode(&mut s);
    f.repo.s().fail_next_save = true;
    let landed = import(&mut s, "a\nb\nc\nd\ne");
    assert_eq!(s.vm.sync_index(), 0);
    // The import's write is in flight.
    s.at_ms(3000);
    s.act(|vm| vm.stamp_lyric_at(2));
    assert_eq!(s.vm.sync_index(), 3);
    assert!(!s.landed(landed));
    s.settle();
    assert_eq!(
        s.vm.sync_index(),
        3,
        "the failed import re-seeded over the click"
    );
}

#[test]
fn a_failed_re_import_and_a_failed_tap_never_put_the_cursor_on_a_timed_line() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    assert_eq!(s.vm.sync_index(), 3);
    f.repo.s().fail_saves = true;
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf");
    assert_eq!(s.vm.sync_index(), 0); // a frame mid-write
                                      // The write is in flight: Space while the import is.
    s.at_ms(2000);
    tap(&mut s);
    assert!(!s.landed(landed));
    s.vm.sync_index(); // a frame drawn after the import's failure
    s.settle();
    f.repo.s().fail_saves = false;
    assert!(s.vm.lyrics()[..3].iter().all(|l| l.time_ms == Some(1000)));
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1000), "Space re-stamped line a");
    assert_eq!(time(&s, 3), Some(7000), "d never got the tap");
}

#[test]
fn the_same_with_no_frame_drawn_between_the_two_failures() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    f.repo.s().fail_saves = true;
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf");
    assert_eq!(s.vm.sync_index(), 0);
    // The write is in flight.
    s.at_ms(2000);
    tap(&mut s);
    assert!(!s.landed(landed));
    s.settle();
    f.repo.s().fail_saves = false;
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1000), "Space re-stamped line a");
    assert_eq!(time(&s, 3), Some(7000), "d never got the tap");
}

#[test]
fn a_failed_import_under_a_newer_one_leaves_the_newer_cursor_alone() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc"));
    sync_mode(&mut s);
    f.repo.s().fail_next_save = true;
    let first = import(&mut s, "x\ny\nz");
    // Its write is in flight.
    let second = import(&mut s, "p\nq\nr");
    assert_eq!(s.vm.sync_index(), 0);
    s.at_ms(1000);
    s.act(|vm| vm.stamp_lyric_at(1));
    assert_eq!(s.vm.sync_index(), 2);
    assert!(!s.landed(first));
    assert!(s.landed(second));
    s.settle();
    let lines: Vec<String> =
        s.vm.lyrics()
            .iter()
            .map(|l| match l.time_ms {
                Some(t) => format!("{}@{t}", l.text),
                None => format!("{}@null", l.text),
            })
            .collect();
    assert_eq!(lines, ["p@null", "q@1000", "r@null"]);
    assert_eq!(s.vm.sync_index(), 2);
}

#[test]
fn an_edit_and_an_import_both_failing_leave_the_cursor_on_the_restored_text() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc", 1);
    assert_eq!(s.vm.sync_index(), 1);
    f.repo.s().fail_saves = true;
    let edit = s.call(|vm| vm.apply_edited_lyrics("a\nb\nc\nd"));
    let imp = import(&mut s, "x\ny");
    assert_eq!(s.vm.sync_index(), 0);
    assert!(!s.landed(edit));
    assert!(!s.landed(imp));
    f.repo.s().fail_saves = false;
    assert_eq!(words(&s), ["a", "b", "c"]);
    assert_eq!(s.vm.sync_index(), 1);
}

#[test]
fn a_failed_import_puts_the_cursor_back_on_the_restored_words() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 5);
    assert_eq!(s.vm.sync_index(), 5);
    f.repo.s().fail_saves = true;
    let call = import(&mut s, "x\ny");
    assert!(!s.landed(call));
    f.repo.s().fail_saves = false;
    assert_eq!(words(&s), ["a", "b", "c", "d", "e", "f"]);
    s.at_ms(7000);
    stamp(&mut s);
    // The restored words are timed through e: the tap belongs on f.
    assert_eq!(time(&s, 0), Some(1000), "Space re-stamped line a");
    assert_eq!(time(&s, 5), Some(7000), "f never got the tap");
}

#[test]
fn a_failed_import_never_leaves_the_cursor_past_the_end() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a"));
    sync_mode(&mut s);
    assert_eq!(s.vm.sync_index(), 0);
    f.repo.s().fail_saves = true;
    let call = import(&mut s, "\n\nx");
    assert!(!s.landed(call));
    f.repo.s().fail_saves = false;
    assert_eq!(words(&s), ["a"]);
    s.at_ms(3000);
    stamp(&mut s);
    assert!(s.vm.sync_index() <= s.vm.lyrics().len());
    assert_eq!(time(&s, 0), Some(3000), "Space did nothing");
}

#[test]
fn peer_words_merged_into_an_ordinary_save_re_seed_the_cursor() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 5);
    assert_eq!(s.vm.sync_index(), 5);
    f.repo.s().peer_words_on_next_save =
        Some(vec![LyricLine::new("x", None), LyricLine::new("y", None)]);
    s.act(|vm| vm.commit_track_move()); // any non-lyrics save (a drag released)
    s.settle();
    assert_eq!(words(&s), ["x", "y"]);
    assert!(
        s.vm.sync_index() < s.vm.lyrics().len(),
        "never past the end"
    );
    s.at_ms(2000);
    stamp(&mut s);
    assert_eq!(
        time(&s, 0),
        Some(2000),
        "Space did nothing: cursor {} of {}",
        s.vm.sync_index(),
        s.vm.lyrics().len()
    );
}

#[test]
fn a_failed_stamp_reverting_onto_a_peers_words_re_seeds_the_cursor() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 5);
    // A peer's new words reached the disk; this tap's write fails and the
    // model reverts to them.
    let mut stored = f.repo.stored();
    stored.lyrics = vec![LyricLine::new("x", None), LyricLine::new("y", None)];
    f.repo.s().stored = Some(stored);
    f.repo.s().fail_next_save = true;
    stamp(&mut s);
    assert_eq!(words(&s), ["x", "y"]);
    s.at_ms(2000);
    stamp(&mut s);
    assert_eq!(
        time(&s, 0),
        Some(2000),
        "Space did nothing: cursor {} of {}",
        s.vm.sync_index(),
        s.vm.lyrics().len()
    );
}

// --- The sync cursor through failed and queued writes ---------------------------------------

#[test]
fn sync_opened_while_a_failed_same_words_re_import_is_in_flight() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    s.vm.set_lyrics_mode(LyricsMode::Read); // Done
    f.repo.s().fail_saves = true;
    let gate = hold_writes(&f);
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf"); // drop in Read mode
    wait_in_flight(&f, &mut s);
    sync_mode(&mut s); // the user opens Sync while it writes
    release(&f, &gate);
    assert!(!s.landed(landed));
    f.repo.s().fail_saves = false;
    assert!(s.vm.lyrics()[..3].iter().all(|l| l.time_ms == Some(1000)));
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(
        time(&s, 0),
        Some(1000),
        "Space overwrote line a (cursor was {})",
        s.vm.sync_index()
    );
    assert_eq!(time(&s, 3), Some(7000), "d never got the tap");
}

#[test]
fn the_same_with_sync_toggled_off_and_on() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    f.repo.s().fail_saves = true;
    let gate = hold_writes(&f);
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf");
    wait_in_flight(&f, &mut s);
    s.vm.set_lyrics_mode(LyricsMode::Read);
    sync_mode(&mut s);
    release(&f, &gate);
    assert!(!s.landed(landed));
    f.repo.s().fail_saves = false;
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1000), "Space overwrote line a");
    assert_eq!(time(&s, 3), Some(7000), "d never got the tap");
}

#[test]
fn a_failed_clear_never_leaves_space_on_a_line_the_disk_holds_timed() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.clear_lyric_times());
    s.settle();
    f.repo.s().fail_saves = false;
    assert!(s.vm.lyrics()[..3].iter().all(|l| l.time_ms == Some(1000)));
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(
        time(&s, 0),
        Some(1000),
        "Space overwrote line a (cursor {})",
        s.vm.sync_index()
    );
    assert_eq!(time(&s, 3), Some(7000));
}

#[test]
fn a_clear_during_a_failed_same_words_re_import_both_failing() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    f.repo.s().fail_saves = true;
    let gate = hold_writes(&f);
    let landed = import(
        &mut s,
        "[00:01.00]a\n[00:02.00]b\n[00:03.00]c\n[00:04.00]d\n[00:05.00]e\n[00:06.00]f",
    );
    wait_in_flight(&f, &mut s);
    s.act(|vm| vm.clear_lyric_times());
    release(&f, &gate);
    assert!(!s.landed(landed));
    s.settle();
    f.repo.s().fail_saves = false;
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(time(&s, 0), Some(1000), "Space overwrote line a");
}

#[test]
fn two_taps_inside_one_round_trip_whose_writes_both_fail() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc\nd"));
    sync_mode(&mut s);
    f.repo.s().fail_saves = true;
    s.at_ms(1000);
    let gate = hold_writes(&f);
    tap(&mut s);
    wait_in_flight(&f, &mut s); // tap 1's write is in flight, sent with a stamped
    s.at_ms(2000);
    tap(&mut s);
    release(&f, &gate);
    s.settle();
    f.repo.s().fail_saves = false;
    assert!(s.vm.lyrics().iter().all(|l| l.time_ms.is_none()));
    assert_eq!(
        s.vm.sync_index(),
        0,
        "cursor on {}: line a lost its tap and is skipped",
        s.vm.sync_index()
    );
}

#[test]
fn three_taps_in_one_round_trip_whose_middle_write_fails_once() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc\nd\ne"));
    sync_mode(&mut s);
    s.at_ms(1000);
    let gate = hold_writes(&f);
    tap(&mut s); // write 0 in flight
    wait_in_flight(&f, &mut s);
    {
        // The next write after it fails once.
        let mut repo = f.repo.s();
        repo.fail_at = vec![repo.save_no + 2];
    }
    s.at_ms(2000);
    tap(&mut s);
    s.at_ms(3000);
    tap(&mut s);
    release(&f, &gate);
    s.settle();
    let stored: Vec<Option<i64>> = f.repo.stored().lyrics.iter().map(|l| l.time_ms).collect();
    assert_eq!(stored, [Some(1000), None, None, None, None]);
    assert_eq!(
        s.vm.sync_index(),
        1,
        "b and c lost their taps, cursor on {}",
        s.vm.sync_index()
    );
}

#[test]
fn a_tap_on_a_re_import_whose_write_fails_once_behind_another_save() {
    let (f, mut s) = Fixture::open(Setup::default());
    stamped(&mut s, "a\nb\nc\nd\ne\nf", 3);
    let gate = hold_writes(&f);
    s.act(|vm| vm.commit_track_move()); // any save in flight (a fader/drag release)
    wait_in_flight(&f, &mut s);
    {
        // The import's write fails once.
        let mut repo = f.repo.s();
        repo.fail_at = vec![repo.save_no + 2];
    }
    let landed = import(&mut s, "a\nb\nc\nd\ne\nf");
    assert_eq!(s.vm.sync_index(), 0); // a frame
    s.at_ms(2000);
    tap(&mut s);
    release(&f, &gate);
    assert!(!s.landed(landed));
    s.settle();
    assert!(f.repo.stored().lyrics[..3]
        .iter()
        .all(|l| l.time_ms == Some(1000)));
    s.at_ms(7000);
    stamp(&mut s);
    assert_eq!(
        stored_time(&f, 1),
        Some(1000),
        "Space overwrote b's good time on disk"
    );
    assert_eq!(stored_time(&f, 3), Some(7000));
}

#[test]
fn space_after_a_click_on_the_last_line_stamps_a_line_still_untimed() {
    let (_f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "a\nb\nc\nd"));
    sync_mode(&mut s);
    s.at_ms(9000);
    s.act(|vm| vm.stamp_lyric_at(3));
    s.settle();
    s.at_ms(1000);
    stamp(&mut s);
    assert!(
        s.vm.lyrics().iter().any(|l| l.time_ms == Some(1000)),
        "Space did nothing: cursor {} of {}",
        s.vm.sync_index(),
        s.vm.lyrics().len()
    );
}
