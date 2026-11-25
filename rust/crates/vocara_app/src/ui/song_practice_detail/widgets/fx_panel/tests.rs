// The FX panel is the whole insert chain for a take OR a bus — nine plugins,
// each with a header toggle and (when on) its own editor.
//
// What matters here is the DISPATCH: the same panel drives a track through
// setTrackFxLive/reorderTrackInserts and a bus through setBusFxLive/
// reorderBusInserts. A wire crossed between those two paths edits the wrong
// channel, which is invisible to every layer below. (Ported from Dart's
// fx_panel_test, the FX panel's part of left_panel_test, and the analyzer
// and meter halves of the playhead-repaint tests.)

use std::time::Duration;

use iced::time::Instant;
use iced::widget::container;
use iced::{window, Point, Rectangle, Size};
use vocara_model::models::audio_fx::{FxKind, TrackFx, SEND_OFF_DB};
use vocara_model::models::fx_presets::FxPresets;

use super::chain::{reorder_target, reordered};
use super::eq::*;
use super::*;
use crate::ui::core::widgets::pan_zoom::testing as pan_zoom;
use crate::ui::song_practice_detail::widgets::mixer_shared::testing::*;

/// The inspector's width (LeftPanel's `_panelWidth`), tall enough that the
/// whole chain lays out.
const W: f32 = 272.0;
const SIZE: Size = Size::new(W, 4000.0);

fn track(fx: Option<&TrackFx>) -> FxPanel<'_> {
    FxPanel::new(FxTarget::Track { id: "t1", fx })
}

fn bus(fx: &TrackFx) -> FxPanel<'_> {
    FxPanel::new(FxTarget::Bus { id: "busA", fx })
}

fn view(panel: FxPanel<'_>) -> Element<'_, Message> {
    container(panel.view()).width(W).height(SIZE.height).into()
}

fn with(f: impl FnOnce(&mut TrackFx)) -> TrackFx {
    let mut fx = TrackFx::flat();
    f(&mut fx);
    fx
}

/// The header toggle of the insert titled `title`: at the right end of its
/// header row.
fn toggle_for(ui: &mut iced_test::Simulator<'_, Message>, title: &str) -> Point {
    let header = text_at(ui, title, 0).unwrap_or_else(|| panic!("{title} header"));
    Point::new(W - 2.0 - 17.0, header.center_y())
}

/// The slider labelled `label`: the middle of its track (between the 64 px
/// label and the 8 + 42 px readout, inside the panel's 2 px padding).
fn slider_at(ui: &mut iced_test::Simulator<'_, Message>, label: &str, fraction: f32) -> Point {
    let row = text_at(ui, label, 0).unwrap_or_else(|| panic!("{label} slider"));
    let (left, right) = (2.0 + 64.0, W - 2.0 - 50.0);
    Point::new(left + (right - left) * fraction, row.center_y())
}

/// The EQ graph's box, under its header.
fn eq_box(ui: &mut iced_test::Simulator<'_, Message>) -> Rectangle {
    let header = text_at(ui, "EQUALIZER", 0).expect("the EQ header");
    let top = header.center_y() - chain::HEADER_HEIGHT / 2.0 + chain::HEADER_HEIGHT + 8.0;
    Rectangle::new(Point::new(2.0, top), Size::new(W - 4.0, EQ_HEIGHT))
}

fn commits(messages: &[Message]) -> usize {
    messages.iter().filter(|m| **m == Message::CommitFx).count()
}

fn track_edits(messages: &[Message]) -> Vec<TrackFx> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetTrackFxLive { id, fx } => {
                assert_eq!(id, "t1");
                Some(fx.clone())
            }
            _ => None,
        })
        .collect()
}

fn bus_edits(messages: &[Message]) -> Vec<(String, TrackFx)> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetBusFxLive { id, fx } => Some((id.clone(), fx.clone())),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------ the readouts

#[test]
fn small_rates_and_times_keep_a_decimal_in_their_readout() {
    // The doubler's 0.6 Hz default read "1 Hz"; a 0.2 ms attack "0 ms".
    assert_eq!(fx_value_label(0.6, "Hz"), "0.6 Hz");
    assert_eq!(fx_value_label(0.2, "ms"), "0.2 ms");
    assert_eq!(fx_value_label(120.0, "Hz"), "120 Hz");
    assert_eq!(fx_value_label(2500.0, "Hz"), "2.5k");
    assert_eq!(fx_value_label(40.0, "ms"), "40 ms");
    // The other units.
    assert_eq!(fx_value_label(4.0, ":1"), "4.0:1");
    assert_eq!(fx_value_label(-18.4, "dB"), "-18 dB");
    assert_eq!(fx_value_label(0.255, ""), "26%");
}

