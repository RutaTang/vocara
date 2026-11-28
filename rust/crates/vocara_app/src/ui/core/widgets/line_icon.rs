//! Line icons drawn to `DESIGN.md`'s icon rules: round caps and joins, a
//! 24-unit grid shown in a 16 px box (15 in dense toolbars, 10 to 14 inside
//! small buttons), and one **optical size** across a row.
//!
//! The optical-size rule: every glyph's visible extent fills about the same
//! area (17 of the grid's 24 units: about 11 px in a 16 px box) and every
//! stroke is the same width on screen ([`STROKE`], 1.2 px). A glyph drawn
//! to its own grid takes that rule through two settings:
//!
//! - its [`Glyph::scale`]: a glyph that fills the whole grid (the settings
//!   gear, 22 units across) is drawn at 75 % about the box's centre, and its
//!   stroke is *not* scaled with it (so it is thickened by the same factor
//!   relative to the glyph: it stays [`STROKE`] wide on screen);
//! - its path data: a sparse glyph is lengthened in its own data (the plus
//!   and minus span 3.5 to 20.5, 17 units, where Lucide's span 14).
//!
//! The stroke is set in screen pixels, not grid units, so the same glyph at
//! 10 px and at 16 px draws the same line weight (the mockups' 2.6-unit
//! plus at 10 px and 1.8-unit plus at 16 px are both about 1.2 px).
//!
//! The Lucide font ([`crate::ui::core::icons`]) stays for the glyphs this set
//! does not draw; its strokes scale with its size (2 units of 24), so a row
//! mixing the two should keep the font's at about 14 px.
//!
//! Glyphs are SVG path data in the 24-unit grid ([`parse`] reads the
//! commands `M L H V C S Q T A Z`, absolute and relative); circles and
//! rounded rectangles are written as arcs.

use std::cell::RefCell;

use iced::advanced::graphics::geometry::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Tree};
use iced::advanced::{renderer, Renderer as _, Widget};
use iced::widget::canvas::{self, Frame, LineCap, LineJoin, Path, Stroke};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

/// The stroke width every line icon draws with, on screen.
pub const STROKE: f32 = 1.2;

/// The icon box in a toolbar.
pub const TOOLBAR_SIZE: f32 = 16.0;
/// The icon box in a dense toolbar (a toolbar group's buttons).
pub const DENSE_SIZE: f32 = 15.0;
/// The icon box inside a 24 px panel button.
pub const PANEL_SIZE: f32 = 14.0;

/// The grid glyphs are drawn in.
pub const GRID: f32 = 24.0;

/// One part of a glyph: path data stroked, or filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Stroke(&'static str),
    Fill(&'static str),
}

/// A glyph: its parts in the 24-unit grid and its optical scale (see the
/// module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub name: &'static str,
    pub parts: &'static [Part],
    /// How much of its own grid the glyph is drawn at, about the centre
    /// (1 for most; 0.75 for a glyph filling the grid).
    pub scale: f32,
}

impl Glyph {
    const fn new(name: &'static str, parts: &'static [Part]) -> Self {
        Glyph {
            name,
            parts,
            scale: 1.0,
        }
    }

    const fn scaled(name: &'static str, parts: &'static [Part], scale: f32) -> Self {
        Glyph { name, parts, scale }
    }

    /// The glyph's outline bounds in its grid, after its optical scale (the
    /// path's control points included, its stroke not).
    pub fn extent(&self) -> Rectangle {
        let mut bounds: Option<(Point, Point)> = None;
        let mut take = |p: Point| {
            bounds = Some(match bounds {
                None => (p, p),
                Some((lo, hi)) => (
                    Point::new(lo.x.min(p.x), lo.y.min(p.y)),
                    Point::new(hi.x.max(p.x), hi.y.max(p.y)),
                ),
            });
        };
        let place = Placement::grid(self.scale);
        for part in self.parts {
            let d = match part {
                Part::Stroke(d) | Part::Fill(d) => d,
            };
            for p in points(d) {
                take(place.map(p));
            }
        }
        let (lo, hi) = bounds.unwrap_or((Point::ORIGIN, Point::ORIGIN));
        Rectangle::new(lo, Size::new(hi.x - lo.x, hi.y - lo.y))
    }
}

