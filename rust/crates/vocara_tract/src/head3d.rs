//! The 3D head: the midsagittal anatomy grown into a head of lines a view can turn (port of the
//! geometry of CHIMERA's `src/ui/render/anatomy3d.ts`; its rendering, camera, glow, particles,
//! afterglow, gaze and expressions are left out).
//!
//! The anatomy is a midsagittal section, so width comes from three places, as in CHIMERA:
//!
//! - the area function: each line of the airway's measuring grid has a gap and an area, which
//!   make an elliptical cross-section, so the 3D airway is exactly the tube the acoustics sees;
//! - the articulators' midline outlines, swept sideways with anatomical widths: the tongue and
//!   the soft palate as ribbed surfaces, the epiglottis as a curled leaf, the lips as a patch
//!   whose opening is the model's aperture and spreading;
//! - fixed proportions for everything else (the head's shell, ears, nose, brain, vertebrae,
//!   nasal cavity, teeth on their arches, palate vault, mandible, larynx, trachea, hyoid,
//!   vocal folds).
//!
//! The jaw turns the mandible, the lower teeth and the lower face's skin (each skin point by how
//! much of the jaw it follows) about the condyles; the larynx carries its cartilages and the
//! hyoid. The vocal folds part as [`FoldPose`] has them.
//!
//! Head centimetres at the body's size: x forward, y up, z out of the right side of the face,
//! the glottis at the origin ([`crate::lines`]). Lines with [`Line::section`] set are the 2D
//! cut-away's at z = 0. Build them with [`Head3d::build`], or [`TractModel::head3d`].
//!
//! [`Line::section`]: crate::lines::Line::section

use crate::anatomy::{HumanArtic, HumanBody, HumanFrame, HumanVocalTract, NEUTRAL};
use crate::components::VocalFolds;
use crate::forward::TractModel;
use crate::geom::{catmull_rom, chaikin_closed, rotate_about, v, Vec2};
use crate::lines::{Emphasis, Lines3d, Point3, Structure};
use crate::math::{asin, clamp, cos, exp, hypot, pow, sin, smoothstep, sqrt, PI};
use crate::profile::{body_point, jaw_angle, wall_x, Profile};

type Table = &'static [(f64, f64)];
type V3 = [f64; 3];

fn interp(tab: Table, x: f64) -> f64 {
    if x <= tab[0].0 {
        return tab[0].1;
    }
    for i in 1..tab.len() {
        if x <= tab[i].0 {
            let ((x0, y0), (x1, y1)) = (tab[i - 1], tab[i]);
            return y0 + ((y1 - y0) * (x - x0)) / (x1 - x0);
        }
    }
    tab[tab.len() - 1].1
}

fn smooth(a: f64, b: f64, x: f64) -> f64 {
    smoothstep(a, b, x)
}

// ------------------------------------------------------------------ proportions
// template cm (the adult male, before the body's scale), by template height unless noted

/// half-width of the head and neck
const WIDTH: Table = &[
    (-4.6, 5.3),
    (0.0, 5.4),
    (2.5, 5.5),
    (4.5, 5.25),
    (6.5, 5.5),
    (8.5, 6.1),
    (10.5, 6.6),
    (12.5, 7.0),
    (15.0, 7.3),
    (17.5, 7.65),
    (19.5, 7.4),
    (21.0, 6.5),
    (22.2, 4.6),
    (23.0, 1.0),
];
/// the face's surface beside the midline features (chin, lips, nose), as body x
const PLANE: Table = &[
    (3.4, 5.9),
    (4.6, 6.2),
    (5.7, 6.15),
    (6.5, 6.2),
    (8.0, 6.35),
    (9.5, 6.35),
    (10.6, 6.55),
    (11.6, 6.75),
    (12.6, 6.9),
    (13.9, 7.15),
    (15.0, 7.45),
    (16.1, 7.95),
];
/// half-width of the midline feature: the chin, then the nose
const FEATURE: Table = &[
    (3.4, 2.0),
    (4.6, 2.2),
    (5.7, 2.4),
    (6.5, 2.6),
    (10.6, 1.7),
    (11.0, 1.75),
    (11.65, 1.2),
    (12.6, 0.95),
    (13.9, 0.75),
    (15.0, 0.9),
    (16.1, 1.6),
];
/// where the head is widest, as a fraction of the way from the back to the face
const WIDEST: Table = &[
    (-4.6, 0.5),
    (3.0, 0.5),
    (6.0, 0.42),
    (12.0, 0.42),
    (16.0, 0.45),
    (23.0, 0.45),
];
/// the jaw's lower border in the side view (x → y): the skin below it is the neck's
const JAW_BORDER: Table = &[
    (-3.0, 8.5),
    (-1.6, 8.0),
    (-0.6, 6.4),
    (1.2, 4.6),
    (3.5, 3.6),
    (5.3, 3.0),
    (7.3, 3.3),
    (8.6, 4.4),
];
/// the back edge of the ramus (y → x): the skin behind it stays with the skull
const RAMUS_BACK: Table = &[
    (6.0, -0.6),
    (7.4, -0.8),
    (9.6, -1.9),
    (10.6, -2.5),
    (12.0, -2.6),
];
const BRAIN_WIDTH: Table = &[
    (12.3, 4.0),
    (13.0, 5.6),
    (14.5, 6.4),
    (16.5, 6.9),
    (18.5, 6.6),
    (20.0, 5.4),
    (21.0, 3.6),
    (21.45, 0.5),
];
/// the tongue's half-width and how far its sides fall below the midline, root (0) to tip (1)
const TONGUE_W: Table = &[
    (0.0, 1.5),
    (0.15, 1.9),
    (0.35, 2.35),
    (0.6, 2.45),
    (0.8, 2.2),
    (0.92, 1.5),
    (1.0, 0.55),
];
const TONGUE_H: Table = &[
    (0.0, 1.3),
    (0.35, 1.1),
    (0.7, 0.9),
    (0.9, 0.55),
    (1.0, 0.25),
];
/// teeth from the midline back, upper and lower: crown width along the arch, height, thickness
const UPPER_TEETH: [[f64; 3]; 7] = [
    [0.85, 1.05, 0.7],
    [0.68, 0.95, 0.65],
    [0.78, 1.1, 0.8],
    [0.7, 0.85, 0.9],
    [0.68, 0.8, 0.9],
    [1.0, 0.75, 1.1],
    [0.92, 0.7, 1.05],
];
const LOWER_TEETH: [[f64; 3]; 7] = [
    [0.54, 0.9, 0.6],
    [0.6, 0.92, 0.62],
    [0.7, 1.0, 0.75],
    [0.7, 0.85, 0.8],
    [0.72, 0.8, 0.85],
    [1.1, 0.72, 1.05],
    [1.05, 0.7, 1.0],
];
/// the lips' patch reaches this far to each side (template cm), past the widest mouth
const LIP_HALF: f64 = 3.0;
/// superellipse exponent of the back half of a cross-section
const PB: f64 = 2.2;

/// The articulation the head's rest shapes are built for: jaw and lips closed.
pub const REST_ARTIC: HumanArtic = HumanArtic {
    jaw: 0.0,
    lip_ap: 0.0,
    labiodental: 0.0,
    ..NEUTRAL
};

/// A head (or brain) cross-section at one height: front and back on the midline, the bare
/// front beside the midline feature, half-width, where it is widest, the feature's size and
/// width, and the superellipse exponent of the front half (the back half uses PB).
#[derive(Clone, Copy, Debug)]
struct Section {
    f: f64,
    bk: f64,
    fs: f64,
    w: f64,
    xw: f64,
    e: f64,
    wf: f64,
    pf: f64,
}

fn bump(u: f64) -> f64 {
    if u.abs() < 1.0 {
        let a = 1.0 - u * u;
        a * a
    } else {
        0.0
    }
}

/// x of the front surface at lateral z (without the midline feature when bare)
fn front_x(s: &Section, z: f64, bare: bool) -> f64 {
    let u = (z.abs() / s.w).min(1.0);
    let c = pow(1.0 - pow(u, s.pf), 1.0 / s.pf);
    s.xw + (s.fs - s.xw) * c + if bare { 0.0 } else { s.e * bump(z / s.wf) }
}

/// half-width of the surface at x (for things on the side of the head)
fn side_z(s: &Section, x: f64) -> f64 {
    let front = x > s.xw;
    let c = if front {
        (x - s.xw) / (s.fs - s.xw).max(1e-6)
    } else {
        (s.xw - x) / (s.xw - s.bk).max(1e-6)
    };
    let p = if front { s.pf } else { PB };
    s.w * pow((1.0 - pow(c.min(1.0), p)).max(0.0), 1.0 / p)
}

