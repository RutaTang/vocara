//! The view model's cases of `song_practice_placement_test.dart`: the loop
//! region over a first import, and where an added clip lands. (The drop
//! tests drive the screen's drop handler, which lays a multi-file drop end
//! to end from the drop's playhead with each import's `ClipAdded` reply;
//! their view-model half is `clips_laid_end_to_end_by_their_replies`.)

use std::sync::Arc;

use vocara_model::models::song_practice::SongPractice;
use vocara_services::waveform_service::WaveformResult;

use super::*;

fn empty_practice() -> SongPractice {
    SongPractice::new("p1", "Song", at(), at())
}

fn long_analyze(duration_ms: i64) -> TakeAnalyzer {
    Arc::new(move |_: &str| {
        Ok(WaveformResult {
            peaks: Vec::new(),
            duration_ms,
        })
    })
}

#[test]
fn the_loop_region_covers_a_first_import_longer_than_the_placeholder_timeline() {
    // A new practice opens on the 20 s placeholder timeline, and the region
    // only ever shrank — so after the first import the loop cycled the first
    // 20 seconds of a three-minute song.
    let (_f, mut s) = Fixture::open(Setup {
        practice: empty_practice(),
        analyze: long_analyze(180_000),
        ..Setup::default()
    });
    assert_eq!(
        s.vm.playback().loop_end_ms(),
        20000,
        "empty: the placeholder"
    );
    let call = s.call(|vm| vm.add_vocal_take("/src/backing-vocal.wav"));
    s.await_reply(call);
    s.settle();
    assert_eq!(s.practice().content_end_ms(), 180_000);
    assert_eq!(s.vm.playback().loop_end_ms(), 180_000);
    // And room past it, as a practice opened on its content has: grown to
    // the content alone, no clip could be dragged to end past it.
    assert!(s.practice().timeline_duration_ms > 180_000);
}

#[test]
fn a_clip_lands_at_the_position_its_caller_captured() {
    let gate = Gate::new();
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![take("t1", "Take 1", "/pool/t1.wav")])),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    s.at_ms(9000);
    let call = s.call(|vm| vm.add_clip_to_take("t1", "/src/a.wav", Some(5000)));
    s.at_ms(14000);
    gate.open();
    let reply = s.await_reply(call);
    assert_eq!(
        reply,
        Reply::ClipAdded {
            call,
            end_ms: Some(6000)
        }
    );
    let starts: Vec<i64> = s
        .track("t1")
        .clips
        .iter()
        .skip(1)
        .map(|c| c.start_ms)
        .collect();
    assert_eq!(starts, [5000]);
}

#[test]
fn a_clip_without_a_captured_position_takes_the_playhead_at_the_drop() {
    let gate = Gate::new();
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![take("t1", "Take 1", "/pool/t1.wav")])),
        analyze: gated_analyze(&gate),
        ..Setup::default()
    });
    s.at_ms(3000);
    let call = s.call(|vm| vm.add_clip_to_take("t1", "/src/a.wav", None));
    s.at_ms(8000);
    gate.open();
    s.await_reply(call);
    assert_eq!(s.track("t1").clips[1].start_ms, 3000);
}

#[test]
fn clips_laid_end_to_end_by_their_replies() {
    // The screen lays a drop's files end to end: each import's reply says
    // where its clip ends, and the next file goes there.
    let (_f, mut s) = Fixture::open(Setup {
        practice: seed_practice(Some(vec![take("t1", "Take 1", "/pool/t1.wav")])),
        ..Setup::default()
    });
    let mut at_ms = 2000;
    for file in ["/src/a.wav", "/src/b.wav", "/src/c.wav"] {
        let call = s.call(|vm| vm.add_clip_to_take("t1", file, Some(at_ms)));
        if let Reply::ClipAdded {
            end_ms: Some(end), ..
        } = s.await_reply(call)
        {
            at_ms = end;
        }
    }
    let starts: Vec<i64> = s
        .track("t1")
        .clips
        .iter()
        .skip(1)
        .map(|c| c.start_ms)
        .collect();
    assert_eq!(starts, [2000, 3000, 4000]);
    // The last one dropped is the one selected.
    let last = s.track("t1").clips[3].id.clone();
    assert_eq!(s.vm.selected_clip_id(), Some(last.as_str()));
}