/// The glyphs the app's toolbars and panels draw (`DESIGN.md`'s mockups).
pub mod glyphs {
    use super::{Glyph, Part::Fill, Part::Stroke};

    /// "New practice", "New take": lengthened to 17 units.
    pub const PLUS: Glyph = Glyph::new("plus", &[Stroke("M12 3.5v17M3.5 12h17")]);
    /// "Zoom out", a minus: lengthened to 17 units.
    pub const MINUS: Glyph = Glyph::new("minus", &[Stroke("M3.5 12h17")]);
    /// The technique library: three volumes, the last leaning.
    pub const TECHNIQUES: Glyph = Glyph::new(
        "techniques",
        &[Stroke("M5 3.5v17M10 3.5v17M14.6 4.2l4.9 16.1")],
    );
    /// Settings: the gear, which fills its grid, at 75 %.
    pub const SETTINGS: Glyph = Glyph::scaled(
        "settings",
        &[
            Stroke("M9 12a3 3 0 1 0 6 0a3 3 0 1 0-6 0z"),
            Stroke(
                "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06\
                 a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09\
                 A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83\
                 l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09\
                 A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83\
                 l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09\
                 a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83\
                 l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09\
                 a1.65 1.65 0 0 0-1.51 1z",
            ),
        ],
        0.75,
    );
    /// Back.
    pub const CHEVRON_LEFT: Glyph = Glyph::new("chevron-left", &[Stroke("M15 18l-6-6 6-6")]);
    pub const CHEVRON_RIGHT: Glyph = Glyph::new("chevron-right", &[Stroke("M9 6l6 6-6 6")]);
    pub const CHEVRON_UP: Glyph = Glyph::new("chevron-up", &[Stroke("M6 15l6-6 6 6")]);
    pub const CHEVRON_DOWN: Glyph = Glyph::new("chevron-down", &[Stroke("M6 9l6 6 6-6")]);
    /// A pop-up button's up-down chevron.
    pub const UP_DOWN: Glyph = Glyph::new("up-down", &[Stroke("M7 9l5-5 5 5M7 15l5 5 5-5")]);
    /// An effect's power button.
    pub const POWER: Glyph = Glyph::new(
        "power",
        &[Stroke("M12 3v8"), Stroke("M6.3 7.3a8 8 0 1 0 11.4 0")],
    );
    /// A row's "⋯" menu.
    pub const MORE: Glyph = Glyph::new(
        "more",
        &[Fill(
            "M3.2 12a1.8 1.8 0 1 0 3.6 0a1.8 1.8 0 1 0-3.6 0z\
             M10.2 12a1.8 1.8 0 1 0 3.6 0a1.8 1.8 0 1 0-3.6 0z\
             M17.2 12a1.8 1.8 0 1 0 3.6 0a1.8 1.8 0 1 0-3.6 0z",
        )],
    );
    /// Transport: play, go to start, stop (filled).
    pub const PLAY: Glyph = Glyph::new("play", &[Fill("M7 4.5v15l12.5-7.5z")]);
    pub const PAUSE: Glyph = Glyph::new("pause", &[Fill("M6.5 5h3.5v14h-3.5zM14 5h3.5v14h-3.5z")]);
    pub const GO_TO_START: Glyph =
        Glyph::new("go-to-start", &[Fill("M6 5h2.2v14H6zM20 5v14L9.5 12z")]);
    pub const STOP: Glyph = Glyph::new(
        "stop",
        &[Fill(
            "M6.5 5h11a1.5 1.5 0 0 1 1.5 1.5v11a1.5 1.5 0 0 1-1.5 1.5h-11\
             a1.5 1.5 0 0 1-1.5-1.5v-11a1.5 1.5 0 0 1 1.5-1.5z",
        )],
    );
    /// Loop.
    pub const LOOP: Glyph = Glyph::new(
        "loop",
        &[
            Stroke("M17 2.5l3.5 3.5-3.5 3.5"),
            Stroke("M3.5 11.5v-1A4.5 4.5 0 0 1 8 6h12.5"),
            Stroke("M7 21.5L3.5 18 7 14.5"),
            Stroke("M20.5 12.5v1A4.5 4.5 0 0 1 16 18H3.5"),
        ],
    );
    /// Follow the playhead: a target.
    pub const FOLLOW: Glyph = Glyph::new(
        "follow",
        &[
            Stroke("M5 12a7 7 0 1 0 14 0a7 7 0 1 0-14 0z"),
            Fill("M10 12a2 2 0 1 0 4 0a2 2 0 1 0-4 0z"),
            Stroke("M12 2v3M12 19v3M2 12h3M19 12h3"),
        ],
    );
    /// Input monitoring: headphones.
    pub const MONITOR: Glyph = Glyph::new(
        "monitor",
        &[
            Stroke("M4 15v-3a8 8 0 0 1 16 0v3"),
            Stroke(
                "M5 14h1a1.5 1.5 0 0 1 1.5 1.5v3.5a1.5 1.5 0 0 1-1.5 1.5h-1\
                 a1.5 1.5 0 0 1-1.5-1.5v-3.5a1.5 1.5 0 0 1 1.5-1.5z",
            ),
            Stroke(
                "M18 14h1a1.5 1.5 0 0 1 1.5 1.5v3.5a1.5 1.5 0 0 1-1.5 1.5h-1\
                 a1.5 1.5 0 0 1-1.5-1.5v-3.5a1.5 1.5 0 0 1 1.5-1.5z",
            ),
        ],
    );
    /// Hide or show the left panel (the inspector).
    pub const PANEL_LEFT: Glyph = Glyph::new(
        "panel-left",
        &[
            Stroke(
                "M5.5 4h13a2.5 2.5 0 0 1 2.5 2.5v11a2.5 2.5 0 0 1-2.5 2.5h-13\
                 a2.5 2.5 0 0 1-2.5-2.5v-11a2.5 2.5 0 0 1 2.5-2.5z",
            ),
            Stroke("M9 4v16"),
        ],
    );
    /// Hide or show the right panel.
    pub const PANEL_RIGHT: Glyph = Glyph::new(
        "panel-right",
        &[
            Stroke(
                "M5.5 4h13a2.5 2.5 0 0 1 2.5 2.5v11a2.5 2.5 0 0 1-2.5 2.5h-13\
                 a2.5 2.5 0 0 1-2.5-2.5v-11a2.5 2.5 0 0 1 2.5-2.5z",
            ),
            Stroke("M15 4v16"),
        ],
    );
    /// The mixer: three faders.
    pub const MIXER: Glyph = Glyph::new(
        "mixer",
        &[
            Stroke("M6 4v16M12 4v16M18 4v16"),
            Stroke("M4 14h4M10 8h4M16 12h4"),
        ],
    );
    /// A note (a sheet with two lines).
    pub const NOTE: Glyph = Glyph::new(
        "note",
        &[Stroke("M5 3h10l4 4v14H5z"), Stroke("M9 12h6M9 16h4")],
    );
    /// An analysis lane (a pulse).
    pub const PULSE: Glyph = Glyph::new("pulse", &[Stroke("M3 12h3l2-5 4 10 3-7 2 2h4")]);

