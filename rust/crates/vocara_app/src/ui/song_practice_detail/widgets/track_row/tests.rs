//! The track row's tests: `track_row_test.dart` (M/S on a reorderable lane,
//! the waveform's width, the clip move-drag guards, the viewport alignment,
//! the faders held or scrolled away, a lane with no room), the widget-level
//! halves of `playhead_repaint_test.dart` (a fader tick leaves the clips'
//! waveforms cached; right-clicks open the clip and lane menus) and of
//! `kept_subtree_fresh_build_test.dart` (a lane's M button and clip drag act
//! on the lane as it is now), and the long-press reorder.
//!
//! The drags run on a [`LiveUi`], which rebuilds the view between events as
//! the app does after every message — the Dart tests' `StatefulBuilder`
//! that applied every live tick back into the row.

use std::sync::Arc;
use std::time::Duration;

use iced::widget::{column, container, scrollable, Space};
use iced::{Settings, Size};
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::round_to_i64;

use super::*;
use crate::ui::core::theme::app_theme;
use crate::ui::core::widgets::waveform_painter::live_waveform_outline;
use crate::ui::song_practice_detail::widgets::live_ui::{
    move_to, press, redraw, release, right_click, scroll, LiveUi,
};

/// The Dart tests' surface: 800 × 600.
const WINDOW: Size = Size {
    width: 800.0,
    height: 600.0,
};

/// The lane's width beside the header in that window.
const LANE_W: f32 = 800.0 - TRACK_HEADER_W;

/// The zoomed viewport the drag tests show: 10 s.
const VIEWPORT_MS: i64 = 10000;

fn created() -> DateTime {
    DateTime::utc_ymd(2026, 1, 1)
}

/// A one-clip take (`'<id>-c0'`), `duration_ms` long from 0, with `peaks`.
fn take(id: &str, duration_ms: i64, peaks: Vec<f64>) -> AudioTrack {
    AudioTrack::new(id, AudioTrackKind::Vocal, "Take 1", created()).with_audio(
        "/x.wav",
        0,
        duration_ms,
        peaks,
    )
}

/// A take with one-second clips at `starts`.
fn clips_take(starts: &[(String, i64)]) -> AudioTrack {
    AudioTrack {
        clips: starts
            .iter()
            .map(|(id, start)| AudioClip {
                start_ms: *start,
                waveform_peaks: vec![0.4, 0.8, 0.4],
                ..AudioClip::new(id.as_str(), format!("/{id}.wav"), 1000)
            })
            .collect(),
        ..AudioTrack::new("t1", AudioTrackKind::Vocal, "Take 1", created())
    }
}

/// The row as the tests mount it: `Take 1` on the 10 s viewport.
fn row_of(track: &AudioTrack) -> TrackRow<'_, Message> {
    track_row(track, "Take 1", VIEWPORT_MS, 0, |m| m)
}

/// The centre of a clip `dur_ms` long starting at `start_ms`, in the window.
fn clip_center(start_ms: i64, dur_ms: i64) -> Point {
    let left = TRACK_HEADER_W + start_ms as f32 / VIEWPORT_MS as f32 * LANE_W;
    let width = dur_ms as f32 / VIEWPORT_MS as f32 * LANE_W;
    Point::new(left + width / 2.0, TRACK_ROW_HEIGHT / 2.0)
}

fn sim<'a>(view: impl Into<Element<'a, Message>>) -> iced_test::Simulator<'a, Message> {
    iced_test::Simulator::with_size(Settings::default(), WINDOW, view)
}

// ---------------------------------------------------- track_row_test

#[test]
fn m_and_s_toggle_on_a_reorderable_lane_and_do_not_select_it() {
    let track = take("t1", 5000, Vec::new());
    let mut ui = sim(row_of(&track).reorderable(true));
    ui.click("M").expect("the M key");
    ui.click("S").expect("the S key");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::ToggleMute, Message::ToggleSolo]
    );
}

#[test]
fn a_clips_waveform_spans_the_clips_full_width_selected_or_not() {
    // Inset by the border and a side padding while the painter mapped the
    // clip's full width: the edges drew up to 4 px off the ruler and
    // playhead, and selecting the clip shifted its waveform a pixel.
    let track = take("t1", 5000, vec![0.5; 100]);
    let bounds = |selected| {
        let mut ui = sim(row_of(&track).selected(selected));
        let waveform = ui.find(waveform_widget_id("t1-c0")).expect("drawn");
        let clip = ui.find(clip_widget_id("t1-c0")).expect("drawn");
        (waveform.bounds(), clip.bounds())
    };
    let (plain, clip) = bounds(false);
    assert_eq!(plain.x, clip.x);
    assert_eq!(plain.width, clip.width);
    assert!((clip.x - TRACK_HEADER_W).abs() < 0.01, "{clip:?}");
    assert!((clip.width - LANE_W / 2.0).abs() < 0.01, "{clip:?}");
    let (picked, _) = bounds(true);
    assert_eq!(picked.x, plain.x);
    assert_eq!(picked.width, plain.width);
}

/// A lane whose clip moves are applied back into the row on every live tick
/// (like the view model's notify), so the viewport re-evaluates mid-drag
/// exactly as it does in the app.
struct ClipLane {
    ui: LiveUi,
    starts: Vec<(String, i64)>,
    live_ids: Vec<String>,
    commits: usize,
    messages: Vec<Message>,
}

impl ClipLane {
    fn new(starts: &[(&str, i64)]) -> Self {
        Self {
            ui: LiveUi::new(WINDOW),
            starts: starts.iter().map(|(id, s)| (id.to_string(), *s)).collect(),
            live_ids: Vec::new(),
            commits: 0,
            messages: Vec::new(),
        }
    }

    fn start(&self, id: &str) -> i64 {
        self.starts.iter().find(|(c, _)| c == id).expect("a clip").1
    }

