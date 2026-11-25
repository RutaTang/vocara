//! The face the text toolbar's labels are set in ([`label_face`]): on
//! macOS the system UI font at the instance Flutter drew it at, else the
//! bundled Inter.
//!
//! **What Flutter drew.** The labels' style (`_kToolbarButtonFontStyle`)
//! set `inherit: false`, 14 px, w400 and no family, so the engine gave the
//! paragraph its default family, `[NSFont systemFontOfSize:14].familyName`
//! (`txt::GetDefaultFontFamilies`, `txt/platform_mac.mm`): CoreText's
//! `.AppleSystemUIFont`, which macOS (10.15 on) serves from one variable
//! face, `/System/Library/Fonts/SFNS.ttf`. On macOS Flutter 3.38 draws
//! with Skia (Impeller only where `FLTEnableImpeller` asks for it, which
//! the app's `Info.plist` did not), and Skia (the revision Flutter 3.38.4
//! pins) went on like this:
//!
//! - **the typeface**: SkParagraph matched the family through
//!   `SkFontMgr_Mac::onMatchFamily` and the regular style of its
//!   descriptors, and `create_from_desc` made it with
//!   `CTFontCreateWithFontDescriptor(desc, 0, nullptr)`: size 0, so
//!   CoreText's default, 12 pt (`SkFontMgr_mac_ct.cpp`);
//! - **the optical size**: every CoreText font Skia measured or drew the
//!   glyphs with was `SkCTFontCreateExactCopy` of it at the size needed
//!   (`SkScalerContext_mac_ct.cpp`), which sets `NSCTFontOpticalSizeAttribute`
//!   to the typeface's own optical size attribute, or, without one, its size:
//!   12 (`SkCTFontCreateExactCopy.cpp`). CoreText sets the `opsz` axis from
//!   that attribute first, clamped to the axis; so the labels were the
//!   12 pt design at every pixel size, whatever the screen's scale, not
//!   the 14 pt one CoreText would pick by itself;
//! - **the weight**: w400, the regular descriptor: `wght` 400;
//! - **the tracking**: none. The exact copy also sets
//!   `NSCTFontUnscaledTrackingAttribute` to 0, which turns CoreText's
//!   `trak` off, and the advances HarfBuzz (11.0.0) shaped with came from
//!   Skia's font functions (`SkFont::getWidths` through CoreText), not
//!   HarfBuzz's own, the only ones that apply `trak` in that release; the
//!   labels' -0.15 px letter spacing is the style's own;
//! - **the line**: SkParagraph's line is the run's metrics (CoreText's
//!   `CTFontGetAscent`, `Descent`, `Leading` of the exact copy at 14 pt),
//!   the leading split above and below (`Run::calculateMetrics`), its
//!   height rounded to a whole pixel (`InternalLineMetrics::height`), and
//!   its glyphs drawn on a baseline half the leading and the ascent below
//!   its top, rounded to a whole pixel too (`TextLine`'s
//!   `correctedBaseline`).
//!
//! **What the port draws.** cosmic-text (iced's text engine, vendored in
//! `rust/vendor/cosmic-text`) draws a face at a fixed instance of its axes
//! when told to (`FontSystem::set_face_instance`): metrics, shaping
//! (harfrust's advances, positioning and substitutions) and outlines
//! (swash) all at those coordinates, and, `untracked`, with the shaper
//! reading the face without its `trak` table (harfrust would apply it at
//! 12 pt, the face having a `STAT` table). [`register`] adds a copy of the
//! face to the font system under a family of its own ([`LABEL_FAMILY`]),
//! at Flutter's instance ([`Instance::of`]: `opsz` 12 and `wght` 400, each
//! clamped to its axis, every other axis at its default), untracked, and
//! sets the labels' line as SkParagraph did from the face's `hhea` metrics
//! and their `MVAR` deltas at that instance ([`Instance::line`]).
//!
//! Off macOS, or without the face, the labels are in Inter, their line
//! set the same way from its metrics. On a Mac (CI's runner), the test
//! `the_labels_are_drawn_at_the_instance_and_metrics_coretext_gave_skia`
//! asks CoreText for Skia's fonts as Skia made them and checks the
//! instance, the glyph advances cosmic-text lays the labels out with and
//! the line against them.

