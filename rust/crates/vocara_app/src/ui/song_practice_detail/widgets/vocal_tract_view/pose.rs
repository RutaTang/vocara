//! [`TractPose`]: everything the Vocal tract tab draws, as plain data. It is
//! the seam between the view and the articulatory engine (see the parent
//! module's docs): the engine's estimate is converted into one of these, and
//! the view knows nothing else about it.
//!
//! # Units and frame
//!
//! Every outline is a polyline in **centimetres** in the midsagittal plane of
//! an adult male head: x forward (toward the lips), y up, the glottis at the
//! origin while the larynx is at rest. The view draws them at one uniform
//! scale, so the proportions are the anatomy's.
//!
//! # What each outline is
//!
//! Closed outlines are listed point by point without repeating the first
//! point; open ones run in the direction given.
//!
//! - `outer_wall` (open): the airway's back and upper boundary, from the
//!   glottis up the pharynx, under the velum and the hard palate, past the
//!   upper incisors to the lips.
//! - `inner_wall` (open): the airway's front and lower boundary, from the
//!   lips back over the tongue and down its root to the glottis. With
//!   `outer_wall` it closes the airway.
//! - `pharynx` (open): the posterior pharyngeal wall, from below the glottis
//!   up to the nasopharynx.
//! - `tongue` (closed): from its root by the epiglottis, up its back and over
//!   the dorsum to the tip, then back along its underside and the floor of
//!   the mouth. The dorsum is the part before the frontmost point.
//! - `velum`, `upper_lip`, `lower_lip`, `upper_teeth`, `lower_teeth`,
//!   `jaw` (the mandible), `epiglottis`, `nasal` (the nasal cavity): closed,
//!   each where it is now (the lower teeth, the lower lip and the mandible
//!   already carried by the jaw).

/// A point in the midsagittal plane, in centimetres: x forward, y up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cm {
    pub x: f32,
    pub y: f32,
}

impl Cm {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// The point `t` of the way from `self` to `to`.
    pub fn lerp(self, to: Cm, t: f32) -> Cm {
        Cm::new(self.x + (to.x - self.x) * t, self.y + (to.y - self.y) * t)
    }

    pub fn dist(self, to: Cm) -> f32 {
        (to.x - self.x).hypot(to.y - self.y)
    }

    pub fn plus(self, d: Cm) -> Cm {
        Cm::new(self.x + d.x, self.y + d.y)
    }

    pub fn minus(self, d: Cm) -> Cm {
        Cm::new(self.x - d.x, self.y - d.y)
    }

    pub fn scale(self, k: f32) -> Cm {
        Cm::new(self.x * k, self.y * k)
    }

    /// The unit vector along `self` (zero stays zero).
    pub fn unit(self) -> Cm {
        let len = self.x.hypot(self.y);
        if len > 1e-6 {
            self.scale(1.0 / len)
        } else {
            Cm::default()
        }
    }
}

/// The outlines of one pose (see the module docs for each).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outlines {
    pub outer_wall: Vec<Cm>,
    pub inner_wall: Vec<Cm>,
    pub pharynx: Vec<Cm>,
    pub tongue: Vec<Cm>,
    pub velum: Vec<Cm>,
    pub upper_lip: Vec<Cm>,
    pub lower_lip: Vec<Cm>,
    pub upper_teeth: Vec<Cm>,
    pub lower_teeth: Vec<Cm>,
    pub jaw: Vec<Cm>,
    pub epiglottis: Vec<Cm>,
    pub nasal: Vec<Cm>,
}

impl Outlines {
    /// Every outline, in a fixed order (with whether it is closed).
    pub fn all(&self) -> [(&Vec<Cm>, bool); 12] {
        [
            (&self.outer_wall, false),
            (&self.inner_wall, false),
            (&self.pharynx, false),
            (&self.tongue, true),
            (&self.velum, true),
            (&self.upper_lip, true),
            (&self.lower_lip, true),
            (&self.upper_teeth, true),
            (&self.lower_teeth, true),
            (&self.jaw, true),
            (&self.epiglottis, true),
            (&self.nasal, true),
        ]
    }

    fn all_mut(&mut self) -> [&mut Vec<Cm>; 12] {
        [
            &mut self.outer_wall,
            &mut self.inner_wall,
            &mut self.pharynx,
            &mut self.tongue,
            &mut self.velum,
            &mut self.upper_lip,
            &mut self.lower_lip,
            &mut self.upper_teeth,
            &mut self.lower_teeth,
            &mut self.jaw,
            &mut self.epiglottis,
            &mut self.nasal,
        ]
    }
}

/// One cross-section of the airway, for the 3D view: a ring about the
/// tract's centreline.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AirwayRing {
    /// On the centreline, in the midsagittal plane (cm).
    pub centre: Cm,
    /// The centreline's direction there (a unit vector, toward the lips):
    /// the ring lies across it.
    pub normal: Cm,
    /// The radius of a circle of the section's area (cm), from the area
    /// function.
    pub radius: f32,
}

/// The articulators' positions as the readout panel shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Components {
    /// The gap between the incisors (mm).
    pub jaw_mm: f32,
    /// The tongue body, low (0) to high (1).
    pub tongue_height: f32,
    /// The tongue body, back (0) to front (1).
    pub tongue_front: f32,
    /// The lips, spread (0) to rounded (1).
    pub lip_round: f32,
    /// The opening between the lips (mm).
    pub lip_aperture_mm: f32,
    /// The larynx above (+) or below (−) its rest (mm).
    pub larynx_mm: f32,
    /// The velum, raised and closed (0) to lowered and open (1).
    pub velum_open: f32,
    /// The voice's pitch (Hz; 0 when there is none).
    pub f0_hz: f32,
    /// The voice's phonation, breathy (0) to pressed (1).
    pub voice_quality: f32,
}

