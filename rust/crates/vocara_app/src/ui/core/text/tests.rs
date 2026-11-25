//! The lines every text is laid out on, against SkParagraph's rules (the
//! module docs) written out here from Skia's source, apart from the
//! vendored cosmic-text's patch that applies them.

use std::borrow::Cow;

use iced::advanced::graphics::text::cosmic_text::{
    self, skrifa, Attrs, Buffer, LayoutRun, Metrics,
};
use iced::advanced::graphics::text::font_system;
use iced::alignment::Vertical;
use iced::widget::{canvas, column, row, Space};
use iced::{mouse, Color, Element, Point, Rectangle, Renderer, Size, Theme};

use super::*;
use crate::ui::core::icons;
use crate::ui::core::theme::font;
use crate::ui::core::widgets::animated::testing::Frames;

/// A face's ascent, descent (positive) and units per em, in its units.
type Verticals = (f32, f32, f32);

/// Inter's (`hhea`; every bundled weight's, checked below).
const INTER: Verticals = (2728.0, 680.0, 2816.0);

/// Lucide's: an ascent of an em and no descent (`hhea`, and `OS/2`'s typo
/// metrics, which its `USE_TYPO_METRICS` has a shaper take).
const LUCIDE: Verticals = (1000.0, 0.0, 1000.0);

fn load_fonts() {
    font::install();
    font_system()
        .write()
        .expect("the font system")
        .load_font(Cow::Borrowed(icons::FONT_BYTES));
}

/// A run's `SkFontMetrics` at `size`: CoreText's ascent and descent (the
/// units scaled in `double`), as `float`s.
fn font_metrics(size: f32, (ascent, descent, upem): Verticals) -> (f32, f32) {
    let scale = |units: f32| (f64::from(units) * f64::from(size) / f64::from(upem)) as f32;
    (scale(ascent), scale(descent))
}

/// `Run::calculateMetrics`: the run's `fCorrectAscent` (negative) and
/// `fCorrectDescent`, with `height` its style's height override if any
/// (no line gap).
fn run_metrics(size: f32, height: Option<f32>, verticals: Verticals) -> (f32, f32) {
    let (ascent, descent) = font_metrics(size, verticals);
    let leading = 0.0f32;
    let mut correct_ascent = (f64::from(-ascent) - f64::from(leading) * 0.5) as f32;
    let mut correct_descent = (f64::from(descent) + f64::from(leading) * 0.5) as f32;
    if let Some(multiplier) = height {
        let run_height = multiplier * size;
        let font_intrinsic_height = correct_descent - correct_ascent;
        let multiplier = run_height / font_intrinsic_height;
        correct_ascent *= multiplier;
        correct_descent *= multiplier;
    }
    (correct_ascent, correct_descent)
}

/// `InternalLineMetrics::height` and `TextLine`'s `correctedBaseline` of
/// a line of these runs' metrics (`InternalLineMetrics::add`: the least
/// ascent, the largest descent; its leading zero).
fn line_of(runs: &[(f32, f32)]) -> (f32, f32) {
    let ascent = runs.iter().map(|r| r.0).fold(f32::MAX, f32::min);
    let descent = runs.iter().map(|r| r.1).fold(f32::MIN, f32::max);
    let leading = 0.0f32;
    let height = (f64::from(descent) - f64::from(ascent) + f64::from(leading)).round() as f32;
    let baseline = (f64::from(leading / 2.0 - ascent) + 0.5).floor() as f32;
    (height, baseline)
}

/// SkParagraph's line for one run: its height and baseline.
fn skparagraph(size: f32, height: Option<f32>, verticals: Verticals) -> (f32, f32) {
    line_of(&[run_metrics(size, height, verticals)])
}

/// `ParagraphImpl::computeEmptyMetrics` then `InternalLineMetrics::height`:
/// an empty paragraph's height.
fn skparagraph_empty(size: f32, height: Option<f32>, verticals: Verticals) -> f32 {
    let (ascent, descent) = font_metrics(size, verticals);
    let (mut ascent, mut descent, mut leading) = (-ascent, descent, 0.0f32);
    let rounded = |a: f32, d: f32, l: f32| (f64::from(d) - f64::from(a) + f64::from(l)).round();
    if let Some(multiplier) = height {
        let intrinsic_height = rounded(ascent, descent, leading) as f32;
        let strut_height = multiplier * size;
        let multiplier = strut_height / intrinsic_height;
        ascent *= multiplier;
        descent *= multiplier;
        leading *= multiplier;
    }
    rounded(ascent, descent, leading) as f32
}

