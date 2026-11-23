//! The transport bar's tests: `detail_transport_bar_test.dart` (the gating
//! of play / stop / loop / follow / REC, the play icon, the zoom icons, the
//! counts, a long title, the busy spinner), the counter's format and fixed
//! width, the zoom slider, and the bar's layout through its pure functions.
//!
//! What is worth pinning is the GATING, because every gate here protects a
//! state the engine cannot recover from on its own: play and stop are dead
//! with nothing loaded, play is dead WHILE RECORDING (starting playback into
//! a live take is how you get the backing bleeding into the capture), and
//! the loop toggle is dead until a loop region exists. A gate that silently
//! opens is not visible anywhere below this widget.

use std::sync::Arc;

use iced::{mouse, Event, Settings};
use iced_test::Simulator;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::util::dart_date_time::DateTime;

use super::*;

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

fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

/// What a click on `icon`'s button reports.
fn tap(bar: TransportBar<'_>, icon: Icon) -> Vec<Message> {
    let mut ui = pump(bar);
    ui.click(glyph(icon).as_str())
        .expect("the icon is on the bar");
    ui.into_messages().collect()
}

fn tap_text(bar: TransportBar<'_>, label: &str) -> Vec<Message> {
    let mut ui = pump(bar);
    ui.click(label).expect("the label is on the bar");
    ui.into_messages().collect()
}

fn bounds_of(ui: &mut Simulator<'_, Message>, label: &str) -> Rectangle {
    ui.find(label).expect("on the bar").bounds()
}

// ------------------------------------------------ detail_transport_bar_test

#[test]
fn an_empty_practice_has_play_and_stop_dead() {
    let p = practice(&[], "Hai Kuo");
    assert!(tap(bar(&p), Icon::Play).is_empty());
    assert!(tap(bar(&p), Icon::Square).is_empty());
    // Follow-cursor is a view preference, not a transport action.
    assert_eq!(
        tap(bar(&p), Icon::LocateFixed),
        vec![Message::ToggleFollowCursor]
    );
}

#[test]
fn a_practice_with_a_take_can_play_and_stop() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap(bar(&p), Icon::Play), vec![Message::TogglePlay]);
    assert_eq!(tap(bar(&p), Icon::Square), vec![Message::Stop]);
}

#[test]
fn play_is_dead_while_recording_even_with_takes_loaded() {
    let p = practice(&["t1"], "Hai Kuo");
    let recording = TransportBar {
        is_recording: true,
        ..bar(&p)
    };
    // Starting playback into a live take bleeds the backing into it.
    assert!(tap(recording, Icon::Play).is_empty());
}

#[test]
fn play_is_dead_while_rec_is_still_arming() {
    // isRecording flips only once the mic delivers: Play in that window
    // started the backing, which the take then jumped back to its start.
    let p = practice(&["t1"], "Hai Kuo");
    let arming = TransportBar {
        is_starting_take: true,
        ..bar(&p)
    };
    assert!(tap(arming, Icon::Play).is_empty());
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
    assert_eq!(tap(recording, Icon::Square), vec![Message::StopRecording]);
}

#[test]
fn the_loop_toggle_is_dead_until_a_loop_region_exists() {
    let p = practice(&["t1"], "Hai Kuo");
    let no_region = TransportBar {
        has_loop: false,
        ..bar(&p)
    };
    assert!(tap(no_region, Icon::Repeat).is_empty());
    assert_eq!(tap(bar(&p), Icon::Repeat), vec![Message::ToggleLoop]);
}

#[test]
fn the_play_icon_follows_the_transport_state() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    assert!(ui.find(glyph(Icon::Play).as_str()).is_ok());
    assert!(ui.find(glyph(Icon::Pause).as_str()).is_err());

    let playing = TransportBar {
        is_playing: true,
        ..bar(&p)
    };
    let mut ui = pump(playing);
    assert!(ui.find(glyph(Icon::Pause).as_str()).is_ok());
    assert!(ui.find(glyph(Icon::Play).as_str()).is_err());
}

#[test]
fn rec_starts_a_take_and_stops_the_one_in_flight() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(tap_text(bar(&p), "REC"), vec![Message::StartRecording]);
    let live = TransportBar {
        is_recording: true,
        ..bar(&p)
    };
    // A live take never restarts.
    assert_eq!(tap_text(live, "REC"), vec![Message::StopRecording]);
}

#[test]
fn stop_and_rec_while_the_take_is_still_arming_cancel_it() {
    // isRecording flips only once the mic delivers (permission, take setup
    // and the ready handshake come first), so a Stop there only stopped the
    // transport and the take rolled moments later.
    let p = practice(&["t1"], "Hai Kuo");
    let arming = TransportBar {
        is_starting_take: true,
        ..bar(&p)
    };
    assert_eq!(tap(arming, Icon::Square), vec![Message::StopRecording]);
    assert_eq!(tap_text(arming, "REC"), vec![Message::StopRecording]);
}

