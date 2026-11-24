//! The app's look: the [`Studio`] palette (port of
//! `ui/core/themes/studio.dart`), forui's zinc-dark palette the Material
//! layer was themed with ([`Zinc`], port of `ui/core/themes/theme.dart`'s
//! `appTheme = FThemes.zinc.dark`), the iced [`Theme`] built from them, the
//! app font (forui's Inter, which `toApproximateMaterialTheme` made the
//! whole app's font), and style functions that reproduce the looks the
//! screens build by hand on top of Material: surfaces, cards, the dialog
//! card and barrier, menus, tooltips, snack bars, the borderless studio text
//! fields and the desktop scrollbar.
//!
//! Flutter's `color.withValues(alpha: a)` *replaces* the alpha; [`alpha`] is
//! its counterpart (not iced's `scale_alpha`, which multiplies).

use crate::ui::core::text;
use iced::border::Radius;
use iced::font::{Family, Weight};
use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Font, Padding, Shadow, Theme, Vector};

/// Dark "studio" palette — charcoal panels with a warm coral accent, modeled
/// on a modern DAW. Tracks are neutral gray by default; the selected track and
/// primary actions use the coral accent.
pub struct Studio;

impl Studio {
    // Surfaces, darkest to lightest.
    pub const BACKGROUND: Color = Color::from_rgb8(0x13, 0x13, 0x16);
    /// Rails.
    pub const SURFACE: Color = Color::from_rgb8(0x1B, 0x1B, 0x21);
    /// Track rows.
    pub const PANEL: Color = Color::from_rgb8(0x1A, 0x1A, 0x20);
    pub const SURFACE_RAISED: Color = Color::from_rgb8(0x24, 0x24, 0x2C);
    /// LCD, note lane.
    pub const INSET: Color = Color::from_rgb8(0x0E, 0x0E, 0x12);
    pub const HOVER: Color = Color::from_rgb8(0x22, 0x22, 0x2A);

    // Lines.
    pub const GRID: Color = Color::from_rgb8(0x25, 0x25, 0x2D);
    pub const GRID_STRONG: Color = Color::from_rgb8(0x34, 0x34, 0x3E);
    pub const BORDER: Color = Color::from_rgb8(0x2A, 0x2A, 0x33);
    pub const BORDER_STRONG: Color = Color::from_rgb8(0x3A, 0x3A, 0x45);

    // Text.
    pub const TEXT_PRIMARY: Color = Color::from_rgb8(0xEA, 0xEA, 0xF0);
    pub const TEXT_SECONDARY: Color = Color::from_rgb8(0x95, 0x95, 0xA2);
    pub const TEXT_FAINT: Color = Color::from_rgb8(0x5E, 0x5E, 0x6B);

    // Accent (coral) + status.
    pub const ACCENT: Color = Color::from_rgb8(0xE0, 0x7A, 0x6A);
    pub const ACCENT_BRIGHT: Color = Color::from_rgb8(0xEC, 0x8C, 0x7E);
    pub const RECORD: Color = Color::from_rgb8(0xE3, 0x60, 0x4E);
    /// Warm white, reads on gray + coral.
    pub const PLAYHEAD: Color = Color::from_rgb8(0xF2, 0xEF, 0xE9);
    pub const LCD: Color = Color::from_rgb8(0xEF, 0xEF, 0xF3);

    // Waveform when the track is not selected.
    pub const WAVEFORM: Color = Color::from_rgb8(0x6A, 0x6A, 0x77);
    pub const TOGGLE_OFF: Color = Color::from_rgb8(0x3A, 0x3A, 0x45);

    // Semantic analysis colors shared by the dock views. Chest is warm, head is
    // cool; mix is derived by lerping the two at the usage site.
    pub const REGISTER_CHEST: Color = Color::from_rgb8(0xE0, 0x82, 0x5A);
    pub const REGISTER_HEAD: Color = Color::from_rgb8(0x5B, 0x8D, 0xEF);
    /// Pitch within tolerance / healthy.
    pub const IN_TUNE: Color = Color::from_rgb8(0x52, 0xB7, 0x88);
    /// Metric in the good range.
    pub const GOOD: Color = Color::from_rgb8(0x63, 0xC0, 0x89);
}

