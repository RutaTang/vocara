use std::cell::RefCell;
use std::rc::Rc;

use iced::widget::Space;
use iced::{Color, Point, Size};

use super::testing::{near, over, Frames, BACKGROUND, MS};
use super::*;

/// An [`Animated`] recording each value it builds its content from.
fn recorded<'a>(target: f32, seen: &Rc<RefCell<Vec<f32>>>) -> Element<'a, ()> {
    let seen = Rc::clone(seen);
    animated(target, 120 * MS, Curve::Linear, move |v: f32| {
        seen.borrow_mut().push(v);
        Space::new().width(10).height(10).into()
    })
    .into()
}

fn window() -> Frames {
    Frames::new(Size::new(200.0, 100.0))
}

#[test]
fn an_animated_value_moves_to_its_new_target_and_then_rests() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let last = || *seen.borrow().last().expect("built");
    let mut window = window();
    let t0 = Instant::now();

    // The first target shows at once, and nothing moves.
    assert!(!window.frame(recorded(0.0, &seen), &[], t0));
    assert_eq!(last(), 0.0);

    // A new target: it starts where it was, and asks for frames.
    assert!(window.frame(recorded(1.0, &seen), &[], t0));
    assert_eq!(last(), 0.0, "start");
    assert!(window.frame(recorded(1.0, &seen), &[], t0 + 60 * MS));
    assert!((last() - 0.5).abs() < 1e-3, "middle: {}", last());
    // At its end it rests, and asks for no more frames.
    assert!(!window.frame(recorded(1.0, &seen), &[], t0 + 120 * MS));
    assert_eq!(last(), 1.0, "end");
    assert!(!window.frame(recorded(1.0, &seen), &[], t0 + 500 * MS));
}

#[test]
fn an_animated_value_retargeted_mid_way_turns_from_where_it_is() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let last = || *seen.borrow().last().expect("built");
    let mut window = window();
    let t0 = Instant::now();
    window.frame(recorded(0.0, &seen), &[], t0);
    window.frame(recorded(1.0, &seen), &[], t0);
    window.frame(recorded(1.0, &seen), &[], t0 + 30 * MS);
    assert!((last() - 0.25).abs() < 1e-3);
    // Back to 0 from 0.25, over the whole 120 ms.
    window.frame(recorded(0.0, &seen), &[], t0 + 30 * MS);
    window.frame(recorded(0.0, &seen), &[], t0 + 90 * MS);
    assert!((last() - 0.125).abs() < 1e-3, "{}", last());
    window.frame(recorded(0.0, &seen), &[], t0 + 150 * MS);
    assert_eq!(last(), 0.0);
}

const REST: Color = Color::from_rgb(0.2, 0.2, 0.2);
const HOVER: Color = Color::from_rgb(1.0, 0.4, 0.2);

/// A 100 × 40 [`AnimatedContainer`] at the window's top left, filled
/// [`REST`], [`HOVER`] while hovered.
fn hover_box<'a>() -> Element<'a, ()> {
    animated_container(Space::new().width(100).height(40), 120 * MS)
        .style(|hovered| Decoration::fill(if hovered { HOVER } else { REST }, 0.0))
        .into()
}

#[test]
fn an_animated_container_follows_the_pointer_over_its_duration() {
    let mut window = window();
    let t0 = Instant::now();
    let inside = Point::new(50.0, 20.0);
    assert!(!window.frame(hover_box(), &[], t0));
    assert!(near(window.pixel(inside), REST));

    window.point_at(inside);
    assert!(window.frame(hover_box(), &[], t0), "it moves");
    assert!(near(window.pixel(inside), REST), "start");
    assert!(window.frame(hover_box(), &[], t0 + 60 * MS));
    assert!(
        near(window.pixel(inside), REST.lerp(HOVER, 0.5)),
        "middle: {:?}",
        window.pixel(inside)
    );
    assert!(!window.frame(hover_box(), &[], t0 + 120 * MS), "and rests");
    assert!(near(window.pixel(inside), HOVER), "end");

    // Out again, back the same way.
    window.point_at(Point::new(150.0, 90.0));
    window.frame(hover_box(), &[], t0 + 200 * MS);
    window.frame(hover_box(), &[], t0 + 260 * MS);
    assert!(near(window.pixel(inside), HOVER.lerp(REST, 0.5)));
    window.frame(hover_box(), &[], t0 + 320 * MS);
    assert!(near(window.pixel(inside), REST));
}

