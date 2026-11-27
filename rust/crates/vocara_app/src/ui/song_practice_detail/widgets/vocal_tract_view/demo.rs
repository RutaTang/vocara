//! [`TractPose::demo`]: plausible animated poses — a singer holding "ah",
//! gliding to "ee", then "oo", resting a moment, and round again — built the
//! way the approved mockup's figure was (its generator's outlines, the jaw
//! turned about the condyle, the lips, the tongue body and the velum moved
//! by a handful of articulatory settings). They stand in for the engine's
//! estimates in tests, screenshots and the app until it is wired.

use std::f32::consts::{PI, TAU};

use super::anatomy::{self, pns, tp, CONDYLE};
use super::geometry::{catmull, nearest, resample, rotate, rotate_all, translate_all};
use super::pose::{AirwayRing, Cm, Components, Fit, Outlines, TractPose};

/// The settings a demo pose is built from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Articulation {
    /// The gap at the incisors (cm; the template's neutral is 0.6).
    pub jaw_open: f32,
    /// The tongue body's centre and radius (cm).
    pub tongue: Cm,
    pub tongue_r: f32,
    /// Spread (0) to rounded (1).
    pub lip_round: f32,
    /// The opening between the lips (cm).
    pub lip_open: f32,
    /// Raised (0) to lowered (1).
    pub velum_open: f32,
    /// The larynx above its rest (cm).
    pub larynx: f32,
    pub f0: f32,
    pub quality: f32,
    pub formants: [f32; 3],
}

impl Articulation {
    fn lerp(&self, b: &Articulation, t: f32) -> Articulation {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Articulation {
            jaw_open: l(self.jaw_open, b.jaw_open),
            tongue: self.tongue.lerp(b.tongue, t),
            tongue_r: l(self.tongue_r, b.tongue_r),
            lip_round: l(self.lip_round, b.lip_round),
            lip_open: l(self.lip_open, b.lip_open),
            velum_open: l(self.velum_open, b.velum_open),
            larynx: l(self.larynx, b.larynx),
            f0: l(self.f0, b.f0),
            quality: l(self.quality, b.quality),
            formants: [
                l(self.formants[0], b.formants[0]),
                l(self.formants[1], b.formants[1]),
                l(self.formants[2], b.formants[2]),
            ],
        }
    }
}

/// An open "ah" ([ɑ]): the jaw open, the tongue low and slightly back, the
/// lips apart, the larynx a little high (the approved mockup's pose).
pub const AH: Articulation = Articulation {
    jaw_open: 1.4,
    tongue: Cm::new(2.134, 6.0),
    tongue_r: 2.25,
    lip_round: 0.35,
    lip_open: 1.4,
    velum_open: 0.08,
    larynx: 0.3,
    f0: 392.0,
    quality: 0.48,
    formants: [640.0, 1190.0, 2620.0],
};

/// A close front "ee" ([i]): the jaw nearly closed, the tongue high and
/// forward under the hard palate, the lips spread, the larynx higher.
pub const EE: Articulation = Articulation {
    jaw_open: 0.75,
    tongue: Cm::new(3.2, 7.45),
    tongue_r: 2.2,
    lip_round: 0.05,
    lip_open: 0.55,
    velum_open: 0.05,
    larynx: 0.45,
    f0: 392.0,
    quality: 0.55,
    formants: [290.0, 2250.0, 2890.0],
};

/// A close back "oo" ([u]): the tongue high and back under the velum, the
/// lips rounded and pushed forward, the larynx lowered.
pub const OO: Articulation = Articulation {
    jaw_open: 0.9,
    tongue: Cm::new(1.75, 7.3),
    tongue_r: 2.15,
    lip_round: 0.95,
    lip_open: 0.45,
    velum_open: 0.06,
    larynx: -0.4,
    f0: 349.2,
    quality: 0.44,
    formants: [310.0, 870.0, 2250.0],
};