fn line_height_of(height: Option<f32>) -> LineHeight {
    height.map_or(NATURAL, LineHeight::Relative)
}

/// `content` laid out in `font` at `size` and `height`, unwrapped.
fn buffer(content: &str, font: Font, size: f32, height: Option<f32>) -> Buffer {
    load_fonts();
    let line_height = line_height_of(height).to_absolute(Pixels(size)).0;
    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    let mut buffer = Buffer::new(fonts, Metrics::new(size, line_height));
    buffer.set_text(
        fonts,
        content,
        &to_attributes(font),
        cosmic_text::Shaping::Advanced,
        None,
    );
    buffer
}

/// Each laid-out line's top, baseline and height.
fn lines(buffer: &Buffer) -> Vec<(f32, f32, f32)> {
    buffer
        .layout_runs()
        .map(|run: LayoutRun<'_>| (run.line_top, run.line_y, run.line_height))
        .collect()
}

const SIZES: [f32; 22] = [
    8.0, 8.5, 9.0, 9.5, 10.0, 10.5, 11.0, 11.5, 12.0, 12.5, 13.0, 13.5, 14.0, 14.5, 15.0, 16.0,
    17.0, 18.0, 20.0, 22.0, 26.0, 32.0,
];

/// No height (a `TextPainter`'s), and every height the Dart styles set.
const HEIGHTS: [Option<f32>; 10] = [
    None,
    Some(1.0),
    Some(1.2),
    Some(1.25),
    Some(1.3),
    Some(1.35),
    Some(1.4),
    Some(1.45),
    Some(1.5),
    Some(2.0),
];

#[test]
fn the_reference_takes_the_bundled_faces_metrics() {
    use skrifa::raw::TableProvider;
    for face in font::FACES {
        let face = skrifa::FontRef::new(face).expect("Inter");
        let hhea = face.hhea().expect("hhea");
        let verticals = (
            f32::from(hhea.ascender().to_i16()),
            -f32::from(hhea.descender().to_i16()),
            f32::from(face.head().expect("head").units_per_em()),
        );
        assert_eq!(verticals, INTER);
        assert_eq!(hhea.line_gap().to_i16(), 0, "no line gap");
    }
    let lucide = skrifa::FontRef::new(icons::FONT_BYTES).expect("Lucide");
    let hhea = lucide.hhea().expect("hhea");
    assert_eq!(
        (
            f32::from(hhea.ascender().to_i16()),
            -f32::from(hhea.descender().to_i16()),
            f32::from(lucide.head().expect("head").units_per_em()),
        ),
        LUCIDE
    );
    assert_eq!(hhea.line_gap().to_i16(), 0, "no line gap");
}

#[test]
fn a_line_is_skparagraphs_for_every_size_and_height() {
    load_fonts();
    for (font, verticals) in [
        (font::REGULAR, INTER),
        (font::SEMIBOLD, INTER),
        (font::BOLD, INTER),
        (icons::FONT, LUCIDE),
    ] {
        for size in SIZES {
            for height in HEIGHTS {
                let line = line_box(font, size, line_height_of(height));
                let expected = skparagraph(size, height, verticals);
                assert_eq!(
                    (line.height, line.baseline),
                    expected,
                    "{font:?} at {size} px, height {height:?}"
                );
                // A line of text in it is the same line.
                let content = if font == icons::FONT {
                    char::from(icons::Icon::Plus).to_string()
                } else {
                    "Hxgy".to_string()
                };
                let laid = buffer(&content, font, size, height);
                let (top, baseline, line_height) = lines(&laid)[0];
                assert_eq!(
                    (top, line_height, baseline - top),
                    (0.0, expected.0, expected.1),
                    "{font:?} at {size} px, height {height:?}"
                );
            }
        }
    }
}

#[test]
fn lines_round_as_skparagraph_rounds_them() {
    // (size, height) → (line, baseline), worked by hand from the rules
    // with Inter's metrics.
    let cases = [
        // A line exactly its size tall, the baseline 0.8 of it down.
        ((14.0, Some(1.0)), (14.0, 11.0)),
        ((20.0, Some(1.0)), (20.0, 16.0)),
        // 19.6, 18.2 and 20.3: rounded to the nearest pixel.
        ((14.0, Some(1.4)), (20.0, 16.0)),
        ((13.0, Some(1.4)), (18.0, 15.0)),
        ((14.0, Some(1.45)), (20.0, 16.0)),
        // On a half pixel the ascent and descent scaled to it fall a hair
        // either side: 19.5 rounds up for 15 at 1.3, down for 13 at 1.5;
        // 10.5 down, 13.5 up.
        ((15.0, Some(1.3)), (20.0, 16.0)),
        ((13.0, Some(1.5)), (19.0, 16.0)),
        ((10.5, Some(1.0)), (10.0, 8.0)),
        ((13.5, Some(1.0)), (14.0, 11.0)),
        // 17.5 rounds up with CoreText's metrics (the units scaled in
        // double), down with a ratio scaled in float: the descent's last
        // bit decides it.
        ((12.5, Some(1.4)), (18.0, 14.0)),
        // No height: the font's own ascent (31/32 em) and descent; 16 px
        // puts the ascent on 15.5, which rounds up.
        ((16.0, None), (19.0, 16.0)),
        ((9.0, None), (11.0, 9.0)),
        ((20.0, None), (24.0, 19.0)),
    ];
    load_fonts();
    for ((size, height), expected) in cases {
        assert_eq!(
            skparagraph(size, height, INTER),
            expected,
            "{size} {height:?}"
        );
        let line = line_box(font::REGULAR, size, line_height_of(height));
        assert_eq!((line.height, line.baseline), expected, "{size} {height:?}");
    }
    // Lucide (an icon at height 1): the em, its baseline at the bottom.
    let icon = line_box(icons::FONT, 16.0, LineHeight::Relative(1.0));
    assert_eq!((icon.height, icon.baseline), (16.0, 16.0));
}

#[test]
fn a_multiplier_within_nearly_zero_of_none_is_none() {
    // `SkScalarNearlyZero(fHeightMultiplier)`: 1/4096 or less.
    load_fonts();
    let natural = skparagraph(16.0, None, INTER);
    for height in [0.0, f32::MIN_POSITIVE, 16.0 / 8192.0] {
        let line = line_box(font::REGULAR, 16.0, LineHeight::Absolute(Pixels(height)));
        assert_eq!((line.height, line.baseline), natural, "{height}");
    }
    let tiny = line_box(
        font::REGULAR,
        16.0,
        LineHeight::Absolute(Pixels(16.0 / 2048.0)),
    );
    assert_eq!(tiny.height, 0.0, "a real, if tiny, height");
}

#[test]
fn lines_lie_on_the_rounded_heights_above_them() {
    // Three lines of 13 px at 1.4 (18.2): 18 each, so the paragraph is 54
    // tall, not 54.6, and each baseline 15 below its line's top.
    let laid = buffer("one\ntwo\nthree", font::REGULAR, 13.0, Some(1.4));
    assert_eq!(
        lines(&laid),
        [(0.0, 15.0, 18.0), (18.0, 33.0, 18.0), (36.0, 51.0, 18.0)]
    );
    // Wrapped lines too.
    load_fonts();
    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    let mut wrapped = Buffer::new(fonts, Metrics::new(12.0, 12.0 * 1.35));
    wrapped.set_size(fonts, Some(60.0), None);
    wrapped.set_text(
        fonts,
        "a few words that wrap onto lines",
        &to_attributes(font::REGULAR),
        cosmic_text::Shaping::Advanced,
        None,
    );
    let (line, baseline) = skparagraph(12.0, Some(1.35), INTER);
    let runs = lines(&wrapped);
    assert!(runs.len() > 2, "{runs:?}");
    for (i, (top, y, height)) in runs.into_iter().enumerate() {
        assert_eq!((top, y - top, height), (i as f32 * line, baseline, line));
    }
}

#[test]
fn an_empty_paragraph_is_measured_by_its_rounded_metrics() {
    load_fonts();
    // 12 px at 1.4: an empty paragraph scales the font's ascent and
    // descent by 16.8 over their sum rounded (15), so 16 tall, where a
    // line of text is 17.
    assert_eq!(skparagraph_empty(12.0, Some(1.4), INTER), 16.0);
    assert_eq!(skparagraph(12.0, Some(1.4), INTER).0, 17.0);
    for size in SIZES {
        for height in HEIGHTS {
            let empty = buffer("", font::REGULAR, size, height);
            let laid = lines(&empty);
            assert_eq!(laid.len(), 1);
            assert_eq!(
                laid[0].2,
                skparagraph_empty(size, height, INTER),
                "{size} {height:?}"
            );
        }
    }
    // An empty line among others is the line break's run: a line's.
    let laid = buffer("a\n\nb", font::REGULAR, 12.0, Some(1.4));
    assert_eq!(
        lines(&laid).iter().map(|l| l.2).collect::<Vec<_>>(),
        [17.0, 17.0, 17.0]
    );
    // iced's text: an empty one is as tall as its empty paragraph.
    let mut ui = iced_test::simulator(Element::<'_, ()>::from(
        column![
            crate::ui::core::text("")
                .size(12)
                .line_height(1.4)
                .font(font::REGULAR),
            crate::ui::core::text("x")
                .size(12)
                .line_height(1.4)
                .font(font::REGULAR),
        ]
        .width(100),
    ));
    let x = ui.find("x").expect("the text").bounds();
    assert_eq!((x.y, x.height), (16.0, 17.0), "{x:?}");
}

