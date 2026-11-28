//! Port of `ui/core/widgets/visible_drop_target.dart`: a file-drop target
//! that takes a drop only where a click would reach it.
//!
//! desktop_drop decided by the target's rectangle alone, never by what was
//! drawn over it, so a drop landed on whatever lay beneath the pointer: a
//! lane under a dialog's backdrop or the lyrics focus overlay, or the part of
//! a lane scrolled out of its list and hidden behind the dock or the
//! transport bar — imported, silently, into a take the user could not see.
//!
//! In iced the same question is the cursor this widget is handed: a layer
//! above it in a `stack` that takes the pointer (an `opaque` backdrop, a
//! dialog) or a `scrollable` it is scrolled out of hands it a cursor that is
//! not "available" over its bounds. So a drop is taken only when
//! `cursor.position_over(bounds)` is, exactly as a click would be.
//!
//! iced reports a drop as one `FileDropped` per file and gives no position
//! with it, so the target judges the drop by the pointer position iced last
//! saw, gathers the files of one drop (consecutive `FileDropped` events) and
//! hands them over together, as desktop_drop's `onDragDone` did. Platform
//! note: winit reports no pointer movement during an external drag on macOS,
//! so the event loop reads the pointer from AppKit and moves the cursor
//! there as a file hovers or drops (the vendored iced_winit's
//! `drop_pointer`); a move is no other event that ends a drop. With no
//! position known at all the drop is refused, never guessed.

use std::path::PathBuf;

use iced::{mouse, window, Element, Event, Rectangle};

use super::wrap::Wrap;

/// `VisibleDropTarget(onDrop: ..., child: ...)`; see [`visible_drop_target`].
pub struct VisibleDropTarget<'a, M> {
    content: Element<'a, M>,
    on_drop: Box<dyn Fn(Vec<PathBuf>) -> M + 'a>,
    on_hover: Option<Box<dyn Fn(bool) -> M + 'a>>,
    enable: bool,
}

/// A drop target around `content` handing a visible drop's paths to
/// `on_drop`.
pub fn visible_drop_target<'a, M>(
    content: impl Into<Element<'a, M>>,
    on_drop: impl Fn(Vec<PathBuf>) -> M + 'a,
) -> VisibleDropTarget<'a, M> {
    VisibleDropTarget {
        content: content.into(),
        on_drop: Box::new(on_drop),
        on_hover: None,
        enable: true,
    }
}

impl<'a, M> VisibleDropTarget<'a, M> {
    /// Whether a drag over it is one it would take, as that changes.
    pub fn on_hover(mut self, on_hover: impl Fn(bool) -> M + 'a) -> Self {
        self.on_hover = Some(Box::new(on_hover));
        self
    }

    /// A disabled target takes no drops and reports no hover.
    pub fn enable(mut self, enable: bool) -> Self {
        self.enable = enable;
        self
    }
}

/// The target's drag state.
#[derive(Debug, Default)]
pub struct DropState {
    hover: bool,
    /// A drop's files are arriving (one `FileDropped` each).
    dropping: bool,
    /// Whether the drop arriving is one this target takes.
    reached: bool,
    files: Vec<PathBuf>,
}

fn reaches(bounds: Rectangle, cursor: mouse::Cursor) -> bool {
    cursor.position_over(bounds).is_some()
}

/// Whether `event` ends a drop whose files are arriving: any event but
/// another file of it, or the pointer moving. On macOS the event loop moves
/// the cursor to the pointer as a drop comes in (winit reports no pointer
/// during a drag in from another app), and a move between two files of one
/// drop split it into one drop per file: the song page took the first and
/// refused the rest while it was busy with it.
fn ends_drop(event: &Event) -> bool {
    !matches!(
        event,
        Event::Window(window::Event::FileDropped(_))
            | Event::Mouse(mouse::Event::CursorMoved { .. })
    )
}