    /// Feeds `events` one by one, rebuilding the row after each.
    fn run(&mut self, events: impl IntoIterator<Item = Event>) {
        for event in events {
            let track = clips_take(&self.starts);
            let messages = self.ui.run(row_of(&track), [event]);
            for m in messages {
                match &m {
                    Message::ClipMoveLive { clip_id, start_ms } => {
                        self.live_ids.push(clip_id.clone());
                        // Mimic the view model's clamp against the FULL
                        // timeline (60 s here), far wider than the zoomed
                        // 10 s viewport — so a clip CAN move fully outside
                        // the visible window.
                        let s = (*start_ms).clamp(0, 59000);
                        if let Some(c) = self.starts.iter_mut().find(|(c, _)| c == clip_id) {
                            c.1 = s;
                        }
                    }
                    Message::MoveCommit => self.commits += 1,
                    _ => {}
                }
                self.messages.push(m);
            }
        }
    }

    fn by(&self, at: Point, dx: f32) -> Event {
        move_to(at.x + dx, at.y)
    }
}

#[test]
fn dragging_a_clip_beyond_the_viewport_still_commits_on_release() {
    let mut lane = ClipLane::new(&[("a", 4000)]);
    let at = clip_center(4000, 1000);
    lane.ui.point_at(at);
    // Cross the slop (the drag is accepted), then push past the right edge.
    lane.run([press(), lane.by(at, 30.0), lane.by(at, 450.0)]);
    assert!(
        lane.start("a") > VIEWPORT_MS,
        "the clip should now be fully outside the zoomed viewport"
    );
    // The drag must still be alive.
    let before = lane.live_ids.len();
    lane.run([lane.by(at, 462.0)]);
    assert!(lane.live_ids.len() > before);
    lane.run([release()]);
    assert!(!lane.live_ids.is_empty());
    assert_eq!(
        lane.commits, 1,
        "releasing an off-viewport drag must still commit the move"
    );
}

#[test]
fn a_mid_drag_cull_cannot_transplant_the_drag_onto_the_next_clip() {
    let mut lane = ClipLane::new(&[("a", 4000), ("b", 8000)]);
    let at = clip_center(4000, 1000);
    lane.ui.point_at(at);
    // Clip a leaves the viewport.
    lane.run([
        press(),
        lane.by(at, 30.0),
        lane.by(at, 450.0),
        lane.by(at, 462.0),
        release(),
    ]);
    assert!(
        lane.live_ids.iter().all(|id| id == "a"),
        "every live tick must keep targeting the dragged clip"
    );
    assert_eq!(lane.start("b"), 8000, "the neighbor clip must not move");
    assert_eq!(lane.commits, 1);
}

#[test]
fn a_drag_whose_release_is_lost_still_commits_the_live_moves() {
    // Dart's pointer cancel: a cancelled drag already moved the model, so it
    // must commit. A mouse has no cancel in iced; a release that never came
    // (the next press arrives first) ends the drag the same way.
    let mut lane = ClipLane::new(&[("a", 4000)]);
    let at = clip_center(4000, 1000);
    lane.ui.point_at(at);
    lane.run([press(), lane.by(at, 30.0), lane.by(at, 80.0)]);
    assert!(!lane.live_ids.is_empty(), "a real live move lands");
    lane.run([press()]);
    assert_eq!(lane.commits, 1);
    lane.run([release()]);
    assert_eq!(lane.commits, 1, "once per started drag");
}

#[test]
fn a_plain_tap_selects_the_clip_and_never_commits_a_move() {
    let mut lane = ClipLane::new(&[("a", 4000)]);
    lane.ui.point_at(clip_center(4000, 1000));
    lane.run([press(), release()]);
    assert_eq!(lane.messages, vec![Message::SelectClip("a".into())]);
    assert_eq!(lane.start("a"), 4000);
    assert_eq!(lane.commits, 0);
}

#[test]
fn a_two_finger_trackpad_swipe_over_a_clip_never_moves_it() {
    // A two-finger trackpad swipe is how the timeline pans; it moved, and
    // saved, the clip the cursor happened to rest on.
    use crate::ui::core::widgets::pan_zoom::testing as pan_zoom;

    let mut lane = ClipLane::new(&[("a", 4000)]);
    lane.ui.point_at(clip_center(4000, 1000));
    let track = clips_take(&lane.starts);
    let swipe = pan_zoom::swipe(&[(-60.0, 0.0), (-200.0, 0.0)], Instant::now());
    let messages = lane.ui.run_sent(row_of(&track), swipe);
    assert!(messages.is_empty(), "a swipe pans the view, not the clip");
    assert_eq!(lane.start("a"), 4000);
}

#[test]
fn a_mouse_drag_still_moves_and_commits_the_clip() {
    let mut lane = ClipLane::new(&[("a", 4000)]);
    let at = clip_center(4000, 1000);
    lane.ui.point_at(at);
    lane.run([press(), lane.by(at, 30.0), lane.by(at, 80.0), release()]);
    assert!(!lane.live_ids.is_empty());
    assert!(lane.start("a") > 4000);
    // The movement that won the drag is swallowed: 50 px of it moved it.
    assert_eq!(
        lane.start("a"),
        4000 + round_to_i64(50.0 / f64::from(LANE_W) * VIEWPORT_MS as f64)
    );
    assert_eq!(lane.commits, 1);
}

#[test]
fn a_dragged_clip_is_selected_before_it_moves() {
    // The selection stayed on the clip selected before, and Delete then
    // asked about that one, not the clip just moved.
    let mut lane = ClipLane::new(&[("a", 4000), ("b", 8000)]);
    let at = clip_center(4000, 1000);
    lane.ui.point_at(at);
    lane.run([press(), lane.by(at, 30.0), lane.by(at, 80.0), release()]);
    let selects: Vec<&Message> = lane
        .messages
        .iter()
        .filter(|m| matches!(m, Message::SelectClip(_)))
        .collect();
    assert_eq!(selects, [&Message::SelectClip("a".into())]);
    let first_move = lane
        .messages
        .iter()
        .position(|m| matches!(m, Message::ClipMoveLive { .. }))
        .expect("a live move");
    assert!(
        matches!(&lane.messages[0], Message::SelectClip(id) if id == "a") && first_move > 0,
        "{:?}",
        lane.messages
    );
    assert_eq!(lane.commits, 1);
}

