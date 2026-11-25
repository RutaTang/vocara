// SPDX-License-Identifier: MIT OR Apache-2.0

use core::fmt::Display;

use crate::{math, CacheKey, CacheKeyFlags, Color};
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

#[cfg(not(feature = "std"))]
use core_maths::CoreFloat;

/// A laid out glyph
#[derive(Clone, Debug)]
pub struct LayoutGlyph {
    /// Start index of cluster in original line
    pub start: usize,
    /// End index of cluster in original line
    pub end: usize,
    /// Font size of the glyph
    pub font_size: f32,
    /// Font weight of the glyph
    pub font_weight: fontdb::Weight,
    /// Line height of the glyph, will override buffer setting
    pub line_height_opt: Option<f32>,
    /// Font id of the glyph
    pub font_id: fontdb::ID,
    /// Font id of the glyph
    pub glyph_id: u16,
    /// X offset of hitbox
    pub x: f32,
    /// Y offset of hitbox
    pub y: f32,
    /// Width of hitbox
    pub w: f32,
    /// Unicode `BiDi` embedding level, character is left-to-right if `level` is divisible by 2
    pub level: unicode_bidi::Level,
    /// X offset in line
    ///
    /// If you are dealing with physical coordinates, use [`Self::physical`] to obtain a
    /// [`PhysicalGlyph`] for rendering.
    ///
    /// This offset is useful when you are dealing with logical units and you do not care or
    /// cannot guarantee pixel grid alignment. For instance, when you want to use the glyphs
    /// for vectorial text, apply linear transformations to the layout, etc.
    pub x_offset: f32,
    /// Y offset in line
    ///
    /// If you are dealing with physical coordinates, use [`Self::physical`] to obtain a
    /// [`PhysicalGlyph`] for rendering.
    ///
    /// This offset is useful when you are dealing with logical units and you do not care or
    /// cannot guarantee pixel grid alignment. For instance, when you want to use the glyphs
    /// for vectorial text, apply linear transformations to the layout, etc.
    pub y_offset: f32,
    /// Optional color override
    pub color_opt: Option<Color>,
    /// Metadata from `Attrs`
    pub metadata: usize,
    /// [`CacheKeyFlags`]
    pub cache_key_flags: CacheKeyFlags,
}

#[derive(Clone, Debug)]
pub struct PhysicalGlyph {
    /// Cache key, see [`CacheKey`]
    pub cache_key: CacheKey,
    /// Integer component of X offset in line
    pub x: i32,
    /// Integer component of Y offset in line
    pub y: i32,
}

impl LayoutGlyph {
    pub fn physical(&self, offset: (f32, f32), scale: f32) -> PhysicalGlyph {
        let x_offset = self.font_size * self.x_offset;
        let y_offset = self.font_size * self.y_offset;

        let (cache_key, x, y) = CacheKey::new(
            self.font_id,
            self.glyph_id,
            self.font_size * scale,
            (
                (self.x + x_offset).mul_add(scale, offset.0),
                // vocara: the origin rounded to the nearest device pixel
                // (half up), as Skia places a glyph whose baseline is not
                // drawn at subpixels (`SkGlyphPositionRoundingSpec`: half a
                // pixel added, then floored); upstream truncates.
                math::floorf((self.y - y_offset).mul_add(scale, offset.1) + 0.5), // Hinting in Y axis
            ),
            self.font_weight,
            self.cache_key_flags,
        );

        PhysicalGlyph { cache_key, x, y }
    }
}

/// A line of laid out glyphs
#[derive(Clone, Debug)]
pub struct LayoutLine {
    /// Width of the line
    pub w: f32,
    /// Maximum ascent of the glyphs in line
    pub max_ascent: f32,
    /// Maximum descent of the glyphs in line
    pub max_descent: f32,
    /// Maximum line height of any spans in line
    pub line_height_opt: Option<f32>,
    /// Glyphs in line
    pub glyphs: Vec<LayoutGlyph>,
    /// vocara: the metrics of the line's runs, one for each change of
    /// font, size or line height along it, in visual order; for a line
    /// without glyphs, its default font's ([`BufferLine::layout`]). Its
    /// box is SkParagraph's ([`LayoutLine::line_box`]).
    ///
    /// [`BufferLine::layout`]: crate::BufferLine::layout
    pub runs: Vec<RunMetrics>,
}

