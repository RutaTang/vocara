//! Inversion: measured formants → the articulation that would make them, frame by frame.
//!
//! [`Inverter`] takes one [`FormantFrame`] per analysis frame (F0, F1–F3 in Hz, how sure the
//! analysis is that the frame is voiced) and keeps a [`TractEstimate`]: the posture of the
//! seven fitted articulators ([`crate::posture`]), the whole articulation, the formants the
//! model gives it and how well they match the measured ones.
//!
//! # The fit
//!
//! For a voiced frame it minimises, over the postures in the search box,
//!
//! ```text
//! J(p) = Σᵢ wᵢ·(ln Fᵢ(p) − ln F̂ᵢ)²  +  S(p − p̄)  +  λ·d²(p)  +  λᵥ·dᵥ²(p)
//! ```
//!
//! - the data: each measured formant F̂ᵢ (i = 1..3) against the model's, in log-frequency,
//!   weighted wᵢ = c / σᵢ² by the frame's confidence c and the formant's expected error σᵢ
//!   (5 % each). Where F0 is high a formant within two harmonics of it is read off a sparse
//!   spectrum, and its σ grows (to three times at F̂ᵢ = F0);
//! - the smoothness S: a random walk from the previous estimate p̄, each articulator moving
//!   about vⱼ cm/s ([`InversionConfig::speed`]) over the time Δt since the last fitted frame,
//!   so a jittery formant track gives a smooth articulation track and after a pause the posture
//!   is free to jump. In coordinates zⱼ = (pⱼ − p̄ⱼ)/(vⱼ·Δt) it is full along the directions the
//!   weighted formants see (the rows of the Jacobian) and only ε of that
//!   ([`InversionConfig::null_space_stiffness`]) along the four they cannot, where the priors
//!   move the posture: smoothness everywhere held a glide's end in whatever compensating
//!   posture it reached. And where the previous posture misses the new formants by more than
//!   their noise (χ² per formant over 2), the singer moved faster than the walk expects: Δt
//!   stretches by that excess (its weight falls by its square, at most a hundredfold); without
//!   it a fast ɑ→i glide left the fit 6 % off, in a posture it never left;
//! - the priors ([`crate::posture`]): λ·d², the squared Mahalanobis distance from CHIMERA's sung
//!   vowels ([`InversionConfig::prior_weight`]), and λᵥ·dᵥ², the distance to the body's nearest
//!   calibrated vowel ([`InversionConfig::vowel_prior_weight`]), so the directions three
//!   formants cannot see stay plausible and near the vowels a singer sings.
//!
//! The minimisation:
//!
//! 1. **Start.** The candidates are the previous estimate and the codebook's entries nearest
//!    the measured formants ([`crate::codebook`]); each one's J is exact (its formants are
//!    known). An entry becomes the start x₀ only if it beats the previous estimate by a margin
//!    (near the estimate the refinement finds what the entry would, and a new start costs a
//!    Jacobian: without the margin, noise made entries win on a third of the frames).
//! 2. **Local model.** ln F around x₀ is linear: its Jacobian by forward differences (seven
//!    model evaluations) when x₀ is new, else the previous frame's, kept current by Broyden's
//!    update after every evaluation.
//! 3. **Refine.** Nelder–Mead ([`crate::optimize`]) minimises J with the local model, inside a
//!    trust region around x₀ (and the search box), warm-started at x₀. The model is evaluated
//!    at the result: if J really fell, the result is taken, and if the local model predicted
//!    the fall well the frame is done (the region grows); if not, the region shrinks and step 3
//!    runs again, up to [`InversionConfig::max_steps`] times a frame.
//!
//! A frame whose confidence is below [`InversionConfig::min_confidence`], or without F1 and F2,
//! is not fitted: the estimate holds and relaxes towards CHIMERA's neutral posture with time
//! constant [`InversionConfig::hold_decay_s`], its confidence fading faster.
//!
//! # What it reports
//!
//! [`TractEstimate::residual`] is the RMS relative error of the fitted formants (0.03 = 3 %),
//! and [`TractEstimate::confidence`] combines it with the frame's own: c·exp(−(residual/8 %)²),
//! fading while the estimate is held. A UI should call an estimate uncertain below about 0.5.
//!
//! # Accuracy and cost
//!
//! Round trips through the model (`tests/inversion.rs`, 26 sung-vowel postures a body, none a
//! codebook entry) give the formants back within 0.2 % on average and 0.8 % at worst; the jaw
//! within 1.5–1.7 mm on average, the tongue's height and frontness within 0.05–0.08 of their
//! 0–1 scales, the lips' rounding within 0.11–0.20 ([`crate::components`]). ±3 % formant noise
//! moves a held posture about 0.3 mm a frame.
//!
//! A frame costs at most 1 + 7 + [`InversionConfig::max_steps`] model evaluations (a new start
//! needs a Jacobian), usually one, plus the Nelder–Mead on the local model (no model
//! evaluations): 0.3–0.45 ms on average, 1.5–2.4 ms at the 99th percentile (release; the
//! ignored `timing_inversion` test). Nothing allocates per frame once the codebook is built
//! (`tests/no_alloc.rs`).