#[test]
fn the_gr_readout_reads_the_reduction_as_a_cut() {
    assert_eq!(gr_readout(0.0), "0.0");
    assert_eq!(gr_readout(0.049), "0.0");
    assert_eq!(gr_readout(6.0), "-6.0");
    assert_eq!(gr_readout(12.34), "-12.3");
}

// ------------------------------------------------------------- the chain

#[test]
fn renders_every_insert_header_for_a_take_with_no_fx_yet() {
    let mut ui = simulator(SIZE, view(track(None)));
    for title in [
        "EQUALIZER",
        "COMPRESSOR",
        "DE-ESSER",
        "GATE",
        "DELAY",
        "SATURATION",
        "DOUBLER",
        "LIMITER",
        "REVERB",
    ] {
        assert!(
            text_at(&mut ui, title, 0).is_some(),
            "{title} header missing"
        );
    }
}

#[test]
fn a_track_toggle_edits_the_track_and_commits() {
    let mut ui = simulator(SIZE, view(track(None)));
    let at = toggle_for(&mut ui, "EQUALIZER");
    ui.point_at(at);
    let _ = ui.simulate([press(), release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    assert_eq!(edits.len(), 1);
    assert!(edits[0].eq_on);
    assert!(
        bus_edits(&messages).is_empty(),
        "a take must not edit a bus"
    );
    // A toggle is a committed edit, not a drag: the live edit, then the
    // commit.
    assert_eq!(messages.last(), Some(&Message::CommitFx));
    assert_eq!(commits(&messages), 1);
}

#[test]
fn a_bus_toggle_edits_the_bus_and_commits() {
    let fx = TrackFx::flat();
    let mut ui = simulator(SIZE, view(bus(&fx)));
    let at = toggle_for(&mut ui, "COMPRESSOR");
    ui.point_at(at);
    let _ = ui.simulate([press(), release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = bus_edits(&messages);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].0, "busA");
    assert!(edits[0].1.comp_on);
    assert!(
        !messages
            .iter()
            .any(|m| matches!(m, Message::SetTrackFxLive { .. })),
        "a bus must not edit a take"
    );
    assert_eq!(commits(&messages), 1);
}

#[test]
fn toggling_an_insert_on_reveals_its_editor_off_hides_it() {
    // The compressor's editor is the only place these labels appear.
    let on = with(|fx| fx.comp_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&on))));
    assert!(text_at(&mut ui, "Ratio", 0).is_some());
    assert!(text_at(&mut ui, "Makeup", 0).is_some());

    let off = TrackFx::flat();
    let mut ui = simulator(SIZE, view(track(Some(&off))));
    assert!(text_at(&mut ui, "Ratio", 0).is_none());
    assert!(text_at(&mut ui, "Makeup", 0).is_none());
}

#[test]
fn every_insert_editor_renders_with_its_plugin_on() {
    // All nine editors laid out at once.
    let fx = with(|fx| {
        fx.eq_on = true;
        fx.comp_on = true;
        fx.de_esser_on = true;
        fx.gate_on = true;
        fx.delay_on = true;
        fx.saturation_on = true;
        fx.doubler_on = true;
        fx.limiter_on = true;
        fx.reverb_on = true;
    });
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    for label in [
        "Threshold",
        "Ratio",
        "Makeup",
        "GR",
        "Freq",
        "Range",
        "Attack",
        "Release",
        "Time",
        "Feedback",
        "Drive",
        "Depth",
        "Rate",
        "Ceiling",
        "SPACE",
        "Space…",
        "Size",
        "Damping",
    ] {
        assert!(text_at(&mut ui, label, 0).is_some(), "{label} missing");
    }
    // The defaults' readouts, in their units.
    assert!(
        text_at(&mut ui, "0.6 Hz", 0).is_some(),
        "the doubler's rate"
    );
}

#[test]
fn choosing_a_preset_replaces_the_chain_but_keeps_the_order() {
    let mut custom = vec![FxKind::Comp, FxKind::Eq];
    custom.extend_from_slice(&FxKind::VALUES[2..]);
    let fx = with(|fx| {
        fx.order = custom.clone();
        fx.send_db = -12.0;
    });
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    ui.click(nth("Choose…", 0)).expect("the preset dropdown");
    let preset = &FxPresets::vocal()[0];
    ui.click(nth(preset.name, 0)).expect("the first preset");
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    assert_eq!(edits.len(), 1);
    // A preset is tone, not the user's plugin order.
    assert_eq!(edits[0].order, custom);
    // The legacy reverb send nothing else can clear goes with the chain.
    assert_eq!(edits[0].send_db, SEND_OFF_DB);
    assert_eq!(
        TrackFx {
            order: FxKind::VALUES.to_vec(),
            ..edits[0].clone()
        },
        preset.fx
    );
    assert_eq!(commits(&messages), 1);
}

