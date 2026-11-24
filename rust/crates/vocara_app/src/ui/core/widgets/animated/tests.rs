use std::cell::RefCell;
use std::rc::Rc;

use iced::advanced::clipboard;
use iced::advanced::renderer::{self as core_renderer, Headless};
use iced::widget::Space;
use iced::{Color, Point, Size};
use iced_test::runtime::user_interface::{self, UserInterface};

use super::*;
use crate::ui::core::theme::{app_theme, font, DEFAULT_TEXT_SIZE};

const MS: Duration = Duration::from_millis(1);

/// A window the view is rebuilt in each frame, diffed into the last
/// frame's widget tree as the app does.
struct Window {
    renderer: Renderer,
    cache: user_interface::Cache,
}

impl Window {
    fn new() -> Self {
        let renderer = iced_test::futures::futures::executor::block_on(
            <Renderer as Headless>::new(font::REGULAR, DEFAULT_TEXT_SIZE.into(), None),
        )
        .expect("a headless renderer");
        Self {
            renderer,
            cache: user_interface::Cache::default(),
        }
    }

    /// Builds `view`, feeds it `events` with the pointer at `cursor`, draws
    /// it; whether it asked for another frame.
    fn frame<'a>(&mut self, view: Element<'a, ()>, cursor: Point, events: &[Event]) -> bool {
        let cursor = mouse::Cursor::Available(cursor);
        let mut ui = UserInterface::build(
            view,
            Size::new(200.0, 100.0),
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let (state, _) = ui.update(
            events,
            cursor,
            &mut self.renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        );
        ui.draw(
            &mut self.renderer,
            &app_theme(),
            &core_renderer::Style {
                text_color: Color::WHITE,
            },
            cursor,
        );
        self.cache = ui.into_cache();
        matches!(
            state,
            user_interface::State::Updated {
                redraw_request: window::RedrawRequest::NextFrame,
                ..
            }
        )
    }
}

fn redraw(at: Instant) -> Event {
    Event::Window(window::Event::RedrawRequested(at))
}

/// An [`Animated`] recording each value it builds its content from.
fn recorded<'a>(target: f32, seen: &Rc<RefCell<Vec<f32>>>) -> Element<'a, ()> {
    let seen = Rc::clone(seen);
    animated(target, 120 * MS, Curve::Linear, move |v: f32| {
        seen.borrow_mut().push(v);
        Space::new().width(10).height(10).into()
    })
    .into()
}

#[test]
fn an_animated_value_moves_to_its_new_target_and_then_rests() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let last = || *seen.borrow().last().expect("built");
    let mut window = Window::new();
    let t0 = Instant::now();
    let away = Point::new(150.0, 90.0);

    // The first target shows at once, and nothing moves.
    assert!(!window.frame(recorded(0.0, &seen), away, &[redraw(t0)]));
    assert_eq!(last(), 0.0);

    // A new target: it starts where it was, and asks for frames.
    assert!(window.frame(recorded(1.0, &seen), away, &[redraw(t0)]));
    assert_eq!(last(), 0.0, "start");
    assert!(window.frame(recorded(1.0, &seen), away, &[redraw(t0 + 60 * MS)]));
    assert!((last() - 0.5).abs() < 1e-3, "middle: {}", last());
    // At its end it rests, and asks for no more frames.
    assert!(!window.frame(recorded(1.0, &seen), away, &[redraw(t0 + 120 * MS)]));
    assert_eq!(last(), 1.0, "end");
    assert!(!window.frame(recorded(1.0, &seen), away, &[redraw(t0 + 500 * MS)]));
}

