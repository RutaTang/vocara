//! Frequency-domain analysis of the linearised airway network (port of CHIMERA's
//! `airway/frequency.rs`): the network's lumped elements solved per frequency by tree
//! elimination (O(N) per frequency), and the resonance peaks of the result.
//!
//! Complex arithmetic is written out on separate real and imaginary parts exactly as CHIMERA
//! has it, so every operation (and its rounding) is the same. [`TransferSolver`] is the same
//! solver with its buffers kept between calls: [`transfer_function`] runs through it, and the
//! formant search ([`crate::forward`]) evaluates it at the frequencies it needs without
//! allocating.

use crate::airway::network::LinearElements;
use crate::math::{hypot, js_max, log10, sqrt, PI};
use crate::physics::{C_SOUND, RHO};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransferResult {
    pub freqs: Vec<f64>,
    /// |U_rad / U_in|
    pub mag: Vec<f64>,
    /// radiated pressure ∝ |jω·U_rad/U_in| (dB, arbitrary reference)
    pub pressure_db: Vec<f64>,
}

/// Transfer function from a volume-velocity source injected at `inject` to total radiated
/// volume velocity. Branches in `cut` are removed (e.g. the glottis: an ideal flow source has
/// infinite internal impedance, which decouples the subglottal system).
pub fn transfer_function(
    el: &LinearElements,
    inject: usize,
    freqs: &[f64],
    cut: &[usize],
) -> TransferResult {
    let mut solver = TransferSolver::default();
    solver.prepare(el, inject, cut);
    let mut mag = vec![0.0; freqs.len()];
    let mut pdb = vec![0.0; freqs.len()];
    for (f, &freq) in freqs.iter().enumerate() {
        let (m, p) = solver.eval(el, freq);
        mag[f] = m;
        pdb[f] = p;
    }
    TransferResult {
        freqs: freqs.to_vec(),
        mag,
        pressure_db: pdb,
    }
}

const UNVISITED: isize = -2;

/// The transfer function's solver for one network state: the elimination tree from the
/// injection node over the open branches ([`TransferSolver::prepare`]), then one solve per
/// frequency ([`TransferSolver::eval`]). Its buffers are kept, so a solver used again for the
/// same network allocates nothing.
#[derive(Clone, Debug, Default)]
pub struct TransferSolver {
    inject: usize,
    adj_start: Vec<usize>,
    adj: Vec<usize>,
    fill: Vec<usize>,
    /// BFS order from the injection node
    order: Vec<usize>,
    parent: Vec<isize>,
    parent_branch: Vec<usize>,
    rad_of: Vec<Option<usize>>,
    // per frequency
    yr: Vec<f64>,
    yi: Vec<f64>,
    er: Vec<f64>,
    ei: Vec<f64>,
    pr: Vec<f64>,
    pi: Vec<f64>,
    rad_yr: Vec<f64>,
    rad_yi: Vec<f64>,
    w_yr: Vec<f64>,
    w_yi: Vec<f64>,
}