/// The row at the top of a lane list, with 30 lanes' worth below it.
fn lane_list<'a>(row: TrackRow<'a, Message>) -> Element<'a, Message> {
    let mut list = column![row].width(Length::Fill);
    for _ in 0..30 {
        list = list.push(Space::new().height(TRACK_ROW_HEIGHT));
    }
    scrollable(list).height(Length::Fill).into()
}

#[test]
fn a_clip_drag_whose_lane_scrolls_out_of_the_lane_list_still_commits_its_moves() {
    // A mouse wheel mid-drag scrolls the lane list and (Dart's list being
    // lazy) unmounted the lane; the move shown on screen was never saved.
    let mut ui = LiveUi::new(WINDOW);
    let mut start = 4000;
    let mut lives = 0;
    let mut commits = 0;
    let at = Point::new(
        TRACK_HEADER_W + 0.4 * LANE_W + 0.05 * LANE_W,
        TRACK_ROW_HEIGHT / 2.0,
    );
    ui.point_at(at);
    let events = [
        press(),
        move_to(at.x + 30.0, at.y),
        move_to(at.x + 70.0, at.y),
        // The wheel turns mid-drag: the list scrolls the take away.
        scroll(0.0, -1500.0),
        move_to(at.x + 80.0, at.y),
        release(),
    ];
    for (i, event) in events.into_iter().enumerate() {
        let track = clips_take(&[("a".into(), start)]);
        for m in ui.run(lane_list(row_of(&track)), [event]) {
            match m {
                Message::ClipMoveLive { start_ms, .. } => {
                    lives += 1;
                    start = start_ms;
                }
                Message::MoveCommit => commits += 1,
                _ => {}
            }
        }
        if i == 2 {
            assert!(lives > 0);
        }
    }
    // The lane is out of view: the list scrolled past it.
    let track = clips_take(&[("a".into(), start)]);
    let shown = ui
        .find(lane_list(row_of(&track)), "M")
        .expect("in the tree")
        .visible_bounds();
    assert!(shown.is_none(), "scrolled away: {shown:?}");
    assert_eq!(commits, 1);
    assert!(start > 4000);
}

/// The volume fader's box in a vocal take's header at the window's
/// top-left: past the 3 px stripe and the 11 px padding, the M and S keys
/// and their gaps (52 px); before the 4 px gap, the two 24 px icon buttons,
/// the 10 px padding and the 1 px border; 16 px tall, centred on the 24 px
/// control row at the bottom of the padded column (over the row's 1 px
/// line).
fn fader_box() -> Rectangle {
    let left = 3.0 + 11.0 + 52.0;
    let right = TRACK_HEADER_W - 1.0 - 10.0 - 48.0 - 4.0;
    let row_bottom = TRACK_ROW_HEIGHT - 1.0 - 8.0;
    Rectangle::new(
        Point::new(left, row_bottom - 12.0 - 8.0),
        Size::new(right - left, 16.0),
    )
}

fn a_take_fader_held_then_slid_saves_its_level(hold: Duration) {
    // Held past the long-press timeout, the press became the lane's reorder
    // drag: the level had jumped on the press and was never saved.
    let mut ui = LiveUi::new(WINDOW);
    let mut volume = 0.8;
    let mut all = Vec::new();
    let fader = fader_box();
    let at = Point::new(fader.x + fader.width * 0.3, fader.center_y());
    ui.point_at(at);
    let step = |ui: &mut LiveUi, volume: &mut f64, all: &mut Vec<Message>, event: Event| {
        let track = AudioTrack {
            volume: *volume,
            ..take("t1", 5000, Vec::new())
        };
        // Every vocal take is reorderable.
        for m in ui.run(row_of(&track).reorderable(true), [event]) {
            if let Message::Volume(v) = m {
                *volume = v;
            }
            all.push(m);
        }
    };
    step(&mut ui, &mut volume, &mut all, press());
    assert!((volume - 0.3).abs() < 0.02, "the press lands on the fader");
    std::thread::sleep(hold);
    for event in [
        redraw(),
        move_to(at.x + 10.0, at.y),
        move_to(at.x + 20.0, at.y),
        release(),
    ] {
        step(&mut ui, &mut volume, &mut all, event);
    }
    let lives = all
        .iter()
        .filter(|m| matches!(m, Message::Volume(_)))
        .count();
    let commits = all.iter().filter(|m| **m == Message::VolumeCommit).count();
    assert!(lives > 0);
    assert_eq!(commits, 1);
    assert!(
        (volume - (0.3 + 20.0 / f64::from(fader.width))).abs() < 0.02,
        "{volume}"
    );
    assert!(!all.contains(&Message::LaneDragStart), "{all:?}");
}

#[test]
fn a_take_fader_pressed_for_150ms_then_slid_saves_its_level() {
    a_take_fader_held_then_slid_saves_its_level(Duration::from_millis(150));
}

#[test]
fn a_take_fader_pressed_for_600ms_then_slid_saves_its_level() {
    a_take_fader_held_then_slid_saves_its_level(Duration::from_millis(600));
}

#[test]
fn a_take_fader_whose_lane_scrolls_out_mid_drag_commits_its_level() {
    // Dart's disposed recognizer fired no end: the level the engine played
    // was never saved, as for a clip move.
    let mut ui = LiveUi::new(WINDOW);
    let mut volume = 0.8;
    let mut commits = 0;
    let fader = fader_box();
    let at = Point::new(fader.center_x(), fader.center_y());
    ui.point_at(at);
    let mut shown = volume;
    for (i, event) in [
        press(),
        move_to(at.x - 20.0, at.y),
        scroll(0.0, -1500.0),
        release(),
    ]
    .into_iter()
    .enumerate()
    {
        let track = AudioTrack {
            volume,
            ..take("t1", 5000, Vec::new())
        };
        for m in ui.run(lane_list(row_of(&track).reorderable(true)), [event]) {
            match m {
                Message::Volume(v) => volume = v,
                Message::VolumeCommit => commits += 1,
                _ => {}
            }
        }
        if i == 1 {
            shown = volume;
        }
    }
    assert_eq!(volume, shown);
    assert!(volume < 0.8);
    assert_eq!(commits, 1);
}