    /// Every glyph above (the tests read them all).
    pub const ALL: [Glyph; 24] = [
        PLUS,
        MINUS,
        TECHNIQUES,
        SETTINGS,
        CHEVRON_LEFT,
        CHEVRON_RIGHT,
        CHEVRON_UP,
        CHEVRON_DOWN,
        UP_DOWN,
        POWER,
        MORE,
        PLAY,
        PAUSE,
        GO_TO_START,
        STOP,
        LOOP,
        FOLLOW,
        MONITOR,
        PANEL_LEFT,
        PANEL_RIGHT,
        MIXER,
        NOTE,
        PULSE,
        PLUS,
    ];
}

/// `glyph` in a `size` × `size` box, drawn in `color` with the [`STROKE`].
pub fn line_icon(glyph: Glyph, size: f32, color: Color) -> LineIcon {
    LineIcon {
        glyph,
        size,
        color: Some(color),
        stroke: STROKE,
    }
}

/// `glyph` in a `size` × `size` box, drawn in the text colour its parent
/// sets (a button's `text_color`, which follows its hover): what the
/// shared controls put in their buttons.
pub fn line_icon_inherit(glyph: Glyph, size: f32) -> LineIcon {
    LineIcon {
        glyph,
        size,
        color: None,
        stroke: STROKE,
    }
}

