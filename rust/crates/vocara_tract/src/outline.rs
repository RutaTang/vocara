//! The 2D cut-away: an articulation's midsagittal section as polylines by structure, as
//! CHIMERA's 2D anatomy view (`src/ui/render/anatomy.ts`) draws it, back to front: the skull
//! and brain, the eye, the spine, the nasal cavity, the palate, the hyoid, larynx and trachea,
//! the skin, the airway's walls, the epiglottis, the teeth, the vocal folds, the tongue (its
//! fibres, then its outline), the jaw, the lips and the velum (smoothed, as CHIMERA smooths
//! them: two and three rounds of corner cutting, closed). CHIMERA clips the tongue's fibres to
//! the tongue's outline as it draws them; here they come clipped (each fibre's runs inside the
//! outline, closed along the floor of the mouth, are lines of their own).
//!
//! Head centimetres at the body's size: x forward, y up, the glottis at the origin for a
//! resting larynx ([`crate::lines`]). Build them with [`TractModel::outlines`].

use crate::anatomy::{HumanArtic, HumanBody, HumanFrame, HumanVocalTract};
use crate::forward::TractModel;
use crate::geom::{chaikin_closed, Vec2};
use crate::lines::{Emphasis, Outlines2d, Structure};
use crate::profile::Profile;

/// What building the outlines keeps between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutlineBuilder {
    pub(crate) profile: Profile,
    smooth: Vec<Vec2>,
    scratch: Vec<Vec2>,
}

fn put(out: &mut Outlines2d, s: Structure, e: Emphasis, closed: bool, pts: &[Vec2]) {
    out.begin(s, e, closed, false)
        .extend(pts.iter().map(|p| [p.x as f32, p.y as f32]));
    out.drop_short();
}

impl OutlineBuilder {
    pub(crate) fn build(
        &mut self,
        t: &HumanVocalTract,
        body: &HumanBody,
        f: &HumanFrame,
        a: &HumanArtic,
        out: &mut Outlines2d,
    ) {
        use Emphasis::{Detail, Outline};
        use Structure::*;
        self.profile.build(t, body, f, a);
        let g = &self.profile;
        out.clear();
        put(out, Skull, Detail, false, &g.inset);
        put(out, Skull, Outline, true, &g.skull);
        put(out, Brain, Outline, true, &g.brain);
        put(out, Brain, Detail, false, &g.sulcus);
        put(out, Eye, Outline, true, &g.eye);
        for v in &g.vertebrae[..g.n_vertebrae] {
            put(out, Spine, Outline, true, v);
        }
        for c in &g.cords {
            put(out, Spine, Detail, false, c);
        }
        put(out, NasalCavity, Outline, true, &g.nasal);
        for t in &g.turbinates {
            put(out, NasalCavity, Detail, true, t);
        }
        put(out, HardPalate, Outline, false, &g.pal_oral);
        put(out, HardPalate, Detail, false, &g.pal_nasal);
        put(out, Hyoid, Outline, true, &g.hyoid);
        put(out, Larynx, Outline, true, &g.thyroid);
        for tr in &g.trachea {
            put(out, Trachea, Outline, false, tr);
        }
        for r in &g.rings[..g.n_rings] {
            put(out, Trachea, Detail, true, r);
        }
        put(out, Skin, Outline, false, &g.skin_lower);
        put(out, Skin, Outline, false, &g.skin_upper);
        put(out, Airway, Outline, false, &g.outer_wall);
        put(out, Airway, Outline, false, &f.inner);
        put(out, Airway, Detail, false, &g.glottis_wall);
        put(
            out,
            Epiglottis,
            Outline,
            false,
            &f.epiglottis[..f.epiglottis.len().min(3)],
        );
        put(out, UpperTeeth, Outline, true, &f.upper_teeth);
        put(out, LowerTeeth, Outline, false, &g.lower_teeth);
        for fold in &g.folds {
            put(out, VocalFolds, Outline, true, fold);
        }
        for fibre in &g.fibres {
            clip_inside(fibre, &f.tongue, out, TongueMuscle, Detail);
        }
        // left open: its closing chord runs along the floor of the mouth
        put(out, Tongue, Outline, false, &f.tongue);
        put(out, Jaw, Outline, false, &g.jaw_lower);
        put(out, Jaw, Detail, false, &g.jaw_upper);
        for (s, pts, rounds) in [
            (UpperLip, &f.upper_lip, 2),
            (LowerLip, &f.lower_lip, 2),
            (Velum, &f.velum, 3),
        ] {
            chaikin_closed(pts, rounds, &mut self.smooth, &mut self.scratch);
            // the smoothed loop repeats its first point: drawn as a closed line without it
            let n = self.smooth.len().saturating_sub(1);
            put(out, s, Outline, true, &self.smooth[..n]);
        }
    }
}

