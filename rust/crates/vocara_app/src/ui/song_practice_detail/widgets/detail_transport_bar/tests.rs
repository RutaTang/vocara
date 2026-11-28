//! The transport bar's tests: `detail_transport_bar_test.dart` (the gating
//! of go to start / play / stop / loop / follow / Record, the play icon,
//! the zoom icons, the counts, a long title, the busy spinner), the time
//! display's format and fixed width, the zoom slider, and the bar's layout
//! through its pure functions.
//!
//! What is worth pinning is the GATING, because every gate here protects a
//! state the engine cannot recover from on its own: play and stop are dead
//! with nothing loaded, play is dead WHILE RECORDING (starting playback into
//! a live take is how you get the backing bleeding into the capture), and
//! the loop toggle is dead until a loop region exists. A gate that silently
//! opens is not visible anywhere below this widget.
//!
//! The icon-only controls are found and clicked by their tooltips, which
//! are their labels (`controls::labelled`).

use std::sync::Arc;

use iced::{mouse, Event, Settings};
use iced_test::Simulator;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::util::dart_date_time::DateTime;

use super::*;
use crate::ui::core::widgets::controls::{thin_slider_value, GROUP_BUTTON_WIDTH};
use crate::ui::core::widgets::studio_text::{testing, Spec};

/// A real window, not a small default: the transport is centred and the
/// title and zoom control get the room either side of it.
const WINDOW: Size = Size {
    width: 1440.0,
    height: 900.0,
};

fn take(id: &str) -> Arc<AudioTrack> {
    Arc::new(
        AudioTrack::new(id, AudioTrackKind::Vocal, id, DateTime::utc_ymd(2026, 1, 1)).with_audio(
            format!("/{id}.wav"),
            0,
            4000,
            Vec::new(),
        ),
    )
}

fn practice(takes: &[&str], title: &str) -> SongPractice {
    let mut p = SongPractice::new(
        "p1",
        title,
        DateTime::utc_ymd(2026, 1, 1),
        DateTime::utc_ymd(2026, 1, 1),
    );
    p.vocal_tracks = takes.iter().map(|id| take(id)).collect();
    p.timeline_duration_ms = 30_000;
    p
}

fn bar(practice: &SongPractice) -> TransportBar<'_> {
    TransportBar {
        practice,
        visible_note_count: 0,
        is_playing: false,
        is_recording: false,
        is_starting_take: false,
        loop_enabled: false,
        has_loop: true,
        follow_cursor: true,
        timeline_zoom: 0.5,
        position: Duration::ZERO,
        input_monitor_enabled: false,
        busy: false,
    }
}

fn pump(bar: TransportBar<'_>) -> Simulator<'_, Message> {
    Simulator::with_size(Settings::default(), WINDOW, detail_transport_bar(bar))
}

/// What a click on the control labelled `label` reports.
fn tap(bar: TransportBar<'_>, label: &str) -> Vec<Message> {
    let mut ui = pump(bar);
    ui.click(label).expect("the control is on the bar");
    ui.into_messages().collect()
}

fn bounds_of(ui: &mut Simulator<'_, Message>, label: &str) -> Rectangle {
    ui.find(label).expect("on the bar").bounds()
}

// ------------------------------------------------ detail_transport_bar_test

#[test]
fn an_empty_practice_has_play_stop_and_go_to_start_dead() {
    let p = practice(&[], "Hai Kuo");
    assert!(tap(bar(&p), "Play").is_empty());
    assert!(tap(bar(&p), "Stop").is_empty());
    assert!(tap(bar(&p), "Go to start").is_empty());
    // Follow-playhead is a view preference, not a transport action.
    assert_eq!(
        tap(bar(&p), "Follow playhead"),
        vec![Message::ToggleFollowCursor]
    );
}

#[test]
fn a_practice_with_a_take_can_play_stop_and_go_to_start() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap(bar(&p), "Play"), vec![Message::TogglePlay]);
    assert_eq!(tap(bar(&p), "Stop"), vec![Message::Stop]);
    assert_eq!(tap(bar(&p), "Go to start"), vec![Message::GoToStart]);
}