#[test]
fn a_line_of_runs_takes_their_largest_ascent_and_descent() {
    // A 20 px run at 1.2 among 12 px text at 1.0, each scaled to its own
    // height (`InternalLineMetrics::add`).
    load_fonts();
    let mut fonts = font_system().write().expect("the font system");
    let fonts = fonts.raw();
    let mut laid = Buffer::new(fonts, Metrics::new(12.0, 12.0));
    let attrs = to_attributes(font::REGULAR);
    let big: Attrs<'_> = attrs.clone().metrics(Metrics::new(20.0, 20.0 * 1.2));
    laid.set_rich_text(
        fonts,
        [
            ("small ", attrs.clone()),
            ("Big", big),
            (" small", attrs.clone()),
        ],
        &attrs,
        cosmic_text::Shaping::Advanced,
        None,
    );
    let expected = line_of(&[
        run_metrics(12.0, Some(1.0), INTER),
        run_metrics(20.0, Some(1.2), INTER),
    ]);
    let run = laid.layout_runs().next().expect("a line");
    assert_eq!((run.line_height, run.line_y), expected);
    assert_eq!(expected, (24.0, 19.0));
}

#[test]
fn a_glyph_is_placed_on_the_nearest_device_pixel() {
    // Skia adds half a pixel and floors (`SkGlyphPositionRoundingSpec`).
    let laid = buffer("H", font::REGULAR, 14.0, Some(1.0));
    let run = laid.layout_runs().next().expect("a line");
    let glyph = &run.glyphs[0];
    for (y, scale, expected) in [
        (10.0, 1.0, 10),
        (10.49, 1.0, 10),
        (10.5, 1.0, 11),
        (10.75, 1.0, 11),
        (5.25, 2.0, 11),
        (-0.5, 1.0, 0),
        (-0.75, 1.0, -1),
    ] {
        assert_eq!(
            glyph.physical((0.0, y * scale), scale).y,
            expected,
            "{y} at {scale}"
        );
    }
}

