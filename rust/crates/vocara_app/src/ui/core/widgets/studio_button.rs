//! The app's text button (port of `ui/core/widgets/studio_button.dart`),
//! drawn as `DESIGN.md`'s desktop buttons: the secondary button
//! (transparent with an inset outline), the primary action (a filled
//! `ACCENT` button with dark text) and a destructive one (an outline with
//! red text, quieter than a filled red button in a toolbar).
//!
//! 28 px tall (`theme::height::TOOLBAR_BUTTON`), 12 px side padding, radius
//! 6, a 14 px icon 6 px before a 12 px label (medium; semibold on the
//! primary), and a click cursor only when enabled. The fill and outline
//! move to a new look over 120 ms, linear, as the Dart `AnimatedContainer`
//! did; on hover and on an enable or disable alike; the label and icon
//! change at once. A dialog's buttons are `controls::primary_button` and
//! `controls::secondary_button`, which share these looks.

use std::time::Duration;

use crate::ui::core::text;
use iced::alignment::Vertical;
use iced::widget::{button, container, row};
use iced::{Color, Element, Length};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, font, Studio};
use crate::ui::core::widgets::animated::{animated_container, Decoration};

/// The look of a [`StudioButton`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StudioButtonVariant {
    /// The secondary button: transparent, an inset `BORDER_STRONG` outline,
    /// `TEXT_PRIMARY` label, `TEXT_SECONDARY` icon; a `HOVER` fill under
    /// the pointer.
    #[default]
    Subtle,
    /// The primary action: filled `ACCENT` (`ACCENT_BRIGHT` under the
    /// pointer), `ON_ACCENT` label and icon.
    Accent,
    /// Destructive: the secondary button's outline with a `RECORD_TEXT`
    /// label and icon, a faint red fill under the pointer.
    Danger,
}

/// The button's height.
pub const HEIGHT: f32 = crate::ui::core::theme::height::TOOLBAR_BUTTON;

/// Its corner radius.
pub const RADIUS: f32 = crate::ui::core::theme::radius::WELL;

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
        return match variant {
            StudioButtonVariant::Accent => (
                Studio::CONTROL,
                Color::TRANSPARENT,
                Studio::TEXT_DISABLED,
                Studio::TEXT_DISABLED,
            ),
            _ => (
                Color::TRANSPARENT,
                Studio::BORDER_STRONG,
                Studio::TEXT_DISABLED,
                Studio::TEXT_DISABLED,
            ),
        };
    }
    match variant {
        StudioButtonVariant::Accent => (
            if hover {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::ACCENT
            },
            Color::TRANSPARENT,
            Studio::ON_ACCENT,
            Studio::ON_ACCENT,
        ),
        StudioButtonVariant::Danger => (
            if hover {
                alpha(Studio::RECORD, 0.12)
            } else {
                Color::TRANSPARENT
            },
            Studio::BORDER_STRONG,
            Studio::RECORD_TEXT,
            Studio::RECORD_TEXT,
        ),
        StudioButtonVariant::Subtle => (
            if hover {
                Studio::HOVER
            } else {
                Color::TRANSPARENT
            },
            Studio::BORDER_STRONG,
            Studio::TEXT_PRIMARY,
            Studio::TEXT_SECONDARY,
        ),
    }
}

impl<'a, M: Clone + 'a> From<StudioButton<M>> for Element<'a, M> {
    fn from(b: StudioButton<M>) -> Self {
        let (_, _, fg, icon_color) = colors(b.variant, b.enabled, false);
        let mut content = row![].spacing(6).align_y(Vertical::Center);
        if let Some(i) = b.icon {
            content = content.push(icons::icon(i, 14.0, icon_color));
        }
        let weight = if b.variant == StudioButtonVariant::Accent {
            font::SEMIBOLD
        } else {
            font::MEDIUM
        };
        content = content.push(
            text(b.label)
                .size(crate::ui::core::theme::type_scale::BODY)
                .font(weight)
                .color(fg),
        );
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
            .padding([0, 12])
            .on_press_maybe(enabled.then_some(b.on_tap))
            .style(bare_button);
        // The decoration animates behind the button (the pointer over it is
        // the `MouseRegion`'s hover).
        animated_container(tap, ANIMATION)
            .style(move |hovered| {
                let (bg, border, _, _) = colors(variant, enabled, hovered);
                Decoration::boxed(bg, RADIUS, border, 1.0)
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
    fn variants_paint_as_the_design_s_buttons() {
        // The primary action: filled coral, dark text; brighter hovered.
        let (bg, _, fg, icon) = colors(StudioButtonVariant::Accent, true, false);
        assert_eq!(
            (bg, fg, icon),
            (Studio::ACCENT, Studio::ON_ACCENT, Studio::ON_ACCENT)
        );
        let (bg, ..) = colors(StudioButtonVariant::Accent, true, true);
        assert_eq!(bg, Studio::ACCENT_BRIGHT);
        // The secondary button: transparent with an outline.
        let (bg, border, fg, icon) = colors(StudioButtonVariant::Subtle, true, false);
        assert_eq!(
            (bg, border, fg, icon),
            (
                Color::TRANSPARENT,
                Studio::BORDER_STRONG,
                Studio::TEXT_PRIMARY,
                Studio::TEXT_SECONDARY
            )
        );
        assert_eq!(
            colors(StudioButtonVariant::Subtle, true, true).0,
            Studio::HOVER
        );
        // Destructive: red text on the outline.
        let (_, border, fg, _) = colors(StudioButtonVariant::Danger, true, false);
        assert_eq!((border, fg), (Studio::BORDER_STRONG, Studio::RECORD_TEXT));
        // Hover never shows on a disabled button.
        assert_eq!(
            colors(StudioButtonVariant::Danger, false, true),
            (
                Color::TRANSPARENT,
                Studio::BORDER_STRONG,
                Studio::TEXT_DISABLED,
                Studio::TEXT_DISABLED
            )
        );
        assert_eq!(HEIGHT, 28.0);
    }
}