#[test]
fn play_and_go_to_start_are_dead_while_recording_even_with_takes_loaded() {
    let p = practice(&["t1"], "Hai Kuo");
    let recording = TransportBar {
        is_recording: true,
        ..bar(&p)
    };
    // Starting playback into a live take bleeds the backing into it;
    // jumping the backing misaligns the rest of the take.
    assert!(tap(recording, "Play").is_empty());
    assert!(tap(recording, "Go to start").is_empty());
}

#[test]
fn play_is_dead_while_record_is_still_arming() {
    // isRecording flips only once the mic delivers: Play in that window
    // started the backing, which the take then jumped back to its start.
    let p = practice(&["t1"], "Hai Kuo");
    let arming = TransportBar {
        is_starting_take: true,
        ..bar(&p)
    };
    assert!(tap(arming, "Play").is_empty());
}

#[test]
fn stop_while_recording_ends_the_take_not_the_transport() {
    // The transport stop paused and rewound the backing under a mic that
    // kept recording, misaligning everything sung after it.
    let p = practice(&["t1"], "Hai Kuo");
    let recording = TransportBar {
        is_recording: true,
        ..bar(&p)
    };
    assert_eq!(tap(recording, "Stop"), vec![Message::StopRecording]);
}

#[test]
fn the_loop_toggle_is_dead_until_a_loop_region_exists() {
    let p = practice(&["t1"], "Hai Kuo");
    let no_region = TransportBar {
        has_loop: false,
        ..bar(&p)
    };
    assert!(tap(no_region, "Loop").is_empty());
    assert_eq!(tap(bar(&p), "Loop"), vec![Message::ToggleLoop]);
}

#[test]
fn input_monitoring_toggles() {
    let p = practice(&[], "Hai Kuo");
    assert_eq!(
        tap(bar(&p), "Input monitoring"),
        vec![Message::ToggleInputMonitor]
    );
}

#[test]
fn the_play_icon_follows_the_transport_state() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    assert!(ui.find("Play").is_ok());
    assert!(ui.find("Pause").is_err());

    let playing = TransportBar {
        is_playing: true,
        ..bar(&p)
    };
    let mut ui = pump(playing);
    assert!(ui.find("Pause").is_ok());
    assert!(ui.find("Play").is_err());
}

#[test]
fn record_starts_a_take_and_stops_the_one_in_flight() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap(bar(&p), RECORD_LABEL), vec![Message::StartRecording]);
    let live = TransportBar {
        is_recording: true,
        ..bar(&p)
    };
    // A live take never restarts.
    assert_eq!(tap(live, RECORD_LABEL), vec![Message::StopRecording]);
}

#[test]
fn stop_and_record_while_the_take_is_still_arming_cancel_it() {
    // isRecording flips only once the mic delivers (permission, take setup
    // and the ready handshake come first), so a Stop there only stopped the
    // transport and the take rolled moments later.
    let p = practice(&["t1"], "Hai Kuo");
    let arming = TransportBar {
        is_starting_take: true,
        ..bar(&p)
    };
    assert_eq!(tap(arming, "Stop"), vec![Message::StopRecording]);
    assert_eq!(tap(arming, RECORD_LABEL), vec![Message::StopRecording]);
}

#[test]
fn the_zoom_icons_step_the_timeline_out_and_in() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap(bar(&p), "Zoom out"), vec![Message::ZoomTimeline(1.25)]);
    assert_eq!(tap(bar(&p), "Zoom in"), vec![Message::ZoomTimeline(0.8)]);
}

#[test]
fn back_goes_back() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap(bar(&p), "Back"), vec![Message::Back]);
}