/// vocara: SkParagraph's `SK_ScalarNearlyZero`, below which a height
/// multiplier is no height at all (`Run::calculateMetrics`).
const NEARLY_ZERO: f32 = 1.0 / 4096.0;

/// vocara: a font's ascent and descent above and below the baseline
/// (positive) and its units per em, in its units, as it is shaped with
/// (`hhea`'s for the app's faces, which CoreText gave Skia too).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FontVerticals {
    pub ascent: f32,
    pub descent: f32,
    pub units_per_em: f32,
}

impl FontVerticals {
    /// The verticals of a font's metrics (`Font::metrics`).
    pub fn of(metrics: &skrifa::metrics::Metrics) -> Self {
        Self {
            ascent: metrics.ascent,
            descent: -metrics.descent,
            units_per_em: f32::from(metrics.units_per_em),
        }
    }

    /// The ascent and descent at `size` pixels as CoreText gave them to
    /// Skia (`CTFontGetAscent`, `CTFontGetDescent`): scaled in `double`,
    /// then made `float`s. (Scaled in `f32` from a ratio, their last bit
    /// can differ, which decides which way SkParagraph rounds a line whose
    /// ascent and descent add up to exactly half a pixel.)
    pub fn at(&self, size: f32) -> (f32, f32) {
        let scale =
            |units: f32| (f64::from(units) * f64::from(size) / f64::from(self.units_per_em)) as f32;
        (scale(self.ascent), scale(self.descent))
    }
}

/// vocara: one run of a line as SkParagraph (the text layout of Flutter's
/// engine) measures it: its font's ascent and descent at its size, and the
/// line height its style asks for. A run of the buffer's line height with a
/// height of zero (or nearly: `SK_ScalarNearlyZero` times its size) asks
/// for none: SkParagraph's text style without a `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunMetrics {
    /// The run's font size, in pixels.
    pub font_size: f32,
    /// Its font's ascent above the baseline at that size, in pixels.
    pub ascent: f32,
    /// Its font's descent below the baseline at that size, in pixels.
    pub descent: f32,
    /// The line height its attributes set ([`crate::Attrs::metrics`]),
    /// if any; else the buffer's.
    pub line_height_opt: Option<f32>,
}

impl RunMetrics {
    /// The run's ascent and descent in a line `line_height` pixels tall
    /// (positive), as SkParagraph's `Run::calculateMetrics` makes them:
    /// the font's own without a height, else scaled together until they
    /// add up to it (`TextLeadingDistribution.proportional`, Flutter's
    /// default). The font's line gap, which it would split above and
    /// below, is taken as zero.
    pub fn skparagraph(&self, line_height: f32) -> (f32, f32) {
        // `fCorrectAscent`, `fCorrectDescent` (an ascent is negative).
        let (mut ascent, mut descent) = (-self.ascent, self.descent);
        // `fHeightMultiplier * fFont.getSize()` is the line height.
        let multiplier = line_height / self.font_size;
        if !(multiplier.abs() > NEARLY_ZERO) {
            return (-ascent, descent);
        }
        let intrinsic = descent - ascent;
        let scale = line_height / intrinsic;
        ascent *= scale;
        descent *= scale;
        (-ascent, descent)
    }

    /// The line an empty paragraph of this run's font lays out as, in a
    /// line height of `line_height`: SkParagraph's empty metrics
    /// (`ParagraphImpl::computeEmptyMetrics`), the font's own scaled by
    /// the height over their sum *rounded*, where a run scales them by the
    /// height over their sum.
    pub fn skparagraph_empty(&self, line_height: f32) -> LineBox {
        let (mut ascent, mut descent) = (-self.ascent, self.descent);
        let multiplier = line_height / self.font_size;
        if multiplier.abs() > NEARLY_ZERO {
            let intrinsic = LineBox::rounded(ascent, descent);
            let scale = line_height / intrinsic;
            ascent *= scale;
            descent *= scale;
        }
        LineBox::of(-ascent, descent)
    }
}