impl TransferSolver {
    /// Builds the elimination tree for `el` from `inject`, without the branches in `cut`.
    pub fn prepare(&mut self, el: &LinearElements, inject: usize, cut: &[usize]) {
        let n = el.n_nodes;
        self.inject = inject;
        // adjacency of open branches
        let adj_start = &mut self.adj_start;
        adj_start.clear();
        adj_start.resize(n + 1, 0);
        let is_cut = |j: usize| cut.contains(&j);
        for j in 0..el.n_branches {
            if !el.open[j] || is_cut(j) {
                continue;
            }
            adj_start[el.a[j] + 1] += 1;
            adj_start[el.b[j] + 1] += 1;
        }
        for k in 0..n {
            adj_start[k + 1] += adj_start[k];
        }
        self.adj.clear();
        self.adj.resize(adj_start[n], 0);
        self.fill.clear();
        self.fill.extend_from_slice(&adj_start[..n]);
        for j in 0..el.n_branches {
            if !el.open[j] || is_cut(j) {
                continue;
            }
            self.adj[self.fill[el.a[j]]] = j;
            self.fill[el.a[j]] += 1;
            self.adj[self.fill[el.b[j]]] = j;
            self.fill[el.b[j]] += 1;
        }
        // BFS tree from the injection node
        self.order.clear();
        self.parent.clear();
        self.parent.resize(n, UNVISITED);
        self.parent_branch.clear();
        self.parent_branch.resize(n, usize::MAX);
        self.parent[inject] = -1;
        self.order.push(inject);
        let mut q = 0;
        while q < self.order.len() {
            let m = self.order[q];
            for e in self.adj_start[m]..self.adj_start[m + 1] {
                let j = self.adj[e];
                let k = if el.a[j] == m { el.b[j] } else { el.a[j] };
                if self.parent[k] != UNVISITED {
                    continue; // visited (a loop would be ignored here)
                }
                self.parent[k] = m as isize;
                self.parent_branch[k] = j;
                self.order.push(k);
            }
            q += 1;
        }
        self.rad_of.clear();
        self.rad_of.resize(n, None);
        for (r, &node) in el.rad_nodes.iter().enumerate() {
            self.rad_of[node] = Some(r);
        }
        for v in [
            &mut self.yr,
            &mut self.yi,
            &mut self.er,
            &mut self.ei,
            &mut self.pr,
            &mut self.pi,
            &mut self.rad_yr,
            &mut self.rad_yi,
            &mut self.w_yr,
            &mut self.w_yi,
        ] {
            v.clear();
            v.resize(n, 0.0);
        }
    }

