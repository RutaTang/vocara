//! The section label used across panels and stat blocks (`DESIGN.md`:
//! "Section heading inside a panel", sentence case). The Flutter app's
//! `CapLabel` upper-cased its text and letter-spaced it; the design system
//! has no uppercase labels, so the label shows its text as given, in the
//! 11 px semibold `TEXT_SECONDARY` section style.

use crate::ui::core::theme::{font, type_scale, Studio};
use crate::ui::core::widgets::studio_text::{studio_text, StudioText};

/// A section label: `label` as given (write it in sentence case: "Pitch
/// range"), 11 px semibold, `Studio::TEXT_SECONDARY`.
pub fn cap_label<'a>(label: &str) -> StudioText<'a> {
    studio_text(label.to_string())
        .size(type_scale::LABEL)
        .font(font::SEMIBOLD)
        .color(Studio::TEXT_SECONDARY)
}

/// [`cap_label`] under the design system's name.
pub fn section_label<'a>(label: &str) -> StudioText<'a> {
    cap_label(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::core::widgets::studio_text::testing::{laid, load_fonts, spec};

    #[test]
    fn shows_the_label_as_written() {
        let mut ui = iced_test::simulator::<(), _, _>(cap_label("Pitch range"));
        assert!(ui.find("Pitch range").is_ok());
        assert!(ui.find("PITCH RANGE").is_err());
    }

    #[test]
    fn the_label_is_as_wide_as_its_text_with_no_spacing() {
        load_fonts();
        let mut settings = crate::app::settings();
        settings.fonts = font::FACES
            .iter()
            .copied()
            .map(std::borrow::Cow::Borrowed)
            .collect();
        let mut ui: iced_test::Simulator<'_, ()> =
            iced_test::Simulator::with_settings(settings, section_label("Pitch range"));
        let label = ui.find("Pitch range").expect("the label");
        let solid = laid(&spec("Pitch range", 11.0, font::SEMIBOLD), f32::INFINITY);
        assert!(
            (label.bounds().width - solid.min_bounds.width).abs() < 0.01,
            "{} wide, not {}",
            label.bounds().width,
            solid.min_bounds.width
        );
    }
}
