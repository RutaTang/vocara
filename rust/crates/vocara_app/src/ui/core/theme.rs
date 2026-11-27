//! The app's look: the design system's tokens (`rust/DESIGN.md`): the
//! [`Studio`] palette and track colours, the [`type_scale`], [`radius`]
//! and [`height`] scales; forui's zinc-dark palette the Flutter app's
//! Material layer was themed with ([`Zinc`], port of
//! `ui/core/themes/theme.dart`'s `appTheme = FThemes.zinc.dark`, kept only
//! for the looks that still copy a third party's: the text field's
//! Cupertino menu, the licence page Flutter's `LicensePage` drew, the
//! Material switch and the desktop scrollbar); the iced [`Theme`]; the app
//! font (Inter); and the style functions every screen shares: surfaces,
//! cards, the dialog card and barrier, menus, tooltips, toasts, text fields
//! and the scrollbar.
//!
//! The values follow `DESIGN.md`, deliberately departing from the Flutter
//! app's look (`PORTING.md`, Design refresh); behaviour and layouts do not
//! change. Where a comment elsewhere says a value matches Flutter, the
//! tokens here win.
//!
//! Flutter's `color.withValues(alpha: a)` *replaces* the alpha; [`alpha`] is
//! its counterpart (not iced's `scale_alpha`, which multiplies).

use crate::ui::core::text;
use iced::border::Radius;
use iced::font::{Family, Weight};
use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Font, Padding, Shadow, Theme, Vector};

/// The design system's colour tokens (`rust/DESIGN.md`, Colour tokens):
/// charcoal surfaces with one coral accent used for meaning. Dark only.
///
/// The values follow `DESIGN.md`, which deliberately departs from the
/// Flutter app's look (`PORTING.md`, Design refresh): where a comment
/// elsewhere says a colour matches Flutter, this palette wins.
pub struct Studio;

impl Studio {
    // Surfaces, darkest to lightest.
    /// Window, arrange and page background.
    pub const BACKGROUND: Color = Color::from_rgb8(0x11, 0x11, 0x14);
    /// The unified toolbar (title bar) at the top of every page.
    pub const TOOLBAR: Color = Color::from_rgb8(0x18, 0x18, 0x1C);
    /// Inspector, right panel, dock, track headers.
    pub const PANEL: Color = Color::from_rgb8(0x17, 0x17, 0x1B);
    /// A selected track header.
    pub const PANEL_SELECTED: Color = Color::from_rgb8(0x1D, 0x1D, 0x22);
    /// Cards, mixer strips.
    pub const SURFACE: Color = Color::from_rgb8(0x1B, 0x1B, 0x20);
    /// Pop-up buttons, raised controls, toolbar groups.
    pub const SURFACE_RAISED: Color = Color::from_rgb8(0x23, 0x23, 0x29);
    /// A segmented control's background.
    pub const SEGMENTED: Color = Color::from_rgb8(0x1C, 0x1C, 0x21);
    /// A segmented control's selected segment.
    pub const SEGMENT_SELECTED: Color = Color::from_rgb8(0x34, 0x34, 0x3D);
    /// M / S / R buttons and an inactive power button.
    pub const CONTROL: Color = Color::from_rgb8(0x26, 0x26, 0x2D);
    /// A selected tab.
    pub const CONTROL_ACTIVE: Color = Color::from_rgb8(0x2A, 0x2A, 0x31);
    /// Time display, meters, value fields.
    pub const INSET: Color = Color::from_rgb8(0x0C, 0x0C, 0x0F);
    /// Graph and lane wells, text fields, a library card's waveform band.
    pub const WELL: Color = Color::from_rgb8(0x0F, 0x0F, 0x12);
    /// Row and button hover.
    pub const HOVER: Color = Color::from_rgb8(0x22, 0x22, 0x28);