#[test]
fn the_zoom_icons_step_the_timeline_out_and_in() {
    let p = practice(&["t1"], "Hai Kuo");
    assert_eq!(
        tap(bar(&p), Icon::ZoomOut),
        vec![Message::ZoomTimeline(1.25)]
    );
    assert_eq!(tap(bar(&p), Icon::ZoomIn), vec![Message::ZoomTimeline(0.8)]);
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

#[test]
fn a_long_title_ends_before_the_transport() {
    let p = practice(&[], LONG);
    let mut ui = pump(bar(&p));
    let title = bounds_of(&mut ui, LONG);
    let play = bounds_of(&mut ui, &glyph(Icon::Play));
    let play_left = play.center_x() - ROUND_BUTTON / 2.0;
    assert!(
        title.x + title.width <= play_left,
        "{title:?} vs {play_left}"
    );
}

#[test]
fn a_long_title_never_overflows_the_bar() {
    let long = LONG.repeat(8);
    let p = practice(&[], &long);
    let mut ui = pump(bar(&p));
    let counts = bounds_of(&mut ui, "0 takes · 0 notes");
    let play = bounds_of(&mut ui, &glyph(Icon::Play));
    // The counts still show, before the transport; the title gave way.
    assert!(counts.x + counts.width <= play.center_x() - ROUND_BUTTON / 2.0);
    let title = bounds_of(&mut ui, &long);
    assert!(title.x + title.width <= counts.x);
    // The transport stays centred on the bar.
    let lcd = bounds_of(&mut ui, "00:00 / 00:30");
    assert!(lcd.x > WINDOW.width / 2.0 - 200.0 && lcd.x < WINDOW.width / 2.0 + 100.0);
}

#[test]
fn busy_shows_the_spinner() {
    // The spinner widens the transport by its 14 px gap and 13 px box, and
    // the transport stays centred: play moves left by half of that.
    let p = practice(&["t1"], "Hai Kuo");
    let idle = bounds_of(&mut pump(bar(&p)), &glyph(Icon::Play));
    let busy = bounds_of(
        &mut pump(TransportBar {
            busy: true,
            ..bar(&p)
        }),
        &glyph(Icon::Play),
    );
    assert!((idle.x - busy.x - 13.5).abs() < 0.6, "{idle:?} {busy:?}");
}

// ------------------------------------------------------------- the counter

#[test]
fn the_counter_reads_the_playhead_over_the_timeline() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(TransportBar {
        position: Duration::from_millis(61_500),
        ..bar(&p)
    });
    assert!(ui.find("01:01 / 00:30").is_ok());
    assert_eq!(lcd_text(Duration::ZERO, 0), "00:00 / 00:00");
    assert_eq!(
        lcd_text(Duration::from_millis(59_999), 180_000),
        "00:59 / 03:00"
    );
    assert_eq!(
        lcd_text(Duration::from_millis(6_000_000), 6_000_000),
        "100:00 / 100:00"
    );
    // A negative timeline length reads as zero, as formatClock clamps it.
    assert_eq!(lcd_text(Duration::ZERO, -5), "00:00 / 00:00");
}

#[test]
fn the_counter_keeps_its_width_as_the_clock_runs() {
    // Tabular figures: the transport would shift a pixel every second
    // otherwise.
    let widths: Vec<f32> = ["00:00 / 03:00", "11:11 / 03:00", "48:59 / 03:00"]
        .iter()
        .map(|s| counter_width(s))
        .collect();
    assert!(widths[0] > 0.0);
    assert!(
        widths.iter().all(|w| (w - widths[0]).abs() < 0.01),
        "{widths:?}"
    );
    // Each character is followed by its 1 px of letter spacing.
    assert!(counter_width("00") > counter_width("0") + 1.0);
}

// ----------------------------------------------------------- the zoom slider

#[test]
fn the_zoom_slider_sets_the_zoom_live_and_commits_on_release() {
    let p = practice(&["t1"], "Hai Kuo");
    let mut ui = pump(bar(&p));
    let zoom_in = bounds_of(&mut ui, &glyph(Icon::ZoomIn));
    // The slider ends 8 px before the zoom-in icon's 2 px pad.
    let right = zoom_in.x - 2.0 - 8.0;
    let left = right - ZOOM_SLIDER_WIDTH;
    let y = zoom_in.center_y();
    ui.point_at(Point::new(left + 24.0, y));
    let _ = ui.simulate([
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(left + 72.0, y),
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
    assert_eq!(zoom_value_at(0.0), 0.0);
    assert_eq!(zoom_value_at(48.0), 0.5);
    assert_eq!(zoom_value_at(-10.0), 0.0);
    assert_eq!(zoom_value_at(200.0), 1.0);
    assert_eq!(zoom_knob_x(0.0, 96.0), 3.0);
    assert_eq!(zoom_knob_x(1.0, 96.0), 93.0);
    assert_eq!(zoom_knob_x(2.0, 96.0), 93.0);
}

// ---------------------------------------------------------------- the pure

#[test]
fn the_counts_label_is_singular_at_one_take() {
    assert_eq!(counts_label(1, 3), "1 take · 3 notes");
    assert_eq!(counts_label(0, 1), "0 takes · 1 notes");
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
    assert_eq!(c, Point::new(300.0, 9.5));
    assert_eq!(r, Point::new(850.0, 15.5));
    // Each side gets half of what the transport leaves, less the gap...
    assert_eq!(side_room(1000.0, 400.0), 288.0);
    // ...and nothing, never less, when the transport takes it all.
    assert_eq!(side_room(300.0, 400.0), 0.0);
}
