//! Text as Flutter's `Text` set it where iced's `text` widget cannot: with
//! letter spacing (`TextStyle.letterSpacing`), tabular figures
//! (`FontFeature.tabularFigures()`) and the ellipsis overflow
//! (`overflow: TextOverflow.ellipsis`, with or without `maxLines`).
//!
//! iced lays its text out with cosmic-text but exposes none of them. This
//! widget builds the cosmic-text buffer itself, with what iced's paragraph
//! sets (metrics, the font's attributes, wrapping, alignment), plus
//! cosmic-text's own letter spacing and OpenType features, and draws it as a
//! raw buffer (`iced::advanced::graphics::text::Raw`). It always shapes
//! fully (`Shaping::Advanced`), as the whole app does (iced's
//! `advanced-shaping` feature): kerning, ligatures, font fallback and
//! non-Latin scripts, as Flutter shaped every text.
//!
//! - **Letter spacing**: Flutter's paragraph (SkParagraph) adds the spacing
//!   after every glyph, the last one included, so a spaced line is
//!   `spacing × glyphs` wider than a solid one (and a centred one sits half
//!   a spacing left of the solid line's centre). cosmic-text adds it to each
//!   glyph's advance: the same.
//! - **Tabular figures** ([`StudioText::tabular`]): the font's `tnum`
//!   feature, which the bundled Inter faces carry: every digit takes the
//!   same advance, so a number keeps its width as it changes.
//! - **Ellipsis** ([`StudioText::ellipsis`]): SkParagraph's. The text wraps
//!   into at most `max_lines` lines at the width it is given (one line never
//!   wraps). When text is left over, the last line keeps the longest run of
//!   its shaping clusters that fits with "…" after it (`TextLine::
//!   createEllipsis`), and the "…" is set without the spacing, as
//!   SkParagraph shapes it apart. With one line that is Flutter's `maxLines:
//!   1` / `softWrap: false` ellipsis: the whole text when it fits, else the
//!   longest prefix that fits followed by "…". A `Text` with an ellipsis and
//!   no `maxLines` is one line too (SkParagraph ends the paragraph at its
//!   first line then), so `ellipsis(1)` ports it.
//! - **Lines**: SkParagraph's, as every text's (the vendored cosmic-text
//!   lays the buffer out so: [`crate::ui::core::text`]): each a whole
//!   pixel tall, its baseline a whole pixel below its top, at the style's
//!   `height` ([`StudioText::line_height`]; 1 by default, the theme's).
//!
//! Its width is the laid-out text's, as iced's `text`: an ellipsized line
//! as wide as what it shows (at most the width it was given), a spaced one
//! with its trailing spacing. Widget operations see the full text (as a
//! Flutter finder reads `Text.data`), then, when it was cut, the text shown.
//!
//! A canvas label with letter spacing or tabular figures is drawn with
//! [`fill_spaced_text`] / [`fill_tabular_text`].

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use iced::advanced::graphics::text::cosmic_text::{
    self, Attrs, Buffer, FeatureTag, FontFeatures, FontSystem, Metrics, Wrap,
};
use iced::advanced::graphics::text::{
    self as graphics_text, font_system, Raw, Renderer as _, Version,
};
use iced::advanced::layout::{self, Layout};
use iced::advanced::text::{Alignment, LineHeight, Renderer as _, Wrapping};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{renderer, Widget};
use iced::alignment::Vertical;
use iced::widget::canvas::Text;
use iced::{mouse, Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

/// The ellipsis SkParagraph appends (`ParagraphStyle`'s default, U+2026).
pub const ELLIPSIS: &str = "\u{2026}";

/// How far past its width a laid-out line may reach before it overflows:
/// cosmic-text sums advances in `f32`.
const EPSILON: f32 = 0.01;

/// Every text is shaped fully: kerned, as Flutter set it.
const SHAPING: cosmic_text::Shaping = cosmic_text::Shaping::Advanced;

/// `FontFeature.tabularFigures()`.
const TABULAR_FIGURES: FeatureTag = FeatureTag::new(b"tnum");

/// A run of text, styled and laid out as Flutter's `Text` (see the module
/// docs). Build it with [`studio_text`].
pub struct StudioText<'a> {
    content: Cow<'a, str>,
    size: Option<Pixels>,
    line_height: LineHeight,
    font: Option<Font>,
    width: Length,
    height: Length,
    align_x: Alignment,
    align_y: Vertical,
    wrapping: Wrapping,
    color: Option<Color>,
    letter_spacing: f32,
    tabular: bool,
    max_lines: Option<usize>,
}

