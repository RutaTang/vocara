//! The forward model: an articulation → its geometry, area function and formants, for one body.
//!
//! [`TractModel`] holds a body's anatomy and airway ([`HumanAirway`], sized as CHIMERA's UI
//! sizes it, at [`ANALYSIS_RATE`]) and every buffer a frame needs, so after the first call
//! nothing it does allocates.
//!
//! # Formants
//!
//! A formant here is what CHIMERA's UI reads: a peak of the airway's volume-velocity transfer
//! function |U_lips / U_glottis| above 150 Hz, the glottis cut (an ideal flow source), injected
//! at the first pharynx node. [`TractModel::transfer`] evaluates that function at any
//! frequencies (CHIMERA's reference: 360 log-spaced from 60 Hz to 8 kHz, then
//! [`find_peaks`](crate::airway::frequency::find_peaks)). [`TractModel::formants`] finds the
//! same peaks with fewer evaluations: a log-spaced scan up from 150 Hz (4.4 % apart, the same
//! maximum test as `find_peaks`), stopping once it has the formants asked for, then each
//! maximum's bracket searched by golden section and its vertex placed by a parabola in dB over
//! log-frequency. It agrees with the reference to within a few tenths of a percent (the
//! reference's own grid error; `tests/forward.rs`). Each formant's bandwidth comes from that
//! parabola's curvature, as a single resonance's −3 dB width would (an estimate: a
//! neighbouring resonance widens or narrows it).

use crate::airway::frequency::TransferSolver;
use crate::airway::human::{HumanAirway, ANALYSIS_RATE};
use crate::airway::network::LinearElements;
use crate::anatomy::{BodyMix, HumanArtic, HumanBody, HumanFrame, HumanVocalTract};
use crate::head3d::Head3d;
use crate::math::{exp, ln, log10, sqrt};
use crate::outline::OutlineBuilder;

/// The most formants [`TractModel::formants`] reports.
pub const MAX_FORMANTS: usize = 4;

/// The formants of one articulation, lowest first. `freq[i]` / `bw[i]` are NaN past
/// [`Formants::count`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Formants {
    /// Peak frequencies (Hz).
    pub freq: [f64; MAX_FORMANTS],
    /// −3 dB bandwidths (Hz), estimated from each peak's curvature (see the module docs).
    pub bw: [f64; MAX_FORMANTS],
    /// How many peaks were found (at most [`MAX_FORMANTS`]).
    pub count: usize,
}

impl Formants {
    pub const NONE: Formants = Formants {
        freq: [f64::NAN; MAX_FORMANTS],
        bw: [f64::NAN; MAX_FORMANTS],
        count: 0,
    };

    /// Formant `n` (1-based: `f(1)` is F1), NaN when the model has fewer peaks.
    pub fn f(&self, n: usize) -> f64 {
        self.freq
            .get(n.wrapping_sub(1))
            .copied()
            .unwrap_or(f64::NAN)
    }
}

/// One measuring line of the geometric area function (see [`crate::anatomy::GridSample`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AreaSample {
    /// Distance from the glottis along the centreline (cm).
    pub s: f64,
    /// Cross-sectional area (cm²).
    pub area: f64,
    /// Midsagittal gap (cm), 0 where closed.
    pub gap: f64,
}

/// One section of the tubes the acoustics sees (the geometry resampled: harmonic-mean areas
/// over equal sections of the pharynx, glottis → velum, and the mouth, velum → lips).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TubeSection {
    /// Where the section starts and ends, from the glottis (cm).
    pub s0: f64,
    pub s1: f64,
    /// Its area (cm²); 0 when closed.
    pub area: f64,
}

/// An articulation's area function: the geometry's measuring lines and the acoustic tubes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AreaFunction {
    /// The measuring grid, glottis to lips (above the epilarynx, which starts at s = 0).
    pub samples: Vec<AreaSample>,
    /// The pharynx's sections, then the mouth's.
    pub sections: Vec<TubeSection>,
    /// Glottis → lips (cm).
    pub length: f64,
    /// Glottis → velopharyngeal junction (cm).
    pub pharynx_length: f64,
    /// Glottis → upper incisors' edge (cm).
    pub teeth_s: f64,
    /// Velopharyngeal port area (cm²): 0 with the velum closed.
    pub velum_port: f64,
}

