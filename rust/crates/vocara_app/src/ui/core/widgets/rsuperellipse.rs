//! Flutter's rounded superellipse (`RSuperellipse`, the shape
//! `RoundedSuperellipseBorder` fills and strokes): Apple's "continuous
//! corners", which flow out of the straight sides along a superellipse
//! before closing on a circular arc, where an `RRect`'s corner is a
//! quarter circle. A port of the Flutter 3.38 engine's
//! `impeller/geometry/round_superellipse_param.cc` (BSD), which both its
//! renderers draw it with: each quarter of the outline is two octants, each
//! the start of a superellipse `|x/a|ⁿ + |y/a|ⁿ = 1` from the middle of a
//! side to a point J, then a circular arc from J to the corner's middle M,
//! which lies as far in from the corner as an `RRect`'s does; both drawn as
//! the engine draws them, as cubic Béziers with its fitted factors.
//!
//! Only the uniform radius the app draws (a radius on every corner, the
//! same across and down) is ported (`MakeBoundsRadius`).

// The engine's constants, digit for digit: an `f32` rounds them as its
// `float` did.
#![allow(clippy::excessive_precision)]

use iced::widget::canvas::{path, Path};
use iced::{Point, Rectangle};

/// `kGapFactor`: how far the corner's middle lies in from the corner, per
/// unit of radius (`1 - cos(π/4)`, as on a circular corner).
const GAP_FACTOR: f32 = 0.292_893_218_81;

/// `n` and `k_xJ = 1 / (1 - xJ / a)` against `ratio = 2a / radius`, at
/// 2.0..2.5 by 0.1 and 2.5..5.0 by 0.5 (`kPrecomputedVariables`).
const OCTANT_TABLE: [[f32; 2]; 11] = [
    [2.000_000_00, 1.132_766_76],
    [2.183_498_05, 1.203_119_21],
    [2.338_886_62, 1.286_987_96],
    [2.486_605_75, 1.363_519_41],
    [2.622_265_96, 1.447_179_76],
    [2.751_489_90, 1.533_858_19],
    [3.362_982_65, 1.982_882_83],
    [4.086_499_29, 2.238_118_46],
    [4.854_811_34, 2.475_634_63],
    [5.629_455_51, 2.729_485_97],
    [6.430_237_96, 2.980_204_21],
];

/// The superellipse arc's Bézier factors for its start and end tangents,
/// against `n` from 2 to 15 by 1 (`SuperellipseBezierFactors`).
const BEZIER_TABLE: [[f32; 2]; 14] = [
    [0.013_394_48, 0.059_949_73],
    [0.136_641_15, 0.135_920_82],
    [0.245_455_46, 0.140_995_16],
    [0.323_531_51, 0.128_080_21],
    [0.390_930_68, 0.117_262_64],
    [0.448_478_00, 0.108_082_78],
    [0.498_174_52, 0.100_261_75],
    [0.541_055_83, 0.093_444_29],
    [0.578_125_78, 0.087_489_84],
    [0.610_509_61, 0.082_247_22],
    [0.639_039_89, 0.077_596_39],
    [0.664_163_38, 0.073_465_30],
    [0.686_753_38, 0.069_749_96],
    [0.706_780_34, 0.065_295_12],
];

/// `ComputeNAndXj`: the superellipse's degree and `xJ / a`.
fn n_and_xj(ratio: f32) -> (f32, f32) {
    let last = OCTANT_TABLE[OCTANT_TABLE.len() - 1];
    if ratio > 5.0 {
        let n = 1.559_599_389 * (ratio - 5.0) + last[0];
        let k_xj = 0.522_807_185 * (ratio - 5.0) + last[1];
        return (n, 1.0 - 1.0 / k_xj);
    }
    let ratio = ratio.clamp(2.0, 5.0);
    let steps = if ratio < 2.5 {
        (ratio - 2.0) * 10.0
    } else {
        (ratio - 2.5) * 2.0 + 6.0 - 1.0
    };
    let left = (steps.floor().max(0.0) as usize).min(OCTANT_TABLE.len() - 2);
    let frac = steps - left as f32;
    let at = |i: usize| (1.0 - frac) * OCTANT_TABLE[left][i] + frac * OCTANT_TABLE[left + 1][i];
    (at(0), 1.0 - 1.0 / at(1))
}

/// `SuperellipseBezierFactors(n)`.
fn bezier_factors(n: f32) -> (f32, f32) {
    let max_n = 2.0 + (BEZIER_TABLE.len() - 1) as f32;
    if n >= max_n {
        return (
            1.07 - 1.307_649_835_f32.exp() * n.powf(-0.856_851_673_1),
            -0.01 + (-0.928_769_032_2_f32).exp() * n.powf(-0.612_090_139_8),
        );
    }
    let steps = (n - 2.0).clamp(0.0, (BEZIER_TABLE.len() - 1) as f32);
    let left = (steps.floor() as usize).min(BEZIER_TABLE.len() - 2);
    let frac = steps - left as f32;
    let at = |i: usize| (1.0 - frac) * BEZIER_TABLE[left][i] + frac * BEZIER_TABLE[left + 1][i];
    (at(0), at(1))
}