/// one side of a cross-section, from the back (z = 0) round to the front (z = 0), as (x, z)
fn half_ring(s: &Section, zf: f64, out: &mut Vec<(f64, f64)>) {
    out.clear();
    const NB: usize = 12;
    const NF: usize = 10;
    const NP: usize = 16;
    for i in 0..NB {
        let th = PI - (i as f64 / NB as f64) * (PI / 2.0);
        out.push((
            s.xw - (s.xw - s.bk) * pow(cos(th).abs(), 2.0 / PB),
            s.w * pow(sin(th), 2.0 / PB),
        ));
    }
    let thf = asin(pow(zf / s.w, s.pf / 2.0).min(1.0));
    for i in 0..NF {
        let th = PI / 2.0 - (i as f64 / NF as f64) * (PI / 2.0 - thf);
        let z = s.w * pow(sin(th), 2.0 / s.pf);
        out.push((
            s.xw + (s.fs - s.xw) * pow(cos(th), 2.0 / s.pf) + s.e * bump(z / s.wf),
            z,
        ));
    }
    for i in 0..=NP {
        let z = zf * (1.0 - i as f64 / NP as f64);
        out.push((front_x(s, z, false), z));
    }
}

/// A closed outline's cross-sections by height, tabulated.
#[derive(Clone, Debug)]
struct Shape {
    rows: Vec<Option<Section>>,
    y0: f64,
    y1: f64,
    dy: f64,
}

impl Shape {
    fn new(outline: &[Vec2], dy: f64, make: impl Fn(f64, f64, f64) -> Section) -> Shape {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in outline {
            lo = lo.min(p.y);
            hi = hi.max(p.y);
        }
        let mut rows = Vec::new();
        let mut y = lo;
        while y <= hi {
            let (mut f, mut bk) = (f64::NEG_INFINITY, f64::INFINITY);
            for w in outline.windows(2) {
                let (a, b) = (w[0], w[1]);
                if (a.y > y) == (b.y > y) {
                    continue;
                }
                let x = a.x + ((b.x - a.x) * (y - a.y)) / (b.y - a.y);
                f = f.max(x);
                bk = bk.min(x);
            }
            rows.push((f > bk).then(|| make(y, f, bk)));
            y += dy;
        }
        Shape {
            rows,
            y0: lo,
            y1: hi,
            dy,
        }
    }

    fn at(&self, y: f64) -> Option<Section> {
        let u = (y - self.y0) / self.dy;
        let i = u.floor();
        if i < 0.0 || i as usize + 1 >= self.rows.len() {
            return None;
        }
        let i = i as usize;
        let (a, b) = (self.rows[i], self.rows[i + 1]);
        let (Some(a), Some(b)) = (a, b) else {
            return a.or(b);
        };
        let t = u - i as f64;
        let l = |p: f64, q: f64| p + (q - p) * t;
        Some(Section {
            f: l(a.f, b.f),
            bk: l(a.bk, b.bk),
            fs: l(a.fs, b.fs),
            w: l(a.w, b.w),
            xw: l(a.xw, b.xw),
            e: l(a.e, b.e),
            wf: l(a.wf, b.wf),
            pf: l(a.pf, b.pf),
        })
    }
}

/// A rest-pose line: its points, and per point how much of the jaw's turn it follows.
#[derive(Clone, Debug)]
struct RestLine {
    structure: Structure,
    emphasis: Emphasis,
    closed: bool,
    points: Vec<V3>,
    /// empty: fixed (0) or fully carried (1) by its group
    jaw: Vec<f64>,
}

/// Collects rest lines (CHIMERA's `Lines`: a line of fewer than two points is dropped).
#[derive(Default)]
struct Collect {
    lines: Vec<RestLine>,
    cur: Vec<V3>,
}

impl Collect {
    fn p(&mut self, p: V3) -> &mut Self {
        self.cur.push(p);
        self
    }
    fn end(&mut self, s: Structure, e: Emphasis, closed: bool) {
        let pts = std::mem::take(&mut self.cur);
        if pts.len() >= 2 {
            self.lines.push(RestLine {
                structure: s,
                emphasis: e,
                closed,
                points: pts,
                jaw: Vec::new(),
            });
        }
    }
    /// an ellipse c + u·cos + v·sin from angle a0 to a1, n + 1 points
    #[allow(clippy::too_many_arguments)]
    fn ellipse(
        &mut self,
        c: V3,
        u: V3,
        w: V3,
        n: usize,
        a0: f64,
        a1: f64,
        s: Structure,
        e: Emphasis,
    ) {
        for i in 0..=n {
            let an = a0 + ((a1 - a0) * i as f64) / n as f64;
            let (cs, sn) = (cos(an), sin(an));
            self.p([
                c[0] + u[0] * cs + w[0] * sn,
                c[1] + u[1] * cs + w[1] * sn,
                c[2] + u[2] * cs + w[2] * sn,
            ]);
        }
        self.end(s, e, false);
    }
}

/// How much of the jaw's rotation a point of the face's skin follows (rest pose, body cm).
fn jaw_weight(k: f64, ph: f64, x: f64, y: f64) -> f64 {
    let xx = x / k;
    let yb = y / k;
    let yy = if yb >= 7.0 { yb } else { 7.0 - (7.0 - yb) / ph };
    let wv = 1.0 - smooth(7.6, 9.1, yy); // the cheek above the mouth's corners stays with the skull
    let border = interp(JAW_BORDER, xx);
    let wl = smooth(border - 1.5, border + 0.15, yy); // the neck below the jaw stays
    let xr = interp(RAMUS_BACK, yy);
    let wb = smooth(xr - 0.7, xr + 0.9, xx); // and behind the ramus, the ear and the back of the neck
    wv * wl * wb
}

/// The runs of a polyline whose points are kept.
fn kept_runs(pts: &[V3], keep: &[bool], closed: bool) -> Vec<Vec<V3>> {
    let n = keep.len();
    if keep.iter().all(|&k| k) {
        let mut all = pts.to_vec();
        if closed && !pts.is_empty() {
            all.push(pts[0]);
        }
        return vec![all];
    }
    let mut out = Vec::new();
    // a closed loop starts just after a dropped point, so no run wraps round the seam
    let start = if closed {
        keep.iter().position(|&k| !k).map_or(0, |i| i + 1)
    } else {
        0
    };
    let mut run: Vec<V3> = Vec::new();
    for j in 0..n {
        let i = (start + j) % n;
        if keep[i] {
            run.push(pts[i]);
        } else {
            if run.len() >= 2 {
                out.push(std::mem::take(&mut run));
            }
            run.clear();
        }
    }
    if run.len() >= 2 {
        out.push(run);
    }
    out
}

fn cumulative(p: &[Vec2]) -> Vec<f64> {
    let mut c = vec![0.0];
    for i in 1..p.len() {
        c.push(c[i - 1] + hypot(p[i].x - p[i - 1].x, p[i].y - p[i - 1].y));
    }
    c
}

fn cumulative_into(p: &[Vec2], c: &mut Vec<f64>) {
    c.clear();
    c.push(0.0);
    for i in 1..p.len() {
        c.push(c[i - 1] + hypot(p[i].x - p[i - 1].x, p[i].y - p[i - 1].y));
    }
}

/// The point at fraction t of a polyline's length.
fn along(p: &[Vec2], cum: &[f64], t: f64) -> Vec2 {
    let s = t * cum[cum.len() - 1];
    let mut j = 0;
    while j + 2 < p.len() && cum[j + 1] < s {
        j += 1;
    }
    let u = (s - cum[j]) / (cum[j + 1] - cum[j]).max(1e-9);
    v(
        p[j].x + (p[j + 1].x - p[j].x) * u,
        p[j].y + (p[j + 1].y - p[j].y) * u,
    )
}

/// A polyline resampled to n points evenly along its length, into `out`.
fn resample(p: &[Vec2], n: usize, cum: &mut Vec<f64>, out: &mut Vec<Vec2>) {
    cumulative_into(p, cum);
    out.clear();
    let l = cum[cum.len() - 1];
    let mut j = 0;
    for i in 0..n {
        let s = (l * i as f64) / (n as f64 - 1.0);
        while j + 2 < p.len() && cum[j + 1] < s {
            j += 1;
        }
        let t = (s - cum[j]) / (cum[j + 1] - cum[j]).max(1e-9);
        out.push(v(
            p[j].x + (p[j + 1].x - p[j].x) * t,
            p[j].y + (p[j + 1].y - p[j].y) * t,
        ));
    }
}