#[test]
fn the_counts_read_from_the_mixer_singular_at_one_take() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(TransportBar {
        visible_note_count: 3,
        ..bar(&p)
    });
    assert!(ui.find("1 take · 3 notes").is_ok());

    let p = practice(&["t1", "t2"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    assert!(ui.find("2 takes · 0 notes").is_ok());
}

// The title had no width of its own: a song title longer than the room
// beside the centred transport ran under the play and stop buttons, and a
// very long one overflowed the bar.
const LONG: &str = "A Considerably Longer Song Title Than Most";

/// The transport group's left edge: the go-to-start button's.
fn transport_left(ui: &mut Simulator<'_, Message>) -> f32 {
    let start = bounds_of(ui, "Go to start");
    start.center_x() - GROUP_BUTTON_WIDTH / 2.0
}

#[test]
fn a_long_title_ends_before_the_transport() {
    let p = practice(&[], LONG);
    let mut ui = pump(bar(&p));
    let title = bounds_of(&mut ui, LONG);
    let left = transport_left(&mut ui);
    assert!(title.x + title.width <= left, "{title:?} vs {left}");
}

#[test]
fn a_long_title_never_overflows_the_bar() {
    let long = LONG.repeat(8);
    let p = practice(&[], &long);
    let mut ui = pump(bar(&p));
    let counts = bounds_of(&mut ui, "0 takes · 0 notes");
    let left = transport_left(&mut ui);
    // The counts still show, before the transport; the title gave way.
    assert!(counts.x + counts.width <= left);
    let title = bounds_of(&mut ui, &long);
    assert!(title.x + title.width <= counts.x);
    // The transport stays centred on the bar.
    let time = bounds_of(&mut ui, "00:00.0");
    assert!(time.x > WINDOW.width / 2.0 - 200.0 && time.x < WINDOW.width / 2.0 + 100.0);
}

#[test]
fn busy_shows_the_spinner_and_the_transport_stays_put() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut idle = pump(bar(&p));
    assert!(idle.find(BUSY_LABEL).is_err());
    let idle_play = bounds_of(&mut idle, "Play");
    let mut busy = pump(TransportBar {
        busy: true,
        ..bar(&p)
    });
    let spinner = bounds_of(&mut busy, BUSY_LABEL);
    let counts = bounds_of(&mut busy, "1 take · 0 notes");
    // After the counts, on the left; the transport does not move.
    assert!(spinner.x > counts.x + counts.width, "{spinner:?}");
    assert_eq!(bounds_of(&mut busy, "Play"), idle_play);
}

// ------------------------------------------------------- the time display

#[test]
fn the_time_display_reads_the_playhead_over_the_timeline() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(TransportBar {
        position: Duration::from_millis(61_560),
        ..bar(&p)
    });
    assert!(ui.find("01:01.5").is_ok());
    assert!(ui.find("00:30.0").is_ok());
    assert_eq!(time_text(0), "00:00.0");
    // The tenth is truncated, never rounded up.
    assert_eq!(time_text(59_999), "00:59.9");
    assert_eq!(time_text(180_000), "03:00.0");
    assert_eq!(time_text(6_000_000), "100:00.0");
    // A negative length reads as zero.
    assert_eq!(time_text(-5), "00:00.0");
    assert_eq!(
        time_display(Duration::from_millis(12_400), 48_000),
        ("00:12.4".to_string(), "00:48.0".to_string())
    );
}

/// The time display's playhead reading's width for `content`.
fn time_width(content: &str) -> f32 {
    let spec = Spec {
        line_height: 18.0,
        letter_spacing: 0.2,
        tabular: true,
        ..testing::spec(content, type_scale::TIME, font::MEDIUM)
    };
    testing::laid(&spec, f32::INFINITY).min_bounds.width
}

#[test]
fn the_time_display_keeps_its_width_as_the_clock_runs() {
    // Tabular figures: the transport would shift a pixel every tenth
    // otherwise.
    let widths: Vec<f32> = ["00:00.0", "11:11.1", "48:59.4"]
        .iter()
        .map(|s| time_width(s))
        .collect();
    assert!(widths[0] > 0.0);
    // Inter kerns its tabular 4 by about a hundredth of a pixel: the
    // display moves by less than a twentieth of a pixel, never the pixel a
    // tenth proportional figures would.
    assert!(
        widths.iter().all(|w| (w - widths[0]).abs() < 0.05),
        "{widths:?}"
    );
}

// ----------------------------------------------------------- the zoom slider