impl<'a, M: 'a> From<VisibleDropTarget<'a, M>> for Element<'a, M> {
    fn from(target: VisibleDropTarget<'a, M>) -> Self {
        let VisibleDropTarget {
            content,
            on_drop,
            on_hover,
            enable,
        } = target;
        Wrap::<M, DropState>::new(
            content,
            Box::new(move |state, event, bounds, cursor, shell| {
                let set_hover =
                    |state: &mut DropState,
                     hover: bool,
                     shell: &mut iced::advanced::Shell<'_, M>| {
                        if hover == state.hover {
                            return;
                        }
                        state.hover = hover;
                        if let Some(f) = &on_hover {
                            shell.publish(f(hover));
                        }
                    };
                // The files of one drop arrive back to back; the first other
                // event ends the drop.
                if state.dropping && ends_drop(event) {
                    state.dropping = false;
                    let files = std::mem::take(&mut state.files);
                    if state.reached && !files.is_empty() {
                        shell.publish(on_drop(files));
                    }
                }
                if !enable {
                    return;
                }
                match event {
                    Event::Window(window::Event::FileHovered(_)) => {
                        set_hover(state, reaches(bounds, cursor), shell);
                    }
                    Event::Window(window::Event::FilesHoveredLeft) => {
                        set_hover(state, false, shell);
                    }
                    Event::Window(window::Event::FileDropped(path)) => {
                        if !state.dropping {
                            state.dropping = true;
                            state.reached = reaches(bounds, cursor);
                            set_hover(state, false, shell);
                            // Ask for the frame whose event ends the drop.
                            shell.request_redraw();
                        }
                        if state.reached {
                            state.files.push(path.clone());
                        }
                    }
                    _ => {}
                }
            }),
        )
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{column, container, opaque, scrollable, stack, Space};
    use iced::{time::Instant, Length, Point};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Drop(Vec<PathBuf>),
        Hover(bool),
    }

    fn target<'a>() -> Element<'a, Msg> {
        visible_drop_target(Space::new().width(300).height(200), Msg::Drop)
            .on_hover(Msg::Hover)
            .into()
    }

    /// A drop of `paths` at `at`, the way the platform reports one.
    fn drop_at(ui: &mut iced_test::Simulator<'_, Msg>, at: Point, paths: &[&str]) {
        ui.point_at(at);
        let mut events = Vec::new();
        for p in paths {
            events.push(Event::Window(window::Event::FileHovered(PathBuf::from(p))));
        }
        for p in paths {
            events.push(Event::Window(window::Event::FileDropped(PathBuf::from(p))));
        }
        events.push(Event::Window(
            window::Event::RedrawRequested(Instant::now()),
        ));
        let _ = ui.simulate(events);
    }

    #[test]
    fn takes_a_drop_where_it_can_be_seen() {
        let mut ui = iced_test::simulator(container(target()));
        drop_at(&mut ui, Point::new(100.0, 100.0), &["/a.wav", "/b.wav"]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![
                Msg::Hover(true),
                Msg::Hover(false),
                Msg::Drop(vec!["/a.wav".into(), "/b.wav".into()])
            ]
        );
    }

    #[test]
    fn a_drop_of_several_files_with_a_move_before_each_is_one_drop() {
        // The event loop moves the cursor to the pointer before a dropped
        // file: each move ended the drop, and three files made three drops.
        let mut ui = iced_test::simulator(container(target()));
        let at = Point::new(100.0, 100.0);
        ui.point_at(at);
        let moved = || Event::Mouse(mouse::Event::CursorMoved { position: at });
        let dropped = |p: &str| Event::Window(window::Event::FileDropped(PathBuf::from(p)));
        let _ = ui.simulate([
            moved(),
            dropped("/a.wav"),
            moved(),
            dropped("/b.wav"),
            Event::Window(window::Event::RedrawRequested(Instant::now())),
        ]);
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Drop(vec!["/a.wav".into(), "/b.wav".into()])]
        );
    }

    #[test]
    fn takes_no_drop_through_something_drawn_over_it() {
        // A dialog's backdrop, or a full-window overlay.
        let backdrop = opaque(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(crate::ui::core::theme::fill(iced::Color::from_rgba8(
                    0, 0, 0, 0.5,
                ))),
        );
        let mut ui = iced_test::simulator(stack![container(target()), backdrop]);
        drop_at(&mut ui, Point::new(100.0, 100.0), &["/a.wav"]);
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn takes_no_drop_where_it_is_scrolled_out_of_its_list() {
        // The list shows its first 100 px; the target lies past them, laid
        // out where the panel below it is drawn.
        let list = scrollable(column![Space::new().height(120), target()]).height(100);
        let panel = container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill);
        let mut ui = iced_test::simulator(column![list, panel]);
        drop_at(&mut ui, Point::new(100.0, 150.0), &["/a.wav"]);

        // Scroll the target into view, then drop on it there.
        ui.point_at(Point::new(100.0, 50.0));
        let _ = ui.simulate([Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -120.0 },
        })]);
        drop_at(&mut ui, Point::new(100.0, 50.0), &["/b.wav"]);
        let drops: Vec<_> = ui
            .into_messages()
            .filter(|m| matches!(m, Msg::Drop(_)))
            .collect();
        assert_eq!(
            drops,
            vec![Msg::Drop(vec!["/b.wav".into()])],
            "the panel is what is there first"
        );
    }

    #[test]
    fn a_disabled_target_takes_nothing() {
        let t: Element<'_, Msg> =
            visible_drop_target(Space::new().width(300).height(200), Msg::Drop)
                .on_hover(Msg::Hover)
                .enable(false)
                .into();
        let mut ui = iced_test::simulator(container(t));
        drop_at(&mut ui, Point::new(100.0, 100.0), &["/a.wav"]);
        assert!(ui.into_messages().next().is_none());
    }
}