#[test]
fn a_take_fader_keeps_its_drag_when_a_take_above_it_is_deleted() {
    // `kept_subtree_fresh_build_test`'s "a take fader, a take deleted above
    // it": the lanes are mounted keyed by take id (as the screen must), so a
    // fader's drag stays with its take when the list shifts under it.
    let takes: Vec<AudioTrack> = ["t0", "t1", "t2"]
        .iter()
        .map(|id| AudioTrack {
            volume: 0.8,
            ..take(id, 5000, Vec::new())
        })
        .collect();
    fn list(shown: &[AudioTrack]) -> Element<'_, (String, Message)> {
        iced::widget::keyed_column(shown.iter().map(|t| {
            let id = t.id.clone();
            // A take's key: its id's number (keys are `'static` values).
            let key: u32 = t.id[1..].parse().expect("t<n>");
            (
                key,
                Element::from(
                    track_row(t, "Take", VIEWPORT_MS, 0, move |m| (id.clone(), m))
                        .reorderable(true),
                ),
            )
        }))
        .into()
    }
    let mut ui = LiveUi::new(WINDOW);
    let fader = fader_box();
    // The second take's fader, one row down.
    let at = Point::new(fader.center_x(), fader.center_y() + TRACK_ROW_HEIGHT);
    ui.point_at(at);
    let grabbed = ui.run(list(&takes), [press()]);
    assert!(matches!(grabbed.as_slice(), [(id, Message::Volume(_))] if id == "t1"));
    // The take above it goes; the drag goes on.
    let rest = &takes[1..];
    let moved = ui.run(
        list(rest),
        [move_to(at.x - 20.0, at.y - TRACK_ROW_HEIGHT), release()],
    );
    assert!(!moved.is_empty());
    assert!(moved.iter().all(|(id, _)| id == "t1"), "{moved:?}");
    assert_eq!(
        moved.last().map(|(_, m)| m.clone()),
        Some(Message::VolumeCommit)
    );
}

#[test]
fn a_lane_with_no_room_for_its_body_builds_without_its_clips() {
    // A lane laid out with no room for its body (the window narrower than
    // the panels around the lanes) clamped each clip's box to [1 px, 0 px]
    // and threw in build.
    let track = clips_take(&[("c1".into(), 0)]);
    let mut ui = sim(container(row_of(&track)).width(TRACK_HEADER_W));
    assert!(ui.find(clip_widget_id("c1")).is_err());
    ui.snapshot(&app_theme()).expect("it draws");
}

// ------------------------------------------- viewport waveform alignment

/// The left and right x of the topmost vertices of `outline` (a lone
/// spike's top).
fn top_span(outline: &[(f64, f64)]) -> (f64, f64) {
    let top = outline
        .iter()
        .map(|&(_, y)| y)
        .fold(f64::INFINITY, f64::min);
    outline
        .iter()
        .filter(|&&(_, y)| (y - top).abs() < 1e-9)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(x, _)| {
            (lo.min(x), hi.max(x))
        })
}

#[test]
fn a_clip_sliced_at_the_viewport_edge_draws_its_transient_at_the_true_timeline_position() {
    // Clip 0..10 s with 10 peaks (1 s per peak); the spike is peak 5,
    // anchored at 5000 ms. Viewport 4500..9500: the slice starts at peak 4
    // (4000 ms), HALF a peak before the box's left edge.
    let peaks: Vec<f64> = (0..10).map(|i| if i == 5 { 1.0 } else { 0.0 }).collect();
    let clip = AudioClip {
        waveform_peaks: peaks,
        ..AudioClip::new("c", "/x.wav", 10000)
    };
    let size = Size::new(500.0, 40.0);
    let v = clip_box(&clip, false, 5000, 4500, f64::from(size.width)).expect("in view");
    assert_eq!(v.left, 0.0);
    assert_eq!(v.width, 500.0);
    let painter = WaveformPainter::sliced(clip_peaks(&clip), v.slice, Color::WHITE);
    let shape = painter.shape(size).expect("painted");
    // The spike's bucket, 5000..6000 ms, is (5000 − 4500) / 5000 of the box
    // in: 50 to 150. Anchoring the slice at the box edge instead would draw
    // it from 1/5 of the width (100), one half-peak (500 ms) late against
    // the ruler.
    let (left, right) = top_span(&shape.outline);
    assert!((left - 50.0).abs() <= 1.0, "{left}");
    assert!((right - 150.0).abs() <= 1.0, "{right}");
}

#[test]
fn the_live_forming_clip_keeps_its_absolute_sample_to_column_mapping() {
    // 100 record peaks of 50 ms (take 0..5000 ms), spike at peak 30
    // (1500 ms). Viewport 1025..2025: the slice starts at peak 20 (1000
    // ms), half a record-peak before the box edge.
    let peaks: Vec<f64> = (0..100).map(|i| if i == 30 { 1.0 } else { 0.0 }).collect();
    let lane = f64::from(LANE_W);
    let v = recording_span(1000, 1025, lane, 0, peaks.len()).expect("in view");
    let pps = recording_pixels_per_sample(1000, lane);
    let (outline, _) = live_waveform_outline(
        &peaks[v.slice.range.clone()],
        500.0,
        40.0,
        pps,
        v.slice.index_offset,
    )
    .expect("painted");
    // The spike bucket covers 1500..1550 ms; its drawn center belongs
    // (1525 − 1025) / 50 record-peaks right of the box edge, at the
    // painter's own absolute pixels-per-sample. Re-basing the slice to the
    // box edge would draw it half a peak (~15 px here) late, and it would
    // freeze on screen while panning.
    let expected = (1525.0 - 1025.0) / 50.0 * pps;
    let (left, right) = top_span(&outline);
    let apex = (left + right) / 2.0;
    assert!((apex - expected).abs() <= 3.0, "{apex} vs {expected}");
}

