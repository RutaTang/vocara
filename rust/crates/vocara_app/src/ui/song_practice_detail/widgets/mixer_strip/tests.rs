// The mixer desk on screen, headless: Dart's mixer_strip_test, the desk's
// part of kept_subtree_fresh_build_test (callbacks of kept strips, faders
// keeping their drag when strips come and go) and of playhead_repaint_test
// (a fader tick redraws its own strip alone).

use std::sync::Arc;

use iced::{Point, Rectangle, Size};
use iced_test::Simulator;
use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;

use crate::ui::song_practice_detail::widgets::track_context_menu::argb;
use crate::ui::song_practice_detail::widgets::track_row::take_color;
use vocara_model::util::dart_date_time::DateTime;

use super::*;
use crate::ui::core::widgets::pan_zoom::testing as pan_zoom;
use crate::ui::song_practice_detail::widgets::mixer_shared::testing::*;

fn take(n: usize) -> Arc<AudioTrack> {
    Arc::new(AudioTrack::new(
        format!("t{n}"),
        AudioTrackKind::Vocal,
        format!("Take {n}"),
        DateTime::local_ymd(2026, 1, 1),
    ))
}

fn takes(range: std::ops::Range<usize>) -> Vec<Arc<AudioTrack>> {
    range.map(take).collect()
}

fn fx_bus(id: &str, name: &str) -> Arc<FxBus> {
    Arc::new(FxBus::new(id, name, TrackFx::flat()))
}

/// The desk as the bottom dock shows it, the master at 0.8 (its dB field
/// "−1.9" apart from the takes' "0.0").
fn desk<'a>(
    tracks: &'a [Arc<AudioTrack>],
    buses: &'a [Arc<FxBus>],
    effects: bool,
) -> MixerStrip<'a> {
    MixerStrip {
        supports_effects: effects,
        master_volume: 0.8,
        ..MixerStrip::new(tracks, buses)
    }
}

/// The master's dB field.
const MASTER_DB: &str = "\u{2212}1.9";

/// The fader canvas (its column and its `meters` meters) between a knob's
/// value label and a dB field: 10 px under the label (the knob slot's 6 px
/// and the fader slot's 4), 16 px over the field (the slot's 6 px over its
/// 20 px box), centred on the strip.
fn fader_between(label: Rectangle, field: Rectangle, meters: usize) -> Rectangle {
    let top = label.y + label.height + 10.0;
    let bottom = field.center_y() - 10.0 - 6.0;
    let width = fader_meters_width(meters);
    Rectangle::new(
        Point::new(label.center_x() - width / 2.0, top),
        Size::new(width, bottom - top),
    )
}

/// The point on a fader canvas's column at `dy` from its top.
fn on_column(fader: Rectangle, dy: f32) -> Point {
    Point::new(fader.x + FADER_WIDTH / 2.0, fader.y + dy)
}

/// The `n`-th take strip's fader box.
fn take_fader(ui: &mut Simulator<'_, Message>, n: usize) -> Rectangle {
    let label = text_at(ui, "C", n).expect("the pan label");
    let field = text_at(ui, "0.0", n).expect("the dB field");
    fader_between(label, field, 2)
}

/// The `n`-th bus strip's fader box.
fn bus_fader(ui: &mut Simulator<'_, Message>, n: usize, field: &str) -> Rectangle {
    let label = text_at(ui, "C", n).expect("the pan label");
    let field = text_at(ui, field, n).expect("the dB field");
    fader_between(label, field, 2)
}

/// The text reading `label` nearest `x` across.
fn text_near(ui: &mut Simulator<'_, Message>, label: &str, x: f32) -> Rectangle {
    (0..)
        .map_while(|n| text_at(ui, label, n))
        .min_by(|a, b| {
            (a.center_x() - x)
                .abs()
                .total_cmp(&(b.center_x() - x).abs())
        })
        .unwrap_or_else(|| panic!("{label} is not on the desk"))
}

/// The master strip's fader box: under its name (a 26 px row under the
/// 3 px cap) and the room a take's buttons and knob take, over its field.
fn master_fader(ui: &mut Simulator<'_, Message>) -> Rectangle {
    let field = text_at(ui, MASTER_DB, 0).expect("the master's field");
    let title = text_near(ui, "Master", field.center_x());
    let top = title.center_y() + 13.0 + BUTTONS_AND_KNOB + 4.0;
    let bottom = field.center_y() - 10.0 - 6.0;
    let width = fader_meters_width(2);
    Rectangle::new(
        Point::new(field.center_x() - width / 2.0, top),
        Size::new(width, bottom - top),
    )
}

/// A strip's step across the desk: its width and its gap.
const STEP: f32 = STRIP_WIDTH + STRIP_GAP;

fn volumes(messages: &[Message]) -> Vec<(String, f64)> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetTrackVolumeLive { id, volume } => Some((id.clone(), *volume)),
            _ => None,
        })
        .collect()
}