/// One octant of a square-like rounded superellipse of half size `a`
/// (`RoundSuperellipseParam::Octant`), centred at `offset`, in a frame
/// where y points up.
#[derive(Debug, Clone, Copy)]
struct Octant {
    offset: Point,
    a: f32,
    n: f32,
    /// J: where the superellipse hands over to the arc.
    circle_start: Point,
    circle_center: Point,
    circle_max_angle: f32,
}

/// `FindCircleCenter`: the centre of the radius-`r` circle through `a` and
/// `b`, on the side the engine picks.
fn circle_center(a: Point, b: Point, r: f32) -> Point {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let m = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let (cx, cy) = (-dy, dx);
    let length = (cx * cx + cy * cy).sqrt();
    let am = (dx * dx + dy * dy).sqrt() / 2.0;
    let cm = (r * r - am * am).sqrt();
    Point::new(m.x - cm * cx / length, m.y - cm * cy / length)
}

/// The signed angle from `u` to `v` (`Vector2::AngleTo`).
fn angle_to(u: (f32, f32), v: (f32, f32)) -> f32 {
    (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1)
}

/// `ComputeOctant`.
fn octant(offset: Point, a: f32, radius: f32) -> Octant {
    let ratio = a * 2.0 / radius;
    let g = GAP_FACTOR * radius;
    let (n, xj_ratio) = n_and_xj(ratio);
    let xj = xj_ratio * a;
    let yj = (1.0 - xj_ratio.powf(n)).powf(1.0 / n) * a;
    let tan_phi_j = (xj / yj).powf(n - 1.0);
    let d = (xj - tan_phi_j * yj) / (1.0 - tan_phi_j);
    let big_r = (a - d - g) * std::f32::consts::SQRT_2;
    let m = Point::new(a - g, a - g);
    let j = Point::new(xj, yj);
    let center = circle_center(j, m, big_r);
    let max_angle = angle_to(
        (m.x - center.x, m.y - center.y),
        (j.x - center.x, j.y - center.y),
    );
    Octant {
        offset,
        a,
        n,
        circle_start: j,
        circle_center: center,
        circle_max_angle: max_angle,
    }
}

/// A piece of the outline: a cubic Bézier's two controls and its end.
pub type Cubic = [Point; 3];

impl Octant {
    /// The superellipse arc's four Bézier points, from the side's middle
    /// (0, a) to J (`SuperellipseArcPoints`).
    fn superellipse_points(&self) -> [Point; 4] {
        let start = Point::new(0.0, self.a);
        let end = self.circle_start;
        let (vx, vy) = (end.x - self.circle_center.x, end.y - self.circle_center.y);
        let length = (vx * vx + vy * vy).sqrt();
        let tangent = (-vy / length, vx / length);
        let (f0, f1) = bezier_factors(self.n);
        [
            start,
            Point::new(start.x + f0 * self.a, start.y),
            Point::new(
                end.x + tangent.0 * f1 * self.a,
                end.y + tangent.1 * f1 * self.a,
            ),
            end,
        ]
    }

    /// The circular arc's four Bézier points, from J to M
    /// (`CircularArcPoints`).
    fn circle_points(&self) -> [Point; 4] {
        let c = self.circle_center;
        let start = (self.circle_start.x - c.x, self.circle_start.y - c.y);
        // Rotated by -max_angle (`Point::Rotate`).
        let (sin, cos) = (-self.circle_max_angle).sin_cos();
        let end = (start.0 * cos - start.1 * sin, start.0 * sin + start.1 * cos);
        let circle_end = Point::new(c.x + end.0, c.y + end.1);
        let radius = (start.0 * start.0 + start.1 * start.1).sqrt();
        let start_tangent = (start.1 / radius, -start.0 / radius);
        let end_tangent = (-end.1 / radius, end.0 / radius);
        let k = (self.circle_max_angle / 4.0).tan() * 4.0 / 3.0 * radius;
        [
            self.circle_start,
            Point::new(
                self.circle_start.x + start_tangent.0 * k,
                self.circle_start.y + start_tangent.1 * k,
            ),
            Point::new(
                circle_end.x + end_tangent.0 * k,
                circle_end.y + end_tangent.1 * k,
            ),
            circle_end,
        ]
    }

    /// `AddOctant`: its two cubics, forward from the side's middle or back
    /// to it, each point flipped across y = x if `flip`, moved by its
    /// offset and handed to `transform`.
    fn cubics(&self, reverse: bool, flip: bool, transform: &dyn Fn(Point) -> Point) -> [Cubic; 2] {
        let place = |p: Point| {
            let p = if flip { Point::new(p.y, p.x) } else { p };
            transform(Point::new(p.x + self.offset.x, p.y + self.offset.y))
        };
        let se = self.superellipse_points();
        let arc = self.circle_points();
        if reverse {
            [
                [place(arc[2]), place(arc[1]), place(arc[0])],
                [place(se[2]), place(se[1]), place(se[0])],
            ]
        } else {
            [
                [place(se[1]), place(se[2]), place(se[3])],
                [place(arc[1]), place(arc[2]), place(arc[3])],
            ]
        }
    }
}