#[test]
fn the_forming_take_grows_rightward_without_its_drawn_part_moving() {
    // `kept_subtree_recording_test`'s recording ticks, at the row's level:
    // each tick adds peaks; the box keeps its left edge and widens by the
    // timeline's px per record interval, and the columns already drawn
    // stay as they were.
    let lane = f64::from(LANE_W);
    let pps = recording_pixels_per_sample(VIEWPORT_MS, lane);
    let peaks: Vec<f64> = (0..60).map(|i| (i % 5) as f64 / 5.0).collect();
    let early = recording_span(VIEWPORT_MS, 0, lane, 2000, 20).expect("in view");
    let later = recording_span(VIEWPORT_MS, 0, lane, 2000, 60).expect("in view");
    assert_eq!(early.left, later.left);
    assert!((later.width - early.width - 40.0 * pps).abs() < 1e-9);
    let draw = |v: &VisibleSpan, n: usize| {
        live_waveform_outline(
            &peaks[..n][v.slice.range.clone()],
            v.width,
            40.0,
            pps,
            v.slice.index_offset,
        )
        .expect("painted")
        .0
    };
    let (a, b) = (draw(&early, 20), draw(&later, 60));
    // The top edge's first columns (the outline starts at (0, mid)).
    let columns = (early.width.floor() as usize).saturating_sub(1);
    assert_eq!(a[..columns], b[..columns]);
    // No timeline: one pixel per sample, as Dart fell back to.
    assert_eq!(recording_pixels_per_sample(0, lane), 1.0);
}

// ---------------------------------------------------------- geometry

#[test]
fn a_span_out_of_view_or_degenerate_has_no_box() {
    assert!(visible_span(0, 0, 500.0, 0, 1000, 3).is_none());
    assert!(visible_span(1000, 0, 500.0, 0, 0, 3).is_none());
    assert!(visible_span(1000, 0, 0.5, 0, 1000, 3).is_none());
    assert!(visible_span(1000, 0, 500.0, 1000, 1000, 3).is_none());
    assert!(visible_span(1000, 2000, 500.0, 0, 1000, 3).is_none());
    // A sliver in view keeps a pixel.
    let v = visible_span(10000, 0, 500.0, 9999, 1000, 3).expect("in view");
    assert_eq!(v.width, 1.0);
    // Clamped to the window.
    let v = visible_span(1000, 500, 500.0, 0, 5000, 3).expect("in view");
    assert_eq!((v.left, v.width), (0.0, 500.0));
}

#[test]
fn the_dragged_clip_parks_just_outside_the_lane() {
    let clip = |start_ms| AudioClip {
        start_ms,
        ..AudioClip::new("c", "/x.wav", 1000)
    };
    // Left of the view: one pixel left of the edge; right of it: at the
    // right edge.
    let v = clip_box(&clip(0), true, 1000, 5000, 400.0).expect("parked");
    assert_eq!((v.left, v.width), (-1.0, 1.0));
    let v = clip_box(&clip(9000), true, 1000, 5000, 400.0).expect("parked");
    assert_eq!((v.left, v.width), (400.0, 1.0));
    assert!(v.slice.range.is_empty());
    // Not dragged: culled.
    assert!(clip_box(&clip(9000), false, 1000, 5000, 400.0).is_none());
}

#[test]
fn a_drag_moves_the_clip_by_its_travel_on_the_timeline() {
    assert_eq!(dragged_start_ms(4000, 59.2, 592.0, 10000), Some(5000));
    // Rounded half away from zero, as Dart's `round()`.
    assert_eq!(dragged_start_ms(0, 0.25, 500.0, 1000), Some(1));
    assert_eq!(dragged_start_ms(0, -0.25, 500.0, 1000), Some(-1));
    // No timeline or no lane: no move.
    assert_eq!(dragged_start_ms(4000, 10.0, 592.0, 0), None);
    assert_eq!(dragged_start_ms(4000, 10.0, 0.0, 10000), None);
}

#[test]
fn a_selected_lane_highlights_its_selected_clip_or_all_of_them() {
    assert!(clip_selected(true, None, "a"));
    assert!(clip_selected(true, Some("a"), "a"));
    assert!(!clip_selected(true, Some("b"), "a"));
    assert!(!clip_selected(false, None, "a"));
}

#[test]
fn a_lane_takes_its_tint_its_backing_neutral_or_its_take_color() {
    let mut track = take("t1", 1000, Vec::new());
    assert_eq!(take_color(&track), Studio::default_track_color("t1"));
    assert!(Studio::TRACK_COLORS.contains(&take_color(&track)));
    track.color_value = Some(0xFF5B8DEF);
    assert_eq!(take_color(&track), argb(0xFF5B8DEF));
    let backing = AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", created());
    assert_eq!(take_color(&backing), Studio::TRACK_BACKING);
    assert_eq!(lane_color("acc", None, true), Studio::TRACK_BACKING);
    assert_eq!(lane_color("t1", Some(0xFF5B8DEF), false), argb(0xFF5B8DEF));
}

// A take's colour was its place among the takes: duplicating Take 1 turned
// Take 2 from ochre to sage, and a reorder recoloured them the same way.
#[test]
fn a_takes_default_colour_is_its_own_wherever_it_is_listed() {
    let ids: Vec<String> = (0..12).map(|i| format!("take-{i}")).collect();
    let colours: Vec<Color> = ids
        .iter()
        .map(|id| take_color(&take(id, 1000, Vec::new())))
        .collect();
    // The same id, the same colour: nothing else goes into it.
    for (id, colour) in ids.iter().zip(&colours) {
        assert_eq!(take_color(&take(id, 1000, Vec::new())), *colour);
    }
    // And the ids spread over the palette.
    let distinct: std::collections::HashSet<[u8; 4]> =
        colours.iter().map(|c| c.into_rgba8()).collect();
    assert!(distinct.len() >= 3, "{distinct:?}");
}

/// A take with clip "a" over 1–5 s and clip "b" over 4–8 s, on top of it.
fn overlapping() -> &'static AudioTrack {
    leaked(AudioTrack {
        clips: vec![
            AudioClip {
                start_ms: 1000,
                ..AudioClip::new("a", "/a.wav", 4000)
            },
            AudioClip {
                start_ms: 4000,
                ..AudioClip::new("b", "/b.wav", 4000)
            },
        ],
        ..AudioTrack::new("t1", AudioTrackKind::Vocal, "Take 1", created())
    })
}

