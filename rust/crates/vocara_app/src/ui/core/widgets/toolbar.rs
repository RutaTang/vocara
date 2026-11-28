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
//!   press itself), and a double tap toggles. The app wraps the window's
//!   content (its pages, their dialogs, the waiting and fatal screens) in
//!   [`drag_region`], which watches the top [`HEIGHT`] px and starts the
//!   gesture only on a press nothing under it took (a button, a field, a
//!   slider capture theirs; so does a disabled button). A surface over the
//!   whole window that takes every press (a dialog's barrier, the lyrics
//!   Focus view, a video's fullscreen) lets the band's presses that nothing
//!   in it took through to the window ([`window_backdrop`]): the
//!   content view answers `mouseDownCanMoveWindow` NO
//!   (`vocara_services::window_toolbar`), so AppKit never moves the window
//!   by itself, and under such a surface it could not be moved at all.
//! - **The environment badge.** Outside production the toolbar ends with
//!   the coral "DEV" / "TEST" pill. The app sets it once
//!   ([`set_environment_badge`]); every page's toolbar shows it at its
//!   right end, so the pages need not know the environment.
//!
//! A page builds its toolbar with [`page_toolbar`] from a row of its own
//! (its back button, [`title`] and [`subtitle`], its actions); the shared
//! controls ([`super::controls`]) give the buttons.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::{column, container, row, Space};
use iced::{
    mouse, Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Theme, Vector,
};

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

    /// Where a press in the title-bar band went down that a
    /// [`window_backdrop`] let through to the window, for the
    /// [`drag_region`] around it, which reads it back as the press reaches
    /// it (on the same thread, in the same update).
    static LET_THROUGH: Cell<Option<Point>> = const { Cell::new(None) };
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

/// Whether `event` is a primary press in the window's title-bar band: its
/// top [`HEIGHT`] px, in window coordinates (where the pointer is).
pub fn is_band_press(event: &Event, cursor: mouse::Cursor) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    ) && cursor.position().is_some_and(|p| p.y < HEIGHT)
}