/// Text with the app's defaults: the renderer's font and size, Flutter's
/// line height under the app's Material theme (1.0, see
/// [`crate::ui::core::text`]), no letter spacing, proportional figures, and
/// no ellipsis (it wraps at words, as iced's does).
pub fn studio_text<'a>(content: impl Into<Cow<'a, str>>) -> StudioText<'a> {
    StudioText {
        content: content.into(),
        size: None,
        line_height: LineHeight::Relative(1.0),
        font: None,
        width: Length::Shrink,
        height: Length::Shrink,
        align_x: Alignment::Default,
        align_y: Vertical::Top,
        wrapping: Wrapping::default(),
        color: None,
        letter_spacing: 0.0,
        tabular: false,
        max_lines: None,
    }
}

impl<'a> StudioText<'a> {
    /// The font size, in logical pixels.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// The line height (`TextStyle.height` is `LineHeight::Relative`).
    pub fn line_height(mut self, line_height: impl Into<LineHeight>) -> Self {
        self.line_height = line_height.into();
        self
    }

    pub fn font(mut self, font: impl Into<Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Centres the text both ways in its bounds.
    pub fn center(self) -> Self {
        self.align_x(Alignment::Center).align_y(Vertical::Center)
    }

    pub fn align_x(mut self, alignment: impl Into<Alignment>) -> Self {
        self.align_x = alignment.into();
        self
    }

    pub fn align_y(mut self, alignment: impl Into<Vertical>) -> Self {
        self.align_y = alignment.into();
        self
    }

    /// How the text wraps when it has no ellipsis (an ellipsized text wraps
    /// at words, breaking a word too long for a line, as Flutter did).
    pub fn wrapping(mut self, wrapping: Wrapping) -> Self {
        self.wrapping = wrapping;
        self
    }

    /// The text's color; the renderer's text color when unset.
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// `TextStyle.letterSpacing`: logical pixels after every glyph.
    pub fn letter_spacing(mut self, spacing: f32) -> Self {
        self.letter_spacing = spacing;
        self
    }

    /// `fontFeatures: [FontFeature.tabularFigures()]`: the font's `tnum`.
    pub fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }

    /// `overflow: TextOverflow.ellipsis` with `maxLines: max_lines` (`1`
    /// for one line, and for an ellipsis with no `maxLines`).
    pub fn ellipsis(mut self, max_lines: usize) -> Self {
        self.max_lines = Some(max_lines.max(1));
        self
    }
}

// ---------------------------------------------------------------- layout

/// What a text is laid out from, besides the room it has.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spec<'s> {
    pub content: &'s str,
    pub size: f32,
    pub line_height: f32,
    pub font: Font,
    pub align_x: Alignment,
    pub wrapping: Wrapping,
    pub letter_spacing: f32,
    pub tabular: bool,
    pub max_lines: Option<usize>,
}

/// A laid-out text: the buffer to draw, its size, and the text it shows
/// when the ellipsis cut it.
#[derive(Debug)]
pub(crate) struct Laid {
    pub buffer: Buffer,
    pub min_bounds: Size,
    pub shown: Option<String>,
}

/// The font attributes of `font` at `size`, with `spacing` px after every
/// glyph (cosmic-text takes it in ems) and, when `tabular`, the `tnum`
/// feature.
fn attributes(font: Font, size: f32, spacing: f32, tabular: bool) -> Attrs<'static> {
    let mut attrs = graphics_text::to_attributes(font);
    if spacing != 0.0 && size > 0.0 {
        attrs = attrs.letter_spacing(spacing / size);
    }
    if tabular {
        let mut features = FontFeatures::new();
        let _ = features.enable(TABULAR_FIGURES);
        attrs = attrs.font_features(features);
    }
    attrs
}

