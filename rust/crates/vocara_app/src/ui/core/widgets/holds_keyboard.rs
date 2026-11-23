//! Port of `ui/core/widgets/holds_keyboard.dart`: takes the keyboard while it
//! is shown, for an overlay that covers controls still in the tree beneath
//! it, and hands it back to whatever had it when the overlay goes.
//!
//! Beneath the lyrics focus view the panel kept it: Space stamped lines no
//! one could see (wrapping round to overwrite good timings), and typing went
//! into the hidden editor, which its Save then kept.
//!
//! In iced every widget sees every keyboard event unless one captures it,
//! and a `stack` stops handing an event down its layers once it is captured.
//! So [`holds_keyboard`] lets its content handle each keyboard event, then
//! captures it: a text field or shortcut beneath the overlay never sees the
//! key, and keeps its own focus state for when the overlay goes (the hand
//! back). Mount the overlay as a later layer of the `stack` it covers, as the
//! Dart overlay lay over the panel. Screens' keyboard subscriptions must skip
//! captured events (`event::Status::Captured`), which is how a shortcut
//! "beneath" the overlay stays quiet.

use iced::{Element, Event};

use super::wrap::Wrap;

/// `HoldsKeyboard(child: child)`.
pub fn holds_keyboard<'a, M: 'a>(child: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Wrap::<M, ()>::new(
        child,
        Box::new(|_, event, _, _, shell| {
            if matches!(event, Event::Keyboard(_)) {
                shell.capture_event();
            }
        }),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::widgets::wrap::Wrap;
    use iced::keyboard;
    use iced::widget::{stack, text};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Beneath,
        Overlay,
    }

    /// A layer that reports every key press it sees.
    fn key_spy<'a>(label: &'a str, msg: Msg) -> Element<'a, Msg> {
        Wrap::<Msg, ()>::new(
            text(label),
            Box::new(move |_, event, _, _, shell| {
                if matches!(event, Event::Keyboard(keyboard::Event::KeyPressed { .. })) {
                    shell.publish(msg.clone());
                }
            }),
        )
        .into()
    }

    #[test]
    fn the_layer_beneath_gets_no_keys_while_the_overlay_holds_them() {
        let ui = stack![
            key_spy("panel", Msg::Beneath),
            holds_keyboard(key_spy("focus view", Msg::Overlay)),
        ];
        let mut ui = iced_test::simulator(ui);
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::Space));
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Overlay]);
    }

    #[test]
    fn without_it_both_layers_get_the_key() {
        let ui = stack![
            key_spy("panel", Msg::Beneath),
            key_spy("focus view", Msg::Overlay)
        ];
        let mut ui = iced_test::simulator(ui);
        let _ = ui.tap_key(keyboard::Key::Named(keyboard::key::Named::Space));
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Msg::Overlay, Msg::Beneath]
        );
    }
}