impl Components {
    fn values(&self) -> [f32; 9] {
        [
            self.jaw_mm,
            self.tongue_height,
            self.tongue_front,
            self.lip_round,
            self.lip_aperture_mm,
            self.larynx_mm,
            self.velum_open,
            self.f0_hz,
            self.voice_quality,
        ]
    }

    fn set_values(&mut self, v: &[f32]) {
        [
            self.jaw_mm,
            self.tongue_height,
            self.tongue_front,
            self.lip_round,
            self.lip_aperture_mm,
            self.larynx_mm,
            self.velum_open,
            self.f0_hz,
            self.voice_quality,
        ] = [v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]];
    }
}

/// How well the estimated shape explains the formants heard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    #[default]
    Good,
    Fair,
    Uncertain,
}

/// One estimate of the vocal tract's shape (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TractPose {
    pub outlines: Outlines,
    /// The middle of the vocal folds (cm): it rises and falls with the
    /// larynx; the larynx, the folds and the hyoid are drawn about it.
    pub glottis: Cm,
    /// How far the mandible has turned about its condyle from closed
    /// (radians, opening positive). The outlines already carry it; the 3D
    /// view turns its mandible by it.
    pub jaw_angle: f32,
    /// The airway's cross-sections from the glottis to the lips.
    pub rings: Vec<AirwayRing>,
    pub components: Components,
    /// F1, F2, F3 (Hz).
    pub formants: [f32; 3],
    pub fit: Fit,
    /// Whether there was a voice to estimate from: an unvoiced estimate
    /// leaves the figure resting on the last voiced shape.
    pub voiced: bool,
}

/// The number of points of each outline and of rings: two poses of one
/// layout interpolate point by point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    outlines: [usize; 12],
    rings: usize,
}

impl TractPose {
    pub fn layout(&self) -> Layout {
        Layout {
            outlines: self.outlines.all().map(|(o, _)| o.len()),
            rings: self.rings.len(),
        }
    }

    /// Every number that moves, in a fixed order (the layout's).
    pub fn values(&self) -> Vec<f32> {
        let mut v = Vec::with_capacity(1024);
        for (outline, _) in self.outlines.all() {
            for p in outline {
                v.extend([p.x, p.y]);
            }
        }
        v.extend([self.glottis.x, self.glottis.y, self.jaw_angle]);
        for r in &self.rings {
            v.extend([r.centre.x, r.centre.y, r.normal.x, r.normal.y, r.radius]);
        }
        v.extend(self.components.values());
        v.extend(self.formants);
        v
    }

    /// The pose with `v` (as [`TractPose::values`] lists them, for this
    /// pose's layout) in place of its numbers.
    pub fn with_values(&self, v: &[f32]) -> TractPose {
        let mut out = self.clone();
        let mut i = 0;
        let mut next = || {
            let x = v.get(i).copied().unwrap_or_default();
            i += 1;
            x
        };
        for outline in out.outlines.all_mut() {
            for p in outline.iter_mut() {
                *p = Cm::new(next(), next());
            }
        }
        out.glottis = Cm::new(next(), next());
        out.jaw_angle = next();
        for r in &mut out.rings {
            r.centre = Cm::new(next(), next());
            r.normal = Cm::new(next(), next());
            r.radius = next();
        }
        let comps: Vec<f32> = (0..9).map(|_| next()).collect();
        out.components.set_values(&comps);
        out.formants = [next(), next(), next()];
        out
    }

    /// This pose with each outline (and the rings) resampled to `layout`'s
    /// counts, along its length: so a pose can move into one of another
    /// layout without a jump.
    pub fn resampled(&self, layout: &Layout) -> TractPose {
        let mut out = self.clone();
        let closed: Vec<bool> = self.outlines.all().iter().map(|(_, c)| *c).collect();
        for (k, outline) in out.outlines.all_mut().into_iter().enumerate() {
            if outline.len() != layout.outlines[k] {
                *outline = super::geometry::resample(outline, layout.outlines[k], closed[k]);
            }
        }
        if out.rings.len() != layout.rings {
            out.rings = resample_rings(&self.rings, layout.rings);
        }
        out
    }

    /// The pose `t` of the way from `a` to `b` (each number interpolated;
    /// `a` resampled to `b`'s layout first when they differ). What is not a
    /// number (the fit, voicing) is `b`'s past halfway.
    pub fn lerp(a: &TractPose, b: &TractPose, t: f32) -> TractPose {
        let layout = b.layout();
        let a = if a.layout() == layout {
            a.clone()
        } else {
            a.resampled(&layout)
        };
        let (va, vb) = (a.values(), b.values());
        let v: Vec<f32> = va.iter().zip(&vb).map(|(x, y)| x + (y - x) * t).collect();
        let base = if t < 0.5 { &a } else { b };
        base.with_values(&v)
    }
}

fn resample_rings(rings: &[AirwayRing], n: usize) -> Vec<AirwayRing> {
    if rings.is_empty() || n == 0 {
        return vec![AirwayRing::default(); n];
    }
    (0..n)
        .map(|i| {
            let u = if n == 1 {
                0.0
            } else {
                i as f32 / (n - 1) as f32 * (rings.len() - 1) as f32
            };
            let k = (u.floor() as usize).min(rings.len() - 1);
            let f = u - k as f32;
            let a = rings[k];
            let b = rings[(k + 1).min(rings.len() - 1)];
            AirwayRing {
                centre: a.centre.lerp(b.centre, f),
                normal: a.normal.lerp(b.normal, f).unit(),
                radius: a.radius + (b.radius - a.radius) * f,
            }
        })
        .collect()
}