/// The formant scan: log-spaced from 150 Hz (CHIMERA's UI reads no formant below it) to past a
/// child's F4, 4.4 % apart up to F3 and 2 % apart past it.
const SCAN_LO: f64 = 150.0;
const SCAN_HI: f64 = 6500.0;
const SCAN_STEP: f64 = 0.043;
const SCAN_STEP_FINE: f64 = 0.0198;
/// Golden-section steps within a maximum's bracket (8.6 % wide at most): 7 leave 0.3 %.
const GOLDEN_STEPS: usize = 7;
/// Half the spacing (in ln f) of the parabola that places a peak's vertex.
const VERTEX_STEP: f64 = 0.0015;
/// dB per unit of the squared normalised offset at a resonance: |H|² ∝ 1/(1 + (2Δf/B)²) →
/// dB ≈ peak − (10/ln 10)·(2Δf/B)² near the peak.
const DB_PER_UNIT: f64 = 10.0 / core::f64::consts::LN_10;

/// A body's vocal-tract model (see the module docs).
#[derive(Clone, Debug)]
pub struct TractModel {
    body: BodyMix,
    airway: HumanAirway,
    el: LinearElements,
    solver: TransferSolver,
    pub(crate) outline_builder: OutlineBuilder,
    /// the 3D head's rest shapes and buffers, built on first use
    pub(crate) head: Option<Box<Head3d>>,
}

impl TractModel {
    /// The model of body `body` (a blend of the adult male, adult female and child CHIMERA
    /// calibrated).
    pub fn new(body: BodyMix) -> Self {
        TractModel {
            body,
            airway: HumanAirway::new(ANALYSIS_RATE, body.body()),
            el: LinearElements::default(),
            solver: TransferSolver::default(),
            outline_builder: OutlineBuilder::default(),
            head: None,
        }
    }

    /// The body this model is of.
    pub fn body(&self) -> BodyMix {
        self.body
    }

    /// The body's dimensions (blended as CHIMERA blends them).
    pub fn anatomy(&self) -> &HumanBody {
        &self.airway.body
    }

    /// The anatomy model (template ↔ head coordinates, the fixed walls).
    pub fn tract(&self) -> &HumanVocalTract {
        &self.airway.tract
    }

    /// Computes the geometry of `a` and sets the airway to it; returns the frame (also
    /// [`TractModel::frame`] until the next call).
    pub fn apply(&mut self, a: &HumanArtic) -> &HumanFrame {
        self.airway.apply(a)
    }

    /// The geometry of the last articulation applied.
    pub fn frame(&self) -> &HumanFrame {
        &self.airway.frame
    }

    /// Applies `a` and readies the transfer function's solver for it.
    fn prepare(&mut self, a: &HumanArtic) {
        self.airway.apply(a);
        self.airway.net.linear_elements_into(&mut self.el);
        self.solver.prepare(
            &self.el,
            self.airway.pharynx.first_node(),
            &[self.airway.glottis_port],
        );
    }

    /// |U_lips / U_glottis| in dB (20·log10(|H| + 1e-12), as CHIMERA's UI draws it) at each of
    /// `freqs`, into `db` (same length).
    pub fn transfer(&mut self, a: &HumanArtic, freqs: &[f64], db: &mut [f64]) {
        self.prepare(a);
        for (f, out) in freqs.iter().zip(db.iter_mut()) {
            let mag = self.solver.eval_mag(*f);
            *out = 20.0 * log10(mag + 1e-12);
        }
    }

    /// The formants of `a` (see the module docs).
    pub fn formants(&mut self, a: &HumanArtic) -> Formants {
        self.prepare(a);
        self.formants_of_prepared(MAX_FORMANTS)
    }

    /// The lowest `n` formants of `a` (at most [`MAX_FORMANTS`]): the scan stops once it has
    /// them, which saves the evaluations above them.
    pub fn lowest_formants(&mut self, a: &HumanArtic, n: usize) -> Formants {
        self.prepare(a);
        self.formants_of_prepared(n.min(MAX_FORMANTS))
    }

    /// The transfer function in dB at log-frequency `lf`.
    fn db_at_ln(&mut self, lf: f64) -> f64 {
        20.0 * log10(self.solver.eval_mag(exp(lf)) + 1e-30)
    }

