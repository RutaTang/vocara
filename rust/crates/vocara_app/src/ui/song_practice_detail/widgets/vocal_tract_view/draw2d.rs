//! The 2D side cutaway, drawn from the pose's centimetres at one uniform
//! scale fitted to the well ([`Fit2d`]): the head and neck, the fixed bone
//! and cartilage, the moving parts, the airway between the walls, and the
//! callouts. The layers and their colours are the approved mockup's; its
//! blur and radial gradients are drawn as a few translucent passes.

use iced::alignment::Vertical;
use iced::widget::canvas::{
    self, gradient, Frame, Gradient, LineCap, LineDash, LineJoin, Path, Stroke,
};
use iced::widget::text::Alignment;
use iced::{Color, Pixels, Point, Size};

use super::anatomy::{anatomy, Anatomy};
use super::callouts::{self, Side};
use super::geometry::{bounds, catmull, dorsum};
use super::pose::{Cm, TractPose};
use crate::ui::core::text;
use crate::ui::core::theme::{alpha, font, Studio};

/// The part of the head the well frames (cm): the mockup's view box, wide
/// enough for the callouts either side, from below the larynx to just under
/// the crown (whose top fades out at the frame's edge).
pub const FRAME_MIN: Cm = Cm::new(-26.0, -4.727);
pub const FRAME_MAX: Cm = Cm::new(28.545, 22.545);

/// The mockup's scale (px per cm), which its stroke widths are for.
pub const MOCKUP_SCALE: f32 = 11.0;

/// How the figure sits in a well: one scale for both axes (the frame fitted
/// inside, never stretched) and where the glottis at rest lands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit2d {
    /// Pixels per cm.
    pub scale: f32,
    /// The pixel of the origin (the glottis at rest).
    pub origin: Point,
}

impl Fit2d {
    pub fn new(size: Size) -> Self {
        let span = FRAME_MAX.minus(FRAME_MIN);
        let scale = (size.width / span.x).min(size.height / span.y).max(0.0);
        let mid = FRAME_MIN.lerp(FRAME_MAX, 0.5);
        Self {
            scale,
            origin: Point::new(
                size.width / 2.0 - mid.x * scale,
                size.height / 2.0 + mid.y * scale,
            ),
        }
    }

    /// The pixel of a point (cm).
    pub fn px(&self, p: Cm) -> Point {
        Point::new(
            self.origin.x + p.x * self.scale,
            self.origin.y - p.y * self.scale,
        )
    }