#[test]
fn the_zoom_slider_sets_the_zoom_live_and_commits_on_release() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    let zoom_in = bounds_of(&mut ui, "Zoom in");
    // The slider ends 6 px before the zoom-in button.
    let right = zoom_in.x - 6.0;
    let left = right - ZOOM_SLIDER_WIDTH;
    let y = zoom_in.center_y();
    // The knob's centre runs from its radius in at either end.
    let at = |v: f32| left + ZOOM_KNOB / 2.0 + v * (ZOOM_SLIDER_WIDTH - ZOOM_KNOB);
    ui.point_at(Point::new(at(0.25), y));
    let _ = ui.simulate([
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(at(0.75), y),
        }),
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(left + 500.0, y),
        }),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]);
    let messages: Vec<Message> = ui.into_messages().collect();
    let live: Vec<f64> = messages
        .iter()
        .filter_map(|m| match m {
            Message::SetTimelineZoomLive(v) => Some(*v),
            _ => None,
        })
        .collect();
    assert_eq!(live.len(), 3, "{messages:?}");
    assert!((live[0] - 0.25).abs() < 0.01);
    assert!((live[1] - 0.75).abs() < 0.01);
    assert_eq!(live[2], 1.0, "clamped");
    assert_eq!(messages.last(), Some(&Message::CommitTimelineZoom));
}

#[test]
fn the_zoom_slider_maps_its_width_onto_zero_to_one() {
    let value = |dx: f32| thin_slider_value(dx, ZOOM_SLIDER_WIDTH, ZOOM_KNOB);
    assert_eq!(value(ZOOM_KNOB / 2.0), 0.0);
    assert_eq!(value(ZOOM_SLIDER_WIDTH / 2.0), 0.5);
    assert_eq!(value(-10.0), 0.0);
    assert_eq!(value(200.0), 1.0);
}

// ---------------------------------------------------------------- the pure

#[test]
fn the_counts_label_is_singular_at_one_take_or_one_note() {
    assert_eq!(counts_label(1, 3), "1 take · 3 notes");
    // The header read "2 takes · 1 notes" once a first note was added.
    assert_eq!(counts_label(0, 1), "0 takes · 1 note");
    assert_eq!(counts_label(2, 1), "2 takes · 1 note");
    assert_eq!(counts_label(12, 0), "12 takes · 0 notes");
}

#[test]
fn a_backing_alone_can_play() {
    // The reference (Original) is playable too.
    let mut p = practice(&[], "Hai Kuo");
    p.reference = Some(Arc::new(AudioTrack::new(
        "r",
        AudioTrackKind::Reference,
        "Original",
        DateTime::utc_ymd(2026, 1, 1),
    )));
    let c = controls(&bar(&p));
    assert_eq!(c.play, Some(Message::TogglePlay));
    assert_eq!(c.stop, Some(Message::Stop));
    assert_eq!(c.go_to_start, Some(Message::GoToStart));
    assert_eq!(c.rec, Message::StartRecording);
    assert!(!c.taking);
}

#[test]
fn the_bar_centres_the_transport_and_bounds_the_sides() {
    let size = Size::new(1000.0, 53.0);
    let [l, c, r] = bar_layout(
        size,
        Size::new(300.0, 30.0),
        Size::new(400.0, 34.0),
        Size::new(150.0, 22.0),
    );
    assert_eq!(l, Point::new(0.0, 11.5));
    // Centred on the window: half the insets' difference left of the bar's
    // own centre.
    assert_eq!(c, Point::new(300.0 - CENTER_OFFSET, 9.5));
    assert_eq!(r, Point::new(850.0, 15.5));
    // Each side gets half of what the transport leaves, less the offset and
    // the gap...
    assert_eq!(side_room(1000.0, 400.0), 288.0 - CENTER_OFFSET);
    // ...and nothing, never less, when the transport takes it all.
    assert_eq!(side_room(300.0, 400.0), 0.0);
}

#[test]
fn the_transport_sits_centred_in_the_toolbar_as_the_mockup_draws_it() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    let play = bounds_of(&mut ui, "Play");
    let record = bounds_of(&mut ui, RECORD_LABEL);
    let start = transport_left(&mut ui);
    let end = record.x + record.width;
    // Centred on the bar, its controls on the 51 px row's middle.
    assert!(
        ((start + end) / 2.0 - WINDOW.width / 2.0).abs() < 14.0,
        "{start} {end}"
    );
    assert!((play.center_y() - (BAR_HEIGHT - 1.0) / 2.0).abs() < 1.0);
    assert_eq!(play.height, height::TOOLBAR_GROUP);
}