/// [`line_icon`] with another on-screen stroke width (the power glyph in an
/// 18 px button, say).
pub fn line_icon_stroked(glyph: Glyph, size: f32, color: Color, stroke: f32) -> LineIcon {
    LineIcon {
        stroke,
        ..line_icon(glyph, size, color)
    }
}

/// One glyph in a square box (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct LineIcon {
    glyph: Glyph,
    size: f32,
    /// `None`: the parent's text colour.
    color: Option<Color>,
    stroke: f32,
}

impl LineIcon {
    /// The on-screen stroke width.
    pub fn stroke(mut self, stroke: f32) -> Self {
        self.stroke = stroke;
        self
    }
}

/// A drawn glyph, kept until the glyph, its size, colour or stroke
/// changes.
#[derive(Default)]
struct Drawn {
    cache: canvas::Cache,
    key: RefCell<Option<DrawnKey>>,
}

/// What a drawn glyph was drawn as: the glyph (its name, unique to each,
/// and its scale), its size, colour and stroke. The glyph is part of it: a button
/// that swaps glyphs in place (Play for Pause, a chevron's way) keeps its
/// widget state, and the cached drawing of the old glyph went on showing.
type DrawnKey = (&'static str, u32, u32, [u8; 4], u32);

impl<M> Widget<M, Theme, Renderer> for LineIcon {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Drawn>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Drawn::default())
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(self.size, self.size, Size::ZERO))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let color = self.color.unwrap_or(style.text_color);
        let state = tree.state.downcast_ref::<Drawn>();
        let key = (
            self.glyph.name,
            self.glyph.scale.to_bits(),
            bounds.width.to_bits() ^ bounds.height.to_bits().rotate_left(16),
            color.into_rgba8(),
            self.stroke.to_bits(),
        );
        if state.key.replace(Some(key)) != Some(key) {
            state.cache.clear();
        }
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            draw(frame, self.glyph, color, self.stroke);
        });
        renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
            renderer.draw_geometry(geometry);
        });
    }
}

impl<'a, M: 'a> From<LineIcon> for Element<'a, M> {
    fn from(icon: LineIcon) -> Self {
        Element::new(icon)
    }
}

/// Draws `glyph` filling `frame`, in `color`, its strokes `stroke` px wide.
pub fn draw(frame: &mut Frame, glyph: Glyph, color: Color, stroke: f32) {
    let size = frame.width().min(frame.height());
    let offset = Vector::new((frame.width() - size) / 2.0, (frame.height() - size) / 2.0);
    let place = Placement {
        scale: size / GRID * glyph.scale,
        origin: Point::new(
            offset.x + size / 2.0 - GRID / 2.0 * size / GRID * glyph.scale,
            offset.y + size / 2.0 - GRID / 2.0 * size / GRID * glyph.scale,
        ),
    };
    let line = Stroke::default()
        .with_color(color)
        .with_width(stroke)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    for part in glyph.parts {
        match part {
            Part::Stroke(d) => frame.stroke(&build(d, place), line),
            Part::Fill(d) => frame.fill(&build(d, place), color),
        }
    }
}