// Small fixed-size linear algebra (seven articulators, three formants) reads clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::sync::{Arc, OnceLock};

use crate::anatomy::{BodyMix, BodyName, HumanArtic};
use crate::codebook::{Codebook, MAX_NEAREST};
use crate::forward::TractModel;
use crate::math::{exp, ln, sqrt};
use crate::optimize::{nelder_mead, NelderMeadOpts};
use crate::posture::{Posture, Prior, VowelPrior, HI, LO, N_FIT};

/// The fit's settings (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InversionConfig {
    /// Expected relative error of the measured F1, F2, F3 (their σ in log-frequency).
    pub formant_sigma: [f64; 3],
    /// How fast each fitted articulator plausibly moves (template cm/s), in
    /// [`crate::posture::FIT_KEYS`] order.
    pub speed: [f64; N_FIT],
    /// λ, the Gaussian prior's weight.
    pub prior_weight: f64,
    /// λᵥ, the nearest-vowel prior's weight.
    pub vowel_prior_weight: f64,
    /// ε: the smoothness' share along the directions the formants cannot see (1: as along the
    /// ones they can).
    pub null_space_stiffness: f64,
    /// Frames less confident than this are held, not fitted.
    pub min_confidence: f64,
    /// Time constant (s) of a held estimate's relaxation to the neutral posture.
    pub hold_decay_s: f64,
    /// Time constant (s) of a held estimate's confidence.
    pub confidence_decay_s: f64,
    /// How many codebook entries are start candidates.
    pub candidates: usize,
    /// Refinement steps (model evaluations after the start) per frame, at most.
    pub max_steps: usize,
}

impl Default for InversionConfig {
    fn default() -> Self {
        InversionConfig {
            formant_sigma: [0.05, 0.05, 0.05],
            speed: [24.0, 24.0, 24.0, 16.0, 24.0, 20.0, 8.0],
            prior_weight: 0.02,
            vowel_prior_weight: 0.02,
            null_space_stiffness: 0.05,
            min_confidence: 0.3,
            hold_decay_s: 1.5,
            confidence_decay_s: 0.3,
            candidates: 8,
            max_steps: 3,
        }
    }
}

/// One analysis frame's measurements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormantFrame {
    /// When the frame is (s), increasing frame to frame.
    pub time: f64,
    /// Fundamental frequency (Hz), NaN when unvoiced.
    pub f0: f64,
    /// F1, F2, F3 (Hz); NaN where not measured.
    pub formants: [f64; 3],
    /// How sure the analysis is that the frame is voiced, 0–1 (the live analysis' YIN
    /// periodicity; 1 for a take's voiced frames, 0 for unvoiced ones).
    pub confidence: f64,
}

/// The estimate after a frame (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TractEstimate {
    /// The frame's time (s).
    pub time: f64,
    /// The fitted articulators.
    pub posture: Posture,
    /// The whole articulation (for the geometry).
    pub articulation: HumanArtic,
    /// F1–F3 (Hz) the model gives the posture; NaN until a frame was fitted, and while held.
    pub model_formants: [f64; 3],
    /// The frame's F1–F3 (Hz).
    pub measured: [f64; 3],
    /// The frame's F0 (Hz), passed through.
    pub f0: f64,
    /// RMS relative error of the fitted formants (NaN when the frame was not fitted).
    pub residual: f64,
    /// How far to trust the estimate, 0–1.
    pub confidence: f64,
    /// Whether this frame was fitted (false: held from an earlier one, or nothing yet).
    pub fitted: bool,
}