    /// |U_rad / U_in| and the radiated pressure (dB) at `freq`, for the elements the tree was
    /// prepared for.
    pub fn eval(&mut self, el: &LinearElements, freq: f64) -> (f64, f64) {
        let inject = self.inject;
        let w = 2.0 * PI * js_max(freq, 1e-3);
        let (yr, yi, er, ei) = (&mut self.yr, &mut self.yi, &mut self.er, &mut self.ei);
        let (pr, pi) = (&mut self.pr, &mut self.pi);
        let (rad_yr, rad_yi, w_yr, w_yi) = (
            &mut self.rad_yr,
            &mut self.rad_yi,
            &mut self.w_yr,
            &mut self.w_yi,
        );
        // shunt admittances
        for &k in &self.order {
            let mut gr = 0.0;
            let mut gi = w * el.c[k];
            w_yr[k] = 0.0;
            w_yi[k] = 0.0;
            if el.wall_area[k] > 0.0 {
                // Y = S / (r + j(ωm − k/ω))
                let zr = el.wall_r[k];
                let zi = w * el.wall_m[k] - el.wall_k[k] / w;
                let d = zr * zr + zi * zi;
                w_yr[k] = (el.wall_area[k] * zr) / d;
                w_yi[k] = (-el.wall_area[k] * zi) / d;
                gr += w_yr[k];
                gi += w_yi[k];
            }
            rad_yr[k] = 0.0;
            rad_yi[k] = 0.0;
            if let Some(r) = self.rad_of[k] {
                let a = el.rad_area[r];
                let lr = (8.0 * RHO) / (3.0 * PI * sqrt(PI * a));
                let rr = (128.0 * RHO * C_SOUND) / (9.0 * PI * PI * a);
                rad_yr[k] = 1.0 / rr;
                rad_yi[k] = -1.0 / (w * lr);
                gr += rad_yr[k];
                gi += rad_yi[k];
            }
            if el.fixed[k] {
                gr += 1e30;
            }
            yr[k] = gr;
            yi[k] = gi;
        }
        // eliminate leaves → root
        for q in (1..self.order.len()).rev() {
            let k = self.order[q];
            let j = self.parent_branch[k];
            // Y_eq = Y / (1 + Z·Y), Z = R + jωL
            let zr = el.r[j];
            let zi = w * el.l[j];
            let dr = 1.0 + zr * yr[k] - zi * yi[k];
            let di = zr * yi[k] + zi * yr[k];
            let dd = dr * dr + di * di;
            er[k] = (yr[k] * dr + yi[k] * di) / dd;
            ei[k] = (yi[k] * dr - yr[k] * di) / dd;
            let m = self.parent[k] as usize;
            yr[m] += er[k];
            yi[m] += ei[k];
        }
        // root pressure for unit injected flow
        {
            let dd = yr[inject] * yr[inject] + yi[inject] * yi[inject];
            pr[inject] = yr[inject] / dd;
            pi[inject] = -yi[inject] / dd;
        }
        // back-substitute root → leaves: I = p_m·Y_eq(k); p_k = p_m − Z·I
        let mut ur = 0.0;
        let mut ui = 0.0;
        for (q, &k) in self.order.iter().enumerate() {
            if q > 0 {
                let m = self.parent[k] as usize;
                let j = self.parent_branch[k];
                let ir = pr[m] * er[k] - pi[m] * ei[k];
                let ii = pr[m] * ei[k] + pi[m] * er[k];
                let zr = el.r[j];
                let zi = w * el.l[j];
                pr[k] = pr[m] - (zr * ir - zi * ii);
                pi[k] = pi[m] - (zr * ii + zi * ir);
            }
            if self.rad_of[k].is_some() {
                ur += pr[k] * rad_yr[k] - pi[k] * rad_yi[k];
                ui += pr[k] * rad_yi[k] + pi[k] * rad_yr[k];
            }
            if el.wall_rad[k] > 0.0 && el.wall_area[k] > 0.0 {
                ur += el.wall_rad[k] * (pr[k] * w_yr[k] - pi[k] * w_yi[k]);
                ui += el.wall_rad[k] * (pr[k] * w_yi[k] + pi[k] * w_yr[k]);
            }
        }
        let mag = hypot(ur, ui);
        (mag, 20.0 * log10(mag * w + 1e-30))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub freq: f64,
    /// -3 dB bandwidth (Hz), NaN if not measurable
    pub bw: f64,
    pub db: f64,
}

/// Local maxima of |H| (dB) with parabolic refinement and −3 dB bandwidths, above `min_freq`
/// (CHIMERA's UI reads formants above 150 Hz).
pub fn find_peaks(freqs: &[f64], mag: &[f64], min_freq: f64) -> Vec<Peak> {
    let n = mag.len();
    let db: Vec<f64> = mag.iter().map(|m| 20.0 * log10(m + 1e-30)).collect();
    let mut out = Vec::new();
    for i in 1..n.saturating_sub(1) {
        if freqs[i] < min_freq {
            continue;
        }
        if db[i] > db[i - 1] && db[i] >= db[i + 1] {
            let (a, b, c) = (db[i - 1], db[i], db[i + 1]);
            let den = a - 2.0 * b + c;
            let off = if den.abs() > 1e-12 {
                (0.5 * (a - c)) / den
            } else {
                0.0
            };
            let df = freqs[i + 1] - freqs[i];
            let f = freqs[i] + off * df;
            let peak_db = b - 0.25 * (a - c) * off;
            // -3 dB points
            let (mut lo, mut hi) = (i, i);
            while lo > 0 && db[lo] > peak_db - 3.0 {
                lo -= 1;
            }
            while hi < n - 1 && db[hi] > peak_db - 3.0 {
                hi += 1;
            }
            let mut bw = f64::NAN;
            if db[lo] <= peak_db - 3.0 && db[hi] <= peak_db - 3.0 {
                let fl = freqs[lo]
                    + ((peak_db - 3.0 - db[lo]) / (db[lo + 1] - db[lo]))
                        * (freqs[lo + 1] - freqs[lo]);
                let fh = freqs[hi - 1]
                    + ((db[hi - 1] - (peak_db - 3.0)) / (db[hi - 1] - db[hi]))
                        * (freqs[hi] - freqs[hi - 1]);
                bw = fh - fl;
            }
            out.push(Peak {
                freq: f,
                bw,
                db: peak_db,
            });
        }
    }
    out
}

/// `n` evenly spaced values from `a` to `b`, both included.
pub fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| a + ((b - a) * i as f64) / (n as f64 - 1.0))
        .collect()
}
