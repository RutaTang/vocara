//! Port of `ui/core/widgets/studio_button.dart`: a restrained, DAW-style
//! button — flat, dark, lightly bordered, with a coral accent — instead of a
//! loud saturated pill.
//!
//! Same geometry (34 px tall, 14 px side padding, radius 7, 1 px border, a
//! 15 px icon 7 px before a 13 px w600 label), colors per variant and hover,
//! and a click cursor only when enabled. Flutter animated the color change
//! over 120 ms (`AnimatedContainer`); iced switches at once.

use iced::alignment::Vertical;
use iced::widget::{button, container, row, text};
use iced::{Background, Border, Color, Element, Length, Shadow, Theme};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, font, Studio};

/// The look of a [`StudioButton`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StudioButtonVariant {
    /// Neutral toolbar button: dark surface, coral icon, light text.
    #[default]
    Subtle,
    /// Tinted accent button: coral-tinted surface, coral border + label.
    Accent,
    /// Destructive: muted red-tinted surface, red border + label.
    Danger,
}

/// The button's height.
pub const HEIGHT: f32 = 34.0;

/// `StudioButton(label: ..., onTap: ...)`; configure with the builder
/// methods, then convert into an [`Element`].
pub struct StudioButton<M> {
    label: String,
    on_tap: M,
    icon: Option<Icon>,
    variant: StudioButtonVariant,
    expand: bool,
    enabled: bool,
}

/// A subtle, enabled, shrink-wrapped [`StudioButton`].
pub fn studio_button<M>(label: impl Into<String>, on_tap: M) -> StudioButton<M> {
    StudioButton {
        label: label.into(),
        on_tap,
        icon: None,
        variant: StudioButtonVariant::Subtle,
        expand: false,
        enabled: true,
    }
}

impl<M> StudioButton<M> {
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: StudioButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Fill the available width (label centred) instead of shrink-wrapping.
    pub fn expand(mut self, expand: bool) -> Self {
        self.expand = expand;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The colors a [`StudioButton`] paints with: `(background, border,
/// label, icon)`.
pub fn colors(
    variant: StudioButtonVariant,
    enabled: bool,
    hovered: bool,
) -> (Color, Color, Color, Color) {
    let hover = hovered && enabled;
    if !enabled {
        return (
            Studio::SURFACE_RAISED,
            Studio::BORDER,
            Studio::TEXT_FAINT,
            Studio::TEXT_FAINT,
        );
    }
    match variant {
        StudioButtonVariant::Accent => (
            alpha(Studio::ACCENT, if hover { 0.24 } else { 0.16 }),
            alpha(Studio::ACCENT, if hover { 0.7 } else { 0.45 }),
            Studio::ACCENT_BRIGHT,
            Studio::ACCENT_BRIGHT,
        ),
        StudioButtonVariant::Danger => (
            alpha(Studio::RECORD, if hover { 0.22 } else { 0.14 }),
            alpha(Studio::RECORD, if hover { 0.7 } else { 0.45 }),
            Studio::RECORD,
            Studio::RECORD,
        ),
        StudioButtonVariant::Subtle => (
            if hover {
                Studio::HOVER
            } else {
                Studio::SURFACE_RAISED
            },
            if hover {
                alpha(Studio::ACCENT, 0.55)
            } else {
                Studio::BORDER
            },
            Studio::TEXT_PRIMARY,
            Studio::ACCENT,
        ),
    }
}

impl<'a, M: Clone + 'a> From<StudioButton<M>> for Element<'a, M> {
    fn from(b: StudioButton<M>) -> Self {
        let (_, _, fg, icon_color) = colors(b.variant, b.enabled, false);
        let mut content = row![].spacing(7).align_y(Vertical::Center);
        if let Some(i) = b.icon {
            content = content.push(icons::icon(i, 15.0, icon_color));
        }
        content = content.push(text(b.label).size(13).font(font::SEMIBOLD).color(fg));
        let width = if b.expand {
            Length::Fill
        } else {
            Length::Shrink
        };
        let inner = container(content)
            .width(width)
            .height(Length::Fill)
            .center_x(width)
            .align_y(Vertical::Center);
        let variant = b.variant;
        let enabled = b.enabled;
        button(inner)
            .height(HEIGHT)
            .width(width)
            .padding([0, 14])
            .on_press_maybe(enabled.then_some(b.on_tap))
            .style(move |_: &Theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                let (bg, border, fg, _) = colors(variant, enabled, hovered);
                button::Style {
                    background: Some(Background::Color(bg)),
                    text_color: fg,
                    border: Border {
                        color: border,
                        width: 1.0,
                        radius: 7.0.into(),
                    },
                    shadow: Shadow::default(),
                    snap: false,
                }
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Tapped,
    }

    #[test]
    fn a_click_on_the_label_taps_the_button() {
        let mut ui = iced_test::simulator(Element::from(studio_button("Delete", Msg::Tapped)));
        ui.click("Delete").expect("label shown");
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Msg::Tapped]);
    }

    #[test]
    fn a_disabled_button_ignores_clicks() {
        let mut ui = iced_test::simulator(Element::from(
            studio_button("Save", Msg::Tapped).enabled(false),
        ));
        ui.click("Save").expect("label shown");
        assert!(ui.into_messages().next().is_none());
    }

    #[test]
    fn variants_paint_as_the_dart_button() {
        let (bg, border, fg, icon) = colors(StudioButtonVariant::Accent, true, true);
        assert!((bg.a - 0.24).abs() < 1e-6 && (border.a - 0.7).abs() < 1e-6);
        assert_eq!((fg, icon), (Studio::ACCENT_BRIGHT, Studio::ACCENT_BRIGHT));
        let (bg, _, fg, icon) = colors(StudioButtonVariant::Subtle, true, false);
        assert_eq!(
            (bg, fg, icon),
            (Studio::SURFACE_RAISED, Studio::TEXT_PRIMARY, Studio::ACCENT)
        );
        // Hover never shows on a disabled button.
        assert_eq!(
            colors(StudioButtonVariant::Danger, false, true),
            (
                Studio::SURFACE_RAISED,
                Studio::BORDER,
                Studio::TEXT_FAINT,
                Studio::TEXT_FAINT
            )
        );
    }
}
