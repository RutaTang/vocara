use iced::widget::container;

use super::*;
use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

const ON: Color = Color::from_rgb8(200, 80, 60);
const OFF: Color = Color::from_rgb8(40, 40, 90);

#[derive(Debug, Clone, PartialEq)]
struct Toggle;

fn view<'a>(on: bool) -> Element<'a, Toggle> {
    container(material_switch(on, Toggle, ON, OFF))
        .width(SIZE.width)
        .height(SIZE.height)
        .into()
}

fn window() -> Frames {
    Frames::new(SIZE)
}

fn at(x: f32, y: f32) -> Point {
    Point::new(x, y)
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

fn move_to(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::CursorMoved { position: at(x, y) })
}

#[test]
fn at_rest_it_draws_forui_s_outline_and_thumb_on_the_given_track() {
    let t0 = Instant::now();
    let mut off = window();
    assert!(!off.frame(view(false), &[], t0), "idle: no frames");
    // The 16 px thumb centred at (20, 20) in the background color.
    assert!(near(off.pixel(at(20.0, 20.0)), Zinc::BACKGROUND), "thumb");
    assert!(near(off.pixel(at(20.0, 10.0)), OFF), "track above it");
    assert!(near(off.pixel(at(44.0, 20.0)), OFF), "track");
    // The 2 px outline inside the track's edge: forui's border.
    assert!(near(off.pixel(at(30.0, 5.0)), Zinc::BORDER), "outline");
    assert!(near(off.pixel(at(2.0, 20.0)), BACKGROUND), "the 4 px aside");

    let mut on = window();
    on.frame(view(true), &[], t0);
    // The 24 px thumb centred at (40, 20); the outline forui's primary.
    assert!(near(on.pixel(at(40.0, 9.0)), Zinc::BACKGROUND), "thumb");
    assert!(near(on.pixel(at(20.0, 20.0)), ON), "track");
    assert!(near(on.pixel(at(30.0, 5.0)), Zinc::PRIMARY), "outline");
}

#[test]
fn a_tap_toggles_it_over_300_ms_overshooting_on_ease_out_back() {
    let mut w = window();
    let t0 = Instant::now();
    w.frame(view(false), &[], t0);
    w.point_at(at(20.0, 20.0));
    let (_, messages) = w.frame_messages(view(false), &[press(), release()], t0 + 10 * MS);
    assert_eq!(messages, [Toggle], "a click toggles");
    // Away, so the hover ring does not tint the track.
    w.point_at(at(-10.0, -10.0));
    // The app flips it: the toggle runs from the next frame.
    let t1 = t0 + 20 * MS;
    assert!(w.frame(view(true), &[], t1), "frames while it moves");
    // Halfway in time the thumb is past its end: easeOutBack(0.5) ≈ 1.067
    // of the way, its right end at ≈ 53.8 where it rests at 52.
    w.frame(view(true), &[], t1 + 150 * MS);
    assert!(
        near(w.pixel(at(52.0, 20.0)), Zinc::BACKGROUND),
        "overshot: the thumb"
    );
    let colors = Curve::EaseOut.transform(0.5);
    assert!(
        near(w.pixel(at(10.0, 20.0)), OFF.lerp(ON, colors)),
        "the track on its way"
    );
    assert!(!w.frame(view(true), &[], t1 + 300 * MS), "rests");
    assert!(near(w.pixel(at(52.0, 20.0)), ON), "settled back");
    assert!(
        near(w.pixel(at(40.0, 9.0)), Zinc::BACKGROUND),
        "the 24 px thumb"
    );
}

#[test]
fn the_first_toggle_from_rest_switches_the_slide_to_ease_out_back() {
    let t0 = Instant::now();
    let mut state = State::new(false);
    assert_eq!(state.position_curve, (Curve::EaseIn, Some(Curve::EaseOut)));
    state.frame(true, t0);
    assert_eq!(
        state.position_curve,
        (Curve::EaseOutBack, Some(Curve::EaseOutBackFlipped))
    );
    // Back off: the flipped curve dips below 0 near its end.
    state.frame(true, t0 + TOGGLE);
    state.frame(false, t0 + TOGGLE);
    let late = state.position_value(t0 + TOGGLE + TOGGLE.mul_f32(0.7));
    assert!(late < 0.0, "undershoots: {late}");
    assert_eq!(state.position_value(t0 + 2 * TOGGLE), 0.0);
}

#[test]
fn the_thumb_stretches_into_a_pill_on_its_way_and_back() {
    let off = round(INACTIVE_RADIUS);
    let on = round(ACTIVE_RADIUS);
    assert_eq!(thumb_size_forward(0.0, off, on), off);
    assert_eq!(thumb_size_forward(0.11, off, on), TRANSITIONAL);
    let stretching = thumb_size_forward(0.05, off, on);
    assert!(stretching.width > stretching.height, "{stretching:?}");
    assert_eq!(thumb_size_forward(0.83, off, on), on);
    assert_eq!(thumb_size_forward(1.0, off, on), on);
    // Going off: held, then stretched, then the off size.
    assert_eq!(thumb_size_reverse(1.0, off, on), on);
    assert_eq!(thumb_size_reverse(0.89, off, on), TRANSITIONAL);
    assert_eq!(thumb_size_reverse(0.17, off, on), off);
    assert_eq!(thumb_size_reverse(0.1, off, on), off);
}

