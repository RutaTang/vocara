//! iced's `text` widget module, re-exported beside [`text`](super::text) so
//! `use crate::ui::core::text;` brings both, as `use iced::widget::text;`
//! does; and the line every text is laid out on, SkParagraph's.
//!
//! **SkParagraph's lines.** Flutter (3.38.4, drawing with Skia at
//! `ea7cdbc6b986bbefcbac92fa429782e59518510f`) laid every paragraph out
//! with SkParagraph, which puts each line, and the glyphs in it, on whole
//! pixels:
//!
//! - **a run's ascent and descent** (`Run::calculateMetrics`, `Run.cpp`):
//!   its font's at its size, its line gap (CoreText's leading) split half
//!   above and half below. With a `height` (the run's style's
//!   `heightOverride`, `OneLineShaper.cpp`; a multiplier within
//!   `SK_ScalarNearlyZero` of zero counts as none), both are scaled by
//!   `height × fontSize / (ascent + descent)`, so they add up to the
//!   height and keep their proportion: Flutter's default
//!   `TextLeadingDistribution.proportional`, which every text of the app
//!   had (nothing set `even`, and an `Icon`'s `even` puts Lucide's glyphs,
//!   which have no descent, where `proportional` would);
//! - **a line's** (`InternalLineMetrics::add`, `Run.h`): the largest
//!   ascent and the largest descent of its runs;
//! - **its height** (`InternalLineMetrics::height`): their sum, rounded to
//!   the nearest whole pixel (`::round`, half away from zero, in double);
//! - **its baseline** (`TextLine::buildTextBlob`'s `correctedBaseline`,
//!   `TextLine.cpp`): the glyphs are drawn its ascent below its top,
//!   rounded to a whole pixel (`floor(ascent + 0.5)`);
//! - **its top** (`TextWrapper::breakTextIntoLines`): the sum of the
//!   rounded heights above it, so the paragraph is as tall as its lines
//!   (`ParagraphImpl::fHeight`), each on a whole pixel;
//! - **an empty paragraph** (`ParagraphImpl::computeEmptyMetrics`): its
//!   style's font's ascent, descent and line gap, with a height scaled by
//!   the height over their *rounded* sum, then rounded as a line's, its
//!   baseline half the line gap and the ascent below its top;
//! - **a `TextPainter`** (and a `RenderParagraph`) paints the paragraph at
//!   the offset it is given: its first line's top there.
//!
//! The app set no `TextHeightBehavior` and no strut, which would change the
//! first and last lines' ascent and descent, or every line's.
//!
//! The vendored cosmic-text (`rust/vendor/cosmic-text`,
//! `LayoutLine::line_box`) lays every buffer out so, so every text the app
//! draws sits on these lines: iced's `text` (whose height is its lines'),
//! [`studio_text`](super::widgets::studio_text), canvas text and the text
//! fields. A style with no `height` (a `TextPainter`'s, which inherits no
//! theme) is [`NATURAL`]: a line height of zero, as SkParagraph takes a
//! multiplier of zero. A run's ascent, descent and line gap are its font's
//! units scaled to its size in `double` and made `float`s, as CoreText
//! gave them to Skia: their last bit decides which way a line whose ascent
//! and descent add up to exactly half a pixel rounds (12.5 px at 1.4 makes
//! 18 so, 17 from a ratio scaled in `float`). The line gap is `hhea`'s with
//! its `MVAR` delta, as CoreText's leading is, whatever a face's `OS/2`
//! table asks for (Lucide asks for its typographic line gap, 90 units, but
//! CoreText's is `hhea`'s, 0); Inter's and Lucide's are zero, and the
//! text toolbar's system UI font's its own (`text_field::toolbar_face`).
//! Skia then places each glyph's baseline on the nearest device pixel
//! (`SkGlyphPositionRoundingSpec`, `SubRunContainer.cpp`: half a pixel
//! added, then floored), which the vendored cosmic-text's
//! `LayoutGlyph::physical` does too, for both renderers.
//!
//! What is not identical: the metrics are the ones cosmic-text shapes
//! with, the same as CoreText's for the bundled faces (`hhea`'s), but not
//! checked against CoreText's here, off a Mac (the toolbar's system UI
//! font's are, on a Mac: `text_field::toolbar_face`'s macOS test).

use iced::advanced::graphics::text::cosmic_text::{self, Buffer, LineBox, Metrics};
use iced::advanced::graphics::text::{font_system, to_attributes, Version};
use iced::{Font, Pixels};
use std::cell::RefCell;

pub use iced::widget::text::*;

/// A text style without a `height`: its lines are its font's own ascent,
/// descent and line gap tall (SkParagraph's height multiplier of zero), as
/// a `TextPainter`'s text was, which inherits no theme. A line height of
/// zero.
pub const NATURAL: LineHeight = LineHeight::Absolute(Pixels(0.0));

/// The line one line of text in `font` at `size` px and `line_height` lays
/// out on: SkParagraph's (see the module docs), its height and baseline
/// whole pixels, measured by the font's own metrics (whatever glyphs it
/// has). It is the line `TextPainter.preferredLineHeight` measured (a
/// space's), which `RenderEditable` sized a field's lines by. Takes the
/// font system's lock: never call it with that lock held.
pub fn line_box(font: Font, size: f32, line_height: LineHeight) -> LineBox {
    type Measured = (Font, u32, u32, Version, LineBox);
    thread_local! {
        static LINES: RefCell<Vec<Measured>> = const { RefCell::new(Vec::new()) };
    }
    let height = line_height.to_absolute(Pixels(size)).0;
    let measured = |lines: &[Measured], version: Version| {
        lines.iter().find_map(|(f, s, h, v, line)| {
            (*f == font && *s == size.to_bits() && *h == height.to_bits() && *v == version)
                .then_some(*line)
        })
    };
    let version = font_system().read().expect("Read font system").version();
    if let Some(line) = LINES.with(|lines| measured(&lines.borrow(), version)) {
        return line;
    }
    let mut fonts = font_system().write().expect("Write font system");
    let version = fonts.version();
    LINES.with(|lines| {
        if let Some(line) = measured(&lines.borrow(), version) {
            return line;
        }
        let fonts = fonts.raw();
        let mut buffer = Buffer::new(fonts, Metrics::new(size, height));
        // No text: its one line is measured by the font itself
        // (`BufferLine::layout`), as a line of text in it, not as an empty
        // paragraph.
        let shaping = cosmic_text::Shaping::Advanced;
        buffer.set_text(fonts, "", &to_attributes(font), shaping, None);
        let line = buffer
            .lines
            .first()
            .and_then(|line| line.layout_opt())
            .and_then(|layout| layout.first())
            .map(|layout| layout.line_box(height, false))
            .unwrap_or_default();
        let mut lines = lines.borrow_mut();
        lines.retain(|(_, _, _, v, _)| *v == version);
        lines.push((font, size.to_bits(), height.to_bits(), version, line));
        line
    })
}

#[cfg(test)]
mod tests;