    // Lines.
    /// Arrange bar lines, semitone lines.
    pub const GRID: Color = Color::from_rgb8(0x18, 0x18, 0x1D);
    /// Ruler major ticks, dividers inside groups.
    pub const GRID_STRONG: Color = Color::from_rgb8(0x34, 0x34, 0x3D);
    /// Panel and toolbar hairlines (1 px).
    pub const BORDER: Color = Color::from_rgb8(0x26, 0x26, 0x2D);
    /// Row separators inside the arrange.
    pub const BORDER_SUBTLE: Color = Color::from_rgb8(0x1E, 0x1E, 0x24);
    /// Control outlines (an inset 1 px outline).
    pub const BORDER_STRONG: Color = Color::from_rgb8(0x2E, 0x2E, 0x36);

    // Text.
    /// Names, values, body.
    pub const TEXT_PRIMARY: Color = Color::from_rgb8(0xEC, 0xEC, 0xF1);
    /// Labels, inactive tabs, icons.
    pub const TEXT_SECONDARY: Color = Color::from_rgb8(0xA7, 0xA7, 0xB3);
    /// Metadata, ruler labels, units (at least 4.5:1 on `BACKGROUND`).
    pub const TEXT_FAINT: Color = Color::from_rgb8(0x80, 0x80, 0x8D);
    /// Axis labels, disabled text (10 px and up only).
    pub const TEXT_DISABLED: Color = Color::from_rgb8(0x6E, 0x6E, 0x7A);

    // Accent (coral) + status.
    /// Selection, primary emphasis.
    pub const ACCENT: Color = Color::from_rgb8(0xE0, 0x7A, 0x6A);
    /// Accent text and icons on dark (power on, the current lyric's time).
    pub const ACCENT_BRIGHT: Color = Color::from_rgb8(0xEC, 0x91, 0x82);
    /// Text on an `ACCENT` fill (a dialog's primary button).
    pub const ON_ACCENT: Color = Color::from_rgb8(0x1B, 0x0F, 0x0D);
    /// The record dot, armed and muted states.
    pub const RECORD: Color = Color::from_rgb8(0xE5, 0x57, 0x4B);
    /// Text and glyphs on a `RECORD` tint (mute on, record on).
    pub const RECORD_TEXT: Color = Color::from_rgb8(0xF0, 0x8A, 0x80);
    /// The playhead line and its ruler marker.
    pub const PLAYHEAD: Color = Color::from_rgb8(0xEC, 0xEC, 0xF1);
    pub const LCD: Color = Color::from_rgb8(0xEF, 0xEF, 0xF3);
    /// Meter upper range, solo on.
    pub const WARN: Color = Color::from_rgb8(0xD9, 0xB5, 0x4A);
    /// Text on a `WARN` tint (solo on).
    pub const WARN_TEXT: Color = Color::from_rgb8(0xE3, 0xC4, 0x6A);
    /// A meter's low range.
    pub const METER_LOW: Color = Color::from_rgb8(0x4F, 0xAE, 0x82);
    /// A sung pitch off its target by more than the tolerance.
    pub const OUT_OF_TUNE: Color = Color::from_rgb8(0xF2, 0xA6, 0x5A);

    // Waveform when the track is not selected.
    pub const WAVEFORM: Color = Color::from_rgb8(0x6A, 0x6A, 0x77);
    pub const TOGGLE_OFF: Color = Color::from_rgb8(0x3A, 0x3A, 0x43);

    // Semantic analysis colors shared by the dock views. Chest is warm, head is
    // cool; mix is derived by lerping the two at the usage site.
    pub const REGISTER_CHEST: Color = Color::from_rgb8(0xE0, 0x82, 0x5A);
    pub const REGISTER_HEAD: Color = Color::from_rgb8(0x5B, 0x8D, 0xEF);
    /// Pitch within tolerance / healthy: the sung pitch line.
    pub const IN_TUNE: Color = Color::from_rgb8(0x7C, 0xC4, 0xA6);
    /// Metric in the good range.
    pub const GOOD: Color = Color::from_rgb8(0x7C, 0xC4, 0xA6);

