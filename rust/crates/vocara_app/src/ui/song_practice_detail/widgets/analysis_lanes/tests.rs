//! The analysis lanes' tests:
//!
//! - `analysis_lanes_test.dart`: the spectrogram, power and loudness lanes
//!   (and the whole fold-out, the pitch lane among them; its own cases are
//!   `pitch_lane`'s) rendered against the analysis the REAL DSP produces for
//!   degenerate audio and over degenerate spans; and the marker lanes' range
//!   drag (ordered, held in times across a page flip, never from a trackpad
//!   swipe).
//! - The loudness half of `lane_painter_window_test.dart`: the windowed
//!   envelope draws exactly the whole-take pixels, and nothing outside its
//!   box. (The pitch half is `pitch_lane`'s, the formant dots
//!   `spectrogram_painter`'s.)
//! - The loudness half of `analysis_frame_timing_test.dart`: the envelope
//!   swells at the onset, frames drawn at their window centres.
//! - The lane half of `playhead_repaint_test.dart` ("an expanded take
//!   repaints its playheads and the power spectrum per tick, nothing
//!   else"): a playhead tick or a screen notification leaves the loudness
//!   envelope's recording alone. The rest of that file needs the detail
//!   screen.
//! - The lane side of `marker_dialogs_test.dart`: a drag hands the editors
//!   its range in time order (the too-short-range title is the note
//!   editor's).
//!
//! A lane "renders" when the headless renderer draws it — layout, every
//! canvas and every text — without a panic (Flutter's `takeException`).

use std::f64::consts::PI;
use std::panic::{catch_unwind, AssertUnwindSafe};

use iced::advanced::renderer::Headless;
use iced::widget::{column, row};
use iced::{Font, Pixels, Settings};
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::Simulator;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_num::pow_f64;
use vocara_services::audio_dsp::{compute_spectrogram, compute_voice_metrics, AnalysisParams};

use super::*;
use crate::ui::core::theme::app_theme;
use crate::ui::song_practice_detail::widgets::live_spectrogram::tests::{raster, sine, Raster};
use crate::ui::song_practice_detail::widgets::live_ui::{move_to, press, redraw, release, LiveUi};

const SR: i64 = 48000;
const HOP: i64 = 512;
const FFT: i64 = 2048;

fn at() -> DateTime {
    DateTime::utc_ymd(2026, 1, 1)
}

fn track(start_ms: i64, duration_ms: i64) -> AudioTrack {
    AudioTrack::new("t1", AudioTrackKind::Vocal, "Take 1", at()).with_audio(
        "/x.wav",
        start_ms,
        duration_ms,
        Vec::new(),
    )
}

/// The analysis the real DSP produces for `samples`.
struct Analysis {
    metrics: Option<Arc<VoiceMetrics>>,
    spectrogram: Option<Arc<Spectrogram>>,
}

fn analyzed(samples: &[f32]) -> Analysis {
    let params = AnalysisParams::default();
    Analysis {
        metrics: Some(Arc::new(compute_voice_metrics(samples, SR, params))),
        spectrogram: Some(Arc::new(compute_spectrogram(samples, SR, params))),
    }
}

/// The take image for `spec`, rendered (as the screen's task would land it).
fn image_for(spec: Option<&Arc<Spectrogram>>) -> LaneImage {
    let mut image = LaneImage::new();
    if let Some(request) = image.sync_request(spec) {
        image.rendered(request.run());
    }
    image
}

/// Draws `element` in a 1440-wide window `height` tall; panics surface.
fn render(element: Element<'_, Message>, height: f32) {
    let mut ui = Simulator::with_size(Settings::default(), Size::new(1440.0, height), element);
    ui.snapshot(&app_theme()).expect("the lane renders");
}

/// Every lane over one (track, span, analysis) combination, one at a time —
/// a lane that panics must not be masked by a sibling, and the failure names
/// which one — then the whole fold-out with every lane on.
fn pump_lanes(
    track: &AudioTrack,
    timeline_ms: i64,
    analysis: &Analysis,
    view_start_ms: i64,
    position_ms: i64,
    loading: bool,
) {
    let image = image_for(analysis.spectrogram.as_ref());
    fn boxed<'b>(
        f: impl Fn() -> Element<'b, Message> + 'b,
    ) -> Box<dyn Fn() -> Element<'b, Message> + 'b> {
        Box::new(f)
    }
    let lanes = [
        (
            "Spectrogram",
            boxed(|| {
                spectrogram_lane(
                    track,
                    timeline_ms,
                    view_start_ms,
                    position_ms,
                    loading,
                    analysis.spectrogram.clone(),
                    Some(&image),
                )
            }),
        ),
        (
            "Power",
            boxed(|| {
                power_lane(
                    track,
                    position_ms,
                    loading,
                    analysis.spectrogram.as_deref(),
                    analysis.metrics.as_deref(),
                )
            }),
        ),
        (
            "Loudness",
            boxed(|| {
                loudness_lane(
                    track,
                    timeline_ms,
                    view_start_ms,
                    position_ms,
                    loading,
                    analysis.metrics.clone(),
                )
            }),
        ),
        (
            "Notes",
            boxed(|| notes_lane(track, timeline_ms, view_start_ms, position_ms)),
        ),
        (
            "Techniques",
            boxed(|| technique_lane(track, timeline_ms, view_start_ms, position_ms)),
        ),
        (
            "All",
            boxed(|| {
                analysis_lanes(
                    track,
                    AnalysisLane::ALL.into_iter().collect(),
                    timeline_ms,
                    position_ms,
                )
                .view_start_ms(view_start_ms)
                .metrics(loading, analysis.metrics.clone())
                .spectrogram(loading, analysis.spectrogram.clone(), &image)
                .into()
            }),
        ),
    ];
    for (name, lane) in lanes {
        let drawn = catch_unwind(AssertUnwindSafe(|| render(lane(), 900.0)));
        assert!(drawn.is_ok(), "the {name} lane panicked");
    }
}

