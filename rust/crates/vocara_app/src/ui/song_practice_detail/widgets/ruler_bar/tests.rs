//! The ruler's tests: `ruler_bar_test.dart` (the loop handles: a click still
//! seeks, a narrow region still moves left, a trackpad swipe leaves the
//! handles alone, a slow drag at deep zoom keeps up with the pointer), and
//! the geometry and the body's scrub through the pure functions and a
//! headless ruler.

use iced::{mouse, Event, Point, Settings, Size};
use iced_test::Simulator;

use super::*;

const TIMELINE_MS: i64 = 10_000;
const SCREEN_W: f32 = 800.0;
/// The ticks' width: the window less the side insets and the header corner.
const LANE_W: f32 = SCREEN_W - 2.0 * TIMELINE_H_MARGIN - TRACK_HEADER_W;
const MID_Y: f32 = RULER_HEIGHT / 2.0;

fn ruler(view_start_ms: i64, view_ms: i64, loop_start_ms: i64, loop_end_ms: i64) -> RulerBar {
    RulerBar {
        timeline_duration_ms: view_ms,
        view_start_ms,
        position: Duration::ZERO,
        loop_start_ms,
        loop_end_ms,
        loop_enabled: true,
    }
}

/// The ruler at the window's top-left, 800 px wide, as the Dart test pumped
/// it.
fn pump(r: RulerBar) -> Simulator<'static, Message> {
    Simulator::with_size(
        Settings::default(),
        Size::new(SCREEN_W, 600.0),
        container(ruler_bar(r)).width(SCREEN_W),
    )
}

/// The ruler's x for `ms` of the 10 s view.
fn at(ms: f64) -> Point {
    at_of(ms, TIMELINE_MS)
}

fn at_of(ms: f64, view_ms: i64) -> Point {
    Point::new(
        TIMELINE_H_MARGIN + TRACK_HEADER_W + (ms / view_ms as f64) as f32 * LANE_W,
        MID_Y,
    )
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

fn move_to(p: Point) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: p })
}

/// A press at `from`, then moves by each of `steps` (x only), then the
/// release.
fn drag(from: Point, steps: &[f32]) -> Vec<Event> {
    let mut events = vec![press()];
    let mut x = from.x;
    for dx in steps {
        x += dx;
        events.push(move_to(Point::new(x, from.y)));
    }
    events.push(release());
    events
}

fn starts(messages: &[Message]) -> Vec<i64> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetLoopStart(ms) => Some(*ms),
            _ => None,
        })
        .collect()
}

fn ends(messages: &[Message]) -> Vec<i64> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetLoopEnd(ms) => Some(*ms),
            _ => None,
        })
        .collect()
}

fn seeks(messages: &[Message]) -> Vec<i64> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::Seek(d) => Some(d.as_millis() as i64),
            _ => None,
        })
        .collect()
}

fn run(r: RulerBar, at: Point, events: Vec<Event>) -> Vec<Message> {
    let mut ui = pump(r);
    ui.point_at(at);
    let _ = ui.simulate(events);
    ui.into_messages().collect()
}

// ------------------------------------------------------- ruler_bar_test.dart

#[test]
fn a_click_on_a_loop_handle_still_seeks() {
    // The default loop starts at 0: its handle covered the start of the
    // ruler, and a click there to go back to 0:00 did nothing.
    let click = at(0.0) + iced::Vector::new(3.0, 0.0);
    let messages = run(
        ruler(0, TIMELINE_MS, 0, 8000),
        click,
        vec![press(), release()],
    );
    let s = seeks(&messages);
    assert_eq!(s.len(), 1, "{messages:?}");
    let expected = 3.0 / f64::from(LANE_W) * TIMELINE_MS as f64;
    assert!((s[0] as f64 - expected).abs() <= 2.0, "{s:?} vs {expected}");
    assert!(starts(&messages).is_empty());
}