    // Track colours: one muted colour per track, of equal lightness, for the
    // header stripe, the clip and its waveform (`DESIGN.md`, Track colours).
    pub const TRACK_CORAL: Color = Color::from_rgb8(0xD9, 0x8C, 0x7E);
    pub const TRACK_OCHRE: Color = Color::from_rgb8(0xC9, 0xA4, 0x6E);
    pub const TRACK_SAGE: Color = Color::from_rgb8(0x6F, 0xB3, 0x9A);
    pub const TRACK_PERIWINKLE: Color = Color::from_rgb8(0x8F, 0xA3, 0xD9);
    pub const TRACK_LILAC: Color = Color::from_rgb8(0xB4, 0x8F, 0xD0);
    /// The backing track's (neutral).
    pub const TRACK_BACKING: Color = Color::from_rgb8(0x8C, 0x8C, 0x99);
    /// The master's.
    pub const TRACK_MASTER: Color = Color::from_rgb8(0xB7, 0xB7, 0xC2);
    /// A selected clip's outline.
    pub const CLIP_SELECTED_OUTLINE: Color = Color::from_rgb8(0xE8, 0xA9, 0x9D);

    /// The vocal tracks' colours, in the order tracks take them.
    pub const TRACK_COLORS: [Color; 5] = [
        Self::TRACK_CORAL,
        Self::TRACK_OCHRE,
        Self::TRACK_SAGE,
        Self::TRACK_PERIWINKLE,
        Self::TRACK_LILAC,
    ];

    /// The colour of the `index`th vocal track (the palette, repeating).
    pub const fn track_color(index: usize) -> Color {
        Self::TRACK_COLORS[index % Self::TRACK_COLORS.len()]
    }
}

/// How a clip is drawn in its track colour (`DESIGN.md`, Track colours):
/// the alphas the colour takes for each part.
pub mod clip {
    /// The clip's fill.
    pub const FILL: f32 = 0.10;
    /// A selected clip's fill.
    pub const FILL_SELECTED: f32 = 0.16;
    /// The clip's 1 px outline (selected: `Studio::CLIP_SELECTED_OUTLINE`).
    pub const OUTLINE: f32 = 0.32;
    /// The 13 px header band holding the name.
    pub const HEADER: f32 = 0.22;
    /// A selected clip's header band.
    pub const HEADER_SELECTED: f32 = 0.34;
    /// The header band's height.
    pub const HEADER_HEIGHT: f32 = 13.0;
    /// The waveform.
    pub const WAVEFORM: f32 = 0.92;
    /// A muted clip's waveform (the clip itself at [`MUTED_OPACITY`]).
    pub const WAVEFORM_MUTED: f32 = 0.45;
    /// A muted clip's opacity.
    pub const MUTED_OPACITY: f32 = 0.60;
}

/// The type scale (`DESIGN.md`, Type): every text size the app uses. No
/// in-between sizes. Weights are [`font`]'s; numbers take tabular figures
/// (`studio_text`'s `tabular`).
pub mod type_scale {
    /// Axis, ruler and small units (10 px regular, tabular).
    pub const AXIS: f32 = 10.0;
    /// Labels, meta, values, dock tabs, control text (11 px).
    pub const LABEL: f32 = 11.0;
    /// Body, track and effect names, panel titles, toolbar buttons (12 px).
    pub const BODY: f32 = 12.0;
    /// Lyrics, list rows, page titles in a toolbar, card titles (13 px).
    pub const TITLE: f32 = 13.0;
    /// The time display (14 px monospaced medium).
    pub const TIME: f32 = 14.0;
    /// A page heading ("Practices"): 15 px semibold.
    pub const HEADING: f32 = 15.0;
    /// A big readout (pitch %, a metric's value): 18 px semibold, tabular.
    pub const READOUT: f32 = 18.0;
    /// The line height of 11 and 12 px text, in pixels.
    pub const LINE: f32 = 16.0;
}