/// A buffer of `spans` with the spec's metrics, in `bounds` (as iced's
/// paragraph, a line starting below the height is not laid out).
fn buffer(
    font_system: &mut FontSystem,
    spec: &Spec<'_>,
    bounds: Size,
    wrap: Wrap,
    spans: &[(&str, Attrs<'static>)],
) -> Buffer {
    let mut buffer = Buffer::new(font_system, Metrics::new(spec.size, spec.line_height));
    buffer.set_size(font_system, Some(bounds.width), Some(bounds.height));
    buffer.set_wrap(font_system, wrap);
    let default_attrs = attributes(spec.font, spec.size, spec.letter_spacing, spec.tabular);
    buffer.set_rich_text(
        font_system,
        spans.iter().map(|(text, attrs)| (*text, attrs.clone())),
        &default_attrs,
        SHAPING,
        None,
    );
    buffer
}

/// One laid-out line: the paragraph (hard line) it is in, its byte range
/// in that paragraph, and its width.
#[derive(Debug, Clone, Copy)]
struct Line {
    paragraph: usize,
    start: usize,
    end: usize,
    width: f32,
}

fn lines(buffer: &Buffer) -> Vec<Line> {
    buffer
        .layout_runs()
        .map(|run| {
            let start = run.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
            let end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(start);
            Line {
                paragraph: run.line_i,
                start,
                end,
                width: run.line_w,
            }
        })
        .collect()
}

/// The shaping clusters of a one-paragraph buffer's glyphs, in logical
/// order: each one's byte range, the x of its leftmost glyph and its
/// advance (its glyphs' advances, spacing included). A cluster's glyphs
/// share their `start`.
fn glyph_clusters(buffer: &Buffer) -> Vec<(Range<usize>, f32, f32)> {
    let mut glyphs: Vec<(usize, usize, f32, f32)> = buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| (g.start, g.end, g.x, g.w)))
        .collect();
    glyphs.sort_by_key(|g| g.0);
    let mut clusters: Vec<(Range<usize>, f32, f32)> = Vec::new();
    for (start, end, x, w) in glyphs {
        match clusters.last_mut() {
            Some(cluster) if cluster.0.start == start => {
                cluster.0.end = cluster.0.end.max(end);
                cluster.1 = cluster.1.min(x);
                cluster.2 += w;
            }
            _ => clusters.push((start..end, x, w)),
        }
    }
    clusters
}

/// The shaping clusters of one unwrapped line, in logical order: each
/// one's byte range and advance.
fn clusters(buffer: &Buffer) -> Vec<(usize, usize, f32)> {
    glyph_clusters(buffer)
        .into_iter()
        .map(|(range, _, advance)| (range.start, range.end, advance))
        .collect()
}

/// The byte length of the longest run of `clusters` (from the first) whose
/// advances fit in `room`: the cut `TextLine::createEllipsis` makes.
pub(crate) fn fitting_prefix(clusters: &[(usize, usize, f32)], room: f32) -> usize {
    let mut used = 0.0;
    let mut cut = 0;
    for &(start, end, advance) in clusters {
        if used + advance > room + EPSILON {
            return start;
        }
        used += advance;
        cut = end;
    }
    cut
}