#[test]
fn a_loop_narrower_than_a_handle_still_moves_left() {
    // The end's handle lies over the start's: a leftward drag took the end,
    // which stops at the start, so the region could not move left at all.
    let r = ruler(0, TIMELINE_MS, 5000, 5100);
    let from = at(5050.0);
    // Past the drag slop, then on.
    let left = run(r, from, drag(from, &[-10.0, -20.0]));
    assert!(!starts(&left).is_empty(), "{left:?}");
    assert!(*starts(&left).last().unwrap() < 5000);
    assert!(ends(&left).is_empty());

    let right = run(r, from, drag(from, &[10.0, 20.0]));
    assert!(*ends(&right).last().unwrap() > 5100, "{right:?}");
}

#[test]
fn a_narrow_loop_whose_start_grip_shows_still_shrinks_from_the_end() {
    // 12 px wide: the end's handle covers part of the start's, not its grip.
    let ms = 12.0 / f64::from(LANE_W) * TIMELINE_MS as f64;
    let r = ruler(0, TIMELINE_MS, 5000, (5000.0 + ms).round() as i64);
    let from = at(5000.0 + ms);
    let messages = run(r, from, drag(from, &[-2.0, -3.0]));
    assert!(!ends(&messages).is_empty(), "{messages:?}");
    assert!(starts(&messages).is_empty());
}

#[test]
fn a_two_finger_trackpad_swipe_over_a_loop_handle_leaves_it() {
    // iced delivers the swipe as wheel scrolls, never as a button drag.
    let swipe = |dx| {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: dx, y: 0.0 },
        })
    };
    let messages = run(
        ruler(0, TIMELINE_MS, 2000, 8000),
        at(8000.0),
        vec![swipe(-40.0), swipe(-120.0)],
    );
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn a_mouse_drag_still_moves_the_loop_edge() {
    let from = at(8000.0);
    let messages = run(
        ruler(0, TIMELINE_MS, 2000, 8000),
        from,
        drag(from, &[-30.0, -30.0]),
    );
    assert!(!ends(&messages).is_empty(), "{messages:?}");
    assert!(*ends(&messages).last().unwrap() < 8000);
}

#[test]
fn a_slow_drag_at_deep_zoom_moves_the_edge_as_far_as_the_pointer() {
    // Each tick added its own rounded delta: at about 1.3 ms per px a quarter
    // pixel rounds to nothing, so the handle never moved.
    const VIEW_MS: i64 = 1000;
    let from = at_of(500.0, VIEW_MS);
    let mut steps = vec![4.0]; // past the drag slop
    steps.extend(std::iter::repeat_n(0.25, 240));
    let messages = run(ruler(7500, VIEW_MS, 2000, 8000), from, drag(from, &steps));
    let e = ends(&messages);
    let moved = e.last().unwrap() - 8000;
    let expected = 60.0 / f64::from(LANE_W) * VIEW_MS as f64;
    assert!(
        (moved as f64 - expected).abs() <= 2.0,
        "{moved} vs {expected}"
    );
}

// ------------------------------------------------------------------ the body

#[test]
fn a_drag_on_the_body_scrubs_from_its_second_move() {
    // The move that wins the drag is swallowed (DragStartBehavior.start);
    // each one after seeks to where the pointer is, clamped to the view.
    let from = at(4000.0);
    let messages = run(
        ruler(0, TIMELINE_MS, 0, TIMELINE_MS),
        from,
        drag(from, &[10.0, 20.0, 2000.0]),
    );
    let s = seeks(&messages);
    let expected = (4000.0 + 30.0 / f64::from(LANE_W) * TIMELINE_MS as f64).round() as i64;
    assert_eq!(s, vec![expected, TIMELINE_MS], "{messages:?}");
}