/// vocara: a line as SkParagraph lays it out: a whole number of pixels
/// tall, its glyphs drawn on a baseline a whole number of pixels below its
/// top. A paragraph's lines lie one under the other, so each line's top is
/// a whole pixel too, and the paragraph is as tall as its lines.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineBox {
    /// Its height: its ascent and descent, rounded to the nearest pixel
    /// (half away from zero: `InternalLineMetrics::height`).
    pub height: f32,
    /// How far below its top its glyphs' baseline lies: its ascent,
    /// rounded to the nearest pixel, half up (`TextLine`'s
    /// `correctedBaseline`).
    pub baseline: f32,
    /// Its ascent above the baseline, unrounded: the largest of its runs'.
    pub ascent: f32,
    /// Its descent below the baseline, unrounded: the largest of its runs'.
    pub descent: f32,
}

impl LineBox {
    /// A line of the given ascent and descent (positive).
    pub fn of(ascent: f32, descent: f32) -> Self {
        Self {
            height: Self::rounded(-ascent, descent),
            baseline: (f64::from(ascent) + 0.5).floor() as f32,
            ascent,
            descent,
        }
    }

    /// `InternalLineMetrics::height` of a line whose ascent is `ascent`
    /// (negative) and descent `descent`: `::round((double)fDescent -
    /// fAscent + fLeading)`, its leading zero.
    fn rounded(ascent: f32, descent: f32) -> f32 {
        (f64::from(descent) - f64::from(ascent)).round() as f32
    }
}

impl LayoutLine {
    /// vocara: the line as SkParagraph lays it out ([`LineBox`]) when the
    /// buffer's line height is `line_height` (a run that sets none of its
    /// own is in it), `empty_paragraph` when it is the only line of a
    /// buffer without text. Its ascent and descent are its runs' largest
    /// ([`RunMetrics::skparagraph`], `InternalLineMetrics::add`); an empty
    /// paragraph's are its font's empty metrics
    /// ([`RunMetrics::skparagraph_empty`]). A line without runs (no font
    /// to measure it by) is the line height, rounded, tall.
    pub fn line_box(&self, line_height: f32, empty_paragraph: bool) -> LineBox {
        let height_of = |run: &RunMetrics| run.line_height_opt.unwrap_or(line_height);
        if empty_paragraph && self.glyphs.is_empty() {
            if let Some(run) = self.runs.first() {
                return run.skparagraph_empty(height_of(run));
            }
        }
        let mut runs = self.runs.iter();
        let Some(first) = runs.next() else {
            let height = self.line_height_opt.unwrap_or(line_height);
            return LineBox::of(height, 0.0);
        };
        let (mut ascent, mut descent) = first.skparagraph(height_of(first));
        for run in runs {
            let (a, d) = run.skparagraph(height_of(run));
            ascent = ascent.max(a);
            descent = descent.max(d);
        }
        LineBox::of(ascent, descent)
    }
}

/// Wrapping mode
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Wrap {
    /// No wrapping
    None,
    /// Wraps at a glyph level
    Glyph,
    /// Wraps at the word level
    Word,
    /// Wraps at the word level, or fallback to glyph level if a word can't fit on a line by itself
    WordOrGlyph,
}

impl Display for Wrap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::None => write!(f, "No Wrap"),
            Self::Word => write!(f, "Word Wrap"),
            Self::WordOrGlyph => write!(f, "Word Wrap or Character"),
            Self::Glyph => write!(f, "Character"),
        }
    }
}

/// Align or justify
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum Align {
    Left,
    Right,
    Center,
    Justified,
    End,
}

impl Display for Align {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Left => write!(f, "Left"),
            Self::Right => write!(f, "Right"),
            Self::Center => write!(f, "Center"),
            Self::Justified => write!(f, "Justified"),
            Self::End => write!(f, "End"),
        }
    }
}