fn count_of(messages: &[Message], message: &Message) -> usize {
    messages.iter().filter(|m| *m == message).count()
}

// ------------------------------------------------------------- the layout

// Regression guard: an earlier fix wrapped the strip columns in
// IntrinsicHeight, and every layout threw. The desk lays out at a normal
// AND a squeezed height, and its faders work at both.
#[test]
fn the_desk_lays_out_and_the_fader_works_at_a_normal_dock_height() {
    let tracks = takes(1..2);
    let size = Size::new(600.0, 400.0);
    let mut ui = simulator(size, desk(&tracks, &[], false).view());
    // Tall enough: the plain column — the fader fills what the fixed rows
    // leave, well past the squeezed fallback's 120.
    let fader = take_fader(&mut ui, 0);
    assert!(fader.height > SQUEEZED_FADER_HEIGHT + 40.0, "{fader:?}");
    // The dB field sits over the strip's 30 px output row, at the desk's
    // inner bottom.
    let field = text_at(&mut ui, "0.0", 0).expect("the dB field");
    assert!((field.center_y() - (400.0 - 10.0 - 30.0 - 10.0)).abs() < 1.0);
    // The fader is live: dragging it reports volume ticks.
    let c = on_column(fader, fader.height / 2.0);
    drive(
        &mut ui,
        &drag_steps(
            c,
            &[Point::new(c.x, c.y - 20.0), Point::new(c.x, c.y - 40.0)],
        ),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(!volumes(&messages).is_empty());
    assert_eq!(count_of(&messages, &Message::CommitTrackVolume), 1);
}

#[test]
fn a_squeezed_desk_scrolls_per_strip_and_the_fader_still_works() {
    // 230 − the desk's padding (20) − the strip's cap (3) = 207 inner px:
    // below the threshold, so the scrolling fallback kicks in.
    let tracks = takes(1..2);
    let size = Size::new(600.0, 230.0);
    let mut ui = simulator(size, desk(&tracks, &[], false).view());
    let fader = take_fader(&mut ui, 0);
    assert!(
        (fader.height - SQUEEZED_FADER_HEIGHT).abs() < 0.5,
        "{fader:?}"
    );
    // The master strip is squeezed too.
    let master = master_fader(&mut ui);
    assert!((master.height - SQUEEZED_FADER_HEIGHT).abs() < 0.5);
    // The fixed-height fader inside the scroll view is interactive.
    let c = on_column(fader, fader.height / 2.0);
    drive(&mut ui, &drag_steps(c, &[Point::new(c.x, c.y - 30.0)]));
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(!volumes(&messages).is_empty());
}

#[test]
fn a_severely_squeezed_desk_still_lays_out() {
    let tracks = takes(1..2);
    let mut ui = simulator(Size::new(600.0, 80.0), desk(&tracks, &[], false).view());
    assert!(text_at(&mut ui, "Take 1", 0).is_some());
    assert!(text_at(&mut ui, MASTER_DB, 0).is_some());
}

#[test]
fn the_squeeze_threshold_is_the_tallest_strips_fixed_rows_plus_travel() {
    // Inner height = dock − 20 (desk padding) − 3 (the strip's cap).
    let inner = |dock: f32| dock - 23.0;
    assert!(inner(400.0) >= STRIP_MIN_CONTENT_HEIGHT);
    assert!(inner(230.0) < STRIP_MIN_CONTENT_HEIGHT);
    assert!(inner(238.0) >= STRIP_MIN_CONTENT_HEIGHT);
    // The fixed rows: cap, name, buttons, knob, the fader's padding, the
    // field and the output; plus 56 px of travel.
    let fixed = 3.0 + 26.0 + 26.0 + (26.0 + 2.0 + 10.0 + 6.0) + 10.0 + 20.0 + 30.0;
    assert_eq!(STRIP_MIN_CONTENT_HEIGHT, fixed + 56.0);
}

#[test]
fn an_empty_desk_says_so() {
    let mut ui = simulator(Size::new(600.0, 300.0), MixerStrip::new(&[], &[]).view());
    assert!(text_at(&mut ui, "No tracks yet", 0).is_some());
    assert!(text_at(&mut ui, "Master", 0).is_none());
}

/// On the native engine an empty desk keeps the input strip: the record gain
/// and the monitor level are set there before the first take.
#[test]
fn an_empty_desk_on_the_effects_engine_keeps_the_input_strip() {
    let desk = MixerStrip {
        supports_effects: true,
        ..MixerStrip::new(&[], &[])
    };
    let mut ui = simulator(Size::new(900.0, 400.0), desk.view());
    assert!(text_at(&mut ui, "Input", 0).is_some());
    assert!(text_at(&mut ui, "Master", 0).is_some());
    assert!(text_at(&mut ui, "No tracks yet", 0).is_none());
}

#[test]
fn the_effects_engine_adds_the_mic_strip_the_buses_and_plus_bus() {
    let tracks = takes(1..2);
    let buses = vec![fx_bus("b1", "Verb")];
    let mut ui = simulator(
        Size::new(1200.0, 400.0),
        desk(&tracks, &buses, false).view(),
    );
    assert!(text_at(&mut ui, "Input", 0).is_none());
    assert!(text_at(&mut ui, "Verb", 0).is_none());
    assert!(text_at(&mut ui, ADD_BUS_LABEL, 0).is_none());
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&tracks, &buses, true).view());
    assert!(text_at(&mut ui, "Input", 0).is_some());
    assert!(text_at(&mut ui, "Verb", 0).is_some());
    assert!(text_at(&mut ui, ADD_BUS_LABEL, 0).is_some());
}

