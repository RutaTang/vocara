//! The fixed, illustrative anatomy around the tract, in centimetres (x
//! forward, y up, the glottis at rest at the origin), from the approved
//! mockup's generator: the articulatory model's published landmarks for an
//! adult male, with standard adult head proportions around them. The head
//! and neck, the eye and the ear, the cervical vertebrae and the hard palate
//! never move; the larynx, the folds and the hyoid are drawn about the
//! pose's glottis.

use std::sync::OnceLock;

use super::geometry::catmull;
use super::pose::Cm;

/// The model's template-to-body transform (an adult male: the oral part
/// scaled by 0.86 forward of x = 0.5).
pub fn tp(x: f32, y: f32) -> Cm {
    if x > 0.5 {
        Cm::new(0.5 + (x - 0.5) * 0.86, y)
    } else {
        Cm::new(x, y)
    }
}

/// The mandible's condyle: the jaw turns about it.
pub const CONDYLE: Cm = Cm::new(-2.5, 10.5);

/// Where the drawn neck ends: below the frame, so it runs out of it.
pub const NECK_FOOT: f32 = -6.6;

/// The posterior nasal spine: the velum hangs from it.
pub fn pns() -> Cm {
    tp(1.0, 9.75)
}

/// A rounded box (a vertebral body): its top-left corner, size and corner
/// radius, in cm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundBox {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

impl RoundBox {
    pub fn centre(&self) -> Cm {
        Cm::new(self.left + self.width / 2.0, self.top - self.height / 2.0)
    }
}

/// The fixed anatomy (see the module docs).
#[derive(Debug)]
pub struct Anatomy {
    /// The head and neck's outline, from the front of the neck's foot over
    /// the face and the crown to the back of the neck's foot, the foot below
    /// the frame (open: its fill closes along the foot).
    pub head: Vec<Cm>,
    /// The top of the head (where its fade starts) and the foot of the neck.
    pub head_top: f32,
    pub head_foot: f32,
    pub ear: Vec<Cm>,
    pub eye: (Cm, f32),
    /// C1 to C6, top to bottom.
    pub vertebrae: [RoundBox; 6],
    /// The hard palate as a bone (closed): its oral surface front to back
    /// along the bottom, its nasal floor above.
    pub palate: Vec<Cm>,
    /// The palate's oral surface, back to front.
    pub palate_surface: Vec<Cm>,
    /// The nasal cavity (closed), as the demo poses carry it.
    pub nasal: Vec<Cm>,
    /// The larynx (thyroid cartilage) with the larynx at rest (closed).
    pub larynx: Vec<Cm>,
    /// The hyoid's centre and half-axes, larynx at rest.
    pub hyoid: (Cm, Cm),
    /// The vocal folds' line, larynx at rest.
    pub folds: (Cm, Cm),
    /// The folds' halo: centre and half-axes.
    pub fold_halo: (Cm, Cm),
}

/// The fixed anatomy, built once.
pub fn anatomy() -> &'static Anatomy {
    static ANATOMY: OnceLock<Anatomy> = OnceLock::new();
    ANATOMY.get_or_init(build)
}

fn pts(list: &[(f32, f32)]) -> Vec<Cm> {
    list.iter().map(|&(x, y)| Cm::new(x, y)).collect()
}

/// The palate's oral surface from the PNS forward (the template's points).
pub fn palate_points() -> Vec<Cm> {
    [
        (1.0, 9.6),
        (1.8, 9.92),
        (2.7, 10.18),
        (3.6, 10.3),
        (4.4, 10.25),
        (5.1, 10.05),
        (5.7, 9.72),
        (6.15, 9.36),
        (6.5, 9.05),
        (6.78, 8.7),
        (6.98, 8.3),
        (7.12, 7.95),
    ]
    .iter()
    .map(|&(x, y)| tp(x, y))
    .collect()
}

fn build() -> Anatomy {
    let pal = catmull(&palate_points(), 6);
    let mut palate = pal.clone();
    palate.extend(pal.iter().rev().map(|p| Cm::new(p.x, p.y + 0.55)));
    let mut nasal = catmull(
        &[
            pns(),
            Cm::new(1.6, 11.6),
            Cm::new(3.2, 13.6),
            Cm::new(5.4, 14.6),
            Cm::new(7.6, 14.2),
            Cm::new(9.1, 12.0),
            Cm::new(9.3, 10.9),
        ],
        5,
    );
    nasal.extend(pal.iter().rev().map(|p| Cm::new(p.x, p.y + 0.6)));
    let head = catmull(
        &pts(&[
            (1.9, -3.2),
            (2.2, -0.6),
            (3.2, 0.9),
            (5.0, 1.7),
            (6.5, 2.05),
            (7.6, 3.1),
            (8.0, 4.4),
            (8.25, 5.3),
            (8.35, 6.35),
            (8.25, 7.2),
            (8.6, 8.0),
            (8.95, 9.1),
            (9.35, 10.15),
            (10.55, 10.55),
            (10.85, 11.35),
            (10.25, 12.6),
            (9.45, 15.0),
            (9.3, 16.4),
            (9.7, 17.6),
            (9.35, 19.6),
            (7.6, 22.6),
            (4.6, 24.9),
            (0.6, 25.8),
            (-3.6, 25.4),
            (-7.3, 23.2),
            (-9.5, 19.4),
            (-9.7, 15.2),
            (-8.6, 11.6),
            (-7.4, 8.6),
            (-6.9, 5.2),
            (-6.9, 1.0),
            (-7.1, -3.2),
        ]),
        5,
    );
    // The neck runs on out of the frame.
    let mut head = head;
    head.insert(0, Cm::new(1.8, NECK_FOOT));
    head.push(Cm::new(-7.15, NECK_FOOT));
    let vertebrae = std::array::from_fn(|i| RoundBox {
        left: -3.55 + 0.04 * i as f32,
        top: 9.55 - 1.85 * i as f32,
        width: 1.85,
        height: 1.38,
        radius: 5.0 / 11.0,
    });
    Anatomy {
        head,
        head_top: 25.8,
        head_foot: NECK_FOOT,
        ear: catmull(
            &pts(&[
                (-3.4, 16.0),
                (-4.6, 16.4),
                (-5.4, 15.0),
                (-5.2, 12.6),
                (-4.2, 11.6),
                (-3.5, 12.4),
            ]),
            4,
        ),
        eye: (Cm::new(7.4, 17.4), 4.2 / 11.0),
        vertebrae,
        palate,
        palate_surface: pal,
        nasal,
        larynx: catmull(
            &pts(&[
                (-0.9, 0.9),
                (0.9, 1.25),
                (2.0, 0.75),
                (2.1, -1.0),
                (1.4, -2.6),
                (-0.6, -2.8),
                (-1.0, -0.8),
            ]),
            4,
        ),
        hyoid: (Cm::new(1.3, 2.85), Cm::new(6.0 / 11.0, 3.0 / 11.0)),
        folds: (Cm::new(-0.75, 0.0), Cm::new(1.45, 0.05)),
        fold_halo: (Cm::new(0.35, 0.02), Cm::new(16.0 / 11.0, 6.0 / 11.0)),
    }
}