/// The corner radii (`DESIGN.md`, Geometry). Round only for knobs, dots
/// and the power button.
pub mod radius {
    /// M / S buttons, clips, small controls, a selected segment.
    pub const SMALL: f32 = 4.0;
    /// Pop-up buttons, ghost buttons, tabs.
    pub const CONTROL: f32 = 5.0;
    /// Segmented controls, wells, text fields, toolbar icon buttons, dialog
    /// buttons, tooltips.
    pub const WELL: f32 = 6.0;
    /// Toolbar groups.
    pub const GROUP: f32 = 7.0;
    /// Cards, panels' rounded boxes, menus.
    pub const CARD: f32 = 8.0;
    /// Dialogs.
    pub const DIALOG: f32 = 10.0;
}

/// Control heights (`DESIGN.md`, Geometry).
pub mod height {
    /// M / S buttons, tab buttons in a strip.
    pub const MINI: f32 = 20.0;
    /// Pop-up buttons, segmented controls, ghost buttons, parameter rows.
    pub const CONTROL: f32 = 22.0;
    /// Panel icon buttons, panel tabs.
    pub const PANEL_BUTTON: f32 = 24.0;
    /// Toolbar icon buttons, dialog buttons, single-line text fields.
    pub const TOOLBAR_BUTTON: f32 = 28.0;
    /// Toolbar groups, the time display.
    pub const TOOLBAR_GROUP: f32 = 30.0;
    /// The unified toolbar (its bottom hairline included).
    pub const TOOLBAR: f32 = 52.0;
}

/// forui's `FThemes.zinc.dark` colors (forui 0.17), which themed the
/// Flutter app's chrome (home, dialogs, buttons) through
/// `toApproximateMaterialTheme()`. Retired for the chrome (`DESIGN.md`):
/// dialogs, menus, tooltips, text fields, toasts and buttons draw with
/// [`Studio`]; only the looks that copy a third party's keep it (the module
/// docs).
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

    /// Loads the bundled faces into the font system every renderer shares,
    /// and takes every other face of their family off it: a copy of Inter
    /// installed on the machine (another version, with other metrics and
    /// kerning) would otherwise be matched first, where Flutter set every
    /// text in the faces forui bundled whatever the system had. Idempotent.
    pub fn install() {
        use iced::advanced::graphics::text::cosmic_text::fontdb::Source;
        use std::borrow::Cow;

        let mut fonts = iced::advanced::graphics::text::font_system()
            .write()
            .expect("the font system");
        for face in FACES {
            fonts.load_font(Cow::Borrowed(face));
        }
        let db = fonts.raw().db_mut();
        let others: Vec<_> = db
            .faces()
            .filter(|face| {
                !matches!(face.source, Source::Binary(_))
                    && face.families.iter().any(|(name, _)| name == FAMILY)
            })
            .map(|face| face.id)
            .collect();
        for id in others {
            db.remove_face(id);
        }
    }

    /// The bundled faces (the weights the Dart UI uses: w400..w800), to load
    /// at startup ([`install`]).
    pub const FACES: [&[u8]; 5] = [
        include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
        include_bytes!("../../../assets/fonts/inter/Inter-ExtraBold.ttf"),
    ];
}