/// forui's `FThemes.zinc.dark` colors (forui 0.17), which themed the chrome
/// (home, dialogs, buttons) through `toApproximateMaterialTheme()`; the DAW
/// timeline uses the dedicated [`Studio`] palette.
pub struct Zinc;

impl Zinc {
    pub const BARRIER: Color = Color::from_rgba8(0, 0, 0, 0x7A as f32 / 255.0);
    pub const BACKGROUND: Color = Color::from_rgb8(0x09, 0x09, 0x0B);
    pub const FOREGROUND: Color = Color::from_rgb8(0xFA, 0xFA, 0xFA);
    pub const PRIMARY: Color = Color::from_rgb8(0xFA, 0xFA, 0xFA);
    pub const PRIMARY_FOREGROUND: Color = Color::from_rgb8(0x18, 0x18, 0x1B);
    pub const SECONDARY: Color = Color::from_rgb8(0x27, 0x27, 0x2A);
    pub const SECONDARY_FOREGROUND: Color = Color::from_rgb8(0xFA, 0xFA, 0xFA);
    pub const MUTED: Color = Color::from_rgb8(0x27, 0x27, 0x2A);
    pub const MUTED_FOREGROUND: Color = Color::from_rgb8(0xA1, 0xA1, 0xAA);
    pub const DESTRUCTIVE: Color = Color::from_rgb8(0x7F, 0x1D, 0x1D);
    pub const DESTRUCTIVE_FOREGROUND: Color = Color::from_rgb8(0xFA, 0xFA, 0xFA);
    pub const ERROR: Color = Color::from_rgb8(0x7F, 0x1D, 0x1D);
    pub const ERROR_FOREGROUND: Color = Color::from_rgb8(0xFA, 0xFA, 0xFA);
    pub const BORDER: Color = Color::from_rgb8(0x27, 0x27, 0x2A);
}

/// `color.withValues(alpha: a)`: the same color with its alpha REPLACED by `a`.
pub const fn alpha(color: Color, a: f32) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a,
    }
}

/// The app font: forui's Inter, which `toApproximateMaterialTheme` set as the
/// Material theme's font family, so every `Text` in the app (Studio styles
/// set no family of their own) rendered in it. Every text is shaped fully,
/// kerned as Flutter set it: the workspace enables iced's `advanced-shaping`
/// feature, which makes `Shaping::Advanced` the default of every text
/// (iced's own default shapes ASCII text glyph by glyph, without kerning).
pub mod font {
    use super::{Family, Font, Weight};

    /// The family name the bundled faces register under.
    pub const FAMILY: &str = "Inter";

    /// `FontWeight.w400`.
    pub const REGULAR: Font = inter(Weight::Normal);
    /// `FontWeight.w500`.
    pub const MEDIUM: Font = inter(Weight::Medium);
    /// `FontWeight.w600`.
    pub const SEMIBOLD: Font = inter(Weight::Semibold);
    /// `FontWeight.w700`.
    pub const BOLD: Font = inter(Weight::Bold);
    /// `FontWeight.w800`.
    pub const EXTRA_BOLD: Font = inter(Weight::ExtraBold);

    /// Inter at `weight`.
    pub const fn inter(weight: Weight) -> Font {
        Font {
            family: Family::Name(FAMILY),
            weight,
            stretch: iced::font::Stretch::Normal,
            style: iced::font::Style::Normal,
        }
    }

    /// The bundled faces (the weights the Dart UI uses: w400..w800), to load
    /// at startup (`iced::application(..).font(..)`, `Settings::fonts`).
    pub const FACES: [&[u8]; 5] = [
        include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-ExtraBold.ttf"),
    ];
}

/// The app's iced theme: forui zinc-dark's colors, with the scaffold
/// background the app set (`scaffoldBackgroundColor: Studio.background`).
pub fn app_theme() -> Theme {
    Theme::custom(
        "Vocara",
        iced::theme::Palette {
            background: Studio::BACKGROUND,
            text: Zinc::FOREGROUND,
            primary: Zinc::PRIMARY,
            success: Studio::GOOD,
            warning: Studio::REGISTER_CHEST,
            danger: Zinc::DESTRUCTIVE,
        },
    )
}

