//! The head around the vocal tract, on the midline (port of CHIMERA's
//! `src/ui/render/profile.ts`, `buildProfile` and its helpers): the skin's profile, the skull,
//! the brain, the eye, the vertebrae and the spinal cord, the nasal cavity and turbinates, the
//! palate, the mandible's midline, the hyoid, the thyroid, the trachea and its rings, the
//! tongue's fibres, the vocal folds, the lower teeth, and the airway's walls as the 2D view draws
//! them, all built from the anatomy's frame. The 2D outlines draw these; the 3D head grows its
//! shapes from them.
//!
//! The constants are CHIMERA's (template centimetres, mostly through the tract's template → body
//! map `p`, the face's profile through [`body_point`], which does not compress the oral region's
//! x, and the jaw-carried points through the jaw's rotation). Some of CHIMERA's offsets are in
//! body centimetres unscaled (a trachea ring's 0.05 step, the folds' 0.1); they are kept.

use crate::anatomy::{HumanArtic, HumanBody, HumanFrame, HumanVocalTract};
use crate::geom::{catmull_rom_into, dist, rotate_about, v, Vec2};
use crate::math::{cos, sin, PI};

/// The posterior pharyngeal wall's x at template height y (template cm).
pub(crate) fn wall_x(y: f64) -> f64 {
    -1.2 - 0.025 * y
}

/// The face's profile in body coordinates (the tract's map compresses the oral region's x,
/// which would flatten the nose): template (x, y) → body.
pub(crate) fn body_point(body: &HumanBody, x: f64, y: f64) -> Vec2 {
    let k = body.scale;
    let ph = body.pharynx;
    v(x * k, (if y < 7.0 { 7.0 - (7.0 - y) * ph } else { y }) * k)
}

/// The jaw's rotation (radians, about the condyle) for an opening at the incisors.
pub(crate) fn jaw_angle(t: &HumanVocalTract, jaw: f64) -> f64 {
    (-jaw.clamp(-0.2, 3.0) * t.scale()) / dist(t.p(-2.5, 10.5), t.p(6.95, 7.75))
}

/// An ellipse of n + 1 points (closed); `rot` is in screen space (y down) as CHIMERA's MRI
/// renderer had it, so negated here.
pub(crate) fn ellipse(c: Vec2, rx: f64, ry: f64, rot: f64, n: usize, out: &mut Vec<Vec2>) {
    let (cr, sr) = (cos(-rot), sin(-rot));
    for i in 0..=n {
        let an = (i as f64 / n as f64) * PI * 2.0;
        let (x, y) = (cos(an) * rx, sin(an) * ry);
        out.push(v(c.x + x * cr - y * sr, c.y + x * sr + y * cr));
    }
}

fn round_rect(p0: Vec2, p1: Vec2, r: f64, out: &mut Vec<Vec2>) {
    let (x0, x1) = (p0.x.min(p1.x), p0.x.max(p1.x));
    let (y0, y1) = (p0.y.min(p1.y), p0.y.max(p1.y));
    let rr = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    let start = out.len();
    let mut corner = |cx: f64, cy: f64, a0: f64| {
        for i in 0..=6 {
            let an = a0 + (i as f64 / 6.0) * (PI / 2.0);
            out.push(v(cx + cos(an) * rr, cy + sin(an) * rr));
        }
    };
    corner(x1 - rr, y1 - rr, 0.0);
    corner(x0 + rr, y1 - rr, PI / 2.0);
    corner(x0 + rr, y0 + rr, PI);
    corner(x1 - rr, y0 + rr, (3.0 * PI) / 2.0);
    out.push(out[start]);
}