#[test]
fn a_press_that_strays_up_or_down_is_neither_a_tap_nor_a_scrub() {
    let from = at(4000.0);
    let mut ui = pump(ruler(0, TIMELINE_MS, 0, TIMELINE_MS));
    ui.point_at(from);
    let _ = ui.simulate([
        press(),
        move_to(Point::new(from.x, from.y + 30.0)),
        release(),
    ]);
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn with_no_span_the_ruler_takes_no_input() {
    let messages = run(ruler(0, 0, 0, 0), at(0.0), vec![press(), release()]);
    assert!(messages.is_empty());
}

#[test]
fn a_click_on_the_header_corner_is_not_the_ruler() {
    let messages = run(
        ruler(0, TIMELINE_MS, 2000, 8000),
        Point::new(TIMELINE_H_MARGIN + 100.0, MID_Y),
        vec![press(), release()],
    );
    assert!(messages.is_empty());
}

#[test]
fn the_ruler_draws_in_every_state() {
    for r in [
        ruler(0, TIMELINE_MS, 2000, 8000),
        ruler(0, 0, 0, 0),
        // A loop overflowing the view on both sides.
        ruler(3000, 1000, 0, TIMELINE_MS),
        RulerBar {
            position: Duration::from_millis(5000),
            loop_enabled: false,
            ..ruler(0, TIMELINE_MS, 6000, 2000)
        },
    ] {
        let mut ui = pump(r);
        ui.snapshot(&crate::ui::core::theme::app_theme())
            .expect("snapshot");
    }
}

// ----------------------------------------------------------------- geometry

#[test]
fn a_time_maps_onto_the_ticks_and_back() {
    let w = f64::from(LANE_W);
    assert_eq!(x_of(7500, 7500, 1000, w), 0.0);
    assert!((x_of(8000, 7500, 1000, w) - w / 2.0).abs() < 1e-9);
    // Off the view on either side: unclamped.
    assert!(x_of(0, 7500, 1000, w) < 0.0);
    assert_eq!(ms_at(w / 2.0, 7500, 1000, w), 8000);
    // Clamped to the view, and rounded half away from zero.
    assert_eq!(ms_at(-50.0, 7500, 1000, w), 7500);
    assert_eq!(ms_at(w + 50.0, 7500, 1000, w), 8500);
    assert_eq!(ms_at(1.0, 0, 3, 2.0), 2);
}

#[test]
fn the_band_is_clamped_to_the_view_and_tinted_by_the_loop_state() {
    assert_eq!(loop_fill(-40.0, 900.0, 564.0), (0.0, 564.0));
    assert_eq!(loop_fill(10.0, 20.0, 564.0), (10.0, 20.0));
    assert!((loop_fill_color(true).a - 0.20).abs() < 1e-6);
    assert!((loop_fill_color(false).a - 0.07).abs() < 1e-6);
}

#[test]
fn the_end_handle_lies_over_the_start_handle() {
    let w = 564.0;
    assert_eq!(handle_at(100.0, w, 100.0, 104.0), Some(Edge::End));
    assert_eq!(handle_at(93.0, w, 100.0, 104.0), Some(Edge::Start));
    assert_eq!(handle_at(120.0, w, 100.0, 104.0), None);
    // Only inside the ruler: the start handle at 0 is half off it.
    assert_eq!(handle_at(-3.0, w, 0.0, 300.0), None);
    assert_eq!(handle_at(3.0, w, 0.0, 300.0), Some(Edge::Start));
    assert!(end_takes_leftward(100.0, 107.9));
    assert!(!end_takes_leftward(100.0, 112.0));
}

#[test]
fn a_handle_drag_adds_the_whole_distance_not_each_rounded_tick() {
    let ms_per_px = 1000.0 / 564.0;
    let mut dx = 0.0;
    let mut last = 8000;
    for _ in 0..240 {
        dx += 0.25;
        last = dragged_edge_ms(8000, dx, ms_per_px);
    }
    assert_eq!(last, 8000 + (60.0 * ms_per_px).round() as i64);
    // Rounding each quarter pixel on its own would never have moved it.
    assert_eq!((0.25f64 * ms_per_px).round(), 0.0);
}
