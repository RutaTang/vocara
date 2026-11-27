//! The readout panel beside the figure: a card of the seven components
//! (each a 2 px track, a soft band over its last two seconds, a glowing
//! marker, its range's ends and a tabular value), then a small vowel map
//! (the F1–F2 quadrilateral, the voice's dot with a short trail, the
//! nearest vowel named), the fit, the formants and a caveat. Resting (no
//! voice), the markers keep their place, dimmed, and the panel says so.

use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::text::Alignment;
use iced::widget::{canvas as canvas_widget, column, container, row, stack, Space};
use iced::{
    mouse, Color, Element, Length, Padding, Pixels, Point, Rectangle, Renderer, Size, Theme,
};
use vocara_model::models::voice_metrics::note_of;

use super::pose::{Components, Fit};
use super::Message;
use crate::ui::core::text;
use crate::ui::core::theme::{alpha, boxed, font, type_scale, Studio};
use crate::ui::core::widgets::controls::labelled;
use crate::ui::core::widgets::fading_scrollbar::{fading_scrollbar, scrollable};
use crate::ui::core::widgets::studio_text::studio_text;

/// The panel's width.
pub const WIDTH: f32 = 404.0;

const CARD: Color = Color::from_rgb8(0x18, 0x18, 0x1C);
const CARD_EDGE: Color = Color::from_rgb8(0x24, 0x24, 0x2B);
const RULE: Color = Color::from_rgb8(0x20, 0x20, 0x26);
const LABEL: Color = Color::from_rgb8(0xC4, 0xC4, 0xCE);
const RANGE_END: Color = Color::from_rgb8(0x5E, 0x5E, 0x6B);
const TRACK: Color = Studio::CONTROL_ACTIVE;
const BAND: Color = Color::from_rgba(224.0 / 255.0, 122.0 / 255.0, 106.0 / 255.0, 0.22);
const KNOB: Color = Color::from_rgb8(0xF4, 0xEF, 0xEA);
const GLOW: Color = Studio::ACCENT_BRIGHT;
const MAP_EDGE: Color = Studio::CONTROL_ACTIVE;

/// The component rows, in order: label, the track's ends.
pub const ROWS: [(&str, &str, &str); 7] = [
    ("Jaw", "Closed", "Open"),
    ("Tongue height", "Low", "High"),
    ("Tongue position", "Front", "Back"),
    ("Lips", "Spread", "Rounded"),
    ("Larynx", "Low", "High"),
    ("Velum", "Open", "Closed"),
    ("Vocal folds", "Breathy", "Pressed"),
];

/// The jaw's track spans 0 to this gap (mm).
pub const JAW_RANGE_MM: f32 = 22.0;
/// The larynx's track spans ± this (mm).
pub const LARYNX_RANGE_MM: f32 = 10.0;

/// Where each component sits along its track (0 at its left end).
pub fn positions(c: &Components) -> [f32; 7] {
    [
        c.jaw_mm / JAW_RANGE_MM,
        c.tongue_height,
        1.0 - c.tongue_front,
        c.lip_round,
        0.5 + c.larynx_mm / (2.0 * LARYNX_RANGE_MM),
        1.0 - c.velum_open,
        c.voice_quality,
    ]
    .map(|v| v.clamp(0.0, 1.0))
}

/// A signed whole number of millimetres, with a true minus.
fn signed_mm(mm: f32) -> String {
    let n = mm.round() as i32;
    match n.signum() {
        1 => format!("+{n} mm"),
        -1 => format!("\u{2212}{} mm", -n),
        _ => "0 mm".to_string(),
    }
}

/// What each row reads.
pub fn values(c: &Components) -> [String; 7] {
    let height = match c.tongue_height {
        h if h < 0.4 => "Low",
        h if h < 0.65 => "Mid",
        _ => "High",
    };
    let position = match c.tongue_front {
        f if f > 0.62 => "Front",
        f if f > 0.36 => "Central",
        _ => "Back",
    };
    let lips = match c.lip_round {
        r if r < 0.2 => "Spread",
        r if r < 0.6 => "Relaxed",
        _ => "Rounded",
    };
    let velum = match c.velum_open {
        v if v < 0.2 => "Closed",
        v if v < 0.6 => "Partly open",
        _ => "Open",
    };
    let quality = match c.voice_quality {
        q if q < 0.35 => "breathy",
        q if q < 0.7 => "clear",
        _ => "pressed",
    };
    let folds = match note_of(f64::from(c.f0_hz)) {
        Some(note) if c.f0_hz > 0.0 => format!("{} \u{b7} {quality}", note.name),
        _ => "\u{2014}".to_string(),
    };
    [
        format!("{} mm", c.jaw_mm.round().max(0.0) as i32),
        height.to_string(),
        position.to_string(),
        lips.to_string(),
        signed_mm(c.larynx_mm),
        velum.to_string(),
        folds,
    ]
}