impl TractEstimate {
    /// The estimate before any frame: the neutral posture, no confidence.
    pub fn initial() -> TractEstimate {
        TractEstimate {
            time: 0.0,
            posture: Posture::NEUTRAL,
            articulation: Posture::NEUTRAL.articulation(),
            model_formants: [f64::NAN; 3],
            measured: [f64::NAN; 3],
            f0: f64::NAN,
            residual: f64::NAN,
            confidence: 0.0,
            fitted: false,
        }
    }
}

/// A body for a singer, from the F3 of their voiced frames: F3 follows the tract's length and
/// moves least from vowel to vowel, so their median is placed (in log-frequency) on the line from
/// the adult male's to the adult female's to the child's median F3 over CHIMERA's calibrated
/// sung vowels (about 2.45, 2.8 and 3.3 kHz). A default to start from, for a view that lets the
/// singer choose: the measured F3 of a high voice is less reliable, and a man with a short
/// tract reads as part female. `None` with fewer than 20 values.
pub fn suggest_body(f3: impl IntoIterator<Item = f64>) -> Option<BodyMix> {
    let mut v: Vec<f64> = f3
        .into_iter()
        .filter(|x| *x > 0.0 && x.is_finite())
        .collect();
    if v.len() < 20 {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = ln(v[v.len() / 2]);
    let reference = |b: BodyName| {
        let mut f: Vec<f64> = crate::posture::sung_vowels(b)
            .map(|v| v.formants[2])
            .filter(|x| x.is_finite())
            .collect();
        f.sort_by(f64::total_cmp);
        ln(f[f.len() / 2])
    };
    let (male, female, child) = (
        reference(BodyName::Male),
        reference(BodyName::Female),
        reference(BodyName::Child),
    );
    Some(if m <= female {
        BodyMix::new((m - male) / (female - male), 0.0)
    } else {
        BodyMix::new(1.0, (m - female) / (child - female))
    })
}

/// Forward-difference steps (template cm) of the Jacobian.
const FD_STEP: [f64; N_FIT] = [0.03, 0.04, 0.04, 0.04, 0.03, 0.03, 0.04];
/// The trust region's half-width, as a fraction of each articulator's search range.
const TRUST_INIT: f64 = 0.08;
const TRUST_MIN: f64 = 0.005;
const TRUST_MAX: f64 = 0.3;
/// Relative error at which a fit's confidence falls to 1/e.
const RESIDUAL_SCALE: f64 = 0.08;
/// A model posture without three formants costs this per missing one.
const MISSING_FORMANT: f64 = 400.0;
/// How much (in cost units: one σ² of formant error each) a codebook entry must beat the
/// previous estimate by to become the start.
const SWITCH_MARGIN: f64 = 1.0;
/// Nelder–Mead iterations on the local model, at most.
const NM_ITERATIONS: usize = 400;
/// The longest gap (s) the smoothness spans.
const MAX_DT: f64 = 1.0;
/// A previous posture's normalised misfit (χ² per formant) up to which its miss is noise: the
/// smoothness holds.
const MISFIT_NOISE: f64 = 2.0;
/// The most the smoothness relaxes (its weight divided by this).
const RELAX_MAX: f64 = 100.0;

fn shared_prior() -> &'static Prior {
    static PRIOR: OnceLock<Prior> = OnceLock::new();
    PRIOR.get_or_init(Prior::of_vowels)
}

/// The frame-by-frame inversion (see the module docs).
#[derive(Clone, Debug)]
pub struct Inverter {
    model: TractModel,
    codebook: Arc<Codebook>,
    vowels: Arc<VowelPrior>,
    config: InversionConfig,
    est: TractEstimate,
    /// a frame has been fitted
    has_fit: bool,
    /// when the last fitted frame was
    fit_time: f64,
    /// when the last frame was (fitted or not)
    last_time: f64,
    /// ln F1–F3 at `est.posture`, when known
    log_f: Option<[f64; 3]>,
    /// ∂ln Fᵢ/∂pⱼ around `est.posture`, when one is kept
    jac: Option<[[f64; N_FIT]; 3]>,
    trust: f64,
    /// model evaluations in the current frame
    evals: usize,
}