/// The window's x at `ms` of the 10 s viewport.
fn lane_x(ms: f32) -> f32 {
    TRACK_HEADER_W + ms / VIEWPORT_MS as f32 * LANE_W
}

/// Low in the clips' bodies: under their "Analyzing…", inside their
/// outlines.
const BODY_Y: f32 = TRACK_ROW_HEIGHT - CLIP_INSET - 6.0;

// Overlapping clips were translucent: where "b" lay over "a" the two showed
// through each other, reading as a third clip.
#[test]
fn the_top_clip_hides_the_one_under_it() {
    let track = overlapping();
    let mut ui = LiveUi::new(WINDOW);
    let frame = ui.snapshot(row_of(track));
    let over_a = frame.pixel(lane_x(4500.0) as usize, BODY_Y as usize);
    let b_alone = frame.pixel(lane_x(7500.0) as usize, BODY_Y as usize);
    let a_alone = frame.pixel(lane_x(1500.0) as usize, BODY_Y as usize);
    assert_eq!(over_a, b_alone, "b, opaque, over a");
    assert_eq!(a_alone, b_alone, "the same fill either way");
}

// Selected, the lower clip's outline was hidden where the upper one lay
// over it.
#[test]
fn the_selected_clips_outline_shows_over_the_clip_on_top() {
    let track = overlapping();
    let mut ui = LiveUi::new(WINDOW);
    let frame = ui.snapshot(row_of(track).selected(true).selected_clip_id(Some("a")));
    let near = |p: [u8; 4], c: Color| {
        let c = c.into_rgba8();
        (0..3).all(|i| (i32::from(p[i]) - i32::from(c[i])).abs() <= 2)
    };
    // "a"'s right edge, inside "b".
    let edge = frame.pixel(lane_x(5000.0) as usize - 1, BODY_Y as usize);
    assert!(near(edge, Studio::CLIP_SELECTED_OUTLINE), "{edge:?}");
}

#[test]
fn a_fader_tick_leaves_the_clips_waveforms_cached() {
    // A fader tick copies the take and its clips; the clip's waveform cache
    // is keyed by its peaks' identity, so the copy must hand out the same
    // buffer — or every clip in the lane repaints at pointer rate.
    let track = take("fader-tick", 5000, vec![0.1, 0.5, 0.9]);
    let ticked = AudioTrack {
        volume: 0.3,
        ..track.clone()
    };
    let a = clip_peaks(&track.clips[0]);
    let b = clip_peaks(&ticked.clips[0]);
    assert!(Arc::ptr_eq(&a, &b));
    // New peaks (a re-analysis) are a new buffer.
    let reanalyzed = take("fader-tick", 5000, vec![0.2, 0.5, 0.9]);
    let c = clip_peaks(&reanalyzed.clips[0]);
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(&*c, &[0.2, 0.5, 0.9]);
}

// ------------------------------------------------------ taps and menus

#[test]
fn a_tap_on_the_header_or_the_lanes_empty_stretch_selects_the_lane() {
    let track = clips_take(&[("a".into(), 4000)]);
    let mut ui = sim(row_of(&track));
    ui.click("Take 1").expect("the label");
    ui.point_at(Point::new(780.0, TRACK_ROW_HEIGHT / 2.0));
    let _ = ui.simulate(iced_test::simulator::click());
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::Select, Message::Select]
    );
}

#[test]
fn right_clicks_open_the_lane_and_clip_menus() {
    let track = clips_take(&[("a".into(), 4000)]);
    let mut ui = sim(row_of(&track));
    for at in [
        Point::new(100.0, 20.0),                   // the header
        Point::new(780.0, TRACK_ROW_HEIGHT / 2.0), // the empty lane
        clip_center(4000, 1000),                   // the clip
    ] {
        ui.point_at(at);
        let _ = ui.simulate(right_click());
    }
    // The ⋯ button, on the press.
    let ellipsis = ui.find(TRACK_MENU).expect("the ⋯ button");
    ui.point_at(ellipsis.bounds().center());
    let _ = ui.simulate([press()]);
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![
            Message::ContextMenu,
            Message::ContextMenu,
            Message::ClipContextMenu("a".into()),
            Message::ContextMenu,
        ]
    );
}

#[test]
fn a_note_marker_opens_its_note() {
    let note = PracticeNote {
        id: "n1".into(),
        position_ms: 2000,
        end_ms: None,
        content: "breath".into(),
        created_at: created(),
        updated_at: created(),
    };
    let track = AudioTrack {
        notes: vec![note],
        ..clips_take(&[("a".into(), 1500)])
    };
    let mut ui = sim(row_of(&track));
    // Over the clip, the marker wins.
    let at = Point::new(
        TRACK_HEADER_W + 0.2 * LANE_W,
        TRACK_ROW_HEIGHT - 1.0 - 1.0 - 8.0,
    );
    ui.point_at(at);
    let _ = ui.simulate(iced_test::simulator::click());
    // It takes taps only.
    let _ = ui.simulate(right_click());
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::EditNote("n1".into())]
    );
}

#[test]
fn the_add_buttons_are_for_vocal_takes_only() {
    let track = take("t1", 5000, Vec::new());
    let mut ui = sim(row_of(&track));
    ui.click(ADD_NOTE).expect("add note");
    ui.click(ADD_TECHNIQUE).expect("add technique");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::AddNote, Message::AddTechnique]
    );
    let backing = AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", created())
        .with_audio("/b.wav", 0, 5000, Vec::new());
    let mut ui = sim(track_row(&backing, "Backing", VIEWPORT_MS, 0, |m| m));
    assert!(ui.find(ADD_NOTE).is_err());
    assert!(ui.find(ADD_TECHNIQUE).is_err());
    // The name as typed.
    assert!(ui.find("Backing").is_ok());
}

#[test]
fn the_chevron_folds_the_lanes_and_without_a_toggle_selects_the_lane() {
    let track = take("t1", 5000, Vec::new());
    let mut ui = sim(row_of(&track).expand(true, false, true));
    ui.click(SHOW_LANES).expect("the chevron");
    let mut ui2 = sim(row_of(&track).expand(true, true, false));
    ui2.click(HIDE_LANES).expect("the chevron");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::ToggleExpand]
    );
    assert_eq!(
        ui2.into_messages().collect::<Vec<_>>(),
        vec![Message::Select]
    );
    // No chevron on a take that cannot fold out.
    let mut ui = sim(row_of(&track));
    assert!(ui.find(SHOW_LANES).is_err());
}