// ------------------------------------------------------------------ pixels

/// The bright columns of row `y` between `x0` and `x1`: an `H`'s stems.
fn stems(frames: &Frames, y: f32, x0: f32, x1: f32) -> Vec<f32> {
    let mut found = Vec::new();
    let mut x = x0;
    while x < x1 {
        if frames.ink(Point::new(x, y), Point::new(x + 1.0, y + 1.0)) >= 250 {
            found.push(x);
        }
        x += 1.0;
    }
    found
}

/// An `H` (Inter Bold, white on black) found between `x0` and `x1`: its
/// stems fill every row from the one under its cap height to the one above
/// `baseline`, and nothing lies on `baseline`'s row or above its cap.
fn assert_h_on(frames: &Frames, x0: f32, x1: f32, baseline: f32, cap: f32) {
    let found = stems(frames, baseline - 3.0, x0, x1);
    assert!(!found.is_empty(), "an H's stems between {x0} and {x1}");
    for x in found {
        let at = |y: f32| frames.ink(Point::new(x, y), Point::new(x + 1.0, y + 1.0));
        for y in [
            baseline - 1.0,
            baseline - 2.0,
            (baseline - cap).ceil() + 1.0,
        ] {
            assert!(at(y) >= 250, "column {x}, row {y}: {}", at(y));
        }
        for y in [baseline, baseline + 1.0, (baseline - cap).floor() - 1.0] {
            assert!(at(y) <= 8, "column {x}, row {y}: {}", at(y));
        }
    }
}