// ------------------------------------------------------------ the faders

fn one_take() -> Vec<Arc<AudioTrack>> {
    takes(0..1)
}

const STRIP: Size = Size::new(600.0, 400.0);

#[test]
fn a_click_on_a_fader_sets_and_saves_the_level() {
    let tracks = one_take();
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    let fader = take_fader(&mut ui, 0);
    ui.point_at(on_column(fader, FADER_PAD + 20.0));
    let _ = ui.simulate([press(), release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let v = volumes(&messages);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].0, "t0");
    assert!((v[0].1 - vertical_fader_value(FADER_PAD + 20.0, fader.height)).abs() < 1e-3);
    assert_eq!(messages.last(), Some(&Message::CommitTrackVolume));
    assert_eq!(count_of(&messages, &Message::CommitTrackVolume), 1);
}

// The tap is cancelled when the drag wins, and saving there too wrote the
// level mid-drag on top of the drag's own save at its end.
#[test]
fn a_held_press_that_turns_into_a_drag_saves_once() {
    let tracks = one_take();
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    let c = {
        let f = take_fader(&mut ui, 0);
        on_column(f, f.height / 2.0)
    };
    drive(
        &mut ui,
        &[
            (c, press()),
            (
                Point::new(c.x, c.y - 30.0),
                move_to(Point::new(c.x, c.y - 30.0)),
            ),
            (
                Point::new(c.x, c.y - 40.0),
                move_to(Point::new(c.x, c.y - 40.0)),
            ),
        ],
    );
    ui.point_at(Point::new(c.x, c.y - 40.0));
    let _ = ui.simulate([release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let v = volumes(&messages);
    assert_eq!(v.len(), 3);
    assert!(v[2].1 > v[0].1, "up is louder");
    assert_eq!(count_of(&messages, &Message::CommitTrackVolume), 1);
    assert_eq!(messages.last(), Some(&Message::CommitTrackVolume));
}

#[test]
fn a_press_that_slips_sideways_before_its_drag_saves_once() {
    let tracks = one_take();
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    let c = {
        let f = take_fader(&mut ui, 0);
        on_column(f, f.height / 2.0)
    };
    drive(
        &mut ui,
        &drag_steps(
            c,
            &[
                Point::new(c.x + 20.0, c.y),
                Point::new(c.x + 20.0, c.y - 20.0),
                Point::new(c.x + 20.0, c.y - 30.0),
            ],
        ),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    assert_eq!(count_of(&messages, &Message::CommitTrackVolume), 1);
}

#[test]
fn a_two_finger_trackpad_swipe_over_a_strip_moves_nothing() {
    // The swipe scrolls the desk; it must not also move (and save) the
    // fader or knob it crosses.
    let tracks = one_take();
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    let fader = {
        let f = take_fader(&mut ui, 0);
        on_column(f, f.height / 2.0)
    };
    let knob = text_at(&mut ui, "C", 0).expect("the knob's label");
    let knob = Point::new(knob.center_x(), knob.y - 15.0);
    for at in [fader, knob] {
        ui.point_at(at);
        let swipe = pan_zoom::swipe(&[(30.0, -40.0), (60.0, -80.0)], iced::time::Instant::now());
        pan_zoom::simulate(&mut ui, swipe);
    }
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn a_strip_fader_scrolled_away_mid_drag_still_saves_its_level() {
    // A sideways scroll during the drag moves the strip out of view; the
    // release still ends its drag, and saves once.
    let tracks = takes(0..30);
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    let c = {
        let f = take_fader(&mut ui, 0);
        on_column(f, f.height / 2.0)
    };
    drive(
        &mut ui,
        &[
            (c, press()),
            (
                Point::new(c.x, c.y - 30.0),
                move_to(Point::new(c.x, c.y - 30.0)),
            ),
            (
                Point::new(c.x, c.y - 40.0),
                move_to(Point::new(c.x, c.y - 40.0)),
            ),
            (Point::new(300.0, 200.0), swipe(-2500.0, 0.0)),
        ],
    );
    let first = ui.find(nth("Take 0", 0)).expect("still in the tree");
    assert!(first.visible_bounds().is_none(), "scrolled out of view");
    ui.point_at(Point::new(300.0, 200.0));
    let _ = ui.simulate([release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(!volumes(&messages).is_empty());
    assert_eq!(count_of(&messages, &Message::CommitTrackVolume), 1);
}

#[test]
fn the_fader_maps_its_travel_linearly_and_clamps() {
    // The cap's half height at each end; the top of the travel is unity.
    assert_eq!(FADER_PAD, 6.0);
    assert_eq!(vertical_fader_value(FADER_PAD, 216.0), 1.0);
    assert_eq!(vertical_fader_value(216.0 - FADER_PAD, 216.0), 0.0);
    assert!((vertical_fader_value(108.0, 216.0) - 0.5).abs() < 1e-12);
    assert_eq!(vertical_fader_value(-30.0, 216.0), 1.0);
    assert_eq!(vertical_fader_value(400.0, 216.0), 0.0);
    // Laid out under 2 × the pad (a master strip squeezed to nothing): the
    // span holds at 1 px instead of flipping the clamp.
    assert_eq!(vertical_fader_value(6.5, 10.0), 0.5);
    assert_eq!(vertical_fader_value(0.0, 0.5), 1.0);
}

// ------------------------------------------ strips keep their own drag

// Matched by position, the master strip's fader was rebuilt from scratch
// whenever a take or bus came or went: a master drag stopped following the
// pointer as an import landed.
#[test]
fn the_master_fader_keeps_its_drag_as_takes_come_and_go() {
    let size = Size::new(900.0, 400.0);
    let before = takes(1..2);
    let after = takes(1..3);
    let at = {
        let mut ui = simulator(size, desk(&before, &[], false).view());
        let f = master_fader(&mut ui);
        on_column(f, f.height / 2.0)
    };
    let mut frames = Frames::new(size);
    let pressed = frames.run(desk(&before, &[], false).view(), at, &[press()]);
    assert!(matches!(
        pressed.messages.as_slice(),
        [Message::SetMasterVolumeLive { .. }]
    ));
    // A take lands: the master strip moves one strip right; the drag goes
    // on.
    let moved = Point::new(at.x + STEP, at.y - 30.0);
    let frame = frames.run(
        desk(&after, &[], false).view(),
        moved,
        &[move_to(moved), release()],
    );
    match frame.messages.as_slice() {
        [Message::SetMasterVolumeLive { gain }, Message::CommitMasterVolume] => {
            assert!(*gain > 0.0);
        }
        other => panic!("the drag must go on: {other:?}"),
    }
}

#[test]
fn a_take_fader_keeps_its_drag_when_a_take_before_it_goes() {
    let size = Size::new(900.0, 400.0);
    let three = takes(0..3);
    let two: Vec<_> = three[1..].to_vec();
    let at = {
        let mut ui = simulator(size, desk(&three, &[], false).view());
        let f = take_fader(&mut ui, 1);
        on_column(f, f.height / 2.0)
    };
    let mut frames = Frames::new(size);
    let pressed = frames.run(desk(&three, &[], false).view(), at, &[press()]);
    assert_eq!(volumes(&pressed.messages)[0].0, "t1");
    // Take 0 is deleted: t1's strip moves left into its place, under the
    // pointer of nobody else.
    let moved = Point::new(at.x - STEP, at.y - 30.0);
    let frame = frames.run(
        desk(&two, &[], false).view(),
        moved,
        &[move_to(moved), release()],
    );
    let v = volumes(&frame.messages);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].0, "t1", "t2 must not take the rest of t1's drag");
    assert_eq!(frame.messages.last(), Some(&Message::CommitTrackVolume));
}

#[test]
fn a_bus_fader_keeps_its_drag_when_a_bus_before_it_goes() {
    let size = Size::new(1400.0, 420.0);
    let buses = vec![fx_bus("A", "A"), fx_bus("B", "B"), fx_bus("C", "C")];
    let fewer: Vec<_> = buses[1..].to_vec();
    let at = {
        let mut ui = simulator(size, desk(&[], &buses, true).view());
        let f = bus_fader(&mut ui, 1, "0.0");
        on_column(f, f.height / 2.0)
    };
    let mut frames = Frames::new(size);
    let pressed = frames.run(desk(&[], &buses, true).view(), at, &[press()]);
    assert!(matches!(
        pressed.messages.as_slice(),
        [Message::SetBusVolumeLive { id, .. }] if id == "B"
    ));
    let moved = Point::new(at.x - STEP, at.y - 30.0);
    let frame = frames.run(
        desk(&[], &fewer, true).view(),
        moved,
        &[move_to(moved), release()],
    );
    match frame.messages.as_slice() {
        [Message::SetBusVolumeLive { id, .. }, Message::CommitFx] => {
            assert_eq!(id, "B", "C must not take the rest of B's drag");
        }
        other => panic!("the drag must go on: {other:?}"),
    }
}

// ---------------------------------------------- a tick redraws one fader

#[test]
fn a_mixer_fader_tick_redraws_its_own_strip_alone() {
    let size = Size::new(900.0, 400.0);
    let mut tracks = takes(0..3);
    let mut frames = Frames::new(size);
    let before = fader_draws();
    let _ = frames.run(desk(&tracks, &[], false).view(), Point::ORIGIN, &[]);
    assert_eq!(fader_draws() - before, 4, "three takes and the master");

    // A fader tick on one take.
    let mut t1 = (*tracks[1]).clone();
    t1.volume = 0.51;
    tracks[1] = Arc::new(t1);
    let before = fader_draws();
    let _ = frames.run(desk(&tracks, &[], false).view(), Point::ORIGIN, &[]);
    assert_eq!(fader_draws() - before, 1, "nor the master strip");

    // An unrelated rebuild (a selection): nothing.
    let before = fader_draws();
    let selected = MixerStrip {
        selected_track_id: Some("t2"),
        ..desk(&tracks, &[], false)
    };
    let _ = frames.run(selected.view(), Point::ORIGIN, &[]);
    assert_eq!(fader_draws() - before, 0);
}

#[test]
fn a_bus_fader_tick_redraws_its_own_strip_alone() {
    // Every strip compared the bus list by identity, and each bus tick
    // makes a new one: every strip in the mixer was rebuilt at pointer rate.
    let size = Size::new(1400.0, 420.0);
    let tracks = takes(0..2);
    let mut buses = vec![fx_bus("v", "Verb")];
    let mut frames = Frames::new(size);
    let _ = frames.run(desk(&tracks, &buses, true).view(), Point::ORIGIN, &[]);
    let mut b = (*buses[0]).clone();
    b.volume = 0.5;
    buses[0] = Arc::new(b);
    let before = fader_draws();
    let _ = frames.run(desk(&tracks, &buses, true).view(), Point::ORIGIN, &[]);
    assert_eq!(fader_draws() - before, 1);
}

// ------------------------------------------------------ the strips' taps

#[test]
fn a_strip_selects_its_take_and_its_buttons_reach_the_take() {
    let tracks = takes(0..2);
    let mut ui = simulator(STRIP, desk(&tracks, &[], false).view());
    ui.click(nth("Take 1", 0)).expect("t1's title");
    ui.click(nth("M", 0)).expect("t0's M");
    ui.click(nth("S", 1)).expect("t1's S");
    // Its delete is its right-click menu's.
    let title = text_at(&mut ui, "Take 1", 0).expect("t1's title");
    ui.point_at(title.center());
    let _ = ui.simulate([right_press(), right_release()]);
    ui.click(nth("Delete take", 0)).expect("the menu's delete");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![
            Message::SelectTrack { id: "t1".into() },
            Message::ToggleMute { id: "t0".into() },
            Message::ToggleSolo { id: "t1".into() },
            Message::DeleteTrack { id: "t1".into() },
        ]
    );
}

#[test]
fn a_take_can_be_routed_through_its_chip_to_the_current_bus() {
    // Rename a bus, then route a strip through its chip menu: the menu lists
    // the current bus name.
    let tracks = takes(0..2);
    let buses = vec![fx_bus("bus1", "Renamed")];
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&tracks, &buses, true).view());
    let titles = count(&mut ui, "Renamed");
    ui.click(nth("Master", 1)).expect("t1's output");
    assert_eq!(
        count(&mut ui, "Renamed"),
        titles + 1,
        "the menu lists the bus"
    );
    ui.click(nth("Renamed", titles))
        .expect("the bus in the menu");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::SetTrackBus {
            id: "t1".into(),
            bus_id: Some("bus1".into())
        }]
    );
}