#[test]
fn an_expanded_take_shows_its_sub_lanes() {
    let track = take("t1", 5000, Vec::new());
    let mut ui = sim(row_of(&track)
        .expand(true, true, true)
        .sub_lanes(crate::ui::core::text("the pitch lane")));
    let lanes = ui.find("the pitch lane").expect("folded out").bounds();
    assert!(lanes.y >= TRACK_ROW_HEIGHT, "{lanes:?}");
    let mut ui = sim(row_of(&track)
        .expand(true, false, true)
        .sub_lanes(crate::ui::core::text("the pitch lane")));
    assert!(ui.find("the pitch lane").is_err());
}

#[test]
fn the_row_draws_everything_it_can_show() {
    let note = PracticeNote {
        id: "n1".into(),
        position_ms: 2000,
        end_ms: None,
        content: "breath".into(),
        created_at: created(),
        updated_at: created(),
    };
    let mut track = AudioTrack {
        notes: vec![note],
        ..clips_take(&[("a".into(), 1500), ("b".into(), 9500)])
    };
    // One clip still analyzing.
    track.clips.push(AudioClip {
        start_ms: 6000,
        ..AudioClip::new("c", "/c.wav", 1000)
    });
    let live: Vec<f64> = (0..40).map(|i| (i % 7) as f64 / 7.0).collect();
    let mut ui = sim(row_of(&track)
        .selected(true)
        .selected_clip_id(Some("a"))
        .armed(true)
        .view_start_ms(500)
        .recording(&live, 7000)
        .expand(true, true, true)
        .reorderable(true)
        .dropping(true));
    assert!(ui.find("Analyzing…").is_ok());
    ui.snapshot(&app_theme()).expect("it draws");
}

// ------------------------------------------------------- reordering

/// Holds the press past the long-press timeout and lets a frame find it.
fn hold(ui: &mut LiveUi, view: impl FnOnce() -> Element<'static, Message>) -> Vec<Message> {
    std::thread::sleep(LONG_PRESS_TIMEOUT + Duration::from_millis(40));
    ui.run(view(), [redraw()])
}

/// A reorderable take, leaked so the views can be rebuilt freely.
fn leaked(track: AudioTrack) -> &'static AudioTrack {
    Box::leak(Box::new(track))
}

#[test]
fn a_long_press_on_a_reorderable_lane_starts_a_reorder_drag() {
    let track = leaked(clips_take(&[("a".into(), 4000)]));
    let view = || Element::from(row_of(track).reorderable(true));
    let mut ui = LiveUi::new(WINDOW);
    // On the header's label: not yet a tap, nor anything else.
    ui.point_at(Point::new(100.0, 20.0));
    assert!(ui.run(view(), [press()]).is_empty());
    assert_eq!(hold(&mut ui, view), vec![Message::LaneDragStart]);
    // The pointer is the lane's now: no select, nothing beneath reacts.
    assert!(ui.run(view(), [move_to(300.0, 200.0)]).is_empty());
    assert_eq!(ui.run(view(), [release()]), vec![Message::LaneDragEnd]);
    // The header's tap was let go, not completed: a later tap is one.
    ui.point_at(Point::new(100.0, 20.0));
    assert_eq!(ui.run(view(), [press(), release()]), vec![Message::Select]);

    // On the lane beside its clip: the same.
    ui.point_at(Point::new(TRACK_HEADER_W + 40.0, 30.0));
    assert!(ui.run(view(), [press()]).is_empty());
    assert_eq!(hold(&mut ui, view), vec![Message::LaneDragStart]);
    assert_eq!(ui.run(view(), [release()]), vec![Message::LaneDragEnd]);
}

#[test]
fn a_clip_held_still_then_dragged_moves_and_reorders_nothing() {
    // Held for half a second to aim (common on a trackpad), the press
    // became the row's long press: the row faded, a ghost of the take's
    // name followed the pointer, and the clip never moved.
    let track = leaked(clips_take(&[("a".into(), 4000)]));
    let view = || Element::from(row_of(track).reorderable(true));
    let mut ui = LiveUi::new(WINDOW);
    let at = clip_center(4000, 1000);
    ui.point_at(at);
    assert!(ui.run(view(), [press()]).is_empty());
    std::thread::sleep(LONG_PRESS_TIMEOUT + Duration::from_millis(100));
    let mut all = ui.run(view(), [redraw()]);
    for dx in [10.0, 20.0, 30.0] {
        all.extend(ui.run(view(), [move_to(at.x + dx, at.y)]));
    }
    all.extend(ui.run(view(), [release()]));
    assert!(!all.contains(&Message::LaneDragStart), "{all:?}");
    let moves = all
        .iter()
        .filter(|m| matches!(m, Message::ClipMoveLive { clip_id, .. } if clip_id == "a"))
        .count();
    assert_eq!(moves, 2, "{all:?}");
    assert_eq!(all.last(), Some(&Message::MoveCommit), "{all:?}");
}

#[test]
fn a_press_that_moves_or_lands_on_a_control_is_no_reorder() {
    let track = leaked(clips_take(&[("a".into(), 4000)]));
    let view = || Element::from(row_of(track).reorderable(true));
    let mut ui = LiveUi::new(WINDOW);
    // Drifting two pixels before the timeout.
    ui.point_at(Point::new(780.0, 30.0));
    ui.run(view(), [press(), move_to(780.0, 32.0)]);
    assert!(hold(&mut ui, view).is_empty());
    assert_eq!(ui.run(view(), [release()]), vec![Message::Select]);
    // The M key takes its pointer on contact.
    let m = ui.find(view(), "M").expect("the M key").bounds().center();
    ui.point_at(m);
    ui.run(view(), [press()]);
    assert!(hold(&mut ui, view).is_empty());
    assert_eq!(ui.run(view(), [release()]), vec![Message::ToggleMute]);
}

