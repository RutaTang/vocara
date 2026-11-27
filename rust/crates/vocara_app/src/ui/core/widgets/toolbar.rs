//! The unified toolbar (`DESIGN.md`, Toolbar): one 52 px row at the top of
//! every page, `Studio::TOOLBAR` with a bottom hairline, that is also the
//! window's title bar. It replaces the Flutter app's 28 px drag bar over a
//! 56 px page header.
//!
//! - **The traffic lights.** The window is frameless on macOS: a hidden,
//!   transparent title bar over a full-size content view, with an empty
//!   unified `NSToolbar` (`vocara_services::window_toolbar`) so AppKit
//!   centres the close, minimise and zoom buttons in the top 52 px. A page
//!   toolbar starts its content [`LEADING_INSET`] in (80 px on macOS) to
//!   clear them; elsewhere it starts 16 in.
//! - **Dragging.** The toolbar's empty area moves the window and a double
//!   click on it zooms (maximizes or restores) it, as the drag bar did and
//!   as `DragToMoveArea` did: the drag starts once a press pans (not on the
//!   press itself), and a double tap toggles. The app wraps every page in
//!   [`drag_region`], which watches the top [`HEIGHT`] px and starts the
//!   gesture only on a press nothing under it took (a button, a field, a
//!   slider capture theirs).
//! - **The environment badge.** Outside production the toolbar ends with
//!   the coral "DEV" / "TEST" pill. The app sets it once
//!   ([`set_environment_badge`]); every page's toolbar shows it at its
//!   right end, so the pages need not know the environment.
//!
//! A page builds its toolbar with [`page_toolbar`] from a row of its own
//! (its back button, [`title`] and [`subtitle`], its actions); the shared
//! controls ([`super::controls`]) give the buttons.

use std::cell::RefCell;
use std::time::Instant;

use iced::alignment::Vertical;
use iced::widget::{column, container, row, Space};
use iced::{mouse, Element, Event, Length, Padding};

use super::controls::{icon_button, IconButton};
use super::gesture::{Gesture, PanDoubleTap};
use super::line_icon::glyphs;
use super::studio_text::studio_text;
use super::wrap::Wrap;
use crate::ui::core::theme::{boxed, fill, font, type_scale, Studio};

/// The toolbar's height, its bottom hairline included.
pub const HEIGHT: f32 = crate::ui::core::theme::height::TOOLBAR;

/// Where a page toolbar's content starts: past the traffic lights on macOS
/// (80 px), 16 px in elsewhere.
pub const LEADING_INSET: f32 = if cfg!(target_os = "macos") {
    80.0
} else {
    16.0
};

/// The toolbar's right padding.
pub const TRAILING_INSET: f32 = 14.0;

thread_local! {
    /// The environment badge ("DEV" / "TEST"; empty in production), for
    /// every page's toolbar. The app's view runs on one thread (the main
    /// thread; a test's own), which sets it as it boots.
    static BADGE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sets the environment badge every page toolbar ends with (empty: none).
pub fn set_environment_badge(badge: &str) {
    BADGE.with(|b| *b.borrow_mut() = badge.to_string());
}

/// The environment badge page toolbars show now.
pub fn environment_badge() -> String {
    BADGE.with(|b| b.borrow().clone())
}

/// A page's toolbar: `content` (a row the page builds) in a 51 px row on
/// `Studio::TOOLBAR` over a 1 px `Studio::BORDER` hairline, starting
/// [`LEADING_INSET`] in and ending [`TRAILING_INSET`] in, then the
/// environment badge, if any. The content fills the row's width; its items
/// are centred vertically.
pub fn page_toolbar<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    let badge = environment_badge();
    let mut bar = row![container(content.into())
        .width(Length::Fill)
        .center_y(Length::Fill)]
    .height(Length::Fill)
    .align_y(Vertical::Center);
    if !badge.is_empty() {
        bar = bar.push(Space::new().width(10)).push(env_badge(&badge));
    }
    column![
        container(bar)
            .width(Length::Fill)
            .height(HEIGHT - 1.0)
            .padding(Padding {
                top: 0.0,
                right: TRAILING_INSET,
                bottom: 0.0,
                left: LEADING_INSET,
            })
            .style(fill(Studio::TOOLBAR)),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
    ]
    .width(Length::Fill)
    .height(HEIGHT)
    .into()
}