impl Inverter {
    /// An inversion for `body` with the default settings (its codebook is built if this process
    /// has not built it yet, see [`Codebook::shared`]).
    pub fn new(body: BodyMix) -> Inverter {
        Inverter::with_config(body, InversionConfig::default())
    }

    pub fn with_config(body: BodyMix, config: InversionConfig) -> Inverter {
        Inverter::with_codebook(Codebook::shared(body), config)
    }

    /// An inversion over a codebook already at hand (its body's).
    pub fn with_codebook(codebook: Arc<Codebook>, config: InversionConfig) -> Inverter {
        Inverter {
            model: TractModel::new(codebook.body()),
            vowels: Arc::new(VowelPrior::of(codebook.body())),
            codebook,
            config,
            est: TractEstimate::initial(),
            has_fit: false,
            fit_time: 0.0,
            last_time: f64::NEG_INFINITY,
            log_f: None,
            jac: None,
            trust: TRUST_INIT,
            evals: 0,
        }
    }

    pub fn body(&self) -> BodyMix {
        self.codebook.body()
    }

    pub fn config(&self) -> &InversionConfig {
        &self.config
    }

    /// The current estimate.
    pub fn estimate(&self) -> &TractEstimate {
        &self.est
    }

    /// How many forward-model evaluations the last frame took (what a frame's cost is mostly).
    pub fn last_evaluations(&self) -> usize {
        self.evals
    }

    /// The forward model the inversion fits with (for the geometry of its estimates).
    pub fn model(&mut self) -> &mut TractModel {
        &mut self.model
    }

    /// Forgets every frame: the next starts afresh.
    pub fn reset(&mut self) {
        self.est = TractEstimate::initial();
        self.has_fit = false;
        self.fit_time = 0.0;
        self.last_time = f64::NEG_INFINITY;
        self.log_f = None;
        self.jac = None;
        self.trust = TRUST_INIT;
    }

    /// Takes one frame and returns the estimate after it. A frame earlier than the last one
    /// starts afresh (a seek).
    pub fn update(&mut self, frame: &FormantFrame) -> &TractEstimate {
        if frame.time < self.last_time {
            self.reset();
        }
        self.evals = 0;
        let usable = frame.confidence >= self.config.min_confidence
            && frame.formants[0] > 0.0
            && frame.formants[1] > 0.0
            && frame.formants[0] < frame.formants[1];
        if usable {
            self.fit(frame);
        } else {
            self.hold(frame);
        }
        self.last_time = frame.time;
        &self.est
    }

    /// An unfitted frame: hold, relaxing to neutral.
    fn hold(&mut self, frame: &FormantFrame) {
        let dt = if self.last_time.is_finite() {
            (frame.time - self.last_time).clamp(0.0, MAX_DT)
        } else {
            0.0
        };
        let e = &mut self.est;
        e.time = frame.time;
        e.measured = frame.formants;
        e.f0 = frame.f0;
        e.fitted = false;
        e.residual = f64::NAN;
        if dt > 0.0 && self.has_fit {
            let keep = exp(-dt / self.config.hold_decay_s);
            let n = Posture::NEUTRAL.0;
            for i in 0..N_FIT {
                e.posture.0[i] = n[i] + (e.posture.0[i] - n[i]) * keep;
            }
            e.articulation = e.posture.articulation();
            e.confidence *= exp(-dt / self.config.confidence_decay_s);
            e.model_formants = [f64::NAN; 3];
            // the posture moved: its formants are no longer known (the Jacobian, a little
            // off, is kept for Broyden to correct)
            self.log_f = None;
        }
    }

    /// The fit's weights for a frame: wᵢ = c/σᵢ², 0 where a formant is missing.
    fn weights(&self, frame: &FormantFrame) -> ([f64; 3], [f64; 3]) {
        let mut w = [0.0; 3];
        let mut y = [0.0; 3];
        let c = frame.confidence.clamp(0.0, 1.0);
        for i in 0..3 {
            let f = frame.formants[i];
            if !(f > 0.0 && f.is_finite()) {
                continue;
            }
            let mut sigma = self.config.formant_sigma[i];
            if frame.f0 > 0.0 {
                // within two harmonics of a high F0, a formant is read off sparse harmonics
                let r = f / frame.f0;
                if r < 2.0 {
                    sigma *= 1.0 + 2.0 * (2.0 - r).min(1.0);
                }
            }
            w[i] = c / (sigma * sigma);
            y[i] = ln(f);
        }
        (w, y)
    }