#[test]
fn a_routed_take_reads_its_bus_and_goes_back_to_the_master() {
    let mut routed = (*take(0)).clone();
    routed.bus_id = Some("bus1".into());
    let tracks = vec![Arc::new(routed)];
    let buses = vec![fx_bus("bus1", "Lead")];
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&tracks, &buses, true).view());
    let masters = count(&mut ui, "Master");
    ui.click(nth("Lead", 0)).expect("the routed output");
    ui.click(nth("Master", masters))
        .expect("Master in the menu");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::SetTrackBus {
            id: "t0".into(),
            bus_id: None
        }]
    );
}

#[test]
fn the_backing_track_plays_dry_with_no_route_chip() {
    let backing = Arc::new(AudioTrack::new(
        "acc",
        AudioTrackKind::Accompaniment,
        "Backing",
        DateTime::local_ymd(2026, 1, 1),
    ));
    let tracks = vec![backing, take(1)];
    let buses = vec![fx_bus("bus1", "Verb")];
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&tracks, &buses, true).view());
    // The backing's output opens nothing: it reads where it goes.
    let verbs = count(&mut ui, "Verb");
    ui.click(nth("Master", 0)).expect("the backing's output");
    assert_eq!(count(&mut ui, "Verb"), verbs, "no menu");
    // The take's does.
    ui.click(nth("Master", 1)).expect("the take's output");
    assert_eq!(count(&mut ui, "Verb"), verbs + 1, "the menu lists the bus");
    // Without buses, the take's opens nothing either.
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&tracks, &[], true).view());
    ui.click(nth("Master", 1)).expect("the take's output");
    let _ = ui.into_messages();
}