/// A take carrying a note and a technique marker of each kind, so the marker
/// lanes have chips to place.
fn annotated(start_ms: i64, duration_ms: i64) -> AudioTrack {
    let mut t = track(start_ms, duration_ms);
    t.notes = vec![
        note("n1", 200, None, "breath"),
        note(
            "n2",
            300,
            Some(900),
            "a long phrase that runs on and on and on",
        ),
    ];
    t.technique_markers = vec![
        marker("m1", 100, None, "Twang"),
        marker("m2", 400, Some(800), "Belt"),
    ];
    t
}

fn note(id: &str, start: i64, end: Option<i64>, content: &str) -> PracticeNote {
    PracticeNote {
        id: id.into(),
        position_ms: start,
        end_ms: end,
        content: content.into(),
        created_at: at(),
        updated_at: at(),
    }
}

fn marker(id: &str, start: i64, end: Option<i64>, name: &str) -> TechniqueMarker {
    TechniqueMarker {
        id: id.into(),
        position_ms: start,
        end_ms: end,
        technique_id: "tech".into(),
        technique_name: name.into(),
        created_at: at(),
        updated_at: at(),
    }
}

// ------------------------------------------------- analysis_lanes_test

#[test]
fn a_normal_take_renders_in_every_lane() {
    let analysis = analyzed(&sine(48000, 220.0, SR as f64));
    pump_lanes(&annotated(0, 1000), 20000, &analysis, 0, 250, false);
}

#[test]
fn a_silent_take_renders_in_every_lane() {
    pump_lanes(
        &annotated(0, 1000),
        20000,
        &analyzed(&[0.0; 48000]),
        0,
        250,
        false,
    );
}

#[test]
fn audio_carrying_non_finite_samples_renders_in_every_lane() {
    let mut data = sine(48000, 220.0, SR as f64);
    data[100] = f32::NAN;
    data[200] = f32::INFINITY;
    data[300] = f32::NEG_INFINITY;
    pump_lanes(&annotated(0, 1000), 20000, &analyzed(&data), 0, 250, false);
}

#[test]
fn a_take_stopped_after_one_buffer_renders_in_every_lane() {
    pump_lanes(
        &annotated(0, 5),
        20000,
        &analyzed(&sine(256, 220.0, SR as f64)),
        0,
        250,
        false,
    );
}

#[test]
fn no_analysis_yet_renders_in_every_lane() {
    let none = Analysis {
        metrics: None,
        spectrogram: None,
    };
    pump_lanes(&annotated(0, 1000), 20000, &none, 0, 250, true);
    // Loading hides even analysis already in hand; without it, each lane
    // says what can be analyzed.
    let t = track(0, 1000);
    for lane in [
        spectrogram_lane(&t, 20000, 0, 250, true, None, None),
        power_lane(&t, 250, true, None, None),
        loudness_lane(&t, 20000, 0, 250, true, None),
    ] {
        let mut ui = iced_test::simulator(lane);
        assert!(ui.find(ANALYZING_HINT).is_ok());
    }
    for (lane, hint) in [
        (
            spectrogram_lane(&t, 20000, 0, 250, false, None, None),
            NO_SPECTROGRAM_HINT,
        ),
        (power_lane(&t, 250, false, None, None), NO_POWER_HINT),
        (
            loudness_lane(&t, 20000, 0, 250, false, None),
            NO_LOUDNESS_HINT,
        ),
    ] {
        let mut ui = iced_test::simulator(lane);
        assert!(ui.find(hint).is_ok(), "{hint}");
    }
}

