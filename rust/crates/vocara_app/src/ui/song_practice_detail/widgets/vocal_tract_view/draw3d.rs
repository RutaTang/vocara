//! The 3D view: the head turned by the camera ([`Projection`]) and drawn as
//! projected lines on a canvas — a proportional head shell as a depth-cued
//! wireframe (its far lines fainter), the cervical spine, the airway as
//! glowing rings along the tract's centreline (each sized from the area
//! function), the tongue's ribs, the mandible's U, the palate's arch and
//! the larynx.

use std::f32::consts::{PI, TAU};

use iced::widget::canvas::{Frame, LineCap, LineDash, LineJoin, Path, Stroke};
use iced::{Color, Point, Size};

use super::anatomy::{anatomy, CONDYLE};
use super::camera::{Camera, Projection, P3};
use super::geometry::{dorsum, length, resample, rotate};
use super::pose::{Cm, TractPose};
use crate::ui::core::theme::{alpha, Studio};

/// The head shell: an ellipsoid around the cranium, the face and the
/// tract, from below the glottis to the crown (cm).
pub const SHELL_CENTRE: P3 = P3::new(1.0, 11.0, 0.0);
pub const SHELL_HALF: P3 = P3::new(8.0, 13.0, 7.0);

const SHELL: Color = Color::from_rgb8(0x8A, 0x8A, 0x96);
/// The shell's meridians (each a whole ellipse through both poles).
const MERIDIANS: usize = 8;
const DISC: Color = Color::from_rgb8(0x1C, 0x1C, 0x22);
const DISC_EDGE: Color = Studio::GRID_STRONG;
const RIB: Color = Color::from_rgb8(0xE6, 0xA4, 0x98);
const RING: Color = Studio::ACCENT_BRIGHT;
const MANDIBLE: Color = Studio::TEXT_SECONDARY;
const PALATE: Color = Studio::TEXT_DISABLED;
const LARYNX: Color = Studio::TRACK_PERIWINKLE;

fn stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke::default()
        .with_color(color)
        .with_width(width)
        .with_line_join(LineJoin::Round)
        .with_line_cap(LineCap::Round)
}

fn polyline(pts: &[Point], closed: bool) -> Path {
    Path::new(|b| {
        let mut it = pts.iter();
        if let Some(first) = it.next() {
            b.move_to(*first);
            for p in it {
                b.line_to(*p);
            }
            if closed {
                b.close();
            }
        }
    })
}

fn p3(c: Cm, z: f32) -> P3 {
    P3::new(c.x, c.y, z)
}

/// The shell's lines: meridians through its poles, then parallels across
/// (drawn fainter).
pub fn shell_lines() -> Vec<Vec<P3>> {
    let point = |lat: f32, lon: f32| {
        P3::new(
            SHELL_CENTRE.x + SHELL_HALF.x * lat.cos() * lon.cos(),
            SHELL_CENTRE.y + SHELL_HALF.y * lat.sin(),
            SHELL_CENTRE.z + SHELL_HALF.z * lat.cos() * lon.sin(),
        )
    };
    let mut lines = Vec::new();
    for k in 0..MERIDIANS {
        let lon = k as f32 / MERIDIANS as f32 * PI;
        lines.push(
            (0..=72)
                .map(|i| point(i as f32 / 72.0 * TAU, lon))
                .collect(),
        );
    }
    for lat in [-0.62f32, -0.22, 0.18, 0.55, 0.9] {
        lines.push(
            (0..=72)
                .map(|i| point(lat, i as f32 / 72.0 * TAU))
                .collect(),
        );
    }
    lines
}