/// Where a glyph's grid lands: `origin + p × scale`.
#[derive(Debug, Clone, Copy)]
struct Placement {
    scale: f32,
    origin: Point,
}

impl Placement {
    /// The grid itself at `scale` about its centre.
    fn grid(scale: f32) -> Self {
        Placement {
            scale,
            origin: Point::new(GRID / 2.0 * (1.0 - scale), GRID / 2.0 * (1.0 - scale)),
        }
    }

    fn map(self, p: Point) -> Point {
        Point::new(
            self.origin.x + p.x * self.scale,
            self.origin.y + p.y * self.scale,
        )
    }
}

/// The path `d` placed by `place`.
fn build(d: &str, place: Placement) -> Path {
    Path::new(|b| {
        for segment in parse(d) {
            match segment {
                Segment::Move(p) => b.move_to(place.map(p)),
                Segment::Line(p) => b.line_to(place.map(p)),
                Segment::Cubic(c1, c2, p) => {
                    b.bezier_curve_to(place.map(c1), place.map(c2), place.map(p));
                }
                Segment::Close => b.close(),
            }
        }
    })
}

/// Every point a path's segments name (for its bounds).
fn points(d: &str) -> Vec<Point> {
    parse(d)
        .into_iter()
        .flat_map(|s| match s {
            Segment::Move(p) | Segment::Line(p) => vec![p],
            Segment::Cubic(a, b, p) => vec![a, b, p],
            Segment::Close => vec![],
        })
        .collect()
}

/// A path segment in absolute coordinates; quadratics and arcs become
/// cubics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

/// Reads SVG path data (`M L H V C S Q T A Z`, absolute and relative,
/// numbers packed as SVG allows: `.33.5`, `1-2`, arc flags `011`). Data it
/// cannot read ends the path there.
pub fn parse(d: &str) -> Vec<Segment> {
    let mut reader = Reader {
        bytes: d.as_bytes(),
        at: 0,
    };
    let mut out = Vec::new();
    let mut current = Point::ORIGIN;
    let mut start = Point::ORIGIN;
    // The last cubic's or quadratic's second control point, for S and T.
    let mut last_cubic: Option<Point> = None;
    let mut last_quad: Option<Point> = None;
    let mut command = None;
    loop {
        reader.skip_separators();
        if reader.done() {
            break;
        }
        if let Some(c) = reader.command() {
            command = Some(c);
        } else if command.is_none() {
            break;
        }
        let Some(c) = command else { break };
        let relative = c.is_ascii_lowercase();
        let base = if relative { current } else { Point::ORIGIN };
        let at = |x: f32, y: f32| Point::new(base.x + x, base.y + y);
        let mut cubic = None;
        let mut quad = None;
        match c.to_ascii_uppercase() {
            b'M' => {
                let Some([x, y]) = reader.numbers() else {
                    break;
                };
                current = at(x, y);
                start = current;
                out.push(Segment::Move(current));
                // Further pairs are lines.
                command = Some(if relative { b'l' } else { b'L' });
            }
            b'L' => {
                let Some([x, y]) = reader.numbers() else {
                    break;
                };
                current = at(x, y);
                out.push(Segment::Line(current));
            }
            b'H' => {
                let Some([x]) = reader.numbers() else { break };
                current = Point::new(if relative { current.x + x } else { x }, current.y);
                out.push(Segment::Line(current));
            }
            b'V' => {
                let Some([y]) = reader.numbers() else { break };
                current = Point::new(current.x, if relative { current.y + y } else { y });
                out.push(Segment::Line(current));
            }
            b'C' => {
                let Some([x1, y1, x2, y2, x, y]) = reader.numbers() else {
                    break;
                };
                let (c1, c2, p) = (at(x1, y1), at(x2, y2), at(x, y));
                out.push(Segment::Cubic(c1, c2, p));
                cubic = Some(c2);
                current = p;
            }
            b'S' => {
                let Some([x2, y2, x, y]) = reader.numbers() else {
                    break;
                };
                let c1 = reflect(last_cubic, current);
                let (c2, p) = (at(x2, y2), at(x, y));
                out.push(Segment::Cubic(c1, c2, p));
                cubic = Some(c2);
                current = p;
            }
            b'Q' => {
                let Some([x1, y1, x, y]) = reader.numbers() else {
                    break;
                };
                let (q, p) = (at(x1, y1), at(x, y));
                out.push(quadratic(current, q, p));
                quad = Some(q);
                current = p;
            }
            b'T' => {
                let Some([x, y]) = reader.numbers() else {
                    break;
                };
                let q = reflect(last_quad, current);
                let p = at(x, y);
                out.push(quadratic(current, q, p));
                quad = Some(q);
                current = p;
            }
            b'A' => {
                let Some([rx, ry, rotation]) = reader.numbers() else {
                    break;
                };
                let (Some(large), Some(sweep)) = (reader.flag(), reader.flag()) else {
                    break;
                };
                let Some([x, y]) = reader.numbers() else {
                    break;
                };
                let p = at(x, y);
                out.extend(arc(current, rx, ry, rotation, large, sweep, p));
                current = p;
            }
            b'Z' => {
                out.push(Segment::Close);
                current = start;
                command = None;
            }
            _ => break,
        }
        last_cubic = cubic;
        last_quad = quad;
    }
    out
}