#[test]
fn a_buss_own_output_never_offers_itself() {
    let buses = vec![fx_bus("a", "Reverb"), fx_bus("b", "Delay")];
    let mut ui = simulator(Size::new(1200.0, 400.0), desk(&[], &buses, true).view());
    let reverbs = count(&mut ui, "Reverb");
    let delays = count(&mut ui, "Delay");
    // Reverb's output (the first bus output).
    ui.click(nth("Master", 0)).expect("the bus's output");
    assert_eq!(count(&mut ui, "Reverb"), reverbs, "never itself");
    assert_eq!(count(&mut ui, "Delay"), delays + 1);
    ui.click(nth("Delay", delays)).expect("Delay in the menu");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::SetBusOutput {
            id: "a".into(),
            output_bus_id: Some("b".into())
        }]
    );
}

// Regression guard: the bus strip's trash icon (and its right-click "Delete
// bus") deleted on the spot, while the timeline's bus lane asks first — and
// a delete takes the bus's whole FX chain and every send into it, with no
// undo. The desk now hands the delete to the host's prompt.
mod deleting_a_bus_asks_first {
    use super::*;

    const SIZE: Size = Size::new(900.0, 420.0);

    fn lead_vox() -> Vec<Arc<FxBus>> {
        vec![fx_bus("b1", "Lead Vox")]
    }

