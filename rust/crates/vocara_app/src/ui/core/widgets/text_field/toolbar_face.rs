//! The face the text toolbar's labels are set in ([`label_face`]): on
//! macOS the system UI font at the instance Flutter drew it at, else the
//! bundled Inter; and the labels' line.
//!
//! **What Flutter drew.** The labels' style (`_kToolbarButtonFontStyle`)
//! set `inherit: false`, 14 px, w400 and no family, so the engine gave the
//! paragraph its default family, the system UI font, and Skia drew it as
//! it drew every `TextPainter`'s text
//! ([`system_font`](crate::ui::core::system_font), whose module docs have
//! the Skia and CoreText steps and their sources): w400 matched to the
//! face's Regular named instance (`wght` 400), at the 12 pt optical size
//! (clamped to the axis), untracked; the labels' -0.15 px letter spacing is
//! the style's own. Their line is SkParagraph's for a style without a
//! height: the run's ascent, descent and line gap at 14 pt (CoreText's, of
//! that instance), the line gap split above and below
//! (`Run::calculateMetrics`), the height rounded to a whole pixel
//! (`InternalLineMetrics::height`), the glyphs drawn on a baseline half the
//! line gap and the ascent below its top, rounded too (`TextLine`'s
//! `correctedBaseline`).
//!
//! **What the port draws.** The labels are set in
//! `system_font::face(Weight::Normal)`, and their line taken from that
//! face's instance at 14 px ([`system_font::Instance::line`]): an item is
//! that line and its padding tall (`toolbar::item_height`), and each label's
//! buffer is drawn with its baseline where this line puts it
//! (`toolbar::label`).
//!
//! Off macOS, or without the face, the labels are in Inter, their line set
//! the same way from its metrics. On a Mac (CI's runner), the test
//! `the_labels_are_drawn_at_the_instance_and_metrics_coretext_gave_skia`
//! asks CoreText for Skia's fonts as Skia made them and checks the
//! instance, the glyph advances cosmic-text lays the labels out with and
//! the line against them (`system_font`'s check, for 14 px, w400).

use std::sync::OnceLock;

use iced::font::Weight;
use iced::Font;

use crate::ui::core::system_font::{self, Face};

use super::toolbar::LABEL_SIZE;

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

impl LabelFace {
    /// The labels set in `face`, on its line at [`LABEL_SIZE`].
    pub fn of(face: &Face) -> Self {
        let (line_height, baseline) = face.line(LABEL_SIZE);
        Self {
            font: face.font,
            line_height,
            baseline,
            system: face.system,
        }
    }
}

/// The labels' face (see the module docs), chosen the first time it is
/// asked for. Takes the font system's lock the first time: never call it
/// with that lock held.
pub fn label_face() -> &'static LabelFace {
    static FACE: OnceLock<LabelFace> = OnceLock::new();
    FACE.get_or_init(|| LabelFace::of(system_font::face(Weight::Normal)))
}

/// Inter (the bundled regular face), its line set from its metrics.
pub fn inter() -> LabelFace {
    LabelFace::of(&system_font::inter(Weight::Normal))
}

#[cfg(test)]
mod tests;