#[test]
fn loudness_with_no_frames_says_what_can_be_analyzed() {
    let empty = analyzed(&[]);
    assert_eq!(empty.metrics.as_ref().map(|m| m.frames), Some(0));
    let mut ui = iced_test::simulator(loudness_lane(
        &track(0, 1000),
        20000,
        0,
        250,
        false,
        empty.metrics,
    ));
    assert!(ui.find(NO_LOUDNESS_HINT).is_ok());
}

#[test]
fn degenerate_spans_render() {
    let analysis = analyzed(&sine(48000, 220.0, SR as f64));
    // A zero-length timeline is not divided by.
    pump_lanes(&annotated(0, 1000), 0, &analysis, 0, 250, false);
    // A zero-length TAKE.
    pump_lanes(&annotated(0, 0), 20000, &analysis, 0, 250, false);
    // A viewport scrolled past the end.
    pump_lanes(&annotated(0, 1000), 20000, &analysis, 999_999, 250, false);
    // A take starting beyond the timeline.
    pump_lanes(&annotated(999_999, 1000), 20000, &analysis, 0, 250, false);
    // The playhead far outside the take (a day in).
    pump_lanes(&annotated(0, 1000), 20000, &analysis, 0, 86_400_000, false);
}

#[test]
fn the_lanes_show_their_labels_and_chips() {
    let analysis = analyzed(&sine(4800, 220.0, SR as f64));
    let image = image_for(analysis.spectrogram.as_ref());
    let t = annotated(0, 1000);
    let lanes = analysis_lanes(&t, AnalysisLane::ALL.into_iter().collect(), 2000, 250)
        .metrics(false, analysis.metrics.clone())
        .spectrogram(false, analysis.spectrogram.clone(), &image);
    let mut ui = Simulator::with_size(
        Settings::default(),
        Size::new(1440.0, 900.0),
        Element::from(lanes),
    );
    for label in [
        // The tabs, which the lanes' header cells repeat.
        "Pitch",
        "Loudness",
        "Spectrogram",
        "Power",
        "Notes",
        "Techniques",
        // The header cells' own.
        "Note at playhead",
        "Power spectrum",
        "At the playhead",
        "2 notes",
        "2 markers",
        // The markers.
        "breath",
        "Twang",
        "Belt",
    ] {
        assert!(ui.find(label).is_ok(), "{label}");
    }
    // Only the lanes that are on.
    let only_notes = analysis_lanes(&t, BTreeSet::from([AnalysisLane::Notes]), 2000, 250);
    let mut ui = iced_test::simulator(Element::from(only_notes));
    assert!(ui.find("2 notes").is_ok());
    assert!(ui.find("Power spectrum").is_err());
    assert!(ui.find("2 markers").is_err());
    assert!(ui.find("Loudness").is_ok(), "every tab, on or off");
}

#[test]
fn a_chip_toggles_its_lane() {
    let t = track(0, 1000);
    let mut ui = iced_test::simulator(Element::from(analysis_lanes(
        &t,
        BTreeSet::from([AnalysisLane::Pitch]),
        2000,
        0,
    )));
    let _ = ui.click("Power").expect("the Power chip");
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(matches!(
        messages.as_slice(),
        [Message::ToggleLane {
            track_id,
            lane: AnalysisLane::Power
        }] if track_id == "t1"
    ));
}

#[test]
fn a_lane_lacking_data_asks_once_per_revision_after_its_frame() {
    let t = track(0, 1000);
    let view = |lacks: bool, revision: u64| {
        Element::from(
            analysis_lanes(&t, BTreeSet::from([AnalysisLane::Pitch]), 2000, 0)
                .lacks_data(lacks, revision),
        )
    };
    let mut ui = LiveUi::new(Size::new(1440.0, 400.0));
    let heals = |messages: Vec<Message>| {
        messages
            .into_iter()
            .filter(|m| matches!(m, Message::HealLane(id) if id == "t1"))
            .count()
    };
    assert_eq!(heals(ui.run(view(false, 1), [redraw()])), 0);
    // Built lacking: asked after the frame, not during the build.
    assert_eq!(heals(ui.run(view(true, 1), [])), 0);
    assert_eq!(heals(ui.run(view(true, 1), [redraw()])), 1);
    // A rebuild with nothing new asks nothing.
    assert_eq!(heals(ui.run(view(true, 1), [redraw(), redraw()])), 0);
    // A notification of the screen rebuilds the lanes, which ask again.
    assert_eq!(heals(ui.run(view(true, 2), [redraw()])), 1);
}

#[test]
fn a_note_chip_edits_its_note_and_a_technique_chip_its_marker() {
    let t = annotated(0, 1000);
    let mut ui = Simulator::with_size(
        Settings::default(),
        Size::new(1440.0, 200.0),
        column![notes_lane(&t, 2000, 0, 0), technique_lane(&t, 2000, 0, 0)],
    );
    let _ = ui.click("breath").expect("the note's chip");
    let _ = ui.click("Belt").expect("the marker's chip");
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(matches!(
        messages.as_slice(),
        [Message::EditNote(n), Message::EditTechnique(m)] if n == "n1" && m == "m2"
    ));
}

