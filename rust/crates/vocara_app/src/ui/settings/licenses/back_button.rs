//! The license page's `BackButton` as the Material 3 app bar built it on
//! macOS under forui's approximate Material theme.
//!
//! - The icon is `Icons.arrow_back_ios_new_rounded` (`BackButtonIcon` on
//!   iOS and macOS), 24 px, in the app bar's foreground
//!   (`Studio.textPrimary`, the theme Settings gave the page). It is drawn
//!   from the glyph's outline: the rounded variant of `arrow_back_ios_new`
//!   from Google's Material Icons (google/material-design-icons, Apache
//!   License 2.0, listed on the license page), the source of the glyph in
//!   Flutter's `MaterialIcons` font, which matches it to within its
//!   1/512-em grid.
//! - The button is `_IconButtonDefaultsM3`'s 40 × 40 (padding 8 around the
//!   icon; its density is `VisualDensity.standard`, not the desktop's
//!   compact one, and the desktop shrink-wraps its tap target), centred in
//!   the app bar's 56 px leading slot.
//! - The app bar's foreground is not the scheme's `onSurface`, so its
//!   leading button takes forui's `iconButtonTheme` with that foreground
//!   and `IconButton.styleFrom(foregroundColor:)`'s overlay: under the
//!   pointer or pressed, forui's ghost background (`secondary`) at once, in
//!   a rounded rectangle of forui's radius 8; over it the `InkWell`'s
//!   overlay, the foreground at 8 % hovered (fading over 50 ms) and 10 %
//!   pressed (over 200 ms) in the same shape.
//! - It has the localized "Back" tooltip, 24 px below its centre.

use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{button, canvas as canvas_widget, container};
use iced::{mouse, Background, Border, Color, Element, Point, Rectangle, Renderer, Shadow, Theme};

use crate::ui::core::theme::{alpha, Studio, Zinc};
use crate::ui::core::widgets::animated::ink_well;
use crate::ui::core::widgets::material_tooltip::material_tooltip;

/// The button's side (`minimumSize`, and padding 8 around a 24 px icon).
pub const SIZE: f32 = 40.0;

/// The icon's side (`iconSize`).
pub const ICON: f32 = 24.0;

/// forui's `style.borderRadius` (8; its `FLerpBorderRadius` only shrinks
/// it on a box under 24 px).
pub const RADIUS: f32 = 8.0;

/// The tooltip's text (`MaterialLocalizations.backButtonTooltip`).
pub const TOOLTIP: &str = "Back";

/// The hovered overlay: the foreground at 8 %.
pub const HOVERED: Color = alpha(Studio::TEXT_PRIMARY, 0.08);

/// The pressed overlay: the foreground at 10 %.
pub const PRESSED: Color = alpha(Studio::TEXT_PRIMARY, 0.10);

/// The back button, reporting `on_press`, its tooltip around it.
pub fn back_button<'a, M: Clone + 'a>(on_press: M) -> Element<'a, M> {
    let icon = container(
        canvas_widget(BackArrow {
            color: Studio::TEXT_PRIMARY,
        })
        .width(ICON)
        .height(ICON),
    )
    .center_x(SIZE)
    .center_y(SIZE);
    let target = button(ink_well(icon, HOVERED, PRESSED, RADIUS))
        .padding(0)
        .style(|_: &Theme, status| {
            let lit = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: lit.then_some(Background::Color(Zinc::SECONDARY)),
                text_color: Studio::TEXT_PRIMARY,
                border: Border {
                    radius: RADIUS.into(),
                    ..Border::default()
                },
                shadow: Shadow::default(),
                snap: false,
            }
        })
        .on_press(on_press);
    // Material's tooltip sits 24 px below the target's centre.
    material_tooltip(target, TOOLTIP, 24.0 - SIZE / 2.0).into()
}

/// `arrow_back_ios_new`, rounded, on a 24-unit grid (the SVG's path, made
/// absolute): `M16.88,2.88 c-.49-.49-1.28-.49-1.77,0 l-8.41,8.41
/// c-.39.39-.39,1.02,0,1.41 l8.41,8.41 c.49.49,1.28.49,1.77,0
/// c.49-.49.49-1.28,0-1.77 L9.54,12 l7.35-7.35 C17.37,4.16,17.37,3.37,16.88,2.88z`.
pub fn back_arrow(scale: f32) -> Path {
    let p = |x: f32, y: f32| Point::new(x * scale, y * scale);
    Path::new(|b| {
        b.move_to(p(16.88, 2.88));
        b.bezier_curve_to(p(16.39, 2.39), p(15.60, 2.39), p(15.11, 2.88));
        b.line_to(p(6.70, 11.29));
        b.bezier_curve_to(p(6.31, 11.68), p(6.31, 12.31), p(6.70, 12.70));
        b.line_to(p(15.11, 21.11));
        b.bezier_curve_to(p(15.60, 21.60), p(16.39, 21.60), p(16.88, 21.11));
        b.bezier_curve_to(p(17.37, 20.62), p(17.37, 19.83), p(16.88, 19.34));
        b.line_to(p(9.54, 12.0));
        b.line_to(p(16.89, 4.65));
        b.bezier_curve_to(p(17.37, 4.16), p(17.37, 3.37), p(16.88, 2.88));
        b.close();
    })
}

/// The icon, filled in `color`.
struct BackArrow {
    color: Color,
}

impl<M> canvas::Program<M> for BackArrow {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill(&back_arrow(bounds.width / ICON), self.color);
        vec![frame.into_geometry()]
    }
}