/// The outline of `bounds` with corners of `radius`, clockwise from the
/// top side's middle (`RoundSuperellipseParam::Dispatch`): its start and
/// its cubics; a rectangle's corners, with no radius, as degenerate ones.
pub fn outline(bounds: Rectangle, radius: f32) -> (Point, Vec<Cubic>) {
    let center = bounds.center();
    let (half_w, half_h) = (bounds.width / 2.0, bounds.height / 2.0);
    let radius = radius.max(0.0);
    let start = Point::new(center.x, bounds.y);
    if radius == 0.0 {
        let corners = [
            Point::new(bounds.x + bounds.width, bounds.y),
            Point::new(bounds.x + bounds.width, bounds.y + bounds.height),
            Point::new(bounds.x, bounds.y + bounds.height),
            Point::new(bounds.x, bounds.y),
        ];
        let cubics = corners.iter().map(|&c| [c, c, c]).chain([[start; 3]]);
        return (start, cubics.collect());
    }
    // `ComputeQuadrant` for the top right, with the same radius across and
    // down: the y axis flipped up (`signed_scale` (1, -1)).
    let c = half_w - half_h;
    let top = octant(Point::new(0.0, -c), half_w, radius);
    let right = octant(Point::new(c, 0.0), half_h, radius);
    let mut cubics = Vec::with_capacity(16);
    // The four quadrants: `AddQuadrant` with `scale_sign` (1, 1), (1, -1),
    // (-1, -1), (-1, 1), the second and the fourth run backwards.
    for (sx, sy, reverse) in [
        (1.0, 1.0, false),
        (1.0, -1.0, true),
        (-1.0, -1.0, false),
        (-1.0, 1.0, true),
    ] {
        let transform = move |p: Point| Point::new(center.x + sx * p.x, center.y - sy * p.y);
        let halves = if reverse {
            [
                right.cubics(false, true, &transform),
                top.cubics(true, false, &transform),
            ]
        } else {
            [
                top.cubics(false, false, &transform),
                right.cubics(true, true, &transform),
            ]
        };
        cubics.extend(halves.into_iter().flatten());
    }
    (start, cubics)
}

/// The outline as a path to fill or stroke.
pub fn path(bounds: Rectangle, radius: f32) -> Path {
    let (start, cubics) = outline(bounds, radius);
    let mut builder = path::Builder::new();
    builder.move_to(start);
    for [a, b, to] in cubics {
        builder.bezier_curve_to(a, b, to);
    }
    builder.close();
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    /// The toolbar's: 222 wide, three items, radius 8.
    fn toolbar() -> Rectangle {
        Rectangle::new(Point::new(10.0, 20.0), iced::Size::new(222.0, 83.8))
    }

    #[test]
    fn the_outline_runs_round_the_box_and_closes() {
        let b = toolbar();
        let (start, cubics) = outline(b, 8.0);
        assert!(
            close(start, Point::new(b.center_x(), b.y)),
            "the top's middle"
        );
        assert_eq!(cubics.len(), 16, "two cubics an octant");
        assert!(close(cubics[15][2], start), "back where it began");
        // Each octant's superellipse starts at a side's middle.
        assert!(close(cubics[3][2], Point::new(b.x + b.width, b.center_y())));
        assert!(close(
            cubics[7][2],
            Point::new(b.center_x(), b.y + b.height)
        ));
        assert!(close(cubics[11][2], Point::new(b.x, b.center_y())));
    }

    #[test]
    fn a_corners_middle_lies_as_far_in_as_a_circular_corners() {
        // M, where the two octants' arcs meet, sits `kGapFactor * radius`
        // in from both sides, as an RRect corner's middle does.
        let b = toolbar();
        let (_, cubics) = outline(b, 8.0);
        let g = GAP_FACTOR * 8.0;
        let m = cubics[1][2];
        assert!(close(m, Point::new(b.x + b.width - g, b.y + g)), "{m:?}");
        let m = cubics[9][2];
        assert!(close(m, Point::new(b.x + g, b.y + b.height - g)), "{m:?}");
    }

    #[test]
    fn j_lies_on_the_superellipse_and_the_arc_through_m() {
        let o = octant(Point::ORIGIN, 41.9, 8.0);
        let j = o.circle_start;
        let on = (j.x / o.a).powf(o.n) + (j.y / o.a).powf(o.n);
        assert!((on - 1.0).abs() < 1e-3, "{on}");
        let g = GAP_FACTOR * 8.0;
        let m = Point::new(o.a - g, o.a - g);
        let r = |p: Point| p.distance(o.circle_center);
        assert!((r(j) - r(m)).abs() < 1e-3);
        // Straighter than a circle: J lies past where a radius-8 quarter
        // circle would leave the side.
        assert!(j.x > o.a - 8.0, "{j:?}");
    }

    #[test]
    fn a_square_corner_has_no_curve() {
        let b = toolbar();
        let (_, cubics) = outline(b, 0.0);
        assert!(close(cubics[0][2], Point::new(b.x + b.width, b.y)));
    }
}