/// The app's iced theme: the page background and the primary text colour;
/// the rest of the palette (what iced's own default styles read) as forui
/// zinc-dark's.
pub fn app_theme() -> Theme {
    Theme::custom(
        "Vocara",
        iced::theme::Palette {
            background: Studio::BACKGROUND,
            text: Studio::TEXT_PRIMARY,
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

/// `color` at `opacity` of its alpha: a color that fades on its own (a
/// barrier's, a highlight's). A faded subtree fades as one layer instead
/// (`widgets::opacity_layer`).
pub const fn faded(color: Color, opacity: f32) -> Color {
    Color {
        a: color.a * opacity,
        ..color
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

/// The inset well (LCD, lanes, text-field wells): `Studio::INSET`, radius
/// [`radius`], an inset `Studio::BORDER` outline (an iced border is drawn
/// inside the box and takes no room).
pub fn inset(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| boxed(Some(Studio::INSET), radius, Some(Studio::BORDER))
}

/// A graph or lane well (`DESIGN.md`: the EQ graph, the pitch lane, a
/// library card's waveform band): `Studio::WELL`, radius [`radius`], an
/// inset `Studio::BORDER_STRONG` outline.
pub fn well(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| boxed(Some(Studio::WELL), radius, Some(Studio::BORDER_STRONG))
}

/// A raised card: `Studio::SURFACE_RAISED`, radius [`radius`], an inset
/// `Studio::BORDER_STRONG` outline.
pub fn card(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        boxed(
            Some(Studio::SURFACE_RAISED),
            radius,
            Some(Studio::BORDER_STRONG),
        )
    }
}

/// The soft shadow under a floating surface (dialogs, menus, toasts, a
/// hovered card): black at 45 %, blurred 24, 8 down.
pub const FLOATING_SHADOW: Shadow = Shadow {
    color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
    offset: Vector::new(0.0, 8.0),
    blur_radius: 24.0,
};

/// The studio dialog card (`DESIGN.md`, Dialogs): `Studio::SURFACE`,
/// radius 10, an inset `Studio::BORDER_STRONG` outline, a soft shadow.
pub fn dialog_card(_: &Theme) -> container::Style {
    container::Style {
        shadow: FLOATING_SHADOW,
        ..boxed(
            Some(Studio::SURFACE),
            radius::DIALOG,
            Some(Studio::BORDER_STRONG),
        )
    }
}

/// The studio dialogs' barrier color: black at 50 %.
pub const DIALOG_BARRIER: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.5);

/// The modal barrier under a studio dialog.
pub fn dialog_barrier(_: &Theme) -> container::Style {
    boxed(Some(DIALOG_BARRIER), 0.0, None)
}

/// A menu or a pop-up's list (`DESIGN.md`, Menus): `Studio::SURFACE`,
/// radius 8, an inset `Studio::BORDER_STRONG` outline, a soft shadow.
pub fn menu(_: &Theme) -> container::Style {
    container::Style {
        shadow: FLOATING_SHADOW,
        ..boxed(
            Some(Studio::SURFACE),
            radius::CARD,
            Some(Studio::BORDER_STRONG),
        )
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

/// A tooltip (`DESIGN.md`, Tooltips): `Studio::SURFACE`, radius 6, an
/// inset `Studio::BORDER_STRONG` outline, `TEXT_PRIMARY` text. No shadow:
/// the tip fades as one layer (`material_tooltip`), which a shadow
/// reaching past it would not. Pair with [`TOOLTIP_PADDING`] and
/// [`TOOLTIP_TEXT_SIZE`].
pub fn tooltip(_: &Theme) -> container::Style {
    container::Style {
        text_color: Some(Studio::TEXT_PRIMARY),
        ..boxed(
            Some(Studio::SURFACE),
            radius::WELL,
            Some(Studio::BORDER_STRONG),
        )
    }
}

/// A tooltip's padding (vertical 4, horizontal 8).
pub const TOOLTIP_PADDING: [f32; 2] = [4.0, 8.0];

/// A tooltip's text: the 11 px label size.
pub const TOOLTIP_TEXT_SIZE: f32 = type_scale::LABEL;

/// A toast's (snack bar's) corner radius: a card's.
pub const SNACK_BAR_RADIUS: f32 = radius::CARD;

/// A toast's colours when it sets none of its own: a raised surface with
/// primary text (`DESIGN.md`'s floating surfaces).
pub const SNACK_BAR_DEFAULT_BACKGROUND: Color = Studio::SURFACE_RAISED;
pub const SNACK_BAR_DEFAULT_TEXT: Color = Studio::TEXT_PRIMARY;

/// A toast's text size: the 12 px body size.
pub const TOAST_TEXT_SIZE: f32 = type_scale::BODY;

/// A toast (snack bar) card: `SNACK_BAR_DEFAULT_BACKGROUND`, radius 8, an
/// inset `Studio::BORDER_STRONG` outline, the floating shadow.
pub fn toast(_: &Theme) -> container::Style {
    container::Style {
        shadow: FLOATING_SHADOW,
        ..boxed(
            Some(SNACK_BAR_DEFAULT_BACKGROUND),
            SNACK_BAR_RADIUS,
            Some(Studio::BORDER_STRONG),
        )
    }
}

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

/// Selected text in a field: the accent at 35 %.
pub const SELECTION: Color = alpha(Studio::ACCENT, 0.35);

/// A single-line text field's height (`DESIGN.md`, Text fields).
pub const FIELD_HEIGHT: f32 = height::TOOLBAR_BUTTON;

/// A text field's text size: the 12 px body size.
pub const FIELD_TEXT_SIZE: f32 = type_scale::BODY;

/// A focused field's outline: the accent at 60 %.
pub const FIELD_FOCUS: Color = alpha(Studio::ACCENT, 0.60);

/// The studio text field inside a well its container draws: no fill or
/// border of its own, `TEXT_PRIMARY` text, a `TEXT_FAINT` hint, faint text
/// when disabled, [`SELECTION`]. The coral caret is the field's own
/// (`text_field`'s `caret_color`). A field that draws its own well takes
/// [`field`] instead.
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
        selection: SELECTION,
    }
}

/// A text field drawing its own well (`DESIGN.md`, Text fields): 28 px
/// single-line with [`FIELD_PADDING`] and 12 px text, `Studio::WELL`,
/// radius 6, an inset `Studio::BORDER_STRONG` outline, [`FIELD_FOCUS`]
/// while focused; [`studio_input`]'s text colours.
pub fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(Studio::WELL),
        border: Border {
            color: if focused {
                FIELD_FOCUS
            } else {
                Studio::BORDER_STRONG
            },
            width: 1.0,
            radius: Radius::from(radius::WELL),
        },
        ..studio_input(theme, status)
    }
}

/// The well around a field that cannot draw its own (a multi-line editor):
/// [`field`]'s, focused or not.
pub fn field_well(focused: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        boxed(
            Some(Studio::WELL),
            radius::WELL,
            Some(if focused {
                FIELD_FOCUS
            } else {
                Studio::BORDER_STRONG
            }),
        )
    }
}