    fn delete() -> Message {
        Message::DeleteBus {
            id: "b1".into(),
            name: "Lead Vox".into(),
        }
    }

    #[test]
    fn the_strip_deletes_through_its_menu_alone() {
        // No trash icon on the strip: a press anywhere on it selects.
        let buses = lead_vox();
        let mut ui = simulator(SIZE, desk(&[], &buses, true).view());
        let title = text_at(&mut ui, "Lead Vox", 0).expect("the bus strip");
        ui.point_at(Point::new(title.center_x(), title.y + 300.0));
        let _ = ui.simulate([press(), release()]);
        assert!(!ui.into_messages().any(|m| m == delete()));
    }

    #[test]
    fn the_right_click_menu() {
        let buses = lead_vox();
        let mut ui = simulator(SIZE, desk(&[], &buses, true).view());
        let title = text_at(&mut ui, "Lead Vox", 0).expect("the bus strip");
        ui.point_at(title.center());
        let _ = ui.simulate([right_press(), right_release()]);
        ui.click(nth("Delete bus", 0)).expect("the menu's delete");
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![delete()]);
    }

    #[test]
    fn the_right_click_menu_opens_at_the_click() {
        // It anchored below the full-height strip, opening at the bottom of
        // the mixer however high up the strip was clicked.
        let buses = lead_vox();
        let mut ui = simulator(SIZE, desk(&[], &buses, true).view());
        let click = text_at(&mut ui, "Lead Vox", 0)
            .expect("the bus strip")
            .center();
        ui.point_at(click);
        let _ = ui.simulate([right_press(), right_release()]);
        let item = text_at(&mut ui, "Rename", 0).expect("the menu");
        assert!((item.y - click.y).abs() < 40.0, "{item:?} vs {click:?}");
    }

    #[test]
    fn rename_asks_for_a_name_from_the_current_one() {
        let buses = lead_vox();
        let mut ui = simulator(SIZE, desk(&[], &buses, true).view());
        let title = text_at(&mut ui, "Lead Vox", 0).expect("the bus strip");
        ui.point_at(title.center());
        let _ = ui.simulate([right_press(), right_release()]);
        ui.click(nth("Rename", 0)).expect("Rename");
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Message::RenameBus {
                id: "b1".into(),
                current: "Lead Vox".into()
            }]
        );
    }

    #[test]
    fn a_tap_outside_closes_the_menu_on_its_release_and_nothing_beneath_sees_it() {
        let buses = lead_vox();
        let mut ui = simulator(SIZE, desk(&[], &buses, true).view());
        let title = text_at(&mut ui, "Lead Vox", 0).expect("the bus strip");
        ui.point_at(title.center());
        let _ = ui.simulate([right_press(), right_release()]);
        assert!(text_at(&mut ui, "Rename", 0).is_some());
        // The "+ Bus" button sits under the barrier, which dismisses on a
        // tap's release (`ModalBarrier`), not its press.
        let add = text_at(&mut ui, ADD_BUS_LABEL, 0).expect("the add-bus tile");
        ui.point_at(add.center());
        let _ = ui.simulate([press()]);
        assert!(text_at(&mut ui, "Rename", 0).is_some(), "still open");
        let _ = ui.simulate([release()]);
        assert!(text_at(&mut ui, "Rename", 0).is_none(), "closed");
        // A secondary click outside dismisses as well.
        ui.point_at(title.center());
        let _ = ui.simulate([right_press(), right_release()]);
        assert!(text_at(&mut ui, "Rename", 0).is_some());
        ui.point_at(add.center());
        let _ = ui.simulate([right_press(), right_release()]);
        assert!(text_at(&mut ui, "Rename", 0).is_none(), "closed again");
        assert!(ui.into_messages().next().is_none());
    }
}