/// `content` (the window's content, its dialogs too) with the toolbar's
/// window gestures over its top [`HEIGHT`] px: a press there that nothing
/// took, or that a [`window_backdrop`] let through, then a pan, publishes
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
                    let let_through = LET_THROUGH
                        .take()
                        .is_some_and(|at| Some(at) == cursor.position());
                    match in_toolbar {
                        // A control in the toolbar took it: not a drag.
                        Some(p) if !shell.is_event_captured() || let_through => {
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

/// `content` as a surface over the whole window (a dialog's barrier, a
/// full-window view) whose part of the title-bar band is still the
/// window's: a primary press there that nothing in it took goes on to the
/// [`drag_region`] around it, which moves or zooms the window from it.
/// Whatever took its press keeps it (a button, a field, a scrollbar's
/// thumb), whatever cursor it shows.
///
/// It takes every press over it, and shows the arrow where the content
/// shows nothing, as `opaque` does: it stands in for one. An `opaque`
/// inside would take every press itself, and leave the window none.
pub fn window_backdrop<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Element::new(Backdrop {
        content: content.into(),
    })
}

struct Backdrop<'a, M> {
    content: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for Backdrop<'_, M> {
    // The content's own tree, as `opaque`'s: put in place of one, the
    // surface keeps its state.
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let over = cursor.position_over(layout.bounds());
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        // A press the content took is its own, whatever it shows under the
        // pointer: a scrollbar's thumb shows the arrow, and read by its
        // cursor, a drag on it near the top moved the window instead.
        if let Some(at) = over.filter(|_| is_band_press(event, cursor)) {
            if !shell.is_event_captured() {
                LET_THROUGH.set(Some(at));
            }
        }
        if over.is_some() && matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_))) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction = self
            .content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer);
        if interaction == mouse::Interaction::None && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{button, column, stack, text};
    use iced::Point;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Drag,
        Maximize,
        Pressed,
        Dismissed,
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

    /// A press at `at` that pans, then a double tap there: what the window
    /// gestures and the rest report.
    fn pan_then_double_tap(ui: &mut iced_test::Simulator<'_, Msg>, at: Point) {
        ui.point_at(at);
        let _ = ui.simulate([press(), move_to(at.x + 10.0, at.y), release()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        ui.point_at(at);
        let _ = ui.simulate([press(), release()]);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = ui.simulate([press(), release()]);
    }

    #[test]
    fn a_disabled_button_in_the_toolbar_moves_and_zooms_nothing() {
        // iced's disabled button let the press through: a double click on
        // the greyed Play zoomed the window, a drag on it moved it.
        use crate::ui::core::widgets::controls::{group_button, icon_button};
        use crate::ui::core::widgets::line_icon::glyphs;
        let page = || {
            drag_region(
                column![
                    page_toolbar(row![
                        title("Practices"),
                        Space::new().width(Length::Fill),
                        icon_button(glyphs::PLAY, Msg::Pressed)
                            .enabled(false)
                            .tooltip("Play"),
                        group_button(glyphs::PLAY, "Go to start", None::<Msg>, false),
                    ]),
                    Space::new().height(Length::Fill),
                ],
                Msg::Drag,
                Msg::Maximize,
            )
        };
        for label in ["Play", "Go to start"] {
            let mut ui = iced_test::simulator(page());
            let at = ui.find(label).expect("the button").bounds().center();
            pan_then_double_tap(&mut ui, at);
            assert!(ui.into_messages().next().is_none(), "{label}");
        }
        // Beside them, the toolbar still moves and zooms the window.
        let mut ui = iced_test::simulator(page());
        pan_then_double_tap(&mut ui, Point::new(300.0, 20.0));
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Drag, Msg::Maximize]
        );
    }

    #[test]
    fn a_dialogs_barrier_lets_the_title_bar_band_move_and_zoom_the_window() {
        // The content view no longer moves the window by itself, and the
        // barrier took every press: under a dialog the window could not be
        // moved at all.
        use crate::ui::core::widgets::confirm_dialog::barrier;
        let window = || {
            let barrier = barrier(iced::Color::BLACK, 0.7, Some(Msg::Dismissed));
            drag_region(stack![page(), barrier], Msg::Drag, Msg::Maximize)
        };
        let mut ui = iced_test::simulator(window());
        pan_then_double_tap(&mut ui, Point::new(300.0, 20.0));
        // The band's taps dismiss nothing, and the toolbar's button under
        // the barrier takes nothing.
        let go = ui.find("Go").expect("the button").bounds().center();
        ui.point_at(go);
        let _ = ui.simulate([press(), release()]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Drag, Msg::Maximize]
        );
        // Below the band, a tap on the barrier dismisses it.
        let mut ui = iced_test::simulator(window());
        ui.point_at(Point::new(300.0, 200.0));
        let _ = ui.simulate([press(), release()]);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Dismissed]);
    }

    #[test]
    fn a_full_window_view_lets_its_empty_band_through_and_keeps_its_controls() {
        // The lyrics Focus view or a video's fullscreen: a view over the
        // whole window with a control in the band.
        let view = || {
            window_backdrop(
                column![
                    row![
                        Space::new().width(Length::Fill),
                        button(text("Exit")).on_press(Msg::Pressed),
                    ]
                    .height(HEIGHT),
                    Space::new().height(Length::Fill),
                ]
                .width(Length::Fill)
                .height(Length::Fill),
            )
        };
        let window = || drag_region(stack![page(), view()], Msg::Drag, Msg::Maximize);
        let mut ui = iced_test::simulator(window());
        pan_then_double_tap(&mut ui, Point::new(300.0, 20.0));
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Drag, Msg::Maximize]
        );
        // Its control keeps its press: a drag from it moves nothing.
        let mut ui = iced_test::simulator(window());
        let exit = ui.find("Exit").expect("the button").bounds().center();
        ui.point_at(exit);
        let _ = ui.simulate([press(), move_to(exit.x - 40.0, exit.y), release()]);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Pressed]);
        // Below the band, nothing is the window's.
        let mut ui = iced_test::simulator(window());
        pan_then_double_tap(&mut ui, Point::new(300.0, 200.0));
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn a_control_in_the_band_that_shows_the_arrow_keeps_its_press() {
        // A scrollbar's thumb takes its press and shows the arrow (or
        // nothing, which the surface shows as the arrow). Read by its
        // cursor, it was no control: a drag on the lyrics Focus view's
        // thumb at the top of its list moved the window.
        for shows in [mouse::Interaction::Idle, mouse::Interaction::None] {
            let thumb = move || {
                Wrap::<Msg, ()>::new(
                    Space::new().width(20).height(HEIGHT),
                    Box::new(|_, event, bounds, cursor, shell| {
                        let press = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)));
                        if press && cursor.is_over(bounds) {
                            shell.publish(Msg::Pressed);
                            shell.capture_event();
                        }
                    }),
                )
                .interaction(Box::new(move |_, bounds, cursor| {
                    cursor.is_over(bounds).then_some(shows)
                }))
            };
            let view = move || {
                window_backdrop(
                    column![
                        row![thumb(), Space::new().width(Length::Fill)].height(HEIGHT),
                        Space::new().height(Length::Fill),
                    ]
                    .width(Length::Fill)
                    .height(Length::Fill),
                )
            };
            let window = || drag_region(stack![page(), view()], Msg::Drag, Msg::Maximize);
            let mut ui = iced_test::simulator(window());
            pan_then_double_tap(&mut ui, Point::new(10.0, 26.0));
            assert_eq!(
                ui.into_messages().collect::<Vec<_>>(),
                vec![Msg::Pressed; 3],
                "{shows:?}"
            );
            // Beside it, the band is still the window's.
            let mut ui = iced_test::simulator(window());
            pan_then_double_tap(&mut ui, Point::new(300.0, 20.0));
            assert_eq!(
                ui.into_messages().collect::<Vec<_>>(),
                vec![Msg::Drag, Msg::Maximize],
                "{shows:?}"
            );
        }
    }

    #[test]
    fn a_drag_on_a_scrollbars_thumb_in_the_band_scrolls_and_moves_no_window() {
        // The lyrics Focus view's list at its top: the thumb lies in the
        // band.
        use crate::ui::core::widgets::animated::testing::{Frames, MS};
        use crate::ui::core::widgets::fading_scrollbar::{fading_scrollbar, scrollable};
        use iced::time::Instant;
        #[derive(Debug, Clone, PartialEq)]
        enum Window {
            Scrolled(f32),
            Drag,
            Maximize,
        }
        let view = || {
            let list = scrollable(column![Space::new().width(100).height(300)])
                .width(100)
                .height(100)
                .on_scroll(|viewport| Window::Scrolled(viewport.absolute_offset().y));
            drag_region(
                window_backdrop(fading_scrollbar(list)),
                Window::Drag,
                Window::Maximize,
            )
        };
        let mut window = Frames::new(Size::new(120.0, 120.0));
        let t0 = Instant::now();
        // The pointer on the thumb shows the bar.
        window.point_at(Point::new(94.0, 10.0));
        window.frame(view(), &[], t0);
        window.frame(view(), &[], t0 + 300 * MS);
        let mut sent = window.frame_messages(view(), &[press()], t0 + 316 * MS).1;
        window.point_at(Point::new(94.0, 36.0));
        sent.extend(window.frame_messages(view(), &[], t0 + 332 * MS).1);
        sent.extend(window.frame_messages(view(), &[release()], t0 + 348 * MS).1);
        assert!(sent.contains(&Window::Scrolled(100.0)), "{sent:?}");
        assert!(
            !sent
                .iter()
                .any(|m| matches!(m, Window::Drag | Window::Maximize)),
            "{sent:?}"
        );
    }
}