    fn formants_of_prepared(&mut self, want: usize) -> Formants {
        let mut out = Formants::NONE;
        let ln_hi = ln(SCAN_HI);
        // the last two scan points: (ln f, dB)
        let mut p2 = ln(SCAN_LO);
        let mut d2 = self.db_at_ln(p2);
        let mut p1 = p2 + SCAN_STEP;
        let mut d1 = self.db_at_ln(p1);
        while out.count < want {
            // finer past F3: F4 can lie a few percent below the piriform fossae's narrow
            // resonance, which a coarse scan sees as one rise
            let step = if out.count >= 3 {
                SCAN_STEP_FINE
            } else {
                SCAN_STEP
            };
            let p0 = p1 + step;
            if p0 > ln_hi {
                break;
            }
            let d0 = self.db_at_ln(p0);
            // a maximum at the middle point (CHIMERA's find_peaks test)
            let is_peak = d1 > d2 && d1 >= d0;
            let lo = p2;
            (p2, d2, p1, d1) = (p1, d1, p0, d0);
            if !is_peak {
                continue;
            }
            let (freq, bw) = self.peak_in(lo, p0);
            out.freq[out.count] = freq;
            out.bw[out.count] = bw;
            out.count += 1;
        }
        out
    }

    /// The maximum of the transfer function between log-frequencies `lo` and `hi` (the scan
    /// found one there): golden-section search, then a
    /// parabola through the best point and two close beside it. Returns its frequency and the
    /// bandwidth its curvature gives.
    fn peak_in(&mut self, mut lo: f64, mut hi: f64) -> (f64, f64) {
        const R: f64 = 0.618_033_988_749_894_9;
        let mut x1 = hi - R * (hi - lo);
        let mut x2 = lo + R * (hi - lo);
        let (mut f1, mut f2) = (self.db_at_ln(x1), self.db_at_ln(x2));
        for _ in 0..GOLDEN_STEPS {
            if f1 > f2 {
                hi = x2;
                x2 = x1;
                f2 = f1;
                x1 = hi - R * (hi - lo);
                f1 = self.db_at_ln(x1);
            } else {
                lo = x1;
                x1 = x2;
                f1 = f2;
                x2 = lo + R * (hi - lo);
                f2 = self.db_at_ln(x2);
            }
        }
        let (xm, fm) = if f1 > f2 { (x1, f1) } else { (x2, f2) };
        let (a, c) = (
            self.db_at_ln(xm - VERTEX_STEP),
            self.db_at_ln(xm + VERTEX_STEP),
        );
        let off = parabola_offset(a, fm, c).clamp(-1.0, 1.0);
        let lf = xm + off * VERTEX_STEP;
        let freq = exp(lf);
        // curvature in dB per (ln f)², and in dB per Hz² at the peak (d ln f = df / f)
        let curvature = (a - 2.0 * fm + c) / (VERTEX_STEP * VERTEX_STEP) / (freq * freq);
        let bw = if curvature < 0.0 {
            2.0 * sqrt(-2.0 * DB_PER_UNIT / curvature)
        } else {
            f64::NAN
        };
        (freq, bw)
    }
    /// The area function of `a`, into `out` (its buffers are reused).
    pub fn area_function(&mut self, a: &HumanArtic, out: &mut AreaFunction) {
        self.airway.apply(a);
        let f = &self.airway.frame;
        out.samples.clear();
        out.samples.extend(f.grid.iter().map(|g| AreaSample {
            s: g.s,
            area: g.area,
            gap: g.d,
        }));
        out.sections.clear();
        let (n_ph, n_or) = (self.airway.n_ph, self.airway.n_or);
        let (ph_len, or_len) = (self.airway.ph_len, self.airway.or_len);
        for (i, &area) in self.airway.ph_areas.iter().enumerate() {
            out.sections.push(TubeSection {
                s0: ph_len * i as f64 / n_ph as f64,
                s1: ph_len * (i + 1) as f64 / n_ph as f64,
                area,
            });
        }
        for (i, &area) in self.airway.or_areas.iter().enumerate() {
            out.sections.push(TubeSection {
                s0: ph_len + or_len * i as f64 / n_or as f64,
                s1: ph_len + or_len * (i + 1) as f64 / n_or as f64,
                area,
            });
        }
        out.length = f.length;
        out.pharynx_length = f.pharynx_length;
        out.teeth_s = f.teeth_s;
        out.velum_port = f.velum_port;
    }
}

/// Where the vertex of the parabola through three evenly spaced values lies, in steps from the
/// middle one (0 when they are collinear).
fn parabola_offset(a: f64, b: f64, c: f64) -> f64 {
    let den = a - 2.0 * b + c;
    if den.abs() > 1e-12 {
        (0.5 * (a - c)) / den
    } else {
        0.0
    }
}
