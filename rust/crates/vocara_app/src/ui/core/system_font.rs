//! The system UI font, which Flutter set every text no theme reached in (a
//! `TextPainter`'s: the text toolbar's labels, the canvas labels), at the
//! instance Flutter's Skia drew each weight at ([`face`]); off macOS, or
//! without the face, the bundled Inter at that weight.
//!
//! **What Flutter drew.** A `TextPainter` inherits no theme, and the app's
//! painters' styles set no `fontFamily`: the engine's paragraph named the
//! empty family (`ParagraphStyle::GetTextStyle`), which no font manager
//! matches, so SkParagraph took its font collection's default families
//! (`FontCollection::findTypefaces`), which the engine sets to
//! `[NSFont systemFontOfSize:14].familyName` (`txt::GetDefaultFontFamilies`,
//! `txt/platform_mac.mm`): CoreText's `.AppleSystemUIFont`, which macOS
//! (10.15 on) serves from one variable face,
//! `/System/Library/Fonts/SFNS.ttf`. On macOS Flutter 3.38 draws with Skia
//! (Impeller only where `FLTEnableImpeller` asks for it, which the app's
//! `Info.plist` did not), and Skia (the revision Flutter 3.38.4 pins,
//! `ea7cdbc6b986bbefcbac92fa429782e59518510f`) went on like this:
//!
//! - **the typeface of a weight**: `FontCollection::matchTypeface` asked
//!   `SkFontMgr_Mac` for the family's style set (`onMatchFamily`): the
//!   descriptors CoreText matches to the family's name
//!   (`CTFontDescriptorCreateMatchingFontDescriptors`, `SkFontStyleSet_Mac`,
//!   `SkFontMgr_mac_ct.cpp`), one for each named instance of a variable
//!   face; and the set's `matchStyle` took the first whose style is nearest
//!   the text's (`findMatchingDesc`, `compute_metric`: the weights'
//!   difference squared, plus the widths' times 100 and a slant's 900,
//!   squared). The text's style is its `FontWeight`'s value, a normal
//!   width, upright (`GetSkFontStyleWeight`, `MakeSkFontStyle`,
//!   `txt/src/skia/paragraph_builder_skia.cc`). A descriptor's is its
//!   CoreText traits (`SkCTFontDescriptorGetSkFontStyle`,
//!   `SkTypeface_mac_ct.cpp`): its weight trait mapped onto CSS weights
//!   through AppKit's `NSFontWeight` constants, -0.8 (`UltraLight`) to 0.62
//!   (`Black`) at 100 to 900, linearly between them and rounded
//!   (`ct_weight_to_fontstyle`, `SkCTFontGetNSFontWeightMapping`:
//!   [`css_weight`]). CoreText gives San Francisco's named weights those
//!   very constants for traits, at `wght` 30, 274, 400, 510, 590, 700, 860
//!   and 1000 (WebKit's measurements, the keyframes of `normalizeCTWeight`
//!   in `Source/WebCore/platform/graphics/cocoa/FontMetricsNormalization.h`:
//!   [`ct_weight`]), the instances `SFNS.ttf` names Ultralight (`wght`
//!   30.925), Light (274.315), Regular, Medium, Semibold, Bold, Heavy and
//!   Black, besides Thin (110.725) and the same weights at other widths
//!   and grades. So `FontWeight.w400` is Regular (`wght` 400), w500 Medium
//!   (510), w600 Semibold (590) and w700 Bold (700): not 100 times the
//!   weight. `create_from_desc` made the typeface with
//!   `CTFontCreateWithFontDescriptor(desc, 0, nullptr)`: size 0, so
//!   CoreText's default, 12 pt. Its style the text's, SkParagraph fakes no
//!   bold (`OneLineShaper::shape`, `fakeMissingFontStyles`, emboldens only
//!   a weight of 600 or more drawn with a typeface 200 or more lighter);
//! - **the optical size**: every CoreText font Skia measured or drew the
//!   glyphs with was `SkCTFontCreateExactCopy` of the typeface at the size
//!   needed (`SkScalerContext_mac_ct.cpp`), which sets
//!   `NSCTFontOpticalSizeAttribute` to the typeface's own optical size
//!   attribute, or, without one, its size: 12 (`SkCTFontCreateExactCopy.cpp`).
//!   CoreText sets the `opsz` axis from that attribute first, clamped to
//!   the axis; so every label was the 12 pt design, whatever its size and
//!   the screen's scale, not the one CoreText would pick for its size by
//!   itself. `SFNS.ttf`'s optical size axis starts at 17 (17 to 96, at 28),
//!   so the labels (8.5 to 14 px) are its 17 pt design either way;
//! - **the other axes**: the named instance's (`SFNS.ttf`'s weights at a
//!   normal width and grade have `wdth` and `GRAD` at their defaults);
//! - **the tracking**: none. The exact copy also sets
//!   `NSCTFontUnscaledTrackingAttribute` to 0, which turns CoreText's
//!   `trak` off, and the advances HarfBuzz (11.0.0) shaped with came from
//!   Skia's font functions (`SkFont::getWidths` through CoreText), not
//!   HarfBuzz's own, the only ones that apply `trak` in that release;
//! - **the line**: a `TextPainter`'s style has no height, so SkParagraph's
//!   line is the run's metrics (CoreText's `CTFontGetAscent`, `Descent`,
//!   `Leading` of the exact copy at its size), the leading split above and
//!   below (`Run::calculateMetrics`), its height rounded to a whole pixel
//!   (`InternalLineMetrics::height`), and its glyphs drawn on a baseline
//!   half the leading and the ascent below its top, rounded to a whole
//!   pixel too (`TextLine`'s `correctedBaseline`).
//!
//! **What the port draws.** cosmic-text (iced's text engine, vendored in
//! `rust/vendor/cosmic-text`) draws a face at a fixed instance of its axes
//! when told to (`FontSystem::set_face_instance`): metrics, shaping
//! (harfrust's advances, positioning and substitutions) and outlines
//! (swash) all at those coordinates, and, `untracked`, with the shaper
//! reading the face without its `trak` table (harfrust would apply it at
//! 12 pt, the face having a `STAT` table). [`register`] adds a copy of the
//! face to the font system for each weight asked for, under a family of
//! its own ([`family`]), at Flutter's instance of that weight
//! ([`Instance::of`]), untracked. The vendored cosmic-text lays every line
//! out on SkParagraph's whole pixels from its font's ascent, descent and
//! line gap (`crate::ui::core::text`), so a label of the face sits on the
//! line SkParagraph gave it; [`Instance::line`] works that line out from
//! the face's `hhea` metrics and their `MVAR` deltas at the instance, for
//! the text toolbar, which sets its line itself.
//!
//! The instance is worked out from the face's own tables: its named
//! instances at every other axis's default (`wdth`, `GRAD`: CoreText's
//! descriptors at a normal width and grade), each one's weight trait from
//! its `wght` through WebKit's measurements ([`ct_weight`]) and so Skia's
//! CSS weight of it ([`css_weight`]); the first nearest the weight asked
//! for is taken, at the optical size Skia drew it at. At San Francisco's
//! named weights from Light up those are the traits WebKit measured (the
//! labels use w400, w600 and w700; WebKit measured no Thin, which this
//! weighs 166); a face without named instances is CoreText's one
//! descriptor of it, its default instance. On a Mac (CI's runner), the test
//! `every_label_is_drawn_at_the_instance_and_on_the_line_coretext_gave_skia`
//! asks CoreText for Skia's typeface of each label's weight as Skia did,
//! and checks the instance, the glyph advances cosmic-text lays the labels
//! out with and their line against its fonts.
//!
//! Off macOS, or without the face, the labels are in Inter at their
//! weight, on its line.