/// Draws the scene for `pose` through `camera` in a well of `size`; `rest`
/// (0 to 1) dims it as the voice stops.
pub fn draw_scene(frame: &mut Frame, size: Size, pose: &TractPose, camera: Camera, rest: f32) {
    let proj = Projection::new(camera, size);
    if proj.scale <= 0.0 {
        return;
    }
    let live = 1.0 - 0.35 * rest;
    draw_shell(frame, &proj);
    // The spine: each vertebral body a disc.
    for v in &anatomy().vertebrae {
        let c = v.centre();
        let disc: Vec<Point> = (0..32)
            .map(|i| {
                let t = i as f32 / 32.0 * TAU;
                proj.point(P3::new(c.x + 0.95 * t.cos(), c.y, 0.95 * t.sin()))
            })
            .collect();
        let path = polyline(&disc, true);
        frame.fill(&path, DISC);
        frame.stroke(&path, stroke(DISC_EDGE, 1.0));
    }
    draw_rings(frame, &proj, pose, live);
    draw_ribs(frame, &proj, pose, live);
    // The mandible's U, on the jaw.
    let side = [
        (-2.5, 6.4, 4.7),
        (-2.4, 5.6, 4.6),
        (-1.6, 4.2, 4.2),
        (1.0, 3.3, 3.4),
        (3.6, 2.9, 2.3),
        (5.3, 3.0, 1.1),
    ];
    let mut u: Vec<P3> = side
        .iter()
        .map(|&(x, y, z)| p3(rotate(Cm::new(x, y), CONDYLE, -pose.jaw_angle), -z))
        .collect();
    u.push(p3(rotate(Cm::new(5.8, 3.2), CONDYLE, -pose.jaw_angle), 0.0));
    u.extend(
        side.iter()
            .rev()
            .map(|&(x, y, z)| p3(rotate(Cm::new(x, y), CONDYLE, -pose.jaw_angle), z)),
    );
    let pts: Vec<Point> = u.iter().map(|p| proj.point(*p)).collect();
    frame.stroke(&polyline(&pts, false), stroke(MANDIBLE, 1.5));
    // The palate's arch: its oral surface, as wide as the dental arch.
    let surface = resample(&anatomy().palate_surface, 14, false);
    let n = surface.len().max(2) - 1;
    let half = |i: usize| {
        let u = i as f32 / n as f32;
        0.35 + 1.75 * (1.0 - u * u)
    };
    let mut arch: Vec<Point> = surface
        .iter()
        .enumerate()
        .map(|(i, c)| proj.point(p3(*c, -half(i))))
        .collect();
    arch.extend(
        surface
            .iter()
            .enumerate()
            .rev()
            .map(|(i, c)| proj.point(p3(*c, half(i)))),
    );
    frame.stroke(&polyline(&arch, false), stroke(PALATE, 1.2));
    // The larynx: the outline of a box about the glottis.
    let g = pose.glottis;
    let mut corners = Vec::with_capacity(8);
    for x in [-0.95, 2.05] {
        for y in [-2.75, 1.15] {
            for z in [-1.35, 1.35] {
                corners.push(proj.point(P3::new(g.x + x, g.y + y, z)));
            }
        }
    }
    let hull = convex_hull(&corners);
    let path = polyline(&hull, true);
    frame.fill(&path, alpha(LARYNX, 0.08 * live));
    frame.stroke(&path, stroke(alpha(LARYNX, live), 1.2));
}

/// The shell, each segment as faint as it is far: its lines in five depth
/// bands, near ones brighter, the parallels fainter than the meridians.
fn draw_shell(frame: &mut Frame, proj: &Projection) {
    const BANDS: usize = 5;
    let mut bands: Vec<Vec<(Point, Point)>> = vec![Vec::new(); 2 * BANDS];
    let reach = SHELL_HALF.x.max(SHELL_HALF.y).max(SHELL_HALF.z);
    for (k, line) in shell_lines().iter().enumerate() {
        let parallel = usize::from(k >= MERIDIANS);
        let pts: Vec<(Point, f32)> = line.iter().map(|p| proj.project(*p)).collect();
        for w in pts.windows(2) {
            let depth = (w[0].1 + w[1].1) / 2.0;
            let t = ((depth / reach + 1.0) / 2.0).clamp(0.0, 0.999);
            bands[parallel * BANDS + (t * BANDS as f32) as usize].push((w[0].0, w[1].0));
        }
    }
    for (i, segments) in bands.iter().enumerate() {
        if segments.is_empty() {
            continue;
        }
        let near = (i % BANDS) as f32 / (BANDS - 1) as f32;
        let a = (0.06 + 0.16 * near) * if i >= BANDS { 0.6 } else { 1.0 };
        let path = Path::new(|b| {
            for (p, q) in segments {
                b.move_to(*p);
                b.line_to(*q);
            }
        });
        frame.stroke(&path, stroke(alpha(SHELL, a), 1.0));
    }
}