fn rot_xy(x: f64, y: f64, c: Vec2, ang: f64) -> (f64, f64) {
    let (dx, dy) = (x - c.x, y - c.y);
    let (cs, sn) = (cos(ang), sin(ang));
    (c.x + dx * cs - dy * sn, c.y + dx * sn + dy * cs)
}

/// How the vocal folds are drawn: how far apart their edges are at the back (cm, each side's
/// edge from the midline beyond the 0.6 mm they never close past).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoldPose {
    pub gap_cm: f64,
}

impl FoldPose {
    /// The folds for `folds` on a body of scale `k`: apart (breathing) when unvoiced; voiced,
    /// together but for a chink that grows with breathiness (to 2.5 mm at the body's size). A
    /// view that animates their vibration can open and close them about this at F0.
    pub fn of(folds: &VocalFolds, k: f64) -> FoldPose {
        let gap = if folds.voiced() {
            let b = if folds.breathiness.is_finite() {
                clamp(folds.breathiness, 0.0, 1.0)
            } else {
                0.0
            };
            0.25 * k * b
        } else {
            0.45 * k
        };
        FoldPose { gap_cm: gap }
    }
}

/// Everything about a body's head that only depends on the body, in the rest pose (jaw closed).
#[derive(Clone, Debug)]
struct Rest {
    head: Shape,
    condyle: Vec2,
    lip_half: f64,
    /// fixed lines, and the shell's (each point with its jaw weight)
    fixed: Vec<RestLine>,
    /// carried by the jaw
    jaw: Vec<RestLine>,
    /// the chin's midline outline (rest pose): part of the section
    chin: Vec<V3>,
    eyes: [V3; 2],
    eye_r: f64,
    center: V3,
}

/// A body's 3D head (see the module docs): its rest shapes, built once, and the buffers each
/// frame is built in.
#[derive(Clone, Debug)]
pub struct Head3d {
    k: f64,
    pharynx: f64,
    lip_width: f64,
    rest: Rest,
    profile: Profile,
    // per-frame scratch
    smooth: Vec<Vec2>,
    scratch: Vec<Vec2>,
    up: Vec<Vec2>,
    lo: Vec<Vec2>,
    cu: Vec<f64>,
    cl: Vec<f64>,
    station: Vec<Vec2>,
    cum: Vec<f64>,
    rings: Vec<[f64; 6]>,
}

impl Head3d {
    /// The head of the body `t` is the tract of (CHIMERA's `buildRest`).
    pub fn new(t: &HumanVocalTract, body: &HumanBody) -> Head3d {
        let mut tract = t.clone();
        let mut frame = HumanFrame::with_capacity();
        tract.compute(&REST_ARTIC, &mut frame);
        let mut profile = Profile::default();
        profile.build(t, body, &frame, &REST_ARTIC);
        let rest = build_rest(t, body, &frame, &profile);
        Head3d {
            k: t.scale(),
            pharynx: body.pharynx,
            lip_width: body.lip_width,
            rest,
            profile,
            smooth: Vec::new(),
            scratch: Vec::new(),
            up: Vec::new(),
            lo: Vec::new(),
            cu: Vec::new(),
            cl: Vec::new(),
            station: Vec::new(),
            cum: Vec::new(),
            rings: Vec::new(),
        }
    }

    /// The eyes' centres (they are spheres: a view draws each as the circle facing it).
    pub fn eyes(&self) -> [Point3; 2] {
        self.rest.eyes.map(p3)
    }

    /// The eyes' radius (cm).
    pub fn eye_radius(&self) -> f32 {
        self.rest.eye_r as f32
    }

    /// The head's middle: where a view that turns it looks.
    pub fn center(&self) -> Point3 {
        p3(self.rest.center)
    }

    /// The point the head nods about (the top of the neck), as CHIMERA's view has it.
    pub fn pivot(&self) -> Point3 {
        let k = self.k;
        p3([
            -2.4 * k,
            self.rest.head.y0 + 0.42 * (self.rest.condyle.y - self.rest.head.y0),
            0.0,
        ])
    }

    /// The head's lines for frame `f` of articulation `a` (its tract's), the folds as `folds`
    /// has them, into `out` (its buffers are reused).
    pub fn build(
        &mut self,
        t: &HumanVocalTract,
        body: &HumanBody,
        f: &HumanFrame,
        a: &HumanArtic,
        folds: FoldPose,
        out: &mut Lines3d,
    ) {
        use Emphasis::{Detail, Outline};
        use Structure::*;
        out.clear();
        let k = self.k;
        let phi = jaw_angle(t, a.jaw);
        let c = self.rest.condyle;
        // the fixed structures and the shell (each shell point turned by its share of the jaw)
        for l in &self.rest.fixed {
            let pts = out.begin(l.structure, l.emphasis, l.closed, false);
            if l.jaw.is_empty() {
                pts.extend(l.points.iter().map(|&p| p3(p)));
            } else {
                for (p, &w) in l.points.iter().zip(&l.jaw) {
                    let (x, y) = if w != 0.0 {
                        rot_xy(p[0], p[1], c, phi * w)
                    } else {
                        (p[0], p[1])
                    };
                    pts.push([x as f32, y as f32, p[2] as f32]);
                }
            }
        }
        // carried by the jaw
        for l in &self.rest.jaw {
            let pts = out.begin(l.structure, l.emphasis, l.closed, false);
            for p in &l.points {
                let (x, y) = rot_xy(p[0], p[1], c, phi);
                pts.push([x as f32, y as f32, p[2] as f32]);
            }
        }

        // the midsagittal section, as the 2D view draws it
        self.profile.build(t, body, f, a);
        let g = &self.profile;
        let section = |out: &mut Lines3d, s, e, closed, pts: &[Vec2]| {
            out.begin(s, e, closed, true)
                .extend(pts.iter().map(|p| [p.x as f32, p.y as f32, 0.0]));
            out.drop_short();
        };
        section(out, Skull, Detail, false, &g.inset);
        section(out, Skull, Outline, true, &g.skull);
        section(out, Brain, Outline, true, &g.brain);
        section(out, Brain, Detail, false, &g.sulcus);
        section(out, NasalCavity, Outline, true, &g.nasal);
        section(out, HardPalate, Outline, false, &g.pal_oral);
        section(out, HardPalate, Detail, false, &g.pal_nasal);
        section(out, Skin, Outline, false, &g.skin_lower);
        section(out, Skin, Outline, false, &g.skin_upper);
        section(out, Airway, Outline, false, &g.outer_wall);
        section(out, Airway, Outline, false, &f.inner);
        section(out, Airway, Detail, false, &g.glottis_wall);

        self.airway(f, k, out);
        self.velum(f, k, out);
        self.epiglottis(f, k, out);
        self.larynx(t, f, a, k, folds, out);

        // the tongue: its outline on the midline and its surface's ribs
        section(out, Tongue, Outline, false, &f.tongue);
        self.tongue(f, a, k, out);
        // the chin, turned by the jaw
        {
            let pts = out.begin(Jaw, Outline, false, true);
            for p in &self.rest.chin {
                let (x, y) = rot_xy(p[0], p[1], c, phi);
                pts.push([x as f32, y as f32, 0.0]);
            }
        }
        self.lips(f, a, phi, out);
        for (s, pts, rounds) in [
            (UpperLip, &f.upper_lip, 2),
            (LowerLip, &f.lower_lip, 2),
            (Velum, &f.velum, 3),
        ] {
            chaikin_closed(pts, rounds, &mut self.smooth, &mut self.scratch);
            let n = self.smooth.len().saturating_sub(1);
            section(out, s, Outline, true, &self.smooth[..n]);
        }
    }