/// The app-wide default text size: Material's `bodyMedium`, which the forui
/// mapping makes `typography.sm` (14 px).
pub const DEFAULT_TEXT_SIZE: f32 = 14.0;

// ---------------------------------------------------------------- containers

/// A box with an optional fill, a uniform corner radius and an optional
/// 1 px border: the `BoxDecoration(color, borderRadius, Border.all(color))`
/// every studio surface is built from.
pub fn boxed(background: Option<Color>, radius: f32, border: Option<Color>) -> container::Style {
    container::Style {
        text_color: None,
        background: background.map(Background::Color),
        border: Border {
            color: border.unwrap_or(Color::TRANSPARENT),
            width: if border.is_some() { 1.0 } else { 0.0 },
            radius: Radius::from(radius),
        },
        shadow: Shadow::default(),
        snap: false,
    }
}

// ------------------------------------------------------------------ fading

/// `color` at `opacity` of its alpha: a color of a faded subtree (an
/// `Opacity` or `FadeTransition` around it), iced having no subtree
/// opacity.
pub const fn faded(color: Color, opacity: f32) -> Color {
    Color {
        a: color.a * opacity,
        ..color
    }
}

fn faded_background(background: Background, opacity: f32) -> Background {
    match background {
        Background::Color(c) => Background::Color(faded(c, opacity)),
        Background::Gradient(g) => Background::Gradient(g.scale_alpha(opacity)),
    }
}

/// A container's look at `opacity`: its fill, border, shadow and text.
pub fn faded_container(style: container::Style, opacity: f32) -> container::Style {
    container::Style {
        text_color: style.text_color.map(|c| faded(c, opacity)),
        background: style.background.map(|b| faded_background(b, opacity)),
        border: Border {
            color: faded(style.border.color, opacity),
            ..style.border
        },
        shadow: Shadow {
            color: faded(style.shadow.color, opacity),
            ..style.shadow
        },
        snap: style.snap,
    }
}

/// A button's look at `opacity`.
pub fn faded_button(style: button::Style, opacity: f32) -> button::Style {
    button::Style {
        background: style.background.map(|b| faded_background(b, opacity)),
        text_color: faded(style.text_color, opacity),
        border: Border {
            color: faded(style.border.color, opacity),
            ..style.border
        },
        shadow: Shadow {
            color: faded(style.shadow.color, opacity),
            ..style.shadow
        },
        snap: style.snap,
    }
}

/// A text field's look at `opacity`.
pub fn faded_input(style: text_input::Style, opacity: f32) -> text_input::Style {
    text_input::Style {
        background: faded_background(style.background, opacity),
        border: Border {
            color: faded(style.border.color, opacity),
            ..style.border
        },
        icon: faded(style.icon, opacity),
        placeholder: faded(style.placeholder, opacity),
        value: faded(style.value, opacity),
        selection: faded(style.selection, opacity),
    }
}

/// A scrollable's look at `opacity`: its container and both rails.
pub fn faded_scrollable(style: scrollable::Style, opacity: f32) -> scrollable::Style {
    let rail = |rail: scrollable::Rail| scrollable::Rail {
        background: rail.background.map(|b| faded_background(b, opacity)),
        border: Border {
            color: faded(rail.border.color, opacity),
            ..rail.border
        },
        scroller: scrollable::Scroller {
            background: faded_background(rail.scroller.background, opacity),
            border: Border {
                color: faded(rail.scroller.border.color, opacity),
                ..rail.scroller.border
            },
        },
    };
    scrollable::Style {
        container: faded_container(style.container, opacity),
        vertical_rail: rail(style.vertical_rail),
        horizontal_rail: rail(style.horizontal_rail),
        gap: style.gap.map(|b| faded_background(b, opacity)),
        auto_scroll: style.auto_scroll,
    }
}

/// A plain fill (`ColoredBox(color: ...)`).
pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| boxed(Some(color), 0.0, None)
}

/// The page background (`Studio.background`, the scaffold color).
pub fn page(_: &Theme) -> container::Style {
    boxed(Some(Studio::BACKGROUND), 0.0, None)
}