#[test]
fn hovering_shows_the_ring_at_once_and_fades_its_color_in_over_50_ms() {
    let mut w = window();
    let t0 = Instant::now();
    w.frame(view(false), &[], t0);
    let ring = at(2.0, 20.0);
    assert!(near(w.pixel(ring), BACKGROUND), "no ring");
    w.point_at(at(20.0, 20.0));
    assert!(w.frame(view(false), &[], t0 + 10 * MS), "fading");
    // At radius 20 at once, from the pressed ring's dark color.
    let pressed = alpha(Zinc::BACKGROUND, f32::from(0x1F_u8) / 255.0);
    assert!(near(w.pixel(ring), over(pressed, BACKGROUND)), "start");
    assert!(!w.frame(view(false), &[], t0 + 60 * MS), "rests");
    let hover = alpha(Zinc::FOREGROUND, 0.08);
    assert!(
        near(w.pixel(ring), over(hover, BACKGROUND)),
        "the hover ring"
    );
    assert!(
        !near(over(hover, BACKGROUND), BACKGROUND),
        "the ring shows on the background"
    );
    // Leaving takes it away at once.
    w.point_at(at(-10.0, -10.0));
    w.frame(view(false), &[], t0 + 70 * MS);
    assert!(near(w.pixel(ring), BACKGROUND), "gone");
}

#[test]
fn a_held_press_grows_the_thumb_to_28_px_after_the_press_timeout() {
    let mut w = window();
    let t0 = Instant::now();
    w.point_at(at(20.0, 20.0));
    w.frame(view(false), &[press()], t0);
    let rim = at(20.0, 7.0);
    assert!(
        w.frame(view(false), &[], t0 + 50 * MS),
        "waits for the timeout"
    );
    assert!(
        !near(w.pixel(rim), Zinc::BACKGROUND),
        "not yet: the track (under the hover ring)"
    );
    // The reaction starts at 100 ms and runs 100 ms.
    w.frame(view(false), &[], t0 + 100 * MS);
    assert!(!w.frame(view(false), &[], t0 + 200 * MS), "held: rests");
    assert!(near(w.pixel(rim), Zinc::BACKGROUND), "the 28 px thumb");
    let (_, messages) = w.frame_messages(view(false), &[release()], t0 + 300 * MS);
    assert_eq!(messages, [Toggle], "a long press still taps");
}

#[test]
fn a_drag_moves_the_thumb_and_a_release_past_halfway_toggles() {
    let mut w = window();
    let t0 = Instant::now();
    w.point_at(at(20.0, 20.0));
    w.frame(view(false), &[press()], t0);
    // 2 px sideways: the drag wins and swallows them; 12 more: 0.6 along.
    w.point_at(at(22.0, 20.0));
    let (_, none) = w.frame_messages(view(false), &[], t0 + 10 * MS);
    w.point_at(at(34.0, 20.0));
    w.frame(view(false), &[], t0 + 20 * MS);
    assert!(none.is_empty(), "a drag does not tap");
    // The thumb's centre at 20 + 0.6 × 20 = 32 (the pressed thumb, 28 px
    // round once the reaction completes, from 18 to 46).
    w.frame(view(false), &[], t0 + 120 * MS);
    assert!(near(w.pixel(at(45.0, 20.0)), Zinc::BACKGROUND), "followed");
    let (_, messages) = w.frame_messages(view(false), &[release()], t0 + 130 * MS);
    assert_eq!(messages, [Toggle], "past halfway: toggles");
}

#[test]
fn a_drag_let_go_short_of_halfway_slides_back_linearly() {
    let mut w = window();
    let t0 = Instant::now();
    w.point_at(at(20.0, 20.0));
    w.frame(view(false), &[press()], t0);
    w.point_at(at(22.0, 20.0));
    w.frame(view(false), &[], t0 + 5 * MS);
    w.point_at(at(28.0, 20.0));
    w.frame(view(false), &[], t0 + 10 * MS);
    let (more, messages) = w.frame_messages(view(false), &[release()], t0 + 20 * MS);
    assert!(messages.is_empty(), "short of halfway: no toggle");
    assert!(more, "slides back");
    // 0.3 of the way back takes 0.3 × 300 = 90 ms.
    w.frame(view(false), &[], t0 + 65 * MS);
    let mut state = State::new(false);
    state.frame(false, t0);
    state.position.set(0.3, t0);
    state.position_curve = (Curve::Linear, None);
    state.position.animate(false, t0);
    assert!((state.position_value(t0 + 45 * MS) - 0.15).abs() < 1e-3);
    assert_eq!(state.position_value(t0 + 90 * MS), 0.0);
    assert!(!w.frame(view(false), &[], t0 + 200 * MS), "rests");
    assert!(near(w.pixel(at(20.0, 20.0)), Zinc::BACKGROUND), "home");
}

#[test]
fn a_press_that_wanders_off_is_no_tap() {
    let mut w = window();
    let t0 = Instant::now();
    w.point_at(at(20.0, 10.0));
    w.frame(view(false), &[press()], t0);
    let (_, messages) =
        w.frame_messages(view(false), &[move_to(20.0, 30.0), release()], t0 + 10 * MS);
    assert!(messages.is_empty());
}

#[test]
fn a_press_outside_it_does_nothing() {
    let mut w = Frames::new(Size::new(120.0, 40.0));
    let t0 = Instant::now();
    w.point_at(at(100.0, 20.0));
    let (more, messages) = w.frame_messages(view(false), &[press(), release()], t0);
    assert!(messages.is_empty());
    assert!(!more);
}