#[test]
fn the_preset_menu_lists_every_preset_with_its_description() {
    let mut ui = simulator(SIZE, view(track(None)));
    ui.click(nth("Choose…", 0)).expect("the preset dropdown");
    for p in FxPresets::vocal() {
        assert!(text_at(&mut ui, p.name, 0).is_some(), "{}", p.name);
        assert!(text_at(&mut ui, p.description, 0).is_some(), "{}", p.name);
    }
    // Escape closes it, picking nothing.
    let _ = ui.tap_key(iced::keyboard::key::Named::Escape);
    assert!(text_at(&mut ui, FxPresets::vocal()[1].name, 0).is_none());
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn a_reverb_space_replaces_the_reverb_and_commits() {
    let fx = with(|fx| fx.reverb_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    ui.click(nth("Space…", 0)).expect("the space dropdown");
    ui.click(nth("Hall", 0)).expect("Hall");
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    let hall = FxPresets::reverb()
        .into_iter()
        .find(|p| p.name == "Hall")
        .expect("Hall");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].reverb, hall.reverb);
    assert_eq!(commits(&messages), 1);
}

#[test]
fn dragging_a_handle_reorders_the_take_chain() {
    let mut ui = simulator(SIZE, view(track(None)));
    let eq = text_at(&mut ui, "EQUALIZER", 0).expect("EQ");
    let handle = Point::new(2.0 + 8.0, eq.center_y());
    // Down past the compressor's middle (each closed card is 36 px tall).
    drive(
        &mut ui,
        &drag_steps(
            handle,
            &[
                Point::new(handle.x, handle.y + 20.0),
                Point::new(handle.x, handle.y + 40.0),
            ],
        ),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    let mut expected = vec![FxKind::Comp, FxKind::Eq];
    expected.extend_from_slice(&FxKind::VALUES[2..]);
    assert_eq!(
        messages,
        vec![Message::ReorderTrackInserts {
            id: "t1".into(),
            order: expected
        }]
    );
}

#[test]
fn dragging_a_handle_reorders_the_bus_chain_and_a_short_drag_nothing() {
    let fx = TrackFx::flat();
    let mut ui = simulator(SIZE, view(bus(&fx)));
    let limiter = text_at(&mut ui, "LIMITER", 0).expect("limiter");
    let handle = Point::new(2.0 + 8.0, limiter.center_y());
    // Up past the doubler's and the saturation's middles.
    drive(
        &mut ui,
        &drag_steps(handle, &[Point::new(handle.x, handle.y - 80.0)]),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    let order = reordered(&FxKind::VALUES, 7, 5);
    assert_eq!(
        messages,
        vec![Message::ReorderBusInserts {
            id: "busA".into(),
            order
        }]
    );

    // Dropped where it started: nothing to report.
    let mut ui = simulator(SIZE, view(bus(&fx)));
    drive(
        &mut ui,
        &drag_steps(handle, &[Point::new(handle.x, handle.y + 6.0)]),
    );
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn the_drop_lands_where_the_dragged_cards_middle_is() {
    let heights = [36.0, 36.0, 120.0, 36.0];
    // Barely moved: stays.
    assert_eq!(reorder_target(&heights, 0, 5.0), 0);
    // Past the next card's middle.
    assert_eq!(reorder_target(&heights, 0, 40.0), 1);
    // Up past the card above's middle (54), not the one before it (18).
    assert_eq!(reorder_target(&heights, 2, 30.0), 2);
    assert_eq!(reorder_target(&heights, 2, -10.0), 1);
    // All the way down.
    assert_eq!(reorder_target(&heights, 0, 500.0), 3);
    assert_eq!(
        reordered(&[FxKind::Eq, FxKind::Comp, FxKind::Gate], 0, 2),
        vec![FxKind::Comp, FxKind::Gate, FxKind::Eq]
    );
}

// ------------------------------------------------------------ the sliders

#[test]
fn a_click_on_a_slider_sets_the_value_and_commits() {
    // The press sets the value; its release (a click) commits it.
    let fx = with(|fx| fx.comp_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let at = slider_at(&mut ui, "Threshold", 0.5);
    ui.point_at(at);
    let _ = ui.simulate([press(), release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    assert_eq!(edits.len(), 1);
    assert!((edits[0].comp.threshold_db - -24.0).abs() < 0.5);
    assert_eq!(messages.last(), Some(&Message::CommitFx));
    assert_eq!(commits(&messages), 1);
}

#[test]
fn a_held_slider_press_that_turns_into_a_pan_commits_once() {
    let fx = with(|fx| fx.comp_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let at = slider_at(&mut ui, "Ratio", 0.25);
    drive(
        &mut ui,
        &[
            (at, press()),
            (
                Point::new(at.x + 30.0, at.y),
                move_to(Point::new(at.x + 30.0, at.y)),
            ),
            (
                Point::new(at.x + 40.0, at.y),
                move_to(Point::new(at.x + 40.0, at.y)),
            ),
        ],
    );
    // Mid-pan: live edits, no commit yet.
    ui.point_at(Point::new(at.x + 40.0, at.y));
    let _ = ui.simulate([release()]);
    let messages: Vec<_> = ui.into_messages().collect();
    let ratios: Vec<f64> = track_edits(&messages)
        .iter()
        .map(|f| f.comp.ratio)
        .collect();
    assert_eq!(ratios.len(), 3);
    assert!(ratios[2] > ratios[0], "it follows the pointer: {ratios:?}");
    assert_eq!(commits(&messages), 1);
    assert_eq!(messages.last(), Some(&Message::CommitFx));
}

#[test]
fn a_slider_drag_past_its_ends_clamps() {
    assert_eq!(fx_slider_value(-20.0, 154.0, -48.0, 0.0), -48.0);
    assert_eq!(fx_slider_value(400.0, 154.0, -48.0, 0.0), 0.0);
    assert!((fx_slider_value(77.0, 154.0, 1.0, 12.0) - 6.5).abs() < 1e-12);
}

#[test]
fn a_two_finger_trackpad_swipe_over_the_editors_moves_nothing() {
    // The swipe scrolls the panel; it must not also move (and save) the
    // slider or EQ band it crosses.
    let fx = with(|fx| {
        fx.comp_on = true;
        fx.eq_on = true;
    });
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let slider = slider_at(&mut ui, "Threshold", 0.5);
    let graph = eq_box(&mut ui).center();
    for at in [slider, graph] {
        ui.point_at(at);
        let swipe = pan_zoom::swipe(&[(30.0, -20.0), (60.0, -40.0)], Instant::now());
        pan_zoom::simulate(&mut ui, swipe);
    }
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn a_wheel_over_the_eq_sets_the_nearest_bands_q_and_a_precise_one_too() {
    // `onPointerSignal`: a scroll signal, a mouse wheel's (40 px a line)
    // or a precise device's without gesture phases (its points).
    let fx = with(|fx| fx.eq_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let graph = eq_box(&mut ui).center();
    ui.point_at(graph);
    let mut pad = pan_zoom::Trackpad::new();
    let t = Instant::now();
    pan_zoom::simulate(&mut ui, [pad.lines(0.0, 1.0, t), pad.pixels(0.0, -40.0, t)]);
    let messages: Vec<_> = ui.into_messages().collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
}

// ------------------------------------------------------------------- EQ

#[test]
fn an_eq_band_drag_edits_live_and_commits_on_release() {
    let fx = with(|fx| fx.eq_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let b = eq_box(&mut ui);
    let c = b.center();
    drive(
        &mut ui,
        &drag_steps(
            c,
            &[
                Point::new(c.x + 30.0, c.y - 10.0),
                Point::new(c.x + 40.0, c.y - 15.0),
            ],
        ),
    );
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    assert_eq!(
        edits.len(),
        1,
        "the pan's start moves nothing; its update does"
    );
    // The band nearest where the pan started (x = 164 of 268: the 2 kHz
    // peak), moved to the pointer: freq from x, gain from y.
    let local = Point::new(c.x + 40.0 - b.x, c.y - 15.0 - b.y);
    let band = edits[0].eq[3];
    assert!(band.enabled);
    assert!((band.freq - x_to_freq(f64::from(local.x), f64::from(b.width))).abs() < 1e-6);
    assert!((band.gain_db - y_to_db(f64::from(local.y), 120.0)).abs() < 1e-6);
    assert!(band.gain_db > 0.0);
    assert!(edits[0].eq_on);
    assert_eq!(messages.last(), Some(&Message::CommitFx));
    assert_eq!(commits(&messages), 1);
}

#[test]
fn a_wheel_q_edit_is_live_at_once_and_commits_after_its_debounce() {
    let fx = with(|fx| fx.eq_on = true);
    let mut ui = simulator(SIZE, view(track(Some(&fx))));
    let at = eq_box(&mut ui).center();
    ui.point_at(at);
    let start = Instant::now();
    let _ = ui.simulate([wheel(0.0, 1.0)]);
    // Within the debounce: the edit is live, not yet written.
    let _ = ui.simulate([redraw(start)]);
    let _ = ui.simulate([redraw(start + Duration::from_millis(200))]);
    let _ = ui.simulate([redraw(Instant::now() + Duration::from_millis(500))]);
    let _ = ui.simulate([redraw(Instant::now() + Duration::from_millis(1500))]);
    let messages: Vec<_> = ui.into_messages().collect();
    let edits = track_edits(&messages);
    assert_eq!(edits.len(), 1);
    // The 700 Hz peak (nearest the middle), narrowed: scrolling up raised
    // its Q.
    let band = edits[0].eq[2];
    assert!((band.q - q_after_wheel(1.0, -40.0)).abs() < 1e-12);
    assert!(band.q > 1.0);
    assert!(edits[0].eq_on);
    // Written once, after the debounce.
    assert_eq!(messages.last(), Some(&Message::CommitFx));
    assert_eq!(commits(&messages), 1);
}

#[test]
fn the_q_wheel_clamps_and_follows_the_scroll_direction() {
    assert!(q_after_wheel(1.0, -40.0) > 1.0);
    assert!(q_after_wheel(1.0, 40.0) < 1.0);
    assert_eq!(q_after_wheel(13.9, -4000.0), 14.0);
    assert_eq!(q_after_wheel(0.31, 4000.0), 0.3);
}

#[test]
fn the_eq_curve_mirrors_the_engines_biquads() {
    // A peaking band reads its gain at its own frequency, and nothing far
    // from it.
    let peak = EqBand {
        gain_db: 6.0,
        ..EqBand::new(vocara_model::models::audio_fx::EqBandKind::Peaking, 1000.0)
    };
    assert!((band_db(&peak, 1000.0) - 6.0).abs() < 1e-6);
    assert!(band_db(&peak, 30.0).abs() < 0.1);
    // Off, or flat, it is silent.
    assert_eq!(
        band_db(
            &EqBand {
                enabled: false,
                ..peak
            },
            1000.0
        ),
        0.0
    );
    assert_eq!(
        band_db(
            &EqBand {
                gain_db: 0.0,
                ..peak
            },
            1000.0
        ),
        0.0
    );
    // Shelves reach their gain beyond their corner.
    let low = EqBand {
        gain_db: -9.0,
        ..EqBand::with_q(
            vocara_model::models::audio_fx::EqBandKind::LowShelf,
            100.0,
            0.7,
        )
    };
    assert!((band_db(&low, 20.0) - -9.0).abs() < 0.3);
    assert!(band_db(&low, 10000.0).abs() < 0.1);
    let high = EqBand {
        gain_db: 4.0,
        ..EqBand::with_q(
            vocara_model::models::audio_fx::EqBandKind::HighShelf,
            6000.0,
            0.7,
        )
    };
    assert!((band_db(&high, 20000.0) - 4.0).abs() < 0.3);
    // The EQ sums its bands, and an EQ that is off is flat.
    let bands = [peak, high];
    let sum = total_db(&bands, true, 1000.0);
    assert!((sum - band_db(&peak, 1000.0) - band_db(&high, 1000.0)).abs() < 1e-12);
    assert_eq!(total_db(&bands, false, 1000.0), 0.0);
    // The memoized response is that sum at the render points.
    let r = response_of(&bands);
    assert_eq!(r.len(), SPECTRUM_POINTS);
    assert_eq!(r[10], total_db(&bands, true, spectrum_point_freq(10)));
}

#[test]
fn the_graph_maps_log_frequency_and_db() {
    assert_eq!(freq_to_x(20.0, 300.0), 0.0);
    assert!((freq_to_x(18000.0, 300.0) - 300.0).abs() < 1e-9);
    assert!((x_to_freq(freq_to_x(1234.0, 300.0), 300.0) - 1234.0).abs() < 1e-6);
    // Past the box, the frequency holds at its ends.
    assert_eq!(x_to_freq(-10.0, 300.0), 20.0);
    assert!((x_to_freq(400.0, 300.0) - 18000.0).abs() < 1e-6);
    assert_eq!(db_to_y(0.0, 120.0), 60.0);
    assert_eq!(db_to_y(15.0, 120.0), 0.0);
    assert_eq!(y_to_db(0.0, 120.0), 15.0);
    assert_eq!(y_to_db(120.0, 120.0), -15.0);
    assert_eq!(y_to_db(-50.0, 120.0), 15.0);
    // A drag keeps the band inside the engine's range and turns it on.
    let off = EqBand {
        enabled: false,
        ..EqBand::new(vocara_model::models::audio_fx::EqBandKind::Peaking, 500.0)
    };
    let moved = band_dragged_to(&off, 600.0, 30.0, 300.0);
    assert!(moved.enabled);
    assert!((moved.freq - 18000.0).abs() < 1e-6);
    assert!((moved.gain_db - 7.5).abs() < 1e-12);
}

#[test]
fn the_nearest_band_is_the_closest_handle_column() {
    let bands = vocara_model::models::audio_fx::FLAT_BANDS;
    let w = 268.0;
    assert_eq!(nearest_band(&bands, 0.0, w), 0);
    assert_eq!(nearest_band(&bands, freq_to_x(700.0, w) + 3.0, w), 2);
    assert_eq!(nearest_band(&bands, w, w), 5);
}

#[test]
fn the_readout_pill_reads_frequency_and_signed_gain() {
    let band = |freq, gain_db| EqBand {
        gain_db,
        ..EqBand::new(vocara_model::models::audio_fx::EqBandKind::Peaking, freq)
    };
    assert_eq!(readout_pill_text(&band(250.4, 3.25)), "250 Hz  ·  +3.3 dB");
    assert_eq!(
        readout_pill_text(&band(2500.0, -6.0)),
        "2.5 kHz  ·  −6.0 dB"
    );
    assert_eq!(fmt_hz(999.6), "1000 Hz");
}

// -------------------------------------------------------------- analyzer

/// A real input column: 1025 bins at 48 kHz / 2048.
fn column(db: impl Fn(usize) -> f32) -> Vec<f32> {
    (0..1025).map(db).collect()
}

const BIN_HZ: f64 = 48000.0 / 2048.0;

/// Ticks `data` every 16 ms from `t` until a frame changes nothing (at
/// most `cap` frames): how many frames it ran, and the time after.
fn settle(
    data: &mut SpectrumData,
    ticker: &mut SpectrumTicker,
    input: Option<EqInput<'_>>,
    mut t: Instant,
    cap: usize,
) -> (usize, Instant) {
    let mut frames = 0;
    while frames < cap {
        let dt = ticker.frame(t);
        frames += 1;
        t += Duration::from_millis(16);
        if !spectrum_tick(data, input, dt) && dt > 0.0 {
            ticker.stop();
            break;
        }
    }
    (frames, t)
}

#[test]
fn the_eq_analyzer_stops_ticking_once_settled_and_a_new_input_wakes_it() {
    let col = column(|b| -40.0 - (b % 7) as f32);
    let input = Some(EqInput {
        col: &col,
        bin_hz: BIN_HZ,
    });
    let mut data = SpectrumData::default();
    let mut ticker = SpectrumTicker::default();
    let (frames, t) = settle(&mut data, &mut ticker, input, Instant::now(), 600);
    assert!(frames < 600, "settled");
    assert!(!ticker.running(), "idle");
    assert!(data.has_signal);

    // Woken with nothing new (an unrelated rebuild): one frame, settled.
    let (again, t) = settle(
        &mut data,
        &mut ticker,
        input,
        t + Duration::from_secs(1),
        600,
    );
    assert_eq!(again, 1);

    // The playhead moved onto louder input: it runs again, then idles.
    let louder = column(|b| -20.0 - (b % 5) as f32);
    let moved = Some(EqInput {
        col: &louder,
        bin_hz: BIN_HZ,
    });
    let (frames, _) = settle(&mut data, &mut ticker, moved, t, 600);
    assert!(frames > 1);
    assert!(frames < 600);
}

#[test]
fn an_input_spectrum_arriving_while_the_analyzer_idles_shows() {
    // Idle and blank (no input yet): a computed column arrives.
    let mut data = SpectrumData::default();
    let mut ticker = SpectrumTicker::default();
    let (_, t) = settle(&mut data, &mut ticker, None, Instant::now(), 600);
    assert!(!data.has_signal, "an idle EQ stays blank");
    let col = column(|_| -20.0);
    let dt = ticker.frame(t);
    assert!(spectrum_tick(
        &mut data,
        Some(EqInput {
            col: &col,
            bin_hz: BIN_HZ
        }),
        dt
    ));
    assert!(data.has_signal);
}

#[test]
fn the_eq_analyzer_decays_to_the_floor_before_it_idles() {
    let col = column(|_| -10.0);
    let input = Some(EqInput {
        col: &col,
        bin_hz: BIN_HZ,
    });
    let mut data = SpectrumData::default();
    let mut ticker = SpectrumTicker::default();
    let mut t = Instant::now();
    for _ in 0..2 {
        let dt = ticker.frame(t);
        spectrum_tick(&mut data, input, dt);
        t += Duration::from_millis(16);
    }
    let ceil = data.ceil;
    // The input goes away (the playhead left the clip): the release takes a
    // few seconds at 34 dB/s, and the ticker runs all of it.
    let (frames, _) = settle(&mut data, &mut ticker, None, t, 1000);
    assert!(frames > 100, "the full release is animated ({frames})");
    assert!(frames < 1000);
    assert!(data.pre.iter().all(|&p| p == -90.0), "at the floor");
    assert_eq!(data.ceil, ceil, "the scale holds without input");
}

#[test]
fn the_ticker_counts_its_frames_like_a_restarted_flutter_ticker() {
    let mut ticker = SpectrumTicker::default();
    let t = Instant::now();
    // A (re)started ticker's first frames count 16 ms.
    assert_eq!(ticker.frame(t), 0.016);
    assert_eq!(ticker.frame(t + Duration::from_millis(20)), 0.016);
    assert!((ticker.frame(t + Duration::from_millis(45)) - 0.025).abs() < 1e-9);
    // A long gap counts at most 0.1 s.
    assert_eq!(ticker.frame(t + Duration::from_secs(3)), 0.1);
    ticker.stop();
    assert!(!ticker.running());
    assert_eq!(ticker.frame(t + Duration::from_secs(9)), 0.016);
    assert!(ticker.running());
}

#[test]
fn the_analyzer_asks_for_frames_only_while_it_moves() {
    // The EQ analyzer ticked at the display rate for as long as it was on
    // screen; a paused transport never let the app go idle.
    let fx = with(|fx| fx.eq_on = true);
    let col = column(|b| -40.0 - (b % 7) as f32);
    fn panel<'a>(fx: &'a TrackFx, col: &'a [f32]) -> FxPanel<'a> {
        FxPanel {
            eq_input: Some(EqInput {
                col,
                bin_hz: BIN_HZ,
            }),
            ..track(Some(fx))
        }
    }
    let mut frames = Frames::new(SIZE);
    let mut t = Instant::now();
    let mut n = 0;
    loop {
        let frame = frames.run(view(panel(&fx, &col)), Point::ORIGIN, &[redraw(t)]);
        n += 1;
        t += Duration::from_millis(16);
        if frame.redraw == window::RedrawRequest::Wait || n >= 600 {
            break;
        }
        assert_eq!(frame.redraw, window::RedrawRequest::NextFrame);
    }
    assert!(n < 600, "it settles and stops asking");

    // The playhead moved: a new column wakes it.
    let moved = column(|b| -25.0 - (b % 3) as f32);
    let frame = frames.run(view(panel(&fx, &moved)), Point::ORIGIN, &[redraw(t)]);
    assert_eq!(frame.redraw, window::RedrawRequest::NextFrame);
}

#[test]
fn the_analyzer_draws_a_takes_input_never_a_buss() {
    let col = column(|_| -30.0);
    let input = Some(EqInput {
        col: &col,
        bin_hz: BIN_HZ,
    });
    let fx = TrackFx::flat();
    let take = FxPanel {
        eq_input: input,
        ..track(Some(&fx))
    };
    assert!(take.analyzer_input().is_some(), "a take has a pre-EQ input");
    let routed = FxPanel {
        eq_input: input,
        ..bus(&fx)
    };
    assert!(
        routed.analyzer_input().is_none(),
        "a bus is a sum of routed takes — it has no input column"
    );
}

// -------------------------------------------------------------- GR meter

#[test]
fn the_gain_reduction_meter_shows_the_reading_it_is_given() {
    let fx = with(|fx| fx.comp_on = true);
    let mut ui = simulator(
        SIZE,
        view(FxPanel {
            gain_reduction_db: 6.0,
            ..bus(&fx)
        }),
    );
    assert!(text_at(&mut ui, "-6.0", 0).is_some());
}

#[test]
fn the_gain_reduction_meter_stops_polling_once_still() {
    let mut meter = GrMeter::default();
    meter.start();
    for i in 1..GrMeter::IDLE_POLLS {
        assert!(!meter.poll(0.0));
        assert!(meter.polling(), "poll {i}");
    }
    assert!(!meter.poll(0.0));
    assert!(!meter.polling(), "idle with nothing playing");

    // Woken: it polls again and shows the reduction, its peak falling
    // 0.8 dB a poll.
    meter.start();
    assert!(meter.poll(6.0));
    assert_eq!((meter.gr, meter.peak), (6.0, 6.0));
    assert!(meter.poll(0.0));
    assert_eq!(meter.gr, 0.0);
    assert!((meter.peak - 5.2).abs() < 1e-12);
    // Held still again (the peak settles first), it goes idle again.
    let mut polls = 0;
    while meter.polling() && polls < 100 {
        meter.poll(0.0);
        polls += 1;
    }
    assert!(!meter.polling());
    assert_eq!(meter.peak, 0.0);
    assert!(polls > GrMeter::IDLE_POLLS as usize);
}

#[test]
fn the_meter_bar_polls_at_20_hz_until_still_then_waits() {
    // The GR meter rebuilt at 20 Hz for as long as it was shown, even with
    // nothing playing.
    let fx = with(|fx| fx.comp_on = true);
    let mut frames = Frames::new(SIZE);
    let t0 = Instant::now();
    let first = frames.run(view(track(Some(&fx))), Point::ORIGIN, &[redraw(t0)]);
    assert_eq!(
        first.redraw,
        window::RedrawRequest::At(t0 + GrMeter::POLL),
        "woken: the first poll is due in 50 ms"
    );
    let mut polls = 0;
    let mut t = t0;
    loop {
        t += GrMeter::POLL;
        let frame = frames.run(view(track(Some(&fx))), Point::ORIGIN, &[redraw(t)]);
        polls += 1;
        if frame.redraw == window::RedrawRequest::Wait || polls > 50 {
            break;
        }
    }
    assert_eq!(
        polls,
        GrMeter::IDLE_POLLS as usize,
        "idle after ten still polls"
    );
    // Any later frame (the playhead moving rebuilds) wakes it.
    let woken = frames.run(
        view(track(Some(&fx))),
        Point::ORIGIN,
        &[redraw(t + Duration::from_secs(2))],
    );
    assert!(matches!(woken.redraw, window::RedrawRequest::At(_)));
}

#[test]
fn the_gr_bar_deepens_from_the_accent_to_red() {
    assert_eq!(gr_color(0.0), Studio::ACCENT);
    assert_eq!(gr_color(1.0 / 1.6), Studio::RECORD);
    assert_eq!(gr_color(1.0), Studio::RECORD);
}

// ------------------------------------------------------ the panel's state

// Regression guard: the inspector built its FxPanel without a key, so
// selecting another take (or bus) reused the panel's live state — a held
// slider, the EQ analyzer's smoothed spectrum, adaptive ceiling and latched
// "has signal" flag — from the previous channel.
#[test]
fn selecting_another_channel_gives_the_fx_panel_fresh_state() {
    let fx = with(|fx| fx.comp_on = true);
    let at = {
        let mut ui = simulator(SIZE, view(track(Some(&fx))));
        slider_at(&mut ui, "Threshold", 0.5)
    };
    let other = |fx| FxPanel::new(FxTarget::Track { id: "t2", fx });

    // Held on t1, then t2 selected: t2's slider owes nothing.
    let mut frames = Frames::new(SIZE);
    let pressed = frames.run(view(track(Some(&fx))), at, &[press()]);
    assert_eq!(track_edits(&pressed.messages).len(), 1);
    let released = frames.run(view(other(Some(&fx))), at, &[release()]);
    assert!(released.messages.is_empty());

    // The same channel kept: the release commits.
    let mut frames = Frames::new(SIZE);
    let _ = frames.run(view(track(Some(&fx))), at, &[press()]);
    let released = frames.run(view(track(Some(&fx))), at, &[release()]);
    assert_eq!(released.messages, vec![Message::CommitFx]);

    // A bus with the same id is another channel too.
    let mut frames = Frames::new(SIZE);
    let _ = frames.run(view(track(Some(&fx))), at, &[press()]);
    let as_bus = FxPanel::new(FxTarget::Bus { id: "t1", fx: &fx });
    let released = frames.run(view(as_bus), at, &[release()]);
    assert!(released.messages.is_empty());
}

#[test]
fn a_held_slider_keeps_its_drag_through_another_cards_edit() {
    // A reorder or an edit elsewhere in the chain rebuilds the panel; the
    // card under the pointer keeps its hold (cards are keyed by kind).
    let on = with(|fx| {
        fx.comp_on = true;
        fx.gate_on = true;
    });
    let at = {
        let mut ui = simulator(SIZE, view(track(Some(&on))));
        slider_at(&mut ui, "Attack", 0.5)
    };
    let mut frames = Frames::new(SIZE);
    let _ = frames.run(view(track(Some(&on))), at, &[press()]);
    // The compressor's threshold changes (another card); the gate's slider
    // is held on.
    let edited = TrackFx {
        comp: vocara_model::models::audio_fx::CompSettings {
            threshold_db: -30.0,
            ..on.comp
        },
        ..on.clone()
    };
    let moved = Point::new(at.x + 20.0, at.y);
    let frame = frames.run(
        view(track(Some(&edited))),
        moved,
        &[move_to(moved), release()],
    );
    let edits = track_edits(&frame.messages);
    assert_eq!(edits.len(), 1);
    assert!(edits[0].gate.attack_ms > 0.0);
    assert_eq!(frame.messages.last(), Some(&Message::CommitFx));
}

#[test]
fn a_toggle_changes_color_linearly_and_eases_its_thumb_over_130_ms() {
    use crate::ui::core::motion::Lerp;
    use crate::ui::core::widgets::animated::testing as anim;

    let view = |on| fx_toggle(on, vec![Message::CommitFx]);
    let mut window = anim::Frames::new(Size::new(40.0, 20.0));
    let at = |w: &anim::Frames, x: f32, c: Color| anim::near(w.pixel(Point::new(x, 9.0)), c);
    let t0 = Instant::now();
    window.frame(view(false), &[], t0);
    assert!(at(&window, 9.0, Studio::TEXT_SECONDARY) && at(&window, 28.0, Studio::BORDER));
    assert!(window.frame(view(true), &[], t0));
    // The thumb turns white at once, and starts where it was.
    assert!(at(&window, 9.0, Color::WHITE), "start");
    assert!(at(&window, 28.0, Studio::BORDER), "start");
    window.frame(view(true), &[], t0 + 65 * anim::MS);
    // Eased out, the thumb is past halfway (its middle at 9 + 16 × 0.68,
    // where a linear one would be at 17): its right edge covers 25.
    assert!(at(&window, 25.0, Color::WHITE), "the thumb eases out");
    let track = Studio::BORDER.lerp(Studio::ACCENT, 0.5);
    assert!(at(&window, 4.0, track), "the color is linear");
    assert!(!window.frame(view(true), &[], t0 + 130 * anim::MS));
    assert!(
        at(&window, 25.0, Color::WHITE) && at(&window, 4.0, Studio::ACCENT),
        "end"
    );
}