use std::sync::OnceLock;

use iced::advanced::graphics::text::cosmic_text::fontdb::{
    Database, FaceInfo, Language, Source, ID,
};
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

/// Where macOS keeps its system UI font.
pub const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";

/// CoreText's default size, at which Skia made each typeface, and so the
/// optical size it drew the labels at (see the module docs).
pub const OPTICAL_SIZE: f32 = 12.0;

/// AppKit's `NSFontWeight` constants, `NSFontWeightUltraLight` to
/// `NSFontWeightBlack`, with -1 and 1 at either end: the CoreText weight
/// traits Skia maps onto CSS weights 0, 100, ..., 1000
/// (`SkCTFontGetNSFontWeightMapping`, whose fallback this is; it reads
/// AppKit's own, which the macOS test checks are these).
pub const NS_FONT_WEIGHTS: [f64; 11] = [
    -1.00, -0.80, -0.60, -0.40, 0.00, 0.23, 0.30, 0.40, 0.56, 0.62, 1.00,
];

/// WebKit's measurements of San Francisco's named weights: each one's
/// CoreText weight trait and `wght` (`normalizeCTWeight`'s keyframes,
/// "experimentally gathered from the various named weights of San
/// Francisco", `FontMetricsNormalization.h`).
pub const SAN_FRANCISCO_WEIGHTS: [(f64, f64); 8] = [
    (-0.8, 30.0),
    (-0.4, 274.0),
    (0.0, 400.0),
    (0.23, 510.0),
    (0.3, 590.0),
    (0.4, 700.0),
    (0.56, 860.0),
    (0.62, 1000.0),
];