/// The inset well (LCD, lanes, text-field wells): `Studio.inset`, radius
/// [`radius`], `Studio.border` outline.
pub fn inset(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| boxed(Some(Studio::INSET), radius, Some(Studio::BORDER))
}

/// A raised card: `Studio.surfaceRaised`, radius [`radius`], `Studio.border`.
pub fn card(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| boxed(Some(Studio::SURFACE_RAISED), radius, Some(Studio::BORDER))
}

/// The studio dialog card (`confirm_dialog.dart`): `Studio.surfaceRaised`,
/// radius 16, `Studio.border`, shadow `0x80000000` blurred 40 at (0, 16).
pub fn dialog_card(_: &Theme) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0x80 as f32 / 255.0),
            offset: Vector::new(0.0, 16.0),
            blur_radius: 40.0,
        },
        ..boxed(Some(Studio::SURFACE_RAISED), 16.0, Some(Studio::BORDER))
    }
}

/// The studio dialogs' barrier color (`Color(0xB0000000)`).
pub const DIALOG_BARRIER: Color = Color::from_rgba8(0, 0, 0, 0xB0 as f32 / 255.0);

/// The modal barrier under a studio dialog.
pub fn dialog_barrier(_: &Theme) -> container::Style {
    boxed(Some(DIALOG_BARRIER), 0.0, None)
}

/// A context menu (`track_context_menu.dart`): `Studio.surfaceRaised`,
/// radius 10, `Studio.border`, Material elevation 10's shadow.
pub fn menu(_: &Theme) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.45),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 20.0,
        },
        ..boxed(Some(Studio::SURFACE_RAISED), 10.0, Some(Studio::BORDER))
    }
}

/// A menu row: transparent, `Studio.hover` under the pointer, at once (a
/// `MouseRegion`'s hover; the `InkWell` rows fade theirs in, see
/// `widgets::animated::ink_hover`).
pub fn menu_item(_: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then_some(Background::Color(Studio::HOVER)),
        text_color: Studio::TEXT_PRIMARY,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// Material 3's default tooltip under the zinc color scheme: the inverse
/// surface (`onSurface`, zinc foreground) with the surface color as text,
/// radius 4. Pair with [`TOOLTIP_PADDING`] and [`TOOLTIP_TEXT_SIZE`].
pub fn tooltip(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(Zinc::BACKGROUND),
        ..boxed(Some(Zinc::FOREGROUND), 4.0, None)
    }
}

/// Material 3 desktop tooltip padding (horizontal 8, vertical 4).
pub const TOOLTIP_PADDING: [f32; 2] = [4.0, 8.0];

/// Material 3 tooltip text: `bodySmall`, forui `typography.xs` (12 px).
pub const TOOLTIP_TEXT_SIZE: f32 = 12.0;

/// A floating snack bar on [`background`] (the screens pass
/// `Studio.surfaceRaised`; Material's default is the inverse surface):
/// Material 3's floating shape, radius 4.
pub fn snack_bar(background: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.4),
            offset: Vector::new(0.0, 3.0),
            blur_radius: 8.0,
        },
        ..boxed(Some(background), 4.0, None)
    }
}

/// Material 3's default snack-bar colors under the zinc scheme.
pub const SNACK_BAR_DEFAULT_BACKGROUND: Color = Zinc::FOREGROUND;
pub const SNACK_BAR_DEFAULT_TEXT: Color = Zinc::BACKGROUND;

// ---------------------------------------------------------------------- text

/// A text style of a fixed color.
pub fn text_color(color: Color) -> impl Fn(&Theme) -> text::Style {
    move |_| text::Style { color: Some(color) }
}

pub fn text_primary(_: &Theme) -> text::Style {
    text::Style {
        color: Some(Studio::TEXT_PRIMARY),
    }
}

pub fn text_secondary(_: &Theme) -> text::Style {
    text::Style {
        color: Some(Studio::TEXT_SECONDARY),
    }
}

pub fn text_faint(_: &Theme) -> text::Style {
    text::Style {
        color: Some(Studio::TEXT_FAINT),
    }
}

// --------------------------------------------------------------------- input

