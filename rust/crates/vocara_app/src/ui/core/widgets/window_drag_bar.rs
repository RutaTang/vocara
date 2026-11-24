//! Port of `ui/core/widgets/window_drag_bar.dart`: a slim top bar acting as
//! the frameless window's drag region (like Electron's
//! `-webkit-app-region: drag`). The macOS traffic lights float over its left;
//! dragging moves the window and a double-click toggles maximize — as
//! window_manager's `DragToMoveArea` did: the drag starts once a press pans
//! (not on the press itself), and a double tap maximizes or restores.
//!
//! The bar publishes `on_drag` / `on_toggle_maximize`; the app turns them
//! into `window::drag` and `window::toggle_maximize` for its window.

use std::time::Instant;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::container;
use iced::{mouse, Element, Event, Length};

use super::gesture::{Gesture, PanDoubleTap};
use super::studio_text::studio_text;
use super::wrap::Wrap;
use crate::ui::core::theme::{boxed, fill, font, Studio};

/// The bar's height.
pub const HEIGHT: f32 = 28.0;

/// `WindowDragBar()`: `badge` is the environment pill ("DEV" / "TEST"),
/// empty in prod (the environment's `badge`).
pub fn window_drag_bar<'a, M: Clone + 'a>(
    badge: &str,
    on_drag: M,
    on_toggle_maximize: M,
) -> Element<'a, M> {
    // Coral environment pill on the right (DEV / TEST); nothing in prod.
    // Right-aligned to clear the traffic lights floating over the left.
    let bar = container(if badge.is_empty() {
        None
    } else {
        Some(env_badge(badge))
    })
    .width(Length::Fill)
    .height(HEIGHT)
    .padding(iced::Padding::ZERO.right(10))
    .align_x(Horizontal::Right)
    .align_y(Vertical::Center)
    .style(fill(Studio::SURFACE));
    Wrap::<M, PanDoubleTap>::new(
        bar,
        Box::new(move |gesture, event, bounds, cursor, shell| {
            let now = Instant::now();
            let completed = match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(p) = cursor.position_over(bounds) {
                        gesture.press(p, now);
                    }
                    None
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => gesture.moved(*position),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    gesture.release(now)
                }
                _ => None,
            };
            match completed {
                Some(Gesture::PanStart) => shell.publish(on_drag.clone()),
                Some(Gesture::DoubleTap) => shell.publish(on_toggle_maximize.clone()),
                _ => {}
            }
        }),
    )
    .into()
}

/// Small uppercase pill marking the non-production environment: 9 px w800
/// `Studio.background` on `Studio.accent`, 0.8 letter spacing, radius 4.
fn env_badge<'a, M: 'a>(label: &str) -> Element<'a, M> {
    container(
        studio_text(label.to_string())
            .size(9)
            .font(font::EXTRA_BOLD)
            .letter_spacing(0.8)
            .line_height(1.0)
            .color(Studio::BACKGROUND),
    )
    .padding([2, 6])
    .style(|_| boxed(Some(Studio::ACCENT), 4.0, None))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Point;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Drag,
        Maximize,
    }

    #[test]
    fn shows_the_badge_outside_prod_only() {
        let mut ui = iced_test::simulator(window_drag_bar("DEV", Msg::Drag, Msg::Maximize));
        assert!(ui.find("DEV").is_ok());
        let mut ui = iced_test::simulator(window_drag_bar("", Msg::Drag, Msg::Maximize));
        assert!(ui.find("DEV").is_err());
    }

    #[test]
    fn a_press_that_pans_drags_the_window_and_a_double_tap_maximizes() {
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let mut ui = iced_test::simulator(window_drag_bar("", Msg::Drag, Msg::Maximize));
        ui.point_at(Point::new(300.0, 10.0));
        // A press alone does not drag.
        let _ = ui.simulate([press.clone(), release.clone()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // The second tap completes a double tap.
        let _ = ui.simulate([press.clone(), release.clone()]);
        // A press that moves starts the drag.
        let _ = ui.simulate([
            press,
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(310.0, 10.0),
            }),
            release,
        ]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Maximize, Msg::Drag]
        );
    }
}
