//! The lyrics cases of `song_practice_structural_edit_test.dart`: a lyrics
//! Save that did not land, the failure groups, the sync cursor, and editing
//! timed lyrics.

use super::*;

fn words(s: &Screen) -> Vec<String> {
    s.vm.lyrics().iter().map(|l| l.text.clone()).collect()
}

fn set_text(s: &mut Screen, text: &str) -> bool {
    let call = s.call(|vm| vm.set_lyrics_text(text));
    s.landed(call)
}

fn at_ms(s: &mut Screen, ms: u64) {
    s.at_ms(ms);
}

#[test]
fn a_lyrics_save_reports_it_did_not_land() {
    // Dart made both Saves in one synchronous turn, so the first write ran
    // after both and sent the second's model, whose revert then took both.
    // Here each Save is a turn of its own (as each tap is): the first write
    // starts before the second Save and fails, unreverted (the model moved
    // on); the second write sends the same words and lands them. Each Save
    // says what its own write did, and the editor keeps its draft exactly
    // when its words are not on disk.
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_at = vec![1];
    let first = s.call(|vm| vm.set_lyrics_text("new words"));
    let second = s.call(|vm| vm.set_lyrics_text("new words"));
    assert!(!s.landed(first), "its write failed");
    assert!(s.landed(second), "its write stored the words");
    s.settle();
    assert_eq!(words(&s), ["new words"]);
    assert_eq!(f.repo.stored().lyrics[0].text, "new words");
}

#[test]
fn a_lyrics_save_whose_write_fails_reports_it_did_not_land_and_reverts() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_at = vec![1];
    let call = s.call(|vm| vm.set_lyrics_text("new words"));
    assert!(!s.landed(call), "the editor must keep the draft");
    s.settle();
    assert_eq!(words(&s), ["la la la"]);
    assert_eq!(f.repo.stored().lyrics[0].text, "la la la");
}

#[test]
fn a_second_save_of_the_same_text_waits_for_the_write_it_repeats() {
    // The unchanged-text shortcut read the text already on screen, so a
    // second Save while the first write was queued said "saved" at once: the
    // editor closed, the write then failed and reverted the lines, and the
    // draft was gone.
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    let first = s.call(|vm| vm.set_lyrics_text("new words"));
    let second = s.call(|vm| vm.set_lyrics_text("new words"));
    assert!(!s.landed(first));
    assert!(!s.landed(second), "the editor must keep the draft");
    s.settle();
    assert_eq!(words(&s), ["la la la"]);
}

#[test]
fn an_unchanged_save_after_a_revert_that_kept_the_text_writes_nothing() {
    // The lyrics write landed while a rename was queued (so it could not
    // clear the pending mark), the rename's save then failed, and its revert
    // found the lines unchanged and returned before clearing the mark: the
    // next unchanged Save wrote again.
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_at = vec![2];
    let lyrics = s.call(|vm| vm.set_lyrics_text("new words"));
    s.act(|vm| vm.rename_track("single", "renamed"));
    assert!(s.landed(lyrics));
    s.settle();
    assert_eq!(words(&s), ["new words"]);
    let saves = {
        let mut r = f.repo.s();
        r.fail_saves = true;
        r.save_no
    };
    assert!(set_text(&mut s, "new words"));
    s.settle();
    assert_eq!(f.repo.s().save_no, saves);
}

#[test]
fn a_failed_lyrics_save_reverts_the_optimistic_lines_and_toasts() {
    let (f, mut s) = Fixture::open(Setup::default());
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.stamp_lyric_at(0)); // optimistic stamp, then a failed write
    s.settle();
    assert_eq!(
        s.vm.lyrics()[0].time_ms,
        None,
        "the un-persisted stamp must not stay on screen"
    );
    assert!(s
        .toasts
        .contains(&"Couldn't save your changes.".to_string()));
}