/// Where a studio text field's text, hint and caret sit inside the field.
/// Every Dart field is a `TextField` with `InputDecoration(isCollapsed:
/// true, border: InputBorder.none)` and no `contentPadding` of its own, so
/// `applyDefaults` gives it forui's `inputDecorationTheme.contentPadding`
/// (`FTextFieldStyle.contentPadding`, 10 all round), which a collapsed
/// decoration keeps; macOS's compact `VisualDensity` takes its
/// `baseSizeAdjustment.dy` (-8) off the field's height and half of it off
/// the text's top (`_RenderDecoration._layout`). So the text is 10 in from
/// the left and right and 6 from the top and bottom, and the field is its
/// lines plus 12 tall.
pub const FIELD_PADDING: Padding = Padding {
    top: 6.0,
    right: 10.0,
    bottom: 6.0,
    left: 10.0,
};

/// The studio text field (`TextField` with `InputDecoration(isCollapsed,
/// border: none)` inside an inset well, as `confirm_dialog.dart` and
/// `studio_field.dart` build it): no fill or border of its own (the well
/// draws them), `Studio.textPrimary` text, `Studio.textFaint` hint, faint
/// text when disabled, and Material 3's selection color (primary at 40 %).
/// iced draws the caret in the value color: the coral `cursorColor` has no
/// counterpart.
pub fn studio_input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let disabled = matches!(status, text_input::Status::Disabled);
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: Studio::TEXT_FAINT,
        placeholder: Studio::TEXT_FAINT,
        value: if disabled {
            Studio::TEXT_FAINT
        } else {
            Studio::TEXT_PRIMARY
        },
        selection: alpha(Zinc::PRIMARY, 0.40),
    }
}

// ---------------------------------------------------------------- scrollable

/// The thickness of [`scrollbar`]'s thumb (Material's desktop scrollbar).
pub const SCROLLBAR_WIDTH: f32 = 8.0;

/// Material 3's desktop scrollbar under the zinc scheme: no track, a
/// radius-8 thumb in `onSurface` at 30 %, 50 % hovered, 60 % dragged.
/// Scrollables take it through `widgets::fading_scrollbar`, which fades the
/// thumb in on a scroll and out after it, and eases its hover color, as
/// Flutter did.
pub fn scrollbar(_: &Theme, status: scrollable::Status) -> scrollable::Style {
    let (h, v) = match status {
        scrollable::Status::Active { .. } => (0.30, 0.30),
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } => (
            if is_horizontal_scrollbar_hovered {
                0.50
            } else {
                0.30
            },
            if is_vertical_scrollbar_hovered {
                0.50
            } else {
                0.30
            },
        ),
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } => (
            if is_horizontal_scrollbar_dragged {
                0.60
            } else {
                0.30
            },
            if is_vertical_scrollbar_dragged {
                0.60
            } else {
                0.30
            },
        ),
    };
    let rail = |a: f32| scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(alpha(Zinc::FOREGROUND, a)),
            border: Border {
                radius: Radius::from(8.0),
                ..Border::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail(v),
        horizontal_rail: rail(h),
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(Studio::SURFACE_RAISED),
            border: Border {
                color: Studio::BORDER,
                width: 1.0,
                radius: Radius::from(8.0),
            },
            shadow: Shadow::default(),
            icon: Studio::TEXT_PRIMARY,
        },
    }
}

/// A scrollable with no visible scrollbar (`ScrollConfiguration(scrollbars:
/// false)`, as the context menus use).
pub fn no_scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollbar(theme, status);
    let hidden = Background::Color(Color::TRANSPARENT);
    style.vertical_rail.scroller.background = hidden;
    style.horizontal_rail.scroller.background = hidden;
    style
}

