//! The 2D figure's callouts: a name for each part, in two aligned columns
//! either side of the head, each joined to its part by an elbowed leader
//! ending in a dot. The moving parts' names are brighter and their dots
//! coral. Each part's anchor follows the part; the names keep their order
//! down a column and never overlap.

use iced::{Point, Size};

use super::anatomy::CONDYLE;
use super::draw2d::Fit2d;
use super::geometry::{centroid, dorsum, rotate};
use super::pose::{Cm, TractPose};

/// Which column a callout sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// A part the figure names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub text: &'static str,
    /// Whether the part moves as you sing (its name brighter, its dot
    /// coral).
    pub moving: bool,
    pub side: Side,
    /// Where on the part its leader ends (cm).
    pub at: Cm,
}

/// The columns, in cm (the mockup's): the left names end at
/// `LEFT_TEXT`, their leaders bending at `LEFT_ELBOW` and ending at
/// `LEFT_LEAD`; the right ones mirror them.
pub const LEFT_LEAD: f32 = -11.82;
pub const LEFT_ELBOW: f32 = -10.73;
pub const RIGHT_ELBOW: f32 = 13.09;
pub const RIGHT_LEAD: f32 = 14.18;
/// The gap between a leader's end and its name (px).
pub const TEXT_GAP: f32 = 6.0;
/// The least distance between two names' centres in a column (px).
pub const MIN_GAP: f32 = 18.0;
/// A name's half height, for the overlap rule (px; 11 px text).
pub const HALF_HEIGHT: f32 = 7.0;

/// Where each part's callout points for `pose`, each column's parts in
/// their order down the column.
pub fn anchors(pose: &TractPose) -> Vec<Anchor> {
    let o = &pose.outlines;
    let lift = pose.glottis;
    let velum = {
        let c = centroid(&o.velum);
        let back = o
            .velum
            .iter()
            .copied()
            .min_by(|a, b| a.x.total_cmp(&b.x))
            .unwrap_or(c);
        c.lerp(back, 0.4)
    };
    let tongue = dorsum(&o.tongue)
        .iter()
        .copied()
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .map_or(centroid(&o.tongue), |top| top.plus(Cm::new(0.05, -0.85)));
    let pharynx = o
        .outer_wall
        .iter()
        .copied()
        .filter(|p| p.x < 1.0)
        .min_by(|a, b| (a.y - 5.0).abs().total_cmp(&(b.y - 5.0).abs()))
        .map_or(Cm::new(-0.6, 5.0), |p| p.plus(Cm::new(0.45, 0.0)));
    let a = |text, moving, side, at| Anchor {
        text,
        moving,
        side,
        at,
    };
    // Each column's names top to bottom.
    vec![
        a("Nasal cavity", false, Side::Right, Cm::new(4.2, 12.6)),
        a("Hard palate", false, Side::Right, Cm::new(3.4, 10.55)),
        a("Velum", true, Side::Left, velum),
        a(
            "Lips",
            true,
            Side::Right,
            centroid(&o.upper_lip).plus(Cm::new(0.3, -0.05)),
        ),
        a("Teeth", false, Side::Right, centroid(&o.upper_teeth)),
        a("Tongue", true, Side::Right, tongue),
        a(
            "Jaw",
            true,
            Side::Right,
            rotate(Cm::new(4.4, 3.3), CONDYLE, -pose.jaw_angle),
        ),
        a("Pharynx", false, Side::Left, pharynx),
        a("Spine", false, Side::Left, Cm::new(-3.0, 4.0)),
        a(
            "Epiglottis",
            false,
            Side::Left,
            centroid(&o.epiglottis).plus(Cm::new(-0.15, 0.25)),
        ),
        a("Larynx", true, Side::Right, Cm::new(1.6, -1.4).plus(lift)),
        a(
            "Vocal folds",
            true,
            Side::Left,
            Cm::new(-0.6, 0.0).plus(lift),
        ),
    ]
}

/// A callout placed on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub text: &'static str,
    pub moving: bool,
    pub side: Side,
    /// The leader: from the part's dot to the elbow, then along to its end.
    pub leader: [Point; 3],
    /// Where the name is set: its vertical middle, and its right end (left
    /// column) or left end (right column).
    pub label: Point,
}

impl Placed {
    /// The name's vertical extent.
    pub fn span(&self) -> (f32, f32) {
        (self.label.y - HALF_HEIGHT, self.label.y + HALF_HEIGHT)
    }
}

/// The callouts of `pose` placed over a well of `size` drawn at `fit`:
/// down each column in a fixed order (the anatomy's, top to bottom, so no
/// name ever swaps places with another as the parts move), each name level
/// with its part unless that would crowd the one above, then lifted where
/// the column runs past the well's foot.
pub fn place(anchors: &[Anchor], fit: &Fit2d, size: Size) -> Vec<Placed> {
    let gap = (fit.scale * 19.0 / 11.0).max(MIN_GAP);
    let top = HALF_HEIGHT + 4.0;
    let bottom = (size.height - HALF_HEIGHT - 4.0).max(top);
    let mut out = Vec::with_capacity(anchors.len());
    for side in [Side::Left, Side::Right] {
        let column: Vec<(Anchor, Point)> = anchors
            .iter()
            .filter(|a| a.side == side)
            .map(|a| (*a, fit.px(a.at)))
            .collect();
        let mut ys: Vec<f32> = Vec::with_capacity(column.len());
        for (_, p) in &column {
            let y = match ys.last() {
                Some(prev) => p.y.max(prev + gap),
                None => p.y.max(top),
            };
            ys.push(y);
        }
        // Past the foot: lift from the bottom up.
        let mut limit = bottom;
        for y in ys.iter_mut().rev() {
            if *y > limit {
                *y = limit;
            }
            limit = *y - gap;
        }
        let (elbow, lead) = match side {
            Side::Left => (LEFT_ELBOW, LEFT_LEAD),
            Side::Right => (RIGHT_ELBOW, RIGHT_LEAD),
        };
        let elbow_x = fit.px(Cm::new(elbow, 0.0)).x;
        let lead_x = fit.px(Cm::new(lead, 0.0)).x;
        let text_x = match side {
            Side::Left => lead_x - TEXT_GAP,
            Side::Right => lead_x + TEXT_GAP,
        };
        for ((anchor, dot), y) in column.into_iter().zip(ys) {
            out.push(Placed {
                text: anchor.text,
                moving: anchor.moving,
                side,
                leader: [dot, Point::new(elbow_x, y), Point::new(lead_x, y)],
                label: Point::new(text_x, y),
            });
        }
    }
    out
}