/// `control` mirrored through `about` (S's and T's first control point),
/// or `about` itself after any other segment.
fn reflect(control: Option<Point>, about: Point) -> Point {
    control.map_or(about, |c| {
        Point::new(2.0 * about.x - c.x, 2.0 * about.y - c.y)
    })
}

/// The quadratic `from`, `q`, `to` as a cubic.
fn quadratic(from: Point, q: Point, to: Point) -> Segment {
    let lerp = |a: Point, b: Point| {
        Point::new(a.x + (b.x - a.x) * 2.0 / 3.0, a.y + (b.y - a.y) * 2.0 / 3.0)
    };
    Segment::Cubic(lerp(from, q), lerp(to, q), to)
}

/// An SVG arc from `from` to `to` as cubics (SVG 1.1, F.6.5 and F.6.6:
/// the radii scaled up when too small; at most 90° per cubic).
fn arc(
    from: Point,
    rx: f32,
    ry: f32,
    rotation: f32,
    large: bool,
    sweep: bool,
    to: Point,
) -> Vec<Segment> {
    if from == to {
        return Vec::new();
    }
    let (mut rx, mut ry) = (f64::from(rx.abs()), f64::from(ry.abs()));
    if rx == 0.0 || ry == 0.0 {
        return vec![Segment::Line(to)];
    }
    let phi = f64::from(rotation).to_radians();
    let (sin, cos) = phi.sin_cos();
    let (x1, y1, x2, y2) = (
        f64::from(from.x),
        f64::from(from.y),
        f64::from(to.x),
        f64::from(to.y),
    );
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cos * dx + sin * dy;
    let y1p = -sin * dx + cos * dy;
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut k = (num / den).max(0.0).sqrt();
    if large == sweep {
        k = -k;
    }
    let cxp = k * rx * y1p / ry;
    let cyp = -k * ry * x1p / rx;
    let cx = cos * cxp - sin * cyp + (x1 + x2) / 2.0;
    let cy = sin * cxp + cos * cyp + (y1 + y2) / 2.0;
    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| {
        let dot = ux * vx + uy * vy;
        let len = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            -a
        } else {
            a
        }
    };
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta = angle(1.0, 0.0, ux, uy);
    let mut delta = angle(ux, uy, vx, vy);
    if !sweep && delta > 0.0 {
        delta -= std::f64::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f64::consts::TAU;
    }
    let pieces = (delta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = delta / pieces as f64;
    let t = 4.0 / 3.0 * (step / 4.0).tan();
    let point = |a: f64| {
        let (s, c) = a.sin_cos();
        (rx * c, ry * s)
    };
    let derivative = |a: f64| {
        let (s, c) = a.sin_cos();
        (-rx * s, ry * c)
    };
    let place = |(x, y): (f64, f64)| {
        Point::new(
            (cos * x - sin * y + cx) as f32,
            (sin * x + cos * y + cy) as f32,
        )
    };
    let mut out = Vec::with_capacity(pieces);
    let mut a = theta;
    for i in 0..pieces {
        let b = a + step;
        let (p0, p1) = (point(a), point(b));
        let (d0, d1) = (derivative(a), derivative(b));
        let c1 = place((p0.0 + t * d0.0, p0.1 + t * d0.1));
        let c2 = place((p1.0 - t * d1.0, p1.1 - t * d1.1));
        // The last end exactly where the data put it.
        let end = if i + 1 == pieces { to } else { place(p1) };
        out.push(Segment::Cubic(c1, c2, end));
        a = b;
    }
    out
}