    /// ln F1–F3 of a posture, or None when the model has fewer than three formants there.
    fn evaluate(&mut self, p: &Posture) -> Option<[f64; 3]> {
        self.evals += 1;
        let f = self.model.lowest_formants(&p.articulation(), 3);
        (f.count == 3).then(|| [ln(f.freq[0]), ln(f.freq[1]), ln(f.freq[2])])
    }

    fn fit(&mut self, frame: &FormantFrame) {
        let cfg = self.config;
        let (w, y) = self.weights(frame);
        // the smoothness' anchor and scale
        let anchor = self.est.posture;
        if self.has_fit && self.log_f.is_none() {
            self.log_f = self.evaluate(&anchor);
        }
        let mut dt = if self.has_fit {
            (frame.time - self.fit_time).clamp(1e-3, MAX_DT)
        } else {
            f64::INFINITY
        };
        // Where the previous posture misses the new formants by more than their noise, the
        // singer moved faster than the random walk expects: the smoothness relaxes (as if more
        // time had passed) by the square of how many times over the noise the miss is, up to
        // RELAX_MAX. Without it a fast glide left the fit behind, in a posture it never left.
        if let Some(lf) = self.log_f {
            let (mut chi2, mut n) = (0.0, 0.0);
            for i in 0..3 {
                if w[i] > 0.0 {
                    let e = lf[i] - y[i];
                    chi2 += w[i] * e * e;
                    n += 1.0;
                }
            }
            let over = chi2 / (n * MISFIT_NOISE).max(1e-9);
            dt *= (over * over).clamp(1.0, RELAX_MAX).sqrt();
        }
        let mut smooth = Smoothness::new(&cfg, anchor, dt, self.jac.as_ref(), &w);
        let prior = shared_prior();
        let (lambda, lambda_v) = (cfg.prior_weight, cfg.vowel_prior_weight);
        let vowels = Arc::clone(&self.vowels);
        let cost = |p: &[f64], lf: Option<&[f64; 3]>, smooth: &Smoothness| -> f64 {
            let mut c = 0.0;
            match lf {
                Some(lf) => {
                    for i in 0..3 {
                        let e = lf[i] - y[i];
                        c += w[i] * e * e;
                    }
                }
                None => c += 3.0 * MISSING_FORMANT,
            }
            c + smooth.cost(p) + lambda * prior.distance2(p) + lambda_v * vowels.distance2(p)
        };

        // 1. the start: the previous estimate or the nearest codebook entries
        let mut x0 = anchor;
        let mut f0: Option<[f64; 3]> = None;
        let mut j0 = f64::INFINITY;
        let mut from_previous = false;
        if let (true, Some(lf)) = (self.has_fit, self.log_f) {
            j0 = cost(&anchor.0, Some(&lf), &smooth);
            f0 = Some(lf);
            from_previous = true;
        }
        let mut near = [0usize; MAX_NEAREST];
        let k = cfg.candidates.clamp(1, MAX_NEAREST);
        let n = self.codebook.nearest(&y, &w, &mut near[..k]);
        // an entry must beat the previous estimate clearly: near it, the refinement finds what
        // the entry would, and a new start costs a Jacobian
        let mut best = j0 - SWITCH_MARGIN;
        for &i in &near[..n] {
            let (p, lf) = self.codebook.entry(i);
            let j = cost(&p.0, Some(&lf), &smooth);
            if j < best {
                (x0, f0, j0, from_previous) = (p, Some(lf), j, false);
                best = j;
            }
        }
        let Some(mut fx0) = f0 else {
            // nothing to start from (no codebook entry): hold
            return self.hold(frame);
        };
        // 2. the local model
        let mut jac = match (from_previous, self.jac) {
            (true, Some(jac)) => jac,
            _ => {
                self.trust = TRUST_INIT;
                let jac = self.jacobian(&x0, &fx0);
                // the smoothness' observable directions are this Jacobian's
                smooth = Smoothness::new(&cfg, anchor, dt, Some(&jac), &w);
                j0 = cost(&x0.0, Some(&fx0), &smooth);
                jac
            }
        };
        // 3. refine
        let mut steps = 0;
        while steps < cfg.max_steps {
            let (lo, hi) = self.region(&x0);
            let model = |p: &[f64]| -> [f64; 3] {
                let mut lf = fx0;
                for (i, row) in jac.iter().enumerate() {
                    for j in 0..N_FIT {
                        lf[i] += row[j] * (p[j] - x0.0[j]);
                    }
                }
                lf
            };
            let mut step = [0.0; N_FIT];
            for j in 0..N_FIT {
                step[j] = 0.5 * (hi[j] - lo[j]).max(1e-6);
            }
            let r = nelder_mead(
                |p| cost(p, Some(&model(p)), &smooth),
                &x0.0,
                NelderMeadOpts {
                    step: Some(&step),
                    lo: Some(&lo),
                    hi: Some(&hi),
                    max_iter: Some(NM_ITERATIONS),
                    tol: Some(1e-10),
                },
            );
            let mut x1 = Posture([0.0; N_FIT]);
            x1.0.copy_from_slice(r.x());
            let predicted = r.fx;
            let moved = (0..N_FIT).any(|j| (x1.0[j] - x0.0[j]).abs() > 1e-5);
            if !moved || j0 - predicted < 1e-6 * j0.max(1e-9) {
                break; // converged: the local model sees nothing better
            }
            steps += 1;
            match self.evaluate(&x1) {
                Some(fx1) => {
                    broyden(&mut jac, &x0, &x1, &fx0, &fx1);
                    let j1 = cost(&x1.0, Some(&fx1), &smooth);
                    if j1 < j0 {
                        let rho = (j0 - j1) / (j0 - predicted).max(1e-12);
                        (x0, fx0, j0) = (x1, fx1, j1);
                        if rho > 0.75 {
                            // the local model predicted well: the step is the frame's answer
                            self.trust = (self.trust * 2.0).min(TRUST_MAX);
                            break;
                        } else if rho < 0.25 {
                            self.trust = (self.trust * 0.5).max(TRUST_MIN);
                        }
                    } else {
                        self.trust = (self.trust * 0.5).max(TRUST_MIN);
                    }
                }
                None => self.trust = (self.trust * 0.5).max(TRUST_MIN),
            }
        }
        self.jac = Some(jac);
        self.log_f = Some(fx0);
        self.has_fit = true;
        self.fit_time = frame.time;
        // the estimate
        let mut sum = 0.0;
        let mut used = 0;
        let mut model_f = [0.0; 3];
        for i in 0..3 {
            model_f[i] = exp(fx0[i]);
            if w[i] > 0.0 {
                let e = exp(fx0[i] - y[i]) - 1.0;
                sum += e * e;
                used += 1;
            }
        }
        let residual = sqrt(sum / used.max(1) as f64);
        let c = frame.confidence.clamp(0.0, 1.0);
        self.est = TractEstimate {
            time: frame.time,
            posture: x0,
            articulation: x0.articulation(),
            model_formants: model_f,
            measured: frame.formants,
            f0: frame.f0,
            residual,
            confidence: c * exp(-(residual / RESIDUAL_SCALE) * (residual / RESIDUAL_SCALE)),
            fitted: true,
        };
    }

