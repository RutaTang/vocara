//! A layer that is seen and not touched: it lays out, draws and answers
//! operations as its content does, but takes no pointer input, so a press
//! on it goes to what lies beneath. A `stack` hands an event on to the
//! layers below while none captures it, and keeps the cursor for them while
//! the layer above reports no interaction: this one captures nothing and
//! reports none.
//!
//! For a passing notice over controls: the song page's toasts covered the
//! bottom of the page for seconds at a time, and the presses meant for the
//! controls beneath them (the lyrics' Save, the dock's tabs) went to the
//! toast.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::{mouse, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};

/// `content`, drawn where it is, that every press passes through.
pub fn click_through<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Element::new(ClickThrough {
        content: content.into(),
    })
}

struct ClickThrough<'a, M> {
    content: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for ClickThrough<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // Never hovered: the pointer is the controls' beneath.
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            mouse::Cursor::Unavailable,
            viewport,
        );
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
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // The pointer's events are the layers' beneath; the rest (its
        // frames, a redraw) still reach the content.
        if matches!(event, Event::Mouse(_) | Event::Touch(_)) {
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            mouse::Cursor::Unavailable,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::text;
    use iced::widget::{button, container, stack};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Beneath,
        Over,
    }

    /// A press on the layer reaches the control beneath it, which the layer
    /// covers, and not the layer's own; its text is still found.
    #[test]
    fn a_press_on_it_goes_to_what_lies_beneath() {
        let view = || -> Element<'static, Msg> {
            stack![
                button(text("Save"))
                    .on_press(Msg::Beneath)
                    .width(Length::Fill)
                    .height(Length::Fill),
                click_through(
                    container(button(text("Wait for the edit")).on_press(Msg::Over))
                        .center(Length::Fill)
                ),
            ]
            .into()
        };
        let mut ui = iced_test::simulator(view());
        assert!(ui.find("Wait for the edit").is_ok(), "still shown");
        let _ = ui.click("Wait for the edit");
        let sent: Vec<Msg> = ui.into_messages().collect();
        assert_eq!(sent, [Msg::Beneath]);
    }
}