#[test]
fn an_ink_hover_fades_its_highlight_in_over_50_ms() {
    let mut window = window();
    let t0 = Instant::now();
    let inside = Point::new(50.0, 20.0);
    let row = || ink_hover::<()>(Space::new().width(100).height(40), HOVER, 0.0);
    window.frame(row(), &[], t0);
    assert!(near(window.pixel(inside), BACKGROUND), "none at rest");
    window.point_at(inside);
    window.frame(row(), &[], t0);
    assert!(near(window.pixel(inside), BACKGROUND), "start");
    window.frame(row(), &[], t0 + 25 * MS);
    let half = over(Color { a: 0.5, ..HOVER }, BACKGROUND);
    assert!(near(window.pixel(inside), half), "middle: alpha only");
    assert!(!window.frame(row(), &[], t0 + 50 * MS));
    assert!(near(window.pixel(inside), HOVER), "end");
}

#[test]
fn an_animated_container_draws_its_opacity_at_once() {
    let mut window = window();
    let t0 = Instant::now();
    let inside = Point::new(50.0, 20.0);
    let view = |opacity: f32| -> Element<'_, ()> {
        animated_container(Space::new().width(100).height(40), 120 * MS)
            .decoration(Decoration::fill(HOVER, 0.0))
            .opacity(opacity)
            .into()
    };
    window.frame(view(1.0), &[], t0);
    assert!(!window.frame(view(0.5), &[], t0), "nothing animates");
    assert!(near(
        window.pixel(inside),
        over(Color { a: 0.5, ..HOVER }, BACKGROUND)
    ));
}

#[test]
fn a_padding_or_alignment_change_relays_out_over_its_duration() {
    let mut window = window();
    let t0 = Instant::now();
    let view = |right: bool| -> Element<'_, ()> {
        let dot = animated_container(Space::new().width(10).height(10), 120 * MS)
            .decoration(Decoration::fill(HOVER, 0.0));
        animated_container(dot, 120 * MS)
            .width(110)
            .height(10)
            .align((if right { 1.0 } else { -1.0 }, 0.0))
            .into()
    };
    let filled = |window: &Frames, x: f32| near(window.pixel(Point::new(x, 5.0)), HOVER);
    assert!(!window.frame(view(false), &[], t0));
    assert!(filled(&window, 5.0) && !filled(&window, 55.0));
    assert!(window.frame(view(true), &[], t0));
    assert!(filled(&window, 5.0), "start");
    assert!(window.frame(view(true), &[], t0 + 60 * MS));
    assert!(filled(&window, 55.0) && !filled(&window, 5.0), "middle");
    assert!(!window.frame(view(true), &[], t0 + 120 * MS));
    assert!(filled(&window, 105.0) && !filled(&window, 55.0), "end");
}

#[test]
fn a_container_decoration_moves_by_its_curve() {
    let t0 = Instant::now();
    let rest = Decoration::boxed(Color::BLACK, 10.0, REST, 1.0);
    let hover = Decoration::boxed(Color::BLACK, 10.0, HOVER, 2.0);
    let look = |decoration| Look {
        decoration: Some(decoration),
        padding: Padding::ZERO,
        align: (-1.0, -1.0),
    };
    let mut state = State::new(look(rest), 120 * MS, Curve::EaseOut);
    let mut messages: Vec<()> = Vec::new();
    let mut shell = Shell::new(&mut messages);
    state.frame(look(hover), Curve::EaseOut, t0, &mut shell);
    let at = |t: Duration| state.motion.value(t0 + t).decoration.expect("a decoration");
    assert_eq!(at(Duration::ZERO), rest, "start");
    let k = Curve::EaseOut.transform(0.5);
    let mid = at(60 * MS);
    assert!(
        (mid.border.r - REST.r.lerp(HOVER.r, k)).abs() < 1e-3,
        "middle"
    );
    assert!((mid.border_width - 1.0.lerp(2.0, k)).abs() < 1e-3);
    assert_eq!(at(120 * MS), hover, "end");
}