/// One row as the panel draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub lo: &'static str,
    pub hi: &'static str,
    pub position: f32,
    /// The range over the last two seconds, if any.
    pub band: Option<(f32, f32)>,
    pub value: String,
}

/// What the panel says under the vowel map's side.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// A voice: how well the shape explains it, and its formants.
    Voiced { fit: Fit, formants: [f32; 3] },
    /// No voice now: the shape held.
    Resting,
    /// Take mode with no take to read.
    NoTake,
}

/// Everything the panel shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Readouts {
    pub rows: Vec<Row>,
    /// The voice on the vowel map now (F1, F2), and its recent trail
    /// (oldest first).
    pub vowel: Option<(f32, f32)>,
    pub trail: Vec<(f32, f32)>,
    pub status: Status,
    /// 0 voiced to 1 resting: the markers dim.
    pub rest: f32,
}

/// The caveat under the readouts.
pub const CAVEAT: &str =
    "An estimate from your formants. Different shapes can make the same sound: a guide, not an X-ray.";

/// How the panel fits a height (see [`metrics`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Space above the card (it lines up with the figure's well when there
    /// is room).
    pub pad_top: f32,
    /// Each component row's height, its rule included.
    pub row: f32,
    /// The gaps in the text beside the vowel map.
    pub text_gap: f32,
    /// The bottom row's height (the vowel map's card).
    pub bottom: f32,
    /// Whether it scrolls (the dock is shorter than the panel's least
    /// height).
    pub scrolls: bool,
}

/// The card's padding above and below its rows.
const CARD_PAD: f32 = 4.0;
/// The gap between the card and the bottom row.
const GAP: f32 = 10.0;
/// The bottom row's natural height (the mockup's vowel map, and the text
/// beside it).
pub const BOTTOM: f32 = 92.0;
/// The bottom row's least height (the text with tighter gaps).
pub const BOTTOM_MIN: f32 = 88.0;
/// The mockup's row height, and the least ones: first as the panel keeps
/// its space above the card, then without it.
pub const ROW: f32 = 30.0;
pub const ROW_TIGHT: f32 = 26.0;
pub const ROW_MIN: f32 = 24.0;
/// Space above the card at full height: the figure's header row and gap,
/// so the card lines up with the figure's well.
pub const PAD_TOP: f32 = 30.0;

/// How the panel lays out in `height` px: the mockup's spacing when it
/// fits; else the rows tighten (to 26 px), then the space above the card
/// goes, then the rows tighten to 24 px, then it scrolls.
pub fn metrics(height: f32) -> Metrics {
    let card = |row: f32| 2.0 * CARD_PAD + 7.0 * row;
    let gap = |bottom: f32| if bottom >= BOTTOM { 6.0 } else { 4.0 };
    // With the space above the card.
    let row = (height - PAD_TOP - card(0.0) - GAP - BOTTOM) / 7.0;
    if row >= ROW_TIGHT {
        let row = row.min(ROW);
        let bottom = height - PAD_TOP - card(row) - GAP;
        return Metrics {
            pad_top: PAD_TOP,
            row,
            text_gap: gap(bottom),
            bottom,
            scrolls: false,
        };
    }
    let pad_top = height - card(ROW_TIGHT) - GAP - BOTTOM_MIN;
    if pad_top >= 0.0 {
        let pad_top = pad_top.min(PAD_TOP);
        let bottom = height - pad_top - card(ROW_TIGHT) - GAP;
        return Metrics {
            pad_top,
            row: ROW_TIGHT,
            text_gap: gap(bottom),
            bottom,
            scrolls: false,
        };
    }
    let row = ((height - card(0.0) - GAP - BOTTOM_MIN) / 7.0).clamp(ROW_MIN, ROW_TIGHT);
    let bottom = (height - card(row) - GAP).max(BOTTOM_MIN);
    Metrics {
        pad_top: 0.0,
        row,
        text_gap: gap(bottom),
        bottom,
        scrolls: card(row) + GAP + bottom > height + 0.5,
    }
}

