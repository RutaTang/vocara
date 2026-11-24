//! Port of `ui/core/widgets/studio_button.dart`: a restrained, DAW-style
//! button — flat, dark, lightly bordered, with a coral accent — instead of a
//! loud saturated pill.
//!
//! Same geometry (34 px tall, 14 px side padding, radius 7, 1 px border, a
//! 15 px icon 7 px before a 13 px w600 label), colors per variant and hover,
//! and a click cursor only when enabled. The fill and border move to a new
//! look over 120 ms, linear, as the Dart `AnimatedContainer` did — on hover
//! and on an enable or disable alike; the label and icon change at once.

use std::time::Duration;

use iced::alignment::Vertical;
use iced::widget::{button, container, row, text};
use iced::{Color, Element, Length};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, faded, font, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};

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

/// How long its fill and border take to change (`AnimatedContainer`).
pub const ANIMATION: Duration = Duration::from_millis(120);

/// `StudioButton(label: ..., onTap: ...)`; configure with the builder
/// methods, then convert into an [`Element`].
pub struct StudioButton<M> {
    label: String,
    on_tap: M,
    icon: Option<Icon>,
    variant: StudioButtonVariant,
    expand: bool,
    enabled: bool,
    /// Every color at this opacity (a faded dialog's button).
    opacity: f32,
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
        opacity: 1.0,
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

    /// Draws every color at `opacity` (inside a fading dialog: iced has no
    /// subtree opacity).
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
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
        let (fg, icon_color) = (faded(fg, b.opacity), faded(icon_color, b.opacity));
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
        let tap = button(inner)
            .height(HEIGHT)
            .width(width)
            .padding([0, 14])
            .on_press_maybe(enabled.then_some(b.on_tap))
            .style(bare_button);
        // The decoration animates behind the button (the pointer over it is
        // the `MouseRegion`'s hover).
        animated_container(tap, ANIMATION)
            .style(move |hovered| {
                let (bg, border, _, _) = colors(variant, enabled, hovered);
                Decoration::boxed(bg, 7.0, border, 1.0)
            })
            .opacity(b.opacity)
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
    fn its_fill_moves_to_the_hover_look_and_back_over_120_ms() {
        use crate::ui::core::motion::Lerp;
        use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};
        use iced::time::Instant;
        use iced::{Point, Size};

        let view = |enabled| {
            Element::from(
                studio_button("Go", Msg::Tapped)
                    .variant(StudioButtonVariant::Accent)
                    .enabled(enabled),
            )
        };
        let mut window = Frames::new(Size::new(120.0, 40.0));
        // Inside the border, left of the label.
        let fill = Point::new(4.0, 17.0);
        let shows = |w: &Frames, c: Color| near(w.pixel(fill), over(c, BACKGROUND));
        let (rest, ..) = colors(StudioButtonVariant::Accent, true, false);
        let (hover, ..) = colors(StudioButtonVariant::Accent, true, true);
        let t0 = Instant::now();
        window.frame(view(true), &[], t0);
        assert!(shows(&window, rest));
        window.point_at(Point::new(20.0, 17.0));
        assert!(window.frame(view(true), &[], t0));
        assert!(shows(&window, rest), "start");
        window.frame(view(true), &[], t0 + 60 * MS);
        assert!(shows(&window, rest.lerp(hover, 0.5)), "middle");
        assert!(!window.frame(view(true), &[], t0 + 120 * MS));
        assert!(shows(&window, hover), "end");
        // Disabled under the pointer: its look moves there too.
        let (disabled, ..) = colors(StudioButtonVariant::Accent, false, true);
        let t1 = t0 + 500 * MS;
        assert!(window.frame(view(false), &[], t1));
        window.frame(view(false), &[], t1 + 60 * MS);
        assert!(shows(&window, hover.lerp(disabled, 0.5)));
        window.frame(view(false), &[], t1 + 120 * MS);
        assert!(shows(&window, disabled));
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
