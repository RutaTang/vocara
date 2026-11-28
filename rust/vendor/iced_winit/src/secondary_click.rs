//! vocara: a left press with Control held is a secondary click on macOS,
//! as AppKit takes it (`menuForEvent:` answers a control-click as it does
//! a right click). Not part of upstream iced_winit (see
//! `rust/vendor/README.md`).
//!
//! winit reports a control-click as the left button with Control held, so
//! everything that opens a context menu on `Button::Right` (a clip's, a
//! take's, a mixer strip's, a text field's) took it as a left click
//! instead: it selected, started a drag or placed the caret, and the clip's
//! menu (Export, Delete clip) was out of reach without a right button. The
//! event loop reports such a press as the right button, and its release
//! too, whether Control is still held by then or not ([`ControlClick`]).

use winit::event::{ElementState, MouseButton};

/// Whether a control-click is a secondary click: the platform's
/// convention on macOS only (elsewhere Control and a click select).
pub const ON: bool = cfg!(target_os = "macos");

/// A window's primary button as the event loop reports it: whether its
/// press went down as a secondary click.
#[derive(Debug, Default)]
pub struct ControlClick {
    secondary: bool,
}

impl ControlClick {
    /// The button a press or release of `button` is reported as, with
    /// Control held (`control`) or not.
    pub fn button(
        &mut self,
        button: MouseButton,
        state: ElementState,
        control: bool,
    ) -> MouseButton {
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                self.secondary = control;
                if control {
                    MouseButton::Right
                } else {
                    MouseButton::Left
                }
            }
            (MouseButton::Left, ElementState::Released) if self.secondary => {
                self.secondary = false;
                MouseButton::Right
            }
            _ => button,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlClick, ElementState, MouseButton};

    use ElementState::{Pressed, Released};
    use MouseButton::{Left, Middle, Right};

    #[test]
    fn a_control_click_is_a_right_click_its_release_too() {
        let mut click = ControlClick::default();
        assert_eq!(click.button(Left, Pressed, true), Right);
        // Control let go before the button: still the right one's release.
        assert_eq!(click.button(Left, Released, false), Right);
        // A click after it is the left one again.
        assert_eq!(click.button(Left, Pressed, false), Left);
        assert_eq!(click.button(Left, Released, true), Left);
    }

    #[test]
    fn the_other_buttons_are_what_they_are() {
        let mut click = ControlClick::default();
        assert_eq!(click.button(Right, Pressed, true), Right);
        assert_eq!(click.button(Right, Released, true), Right);
        assert_eq!(click.button(Middle, Pressed, true), Middle);
        // A left release whose press it did not see is the left one's.
        assert_eq!(click.button(Left, Released, false), Left);
    }
}