    // ------------------------------------------------------------------ the airway
    fn airway(&mut self, f: &HumanFrame, k: f64, out: &mut Lines3d) {
        let g = &f.grid;
        let r = &mut self.rings;
        r.clear();
        if g.len() < 2 {
            return;
        }
        let (mut ux, mut uy) = (-1.0, 0.0);
        for s in g.iter().take(80) {
            let (dx, dy) = (s.outer.x - s.inner.x, s.outer.y - s.inner.y);
            let d = hypot(dx, dy);
            if d > 1e-4 {
                ux = dx / d;
                uy = dy / d;
            }
            // the acoustic gap hangs from the outer wall (contact models can shorten the
            // ray's gap)
            let half = s.d / 2.0;
            // an ellipse with this midsagittal gap and the section's area
            let b = if s.d > 0.01 {
                (3.5 * k).min((2.0 * s.area) / (PI * s.d))
            } else {
                0.0
            };
            r.push([
                s.outer.x - ux * half,
                s.outer.y - uy * half,
                ux,
                uy,
                half,
                b,
            ]);
        }
        for ring in r.iter().step_by(2) {
            let [cx, cy, ux, uy, a, b] = *ring;
            let pts = out.begin(Structure::Airway, Emphasis::Detail, false, false);
            for i in 0..=24 {
                let an = (i as f64 / 24.0) * PI * 2.0;
                let (c, sn) = (cos(an) * a, sin(an) * b);
                pts.push(p3([cx + ux * c, cy + uy * c, sn]));
            }
        }
        // the tube's side walls
        for side in [1.0, -1.0] {
            let pts = out.begin(Structure::Airway, Emphasis::Outline, false, false);
            pts.extend(r.iter().map(|q| p3([q[0], q[1], side * q[5]])));
        }
        // the epilarynx: a short round tube up from the folds
        let gl = f.glottis;
        for u in [0.3, 0.65] {
            let cx = gl.x + (r[0][0] - gl.x) * u;
            let cy = gl.y + (r[0][1] - gl.y) * u;
            let rad = (0.55 + 0.25 * u) * k;
            let pts = out.begin(Structure::Airway, Emphasis::Detail, false, false);
            for i in 0..=20 {
                let an = (i as f64 / 20.0) * PI * 2.0;
                pts.push(p3([cx + rad * cos(an), cy, rad * sin(an)]));
            }
        }
    }

    // ------------------------------------------------------------------ soft palate, epiglottis
    fn velum(&mut self, f: &HumanFrame, k: f64, out: &mut Lines3d) {
        let vel = &f.velum;
        if vel.len() < 4 {
            return;
        }
        let n = vel.len() / 2 - 1; // 0..n along the nasal side
                                   // (x, y, nx, ny, half-width, droop) at each station
        let mut fr = [[0.0; 6]; 16];
        let n = n.min(15);
        for (i, q) in fr.iter_mut().enumerate().take(n + 1) {
            let (a, b) = (vel[i], vel[vel.len() - 1 - i]);
            let (mut nx, mut ny) = (a.x - b.x, a.y - b.y);
            let l = hypot(nx, ny);
            let l = if l == 0.0 { 1.0 } else { l };
            nx /= l;
            ny /= l;
            let u = i as f64 / n as f64;
            *q = [
                (a.x + b.x) / 2.0,
                (a.y + b.y) / 2.0,
                nx,
                ny,
                (1.95 - 1.5 * pow(u, 1.5)) * k,
                0.45 * k * u,
            ];
        }
        let at = |q: &[f64; 6], u: f64| -> Point3 {
            let off = 0.12 * k * (1.0 - u * u) - q[5] * u * u;
            p3([q[0] + q[2] * off, q[1] + q[3] * off, q[4] * u])
        };
        for i in [1, 3, 5, 7, 9].into_iter().filter(|&i| i <= n) {
            let pts = out.begin(Structure::Velum, Emphasis::Detail, false, false);
            for j in 0..=12 {
                pts.push(at(&fr[i], -1.0 + j as f64 / 6.0));
            }
        }
        for u in [-1.0, 1.0] {
            let pts = out.begin(Structure::Velum, Emphasis::Outline, false, false);
            pts.extend(fr[..=n].iter().map(|q| at(q, u)));
        }
    }

    fn epiglottis(&mut self, f: &HumanFrame, k: f64, out: &mut Lines3d) {
        if f.epiglottis.len() < 2 {
            return;
        }
        let (base, tip) = (f.epiglottis[0], f.epiglottis[1]);
        let (mut dx, mut dy) = (tip.x - base.x, tip.y - base.y);
        let l = hypot(dx, dy);
        let l = if l == 0.0 { 1.0 } else { l };
        dx /= l;
        dy /= l;
        // it is a leaf, curling back towards the larynx at its edges
        let back = v(-dy, dx); // the side away from the tongue
        let half_w = |u: f64| (0.22 + 0.9 * pow(sin((u * PI) / 2.0), 1.3)) * k;
        const N: usize = 10;
        let pts = out.begin(Structure::Epiglottis, Emphasis::Outline, false, false);
        for i in 0..=N {
            let u = i as f64 / N as f64;
            let w = half_w(u);
            let curl = 0.28 * k * (w / (1.12 * k));
            pts.push(p3([
                base.x + dx * l * u + back.x * curl,
                base.y + dy * l * u + back.y * curl,
                w,
            ]));
        }
        for i in 1..12 {
            let z = half_w(1.0) * (1.0 - (2.0 * i as f64) / 12.0);
            let curl = 0.28 * k * (z.abs() / (1.12 * k)).powi(2);
            let edge = 0.08 * k * (1.0 - (z / half_w(1.0)).powi(2));
            pts.push(p3([
                tip.x + back.x * curl + dx * edge,
                tip.y + back.y * curl + dy * edge,
                z,
            ]));
        }
        for i in (0..=N).rev() {
            let u = i as f64 / N as f64;
            let w = half_w(u);
            let curl = 0.28 * k * (w / (1.12 * k));
            pts.push(p3([
                base.x + dx * l * u + back.x * curl,
                base.y + dy * l * u + back.y * curl,
                -w,
            ]));
        }
        out.begin(Structure::Epiglottis, Emphasis::Outline, false, true)
            .extend([p3([base.x, base.y, 0.0]), p3([tip.x, tip.y, 0.0])]);
    }

    /// The larynx's cartilages and folds round the glottis, the trachea down to the neck's end.
    fn larynx(
        &mut self,
        t: &HumanVocalTract,
        f: &HumanFrame,
        a: &HumanArtic,
        k: f64,
        folds: FoldPose,
        out: &mut Lines3d,
    ) {
        use Emphasis::{Detail, Outline};
        use Structure::*;
        let g = f.glottis;
        let at = |x: f64, y: f64, z: f64| -> Point3 { p3([g.x + x * k, g.y + y * k, z * k]) };
        let pl = |out: &mut Lines3d, s, e, pts: &[[f64; 3]]| {
            out.begin(s, e, false, false)
                .extend(pts.iter().map(|q| at(q[0], q[1], q[2])));
        };
        // thyroid cartilage: two plates meeting at the front (the Adam's apple)
        for s in [1.0, -1.0] {
            pl(
                out,
                Larynx,
                Outline,
                &[
                    [1.45, 1.75, 0.0],
                    [0.75, 2.0, s * 1.2],
                    [-0.5, 1.95, s * 1.9],
                    [-0.6, -0.6, s * 1.85],
                    [0.25, -0.85, s * 1.5],
                    [1.05, -0.8, s * 0.55],
                    [1.2, -0.75, 0.0],
                    [1.6, 0.05, 0.0],
                    [1.9, 1.15, 0.0],
                    [1.45, 1.75, 0.0],
                ],
            );
            pl(
                out,
                Larynx,
                Outline,
                &[[-0.5, 1.95, s * 1.9], [-0.7, 2.85, s * 1.85]],
            );
            pl(
                out,
                Larynx,
                Outline,
                &[[-0.6, -0.6, s * 1.85], [-0.45, -1.25, s * 1.7]],
            );
        }
        // cricoid: a signet ring, low in front and tall behind
        for (y0, rise) in [(-1.75, 0.15), (-1.4, 1.1)] {
            let pts = out.begin(Larynx, Outline, false, false);
            for i in 0..=28 {
                let an = (i as f64 / 28.0) * PI * 2.0;
                pts.push(at(
                    0.1 + 1.0 * cos(an),
                    y0 + (rise * (1.0 - cos(an))) / 2.0,
                    1.1 * sin(an),
                ));
            }
        }
        // trachea: C-shaped rings, open at the back, down to the head's lowest point
        let bottom = self.rest.head.y0;
        let top = g.y - 2.1 * k;
        let cx = g.x + 0.08 * k;
        let mut y = top;
        while y > bottom {
            let pts = out.begin(Trachea, Detail, false, false);
            for i in 0..=20 {
                let an = -0.8 * PI + (1.6 * PI * i as f64) / 20.0;
                pts.push(p3([cx + 0.82 * k * cos(an), y, 0.95 * k * sin(an)]));
            }
            y -= 0.5 * k;
        }
        for s in [1.0, -1.0] {
            out.begin(Trachea, Detail, false, false)
                .extend([p3([cx, top, s * 0.95 * k]), p3([cx, bottom, s * 0.95 * k])]);
        }
        // its front and back walls on the midline, as the 2D view draws them
        for s in [1.0, -1.0] {
            out.begin(Trachea, Outline, false, true).extend([
                p3([cx + s * 0.82 * k, g.y, 0.0]),
                p3([cx + s * 0.82 * k, bottom, 0.0]),
            ]);
        }
        // hyoid: a U open at the back, riding above the larynx
        let hy = v(t.p(1.6, 0.0).x, t.p(0.0, 2.9).y + 0.6 * a.larynx * k);
        for dy in [-0.16, 0.16] {
            let pts = out.begin(Hyoid, Outline, false, false);
            for i in 0..=20 {
                let u = -1.0 + i as f64 / 10.0;
                pts.push(p3([
                    hy.x + (0.35 - 0.65 * u * u) * k,
                    hy.y + dy * k,
                    1.15 * u * k,
                ]));
            }
            for s in [1.0, -1.0] {
                out.begin(Hyoid, Outline, false, false).extend([
                    p3([hy.x - 0.3 * k, hy.y + dy * k, s * 1.15 * k]),
                    p3([hy.x - 2.8 * k, hy.y + (0.7 + dy) * k, s * 2.05 * k]),
                ]);
            }
        }
        // the vocal folds and what is round them: the folds' edges (apart by the pose's gap at
        // the back), the conus below, the ventricular folds above, the arytenoids
        let open = folds.gap_cm.max(0.0);
        for s in [1.0, -1.0] {
            out.begin(VocalFolds, Outline, false, false)
                .extend([at(0.72, 0.1, 0.0), at(-0.55, 0.1, s * (0.06 + open / k))]);
            out.begin(VocalFolds, Detail, false, false)
                .extend([at(0.72, 0.0, 0.0), at(-0.6, 0.0, s * 0.62)]);
            out.begin(VocalFolds, Detail, false, false)
                .extend([at(0.65, 0.45, 0.0), at(-0.5, 0.45, s * 0.45)]);
            let centre = at(-0.72, 0.15, s * (0.3 + open / k));
            let pts = out.begin(VocalFolds, Detail, false, false);
            for i in 0..=10 {
                let an = (i as f64 / 10.0) * PI * 2.0;
                let (cs, sn) = (cos(an), sin(an));
                pts.push([
                    centre[0] + (0.18 * k * cs) as f32,
                    centre[1] + (0.12 * k * sn) as f32,
                    centre[2] + (0.12 * k * sn) as f32,
                ]);
            }
        }
    }