/// Lays `spec` out in `bounds` (see the module docs).
pub(crate) fn lay_out(font_system: &mut FontSystem, spec: &Spec<'_>, bounds: Size) -> Laid {
    let styled = attributes(spec.font, spec.size, spec.letter_spacing, spec.tabular);
    let finish = |font_system: &mut FontSystem, mut buffer: Buffer, shown: Option<String>| {
        let min_bounds = graphics_text::align(&mut buffer, font_system, spec.align_x);
        Laid {
            buffer,
            min_bounds,
            shown,
        }
    };
    let Some(max_lines) = spec.max_lines.filter(|_| bounds.width.is_finite()) else {
        let wrap = graphics_text::to_wrap(spec.wrapping);
        let plain = buffer(font_system, spec, bounds, wrap, &[(spec.content, styled)]);
        return finish(font_system, plain, None);
    };
    let wrap = if max_lines == 1 {
        Wrap::None
    } else {
        Wrap::WordOrGlyph
    };
    // Every line, however tall the room: whether text is left over does
    // not depend on the height.
    let whole = buffer(
        font_system,
        spec,
        Size::new(bounds.width, f32::INFINITY),
        wrap,
        &[(spec.content, styled.clone())],
    );
    let laid_lines = lines(&whole);
    // Text is left over past the last line, or the one line is too long. (A
    // wrapped line is wider than the room only when one cluster is, which
    // SkParagraph clips without an ellipsis.)
    let overflows = laid_lines.len() > max_lines
        || (max_lines == 1
            && laid_lines
                .first()
                .is_some_and(|line| line.width > bounds.width + EPSILON));
    if !overflows {
        let mut fitted = whole;
        fitted.set_size(font_system, Some(bounds.width), Some(bounds.height));
        return finish(font_system, fitted, None);
    }
    let paragraph_text = |paragraph: usize| {
        whole
            .lines
            .get(paragraph)
            .map(|line| line.text())
            .unwrap_or_default()
    };
    // The lines before the last stay as laid out (each ending where it
    // wrapped); the last runs on with the rest of its paragraph, cut.
    let mut kept = String::new();
    for line in &laid_lines[..max_lines - 1] {
        let text = paragraph_text(line.paragraph);
        kept.push_str(
            text.get(line.start..line.end)
                .unwrap_or_default()
                .trim_end(),
        );
        kept.push('\n');
    }
    let last = laid_lines[max_lines - 1];
    let rest = paragraph_text(last.paragraph)
        .get(last.start..)
        .unwrap_or_default()
        .to_string();
    let solid = attributes(spec.font, spec.size, 0.0, false);
    let ellipsis = buffer(
        font_system,
        spec,
        Size::INFINITE,
        Wrap::None,
        &[(ELLIPSIS, solid.clone())],
    );
    let ellipsis_width = lines(&ellipsis).first().map_or(0.0, |line| line.width);
    let unwrapped = buffer(
        font_system,
        spec,
        Size::INFINITE,
        Wrap::None,
        &[(&rest, styled.clone())],
    );
    let rest_clusters = clusters(&unwrapped);
    let mut cut = fitting_prefix(&rest_clusters, bounds.width - ellipsis_width);
    loop {
        let head = format!("{kept}{}", &rest[..cut]);
        let shown = buffer(
            font_system,
            spec,
            bounds,
            Wrap::None,
            &[(&head, styled.clone()), (ELLIPSIS, solid.clone())],
        );
        // Shaped together, the cut line can kern a hair wider than its
        // clusters summed: give back a cluster until it fits.
        let too_wide = lines(&shown)
            .last()
            .is_some_and(|line| line.width > bounds.width + EPSILON);
        let shorter = rest_clusters.iter().rev().find(|c| c.0 < cut).map(|c| c.0);
        match (too_wide, shorter) {
            (true, Some(shorter)) => cut = shorter,
            _ => return finish(font_system, shown, Some(format!("{head}{ELLIPSIS}"))),
        }
    }
}

/// One unwrapped line of `content` laid out as [`StudioText`] lays it out,
/// for a widget that places its own text: the buffer to draw with
/// [`draw_line`], and its size.
pub(crate) fn line(
    content: &str,
    size: f32,
    line_height: LineHeight,
    font: Font,
    tabular: bool,
) -> (Arc<Buffer>, Size) {
    let spec = Spec {
        content,
        size,
        line_height: line_height.to_absolute(Pixels(size)).0,
        font,
        align_x: Alignment::Default,
        wrapping: Wrapping::None,
        letter_spacing: 0.0,
        tabular,
        max_lines: None,
    };
    let mut font_system = font_system().write().expect("Write font system");
    let laid = lay_out(font_system.raw(), &spec, Size::INFINITE);
    (Arc::new(laid.buffer), laid.min_bounds)
}

/// Draws a [`line`] with its top-left at `position`.
pub(crate) fn draw_line(
    renderer: &mut Renderer,
    buffer: &Arc<Buffer>,
    position: Point,
    color: Color,
    viewport: Rectangle,
) {
    renderer.fill_raw(Raw {
        buffer: Arc::downgrade(buffer),
        position,
        color,
        clip_bounds: viewport,
    });
}

// ---------------------------------------------------------------- canvas

/// `content` set on one line as [`StudioText`] sets it: each shaping
/// cluster's byte range, x and advance, and the line's width.
pub(crate) fn line_clusters(
    font_system: &mut FontSystem,
    spec: &Spec<'_>,
) -> (Vec<(Range<usize>, f32, f32)>, f32) {
    let one_line = Spec {
        wrapping: Wrapping::None,
        max_lines: None,
        ..spec.clone()
    };
    let laid = lay_out(font_system, &one_line, Size::INFINITE);
    (glyph_clusters(&laid.buffer), laid.min_bounds.width)
}