use std::sync::OnceLock;

use iced::advanced::graphics::text::cosmic_text::fontdb::{Language, Source, ID};
use iced::advanced::graphics::text::cosmic_text::skrifa::{
    raw::{
        tables::variations::{DeltaSetIndex, FloatItemDeltaTarget},
        types::{F2Dot14, FWord},
        TableProvider,
    },
    FontRef, MetadataProvider, Tag,
};
use iced::advanced::graphics::text::cosmic_text::{FaceInstance, FontSystem};
use iced::font::{Family, Stretch, Style, Weight};
use iced::Font;

use crate::ui::core::theme::font;

use super::toolbar::LABEL_SIZE;

/// Where macOS keeps its system UI font.
pub const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";

/// The family the labels' instance of the system UI font is added to the
/// font system under (no font of the machine's has it).
pub const LABEL_FAMILY: &str = ".Vocara Toolbar Label";

/// CoreText's default size, at which Skia made the typeface, and so the
/// optical size it drew the labels at (see the module docs).
pub const OPTICAL_SIZE: f32 = 12.0;

/// The labels' weight (`FontWeight.w400`).
pub const WEIGHT: f32 = 400.0;

/// The labels' face and line.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelFace {
    pub font: Font,
    /// The line's height, in pixels: SkParagraph's, a whole pixel.
    pub line_height: f32,
    /// How far below the line's top its glyphs' baseline lies, in pixels:
    /// SkParagraph's, a whole pixel.
    pub baseline: f32,
    /// Whether it is the system UI font (else Inter).
    pub system: bool,
}

/// The labels' face (see the module docs), chosen the first time it is
/// asked for. Takes the font system's lock the first time: never call it
/// with that lock held.
pub fn label_face() -> &'static LabelFace {
    static FACE: OnceLock<LabelFace> = OnceLock::new();
    FACE.get_or_init(choose)
}

/// Inter (the bundled regular face), its line set from its metrics.
pub fn inter() -> LabelFace {
    let instance = FontRef::new(font::FACES[0])
        .ok()
        .and_then(|face| Instance::of(&face))
        .expect("the bundled Inter parses");
    let (line_height, baseline) = instance.line(LABEL_SIZE);
    LabelFace {
        font: font::REGULAR,
        line_height,
        baseline,
        system: false,
    }
}

#[cfg(target_os = "macos")]
fn choose() -> LabelFace {
    use iced::advanced::graphics::text::font_system;
    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    system_ui_face(fonts.db())
        .and_then(|id| register(fonts, id))
        .map(|(face, _)| face)
        .unwrap_or_else(inter)
}

#[cfg(not(target_os = "macos"))]
fn choose() -> LabelFace {
    inter()
}

/// The system UI face the font system loaded from [`SYSTEM_UI_FONT`], if
/// any.
pub fn system_ui_face(
    db: &iced::advanced::graphics::text::cosmic_text::fontdb::Database,
) -> Option<ID> {
    db.faces()
        .find(|face| match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => {
                // However the font system spelled the folder it loaded.
                path.ends_with(SYSTEM_UI_FONT.trim_start_matches('/'))
            }
            Source::Binary(_) => false,
        })
        .map(|face| face.id)
}

/// Adds a copy of the face `id` to `fonts` under [`LABEL_FAMILY`], drawn
/// at Flutter's instance of it, untracked: the labels' face, and the
/// instance.
pub fn register(fonts: &mut FontSystem, id: ID) -> Option<(LabelFace, Instance)> {
    let info = fonts.db().face(id)?.clone();
    let instance = fonts
        .db()
        .with_face_data(id, |data, index| {
            Instance::of(&FontRef::from_index(data, index).ok()?)
        })
        .flatten()?;
    let copy = iced::advanced::graphics::text::cosmic_text::fontdb::FaceInfo {
        families: vec![(LABEL_FAMILY.to_string(), Language::English_UnitedStates)],
        ..info
    };
    let copy = fonts.db_mut().push_face_info(copy);
    fonts.set_face_instance(
        copy,
        Some(FaceInstance {
            coords: instance
                .coords
                .iter()
                .map(|(tag, value)| (tag.to_be_bytes(), *value))
                .collect(),
            untracked: true,
        }),
    );
    let (line_height, baseline) = instance.line(LABEL_SIZE);
    let face = LabelFace {
        font: Font {
            family: Family::Name(LABEL_FAMILY),
            weight: Weight::Normal,
            stretch: Stretch::Normal,
            style: Style::Normal,
        },
        line_height,
        baseline,
        system: true,
    };
    Some((face, instance))
}