    // ------------------------------------------------------------------ the tongue
    fn tongue(&mut self, f: &HumanFrame, a: &HumanArtic, k: f64, out: &mut Lines3d) {
        let tongue = &f.tongue;
        if tongue.len() < 3 {
            return;
        }
        let mut i_tip = 0;
        let mut bd = f64::INFINITY;
        for (i, p) in tongue.iter().enumerate() {
            let d = (p.x - f.tip.x).powi(2) + (p.y - f.tip.y).powi(2);
            if d < bd {
                bd = d;
                i_tip = i;
            }
        }
        // the upper surface from the root over the dorsum to the tip, in even stations
        const N: usize = 22;
        resample(
            &tongue[..=i_tip.max(1)],
            N,
            &mut self.cum,
            &mut self.station,
        );
        let st = &self.station;
        let lateral = clamp(a.lateral, 0.0, 1.0);
        // (x, y, nx, ny, half-width, side drop) per station
        let mut frames = [[0.0; 6]; N];
        for (i, q) in frames.iter_mut().enumerate() {
            let (q0, q1) = (st[i.saturating_sub(1)], st[(i + 1).min(N - 1)]);
            let (mut tx, mut ty) = (q1.x - q0.x, q1.y - q0.y);
            let tl = hypot(tx, ty);
            let tl = if tl == 0.0 { 1.0 } else { tl };
            tx /= tl;
            ty /= tl;
            let sg = i as f64 / (N - 1) as f64;
            // outward: the surface runs root → tip with the tongue below it, so its left normal
            *q = [
                st[i].x,
                st[i].y,
                -ty,
                tx,
                interp(TONGUE_W, sg) * k,
                (interp(TONGUE_H, sg) + 0.8 * lateral * smooth(0.45, 0.8, sg)) * k,
            ];
        }
        let at = |q: &[f64; 6], ph: f64| -> Point3 {
            let drop = q[5] * (1.0 - cos(ph));
            p3([q[0] - q[2] * drop, q[1] - q[3] * drop, q[4] * sin(ph)])
        };
        for q in frames.iter().take(N - 1).skip(1).step_by(2) {
            let pts = out.begin(Structure::Tongue, Emphasis::Detail, false, false);
            for j in 0..=12 {
                pts.push(at(q, -PI / 2.0 + (PI * j as f64) / 12.0));
            }
        }
        for ph in [-PI / 2.0, -PI / 4.0, PI / 4.0, PI / 2.0] {
            let e = if ph.abs() > 1.0 {
                Emphasis::Outline
            } else {
                Emphasis::Detail
            };
            out.begin(Structure::Tongue, e, false, false)
                .extend(frames.iter().map(|q| at(q, ph)));
        }
    }

    // ------------------------------------------------------------------ the lips
    /// The lips as a patch over the mouth: the model's midline outlines swept sideways. The
    /// opening's width is the model's lip width (rounded to spread) and its height the midline
    /// gap, closing to the corners. The upper lip stays with the skull and the lower with the
    /// jaw; the corners go half way, and towards the patch's edges each point follows the face's
    /// skin.
    fn lips(&mut self, f: &HumanFrame, a: &HumanArtic, phi: f64, out: &mut Lines3d) {
        use Emphasis::{Detail, Outline};
        use Structure::*;
        if f.upper_lip.len() < 8 || f.lower_lip.len() < 8 {
            return;
        }
        let k = self.k;
        let c = self.rest.condyle;
        let ph = self.pharynx;
        let wl = self.rest.lip_half;
        let wm = (0.3 * k)
            .max(((1.1 + (self.lip_width - 1.1) * clamp(a.lip_width, 0.0, 1.0)) * k) / 2.0);
        // edge → nose; edge → chin as if the jaw were closed
        self.up.clear();
        self.up.extend_from_slice(&f.upper_lip[6..]);
        self.lo.clear();
        self.lo
            .extend(f.lower_lip[6..].iter().map(|p| rotate_about(*p, c, -phi)));
        cumulative_into(&self.up, &mut self.cu);
        cumulative_into(&self.lo, &mut self.cl);
        let head = &self.rest.head;
        let (up, lo, cu, cl) = (&self.up, &self.lo, &self.cu, &self.cl);
        let (ul, ll) = (up[0], lo[0]);
        let yc = (ul.y + ll.y) / 2.0;
        let base = |y: f64, z: f64| head.at(y).map_or(0.0, |s| front_x(&s, z, true));
        let bulge0u = ul.x - base(ul.y, 0.0);
        let bulge0l = ll.x - base(ll.y, 0.0);
        let avg0 = (bulge0u + bulge0l) / 2.0;
        let (y_top, y_bot) = (up[up.len() - 1].y, lo[lo.len() - 1].y);
        // a point of the patch: upper or lower lip, t from the edge (0) to the patch's far edge
        // (1), at z
        let pt = |upper: bool, t: f64, z: f64| -> Point3 {
            let sz = sqrt((1.0 - (z / wm).powi(2)).max(0.0));
            let m = if upper {
                along(up, cu, t)
            } else {
                along(lo, cl, t)
            };
            let (edge, far) = if upper { (ul, y_top) } else { (ll, y_bot) };
            let ye = yc + (edge.y - yc) * sz;
            let span = far - edge.y;
            let y =
                ye + ((m.y - edge.y) * (far - ye)) / if span.abs() < 1e-4 { 1e-4 } else { span };
            let g = pow((1.0 - (z / wl).powi(2)).max(0.0), 1.5);
            let b0 = if upper { bulge0u } else { bulge0l };
            let bulge = m.x - base(m.y, 0.0) + (avg0 - b0) * (1.0 - sz) * (1.0 - t);
            let x = base(y, z) + bulge * g;
            let natural = if upper {
                0.5 * (1.0 - sz) * (1.0 - t)
            } else {
                0.5 * (1.0 + sz) + 0.5 * (1.0 - sz) * t
            };
            let w = natural + (jaw_weight(k, ph, x, y) - natural) * smooth(wm, wl, z.abs());
            let (rx, ry) = if w != 0.0 {
                rot_xy(x, y, c, phi * w)
            } else {
                (x, y)
            };
            p3([rx, ry, z])
        };
        let row = |out: &mut Lines3d,
                   s,
                   e,
                   upper: bool,
                   t: &dyn Fn(f64) -> f64,
                   z0: f64,
                   z1: f64,
                   n: usize| {
            let pts = out.begin(s, e, false, false);
            for i in 0..=n {
                let z = z0 + ((z1 - z0) * i as f64) / n as f64;
                pts.push(pt(upper, t(z), z));
            }
        };
        for upper in [true, false] {
            // the patch's own lines of skin
            for tt in [0.66, 0.88] {
                row(out, Skin, Detail, upper, &|_| tt, -wl, wl, 24);
            }
            for z in [-0.8, -0.5, 0.5, 0.8].map(|q| q * wl) {
                let pts = out.begin(Skin, Detail, false, false);
                for i in 0..=8 {
                    pts.push(pt(upper, 0.45 + (0.55 * i as f64) / 8.0, z));
                }
            }
            // the vermilion border, closing to the corners; the upper one with a Cupid's bow
            let wv = wl.min(wm + 0.3 * k);
            let border = move |z: f64| -> f64 {
                let s = sqrt((1.0 - (z / wv).powi(2)).max(0.0));
                let bow = if upper {
                    -0.07 * exp(-(z / (0.32 * k)).powi(2))
                        + 0.05 * exp(-((z.abs() - 0.55 * k) / (0.3 * k)).powi(2))
                } else {
                    0.0
                };
                ((if upper { 0.42 } else { 0.45 }) * s + bow * s).max(0.0)
            };
            let lip = if upper { UpperLip } else { LowerLip };
            row(out, lip, Outline, upper, &border, -wv, wv, 28);
            row(out, lip, Detail, upper, &|z| 0.45 * border(z), -wv, wv, 28);
        }
        // where the lips meet beyond the corners
        for s in [1.0, -1.0] {
            row(out, Mouth, Detail, true, &|_| 0.0, s * wm, s * wl, 6);
        }
        // the opening (the lips apart in the model's frame: lo is in the jaw's rest pose)
        let open = f.upper_lip[6].y - f.lower_lip[6].y > 0.02 * k;
        const N: usize = 24;
        let pts = out.begin(Mouth, Outline, open, false);
        for i in 0..=N {
            pts.push(pt(true, 0.0, -wm + (2.0 * wm * i as f64) / N as f64));
        }
        if open {
            for i in (1..N).rev() {
                pts.push(pt(false, 0.0, -wm + (2.0 * wm * i as f64) / N as f64));
            }
        }
    }
}

