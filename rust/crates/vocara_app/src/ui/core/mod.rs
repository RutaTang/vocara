//! The shared UI layer: theme, icons and widgets every screen uses (port of
//! `client/lib/ui/core`).

pub mod icons;
pub mod motion;
pub mod scale_factor;
pub mod text;
pub mod theme;
pub mod widgets;

/// A text widget as the Dart app's `Text` laid it out: every screen and
/// dialog sat under a `Material`, whose default text style was forui's
/// `bodyMedium`, and `toApproximateMaterialTheme()` gives every text theme
/// slot `height: 1`. So a line is exactly the font size tall unless the
/// Dart style set its own `height` (which the port sets with
/// `.line_height`); iced's own default is 1.3.
pub fn text<'a, Theme, Renderer>(
    content: impl text::IntoFragment<'a>,
) -> text::Text<'a, Theme, Renderer>
where
    Theme: text::Catalog + 'a,
    Renderer: iced::advanced::text::Renderer,
{
    iced::widget::text(content).line_height(1.0)
}