/// The midline outlines of one frame (see the module docs); every field is rebuilt by
/// [`Profile::build`] into its own buffer.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub skin_lower: Vec<Vec2>,
    pub skin_upper: Vec<Vec2>,
    pub inset: Vec<Vec2>,
    pub skull: Vec<Vec2>,
    pub brain: Vec<Vec2>,
    pub sulcus: Vec<Vec2>,
    pub eye: Vec<Vec2>,
    pub eye_c: Vec2,
    pub vertebrae: Vec<Vec<Vec2>>,
    pub cords: [Vec<Vec2>; 2],
    pub nasal: Vec<Vec2>,
    pub turbinates: [Vec<Vec2>; 3],
    pub pal_oral: Vec<Vec2>,
    pub pal_nasal: Vec<Vec2>,
    pub jaw_lower: Vec<Vec2>,
    pub jaw_upper: Vec<Vec2>,
    pub hyoid: Vec<Vec2>,
    pub thyroid: Vec<Vec2>,
    pub trachea: [Vec<Vec2>; 2],
    pub rings: Vec<Vec<Vec2>>,
    pub outer_wall: Vec<Vec2>,
    pub glottis_wall: Vec<Vec2>,
    pub fibres: Vec<Vec<Vec2>>,
    pub folds: [Vec<Vec2>; 2],
    pub lower_teeth: Vec<Vec2>,
    /// how many of `vertebrae` and `rings` this frame has
    pub n_vertebrae: usize,
    pub n_rings: usize,
    scratch: Vec<Vec2>,
}