#[test]
fn two_taps_inside_one_save_round_trip_stamp_two_different_lines() {
    let (_f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "one\ntwo\nthree"));
    s.settle();
    at_ms(&mut s, 500);
    assert_eq!(s.vm.sync_index(), 0);
    s.act(|vm| vm.stamp_next_lyric());
    s.act(|vm| vm.stamp_next_lyric());
    s.settle();
    let lines = s.vm.lyrics().to_vec();
    assert!(lines[0].time_ms.is_some());
    assert!(
        lines[1].time_ms.is_some(),
        "the second tap must stamp the NEXT line, not re-stamp the first"
    );
    assert_eq!(lines[2].time_ms, None);
    assert_eq!(s.vm.sync_index(), 2);
}

#[test]
fn a_stray_timestamp_in_typed_lyrics_keeps_every_line() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| {
        vm.apply_edited_lyrics("I waited [04:33] for you\nand then you came\nand we sang")
    });
    assert!(s.landed(call));
    assert_eq!(
        words(&s),
        [
            "I waited [04:33] for you",
            "and then you came",
            "and we sang"
        ]
    );
}

#[test]
fn pasting_a_real_lrc_document_into_the_editor_still_syncs_it() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.apply_edited_lyrics("[ti:Song]\n[00:12.50]hello\n[01:03.25]world"));
    s.landed(call);
    assert_eq!(words(&s), ["hello", "world"]);
    assert_eq!(s.vm.lyrics()[0].time_ms, Some(12500));
}

#[test]
fn a_failed_stamp_leaves_the_cursor_on_the_line_it_could_not_stamp() {
    let (f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "one\ntwo\nthree"));
    s.settle();
    f.repo.s().fail_saves = true;
    s.act(|vm| vm.stamp_next_lyric());
    s.settle();
    assert_eq!(s.vm.lyrics()[0].time_ms, None);
    assert_eq!(
        s.vm.sync_index(),
        0,
        "advancing past a line the write lost would skip it silently"
    );
}

#[test]
fn a_leading_blank_line_never_takes_a_tap() {
    // Pasted lyrics often open on a blank line, which the Sync view draws as
    // a gap with no highlight. After Clear (or re-entering Sync with every
    // line timed) the cursor sat on it, and every stamp landed a line late.
    let (_f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "\nfirst line\nsecond line"));
    s.vm.set_lyrics_mode(LyricsMode::Sync);
    s.act(|vm| vm.clear_lyric_times());
    s.settle();
    at_ms(&mut s, 1500);
    s.act(|vm| vm.stamp_next_lyric());
    s.settle();
    assert_eq!(s.vm.lyrics()[0].time_ms, None);
    assert_eq!(s.vm.lyrics()[1].time_ms, Some(1500));
    at_ms(&mut s, 2500);
    s.act(|vm| vm.stamp_next_lyric());
    s.settle();
    assert_eq!(s.vm.lyrics()[2].time_ms, Some(2500));
    // Every line timed: Sync starts over from the first words.
    s.vm.set_lyrics_mode(LyricsMode::Read);
    s.vm.set_lyrics_mode(LyricsMode::Sync);
    assert_eq!(s.vm.sync_index(), 1);
}

#[test]
fn re_timing_a_line_by_click_then_space_stamps_the_next_line() {
    let (_f, mut s) = Fixture::open(Setup::default());
    assert!(set_text(&mut s, "one\n\ntwo\nthree"));
    s.vm.set_lyrics_mode(LyricsMode::Sync);
    at_ms(&mut s, 4000);
    s.act(|vm| vm.stamp_lyric_at(2));
    at_ms(&mut s, 6000);
    s.act(|vm| vm.stamp_next_lyric());
    s.settle();
    assert_eq!(s.vm.lyrics()[2].time_ms, Some(4000));
    assert_eq!(s.vm.lyrics()[3].time_ms, Some(6000));
}

#[test]
fn a_text_file_with_one_bracketed_time_keeps_every_line() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join("song.txt");
    std::fs::write(&file, "Guitar solo [2:30]\nfirst line\nsecond line").expect("lyrics");
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics_from_file(&file.to_string_lossy()));
    s.await_reply(call);
    assert_eq!(
        words(&s),
        ["Guitar solo [2:30]", "first line", "second line"]
    );
}