/// The panel at `height`.
pub fn panel<'a>(r: &Readouts, height: f32) -> Element<'a, Message> {
    let m = metrics(height);
    let n = r.rows.len();
    let rows = r
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| component_row(row, m.row, i + 1 < n, r.rest));
    let card = container(column(rows))
        .padding(Padding::from([CARD_PAD, 14.0]))
        .width(Length::Fill)
        .style(|_: &Theme| boxed(Some(CARD), 10.0, Some(CARD_EDGE)));
    let map = container(
        canvas_widget(VowelMap {
            at: r.vowel,
            trail: r.trail.clone(),
            rest: r.rest,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(150)
    .height(m.bottom.min(MAP_MAX))
    .style(|_: &Theme| boxed(Some(CARD), 10.0, Some(CARD_EDGE)));
    let map = labelled(map, vowel_map_label(r.vowel));
    let bottom = row![map, status_column(&r.status, m.text_gap)]
        .spacing(10)
        .height(m.bottom);
    let body = column![
        Space::new().height(m.pad_top),
        card,
        Space::new().height(GAP),
        bottom
    ]
    .width(Length::Fill);
    let body: Element<'a, Message> = if m.scrolls {
        fading_scrollbar(scrollable(body).height(Length::Fill))
    } else {
        body.into()
    };
    labelled(
        container(body).width(WIDTH).height(Length::Fill),
        "Components",
    )
}

fn component_row<'a>(row: &Row, height: f32, rule: bool, rest: f32) -> Element<'a, Message> {
    let label = studio_text(row.label)
        .size(type_scale::BODY)
        .line_height(Pixels(type_scale::LINE))
        .color(LABEL)
        .width(96);
    let value = studio_text(row.value.clone())
        .size(type_scale::BODY)
        .line_height(Pixels(type_scale::LINE))
        .font(font::MEDIUM)
        .tabular()
        .color(if rest > 0.5 {
            Studio::TEXT_SECONDARY
        } else {
            Studio::TEXT_PRIMARY
        })
        .align_x(Alignment::Right)
        .width(68);
    let track = canvas_widget(Track {
        lo: row.lo,
        hi: row.hi,
        position: row.position,
        band: row.band,
        rest,
    })
    .width(Length::Fill)
    .height(height - 1.0);
    let line = row![label, track, value]
        .spacing(12)
        .align_y(Vertical::Center)
        .height(height - 1.0);
    let line = labelled(line, format!("{}: {}", row.label, row.value));
    column![
        line,
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(move |_: &Theme| boxed(
                Some(if rule { RULE } else { Color::TRANSPARENT }),
                0.0,
                None
            )),
    ]
    .into()
}

/// A component's track: the 2 px line, the band over its recent range, the
/// marker with its glow, and the range's ends above the line.
struct Track {
    lo: &'static str,
    hi: &'static str,
    position: f32,
    band: Option<(f32, f32)>,
    rest: f32,
}

impl canvas::Program<Message> for Track {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let mid = (bounds.height / 2.0).round();
        let live = 1.0 - 0.55 * self.rest;
        frame.fill(
            &Path::rounded_rectangle(Point::new(0.0, mid - 1.0), Size::new(w, 2.0), 1.0.into()),
            TRACK,
        );
        if let Some((a, b)) = self.band {
            let (a, b) = (a.min(b) * w, a.max(b) * w);
            let width = (b - a).max(6.0);
            let left = ((a + b) / 2.0 - width / 2.0).clamp(0.0, (w - width).max(0.0));
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(left, mid - 3.0),
                    Size::new(width, 6.0),
                    3.0.into(),
                ),
                alpha(BAND, BAND.a * live),
            );
        }
        let x = self.position.clamp(0.0, 1.0) * w;
        let knob = Point::new(x, mid);
        // The glow (`0 0 10px`), the ring (`0 0 0 3px` at 20 %), the knob.
        for (r, a) in [(10.0, 0.07), (8.0, 0.10), (6.5, 0.14)] {
            frame.fill(&Path::circle(knob, r), alpha(GLOW, a * live * live));
        }
        frame.fill(&Path::circle(knob, 8.0), alpha(GLOW, 0.20 * live));
        frame.fill(
            &Path::circle(knob, 5.0),
            if self.rest > 0.0 {
                mix(KNOB, Studio::TEXT_FAINT, self.rest * 0.6)
            } else {
                KNOB
            },
        );
        let end = |content: &str, x: f32, align: Alignment| canvas::Text {
            content: content.to_string(),
            position: Point::new(x, mid - 8.0),
            color: RANGE_END,
            size: Pixels(9.0),
            line_height: text::NATURAL,
            font: font::REGULAR,
            align_x: align,
            align_y: Vertical::Center,
            ..canvas::Text::default()
        };
        if bounds.height >= 22.0 {
            frame.fill_text(end(self.lo, 0.0, Alignment::Left));
            frame.fill_text(end(self.hi, w, Alignment::Right));
        }
        vec![frame.into_geometry()]
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