    /// A stroke width given at the mockup's scale, at this one (kept
    /// within reach of the mockup's so lines neither vanish nor thicken).
    pub fn w(&self, mockup: f32) -> f32 {
        mockup * (self.scale / MOCKUP_SCALE).clamp(0.85, 1.25)
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

const SKIN: Color = rgb(0x1B, 0x1B, 0x21);
const SKIN_FOOT: Color = rgb(0x17, 0x17, 0x1C);
const SKIN_EDGE: Color = rgb(0x3A, 0x3A, 0x43);
const FAINT_LINE: Color = Studio::BORDER_STRONG;
const VERTEBRA: Color = rgb(0x1E, 0x1E, 0x24);
const VERTEBRA_EDGE: Color = rgb(0x2C, 0x2C, 0x34);
const NASAL: Color = rgb(0x12, 0x12, 0x16);
pub const BONE_TOP: Color = rgb(0x3C, 0x3C, 0x45);
const BONE_FOOT: Color = rgb(0x2B, 0x2B, 0x32);
const BONE_EDGE: Color = rgb(0x4E, 0x4E, 0x58);
const AIR: Color = Studio::IN_TUNE;
const WALL: Color = rgb(0x45, 0x45, 0x4F);
const VELUM: Color = rgb(0xC4, 0x7B, 0x6E);
const CORAL_EDGE: Color = rgb(0xE6, 0xA4, 0x98);
const TONGUE_GLOW: Color = rgb(0xD9, 0x8C, 0x7E);
pub const TONGUE_TOP: Color = rgb(0xE9, 0xA9, 0x9C);
pub const TONGUE_FOOT: Color = rgb(0xB5, 0x66, 0x5A);
const TONGUE_EDGE: Color = rgb(0xF2, 0xB6, 0xAA);
const LIP_BACK: Color = rgb(0xC2, 0x76, 0x68);
const TEETH: Color = rgb(0xE6, 0xE2, 0xDA);
const LARYNX: Color = Studio::TRACK_PERIWINKLE;
const LEADER: Color = WALL;

fn fade(c: Color, k: f32) -> Color {
    Color { a: c.a * k, ..c }
}

/// A path through `pts` at `fit`, closed or open.
pub fn poly(fit: &Fit2d, pts: &[Cm], closed: bool) -> Path {
    Path::new(|b| {
        let mut it = pts.iter();
        if let Some(first) = it.next() {
            b.move_to(fit.px(*first));
            for p in it {
                b.line_to(fit.px(*p));
            }
            if closed {
                b.close();
            }
        }
    })
}

/// A vertical gradient across the pixels `top` to `bottom`.
fn vertical(top: f32, bottom: f32, stops: &[(f32, Color)]) -> canvas::Fill {
    let mut g = gradient::Linear::new(Point::new(0.0, top), Point::new(0.0, bottom));
    for &(at, c) in stops {
        g = g.add_stop(at, c);
    }
    canvas::Fill::from(Gradient::Linear(g))
}

/// `pts`' box on screen: (top-left, bottom-right).
fn px_box(fit: &Fit2d, pts: &[Cm]) -> (Point, Point) {
    let (lo, hi) = bounds(pts);
    (fit.px(Cm::new(lo.x, hi.y)), fit.px(Cm::new(hi.x, lo.y)))
}

/// The bone gradient over `pts`' box.
fn bone(fit: &Fit2d, pts: &[Cm], k: f32) -> canvas::Fill {
    let (a, b) = px_box(fit, pts);
    vertical(
        a.y,
        b.y,
        &[(0.0, fade(BONE_TOP, k)), (1.0, fade(BONE_FOOT, k))],
    )
}

fn stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke::default()
        .with_color(color)
        .with_width(width)
        .with_line_join(LineJoin::Round)
}

/// The well's soft light: the mockup's radial gradient (lighter a little
/// above the middle, darker toward the corners) as a few nested ellipses,
/// each a shade lighter, over the well's own corner shade.
pub fn draw_well_glow(frame: &mut Frame, size: Size) {
    let centre = Point::new(size.width * 0.5, size.height * 0.45);
    let inner = rgb(0x15, 0x15, 0x1A);
    let outer = rgb(0x0E, 0x0E, 0x11);
    let steps = 8;
    for i in 0..steps {
        let f = 0.48 * (1.0 - i as f32 / steps as f32);
        let t = ((f - 0.03) / 0.7).clamp(0.0, 1.0);
        let c = Color::from_rgb(
            inner.r + (outer.r - inner.r) * t,
            inner.g + (outer.g - inner.g) * t,
            inner.b + (outer.b - inner.b) * t,
        );
        let (rx, ry) = (f * 1.2 * size.width, f * 1.4 * size.height);
        frame.fill(
            &Path::new(|b| {
                b.ellipse(canvas::path::arc::Elliptical {
                    center: centre,
                    radii: iced::Vector::new(rx, ry),
                    rotation: iced::Radians(0.0),
                    start_angle: iced::Radians(0.0),
                    end_angle: iced::Radians(std::f32::consts::TAU),
                });
            }),
            c,
        );
    }
}

/// The well's corner shade, under [`draw_well_glow`].
pub const WELL_BASE: Color = rgb(0x10, 0x10, 0x13);

fn ellipse(fit: &Fit2d, centre: Cm, half: Cm) -> Path {
    Path::new(|b| {
        b.ellipse(canvas::path::arc::Elliptical {
            center: fit.px(centre),
            radii: iced::Vector::new(half.x * fit.scale, half.y * fit.scale),
            rotation: iced::Radians(0.0),
            start_angle: iced::Radians(0.0),
            end_angle: iced::Radians(std::f32::consts::TAU),
        });
    })
}

/// Draws the figure for `pose` in a well of `size`; `rest` (0 to 1) dims it
/// as the voice stops.
pub fn draw_figure(frame: &mut Frame, size: Size, pose: &TractPose, rest: f32) {
    let fit = Fit2d::new(size);
    if fit.scale <= 0.0 {
        return;
    }
    let a = anatomy();
    let o = &pose.outlines;
    let moving = 1.0 - 0.32 * rest;
    let air_k = 1.0 - 0.45 * rest;
    draw_head(frame, &fit, a);
    // The nasal cavity, then bone: the mandible (on the jaw) and the palate.
    frame.fill(&poly(&fit, &o.nasal, true), NASAL);
    frame.stroke(
        &poly(&fit, &o.nasal, true),
        stroke(Studio::BORDER, fit.w(1.0)),
    );
    frame.fill(&poly(&fit, &o.jaw, true), bone(&fit, &o.jaw, 0.9));
    frame.fill(&poly(&fit, &a.palate, true), bone(&fit, &a.palate, 1.0));
    // The airway, its centreline dotted.
    if o.outer_wall.len() > 1 && o.inner_wall.len() > 1 {
        let mut air = o.outer_wall.clone();
        air.extend(&o.inner_wall);
        let (tl, br) = px_box(&fit, &air);
        let g = gradient::Linear::new(Point::new(br.x, tl.y), Point::new(tl.x, br.y))
            .add_stop(0.0, alpha(AIR, 0.32 * air_k))
            .add_stop(1.0, alpha(AIR, 0.12 * air_k));
        frame.fill(
            &poly(&fit, &air, true),
            canvas::Fill::from(Gradient::Linear(g)),
        );
    }
    let centres: Vec<Cm> = pose.rings.iter().map(|r| r.centre).collect();
    if centres.len() > 1 {
        let line = catmull(&centres, 4);
        let dash = [fit.w(1.0), fit.w(3.5)];
        frame.stroke(
            &poly(&fit, &line, false),
            Stroke {
                line_dash: LineDash {
                    segments: &dash,
                    offset: 0,
                },
                ..stroke(alpha(AIR, 0.45 * air_k), fit.w(1.0)).with_line_cap(LineCap::Round)
            },
        );
    }
    // The posterior pharyngeal wall.
    frame.stroke(
        &poly(&fit, &o.pharynx, false),
        stroke(WALL, fit.w(2.0)).with_line_cap(LineCap::Round),
    );
    // The velum.
    frame.fill(&poly(&fit, &o.velum, true), fade(VELUM, moving));
    frame.stroke(
        &poly(&fit, &o.velum, true),
        stroke(fade(CORAL_EDGE, moving), fit.w(1.0)),
    );
    draw_tongue(frame, &fit, &o.tongue, moving);
    // The teeth, then the lips over them.
    frame.fill(&poly(&fit, &o.upper_teeth, true), TEETH);
    frame.fill(&poly(&fit, &o.lower_teeth, true), TEETH);
    for lip in [&o.upper_lip, &o.lower_lip] {
        let (l, r) = px_box(&fit, lip);
        let g = gradient::Linear::new(Point::new(l.x, 0.0), Point::new(r.x, 0.0))
            .add_stop(0.0, fade(LIP_BACK, moving))
            .add_stop(1.0, fade(CORAL_EDGE, moving));
        frame.fill(
            &poly(&fit, lip, true),
            canvas::Fill::from(Gradient::Linear(g)),
        );
        frame.stroke(
            &poly(&fit, lip, true),
            stroke(fade(TONGUE_EDGE, moving), fit.w(0.8)),
        );
    }
    // The epiglottis, the hyoid and the larynx with the folds, on the
    // glottis.
    frame.fill(&poly(&fit, &o.epiglottis, true), BONE_TOP);
    frame.stroke(
        &poly(&fit, &o.epiglottis, true),
        stroke(BONE_EDGE, fit.w(0.8)),
    );
    let lift = pose.glottis;
    let hyoid = ellipse(&fit, a.hyoid.0.plus(lift), a.hyoid.1);
    let (hc, hh) = (fit.px(a.hyoid.0.plus(lift)), a.hyoid.1.y * fit.scale);
    frame.fill(
        &hyoid,
        vertical(hc.y - hh, hc.y + hh, &[(0.0, BONE_TOP), (1.0, BONE_FOOT)]),
    );
    frame.stroke(&hyoid, stroke(BONE_EDGE, fit.w(0.8)));
    let larynx: Vec<Cm> = a.larynx.iter().map(|p| p.plus(lift)).collect();
    frame.fill(&poly(&fit, &larynx, true), alpha(LARYNX, 0.10 * moving));
    frame.stroke(
        &poly(&fit, &larynx, true),
        stroke(fade(LARYNX, moving), fit.w(1.2)),
    );
    draw_folds(frame, &fit, a, lift, rest);
    draw_callouts(frame, &fit, size, pose);
}

/// The head and neck (its crown fading out above the frame), the eye and
/// the ear, and the cervical vertebrae.
fn draw_head(frame: &mut Frame, fit: &Fit2d, a: &Anatomy) {
    let top = fit.px(Cm::new(0.0, a.head_top)).y;
    let foot = fit.px(Cm::new(0.0, a.head_foot)).y;
    let head = poly(fit, &a.head, false);
    frame.fill(
        &poly(fit, &a.head, true),
        vertical(
            top,
            foot,
            &[(0.0, alpha(SKIN, 0.0)), (0.16, SKIN), (1.0, SKIN_FOOT)],
        ),
    );
    let mut edge = gradient::Linear::new(Point::new(0.0, top), Point::new(0.0, foot));
    edge = edge
        .add_stop(0.0, alpha(SKIN_EDGE, 0.0))
        .add_stop(0.18, SKIN_EDGE)
        .add_stop(1.0, SKIN_EDGE);
    frame.stroke(
        &head,
        Stroke {
            style: canvas::Style::Gradient(Gradient::Linear(edge)),
            ..stroke(SKIN_EDGE, fit.w(1.2))
        },
    );
    frame.stroke(
        &Path::circle(fit.px(a.eye.0), a.eye.1 * fit.scale),
        stroke(FAINT_LINE, fit.w(1.0)),
    );
    frame.stroke(&poly(fit, &a.ear, false), stroke(FAINT_LINE, fit.w(1.0)));
    for v in &a.vertebrae {
        let tl = fit.px(Cm::new(v.left, v.top));
        let rect = Path::rounded_rectangle(
            tl,
            Size::new(v.width * fit.scale, v.height * fit.scale),
            (v.radius * fit.scale).into(),
        );
        frame.fill(&rect, VERTEBRA);
        frame.stroke(&rect, stroke(VERTEBRA_EDGE, fit.w(1.0)));
    }
}

/// The tongue: a soft coral glow (the mockup's blur, as three widening
/// translucent outlines), its gradient body and a highlight along the
/// dorsum.
fn draw_tongue(frame: &mut Frame, fit: &Fit2d, tongue: &[Cm], k: f32) {
    if tongue.len() < 3 {
        return;
    }
    let path = poly(fit, tongue, true);
    for (width, a) in [(12.0, 0.05), (8.0, 0.07), (4.0, 0.09)] {
        frame.stroke(&path, stroke(alpha(TONGUE_GLOW, a * k), fit.w(width)));
    }
    let (tl, br) = px_box(fit, tongue);
    frame.fill(
        &path,
        vertical(
            tl.y,
            br.y,
            &[(0.0, fade(TONGUE_TOP, k)), (1.0, fade(TONGUE_FOOT, k))],
        ),
    );
    frame.stroke(&path, stroke(fade(TONGUE_EDGE, k), fit.w(1.0)));
    let hi = highlight(tongue);
    if hi.len() > 1 {
        frame.stroke(
            &poly(fit, &hi, false),
            stroke(alpha(Color::WHITE, 0.2 * k), fit.w(2.4)).with_line_cap(LineCap::Round),
        );
    }
}

/// The tongue's highlight: the crown of its dorsum, set 3.5 mm in.
pub fn highlight(tongue: &[Cm]) -> Vec<Cm> {
    let back = dorsum(tongue);
    let Some(top) = back.iter().copied().max_by(|a, b| a.y.total_cmp(&b.y)) else {
        return Vec::new();
    };
    let centre = Cm::new(top.x, top.y - 2.25);
    back.iter()
        .filter(|p| p.x >= top.x - 1.75 && p.x <= top.x + 1.15 && p.y >= top.y - 1.3)
        .map(|p| p.plus(centre.minus(*p).unit().scale(0.35)))
        .collect()
}

/// The vocal folds: a lit line with a halo that brightens while voiced.
fn draw_folds(frame: &mut Frame, fit: &Fit2d, a: &Anatomy, lift: Cm, rest: f32) {
    let voiced = 1.0 - rest;
    let (centre, half) = a.fold_halo;
    let passes = 6;
    for i in 0..passes {
        let f = 1.0 - i as f32 / passes as f32;
        frame.fill(
            &ellipse(fit, centre.plus(lift), half.scale(f)),
            alpha(
                Color::WHITE,
                (0.51 / passes as f32) * (0.25 + 0.75 * voiced),
            ),
        );
    }
    frame.stroke(
        &Path::line(fit.px(a.folds.0.plus(lift)), fit.px(a.folds.1.plus(lift))),
        stroke(alpha(Color::WHITE, 0.55 + 0.45 * voiced), fit.w(1.8)).with_line_cap(LineCap::Round),
    );
}

/// The callouts: leaders, dots and names.
fn draw_callouts(frame: &mut Frame, fit: &Fit2d, size: Size, pose: &TractPose) {
    let placed = callouts::place(&callouts::anchors(pose), fit, size);
    for c in &placed {
        let [dot, elbow, end] = c.leader;
        frame.stroke(
            &Path::new(|b| {
                b.move_to(dot);
                b.line_to(elbow);
                b.line_to(end);
            }),
            stroke(LEADER, 1.0),
        );
    }
    for c in &placed {
        let (dot_color, text_color) = if c.moving {
            (Studio::ACCENT_BRIGHT, Studio::TEXT_PRIMARY)
        } else {
            (Studio::TEXT_FAINT, Studio::TEXT_SECONDARY)
        };
        frame.fill(&Path::circle(c.leader[0], fit.w(2.3)), dot_color);
        frame.fill_text(canvas::Text {
            content: c.text.to_string(),
            position: c.label,
            color: text_color,
            size: Pixels(11.0),
            line_height: text::NATURAL,
            font: font::REGULAR,
            align_x: match c.side {
                Side::Left => Alignment::Right,
                Side::Right => Alignment::Left,
            },
            align_y: Vertical::Center,
            ..canvas::Text::default()
        });
    }
}