/// A button that draws nothing of its own: the `GestureDetector` /
/// `MouseRegion` hit areas the studio widgets wrap their custom boxes in.
pub fn bare_button(_: &Theme, _: button::Status) -> button::Style {
    button::Style {
        background: None,
        text_color: Studio::TEXT_PRIMARY,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_palette_matches_the_dart_hex_values() {
        assert_eq!(Studio::BACKGROUND.into_rgba8(), [0x13, 0x13, 0x16, 0xFF]);
        assert_eq!(Studio::ACCENT.into_rgba8(), [0xE0, 0x7A, 0x6A, 0xFF]);
        assert_eq!(Studio::PLAYHEAD.into_rgba8(), [0xF2, 0xEF, 0xE9, 0xFF]);
        assert_eq!(Studio::GOOD.into_rgba8(), [0x63, 0xC0, 0x89, 0xFF]);
        assert_eq!(DIALOG_BARRIER.into_rgba8(), [0, 0, 0, 0xB0]);
        assert_eq!(Zinc::BARRIER.into_rgba8(), [0, 0, 0, 0x7A]);
    }

    #[test]
    fn alpha_replaces_rather_than_scales() {
        let c = alpha(alpha(Studio::ACCENT, 0.5), 0.16);
        assert!((c.a - 0.16).abs() < 1e-6);
        assert_eq!(c.r, Studio::ACCENT.r);
    }

    #[test]
    fn the_theme_uses_the_scaffold_background() {
        assert_eq!(app_theme().palette().background, Studio::BACKGROUND);
        assert_eq!(app_theme().palette().text, Zinc::FOREGROUND);
    }

    #[test]
    fn text_is_kerned_as_flutter_kerns_it() {
        // iced's `text`, as the app builds it: shaped fully (the
        // `advanced-shaping` feature), so Inter's "AV" kerning pair sets the
        // two letters closer than their two advances. Shaped glyph by glyph
        // (iced's default for ASCII), they sat exactly their advances apart.
        use crate::ui::core::text;
        use iced::widget::row;
        use std::borrow::Cow;

        assert_eq!(
            iced::widget::text::Shaping::default(),
            iced::widget::text::Shaping::Advanced
        );
        let mut settings = crate::app::settings();
        settings.fonts = font::FACES.iter().copied().map(Cow::Borrowed).collect();
        let glyphs = row![text("AV").size(40), text("A").size(40), text("V").size(40),];
        let mut ui: iced_test::Simulator<'_, ()> =
            iced_test::Simulator::with_settings(settings, glyphs);
        let width = |ui: &mut iced_test::Simulator<'_, ()>, label: &str| {
            ui.find(label).expect("the text").bounds().width
        };
        let pair = width(&mut ui, "AV");
        let apart = width(&mut ui, "A") + width(&mut ui, "V");
        assert!(pair < apart - 1.0, "{pair} vs {apart}");
    }

    /// What full shaping costs over glyph-by-glyph shaping, per text laid
    /// out (iced keeps a text's paragraph until the text changes, so this is
    /// paid per change, not per frame). A measurement, not a check: run it
    /// alone, in release, with `--ignored --nocapture`.
    #[test]
    #[ignore = "a timing measurement"]
    fn shaping_cost() {
        use iced::advanced::text::{self as core_text, Paragraph as _};
        use iced::alignment::Vertical;
        use iced::{Pixels, Renderer, Size};
        use std::time::Instant;

        crate::ui::core::widgets::studio_text::testing::load_fonts();
        let labels = [
            "VOCAL TAKES",
            "Bohemian Rhapsody",
            "-14.0 dB",
            "00:59 / 03:00",
            "Extracting the backing",
            "backing · 2 takes · 1 note",
            "Updated 2026-03-04 05:06",
            "New practice",
            "And I will always love you, I will always love you",
            "Drag an audio or video clip here, or click to browse",
            "Twang",
            "L12",
        ];
        let shape = |content: &str, shaping| {
            <Renderer as core_text::Renderer>::Paragraph::with_text(core_text::Text {
                content,
                bounds: Size::new(240.0, f32::INFINITY),
                size: Pixels(13.0),
                line_height: core_text::LineHeight::default(),
                font: font::REGULAR,
                align_x: core_text::Alignment::Default,
                align_y: Vertical::Top,
                shaping,
                wrapping: core_text::Wrapping::Word,
            })
        };
        let rounds = 400;
        for shaping in [core_text::Shaping::Basic, core_text::Shaping::Advanced] {
            for label in labels {
                let _ = shape(label, shaping);
            }
            let start = Instant::now();
            for _ in 0..rounds {
                for label in labels {
                    let _ = shape(label, shaping);
                }
            }
            let each =
                start.elapsed().as_secs_f64() * 1e6 / f64::from(rounds) / labels.len() as f64;
            println!("{shaping:?}: {each:.1} µs per text");
        }
    }
}