fn p3(p: V3) -> Point3 {
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

// ------------------------------------------------------------------ the rest pose

fn build_rest(t: &HumanVocalTract, body: &HumanBody, frame: &HumanFrame, prof: &Profile) -> Rest {
    use Emphasis::{Detail, Outline};
    use Structure::*;
    let k = t.scale();
    let ph = body.pharynx;
    let b = |x: f64, y: f64| body_point(body, x, y);
    let by = |yy: f64| b(0.0, yy).y;
    let un_by = |y: f64| {
        if y / k >= 7.0 {
            y / k
        } else {
            7.0 - (7.0 - y / k) / ph
        }
    };
    let p = |x: f64, y: f64| t.p(x, y);
    let mut skin = prof.skin_lower.clone();
    skin.extend_from_slice(&prof.skin_upper);
    let head = Shape::new(&skin, 0.08 * k, |y, f, bk| {
        let yy = un_by(y);
        let plane = if (3.4..=16.1).contains(&yy) {
            interp(PLANE, yy) * k
        } else {
            f64::INFINITY
        };
        let fs = f.min(plane);
        let pf = 2.2 + 0.4 * smooth(2.5, 3.5, yy) * (1.0 - smooth(17.0, 18.5, yy));
        Section {
            f,
            bk,
            fs,
            w: interp(WIDTH, yy) * k,
            xw: bk + (fs - bk) * interp(WIDEST, yy),
            e: f - fs,
            wf: interp(FEATURE, yy) * k,
            pf,
        }
    });
    let condyle = p(-2.5, 10.5);
    let lip_top = frame.upper_lip[frame.upper_lip.len() - 1].y;
    let lip_bottom = frame.lower_lip[frame.lower_lip.len() - 1].y;
    let lip_half = LIP_HALF * k;
    let in_lips = |x: f64, y: f64, z: f64, s: &Section| {
        y > lip_bottom && y < lip_top && z.abs() < lip_half && x > s.xw
    };

    // ---- the head's shell: rings by height, verticals over the face, meridians round the back
    let mut fixed: Vec<RestLine> = Vec::new();
    let add_shell = |fixed: &mut Vec<RestLine>, pts: &[V3], keep: &[bool], closed: bool| {
        for run in kept_runs(pts, keep, closed) {
            if run.len() < 2 {
                continue; // an empty stretch between two gaps
            }
            let jaw = run.iter().map(|q| jaw_weight(k, ph, q[0], q[1])).collect();
            fixed.push(RestLine {
                structure: Skin,
                emphasis: Detail,
                closed: false,
                points: run,
                jaw,
            });
        }
    };
    let mut half: Vec<(f64, f64)> = Vec::new();
    let ring_step = 1.4 * k;
    let mut y = head.y0 + 0.25 * k;
    while y < head.y1 - 0.2 * k {
        if let Some(s) = head.at(y).filter(|s| s.w >= 0.2 * k) {
            half_ring(&s, (0.6 * s.w).min(3.6 * k), &mut half);
            let n = half.len();
            // right side from the back to the front, then the left side back again (no doubled
            // ends)
            let mut pts: Vec<V3> = half.iter().map(|&(x, z)| [x, y, z]).collect();
            for i in (1..n - 1).rev() {
                pts.push([half[i].0, y, -half[i].1]);
            }
            let keep: Vec<bool> = pts.iter().map(|q| !in_lips(q[0], y, q[2], &s)).collect();
            add_shell(&mut fixed, &pts, &keep, true);
        }
        y += ring_step;
    }
    let vstep = 0.3 * k;
    for z in [-3.6, -2.4, -1.2, 1.2, 2.4, 3.6].map(|q| q * k) {
        let (mut pts, mut keep): (Vec<V3>, Vec<bool>) = (Vec::new(), Vec::new());
        let mut y = head.y0 + 0.1 * k;
        while y <= head.y1 {
            match head.at(y) {
                Some(s) if z.abs() <= 0.97 * s.w => {
                    let x = front_x(&s, z, false);
                    pts.push([x, y, z]);
                    keep.push(!in_lips(x, y, z, &s));
                }
                _ => {
                    add_shell(&mut fixed, &pts, &keep, false);
                    pts.clear();
                    keep.clear();
                }
            }
            y += vstep;
        }
        add_shell(&mut fixed, &pts, &keep, false);
    }
    for th in [0.5, 0.8].map(|q| q * PI) {
        for side in [1.0, -1.0] {
            let (mut pts, mut keep): (Vec<V3>, Vec<bool>) = (Vec::new(), Vec::new());
            let mut y = head.y0 + 0.1 * k;
            while y <= head.y1 {
                if let Some(s) = head.at(y).filter(|s| s.w >= 0.2 * k) {
                    pts.push([
                        s.xw - (s.xw - s.bk) * pow(cos(th).abs(), 2.0 / PB),
                        y,
                        side * s.w * pow(sin(th), 2.0 / PB),
                    ]);
                    keep.push(true);
                }
                y += vstep;
            }
            add_shell(&mut fixed, &pts, &keep, false);
        }
    }

    let mut lines = Collect::default();
    // a template outline on the side of the head, standing out from it by `off` (body cm)
    let on_side =
        |lines: &mut Collect, pts: &[(f64, f64)], off: &dyn Fn(usize, usize) -> f64, side: f64| {
            let ctrl: Vec<Vec2> = pts.iter().map(|&(x, y)| b(x, y)).collect();
            let curve = catmull_rom(&ctrl, 4);
            let n = curve.len();
            for (i, q) in curve.iter().enumerate() {
                let sz = head.at(q.y).map_or(0.0, |s| side_z(&s, q.x));
                lines.p([q.x, q.y, side * (sz + off(i, n))]);
            }
            lines.end(Ear, Outline, false);
        };
    // ---- ears
    for side in [1.0, -1.0] {
        on_side(
            &mut lines,
            &[
                (-2.55, 13.4),
                (-2.85, 14.75),
                (-3.65, 15.45),
                (-4.6, 15.25),
                (-5.2, 14.25),
                (-5.4, 12.8),
                (-5.05, 11.3),
                (-4.55, 10.2),
                (-3.95, 9.55),
                (-3.35, 9.65),
                (-3.0, 10.35),
            ],
            &|i, n| (0.25 + 1.2 * pow(sin((PI * i as f64) / (n as f64 - 1.0)), 1.5)) * k,
            side,
        );
        on_side(
            &mut lines,
            &[
                (-3.15, 14.3),
                (-3.95, 14.45),
                (-4.55, 13.5),
                (-4.6, 12.1),
                (-4.15, 11.05),
            ],
            &|_, _| 0.55 * k,
            side,
        );
        on_side(
            &mut lines,
            &[(-2.7, 11.2), (-2.55, 11.85), (-2.75, 12.35)],
            &|_, _| 0.35 * k,
            side,
        );
        let concha: Vec<(f64, f64)> = (0..=16)
            .map(|i| {
                let an = (i as f64 / 16.0) * PI * 2.0;
                (-3.5 + 0.6 * cos(an), 12.0 + 0.85 * sin(an))
            })
            .collect();
        on_side(&mut lines, &concha, &|_, _| 0.12 * k, side);
    }
    // ---- the nose's sides and wings, so it reads from the front
    for side in [1.0, -1.0] {
        let mut yy: f64 = 14.6;
        while yy >= 11.3 {
            let y = by(yy);
            if let Some(s) = head.at(y) {
                let z = side * (0.3 + (1.05 * (14.6 - yy)) / 3.3) * k;
                lines.p([front_x(&s, z, false), y, z]);
            }
            yy -= 0.15;
        }
        lines.end(Nose, Outline, false);
        for (x, yy, z) in [
            (7.95, 11.05, 0.95),
            (7.4, 11.15, 1.45),
            (7.25, 11.6, 1.55),
            (7.55, 12.1, 1.35),
            (8.2, 12.15, 1.0),
        ] {
            lines.p([x * k, by(yy), side * z * k]);
        }
        lines.end(Nose, Outline, false);
    }
    // ---- nostrils, under the tip of the nose
    for side in [1.0, -1.0] {
        lines.ellipse(
            [8.05 * k, by(10.98), side * 0.62 * k],
            [0.42 * k, 0.04 * k, 0.0],
            [0.0, 0.0, 0.26 * k],
            16,
            0.0,
            PI * 2.0,
            Nose,
            Outline,
        );
    }

    // ---- brain: rings and meridians over the 2D outline
    let brain = Shape::new(&prof.brain, 0.08 * k, |y, f, bk| Section {
        f,
        bk,
        fs: f,
        w: interp(BRAIN_WIDTH, un_by(y)) * k,
        xw: bk + (f - bk) * 0.48,
        e: 0.0,
        wf: 1.0,
        pf: 2.3,
    });
    let brain_point = |s: &Section, th: f64| -> (f64, f64) {
        let (c, sn) = (cos(th), sin(th));
        let p = if c >= 0.0 { s.pf } else { PB };
        let x = if c >= 0.0 {
            s.xw + (s.fs - s.xw) * pow(c, 2.0 / p)
        } else {
            s.xw - (s.xw - s.bk) * pow(-c, 2.0 / p)
        };
        (
            x,
            s.w * pow(sn.abs(), 2.0 / p) * sn.signum() * f64::from(sn != 0.0),
        )
    };
    let mut y = brain.y0 + 0.6 * k;
    while y < brain.y1 - 0.15 * k {
        if let Some(s) = brain.at(y) {
            for i in 0..=48 {
                let (x, z) = brain_point(&s, (i as f64 / 48.0) * PI * 2.0);
                lines.p([x, y, z]);
            }
            lines.end(Brain, Detail, false);
        }
        y += 1.5 * k;
    }
    for th in [0.3, 0.7].map(|q| q * PI) {
        for side in [1.0, -1.0] {
            let mut y = brain.y0;
            while y <= brain.y1 {
                if let Some(s) = brain.at(y) {
                    let (x, z) = brain_point(&s, th);
                    lines.p([x, y, side * z.abs()]);
                }
                y += 0.3 * k;
            }
            lines.end(Brain, Detail, false);
        }
    }

    // ---- cervical vertebrae: bodies, arches, processes; the spinal cord through them
    for i in 0..9 {
        let y0 = -4.4 + i as f64 * 1.75;
        if y0 > 10.5 {
            break;
        }
        let x0 = wall_x(y0) - 2.25;
        let (lo, hi) = (p(x0, y0 + 0.08), p(x0, y0 + 1.32));
        let cx = p(x0 + 0.8, 0.0).x;
        let (rx, rz) = (0.8 * k, 1.05 * k);
        for yy in [lo.y, hi.y] {
            lines.ellipse(
                [cx, yy, 0.0],
                [rx, 0.0, 0.0],
                [0.0, 0.0, rz],
                24,
                0.0,
                PI * 2.0,
                Spine,
                Detail,
            );
        }
        for an in [0.25, 0.75, 1.25, 1.75].map(|q| q * PI) {
            lines.p([cx + rx * cos(an), lo.y, rz * sin(an)]).p([
                cx + rx * cos(an),
                hi.y,
                rz * sin(an),
            ]);
            lines.end(Spine, Detail, false);
        }
        let ym = (lo.y + hi.y) / 2.0;
        let ax = cx - 2.15 * k;
        lines.ellipse(
            [ax, ym, 0.0],
            [0.72 * k, 0.0, 0.0],
            [0.0, 0.0, 0.88 * k],
            20,
            0.0,
            PI * 2.0,
            Spine,
            Detail,
        );
        for s in [1.0, -1.0] {
            lines
                .p([cx - 0.7 * k, ym, s * 0.55 * k])
                .p([ax + 0.45 * k, ym, s * 0.72 * k]);
            lines.end(Spine, Detail, false);
            lines
                .p([cx - 0.5 * k, ym, s * 0.95 * k])
                .p([cx - 0.9 * k, ym - 0.1 * k, s * 2.3 * k]);
            lines.end(Spine, Detail, false);
        }
        lines
            .p([ax - 0.72 * k, ym, 0.0])
            .p([ax - 1.8 * k, ym - 0.55 * k, 0.0]);
        lines.end(Spine, Detail, false);
    }
    {
        let cx = p(-4.9, 0.0).x;
        let mut yy = -4.4;
        while yy <= 11.0 {
            lines.ellipse(
                [cx, p(0.0, yy).y, 0.0],
                [0.42 * k, 0.0, 0.0],
                [0.0, 0.0, 0.6 * k],
                16,
                0.0,
                PI * 2.0,
                Spine,
                Detail,
            );
            yy += 1.75;
        }
        for s in [1.0, -1.0] {
            lines
                .p([cx, p(0.0, -4.6).y, s * 0.6 * k])
                .p([cx, p(0.0, 11.4).y, s * 0.6 * k]);
            lines.end(Spine, Detail, false);
        }
    }

    // ---- nasal cavity: the septum is the 2D outline; its side walls and the turbinates
    for s in [1.0, -1.0] {
        for q in &prof.nasal {
            lines.p([q.x, q.y, s * 1.2 * k]);
        }
        lines.end(NasalCavity, Detail, true);
        for tb in &prof.turbinates {
            for q in tb {
                lines.p([q.x, q.y, s * 0.95 * k]);
            }
            lines.end(NasalCavity, Detail, false);
        }
    }

    // ---- upper teeth on their arch, the palate's vault between them
    let up_front = p(7.3, 0.0).x;
    let (up_depth, up_half) = (4.2 * k, 2.65 * k);
    teeth(
        &mut lines,
        k,
        up_front,
        up_depth,
        up_half,
        &UPPER_TEETH,
        &|u| (7.95 + 0.3 * u * u) * k,
        1.0,
        UpperTeeth,
    );
    for i in [0, 2, 4, 5, 6, 7] {
        let Some(&m) = prof.pal_oral.get(i) else {
            continue;
        };
        // the arch's half-width at this depth, inside the teeth
        let u = ((up_front - m.x) / up_depth).max(0.0);
        let za = (if u >= 1.0 {
            up_half
        } else {
            up_half * pow(u, 1.0 / 1.85)
        }) - 0.45 * k;
        let ya = (8.95 + 0.3 * (za / up_half).min(1.0).powi(2)) * k;
        if za < 0.6 * k || m.y < ya + 0.15 * k {
            continue;
        }
        for j in 0..=20 {
            let z = za * (j as f64 / 10.0 - 1.0);
            lines.p([
                m.x,
                ya + (m.y - ya) * pow(1.0 - pow(z.abs() / za, 2.4), 0.7),
                z,
            ]);
        }
        lines.end(HardPalate, Detail, false);
    }
    for l in lines.lines.drain(..) {
        fixed.push(l);
    }

    // ---- the mandible (rest pose; the jaw turns it about the condyles)
    let m3 = |x: f64, y: f64, z: f64| -> V3 {
        let q = p(x, y);
        [q.x, q.y, z * k]
    };
    // a smooth curve through mandible landmarks (template x, y and z)
    let mline = |lines: &mut Collect, pts: &[[f64; 3]], side: f64, s: Structure, e: Emphasis| {
        let q: Vec<V3> = pts.iter().map(|w| m3(w[0], w[1], side * w[2])).collect();
        for i in 0..q.len() - 1 {
            let (p0, p1, p2, p3) = (
                q[i.saturating_sub(1)],
                q[i],
                q[i + 1],
                q[(i + 2).min(q.len() - 1)],
            );
            for jj in 0..4 {
                let t = jj as f64 / 4.0;
                let (t2, t3) = (t * t, t * t * t);
                let cr = |a: f64, b: f64, cc: f64, d: f64| {
                    0.5 * (2.0 * b
                        + (-a + cc) * t
                        + (2.0 * a - 5.0 * b + 4.0 * cc - d) * t2
                        + (-a + 3.0 * b - 3.0 * cc + d) * t3)
                };
                lines.p([
                    cr(p0[0], p1[0], p2[0], p3[0]),
                    cr(p0[1], p1[1], p2[1], p3[1]),
                    cr(p0[2], p1[2], p2[2], p3[2]),
                ]);
            }
        }
        lines.p(q[q.len() - 1]);
        lines.end(s, e, false);
    };
    for side in [1.0, -1.0] {
        // lower border, then the ramus up to the condyle
        mline(
            &mut lines,
            &[
                [6.4, 4.6, 0.0],
                [5.6, 4.45, 1.5],
                [4.5, 4.4, 2.6],
                [3.2, 4.5, 3.5],
                [1.8, 4.85, 4.15],
                [0.4, 5.6, 4.6],
                [-0.55, 6.6, 4.85],
                [-0.9, 7.6, 4.9],
                [-1.45, 8.9, 4.95],
                [-1.9, 9.7, 5.0],
                [-2.15, 10.2, 5.0],
            ],
            side,
            Jaw,
            Outline,
        );
        // condyle neck forward, the notch, the coronoid and the ramus' front edge down to the
        // teeth
        mline(
            &mut lines,
            &[
                [-1.8, 10.25, 4.95],
                [-0.9, 9.4, 4.85],
                [0.15, 10.1, 4.6],
                [0.6, 8.9, 4.35],
                [1.1, 7.7, 4.0],
                [1.9, 7.2, 3.55],
            ],
            side,
            Jaw,
            Outline,
        );
        lines.ellipse(
            m3(-2.5, 10.5, side * 4.95),
            [0.45 * k, 0.0, 0.0],
            [0.0, 0.12 * k, 0.85 * k],
            16,
            0.0,
            PI * 2.0,
            Jaw,
            Outline,
        );
    }
    for side in [1.0, -1.0] {
        // the alveolar border along the lower teeth, and the body's height at a few places
        mline(
            &mut lines,
            &[
                [1.9, 7.2, 3.55],
                [3.0, 7.15, 3.15],
                [4.3, 7.1, 2.6],
                [5.4, 7.05, 1.85],
                [6.2, 7.05, 1.05],
                [6.72, 7.15, 0.0],
            ],
            side,
            Jaw,
            Detail,
        );
        for [xa, za, xb, yb, zb] in [
            [3.0, 3.15, 3.2, 4.5, 3.5],
            [5.4, 1.85, 5.6, 4.45, 1.5],
            [1.9, 3.55, 1.8, 4.85, 4.15],
        ] {
            mline(
                &mut lines,
                &[
                    [xa, 7.15, za],
                    [(xa + xb) / 2.0, 5.9, (za + zb) / 2.0 + 0.15],
                    [xb, yb, zb],
                ],
                side,
                Jaw,
                Detail,
            );
        }
    }
    let lo_front = p(6.98, 0.0).x;
    teeth(
        &mut lines,
        k,
        lo_front,
        4.0 * k,
        2.5 * k,
        &LOWER_TEETH,
        &|u| (7.75 + 0.45 * pow(u, 1.5)) * k,
        -1.0,
        LowerTeeth,
    );
    let jaw = std::mem::take(&mut lines.lines);
    // the chin on the midline: the 2D view's jaw outline
    mline(
        &mut lines,
        &[
            [7.25, 6.75, 0.0],
            [7.4, 5.4, 0.0],
            [6.4, 4.6, 0.0],
            [5.6, 5.0, 0.0],
            [5.9, 6.2, 0.0],
            [6.45, 6.55, 0.0],
        ],
        1.0,
        Jaw,
        Outline,
    );
    let chin = lines.lines.pop().map(|l| l.points).unwrap_or_default();

    // eyes: the 2D view draws one on the midline; here they sit either side of the nose
    let eye_c = b(6.15, 15.35);
    Rest {
        condyle,
        lip_half,
        fixed,
        jaw,
        chin,
        eyes: [[eye_c.x, eye_c.y, 3.15 * k], [eye_c.x, eye_c.y, -3.15 * k]],
        eye_r: 1.05 * k,
        center: [0.3 * k, (head.y0 + head.y1) / 2.0, 0.0],
        head,
    }
}

/// Teeth on an arch x(z) = front − depth·(|z|/half)^1.85: each tooth's face towards the lips
/// and cheeks (a flat biting edge, narrowing to the gum) and its biting surface; `occ` gives
/// the biting plane's height along the arch, `dir` which way the crowns grow (+1 up).
#[allow(clippy::too_many_arguments)]
fn teeth(
    lines: &mut Collect,
    k: f64,
    front: f64,
    depth: f64,
    half: f64,
    spec: &[[f64; 3]],
    occ: &dyn Fn(f64) -> f64,
    dir: f64,
    s: Structure,
) {
    let pts: Vec<Vec2> = (0..=60)
        .map(|i| {
            let z = (half * i as f64) / 60.0;
            v(front - depth * pow(z / half, 1.85), z)
        })
        .collect();
    let cum = cumulative(&pts);
    let len = cum[cum.len() - 1];
    let total: f64 = spec.iter().map(|q| q[0]).sum();
    let scale = (0.97 * len) / (total * k);
    for side in [1.0, -1.0] {
        let mut s0 = 0.0;
        for &[w0, h0, th0] in spec {
            let (w, h, th) = (w0 * k * scale, h0 * k, th0 * k);
            let tc = (s0 + w / 2.0) / len;
            s0 += w;
            let c = along(&pts, &cum, tc);
            let c1 = along(&pts, &cum, (tc + 0.01).min(1.0));
            let c0 = along(&pts, &cum, (tc - 0.01).max(0.0));
            // tangent along the arch (x, z) and the outward normal
            let (mut tx, mut tz) = (c1.x - c0.x, c1.y - c0.y);
            let tl = hypot(tx, tz);
            let tl = if tl == 0.0 { 1.0 } else { tl };
            tx /= tl;
            tz /= tl;
            let (nx, nz) = (tz, -tx); // outward (front at the midline)
            let y0 = occ(c.y / half);
            let p3 = |u: f64, vv: f64, inward: f64| -> V3 {
                [
                    c.x + tx * u - nx * inward,
                    y0 + dir * vv,
                    side * (c.y + tz * u - nz * inward),
                ]
            };
            for (u, vv) in [
                (-0.5, 0.0),
                (-0.5, 0.55),
                (-0.38, 1.0),
                (0.0, 1.08),
                (0.38, 1.0),
                (0.5, 0.55),
                (0.5, 0.0),
            ] {
                lines.p(p3(u * w, vv * h, 0.0));
            }
            lines.end(s, Emphasis::Outline, true);
            for (u, vv) in [(-0.45, 0.1), (0.45, 0.1), (0.45, 0.9), (-0.45, 0.9)] {
                lines.p(p3(u * w, 0.0, vv * th));
            }
            lines.end(s, Emphasis::Detail, true);
        }
    }
}

impl TractModel {
    /// The 3D head of `a` (see the module docs), the folds as `folds` has them, into `out` (its
    /// buffers are reused). The head's rest shapes are built on the first call (a few
    /// milliseconds) and kept; [`TractModel::head`] gives them (the eyes, the centre).
    pub fn head3d(&mut self, a: &HumanArtic, folds: &VocalFolds, out: &mut Lines3d) {
        self.apply(a);
        let mut head = self
            .head
            .take()
            .unwrap_or_else(|| Box::new(Head3d::new(self.tract(), self.anatomy())));
        let pose = FoldPose::of(folds, self.tract().scale());
        head.build(self.tract(), self.anatomy(), self.frame(), a, pose, out);
        self.head = Some(head);
    }

    /// The body's 3D head (its eyes, centre and pivot), built if it was not yet.
    pub fn head(&mut self) -> &Head3d {
        if self.head.is_none() {
            self.head = Some(Box::new(Head3d::new(self.tract(), self.anatomy())));
        }
        self.head.as_deref().expect("built above")
    }
}