    /// The trust region around `x` (inside the search box).
    fn region(&self, x: &Posture) -> ([f64; N_FIT], [f64; N_FIT]) {
        let mut lo = [0.0; N_FIT];
        let mut hi = [0.0; N_FIT];
        for j in 0..N_FIT {
            let r = self.trust * (HI[j] - LO[j]);
            lo[j] = (x.0[j] - r).max(LO[j]);
            hi[j] = (x.0[j] + r).min(HI[j]);
        }
        (lo, hi)
    }

    /// ∂ln Fᵢ/∂pⱼ at `x` (whose ln F is `fx`) by forward differences (backward at the box's
    /// top, or where the forward step loses a formant; a zero column where both do).
    fn jacobian(&mut self, x: &Posture, fx: &[f64; 3]) -> [[f64; N_FIT]; 3] {
        let mut jac = [[0.0; N_FIT]; 3];
        for j in 0..N_FIT {
            let h = FD_STEP[j];
            let mut tries = [h, -h];
            if x.0[j] + h > HI[j] {
                tries.swap(0, 1);
            }
            for d in tries {
                let mut p = *x;
                p.0[j] += d;
                if let Some(fp) = self.evaluate(&p) {
                    for i in 0..3 {
                        jac[i][j] = (fp[i] - fx[i]) / d;
                    }
                    break;
                }
            }
        }
        jac
    }
}