/// The vowels named on the map (F1, F2 in Hz of an adult male voice), and
/// what the panel calls each.
pub const VOWELS: [(&str, &str, f32, f32); 7] = [
    ("i", "ee", 280.0, 2250.0),
    ("ɛ", "eh", 530.0, 1840.0),
    ("æ", "a", 690.0, 1660.0),
    ("ɑ", "ah", 720.0, 1090.0),
    ("ɔ", "aw", 570.0, 840.0),
    ("o", "oh", 450.0, 880.0),
    ("u", "oo", 310.0, 870.0),
];

/// The vowel map card's tallest: in a tall dock the card stays a card, not
/// an empty column.
pub const MAP_MAX: f32 = 124.0;

/// The room the vowel map's caption keeps below it.
const CAPTION: f32 = 14.0;

/// The map's own units (the mockup's 150 × 92 box).
pub const MAP_SIZE: Size = Size::new(150.0, 92.0);

/// Where (F1, F2) lands on the map, in its units: the quadrilateral's
/// corners are [i] (top left), [u] (top right), [æ] (bottom left) and [ɑ]
/// (bottom right); F1 runs down, F2 (on a log scale) right to left.
pub fn map_point(f1: f32, f2: f32) -> Point {
    let y = ((f1 - 300.0) / 450.0).clamp(-0.1, 1.15);
    let back = 850.0 + 200.0 * y;
    let front = 2300.0 - 550.0 * y;
    let f = ((f2.max(1.0) / back).ln() / (front / back).ln()).clamp(-0.1, 1.1);
    let left = 22.0 + 24.0 * y;
    let right = 128.0 - 24.0 * y;
    Point::new(left + (1.0 - f) * (right - left), 16.0 + 62.0 * y)
}

/// The named vowel nearest (F1, F2) on the map.
pub fn nearest_vowel(f1: f32, f2: f32) -> &'static str {
    let p = map_point(f1, f2);
    VOWELS
        .iter()
        .map(|&(_, name, a, b)| (name, map_point(a, b).distance(p)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or("ah", |(name, _)| name)
}

fn vowel_map_label(at: Option<(f32, f32)>) -> String {
    match at {
        Some((f1, f2)) => format!("Vowel map: your vowel sits near {}", nearest_vowel(f1, f2)),
        None => "Vowel map".to_string(),
    }
}

/// The vowel map (see the module docs).
struct VowelMap {
    at: Option<(f32, f32)>,
    trail: Vec<(f32, f32)>,
    rest: f32,
}

impl canvas::Program<Message> for VowelMap {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // The map at one scale, fitted and centred above its caption
        // (never past 1.15×).
        let room = Size::new(bounds.width, (bounds.height - CAPTION).max(1.0));
        let s = (room.width / MAP_SIZE.width)
            .min(room.height / MAP_SIZE.height)
            .min(1.15);
        let ox = (room.width - MAP_SIZE.width * s) / 2.0;
        let oy = (room.height - MAP_SIZE.height * s) / 2.0 + 2.0;
        let at = |p: Point| Point::new(ox + p.x * s, oy + p.y * s);
        frame.stroke(
            &Path::new(|b| {
                b.move_to(at(Point::new(22.0, 16.0)));
                b.line_to(at(Point::new(128.0, 16.0)));
                b.line_to(at(Point::new(104.0, 78.0)));
                b.line_to(at(Point::new(46.0, 78.0)));
                b.close();
            }),
            Stroke::default().with_color(MAP_EDGE).with_width(1.0),
        );
        for (glyph, x, y) in [
            ("i", 14.0, 14.0),
            ("u", 130.0, 14.0),
            ("æ", 40.0, 88.0),
            ("ɑ", 104.0, 88.0),
            ("ɨ", 74.0, 12.0),
        ] {
            frame.fill_text(canvas::Text {
                content: glyph.to_string(),
                position: at(Point::new(x, y)),
                color: Studio::TEXT_DISABLED,
                size: Pixels(9.0 * s.max(0.8)),
                line_height: text::NATURAL,
                font: font::REGULAR,
                align_x: Alignment::Left,
                align_y: Vertical::Bottom,
                ..canvas::Text::default()
            });
        }
        let live = 1.0 - 0.6 * self.rest;
        if self.trail.len() > 1 {
            let pts: Vec<Point> = self
                .trail
                .iter()
                .map(|&(a, b)| at(map_point(a, b)))
                .collect();
            frame.stroke(
                &Path::new(|b| {
                    b.move_to(pts[0]);
                    for p in &pts[1..] {
                        b.line_to(*p);
                    }
                }),
                Stroke::default()
                    .with_color(alpha(GLOW, 0.45 * live))
                    .with_width(2.0)
                    .with_line_cap(LineCap::Round),
            );
        }
        if let Some((f1, f2)) = self.at {
            let p = at(map_point(f1, f2));
            frame.fill(&Path::circle(p, 7.0), alpha(GLOW, 0.25 * live));
            frame.fill(
                &Path::circle(p, 3.5),
                mix(KNOB, Studio::TEXT_FAINT, self.rest * 0.6),
            );
        }
        let name = match self.at {
            Some((f1, f2)) => format!("Vowel \u{b7} \u{201c}{}\u{201d}", nearest_vowel(f1, f2)),
            None => "Vowel".to_string(),
        };
        frame.fill_text(canvas::Text {
            content: name,
            position: Point::new(10.0, bounds.height - 6.0),
            color: Studio::TEXT_FAINT,
            size: Pixels(type_scale::AXIS),
            line_height: text::NATURAL,
            font: font::REGULAR,
            align_x: Alignment::Left,
            align_y: Vertical::Bottom,
            ..canvas::Text::default()
        });
        vec![frame.into_geometry()]
    }
}