impl Profile {
    /// The outlines for frame `f` of articulation `a` (CHIMERA's `buildProfile`).
    pub fn build(&mut self, t: &HumanVocalTract, body: &HumanBody, f: &HumanFrame, a: &HumanArtic) {
        let k = t.scale();
        let p = |x: f64, y: f64| t.p(x, y);
        let condyle = t.p(-2.5, 10.5);
        let phi = jaw_angle(t, a.jaw);
        let j = |x: f64, y: f64| rotate_about(t.p(x, y), condyle, phi);
        let b = |x: f64, y: f64| body_point(body, x, y);
        let lar = a.larynx * k;
        // edge → nose, and edge → chin
        let ul_out = &f.upper_lip[7.min(f.upper_lip.len())..];
        let ll_out = &f.lower_lip[7.min(f.lower_lip.len())..];

        // the skin: the neck's front up to the chin and the lower lip, then the upper lip, the
        // face and the cranium round to the back of the neck
        self.skin_lower.clear();
        catmull_rom_into(
            &[
                b(2.3, -4.6),
                b(2.38, -1.5),
                b(2.52, 0.4),
                b(3.0, 1.7),
                j(5.3, 3.0),
                j(7.25, 3.35),
                j(7.95, 4.6),
                j(7.55, 5.7),
            ],
            4,
            &mut self.skin_lower,
        );
        self.skin_lower.extend(ll_out.iter().rev());
        self.skin_upper.clear();
        self.skin_upper.extend_from_slice(ul_out);
        let face_start = ul_out.last().copied().unwrap_or(b(7.05, 10.6));
        self.scratch.clear();
        catmull_rom_into(
            &[
                face_start,
                b(7.05, 10.6),
                b(7.75, 10.78),
                b(8.75, 11.05),
                b(9.25, 11.65),
                b(8.95, 12.6),
                b(8.35, 13.9),
                b(7.7, 15.0),
                b(7.95, 16.1),
                b(7.9, 18.1),
                b(6.9, 20.5),
            ],
            4,
            &mut self.scratch,
        );
        self.skin_upper.extend_from_slice(&self.scratch[1..]);
        self.scratch.clear();
        catmull_rom_into(
            &[
                b(6.9, 20.5),
                b(3.0, 22.9),
                b(-2.2, 22.7),
                b(-6.9, 19.6),
                b(-8.7, 15.4),
                b(-8.3, 11.9),
                b(-7.4, 8.9),
                b(-6.8, 4.8),
                b(-6.6, 0.0),
                b(-6.6, -4.6),
            ],
            4,
            &mut self.scratch,
        );
        self.skin_upper.extend_from_slice(&self.scratch[1..]);

        let spline = |pts: &[Vec2], per: usize, out: &mut Vec<Vec2>| {
            out.clear();
            catmull_rom_into(pts, per, out);
        };
        spline(
            &[
                b(1.85, -4.6),
                b(1.9, -0.8),
                b(2.3, 1.6),
                j(5.2, 3.6),
                j(6.9, 3.9),
                j(7.4, 4.9),
                j(7.2, 5.9),
                b(6.6, 9.9),
                b(7.6, 10.9),
                b(8.55, 11.5),
                b(8.1, 13.4),
                b(7.25, 15.2),
                b(7.45, 18.0),
                b(3.0, 22.2),
                b(-2.0, 22.0),
                b(-6.3, 19.2),
                b(-7.9, 15.3),
                b(-7.6, 12.0),
                b(-6.7, 8.7),
                b(-6.1, 4.6),
                b(-6.0, -4.6),
            ],
            3,
            &mut self.inset,
        );
        spline(
            &[
                b(7.0, 16.2),
                b(7.3, 18.5),
                b(6.0, 20.7),
                b(3.0, 22.1),
                b(-2.0, 22.0),
                b(-6.3, 19.2),
                b(-7.9, 15.3),
                b(-7.4, 12.6),
                b(-4.6, 12.2),
                b(-2.2, 13.3),
                b(1.0, 13.7),
                b(4.1, 14.1),
                b(6.2, 14.9),
            ],
            4,
            &mut self.skull,
        );
        spline(
            &[
                b(6.55, 16.3),
                b(6.75, 18.4),
                b(5.5, 20.1),
                b(2.8, 21.4),
                b(-1.9, 21.3),
                b(-5.8, 18.8),
                b(-7.1, 15.4),
                b(-6.7, 13.1),
                b(-4.4, 12.8),
                b(-2.1, 13.9),
                b(1.0, 14.3),
                b(4.0, 14.7),
                b(5.9, 15.4),
            ],
            4,
            &mut self.brain,
        );
        spline(
            &[b(5.4, 19.4), b(2.4, 20.5), b(-1.4, 20.4), b(-4.8, 18.4)],
            4,
            &mut self.sulcus,
        );
        self.eye_c = b(6.5, 15.4);
        self.eye.clear();
        ellipse(self.eye_c, 1.1 * k, 0.95 * k, 0.0, 40, &mut self.eye);

        // cervical vertebrae
        self.vertebrae.resize_with(9, Vec::new);
        self.n_vertebrae = 0;
        for i in 0..9 {
            let y0 = -4.4 + i as f64 * 1.75;
            if y0 > 10.5 {
                break;
            }
            let x0 = wall_x(y0) - 2.25;
            let out = &mut self.vertebrae[i];
            out.clear();
            round_rect(p(x0, y0 + 1.4), p(x0 + 1.6, y0), 0.32 * k, out);
            self.n_vertebrae += 1;
        }
        for (cord, pts) in self.cords.iter_mut().zip([
            &[
                p(-5.3, -4.6),
                p(-4.5, -4.6),
                p(-4.5, 11.8),
                p(-5.1, 12.4),
                p(-5.6, 11.6),
            ][..],
            &[p(-5.1, -4.6), p(-4.7, -4.6), p(-4.72, 11.4), p(-5.15, 11.6)][..],
        ]) {
            cord.clear();
            cord.extend_from_slice(pts);
        }
        spline(
            &[
                b(0.3, 11.2),
                b(1.1, 10.25),
                b(2.7, 10.75),
                b(4.6, 10.85),
                b(6.4, 10.65),
                b(7.6, 10.85),
                b(8.45, 11.3),
                b(8.0, 12.2),
                b(6.7, 13.4),
                b(4.4, 13.9),
                b(1.8, 13.6),
                b(0.2, 12.6),
            ],
            4,
            &mut self.nasal,
        );
        for (out, (cx, cy, rx, ry)) in self.turbinates.iter_mut().zip([
            (3.0, 11.45, 2.1, 0.32),
            (3.5, 12.45, 1.9, 0.3),
            (4.0, 13.3, 1.45, 0.22),
        ]) {
            out.clear();
            ellipse(b(cx, cy), rx * k, ry * k, -0.04, 40, out);
        }
        // the hard palate: its oral surface, and the nasal floor above it
        self.pal_oral.clear();
        for (x, y) in [
            (1.0, 9.6),
            (1.8, 9.92),
            (2.7, 10.18),
            (3.6, 10.3),
            (4.4, 10.25),
            (5.1, 10.05),
            (5.7, 9.72),
            (6.15, 9.36),
            (6.55, 9.15),
            (7.0, 9.6),
        ] {
            self.pal_oral.push(p(x, y));
        }
        self.pal_nasal.clear();
        self.pal_nasal
            .extend(self.pal_oral.iter().map(|q| v(q.x, q.y + 0.55 * k)));
        // the mandible's body only: its ramus is lateral, not in the midsagittal plane
        self.jaw_lower.clear();
        self.jaw_lower
            .extend(f.jaw_bone.iter().filter(|q| q.x > 0.8 * k).copied());
        self.jaw_upper.clear();
        self.jaw_upper
            .extend(self.jaw_lower.iter().map(|q| v(q.x, q.y + 0.75 * k)));
        let g = f.glottis;
        let hy = v(p(1.6, 0.0).x, p(0.0, 2.9).y + 0.6 * lar);
        self.hyoid.clear();
        ellipse(hy, 0.5 * k, 0.28 * k, 0.3, 28, &mut self.hyoid);
        self.thyroid.clear();
        for (dx, dy) in [
            (0.75, -1.6),
            (1.75, -0.6),
            (1.95, 1.2),
            (1.45, 1.9),
            (1.15, 1.1),
            (1.3, -0.4),
            (0.75, -1.6),
        ] {
            self.thyroid.push(v(g.x + dx * k, g.y + dy * k));
        }
        self.trachea[0].clear();
        self.trachea[0].extend_from_slice(&[v(g.x - 0.65 * k, g.y), v(g.x - 0.75 * k, -4.8)]);
        self.trachea[1].clear();
        self.trachea[1].extend_from_slice(&[v(g.x + 0.75 * k, g.y), v(g.x + 0.95 * k, -4.8)]);
        self.rings.resize_with(4, Vec::new);
        self.n_rings = 0;
        for i in 0..4 {
            let y = g.y - (0.9 + i as f64 * 1.0) * k;
            if y < -4.4 {
                break;
            }
            let r = &mut self.rings[i];
            r.clear();
            r.extend_from_slice(&[
                v(g.x + 0.8 * k, y),
                v(g.x + 1.05 * k, y - 0.05),
                v(g.x + 1.05 * k, y - 0.5 * k),
                v(g.x + 0.82 * k, y - 0.45 * k),
                v(g.x + 0.8 * k, y),
            ]);
            self.n_rings += 1;
        }
        // the tongue's fibres fanning from the jaw's genial tubercle
        let origin = j(6.0, 5.45);
        let tc = f.tongue_center;
        self.fibres.resize_with(17, Vec::new);
        for (i, c) in self.fibres.iter_mut().enumerate() {
            let ang = PI * (0.18 + (0.9 * i as f64) / 16.0);
            let end = v(
                tc.x + cos(ang) * f.tongue_radius * 1.25,
                tc.y + sin(ang) * f.tongue_radius * 1.25,
            );
            let ctrl = v(tc.x, (tc.y + origin.y) / 2.0);
            c.clear();
            for jj in 0..=12 {
                let u = jj as f64 / 12.0;
                c.push(v(
                    (1.0 - u) * (1.0 - u) * origin.x + 2.0 * (1.0 - u) * u * ctrl.x + u * u * end.x,
                    (1.0 - u) * (1.0 - u) * origin.y + 2.0 * (1.0 - u) * u * ctrl.y + u * u * end.y,
                ));
            }
        }
        // the airway's walls as drawn: the outer wall above the glottis, and the posterior wall
        // down to it
        self.outer_wall.clear();
        self.outer_wall
            .extend(f.outer.iter().filter(|q| q.y > g.y || q.x > 0.0).copied());
        let wall_at_glottis = v(wall_x(g.y / k) * k, g.y);
        self.glottis_wall.clear();
        self.glottis_wall.push(wall_at_glottis);
        self.glottis_wall.push(
            f.outer
                .iter()
                .find(|q| q.y > g.y)
                .copied()
                .unwrap_or(wall_at_glottis),
        );
        self.folds[0].clear();
        self.folds[0].extend_from_slice(&[
            v(g.x - 0.62 * k, g.y + 0.05),
            v(g.x - 0.1, g.y + 0.12 * k),
            v(g.x - 0.62 * k, g.y + 0.32 * k),
            v(g.x - 0.62 * k, g.y + 0.05),
        ]);
        self.folds[1].clear();
        self.folds[1].extend_from_slice(&[
            v(g.x + 0.72 * k, g.y + 0.05),
            v(g.x + 0.12, g.y + 0.12 * k),
            v(g.x + 0.72 * k, g.y + 0.32 * k),
            v(g.x + 0.72 * k, g.y + 0.05),
        ]);
        self.lower_teeth.clear();
        self.lower_teeth.extend_from_slice(&f.lower_teeth);
        self.lower_teeth.push(j(6.8, 6.0));
        self.lower_teeth.push(j(6.4, 6.2));
        if let Some(&first) = f.lower_teeth.first() {
            self.lower_teeth.push(first);
        }
    }
}