/// The instance of a face Flutter drew the labels at, and its line
/// metrics there.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// Its coordinate on each of the face's axes (`fvar` order), in user
    /// units: `opsz` [`OPTICAL_SIZE`] and `wght` [`WEIGHT`], clamped, the
    /// others their defaults.
    pub coords: Vec<(Tag, f32)>,
    pub units_per_em: u16,
    /// `hhea`'s ascender, |descender| and line gap, with their `MVAR`
    /// deltas there (`hasc`, `hdsc`, `hlgp`, unrounded), in font units:
    /// what CoreText reports (the macOS test checks it).
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
}

impl Instance {
    /// Flutter's instance of `face` (see the module docs).
    pub fn of(face: &FontRef<'_>) -> Option<Self> {
        let axes = face.axes();
        let coords: Vec<(Tag, f32)> = axes
            .iter()
            .map(|axis| {
                let value = match &axis.tag().to_be_bytes() {
                    b"opsz" => OPTICAL_SIZE,
                    b"wght" => WEIGHT,
                    _ => axis.default_value(),
                };
                (axis.tag(), value.clamp(axis.min_value(), axis.max_value()))
            })
            .collect();
        let location = axes.location(coords.iter().copied());
        let normalized: Vec<F2Dot14> = location.coords().to_vec();
        let hhea = face.hhea().ok()?;
        let mvar = face.mvar().ok();
        // A metric's `MVAR` delta there, unrounded.
        let delta = |tag: &[u8; 4]| {
            let mvar = mvar.as_ref()?;
            let record = mvar
                .value_records()
                .iter()
                .find(|record| record.value_tag() == Tag::new(tag))?;
            let index = DeltaSetIndex {
                outer: record.delta_set_outer_index(),
                inner: record.delta_set_inner_index(),
            };
            mvar.item_variation_store()?
                .ok()?
                .compute_float_delta(index, &normalized)
                .ok()
        };
        let metric = |value: FWord, tag: &[u8; 4]| {
            delta(tag).map_or(f32::from(value.to_i16()), |delta| {
                value.apply_float_delta(delta)
            })
        };
        Some(Self {
            coords,
            units_per_em: face.head().ok()?.units_per_em(),
            ascent: metric(hhea.ascender(), b"hasc"),
            descent: -metric(hhea.descender(), b"hdsc"),
            leading: metric(hhea.line_gap(), b"hlgp"),
        })
    }

    /// Its coordinate on the axis `tag`, if the face has the axis.
    pub fn coord(&self, tag: &[u8; 4]) -> Option<f32> {
        self.coords
            .iter()
            .find(|(t, _)| t.to_be_bytes() == *tag)
            .map(|(_, value)| *value)
    }

    /// SkParagraph's line at `size` pixels: its height (the ascent, the
    /// descent and the leading, rounded to a whole pixel:
    /// `InternalLineMetrics::height`) and its baseline's distance below
    /// its top (half the leading over the ascent, rounded to a whole pixel
    /// too: `TextLine`'s `correctedBaseline`, where its glyphs are drawn).
    pub fn line(&self, size: f32) -> (f32, f32) {
        let scale = size / f32::from(self.units_per_em.max(1));
        let (ascent, descent, leading) = (
            self.ascent * scale,
            self.descent * scale,
            self.leading * scale,
        );
        (
            (ascent + descent + leading).round(),
            (leading / 2.0 + ascent + 0.5).floor(),
        )
    }
}

#[cfg(test)]
mod tests;