#[test]
fn a_bus_strip_selects_its_bus_and_its_buttons_reach_the_bus() {
    let buses = vec![fx_bus("b1", "Verb")];
    let mut ui = simulator(Size::new(900.0, 420.0), desk(&[], &buses, true).view());
    ui.click(nth("Verb", 0)).expect("the title");
    ui.click(nth("M", 0)).expect("M");
    ui.click(nth("S", 0)).expect("S");
    ui.click(nth(ADD_BUS_LABEL, 0)).expect("the add-bus tile");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![
            Message::SelectBus { id: "b1".into() },
            Message::ToggleBusMute { id: "b1".into() },
            Message::ToggleBusSolo { id: "b1".into() },
            Message::CreateBus,
        ]
    );
}

#[test]
fn a_bus_fader_commits_through_the_fx_commit() {
    let buses = vec![fx_bus("b1", "Verb")];
    let mut ui = simulator(Size::new(900.0, 420.0), desk(&[], &buses, true).view());
    let c = {
        let f = bus_fader(&mut ui, 0, "0.0");
        on_column(f, f.height / 2.0)
    };
    drive(&mut ui, &drag_steps(c, &[Point::new(c.x, c.y + 30.0)]));
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(matches!(
        messages.first(),
        Some(Message::SetBusVolumeLive { id, .. }) if id == "b1"
    ));
    assert_eq!(messages.last(), Some(&Message::CommitFx));
}

// ------------------------------------------------------- the mic strip

#[test]
fn the_input_strip_sets_the_record_gain_and_commits_the_gains() {
    let mic = MicInput {
        record_gain_db: 20.0,
        monitor_gain: 0.5,
        ..MicInput::default()
    };
    let tracks = one_take();
    let view = MixerStrip {
        mic,
        ..desk(&tracks, &[], true)
    };
    let mut ui = simulator(STRIP, view.view());
    assert!(text_at(&mut ui, "+20.0", 0).is_some());
    assert!(text_at(&mut ui, "50%", 0).is_some());
    let label = text_at(&mut ui, "50%", 0).expect("the monitor's reading");
    let field = text_at(&mut ui, "+20.0", 0).expect("the record gain's field");
    let fader = fader_between(label, field, 1);
    let c = on_column(fader, fader.height / 2.0);
    drive(&mut ui, &drag_steps(c, &[Point::new(c.x, c.y - 40.0)]));
    let messages: Vec<_> = ui.into_messages().collect();
    let dbs: Vec<f64> = messages
        .iter()
        .filter_map(|m| match m {
            Message::SetRecordGainDbLive { db } => Some(*db),
            _ => None,
        })
        .collect();
    assert_eq!(dbs.len(), 2);
    assert!(dbs[1] > dbs[0]);
    assert_eq!(messages.last(), Some(&Message::CommitAudioGains));
}

/// The input strip's monitor knob: over its reading.
fn monitor_knob(ui: &mut Simulator<'_, Message>, reading: &str) -> Point {
    let label = text_at(ui, reading, 0).expect("the monitor's reading");
    Point::new(label.center_x(), label.y - 2.0 - PAN_KNOB_SIZE / 2.0)
}

#[test]
fn the_monitor_knob_sets_how_loud_you_hear_yourself() {
    let mic = MicInput {
        monitor_gain: 0.5,
        ..MicInput::default()
    };
    let tracks = one_take();
    let view = MixerStrip {
        mic,
        ..desk(&tracks, &[], true)
    };
    let mut ui = simulator(STRIP, view.view());
    let at = monitor_knob(&mut ui, "50%");
    drive(
        &mut ui,
        &drag_steps(
            at,
            &[Point::new(at.x + 20.0, at.y), Point::new(at.x + 40.0, at.y)],
        ),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    let gains: Vec<f64> = messages
        .iter()
        .filter_map(|m| match m {
            Message::SetMonitorGainLive { gain } => Some(*gain),
            _ => None,
        })
        .collect();
    assert!(!gains.is_empty(), "{messages:?}");
    // Right is louder: 40 px of 160 is a quarter of the range.
    assert!((gains.last().unwrap() - level_after_drag(0.5, 40.0)).abs() < 1e-9);
    assert_eq!(level_after_drag(0.5, 40.0), 0.75);
    assert_eq!(level_after_drag(0.9, 80.0), 1.0);
    assert_eq!(messages.last(), Some(&Message::CommitAudioGains));
}

#[test]
fn a_locked_monitor_reads_speaker_and_ignores_the_pointer() {
    let mic = MicInput {
        monitor_allowed: false,
        monitor_gain: 0.8,
        ..MicInput::default()
    };
    let tracks = one_take();
    let view = MixerStrip {
        mic,
        ..desk(&tracks, &[], true)
    };
    let mut ui = simulator(STRIP, view.view());
    assert!(text_at(&mut ui, "spkr", 0).is_some());
    let at = monitor_knob(&mut ui, "spkr");
    drive(&mut ui, &drag_steps(at, &[Point::new(at.x + 40.0, at.y)]));
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        !messages.iter().any(|m| matches!(
            m,
            Message::SetMonitorGainLive { .. } | Message::CommitAudioGains
        )),
        "{messages:?}"
    );
}