/// A page's title in its toolbar: 13 px semibold `TEXT_PRIMARY`, one line,
/// ellipsized.
pub fn title<'a, M: 'a>(text: impl Into<std::borrow::Cow<'a, str>>) -> Element<'a, M> {
    studio_text(text)
        .size(type_scale::TITLE)
        .font(font::SEMIBOLD)
        .line_height(iced::Pixels(18.0))
        .color(Studio::TEXT_PRIMARY)
        .ellipsis(1)
        .into()
}

/// A line after the title ("3 takes · 4 notes"): 12 px `TEXT_FAINT`.
pub fn subtitle<'a, M: 'a>(text: impl Into<std::borrow::Cow<'a, str>>) -> Element<'a, M> {
    studio_text(text)
        .size(type_scale::BODY)
        .line_height(iced::Pixels(type_scale::LINE))
        .tabular()
        .color(Studio::TEXT_FAINT)
        .ellipsis(1)
        .into()
}

/// The back button: a 28 px toolbar icon button with a 14 px chevron and a
/// "Back" tooltip.
pub fn back_button<'a, M: Clone + 'a>(on_press: M) -> IconButton<'a, M> {
    icon_button(glyphs::CHEVRON_LEFT, on_press)
        .icon_size(14.0)
        .tooltip("Back")
}

/// The coral pill marking a non-production environment: 9 px extra-bold
/// `Studio::BACKGROUND` on `Studio::ACCENT`, 0.8 letter spacing, radius 4.
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

/// `content` (the pages) with the toolbar's window gestures over its top
/// [`HEIGHT`] px: a press there that nothing took, then a pan, publishes
/// `on_drag` (start a window drag); a double tap publishes
/// `on_toggle_maximize`. Everything else passes through untouched.
pub fn drag_region<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    on_drag: M,
    on_toggle_maximize: M,
) -> Element<'a, M> {
    Wrap::<M, PanDoubleTap>::new(
        content,
        Box::new(move |gesture, event, bounds, cursor, shell| {
            let now = Instant::now();
            let completed = match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    let in_toolbar = cursor
                        .position_over(bounds)
                        .filter(|p| p.y - bounds.y < HEIGHT);
                    match in_toolbar {
                        // A control in the toolbar took it: not a drag.
                        Some(p) if !shell.is_event_captured() => {
                            gesture.press(p, now);
                            None
                        }
                        _ => {
                            // A press elsewhere ends any gesture in flight.
                            *gesture = PanDoubleTap::default();
                            None
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{button, column, text};
    use iced::Point;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Drag,
        Maximize,
        Pressed,
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }
    fn move_to(x: f32, y: f32) -> Event {
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(x, y),
        })
    }

    /// A page: a toolbar with a button at its right, a body under it.
    fn page<'a>() -> Element<'a, Msg> {
        drag_region(
            column![
                page_toolbar(
                    row![
                        title("Practices"),
                        Space::new().width(Length::Fill),
                        button(text("Go")).on_press(Msg::Pressed),
                    ]
                    .align_y(Vertical::Center),
                ),
                container(Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill),
            ],
            Msg::Drag,
            Msg::Maximize,
        )
    }

    #[test]
    fn the_toolbar_is_52_tall_and_starts_past_the_traffic_lights() {
        let mut ui = iced_test::simulator(page());
        let title = ui.find("Practices").expect("the title").bounds();
        assert_eq!(title.x, LEADING_INSET);
        assert!(title.y > 0.0 && title.y + title.height < HEIGHT);
        assert!(
            (title.center_y() - (HEIGHT - 1.0) / 2.0).abs() < 1.0,
            "{title:?}"
        );
        assert_eq!(HEIGHT, 52.0);
        if cfg!(target_os = "macos") {
            assert_eq!(LEADING_INSET, 80.0);
        }
    }

    #[test]
    fn a_press_that_pans_drags_the_window_and_a_double_tap_maximizes() {
        let mut ui = iced_test::simulator(page());
        ui.point_at(Point::new(300.0, 20.0));
        // A press alone does not drag.
        let _ = ui.simulate([press(), release()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        // The second tap completes a double tap.
        let _ = ui.simulate([press(), release()]);
        // A press that moves starts the drag.
        let _ = ui.simulate([press(), move_to(310.0, 20.0), release()]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Maximize, Msg::Drag]
        );
    }

    #[test]
    fn a_control_in_the_toolbar_and_the_page_below_keep_their_presses() {
        // The button: pressed, and a drag from it moves nothing.
        let mut ui = iced_test::simulator(page());
        let go = ui.find("Go").expect("the button").bounds().center();
        ui.point_at(go);
        let _ = ui.simulate([press(), move_to(go.x - 40.0, go.y), release()]);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Pressed]);
        // Below the toolbar: a drag there is the page's.
        let mut ui = iced_test::simulator(page());
        ui.point_at(Point::new(300.0, 200.0));
        let _ = ui.simulate([press(), move_to(340.0, 200.0), release()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = ui.simulate([press(), release(), press(), release()]);
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn shows_the_badge_outside_prod_only() {
        set_environment_badge("DEV");
        let mut ui = iced_test::simulator(page());
        assert!(ui.find("DEV").is_ok());
        set_environment_badge("");
        let mut ui = iced_test::simulator(page());
        assert!(ui.find("DEV").is_err());
    }
}