/// Inter Bold's cap height (2048 units of 2816) at `size`.
fn cap(size: f32) -> f32 {
    size * 2048.0 / 2816.0
}

#[test]
fn a_labels_glyphs_sit_on_skparagraphs_baseline() {
    // 20 px at height 1: a 20 px line, the baseline 16 below its top.
    // Laid out at 10, the H's stems end on row 25. (Centred in the line
    // as cosmic-text put them, unrounded, they reached row 26.) At 30.5,
    // Skia puts the baseline on the nearest device pixel: 47 (iced's
    // renderers truncated the text's origin: 46).
    let h = || {
        crate::ui::core::text("H")
            .size(20)
            .font(font::BOLD)
            .color(Color::WHITE)
    };
    let view = || -> Element<'static, ()> {
        row![
            column![Space::new().height(10), h()].width(40),
            column![Space::new().height(30.5), h()].width(40),
            column![Space::new().height(30.4), h()].width(40),
        ]
        .into()
    };
    let mut frames = Frames::new(Size::new(120.0, 80.0));
    let _ = frames.frame(view(), &[], iced::time::Instant::now());
    assert_h_on(&frames, 0.0, 40.0, 26.0, cap(20.0));
    assert_h_on(&frames, 40.0, 80.0, 47.0, cap(20.0));
    assert_h_on(&frames, 80.0, 120.0, 46.0, cap(20.0));
    // Its box: the line, a whole pixel tall.
    let mut ui = iced_test::simulator(Element::<'_, ()>::from(
        column![
            h(),
            crate::ui::core::text("Hxg")
                .size(13)
                .line_height(1.4)
                .font(font::REGULAR)
        ]
        .width(100),
    ));
    let first = ui.find("H").expect("the H").bounds();
    let second = ui.find("Hxg").expect("the line").bounds();
    assert_eq!((first.y, first.height), (0.0, 20.0));
    assert_eq!((second.y, second.height), (20.0, 18.0));
}

/// A canvas that paints `H` as a `TextPainter` did: no height, its top at
/// (4, 10).
struct Painted;

impl canvas::Program<()> for Painted {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_text(canvas::Text {
            content: "H".to_string(),
            position: Point::new(4.0, 10.0),
            color: Color::WHITE,
            size: Pixels(20.0),
            line_height: NATURAL,
            font: font::BOLD,
            align_y: Vertical::Top,
            ..canvas::Text::default()
        });
        vec![frame.into_geometry()]
    }
}

#[test]
fn a_painted_labels_glyphs_sit_on_its_fonts_own_line() {
    // 20 px without a height: Inter's ascent, 19.375, rounds to 19, so
    // the baseline lies on 29 and the stems end on row 28 (at iced's 1.3
    // the line was 26 tall and its baseline 30).
    let mut frames = Frames::new(Size::new(60.0, 60.0));
    let painted: Element<'_, ()> = canvas::Canvas::new(Painted).width(60).height(60).into();
    let _ = frames.frame(painted, &[], iced::time::Instant::now());
    assert_h_on(&frames, 0.0, 40.0, 29.0, cap(20.0));
    // `studio_text` sets its own buffer on the same lines.
    let spaced: Element<'_, ()> = column![
        Space::new().height(10),
        crate::ui::core::widgets::studio_text::studio_text("H")
            .size(20)
            .font(font::BOLD)
            .letter_spacing(1.0)
            .color(Color::WHITE),
    ]
    .into();
    let mut frames = Frames::new(Size::new(60.0, 60.0));
    let _ = frames.frame(spaced, &[], iced::time::Instant::now());
    assert_h_on(&frames, 0.0, 40.0, 26.0, cap(20.0));
}