/// The airway: a ring across the tract at each cross-section (the glottis
/// white, the rest coral, brighter toward the lips), each with a soft glow,
/// and the centreline dotted.
fn draw_rings(frame: &mut Frame, proj: &Projection, pose: &TractPose, k: f32) {
    let n = pose.rings.len();
    let rings: Vec<(Vec<Point>, Color)> = pose
        .rings
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let across = Cm::new(-r.normal.y, r.normal.x);
            let pts = (0..40)
                .map(|j| {
                    let t = j as f32 / 40.0 * TAU;
                    let c = r.centre.plus(across.scale(r.radius * t.cos()));
                    proj.point(P3::new(c.x, c.y, r.radius * t.sin()))
                })
                .collect();
            let color = if i == 0 {
                Studio::TEXT_PRIMARY
            } else {
                alpha(RING, (0.35 + 0.45 * i as f32 / (n.max(2) - 1) as f32) * k)
            };
            (pts, color)
        })
        .collect();
    for (pts, color) in &rings {
        let path = polyline(pts, true);
        frame.stroke(
            &path,
            stroke(
                Color {
                    a: color.a * 0.10,
                    ..*color
                },
                6.0,
            ),
        );
        frame.stroke(
            &path,
            stroke(
                Color {
                    a: color.a * 0.22,
                    ..*color
                },
                3.2,
            ),
        );
    }
    for (pts, color) in &rings {
        frame.stroke(&polyline(pts, true), stroke(*color, 1.1));
    }
    let centre: Vec<Point> = pose
        .rings
        .iter()
        .map(|r| proj.point(p3(r.centre, 0.0)))
        .collect();
    if centre.len() > 1 {
        frame.stroke(
            &polyline(&centre, false),
            Stroke {
                line_dash: LineDash {
                    segments: &[1.0, 3.0],
                    offset: 0,
                },
                ..stroke(alpha(Studio::IN_TUNE, 0.5 * k), 1.1)
            },
        );
    }
}

/// The tongue's ribs: lines across its dorsum, each dropping toward the
/// tongue's edges.
fn draw_ribs(frame: &mut Frame, proj: &Projection, pose: &TractPose, k: f32) {
    let back = dorsum(&pose.outlines.tongue);
    if back.len() < 2 || length(back, false) <= 0.0 {
        return;
    }
    let along = resample(back, 40, false);
    let color = alpha(RIB, k);
    for i in 0..11 {
        let u = 0.12 + 0.8 * i as f32 / 10.0;
        let c = along[((u * (along.len() - 1) as f32) as usize).min(along.len() - 1)];
        let half = 2.2 * (1.0 - 0.45 * u);
        let pts: Vec<Point> = (0..=12)
            .map(|j| {
                let z = -half + 2.0 * half * j as f32 / 12.0;
                let drop = 0.55 * (z / half).powi(2);
                proj.point(P3::new(c.x, c.y - drop, z))
            })
            .collect();
        frame.stroke(&polyline(&pts, false), stroke(color, 1.3));
    }
}

/// The convex hull of `pts` (counter-clockwise on screen).
pub fn convex_hull(pts: &[Point]) -> Vec<Point> {
    let mut p: Vec<Point> = pts.to_vec();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    if p.len() < 3 {
        return p;
    }
    let cross =
        |o: Point, a: Point, b: Point| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Point> = Vec::with_capacity(p.len() * 2);
    for &q in &p {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
            hull.pop();
        }
        hull.push(q);
    }
    let lower = hull.len() + 1;
    for &q in p.iter().rev().skip(1) {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
            hull.pop();
        }
        hull.push(q);
    }
    hull.pop();
    hull
}
