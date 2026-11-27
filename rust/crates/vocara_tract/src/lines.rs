//! Drawing data: polylines grouped by the structure they draw, for the 2D cut-away
//! ([`Outlines2d`], see [`crate::outline`]) and the 3D head ([`Lines3d`], see
//! [`crate::head3d`]).
//!
//! Head coordinates throughout, in centimetres at the body's size: x forward, y up, z out of
//! the right side of the face, the glottis at the origin for a resting larynx. A set is rebuilt
//! frame after frame into the same buffers ([`LineSet::clear`] keeps them), so a renderer that
//! keeps its set allocates nothing once warm.

/// A point of the 2D cut-away (x, y).
pub type Point2 = [f32; 2];
/// A point of the 3D head (x, y, z).
pub type Point3 = [f32; 3];

/// What a line draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Structure {
    /// The skin: the face's and neck's profile on the midline; in 3D also the head's wireframe
    /// shell around it.
    Skin,
    /// The skull's section (and the inner table of the bone).
    Skull,
    Brain,
    /// The eye's outline (2D; in 3D the eyes are spheres, see `Head3d::eyes`).
    Eye,
    /// The ears (3D).
    Ear,
    /// The nose's sides, wings and nostrils (3D; in 2D the nose is the skin's profile).
    Nose,
    /// The cervical vertebrae and the spinal cord.
    Spine,
    /// The nasal cavity: its outline, side walls and turbinates.
    NasalCavity,
    /// The hard palate (its oral and nasal surfaces; in 3D also its vault).
    HardPalate,
    /// The soft palate, closing the nasal port when raised.
    Velum,
    /// The tongue's outline (in 3D also its surface's ribs).
    Tongue,
    /// The tongue's muscle fibres fanning from the jaw (2D, drawn inside the tongue's outline).
    TongueMuscle,
    /// The mandible, carried by the jaw.
    Jaw,
    UpperTeeth,
    LowerTeeth,
    UpperLip,
    LowerLip,
    /// The lips' opening (3D: the mouth's line, closed when the lips part).
    Mouth,
    Epiglottis,
    /// The airway's walls: the posterior pharyngeal wall over the velum, palate and incisors to
    /// the lips, and the tongue's surface opposite (2D); in 3D its cross-sections and side
    /// walls, as wide as the acoustics has them.
    Airway,
    /// The thyroid and cricoid cartilages.
    Larynx,
    VocalFolds,
    Hyoid,
    Trachea,
}

impl Structure {
    /// Every structure, in this order.
    pub const ALL: [Structure; 24] = [
        Structure::Skin,
        Structure::Skull,
        Structure::Brain,
        Structure::Eye,
        Structure::Ear,
        Structure::Nose,
        Structure::Spine,
        Structure::NasalCavity,
        Structure::HardPalate,
        Structure::Velum,
        Structure::Tongue,
        Structure::TongueMuscle,
        Structure::Jaw,
        Structure::UpperTeeth,
        Structure::LowerTeeth,
        Structure::UpperLip,
        Structure::LowerLip,
        Structure::Mouth,
        Structure::Epiglottis,
        Structure::Airway,
        Structure::Larynx,
        Structure::VocalFolds,
        Structure::Hyoid,
        Structure::Trachea,
    ];
}

/// How a line weighs in its structure's drawing (CHIMERA draws a structure's main outline
/// bright and its details dimmer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Emphasis {
    /// The structure's own outline.
    Outline,
    /// A detail: a second surface, an inner line, a wireframe line.
    Detail,
}

/// One polyline of a [`LineSet`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line<'a, P> {
    pub structure: Structure,
    pub emphasis: Emphasis,
    /// Whether the last point joins the first.
    pub closed: bool,
    /// In 3D: part of the midsagittal cut-away (the 2D drawing, at z = 0). Seen edge-on from
    /// the front or above it piles into one bright bar; CHIMERA fades it as the head turns.
    pub section: bool,
    pub points: &'a [P],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Meta {
    structure: Structure,
    emphasis: Emphasis,
    closed: bool,
    section: bool,
    /// where its points start in the set's buffer (they end where the next line's start)
    start: usize,
}

/// Polylines, rebuilt in place frame after frame (see the module docs). The points of every
/// line share one buffer, so a frame with a few more or fewer lines than the last (a ring of
/// the trachea more, a tongue fibre clipped in two) reuses the same memory.
#[derive(Clone, Debug, PartialEq)]
pub struct LineSet<P> {
    meta: Vec<Meta>,
    points: Vec<P>,
}

impl<P> Default for LineSet<P> {
    fn default() -> Self {
        LineSet {
            meta: Vec::new(),
            points: Vec::new(),
        }
    }
}

/// The 2D cut-away's lines.
pub type Outlines2d = LineSet<Point2>;
/// The 3D head's lines.
pub type Lines3d = LineSet<Point3>;

impl<P: Copy> LineSet<P> {
    /// Empties the set, keeping its buffers.
    pub fn clear(&mut self) {
        self.meta.clear();
        self.points.clear();
    }

    /// Line `i` (in drawing order).
    pub fn line(&self, i: usize) -> Line<'_, P> {
        let m = self.meta[i];
        let end = self
            .meta
            .get(i + 1)
            .map_or(self.points.len(), |next| next.start);
        Line {
            structure: m.structure,
            emphasis: m.emphasis,
            closed: m.closed,
            section: m.section,
            points: &self.points[m.start..end],
        }
    }

    /// The lines, in drawing order (back to front, as CHIMERA draws them).
    pub fn lines(&self) -> impl ExactSizeIterator<Item = Line<'_, P>> + '_ {
        (0..self.meta.len()).map(|i| self.line(i))
    }

    /// The lines of one structure.
    pub fn of(&self, structure: Structure) -> impl Iterator<Item = Line<'_, P>> + '_ {
        self.lines().filter(move |l| l.structure == structure)
    }

    /// How many lines.
    pub fn len(&self) -> usize {
        self.meta.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    /// All the points, line after line.
    pub fn points(&self) -> &[P] {
        &self.points
    }

    /// Starts a line; its points are what is pushed onto the returned buffer until the next
    /// line starts (only push: the buffer holds the earlier lines' points too).
    pub(crate) fn begin(
        &mut self,
        structure: Structure,
        emphasis: Emphasis,
        closed: bool,
        section: bool,
    ) -> &mut Vec<P> {
        self.meta.push(Meta {
            structure,
            emphasis,
            closed,
            section,
            start: self.points.len(),
        });
        &mut self.points
    }

    /// Drops the last line if it has fewer than two points (CHIMERA's `Lines.end` keeps only
    /// lines of two points or more).
    pub(crate) fn drop_short(&mut self) {
        if let Some(m) = self.meta.last() {
            if self.points.len() - m.start < 2 {
                self.points.truncate(m.start);
                self.meta.pop();
            }
        }
    }
}