#[test]
fn a_lane_that_is_not_reorderable_never_starts_one() {
    let track = leaked(clips_take(&[("a".into(), 4000)]));
    let view = || Element::from(row_of(track));
    let mut ui = LiveUi::new(WINDOW);
    ui.point_at(Point::new(100.0, 20.0));
    ui.run(view(), [press()]);
    assert!(hold(&mut ui, view).is_empty());
    assert_eq!(ui.run(view(), [release()]), vec![Message::Select]);
}

#[test]
fn the_dragged_lane_shows_its_label_under_the_pointer() {
    let track = leaked(take("t1", 5000, Vec::new()));
    let view = || Element::from(row_of(track).reorderable(true));
    let near = |a: [u8; 4], b: Color, tolerance: i32| {
        let b = b.into_rgba8();
        (0..3).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= tolerance)
    };
    let header =
        |frame: &crate::ui::song_practice_detail::widgets::live_ui::Frame| frame.pixel(120, 20);
    let mut ui = LiveUi::new(WINDOW);
    // The header's name and the clip's.
    assert_eq!(ui.find_all(view(), "Take 1").len(), 2);
    let before = ui.snapshot(view());
    assert!(
        near(header(&before), Studio::PANEL, 1),
        "{:?}",
        header(&before)
    );
    ui.point_at(Point::new(100.0, 20.0));
    ui.run(view(), [press()]);
    assert_eq!(hold(&mut ui, view), vec![Message::LaneDragStart]);
    // The header's and the clip's names, and the ghost's at the press.
    let labels = ui.find_all(view(), "Take 1");
    assert_eq!(labels.len(), 3);
    let ghost = labels[2].bounds();
    assert!(ghost.x > 100.0 && ghost.y > 20.0, "{ghost:?}");

    // The ghost follows the pointer, and the row reads at 35 % meanwhile.
    ui.run(view(), [move_to(500.0, 300.0)]);
    let frame = ui.snapshot(view());
    let veiled = Color {
        r: Studio::PANEL.r * 0.35 + Studio::BACKGROUND.r * 0.65,
        g: Studio::PANEL.g * 0.35 + Studio::BACKGROUND.g * 0.65,
        b: Studio::PANEL.b * 0.35 + Studio::BACKGROUND.b * 0.65,
        a: 1.0,
    };
    assert!(near(header(&frame), veiled, 1), "{:?}", header(&frame));
    // Inside the chip (a menu's card), just under its top-left corner's
    // outline.
    assert!(
        near(frame.pixel(506, 303), Studio::SURFACE, 2),
        "{:?}",
        frame.pixel(506, 303)
    );

    ui.run(view(), [release()]);
    assert_eq!(ui.find_all(view(), "Take 1").len(), 2);
    let after = ui.snapshot(view());
    assert!(near(header(&after), Studio::PANEL, 1));
    assert!(near(after.pixel(506, 303), Studio::BACKGROUND, 1));
}

/// Two reorderable takes, `t1` over `t2`, while the screen says `dragged`
/// is being dragged and `drop_on` is under it; messages tagged by lane.
fn two_lanes<'a>(
    t1: &'a AudioTrack,
    t2: &'a AudioTrack,
    dragged: Option<&str>,
    drop_on: Option<&str>,
) -> Element<'a, (String, Message)> {
    let lane = |t: &'a AudioTrack| {
        let id = t.id.clone();
        Element::from(
            track_row(t, t.display_name.as_str(), VIEWPORT_MS, 0, move |m| {
                (id.clone(), m)
            })
            .reorderable(true)
            .lane_drag(dragged)
            .dropping(drop_on == Some(t.id.as_str())),
        )
    };
    column![lane(t1), lane(t2)].into()
}

#[test]
fn a_lane_dropped_on_another_reports_the_move_to_the_one_below_the_pointer() {
    let t1 = take("t1", 5000, Vec::new());
    let t2 = AudioTrack {
        id: "t2".into(),
        display_name: "Take 2".into(),
        ..take("t2", 5000, Vec::new())
    };
    let tagged = |id: &str, m: Message| (id.to_string(), m);
    let mut ui = LiveUi::new(WINDOW);
    ui.point_at(Point::new(100.0, 20.0));
    ui.run(two_lanes(&t1, &t2, None, None), [press()]);
    std::thread::sleep(LONG_PRESS_TIMEOUT + Duration::from_millis(40));
    assert_eq!(
        ui.run(two_lanes(&t1, &t2, None, None), [redraw()]),
        vec![tagged("t1", Message::LaneDragStart)]
    );
    // The screen marks t1 as dragged on every row. Over t1 itself: nothing
    // (a lane never accepts itself); into t2: over.
    let dragged = Some("t1");
    assert!(ui
        .run(two_lanes(&t1, &t2, dragged, None), [move_to(300.0, 40.0)])
        .is_empty());
    assert_eq!(
        ui.run(
            two_lanes(&t1, &t2, dragged, None),
            [move_to(300.0, TRACK_ROW_HEIGHT + 30.0)]
        ),
        vec![tagged("t2", Message::LaneDragOver(true))]
    );
    // Its drop line: the row's content 2 px lower.
    let shown = |ui: &mut LiveUi, drop_on| {
        ui.find(two_lanes(&t1, &t2, dragged, drop_on), "Take 2")
            .expect("the label")
            .bounds()
            .y
    };
    let plain = shown(&mut ui, None);
    assert!((shown(&mut ui, Some("t2")) - plain - 2.0).abs() < 0.01);
    // Out and back in.
    assert_eq!(
        ui.run(
            two_lanes(&t1, &t2, dragged, Some("t2")),
            [move_to(300.0, 40.0)]
        ),
        vec![tagged("t2", Message::LaneDragOver(false))]
    );
    ui.run(
        two_lanes(&t1, &t2, dragged, None),
        [move_to(300.0, TRACK_ROW_HEIGHT + 30.0)],
    );
    let mut dropped = ui.run(two_lanes(&t1, &t2, dragged, Some("t2")), [release()]);
    dropped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        dropped,
        vec![
            tagged("t1", Message::LaneDragEnd),
            tagged("t2", Message::CrossLaneMove("t1".into())),
        ]
    );
}