/// `frame.fill_text(text)` with `spacing` px after every glyph
/// (`TextStyle.letterSpacing` on a `TextPainter`): see [`fill_canvas_text`].
pub fn fill_spaced_text(frame: &mut iced::widget::canvas::Frame, text: Text, spacing: f32) {
    fill_canvas_text(frame, text, spacing);
}

/// `frame.fill_text(text)` with tabular figures (`fontFeatures:
/// [FontFeature.tabularFigures()]` on a `TextPainter`). A canvas's text
/// carries no OpenType features, so it is set in a copy of its face whose
/// every run takes `tnum` ([`tabular_font`]): the font's own tabular
/// figures, shaped as HarfBuzz shaped them for SkParagraph.
pub fn fill_tabular_text(frame: &mut iced::widget::canvas::Frame, text: Text) {
    let font = tabular_font(text.font);
    frame.fill_text(Text { font, ..text });
}

/// A font that sets text as `font` does, with tabular figures: a copy of
/// the face `font` resolves to, added to the font system under a family of
/// its own, at the face's instance with the `tnum` feature on every run
/// (cosmic-text's `FaceInstance::features`). One copy per font, made the
/// first time it is asked for; `font` itself if it names no family or the
/// font system has no face for it. Takes the font system's lock: never call
/// it with that lock held.
pub fn tabular_font(font: Font) -> Font {
    use std::sync::Mutex;
    static COPIES: Mutex<Vec<(Font, Font)>> = Mutex::new(Vec::new());

    let mut copies = COPIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, copy)) = copies.iter().find(|(of, _)| *of == font) {
        return *copy;
    }
    let mut fonts = font_system().write().expect("Write font system");
    let copy = tabular_copy(fonts.raw(), font, copies.len()).unwrap_or(font);
    copies.push((font, copy));
    copy
}

/// [`tabular_font`]'s copy of `font`'s face, the `index`th made.
fn tabular_copy(fonts: &mut FontSystem, font: Font, index: usize) -> Option<Font> {
    use cosmic_text::fontdb::{self, FaceInfo, Language, Query};
    use cosmic_text::FaceInstance;

    let iced::font::Family::Name(name) = font.family else {
        return None;
    };
    let weight = fontdb::Weight(crate::ui::core::system_font::weight_value(font.weight));
    let id = fonts.db().query(&Query {
        families: &[fontdb::Family::Name(name)],
        weight,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    })?;
    let info = fonts.db().face(id)?.clone();
    let family: &'static str =
        Box::leak(format!(".Vocara tabular {index} {name}").into_boxed_str());
    let instance = fonts.face_instance(id).cloned().unwrap_or(FaceInstance {
        coords: Vec::new(),
        untracked: false,
        features: Vec::new(),
    });
    let copy = fonts.db_mut().push_face_info(FaceInfo {
        families: vec![(family.to_string(), Language::English_UnitedStates)],
        ..info
    });
    fonts.set_face_instance(
        copy,
        Some(FaceInstance {
            features: vec![(*b"tnum", 1)],
            ..instance
        }),
    );
    Some(Font {
        family: iced::font::Family::Name(family),
        ..font
    })
}

/// `frame.fill_text(text)` set as [`StudioText`] sets it, with `spacing` px
/// after every glyph. A canvas draws its own text, which takes no letter
/// spacing, so the line is laid out here and each cluster drawn where it
/// landed. The alignment is the laid-out line's.
pub fn fill_canvas_text(frame: &mut iced::widget::canvas::Frame, text: Text, spacing: f32) {
    if spacing == 0.0 || text.content.is_empty() {
        frame.fill_text(text);
        return;
    }
    let (clusters, width) = {
        let mut font_system = font_system().write().expect("Write font system");
        let spec = Spec {
            content: &text.content,
            size: text.size.0,
            line_height: text.line_height.to_absolute(text.size).0,
            font: text.font,
            align_x: Alignment::Default,
            wrapping: Wrapping::None,
            letter_spacing: spacing,
            tabular: false,
            max_lines: None,
        };
        line_clusters(font_system.raw(), &spec)
    };
    let left = match text.align_x {
        Alignment::Center => text.position.x - width / 2.0,
        Alignment::Right => text.position.x - width,
        Alignment::Default | Alignment::Left | Alignment::Justified => text.position.x,
    };
    for (range, x, _) in clusters {
        frame.fill_text(Text {
            content: text.content[range].to_string(),
            position: Point::new(left + x, text.position.y),
            align_x: Alignment::Left,
            ..text.clone()
        });
    }
}