/// The smoothness term (see the module docs): in coordinates scaled by each articulator's
/// reach over Δt, z = (p − p̄)/(v·Δt), it is ε·|z|² + (1 − ε)·|Q·z|², Q an orthonormal basis of
/// the directions the weighted formants see (the rows of W^½·G, G the Jacobian, in those
/// coordinates). Full along what the formants measure, so their noise is filtered; weak (ε)
/// along what they cannot see, where the prior moves the posture.
struct Smoothness {
    anchor: Posture,
    /// 1/(vⱼ·Δt): the scaled coordinates' factors (0: no smoothness)
    inv_reach: [f64; N_FIT],
    q: [[f64; N_FIT]; 3],
    rank: usize,
    eps: f64,
}

impl Smoothness {
    fn new(
        cfg: &InversionConfig,
        anchor: Posture,
        dt: f64,
        jac: Option<&[[f64; N_FIT]; 3]>,
        w: &[f64; 3],
    ) -> Smoothness {
        let mut s = Smoothness {
            anchor,
            inv_reach: [0.0; N_FIT],
            q: [[0.0; N_FIT]; 3],
            rank: 0,
            eps: 1.0,
        };
        if !dt.is_finite() {
            return s;
        }
        for j in 0..N_FIT {
            s.inv_reach[j] = 1.0 / (cfg.speed[j] * dt);
        }
        let Some(jac) = jac else {
            return s; // no Jacobian: smooth in every direction
        };
        // Gram–Schmidt on the rows of W^½·G·D, D = diag(v·Δt)
        for (i, row) in jac.iter().enumerate() {
            let mut r = [0.0; N_FIT];
            for j in 0..N_FIT {
                r[j] = w[i].sqrt() * row[j] / s.inv_reach[j];
            }
            for k in 0..s.rank {
                let dot: f64 = (0..N_FIT).map(|j| r[j] * s.q[k][j]).sum();
                for j in 0..N_FIT {
                    r[j] -= dot * s.q[k][j];
                }
            }
            let norm = r.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm > 1e-9 {
                for j in 0..N_FIT {
                    s.q[s.rank][j] = r[j] / norm;
                }
                s.rank += 1;
            }
        }
        s.eps = cfg.null_space_stiffness;
        s
    }

    fn cost(&self, p: &[f64]) -> f64 {
        let mut z = [0.0; N_FIT];
        let mut all = 0.0;
        for j in 0..N_FIT {
            z[j] = (p[j] - self.anchor.0[j]) * self.inv_reach[j];
            all += z[j] * z[j];
        }
        let mut seen = 0.0;
        for q in &self.q[..self.rank] {
            let dot: f64 = (0..N_FIT).map(|j| q[j] * z[j]).sum();
            seen += dot * dot;
        }
        self.eps * all + (1.0 - self.eps) * seen
    }
}

/// Broyden's update of the Jacobian after a step x0 → x1 that took ln F from f0 to f1.
fn broyden(jac: &mut [[f64; N_FIT]; 3], x0: &Posture, x1: &Posture, f0: &[f64; 3], f1: &[f64; 3]) {
    let mut dx = [0.0; N_FIT];
    let mut dd = 0.0;
    for j in 0..N_FIT {
        dx[j] = x1.0[j] - x0.0[j];
        dd += dx[j] * dx[j];
    }
    if dd < 1e-12 {
        return;
    }
    for i in 0..3 {
        let mut pred = 0.0;
        for j in 0..N_FIT {
            pred += jac[i][j] * dx[j];
        }
        let r = (f1[i] - f0[i]) - pred;
        for j in 0..N_FIT {
            jac[i][j] += r * dx[j] / dd;
        }
    }
}