#[test]
fn an_animated_value_retargeted_mid_way_turns_from_where_it_is() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let last = || *seen.borrow().last().expect("built");
    let mut window = Window::new();
    let t0 = Instant::now();
    let away = Point::new(150.0, 90.0);
    window.frame(recorded(0.0, &seen), away, &[redraw(t0)]);
    window.frame(recorded(1.0, &seen), away, &[redraw(t0)]);
    window.frame(recorded(1.0, &seen), away, &[redraw(t0 + 30 * MS)]);
    assert!((last() - 0.25).abs() < 1e-3);
    // Back to 0 from 0.25, over the whole 120 ms.
    window.frame(recorded(0.0, &seen), away, &[redraw(t0 + 30 * MS)]);
    window.frame(recorded(0.0, &seen), away, &[redraw(t0 + 90 * MS)]);
    assert!((last() - 0.125).abs() < 1e-3, "{}", last());
    window.frame(recorded(0.0, &seen), away, &[redraw(t0 + 150 * MS)]);
    assert_eq!(last(), 0.0);
}

/// A 100 × 40 [`AnimatedContainer`] at the window's top left whose style
/// records whether it was hovered.
fn hover_box<'a>(hovers: &Rc<RefCell<Vec<bool>>>) -> Element<'a, ()> {
    let hovers = Rc::clone(hovers);
    animated_container(Space::new().width(100).height(40), 120 * MS)
        .style(move |hovered| {
            hovers.borrow_mut().push(hovered);
            Decoration::boxed(Color::BLACK, 4.0, Color::WHITE, 1.0)
        })
        .into()
}

#[test]
fn an_animated_container_follows_the_pointer_and_animates_only_while_it_changes() {
    let hovers = Rc::new(RefCell::new(Vec::new()));
    let mut window = Window::new();
    let t0 = Instant::now();
    let (over, away) = (Point::new(50.0, 20.0), Point::new(150.0, 90.0));
    assert!(!window.frame(hover_box(&hovers), away, &[redraw(t0)]));
    assert_eq!(hovers.borrow().last(), Some(&false));
    // The pointer comes in: a frame, styled hovered.
    let moved = Event::Mouse(mouse::Event::CursorMoved { position: over });
    assert!(window.frame(hover_box(&hovers), over, &[moved]));
    // (The style is the same either way here, so nothing animates.)
    assert!(!window.frame(hover_box(&hovers), over, &[redraw(t0 + 10 * MS)]));
    assert_eq!(hovers.borrow().last(), Some(&true));
}

#[test]
fn a_container_decoration_moves_by_its_curve() {
    let t0 = Instant::now();
    let rest = Decoration::boxed(Color::BLACK, 10.0, Color::from_rgb(0.2, 0.2, 0.2), 1.0);
    let hover = Decoration::boxed(Color::BLACK, 10.0, Color::from_rgb(1.0, 0.4, 0.2), 1.0);
    let look = |decoration| Look {
        decoration: Some(decoration),
        padding: Padding::ZERO,
        align: (-1.0, -1.0),
    };
    let mut state = State::new(look(rest), 120 * MS, Curve::Linear);
    let mut shell_messages: Vec<()> = Vec::new();
    let mut shell = Shell::new(&mut shell_messages);
    state.frame(look(hover), t0, &mut shell);
    let at = |t: Duration| state.motion.value(t0 + t).decoration.expect("a decoration");
    assert_eq!(at(Duration::ZERO).border, rest.border, "start");
    let mid = at(60 * MS).border;
    assert!(
        (mid.r - 0.6).abs() < 1e-3 && (mid.g - 0.3).abs() < 1e-3,
        "middle"
    );
    assert_eq!(at(120 * MS).border, hover.border, "end");
}

#[test]
fn a_padding_or_alignment_change_relays_out_until_it_rests() {
    let mut window = Window::new();
    let t0 = Instant::now();
    let away = Point::new(150.0, 90.0);
    let view = |dense: bool| -> Element<'_, ()> {
        animated_container(Space::new().width(10).height(10), 120 * MS)
            .padding(if dense { 12.0 } else { 22.0 })
            .into()
    };
    assert!(!window.frame(view(false), away, &[redraw(t0)]));
    assert!(window.frame(view(true), away, &[redraw(t0)]));
    assert!(window.frame(view(true), away, &[redraw(t0 + 60 * MS)]));
    assert!(!window.frame(view(true), away, &[redraw(t0 + 120 * MS)]));
}