/// Skia's CSS weight of a CoreText weight trait (`ct_weight_to_fontstyle`
/// for a font CoreText has installed: [`NS_FONT_WEIGHTS`] at 0 to 1000,
/// linearly between, rounded half up; below -1 0, from 1 on 1000).
pub fn css_weight(ct_weight: f64) -> i32 {
    let map = NS_FONT_WEIGHTS;
    if ct_weight < map[0] {
        return 0;
    }
    for (i, pair) in map.windows(2).enumerate() {
        let (low, high) = (pair[0], pair[1]);
        if ct_weight < high {
            let (from, to) = (100.0 * i as f64, 100.0 * (i + 1) as f64);
            let css = from + ((ct_weight - low) * (to - from)) / (high - low);
            // `RoundCGFloatToInt`: `s + 0.5`, truncated.
            return (css + 0.5) as i32;
        }
    }
    1000
}

/// CoreText's weight trait of a named instance of San Francisco at `wght`:
/// [`SAN_FRANCISCO_WEIGHTS`], linearly between them (WebKit's
/// `denormalizeCTWeight`), the first's below them and the last's above.
pub fn ct_weight(wght: f32) -> f64 {
    let wght = f64::from(wght);
    let keys = SAN_FRANCISCO_WEIGHTS;
    if wght < keys[0].1 {
        return keys[0].0;
    }
    for pair in keys.windows(2) {
        let ((ct_low, low), (ct_high, high)) = (pair[0], pair[1]);
        if (low..=high).contains(&wght) {
            return ct_low + (wght - low) / (high - low) * (ct_high - ct_low);
        }
    }
    keys[keys.len() - 1].0
}

/// The CSS value of a weight (`FontWeight.w100` is 100, and so on).
pub fn weight_value(weight: Weight) -> u16 {
    match weight {
        Weight::Thin => 100,
        Weight::ExtraLight => 200,
        Weight::Light => 300,
        Weight::Normal => 400,
        Weight::Medium => 500,
        Weight::Semibold => 600,
        Weight::Bold => 700,
        Weight::ExtraBold => 800,
        Weight::Black => 900,
    }
}

/// The family a weight's instance of the system UI font is added to the
/// font system under (no font of the machine's has it).
pub fn family(weight: Weight) -> &'static str {
    match weight {
        Weight::Thin => ".Vocara System UI 100",
        Weight::ExtraLight => ".Vocara System UI 200",
        Weight::Light => ".Vocara System UI 300",
        Weight::Normal => ".Vocara System UI 400",
        Weight::Medium => ".Vocara System UI 500",
        Weight::Semibold => ".Vocara System UI 600",
        Weight::Bold => ".Vocara System UI 700",
        Weight::ExtraBold => ".Vocara System UI 800",
        Weight::Black => ".Vocara System UI 900",
    }
}

/// The face a label of a weight is set in, and its metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub font: Font,
    /// The instance it is drawn at, and its metrics there (Inter's own,
    /// without axes, for Inter).
    pub instance: Instance,
    /// Whether it is the system UI font (else Inter).
    pub system: bool,
}

impl Face {
    /// SkParagraph's line at `size` pixels ([`Instance::line`]).
    pub fn line(&self, size: f32) -> (f32, f32) {
        self.instance.line(size)
    }
}

/// The face labels of `weight` are set in (see the module docs), chosen
/// the first time it is asked for. Takes the font system's lock the first
/// time: never call it with that lock held.
pub fn face(weight: Weight) -> &'static Face {
    static FACES: [OnceLock<Face>; 9] = [const { OnceLock::new() }; 9];
    let index = usize::from(weight_value(weight) / 100 - 1);
    FACES[index].get_or_init(|| choose(weight))
}

/// [`face`]'s font.
pub fn font(weight: Weight) -> Font {
    face(weight).font
}

/// Inter at `weight` (the bundled face nearest it), its metrics its own.
pub fn inter(weight: Weight) -> Face {
    // `font::FACES`: w400 to w800.
    let index = usize::from(weight_value(weight).clamp(400, 800) / 100 - 4);
    let instance = FontRef::new(font::FACES[index])
        .ok()
        .and_then(|face| Instance::of(&face, weight))
        .expect("the bundled Inter parses");
    Face {
        font: font::inter(weight),
        instance,
        system: false,
    }
}