#[test]
fn r_records_and_i_arms_the_mic_as_the_toolbar_does() {
    let tracks = one_take();
    let mut ui = simulator(STRIP, desk(&tracks, &[], true).view());
    ui.click(nth("R", 0)).expect("R");
    ui.click(nth("I", 0)).expect("I");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::StartRecording, Message::ToggleInputMonitor]
    );
    // A take in flight: R ends it.
    let taking = MixerStrip {
        mic: MicInput {
            taking: true,
            ..MicInput::default()
        },
        ..desk(&tracks, &[], true)
    };
    let mut ui = simulator(STRIP, taking.view());
    ui.click(nth("R", 0)).expect("R");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::StopRecording]
    );
}

#[test]
fn the_db_fields_read_with_a_true_minus_sign() {
    assert_eq!(strip_db(1.0), "0.0");
    assert_eq!(strip_db(0.8), MASTER_DB);
    assert_eq!(strip_db(0.0), "\u{2212}∞");
    assert_eq!(record_gain_label(12.0), "+12.0");
    assert_eq!(record_gain_label(0.0), "+0.0");
}

#[test]
fn the_monitor_readout_and_its_lock() {
    let mic = |allowed, gain, mark| MicInput {
        monitor_allowed: allowed,
        monitor_gain: gain,
        suggest_headphones_mark: mark,
        ..MicInput::default()
    };
    assert_eq!(monitor_readout(&mic(false, 0.7, false)), "spkr");
    assert_eq!(monitor_readout(&mic(true, 0.0005, false)), "off");
    assert_eq!(monitor_readout(&mic(true, 0.355, false)), "36%");
    assert_eq!(monitor_lock_message(&mic(true, 0.5, true)), None);
    assert!(monitor_lock_message(&mic(false, 0.5, true))
        .is_some_and(|m| m.contains("\"Headphones only\" in Settings")));
    assert_eq!(
        monitor_lock_message(&mic(false, 0.5, false)),
        Some("Hearing yourself is off on this output so speakers can't feed back.")
    );
}

#[test]
fn the_record_gain_maps_onto_the_rec_fader() {
    assert_eq!(record_gain_position(0.0), 0.0);
    assert_eq!(record_gain_position(20.0), 0.5);
    assert_eq!(record_gain_position(55.0), 1.0);
    assert_eq!(record_gain_at(0.25), 10.0);
}

// ---------------------------------------------------------- the colors

#[test]
fn a_take_without_a_tint_keeps_a_stable_color_from_its_id() {
    // "t1": 't' (116) + '1' (49) = 165; 165 % 16 = 5 → teal.
    assert_eq!(default_take_color("t1"), argb(0xFF49_B6A8));
    assert_eq!(default_take_color("t1"), default_take_color("t1"));
    let mut tinted = (*take(1)).clone();
    tinted.color_value = Some(0xFF22_CC44);
    assert_eq!(track_accent(&tinted), argb(0xFF22_CC44));
    let backing = AudioTrack::new(
        "acc",
        AudioTrackKind::Reference,
        "Original",
        DateTime::local_ymd(2026, 1, 1),
    );
    // The strip's cap is its lane's colour: the backing's neutral, a take's
    // its own, by its id.
    assert_eq!(track_accent(&backing), Studio::TRACK_BACKING);
    assert_eq!(track_accent(&take(2)), take_color(&take(2)));
    assert_eq!(track_accent(&take(2)), Studio::default_track_color(&take(2).id));
    // A bus's cap is its lane's: periwinkle untinted.
    assert_eq!(bus_accent(&fx_bus("a", "Verb")), Studio::TRACK_PERIWINKLE);
}

#[test]
fn a_route_reads_its_bus_name_or_the_master() {
    let buses = vec![fx_bus("a", "Verb")];
    assert_eq!(bus_name(&buses, None), "Master");
    assert_eq!(bus_name(&buses, Some("a")), "Verb");
    assert_eq!(bus_name(&buses, Some("gone")), "Master");
}