// ---------------------------------------------------------------- widget

/// What the last layout was made from.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    content: String,
    bounds: Size,
    size: f32,
    line_height: f32,
    font: Font,
    align_x: Alignment,
    wrapping: Wrapping,
    letter_spacing: f32,
    tabular: bool,
    max_lines: Option<usize>,
    version: Version,
}

/// The widget's tree state: the last layout and what it was made from.
#[derive(Default)]
struct State {
    key: Option<Key>,
    buffer: Option<Arc<Buffer>>,
    min_bounds: Size,
    shown: Option<String>,
}

impl StudioText<'_> {
    fn spec(&self, renderer: &Renderer) -> Spec<'_> {
        let size = self.size.unwrap_or_else(|| renderer.default_size()).0;
        Spec {
            content: &self.content,
            size,
            line_height: self.line_height.to_absolute(Pixels(size)).0,
            font: self.font.unwrap_or_else(|| renderer.default_font()),
            align_x: self.align_x,
            wrapping: self.wrapping,
            letter_spacing: self.letter_spacing,
            tabular: self.tabular,
            max_lines: self.max_lines,
        }
    }
}

impl<M> Widget<M, Theme, Renderer> for StudioText<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let spec = self.spec(renderer);
        layout::sized(limits, self.width, self.height, |limits| {
            let bounds = limits.max();
            let mut font_system = font_system().write().expect("Write font system");
            let key = Key {
                content: spec.content.to_string(),
                bounds,
                size: spec.size,
                line_height: spec.line_height,
                font: spec.font,
                align_x: spec.align_x,
                wrapping: spec.wrapping,
                letter_spacing: spec.letter_spacing,
                tabular: spec.tabular,
                max_lines: spec.max_lines,
                version: font_system.version(),
            };
            if state.key.as_ref() != Some(&key) {
                let laid = lay_out(font_system.raw(), &spec, bounds);
                state.buffer = Some(Arc::new(laid.buffer));
                state.min_bounds = laid.min_bounds;
                state.shown = laid.shown;
                state.key = Some(key);
            }
            state.min_bounds
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let Some(buffer) = &state.buffer else {
            return;
        };
        let position = layout
            .bounds()
            .anchor(state.min_bounds, self.align_x, self.align_y);
        draw_line(
            renderer,
            buffer,
            position,
            self.color.unwrap_or(style.text_color),
            *viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.content);
        if let Some(shown) = &tree.state.downcast_ref::<State>().shown {
            operation.text(None, layout.bounds(), shown);
        }
    }
}

impl<'a, M: 'a> From<StudioText<'a>> for Element<'a, M> {
    fn from(text: StudioText<'a>) -> Self {
        Element::new(text)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! Loading the app's faces into the font system every test shares, and
    //! laying text out with them, for tests that measure text.

    use super::*;
    use crate::ui::core::theme::font;

    /// Loads Inter (every weight the app bundles); loading twice is a no-op.
    pub(crate) fn load_fonts() {
        font::install();
    }

    /// A one-line spec of `content` in Inter at `size` and `weight`.
    pub(crate) fn spec(content: &str, size: f32, font: Font) -> Spec<'_> {
        Spec {
            content,
            size,
            line_height: LineHeight::default().to_absolute(Pixels(size)).0,
            font,
            align_x: Alignment::Default,
            wrapping: Wrapping::None,
            letter_spacing: 0.0,
            tabular: false,
            max_lines: None,
        }
    }

    /// `spec` laid out in `width` (unbounded height).
    pub(crate) fn laid(spec: &Spec<'_>, width: f32) -> Laid {
        load_fonts();
        let mut font_system = font_system().write().expect("Write font system");
        lay_out(font_system.raw(), spec, Size::new(width, f32::INFINITY))
    }
}

#[cfg(test)]
mod tests;