// ---------------------------------------------------------------- scrollable

/// `color.withOpacity(opacity)`: the alpha replaced by `opacity`, rounded to
/// 8 bits (`withAlpha((255 * opacity).round())`).
pub fn with_opacity(color: Color, opacity: f32) -> Color {
    alpha(color, (255.0 * opacity).round() / 255.0)
}

/// The thumb of Material's desktop scrollbar under the zinc scheme, which
/// is dark: `onSurface` at 30 %, 65 % under the pointer, 75 % dragged
/// (`_MaterialScrollbarState._thumbColor`, `Brightness.dark`, each
/// `withOpacity`), radius 8 (`_kScrollbarRadius`), no track. Scrollables
/// take it through `widgets::fading_scrollbar`, which draws the thumb
/// itself (iced's own scrollbar hidden) with these colors and this radius:
/// 8 wide, 2 in from the edge, at least 48 long, fading in on a scroll and
/// out after it and easing its hover color, as Flutter did.
pub fn scrollbar(_: &Theme, status: scrollable::Status) -> scrollable::Style {
    let (idle, hovered, dragged) = (0.30, 0.65, 0.75);
    let (h, v) = match status {
        scrollable::Status::Active { .. } => (idle, idle),
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered: x,
            is_vertical_scrollbar_hovered: y,
            ..
        } => (
            if x { hovered } else { idle },
            if y { hovered } else { idle },
        ),
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged: x,
            is_vertical_scrollbar_dragged: y,
            ..
        } => (
            if x { dragged } else { idle },
            if y { dragged } else { idle },
        ),
    };
    let rail = |a: f32| scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(with_opacity(Zinc::FOREGROUND, a)),
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
    fn studio_palette_matches_the_design_tokens() {
        let hex = |c: Color| {
            let [r, g, b, _] = c.into_rgba8();
            format!("#{r:02X}{g:02X}{b:02X}")
        };
        // DESIGN.md's colour table, row by row.
        for (token, want) in [
            (Studio::BACKGROUND, "#111114"),
            (Studio::TOOLBAR, "#18181C"),
            (Studio::PANEL, "#17171B"),
            (Studio::PANEL_SELECTED, "#1D1D22"),
            (Studio::SURFACE, "#1B1B20"),
            (Studio::SURFACE_RAISED, "#232329"),
            (Studio::SEGMENTED, "#1C1C21"),
            (Studio::CONTROL, "#26262D"),
            (Studio::CONTROL_ACTIVE, "#2A2A31"),
            (Studio::SEGMENT_SELECTED, "#34343D"),
            (Studio::INSET, "#0C0C0F"),
            (Studio::WELL, "#0F0F12"),
            (Studio::HOVER, "#222228"),
            (Studio::GRID, "#18181D"),
            (Studio::GRID_STRONG, "#34343D"),
            (Studio::BORDER, "#26262D"),
            (Studio::BORDER_SUBTLE, "#1E1E24"),
            (Studio::BORDER_STRONG, "#2E2E36"),
            (Studio::TEXT_PRIMARY, "#ECECF1"),
            (Studio::TEXT_SECONDARY, "#A7A7B3"),
            (Studio::TEXT_FAINT, "#80808D"),
            (Studio::TEXT_DISABLED, "#6E6E7A"),
            (Studio::ACCENT, "#E07A6A"),
            (Studio::ACCENT_BRIGHT, "#EC9182"),
            (Studio::ON_ACCENT, "#1B0F0D"),
            (Studio::RECORD, "#E5574B"),
            (Studio::RECORD_TEXT, "#F08A80"),
            (Studio::PLAYHEAD, "#ECECF1"),
            (Studio::IN_TUNE, "#7CC4A6"),
            (Studio::GOOD, "#7CC4A6"),
            (Studio::METER_LOW, "#4FAE82"),
            (Studio::WARN, "#D9B54A"),
            (Studio::WARN_TEXT, "#E3C46A"),
        ] {
            assert_eq!(hex(token), want);
        }
        assert_eq!(
            Studio::TRACK_COLORS.map(hex),
            ["#D98C7E", "#C9A46E", "#6FB39A", "#8FA3D9", "#B48FD0"]
        );
        assert_eq!(hex(Studio::TRACK_BACKING), "#8C8C99");
        assert_eq!(hex(Studio::TRACK_MASTER), "#B7B7C2");
        assert_eq!(Studio::track_color(6), Studio::TRACK_OCHRE, "repeats");
        // The barrier is black at 50 %.
        assert_eq!(DIALOG_BARRIER.into_rgba8(), [0, 0, 0, 0x80]);
        assert_eq!(Zinc::BARRIER.into_rgba8(), [0, 0, 0, 0x7A]);
    }

    #[test]
    fn text_on_the_background_meets_its_contrast() {
        // WCAG's relative luminance and contrast ratio.
        let luminance = |c: Color| {
            let lin = |v: f32| {
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
        };
        let contrast = |a: Color, b: Color| {
            let (x, y) = (luminance(a), luminance(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        // DESIGN.md: TEXT_FAINT is at least 4.5:1 on the background.
        assert!(contrast(Studio::TEXT_FAINT, Studio::BACKGROUND) >= 4.5);
        assert!(contrast(Studio::TEXT_SECONDARY, Studio::PANEL) >= 4.5);
    }

    #[test]
    fn a_field_draws_its_well_and_its_focus() {
        let theme = app_theme();
        let rest = field(&theme, text_input::Status::Active);
        assert_eq!(rest.background, Background::Color(Studio::WELL));
        assert_eq!(rest.border.color, Studio::BORDER_STRONG);
        assert_eq!(rest.border.radius, Radius::from(6.0));
        let focused = field(&theme, text_input::Status::Focused { is_hovered: false });
        assert_eq!(focused.border.color, alpha(Studio::ACCENT, 0.6));
        assert_eq!(focused.placeholder, Studio::TEXT_FAINT);
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
        assert_eq!(app_theme().palette().text, Studio::TEXT_PRIMARY);
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