/// One step of the demo's script: hold `vowel` for `hold` s (voiced or
/// not), then glide to the next step's over `glide` s.
struct Step {
    vowel: Articulation,
    hold: f32,
    glide: f32,
    voiced: bool,
}

const SCRIPT: [Step; 4] = [
    Step {
        vowel: AH,
        hold: 2.4,
        glide: 0.45,
        voiced: true,
    },
    Step {
        vowel: EE,
        hold: 2.4,
        glide: 0.45,
        voiced: true,
    },
    Step {
        vowel: OO,
        hold: 2.4,
        glide: 0.0,
        voiced: true,
    },
    // A breath on the last shape before "ah" again.
    Step {
        vowel: OO,
        hold: 0.8,
        glide: 0.45,
        voiced: false,
    },
];

/// The demo's period (s).
pub fn period() -> f32 {
    SCRIPT.iter().map(|s| s.hold + s.glide).sum()
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The demo's settings at `t` s, with whether it is voiced and how sure an
/// estimate would be (holds are a good fit, glides a fair one).
pub fn script_at(t: f64) -> (Articulation, bool, Fit) {
    let p = f64::from(period());
    let mut u = t.rem_euclid(p) as f32;
    for (i, step) in SCRIPT.iter().enumerate() {
        if u < step.hold {
            let fit = if step.voiced {
                Fit::Good
            } else {
                Fit::Uncertain
            };
            return (wobble(step.vowel, t as f32, u), step.voiced, fit);
        }
        u -= step.hold;
        if u < step.glide {
            let next = &SCRIPT[(i + 1) % SCRIPT.len()];
            let a = step.vowel.lerp(&next.vowel, smoothstep(u / step.glide));
            return (wobble(a, t as f32, 1.0), next.voiced, Fit::Fair);
        }
        u -= step.glide;
    }
    (AH, true, Fit::Good)
}

/// When the demo's drift passes through each vowel's own settings (s): the
/// screenshots' moment.
pub const STILL: f32 = 2.2;

/// A held vowel is never quite still: a slow drift of the jaw and the
/// tongue, and a vibrato that sets in after the onset.
fn wobble(mut a: Articulation, t: f32, held: f32) -> Articulation {
    let u = t - STILL;
    a.jaw_open += 0.14 * (TAU * 0.55 * u).sin();
    a.tongue.y += 0.1 * (TAU * 0.4 * t + 1.0).sin();
    a.tongue.x += 0.09 * (TAU * 0.33 * t + 2.0).sin();
    a.lip_round = (a.lip_round + 0.05 * (TAU * 0.3 * t).sin()).clamp(0.0, 1.0);
    a.larynx += 0.08 * (TAU * 0.45 * t + 0.5).sin();
    let vibrato = smoothstep((held - 0.35) / 0.4);
    a.f0 *= 2f32.powf(vibrato * 0.35 / 12.0 * (TAU * 5.4 * t).sin());
    a.quality += 0.05 * (TAU * 0.25 * t).sin();
    a.formants[0] += 25.0 * (TAU * 0.55 * u).sin();
    a.formants[1] += 45.0 * (TAU * 0.33 * u).sin();
    a
}

impl TractPose {
    /// The demo's pose at `t` s (periodic; see the module docs).
    pub fn demo(t: f64) -> TractPose {
        let (a, voiced, fit) = script_at(t);
        articulate(&a, voiced, fit)
    }
}

/// The jaw's rotation for a gap of `open` cm at the incisors (negative:
/// the front goes down).
fn jaw_rotation(open: f32) -> f32 {
    let r = CONDYLE.dist(Cm::new(6.38, 6.45));
    -(open - 0.6) / r
}

fn pts(list: &[(f32, f32)]) -> Vec<Cm> {
    list.iter().map(|&(x, y)| Cm::new(x, y)).collect()
}

/// How far forward a lip point moves as the lips round (0 at their roots
/// by the teeth, 1 at their front).
fn lip_weight(x: f32) -> f32 {
    ((x - 6.55) / 1.0).clamp(0.0, 1.0)
}

/// A pose built from the settings `a` (see the module docs).
pub fn articulate(a: &Articulation, voiced: bool, fit: Fit) -> TractPose {
    let jaw = jaw_rotation(a.jaw_open);
    let on_jaw = |p: &[Cm]| rotate_all(p, CONDYLE, jaw);
    let lar = a.larynx;
    let glottis = Cm::new(0.0, lar);
    // The mandible: its outer edge and its inner (alveolar) edge.
    let mand0 = catmull(
        &[
            Cm::new(6.0, 6.2),
            Cm::new(6.3, 4.6),
            Cm::new(5.7, 3.1),
            Cm::new(3.6, 2.9),
            Cm::new(1.0, 3.3),
            Cm::new(-1.6, 4.2),
            Cm::new(-2.4, 5.6),
            Cm::new(-2.6, 8.4),
            CONDYLE,
        ],
        5,
    );
    let mut jaw_outline = on_jaw(&mand0);
    let inner: Vec<Cm> = mand0[..16]
        .iter()
        .rev()
        .map(|p| Cm::new(p.x + 0.05, p.y + 0.9))
        .collect();
    jaw_outline.extend(on_jaw(&inner));
    // The teeth: the upper fixed, the lower on the jaw.
    let upper_teeth = catmull(
        &pts(&[
            (6.19, 7.95),
            (6.55, 7.75),
            (6.62, 7.05),
            (6.38, 7.02),
            (6.05, 7.6),
        ]),
        3,
    );
    let li0 = pts(&[
        (6.05, 6.0),
        (6.42, 6.22),
        (6.55, 6.95),
        (6.3, 6.98),
        (5.95, 6.5),
    ]);
    let lower_teeth = catmull(&on_jaw(&li0), 3);
    let incisor = rotate(Cm::new(6.55, 6.95), CONDYLE, jaw);
    // The lips: rounding pushes them forward and thickens them; the lower
    // one rides on the jaw, lifted or dropped to the opening.
    let protrude = -0.1 + 0.65 * a.lip_round;
    let thick = 0.92 + 0.22 * a.lip_round;
    let shape =
        |p: &Cm, mid: f32| Cm::new(p.x + protrude * lip_weight(p.x), mid + (p.y - mid) * thick);
    let ul: Vec<Cm> = pts(&[
        (6.55, 8.3),
        (7.4, 8.4),
        (7.95, 7.8),
        (7.75, 7.15),
        (6.75, 7.2),
    ])
    .iter()
    .map(|p| shape(p, 7.8))
    .collect();
    let upper_lip = catmull(&ul, 4);
    let lip_line = upper_lip
        .iter()
        .filter(|p| p.x > 7.0 + protrude * 0.5)
        .map(|p| p.y)
        .fold(f32::INFINITY, f32::min);
    let ll: Vec<Cm> = pts(&[
        (6.6, 6.75),
        (7.6, 6.8),
        (7.95, 6.2),
        (7.55, 5.5),
        (6.55, 5.6),
    ])
    .iter()
    .map(|p| shape(p, 6.15))
    .collect();
    let ll = on_jaw(&ll);
    let ll_top = ll[..2]
        .iter()
        .map(|p| p.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let lift = ((lip_line - a.lip_open) - ll_top).clamp(-0.6, 0.8);
    let lower_lip = catmull(&translate_all(&ll, Cm::new(0.0, lift)), 4);
    let ll_front = Cm::new(ll[1].x + 0.35, ll_top + lift);
    // The tongue: its body a circle, its root down by the epiglottis, its
    // blade to a tip resting behind the lower incisors.
    let c = a.tongue;
    let r = a.tongue_r;
    let dx = c.x - AH.tongue.x;
    let arc: Vec<Cm> = (0..25)
        .map(|k| {
            let deg = 205.0 - 8.0 * k as f32;
            let t = deg * PI / 180.0;
            Cm::new(c.x + r * t.cos(), c.y + r * t.sin())
        })
        .collect();
    let tip = rotate(Cm::new(5.55, 6.25), CONDYLE, jaw);
    let arc_end = c.plus(Cm::new(
        r * 10f32.to_radians().cos(),
        r * 10f32.to_radians().sin(),
    ));
    let blade = arc_end.lerp(tip, 0.5).plus(Cm::new(0.1, 0.49));
    let under = on_jaw(&pts(&[(5.25, 5.55), (4.3, 4.55)]));
    let root = [
        Cm::new(0.75 + 0.35 * dx, 1.85 + 0.6 * lar),
        Cm::new(0.2 + 0.6 * dx, 3.0 + 0.3 * lar),
        Cm::new(-0.05 + 0.85 * dx, 4.4 + 0.1 * lar),
    ];
    let mut t_pts: Vec<Cm> = root.to_vec();
    t_pts.extend(&arc);
    t_pts.extend([blade, tip]);
    t_pts.extend(&under);
    t_pts.extend([
        Cm::new(2.7, 3.35 + 0.2 * lar),
        Cm::new(1.45, 2.4 + 0.4 * lar),
    ]);
    let tongue = catmull(&t_pts, 3);
    // The velum, turned down about the PNS as it opens.
    let velum_turn = 0.5 * a.velum_open;
    let on_velum = |p: Cm| rotate(p, pns(), velum_turn);
    let velum = rotate_all(
        &catmull(
            &[
                pns(),
                Cm::new(0.3, 9.95),
                Cm::new(-0.45, 9.55),
                Cm::new(-0.85, 9.0),
                Cm::new(-0.7, 8.75),
                Cm::new(-0.2, 8.95),
                Cm::new(0.45, 9.25),
                Cm::new(0.95, 9.45),
            ],
            4,
        ),
        pns(),
        velum_turn,
    );
    let epi_shift = Cm::new(0.35 * dx, 0.7 * lar);
    let epiglottis = translate_all(
        &catmull(
            &pts(&[
                (0.55, 1.6),
                (0.1, 2.7),
                (-0.25, 3.9),
                (-0.05, 4.05),
                (0.4, 2.9),
                (0.85, 1.8),
            ]),
            3,
        ),
        epi_shift,
    );
    let pharynx: Vec<Cm> = catmull(
        &[
            (-1.0, -1.5),
            (-1.1, 1.5),
            (-1.2, 3.6),
            (-1.3, 5.6),
            (-1.38, 7.5),
            (-1.33, 8.9),
            (-1.05, 10.1),
            (-0.45, 10.9),
            (0.4, 11.3),
        ]
        .iter()
        .map(|&(x, y)| tp(x, y))
        .collect::<Vec<_>>(),
        6,
    )
    .into_iter()
    .map(|p| Cm::new(p.x, p.y + lar * (1.0 - p.y / 6.0).clamp(0.0, 1.0)))
    .collect();
    // The airway's walls.
    let outer_wall = catmull(
        &[
            Cm::new(-0.85, -0.05 + lar),
            Cm::new(-0.95, 2.0 + 0.6 * lar),
            Cm::new(-1.05, 4.5 + 0.2 * lar),
            Cm::new(-1.15, 7.0),
            on_velum(Cm::new(-0.75, 8.7)),
            on_velum(Cm::new(0.6, 9.2)),
            Cm::new(1.6, 9.62),
            Cm::new(2.6, 9.95),
            Cm::new(3.8, 10.05),
            Cm::new(5.0, 9.6),
            Cm::new(6.0, 8.7),
            Cm::new(6.6, 7.4),
            Cm::new(7.9 + protrude, lip_line),
        ],
        4,
    );
    let mut inner_pts = vec![
        ll_front,
        Cm::new(incisor.x + 0.45, incisor.y + 0.12 + lift.min(0.0) * 0.5),
        blade.plus(Cm::new(0.0, 0.05)),
    ];
    inner_pts.extend(arc.iter().rev().step_by(2).map(|p| {
        let out = p.minus(c).unit().scale(0.04);
        p.plus(out)
    }));
    inner_pts.extend([
        Cm::new(-0.05 + 0.85 * dx, 4.2 + 0.1 * lar),
        Cm::new(0.05 + 0.3 * dx, 2.4 + 0.5 * lar),
        Cm::new(0.6, 0.1 + lar),
    ]);
    let inner_wall = catmull(&inner_pts, 4);
    let rings = rings_between(&outer_wall, &inner_wall, glottis, 18);
    let components = Components {
        jaw_mm: a.jaw_open * 10.0,
        tongue_height: ((c.y - 5.3) / 2.5).clamp(0.0, 1.0),
        tongue_front: ((c.x - 1.4) / 2.0).clamp(0.0, 1.0),
        lip_round: a.lip_round.clamp(0.0, 1.0),
        lip_aperture_mm: a.lip_open * 10.0,
        larynx_mm: lar * 10.0,
        velum_open: a.velum_open.clamp(0.0, 1.0),
        f0_hz: if voiced { a.f0 } else { 0.0 },
        voice_quality: a.quality.clamp(0.0, 1.0),
    };
    TractPose {
        outlines: Outlines {
            outer_wall,
            inner_wall,
            pharynx,
            tongue,
            velum,
            upper_lip,
            lower_lip,
            upper_teeth,
            lower_teeth,
            jaw: jaw_outline,
            epiglottis,
            nasal: anatomy::anatomy().nasal.clone(),
        },
        glottis,
        jaw_angle: -jaw,
        rings,
        components,
        formants: a.formants,
        fit,
        voiced,
    }
}

/// `n` cross-sections of the airway between its walls, from the glottis to
/// the lips: each centred midway across, its radius that of a circle of the
/// section's area (the midsagittal distance `d` taken to an area of
/// 1.6·d^1.5 cm², a common area function), its normal along the
/// centreline.
pub fn rings_between(outer: &[Cm], inner: &[Cm], glottis: Cm, n: usize) -> Vec<AirwayRing> {
    let outer = resample(outer, 160, false);
    let inner = resample(inner, 400, false);
    let mut centres: Vec<(Cm, f32)> = (0..n)
        .map(|i| {
            if i == 0 {
                // The glottis: a ring the width of the larynx's lumen.
                return (glottis.plus(Cm::new(0.35, 0.05)), 0.7);
            }
            let u = i as f32 / (n - 1) as f32;
            let k = ((u * (outer.len() - 1) as f32).round() as usize).min(outer.len() - 1);
            let o = outer[k];
            let (q, d) = nearest(&inner, o);
            let area = 1.6 * d.max(0.05).powf(1.5);
            (o.lerp(q, 0.5), (area / PI).sqrt())
        })
        .collect();
    // Smooth the centreline once (the walls' bends make it uneven).
    let raw = centres.clone();
    for i in 1..n.saturating_sub(1) {
        let c = raw[i - 1].0.plus(raw[i].0.scale(2.0)).plus(raw[i + 1].0);
        centres[i].0 = c.scale(0.25);
    }
    (0..n)
        .map(|i| {
            let prev = centres[i.saturating_sub(1)].0;
            let next = centres[(i + 1).min(n - 1)].0;
            AirwayRing {
                centre: centres[i].0,
                // The glottis lies level.
                normal: if i == 0 {
                    Cm::new(0.0, 1.0)
                } else {
                    next.minus(prev).unit()
                },
                radius: centres[i].1,
            }
        })
        .collect()
}
