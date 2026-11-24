//! Port of `ui/core/widgets/cap_label.dart`: a tiny uppercase section label
//! used across panels and stat blocks.

use crate::ui::core::theme::{font, Studio};
use crate::ui::core::widgets::studio_text::{studio_text, StudioText};

/// `CapLabel(text)`: `text` upper-cased, 9 px, w700, 0.5 letter spacing,
/// `Studio.textFaint`.
pub fn cap_label<'a>(label: &str) -> StudioText<'a> {
    studio_text(label.to_uppercase())
        .size(9)
        .font(font::BOLD)
        .letter_spacing(0.5)
        .color(Studio::TEXT_FAINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::widgets::studio_text::testing::{laid, load_fonts, spec};

    #[test]
    fn upper_cases_the_label() {
        let mut ui = iced_test::simulator::<(), _, _>(cap_label("Pitch range"));
        assert!(ui.find("PITCH RANGE").is_ok());
        assert!(ui.find("Pitch range").is_err());
    }

    #[test]
    fn the_label_is_as_wide_as_its_text_and_spacing() {
        load_fonts();
        let mut settings = crate::app::settings();
        settings.fonts = font::FACES
            .iter()
            .copied()
            .map(std::borrow::Cow::Borrowed)
            .collect();
        let mut ui: iced_test::Simulator<'_, ()> =
            iced_test::Simulator::with_settings(settings, cap_label("Pitch range"));
        let label = ui.find("PITCH RANGE").expect("the label");
        let solid = laid(&spec("PITCH RANGE", 9.0, font::BOLD), f32::INFINITY);
        // Half a pixel after each of its 11 glyphs, the last one included.
        let spaced = solid.min_bounds.width + 11.0 * 0.5;
        assert!(
            (label.bounds().width - spaced).abs() < 0.01,
            "{} wide, not {spaced}",
            label.bounds().width
        );
    }
}