#[cfg(target_os = "macos")]
fn choose(weight: Weight) -> Face {
    use iced::advanced::graphics::text::font_system;
    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    system_ui_face(fonts.db())
        .and_then(|id| register(fonts, id, weight))
        .unwrap_or_else(|| inter(weight))
}

#[cfg(not(target_os = "macos"))]
fn choose(weight: Weight) -> Face {
    inter(weight)
}

/// The system UI face the font system loaded from [`SYSTEM_UI_FONT`], if
/// any.
pub fn system_ui_face(db: &Database) -> Option<ID> {
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

/// Adds a copy of the face `id` to `fonts` under `weight`'s [`family`],
/// drawn at Flutter's instance of it for that weight, untracked: the face
/// of that weight.
pub fn register(fonts: &mut FontSystem, id: ID, weight: Weight) -> Option<Face> {
    let info = fonts.db().face(id)?.clone();
    let instance = fonts
        .db()
        .with_face_data(id, |data, index| {
            Instance::of(&FontRef::from_index(data, index).ok()?, weight)
        })
        .flatten()?;
    let family = family(weight);
    let copy = FaceInfo {
        families: vec![(family.to_string(), Language::English_UnitedStates)],
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
            features: Vec::new(),
        }),
    );
    Some(Face {
        font: Font {
            family: Family::Name(family),
            weight: Weight::Normal,
            stretch: Stretch::Normal,
            style: Style::Normal,
        },
        instance,
        system: true,
    })
}

/// The instance of a face Flutter drew a weight at, and its line metrics
/// there.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// Its coordinate on each of the face's axes (`fvar` order), in user
    /// units: the named instance's, `opsz` at [`OPTICAL_SIZE`] clamped to
    /// its axis.
    pub coords: Vec<(Tag, f32)>,
    /// Skia's CSS weight of the named instance it is (its descriptor's
    /// style: [`css_weight`] of [`ct_weight`] of its `wght`), if the face
    /// has named instances.
    pub skia_weight: Option<i32>,
    pub units_per_em: u16,
    /// `hhea`'s ascender, |descender| and line gap, with their `MVAR`
    /// deltas there (`hasc`, `hdsc`, `hlgp`, unrounded), in font units:
    /// what CoreText reports (the macOS test checks it).
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
}

impl Instance {
    /// Flutter's instance of `face` for `weight` (see the module docs).
    pub fn of(face: &FontRef<'_>, weight: Weight) -> Option<Self> {
        let axes = face.axes();
        let wanted = i32::from(weight_value(weight));
        // CoreText's descriptors of the face at a normal width and grade:
        // its named instances at every axis's default but `wght`'s and
        // `opsz`'s, each with Skia's CSS weight of it.
        let named: Vec<(Vec<f32>, i32)> = face
            .named_instances()
            .iter()
            .filter_map(|instance| {
                let coords: Vec<f32> = instance.user_coords().collect();
                let mut wght = None;
                for (axis, value) in axes.iter().zip(&coords) {
                    match &axis.tag().to_be_bytes() {
                        b"wght" => wght = Some(*value),
                        b"opsz" => {}
                        _ if (value - axis.default_value()).abs() > 1e-3 => return None,
                        _ => {}
                    }
                }
                let css = css_weight(ct_weight(wght.unwrap_or(400.0)));
                Some((coords, css))
            })
            .collect();
        // `findMatchingDesc`: the first of the least `compute_metric`
        // (their widths and slants alike, the weights' difference squared).
        let chosen = named
            .iter()
            .min_by_key(|(_, css)| (css - wanted).pow(2))
            .cloned();
        let skia_weight = chosen.as_ref().map(|(_, css)| *css);
        let at: Vec<f32> = match chosen {
            Some((coords, _)) => coords,
            // Its one descriptor: the default instance.
            None => axes.iter().map(|axis| axis.default_value()).collect(),
        };
        let coords: Vec<(Tag, f32)> = axes
            .iter()
            .zip(at)
            .map(|(axis, value)| {
                let value = match &axis.tag().to_be_bytes() {
                    b"opsz" => OPTICAL_SIZE,
                    _ => value,
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
            skia_weight,
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

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod core_text;
#[cfg(test)]
pub(crate) mod made_up;
#[cfg(test)]
pub(crate) mod tests;