impl TractModel {
    /// The 2D cut-away of `a` (see the module docs), into `out` (its buffers are reused).
    pub fn outlines(&mut self, a: &HumanArtic, out: &mut Outlines2d) {
        self.apply(a);
        // the builder is the model's, apart from its airway: taken out for the call
        let mut b = std::mem::take(&mut self.outline_builder);
        b.build(self.tract(), self.anatomy(), self.frame(), a, out);
        self.outline_builder = b;
    }
}

/// Whether `p` is inside the closed polygon `poly` (even-odd rule).
fn inside(poly: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + ((p.y - a.y) * (b.x - a.x)) / (b.y - a.y) {
            inside = !inside;
        }
    }
    inside
}

/// Where segment a → b first crosses the closed polygon's edge (as a point), if it does.
fn first_crossing(poly: &[Vec2], a: Vec2, b: Vec2) -> Option<Vec2> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let n = poly.len();
    let mut best: Option<f64> = None;
    for i in 0..n {
        let (p, q) = (poly[i], poly[(i + 1) % n]);
        let (ex, ey) = (q.x - p.x, q.y - p.y);
        let den = dx * ey - dy * ex;
        if den.abs() < 1e-15 {
            continue;
        }
        let t = ((p.x - a.x) * ey - (p.y - a.y) * ex) / den;
        let u = ((p.x - a.x) * dy - (p.y - a.y) * dx) / den;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    }
    best.map(|t| crate::geom::v(a.x + dx * t, a.y + dy * t))
}

/// The runs of polyline `line` inside the closed polygon `poly`, each a line of `out`, cut at
/// the polygon's edge.
fn clip_inside(line: &[Vec2], poly: &[Vec2], out: &mut Outlines2d, s: Structure, e: Emphasis) {
    if poly.len() < 3 {
        return;
    }
    let mut open: Option<&mut Vec<[f32; 2]>> = None;
    let mut prev: Option<(Vec2, bool)> = None;
    let pt = |p: Vec2| [p.x as f32, p.y as f32];
    for &p in line {
        let now = inside(poly, p);
        match (prev, now) {
            (Some((a, true)), false) => {
                // leaving: end the run at the edge
                if let (Some(run), Some(x)) = (open.as_deref_mut(), first_crossing(poly, a, p)) {
                    run.push(pt(x));
                }
                open = None;
                out.drop_short();
            }
            (Some((a, false)), true) => {
                // entering: start a run at the edge
                let run = out.begin(s, e, false, false);
                if let Some(x) = first_crossing(poly, p, a) {
                    run.push(pt(x));
                }
                run.push(pt(p));
                open = Some(run);
            }
            (None, true) => {
                let run = out.begin(s, e, false, false);
                run.push(pt(p));
                open = Some(run);
            }
            (_, true) => {
                if let Some(run) = open.as_deref_mut() {
                    run.push(pt(p));
                }
            }
            _ => {}
        }
        prev = Some((p, now));
    }
    if open.is_some() {
        out.drop_short();
    }
}