/// The fit's name and colour.
pub fn fit_label(fit: Fit) -> (&'static str, Color, Color) {
    match fit {
        Fit::Good => ("Good fit", Studio::GOOD, Studio::GOOD),
        Fit::Fair => ("Fair fit", Studio::WARN, Studio::WARN_TEXT),
        Fit::Uncertain => ("Uncertain", Studio::TEXT_FAINT, Studio::TEXT_SECONDARY),
    }
}

/// "F1 640 · F2 1190 · F3 2620 Hz".
pub fn formants_line(f: [f32; 3]) -> String {
    format!(
        "F1 {} \u{b7} F2 {} \u{b7} F3 {} Hz",
        f[0].round() as i32,
        f[1].round() as i32,
        f[2].round() as i32
    )
}

fn status_column<'a>(status: &Status, gap: f32) -> Element<'a, Message> {
    let line = |s: String, color: Color| {
        studio_text(s)
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .color(color)
    };
    let (head, dot, head_color, glow, second) = match status {
        Status::Voiced { fit, formants } => {
            let (name, dot, text) = fit_label(*fit);
            (name, dot, text, true, formants_line(*formants))
        }
        Status::Resting => (
            "Resting",
            Studio::TEXT_FAINT,
            Studio::TEXT_SECONDARY,
            false,
            "No voice just now: the last shape is held.".to_string(),
        ),
        Status::NoTake => (
            "No take selected",
            Studio::TEXT_FAINT,
            Studio::TEXT_SECONDARY,
            false,
            "Select a recorded take, or switch to Live.".to_string(),
        ),
    };
    let dot_widget = stack![
        container(
            container(Space::new())
                .width(10)
                .height(10)
                .style(move |_: &Theme| boxed(
                    Some(alpha(dot, if glow { 0.28 } else { 0.0 })),
                    5.0,
                    None
                ))
        )
        .center(10),
        container(
            container(Space::new())
                .width(6)
                .height(6)
                .style(move |_: &Theme| boxed(Some(dot), 3.0, None))
        )
        .center(10),
    ];
    let fit_row = row![dot_widget, line(head.to_string(), head_color)]
        .spacing(4)
        .align_y(Vertical::Center);
    let mut second_line = line(second, Studio::TEXT_SECONDARY);
    if matches!(status, Status::Voiced { .. }) {
        second_line = second_line.tabular();
    }
    column![
        fit_row,
        second_line,
        line(CAVEAT.to_string(), Studio::TEXT_DISABLED).width(Length::Fill),
    ]
    .spacing(gap)
    .padding(Padding {
        top: 2.0,
        ..Padding::ZERO
    })
    .width(Length::Fill)
    .align_x(Horizontal::Left)
    .into()
}