#[test]
fn a_dropped_file_of_another_kind_is_refused_with_a_word() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics_from_file("/somewhere/song.pdf"));
    assert_eq!(s.reply(call), Some(&Reply::Done(call)));
    assert_eq!(s.toasts, ["Drop a .lrc or .txt lyrics file."]);
}

#[test]
fn a_fixed_typo_in_the_first_chorus_leaves_the_second_its_own_time() {
    // Times were matched to the edited text by line text alone, first come
    // first served.
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics("[00:30.00]chorus\n[00:40.00]verse\n[01:30.00]chorus"));
    assert!(s.landed(call));
    let times =
        |s: &Screen| -> Vec<Option<i64>> { s.vm.lyrics().iter().map(|l| l.time_ms).collect() };
    assert!(set_text(&mut s, "chorus!\nverse\nchorus"));
    assert_eq!(times(&s), [None, Some(40000), Some(90000)]);
    assert!(set_text(&mut s, "verse\nchorus")); // the first chorus deleted
    assert_eq!(times(&s), [Some(40000), Some(90000)]);
}

#[test]
fn an_edit_keeps_the_times_of_the_lines_it_continues() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics("[00:01.00]one\n[00:02.00]two\n[00:03.00]three"));
    assert!(s.landed(call));
    // A line inserted: the others keep their own times, the new one has none.
    assert!(set_text(&mut s, "one\nnew\ntwo\nthree"));
    let times: Vec<Option<i64>> = s.vm.lyrics().iter().map(|l| l.time_ms).collect();
    assert_eq!(times, [Some(1000), None, Some(2000), Some(3000)]);
}

#[test]
fn an_unchanged_save_keeps_the_timed_blank_lines() {
    let (f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics("[00:01.00]one\n[00:02.00]"));
    assert!(s.landed(call));
    s.settle();
    let before = s.vm.lyrics().to_vec();
    let saves = f.repo.s().save_no;
    let text = s.vm.lyrics_as_text();
    assert!(set_text(&mut s, &text));
    assert_eq!(s.vm.lyrics(), before.as_slice());
    assert_eq!(f.repo.s().save_no, saves, "nothing to write");
}

#[test]
fn an_edit_keeps_the_end_marker_of_a_line_that_is_still_last() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let call = s.call(|vm| vm.import_lyrics("[00:01.00]one\n[00:02.00]two\n[00:03.00]"));
    assert!(s.landed(call));
    assert!(set_text(&mut s, "one changed\ntwo"));
    let lines = s.vm.lyrics().to_vec();
    assert_eq!(lines.len(), 3);
    assert!(lines[2].is_blank());
    assert_eq!(lines[2].time_ms, Some(3000));
}

#[test]
fn lyrics_mode_edit_bumps_the_session_and_closing_drops_the_draft() {
    let (_f, mut s) = Fixture::open(Setup::default());
    let session = s.vm.lyrics_edit_session();
    s.vm.set_lyrics_mode(LyricsMode::Edit);
    assert_eq!(s.vm.lyrics_edit_session(), session + 1);
    s.vm.set_lyrics_draft(Some("draft".into()));
    s.vm.set_lyrics_mode(LyricsMode::Read);
    assert_eq!(s.vm.lyrics_draft(), None);
}

#[test]
fn the_current_lyric_is_the_latest_time_at_or_before_the_playhead() {
    let (_f, mut s) = Fixture::open(Setup::default());
    // Stamped out of line order: the times need not follow the lines.
    assert!(set_text(&mut s, "late\nearly\nmid"));
    for (line, ms) in [(0, 5000), (1, 1000), (2, 3000)] {
        s.at_ms(ms);
        s.act(|vm| vm.stamp_lyric_at(line));
    }
    s.settle();
    assert_eq!(s.vm.current_lyric_index(500), None);
    assert_eq!(s.vm.current_lyric_index(3500), Some(2));
    assert_eq!(s.vm.current_lyric_index(9000), Some(0));
}
