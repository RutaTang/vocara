//! Port of `ui/core/widgets/cap_label.dart`: a tiny uppercase section label
//! used across panels and stat blocks.
//!
//! The Dart label set `letterSpacing: 0.5`; iced text has no letter spacing,
//! so the Rust label is set solid.

use iced::widget::{text, Text};

use crate::ui::core::theme::{font, Studio};

/// `CapLabel(text)`: `text` upper-cased, 9 px, w700, `Studio.textFaint`.
pub fn cap_label<'a>(label: &str) -> Text<'a> {
    text(label.to_uppercase())
        .size(9)
        .font(font::BOLD)
        .color(Studio::TEXT_FAINT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_cases_the_label() {
        let mut ui = iced_test::simulator::<(), _, _>(cap_label("Pitch range"));
        assert!(ui.find("PITCH RANGE").is_ok());
        assert!(ui.find("Pitch range").is_err());
    }
}