/// A cursor over path data.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn skip_separators(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b',')
        {
            self.at += 1;
        }
    }

    /// The command letter here, if there is one.
    fn command(&mut self) -> Option<u8> {
        let c = *self.bytes.get(self.at)?;
        if c.is_ascii_alphabetic() && c != b'e' && c != b'E' {
            self.at += 1;
            Some(c)
        } else {
            None
        }
    }

    /// `N` numbers, or nothing when the data runs out.
    fn numbers<const N: usize>(&mut self) -> Option<[f32; N]> {
        let mut out = [0.0; N];
        for slot in &mut out {
            *slot = self.number()?;
        }
        Some(out)
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        let mut seen_dot = false;
        let mut seen_digit = false;
        if matches!(self.bytes.get(self.at), Some(b'-' | b'+')) {
            self.at += 1;
        }
        while let Some(&b) = self.bytes.get(self.at) {
            if b.is_ascii_digit() {
                seen_digit = true;
            } else if b == b'.' && !seen_dot {
                seen_dot = true;
            } else if (b == b'e' || b == b'E') && seen_digit {
                self.at += 1;
                if matches!(self.bytes.get(self.at), Some(b'-' | b'+')) {
                    self.at += 1;
                }
                continue;
            } else {
                break;
            }
            self.at += 1;
        }
        if !seen_digit {
            self.at = start;
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()?
            .parse()
            .ok()
    }

    /// An arc flag: one `0` or `1` (packed flags are one character each).
    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        match b {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn reads_absolute_relative_and_packed_data() {
        let segments = parse("M12 3.5v17M3.5 12h17");
        assert_eq!(
            segments,
            vec![
                Segment::Move(Point::new(12.0, 3.5)),
                Segment::Line(Point::new(12.0, 20.5)),
                Segment::Move(Point::new(3.5, 12.0)),
                Segment::Line(Point::new(20.5, 12.0)),
            ]
        );
        // Implicit lines after a move, packed numbers.
        let segments = parse("m15 18-6-6 6-6");
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[2], Segment::Line(Point::new(15.0, 6.0)));
        let segments = parse("M1 1l.5.5");
        assert_eq!(segments[1], Segment::Line(Point::new(1.5, 1.5)));
    }

    #[test]
    fn an_arc_ends_where_its_data_says_and_bulges_the_right_way() {
        // A half circle of radius 3 from (9, 12) to (15, 12), sweeping
        // counter-clockwise on screen (sweep 0): it passes below the centre.
        let segments = parse("M9 12a3 3 0 1 0 6 0");
        let Some(Segment::Cubic(_, _, end)) = segments.last() else {
            panic!("{segments:?}")
        };
        assert!(close(end.x, 15.0) && close(end.y, 12.0));
        let lowest = points("M9 12a3 3 0 1 0 6 0")
            .into_iter()
            .map(|p| p.y)
            .fold(f32::MIN, f32::max);
        assert!(lowest > 14.0, "{lowest}");
        // Every glyph reads to the end of its data.
        for glyph in glyphs::ALL {
            for part in glyph.parts {
                let (Part::Stroke(d) | Part::Fill(d)) = part;
                assert!(!parse(d).is_empty(), "{}", glyph.name);
            }
        }
    }

    #[test]
    fn a_toolbar_row_has_one_optical_size() {
        // DESIGN.md: about 17 of 24 units across, the gear at 75 %.
        for glyph in [glyphs::PLUS, glyphs::TECHNIQUES, glyphs::SETTINGS] {
            let extent = glyph.extent();
            let across = extent.width.max(extent.height);
            assert!(
                (15.5..=17.5).contains(&across),
                "{} spans {across}",
                glyph.name
            );
            // Centred in the grid.
            let centre = extent.center();
            assert!(
                (centre.x - 12.0).abs() < 1.5 && (centre.y - 12.0).abs() < 1.5,
                "{} {centre:?}",
                glyph.name
            );
        }
        // The gear fills its grid, so it is the one scaled.
        assert_eq!(glyphs::SETTINGS.scale, 0.75);
        assert!(glyphs::SETTINGS.parts.len() == 2);
    }

    #[test]
    fn a_glyph_draws_the_same_stroke_at_every_size() {
        use crate::ui::core::widgets::animated::testing::{near, Frames, BACKGROUND};
        use iced::time::Instant;
        use iced::widget::{container, row};

        // A vertical bar across the box: its ink is the stroke's width,
        // whatever the box.
        const BAR: Glyph = Glyph::new("bar", &[Part::Stroke("M12 2v20")]);
        let inked = |frames: &Frames, y: f32, from: f32, to: f32| {
            let mut n = 0;
            let mut x = from;
            while x < to {
                if !near(frames.pixel(Point::new(x, y)), BACKGROUND) {
                    n += 1;
                }
                x += 1.0;
            }
            n
        };
        let mut frames = Frames::new(Size::new(100.0, 40.0));
        frames.frame(
            container(row![
                Element::<()>::from(line_icon_stroked(BAR, 16.0, Color::WHITE, 2.0)),
                Element::<()>::from(line_icon_stroked(BAR, 32.0, Color::WHITE, 2.0)),
            ]),
            &[],
            Instant::now(),
        );
        let small = inked(&frames, 8.0, 0.0, 16.0);
        let large = inked(&frames, 16.0, 16.0, 48.0);
        assert!((2..=3).contains(&small), "{small}");
        assert_eq!(small, large);
    }

    /// A glyph swapped for another in place (Play for Pause) keeps the
    /// widget's state: the new glyph is drawn, not the old one's cached
    /// drawing.
    #[test]
    fn a_glyph_swapped_in_place_is_drawn_anew() {
        use crate::ui::core::widgets::animated::testing::{near, Frames, BACKGROUND};
        use iced::time::Instant;
        use iced::widget::container;

        const VERTICAL: Glyph = Glyph::new("vertical", &[Part::Stroke("M12 2v20")]);
        const HORIZONTAL: Glyph = Glyph::new("horizontal", &[Part::Stroke("M2 12h20")]);
        let inked =
            |frames: &Frames, x: f32, y: f32| !near(frames.pixel(Point::new(x, y)), BACKGROUND);
        let mut frames = Frames::new(Size::new(40.0, 40.0));
        let t0 = Instant::now();
        frames.frame(
            container(Element::<()>::from(line_icon(VERTICAL, 32.0, Color::WHITE))),
            &[],
            t0,
        );
        assert!(inked(&frames, 16.0, 6.0), "the vertical bar");
        assert!(!inked(&frames, 6.0, 16.0));
        frames.frame(
            container(Element::<()>::from(line_icon(
                HORIZONTAL,
                32.0,
                Color::WHITE,
            ))),
            &[],
            t0 + std::time::Duration::from_millis(16),
        );
        assert!(inked(&frames, 6.0, 16.0), "the horizontal bar");
        assert!(!inked(&frames, 16.0, 6.0), "the vertical bar is gone");
    }
}