// ------------------------------------------- the range drag (marker lanes)

/// The notes lane over a minute-long take, a 10-second window from
/// `view_start` (follow-cursor moves it mid-gesture), in a 1440 × 56 window.
fn notes_view<'a>(t: &'a AudioTrack, view_start: i64) -> Element<'a, Message> {
    notes_lane(t, 10000, view_start, 0)
}

const LANE_W: f32 = 1440.0;
const LANE_CY: f32 = 28.0;

/// What a press at `from`, two moves of `by / 2` and a release added; the
/// release lands on `release_view`'s lane.
fn drag(ui: &mut LiveUi, t: &AudioTrack, from: f32, by: f32, release_view: i64) -> Vec<Message> {
    let mut out = ui.run(
        notes_view(t, 0),
        [
            move_to(from, LANE_CY),
            press(),
            move_to(from + by / 2.0, LANE_CY),
            move_to(from + by, LANE_CY),
        ],
    );
    // A frame between the moves and the release, as the app draws one.
    out.extend(ui.run(notes_view(t, release_view), [redraw(), release()]));
    out
}

fn added(messages: Vec<Message>) -> Vec<(i64, Option<i64>)> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            Message::AddNote { start_ms, end_ms } => Some((start_ms, end_ms)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_right_to_left_drag_commits_an_ordered_range() {
    let t = track(0, 60000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let added = added(drag(&mut ui, &t, LANE_W - 100.0, -400.0, 0));
    let [(start, Some(end))] = added.as_slice() else {
        panic!("one range: {added:?}");
    };
    assert!(start < end);
}

#[test]
fn a_page_flip_while_the_drag_is_held_commits_the_range_that_was_dragged_over() {
    let t = track(0, 60000);
    let from = TRACK_HEADER_W + (LANE_W - TRACK_HEADER_W) / 2.0 - 200.0;
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let dragged = added(drag(&mut ui, &t, from, 240.0, 0));
    assert_eq!(dragged.len(), 1);
    // The playhead reached the window's end: follow-cursor pages on.
    let flipped = added(drag(&mut ui, &t, from, 240.0, 7800));
    assert_eq!(flipped, dragged);
}

#[test]
fn a_two_finger_trackpad_swipe_adds_nothing() {
    // It reaches iced as a wheel scroll, and pans the timeline; over a
    // marker lane it once opened the editor for a range nobody drew.
    use crate::ui::core::widgets::pan_zoom::testing as pan_zoom;

    let t = track(0, 60000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let mut sent = vec![(move_to(800.0, LANE_CY), None)];
    sent.extend(pan_zoom::swipe(
        &[(-60.0, 0.0), (-240.0, 0.0)],
        iced::time::Instant::now(),
    ));
    sent.push((redraw(), None));
    let messages = ui.run_sent(notes_view(&t, 0), sent);
    assert!(added(messages).is_empty());
}

#[test]
fn a_mouse_drag_still_adds_its_range() {
    let t = track(0, 60000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let added = added(drag(&mut ui, &t, 800.0, 240.0, 0));
    let [(start, Some(end))] = added.as_slice() else {
        panic!("one range: {added:?}");
    };
    assert!(end > start);
}

#[test]
fn a_tap_on_the_lane_adds_a_point_where_it_landed() {
    let t = track(0, 60000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let body_w = f64::from(LANE_W - TRACK_HEADER_W);
    let x = TRACK_HEADER_W + 300.0;
    let messages = ui.run(
        notes_view(&t, 2000),
        [move_to(x, LANE_CY), press(), release()],
    );
    assert_eq!(
        added(messages),
        vec![(lane_ms_of(300.0, 10000, 2000, body_w), None)]
    );
    // A press that wanders off vertically past the tap slop adds nothing.
    let messages = ui.run(
        notes_view(&t, 2000),
        [move_to(x, 5.0), press(), move_to(x, 50.0), release()],
    );
    assert!(added(messages).is_empty());
}

#[test]
fn the_technique_lane_adds_techniques() {
    let t = track(0, 60000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    let messages = ui.run(
        technique_lane(&t, 10000, 0, 0),
        [
            move_to(600.0, LANE_CY),
            press(),
            move_to(700.0, LANE_CY),
            move_to(800.0, LANE_CY),
            release(),
        ],
    );
    assert!(matches!(
        messages.as_slice(),
        [Message::AddTechnique { start_ms, end_ms: Some(end) }] if start_ms < end
    ));
}

#[test]
fn a_press_on_a_chip_is_the_chips_not_the_lanes() {
    let t = annotated(0, 1000);
    let mut ui = LiveUi::new(Size::new(LANE_W, 56.0));
    // The point note at 200 ms of a 2 s window: its pill starts there.
    let x = TRACK_HEADER_W + (LANE_W - TRACK_HEADER_W) * 0.1 + 10.0;
    let messages = ui.run(
        notes_lane(&t, 2000, 0, 0),
        [move_to(x, LANE_CY), press(), release()],
    );
    assert!(matches!(messages.as_slice(), [Message::EditNote(id)] if id == "n1"));
}

// --------------------------------------------------- pure geometry

#[test]
fn ranges_are_ordered_and_the_highlight_is_clamped_to_the_box() {
    assert_eq!(ordered_range(500, 200), (200, 500));
    assert_eq!(ordered_range(200, 500), (200, 500));
    assert_eq!(ordered_range(300, 300), (300, 300));
    // 1 px per 10 ms on a 1000 px lane from 0.
    assert_eq!(
        drag_highlight((1000, 3000), 10000, 0, 1000.0),
        (100.0, 200.0)
    );
    // Scrolled partly out: clamped to the box, at least a pixel.
    assert_eq!(
        drag_highlight((-5000, 3000), 10000, 0, 1000.0),
        (0.0, 300.0)
    );
    assert_eq!(drag_highlight((3000, 3000), 10000, 0, 1000.0), (300.0, 1.0));
    assert_eq!(
        drag_highlight((50000, 60000), 10000, 0, 1000.0),
        (1000.0, 1.0)
    );
}

#[test]
fn markers_are_culled_past_a_margin_and_ranges_keep_their_label_in_view() {
    // A point 15 px left of the view is still placed; 25 px is not.
    assert_eq!(
        marker_box(-150, None, 10000, 0, 1000.0),
        Some(MarkerBox::Point { left: -15.0 })
    );
    assert_eq!(marker_box(-250, None, 10000, 0, 1000.0), None);
    assert_eq!(marker_box(10250, None, 10000, 0, 1000.0), None);
    // A range beginning off-screen keeps its left edge (icon and label) at
    // the viewport's edge.
    assert_eq!(
        marker_box(-5000, Some(2000), 10000, 0, 1000.0),
        Some(MarkerBox::Range {
            left: 0.0,
            width: 200.0
        })
    );
    // At least 22 px wide.
    assert_eq!(
        marker_box(1000, Some(1010), 10000, 0, 1000.0),
        Some(MarkerBox::Range {
            left: 100.0,
            width: 22.0
        })
    );
    // An end not past the start is a point.
    assert_eq!(
        marker_box(1000, Some(1000), 10000, 0, 1000.0),
        Some(MarkerBox::Point { left: 100.0 })
    );
    assert_eq!(marker_box(-9000, Some(-3000), 10000, 0, 1000.0), None);
}

#[test]
fn the_lane_maps_times_and_pixels_both_ways() {
    assert_eq!(lane_x_of(2500, 10000, 500, 1000.0), 200.0);
    assert_eq!(lane_ms_of(200.0, 10000, 500, 1000.0), 2500);
    // No timeline: nothing to divide by.
    assert_eq!(lane_x_of(2500, 0, 500, 1000.0), 0.0);
    assert_eq!(lane_ms_of(200.0, 0, 500, 1000.0), 0);
    // The take box: at least a pixel.
    assert_eq!(take_box(1000, 2000, 10000, 0, 1000.0), (100.0, 200.0));
    assert_eq!(take_box(1000, 0, 10000, 0, 1000.0), (100.0, 1.0));
    // Zoomed in, the box runs past both edges at the take's own scale.
    assert_eq!(take_box(0, 60000, 1000, 30000, 1000.0), (-30000.0, 60000.0));
}

#[test]
fn the_power_lane_reads_the_frame_under_the_playhead_and_nothing_outside_the_take() {
    let analysis = analyzed(&sine(48000, 220.0, SR as f64));
    let spec = analysis.spectrogram.as_deref().expect("spectrogram");
    let m = analysis.metrics.as_deref().expect("metrics");
    // A take placed at 2 s: 2.25 s is 250 ms into it.
    let (column, formants) = playhead_power(2250, 2000, 1000, spec, Some(m));
    assert_eq!(&*column, spec.column_at_time(0.25));
    let fr = m.frame_at_time(0.25);
    assert_eq!(formants.len(), 3);
    for (got, want) in formants.iter().zip([m.f1[fr], m.f2[fr], m.f3[fr]]) {
        assert!(got.to_bits() == f64::from(want).to_bits());
    }
    // Before and after the take: an empty spectrum, not its edge frame.
    for position in [1999, 3001, 86_400_000] {
        let (column, formants) = playhead_power(position, 2000, 1000, spec, Some(m));
        assert!(column.is_empty() && formants.is_empty(), "{position}");
    }
    // No metrics: the spectrum alone.
    let (column, formants) = playhead_power(2250, 2000, 1000, spec, None);
    assert!(!column.is_empty() && formants.is_empty());
}

#[test]
fn the_peak_is_the_loudest_finite_frame_found_once_per_take() {
    let m = Arc::new(loudness_metrics(100));
    let fresh = loudness_peak(&m);
    assert_eq!(loudness_peak_of(&m), fresh);
    assert!(fresh.is_some_and(f64::is_finite));
    // A copy is another take, with its own entry.
    let other = Arc::new(VoiceMetrics::clone(&m));
    assert_eq!(loudness_peak_of(&other), fresh);
    // Nothing finite: no peak, and no envelope.
    let mut silent = loudness_metrics(10);
    silent.loudness = vec![f32::NAN, f32::NEG_INFINITY, f32::INFINITY]
        .into_iter()
        .cycle()
        .take(10)
        .collect();
    let silent = Arc::new(silent);
    assert_eq!(loudness_peak_of(&silent), None);
    let p = LoudnessLanePainter {
        metrics: silent,
        timeline_duration_ms: 1000,
        start_ms: 0,
        view_start_ms: 0,
    };
    assert_eq!(p.shape(LANE), None);
}

#[test]
fn a_nan_gap_breaks_the_envelope_into_runs() {
    let mut m = loudness_metrics(60);
    for v in &mut m.loudness[20..25] {
        *v = f32::NAN;
    }
    let p = LoudnessLanePainter {
        metrics: Arc::new(m),
        timeline_duration_ms: 1000,
        start_ms: 0,
        view_start_ms: 0,
    };
    let shape = p.shape(LANE).expect("an envelope");
    assert_eq!(shape.lines.len(), 2);
    assert_eq!(shape.areas.len(), 2);
    assert_eq!(shape.lines[0].len(), 20);
    assert_eq!(shape.lines[1].len(), 35);
    for (area, line) in shape.areas.iter().zip(&shape.lines) {
        // Down to the bottom at either end of its run.
        let h = f64::from(LANE.height);
        assert_eq!(area.first(), Some(&(line[0].0, h)));
        assert_eq!(area.last(), Some(&(line[line.len() - 1].0, h)));
        assert_eq!(&area[1..area.len() - 1], line.as_slice());
    }
}

#[test]
fn a_lane_image_follows_the_spectrogram_it_shows() {
    let a = analyzed(&sine(4800, 220.0, SR as f64))
        .spectrogram
        .expect("spec");
    let b = analyzed(&sine(4800, 330.0, SR as f64))
        .spectrogram
        .expect("spec");
    let mut image = LaneImage::new();
    let first = image.sync_request(Some(&a)).expect("a render");
    assert!(image.shows(&a));
    assert!(
        image.sync_request(Some(&a)).is_none(),
        "the same one is kept"
    );
    // Another take's render landing late is dropped.
    let _second = image.sync_request(Some(&b)).expect("a render");
    image.rendered(first.run());
    assert!(image.shows(&b) && !image.shows(&a));
    assert!(image.image.image().is_none());
    // Folded in: let go.
    assert!(image.sync_request(None).is_none());
    assert!(!image.shows(&b));
}

// ------------------------------------------ lane_painter_window_test

const LANE: Size = Size {
    width: 300.0,
    height: 96.0,
};

/// `frames` of synthetic analysis: loudness swinging ±10 dB around −30
/// dBFS, with a NaN gap (as between the clips of a multi-clip take) every
/// 900 frames.
fn loudness_metrics(frames: usize) -> VoiceMetrics {
    let curve = |f: &dyn Fn(usize) -> f64| (0..frames).map(|i| f(i) as f32).collect::<Vec<_>>();
    let voiced = |i: usize| i % 47 < 33;
    VoiceMetrics {
        frames,
        hop: HOP,
        fft_size: FFT,
        sample_rate: SR,
        ring: vec![0.0; frames],
        tilt: vec![0.0; frames],
        cpp: vec![0.0; frames],
        loudness: curve(&|i| {
            if i % 900 > 860 {
                f64::NAN
            } else {
                -30.0 + 10.0 * (i as f64 / 40.0).sin()
            }
        }),
        f0: curve(&|i| {
            if voiced(i) {
                220.0
                    * pow_f64(2.0, ((i / 47) % 12) as f64 / 12.0)
                    * (1.0 + 0.01 * (i as f64).sin())
            } else {
                f64::NAN
            }
        }),
        f1: vec![f32::NAN; frames],
        f2: vec![f32::NAN; frames],
        f3: vec![f32::NAN; frames],
        h1h2: vec![f32::NAN; frames],
        active: (0..frames).map(|i| u8::from(voiced(i))).collect(),
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    }
}

/// The loudness lane exactly as it painted before windowing: every frame of
/// the take, as a frozen reference (`_WholeTakeLoudness`).
struct WholeTakeLoudness(LoudnessLanePainter);

impl canvas::Program<Message> for WholeTakeLoudness {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let p = &self.0;
        let (dur, m) = (p.timeline_duration_ms, &p.metrics);
        if dur <= 0 || m.frames == 0 {
            return vec![frame.into_geometry()];
        }
        let mut peak = -1e9;
        for &v in &m.loudness {
            let v = f64::from(v);
            if v.is_finite() && v > peak {
                peak = v;
            }
        }
        let floor = peak - 48.0;
        let (w, h) = (f64::from(bounds.width), f64::from(bounds.height));
        let x_of = |ms: f64| (ms - p.view_start_ms as f64) / dur as f64 * w;
        let y_of = |db: f64| (1.0 - ((db - floor) / 48.0).clamp(0.0, 1.0)) * h;
        let pt = |x: f64, y: f64| Point::new(x as f32, y as f32);
        let mut runs: Vec<Vec<(f64, f64)>> = vec![Vec::new()];
        for fr in 0..m.frames {
            let db = f64::from(m.loudness[fr]);
            if !db.is_finite() {
                if !runs.last().is_some_and(Vec::is_empty) {
                    runs.push(Vec::new());
                }
                continue;
            }
            let x = x_of(p.start_ms as f64 + m.time_of_frame(fr as i64) * 1000.0);
            if let Some(run) = runs.last_mut() {
                run.push((x, y_of(db)));
            }
        }
        runs.retain(|r| !r.is_empty());
        let area = Path::new(|b| {
            for run in &runs {
                b.move_to(pt(run[0].0, h));
                for &(x, y) in run {
                    b.line_to(pt(x, y));
                }
                b.line_to(pt(run[run.len() - 1].0, h));
                b.close();
            }
        });
        let line = Path::new(|b| {
            for run in &runs {
                b.move_to(pt(run[0].0, run[0].1));
                for &(x, y) in &run[1..] {
                    b.line_to(pt(x, y));
                }
            }
        });
        frame.fill(&area, alpha(Studio::ACCENT, 0.18));
        frame.stroke(
            &line,
            Stroke::default()
                .with_color(alpha(Studio::ACCENT, 0.85))
                .with_width(1.5),
        );
        vec![frame.into_geometry()]
    }
}

/// One minute of synthetic analysis at 48 kHz / 512, its loudness painter
/// over a window.
fn window_painter(dur_ms: i64, view_start_ms: i64, start_ms: i64) -> LoudnessLanePainter {
    LoudnessLanePainter {
        metrics: Arc::new(loudness_metrics(5625)),
        timeline_duration_ms: dur_ms,
        start_ms,
        view_start_ms,
    }
}

/// Windows across the take: the whole of it, deep zoom (a 1-second window),
/// deeper still (a quarter second, whose 8 px margin is narrower than the
/// half window each frame is drawn past its start, so the frame window must
/// bracket frames by their CENTRES), either end, a window cutting a
/// loudness gap, a take placed later on the timeline, and a window past the
/// take's end: `(dur_ms, view_start_ms, start_ms)`.
const WINDOWS: [(i64, i64, i64); 8] = [
    (70000, 0, 0),
    (1000, 30017, 0),
    (250, 30025, 0),
    (5000, 0, 0),
    (4000, 57000, 0),
    (2300, 9411, 0),
    (6000, 41000, 12000),
    (5000, 64000, 0),
];

fn rasterize(element: Element<'_, Message>, size: Size) -> Raster {
    let mut ui = Simulator::with_size(Settings::default(), size, element);
    raster(&mut ui)
}

#[test]
fn the_windowed_loudness_lane_draws_exactly_the_whole_take_pixels() {
    for (dur, view, start) in WINDOWS {
        let p = window_painter(dur, view, start);
        let want = rasterize(
            canvas_widget(WholeTakeLoudness(p.clone()))
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            LANE,
        );
        let got = rasterize(p.view(), LANE);
        assert_eq!(got.rgba.len(), want.rgba.len());
        let differing = got
            .rgba
            .iter()
            .zip(&want.rgba)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differing, 0, "({dur}, {view}, {start})");
    }
}

#[test]
fn a_zoomed_loudness_lane_paints_nothing_outside_its_box() {
    let inset = 100.0;
    let p = window_painter(5000, 25000, 0);
    let view = row![
        Space::new().width(inset),
        container(p.view()).width(LANE.width).height(LANE.height),
        Space::new().width(inset),
    ];
    let r = rasterize(
        view.into(),
        Size::new(LANE.width + 2.0 * inset, LANE.height),
    );
    let background = Studio::BACKGROUND.into_rgba8();
    // The snapshot may be at a multiple of the logical size.
    let scale = r.width as f32 / (LANE.width + 2.0 * inset);
    let (lo, hi) = (
        (inset * scale) as usize,
        ((inset + LANE.width) * scale) as usize,
    );
    let (mut inside, mut outside) = (0, 0);
    for (i, px) in r.pixels().enumerate() {
        if px == background {
            continue;
        }
        let x = i % r.width;
        if x < lo || x >= hi {
            outside += 1;
        } else {
            inside += 1;
        }
    }
    assert!(inside > 0, "the window shows the take");
    assert_eq!(outside, 0, "the rest of the take stays off screen");
}

// --------------------------------------- analysis_frame_timing_test

/// Silence, then a steady 220 Hz tone from exactly 500 ms, 1.2 s in all.
fn burst() -> Vec<f32> {
    let n = (1200 * SR / 1000) as usize;
    let onset = (500 * SR / 1000) as usize;
    (0..n)
        .map(|i| {
            if i < onset {
                0.0
            } else {
                (0.3 * (2.0 * PI * 220.0 * (i - onset) as f64 / SR as f64).sin()) as f32
            }
        })
        .collect()
}

/// The first x along `run` at or above the height `y` (smaller y =
/// higher), walking it in quarter-pixel steps.
fn first_x_at_or_above(runs: &[Vec<(f64, f64)>], y: f64) -> f64 {
    for run in runs {
        for pair in run.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            let len = (x1 - x0).hypot(y1 - y0);
            let mut d = 0.0;
            while d <= len {
                let t = if len == 0.0 { 0.0 } else { d / len };
                let (px, py) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                if py <= y {
                    return px;
                }
                d += 0.25;
            }
        }
    }
    f64::NAN
}

#[test]
fn the_loudness_lane_swells_at_500_ms() {
    // 1 px per ms: x reads directly as a time.
    let lane = Size::new(1200.0, 96.0);
    let m = Arc::new(compute_voice_metrics(
        &burst(),
        SR,
        AnalysisParams::default(),
    ));
    let shape = LoudnessLanePainter {
        metrics: m,
        timeline_duration_ms: 1200,
        start_ms: 0,
        view_start_ms: 0,
    }
    .shape(lane)
    .expect("an envelope");
    // The painter auto-ranges 48 dB under the take's peak (the steady tone);
    // find where the envelope first comes within 3 dB of it.
    let y3 = 3.0103 / 48.0 * f64::from(lane.height);
    let hop_ms = HOP as f64 / SR as f64 * 1000.0;
    let x = first_x_at_or_above(&shape.lines, y3);
    assert!((x - 500.0).abs() <= hop_ms, "swells at {x}");
}

// ------------------------------------------- playhead_repaint_test

fn renderer() -> iced::Renderer {
    iced_test::futures::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::with_name("Fira Sans"),
        Pixels(16.0),
        None,
    ))
    .expect("a headless renderer")
}

fn loudness_records() -> usize {
    LOUDNESS_RECORDS.with(std::cell::Cell::get)
}

/// Rebuilds a view across frames as the app does — a new view each frame,
/// diffed into the last frame's widget tree — and counts what each frame's
/// loudness envelopes recorded.
struct Frames {
    renderer: iced::Renderer,
    cache: user_interface::Cache,
}

impl Frames {
    fn new() -> Self {
        Self {
            renderer: renderer(),
            cache: user_interface::Cache::default(),
        }
    }

    fn draw(&mut self, view: Element<'_, Message>) -> usize {
        let before = loudness_records();
        let mut ui = UserInterface::build(
            view,
            Size::new(1440.0, 600.0),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        ui.draw(
            &mut self.renderer,
            &app_theme(),
            &iced::advanced::renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        self.cache = ui.into_cache();
        loudness_records() - before
    }
}

#[test]
fn a_playhead_tick_redraws_the_playheads_and_the_power_spectrum_not_the_envelope() {
    let analysis = analyzed(&sine(48000, 220.0, SR as f64));
    let image = image_for(analysis.spectrogram.as_ref());
    let t = track(0, 1000);
    let lanes = BTreeSet::from([
        AnalysisLane::Pitch,
        AnalysisLane::Loudness,
        AnalysisLane::Power,
        AnalysisLane::Spectrogram,
    ]);
    let view = |position_ms: i64, metrics: Option<Arc<VoiceMetrics>>| -> Element<'_, Message> {
        analysis_lanes(&t, lanes.clone(), 2000, position_ms)
            .metrics(false, metrics)
            .spectrogram(false, analysis.spectrogram.clone(), &image)
            .into()
    };
    let mut frames = Frames::new();
    assert_eq!(frames.draw(view(100, analysis.metrics.clone())), 1);
    for ms in (116..=260).step_by(16) {
        assert_eq!(
            frames.draw(view(ms, analysis.metrics.clone())),
            0,
            "a tick at {ms} ms"
        );
    }
    // A notification of the screen with nothing new for the lane.
    assert_eq!(frames.draw(view(260, analysis.metrics.clone())), 0);
    // A new analysis (a new Arc) is drawn again.
    let again = analyzed(&sine(48000, 220.0, SR as f64)).metrics;
    assert_eq!(frames.draw(view(260, again)), 1, "a new analysis redraws");
}
