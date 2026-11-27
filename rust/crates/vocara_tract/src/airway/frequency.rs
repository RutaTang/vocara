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
        let (m, p) = solver.eval(freq);
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
///
/// `prepare` copies each reached node's elements (and those of its branch to its parent) into
/// arrays in the tree's breadth-first order, so a solve walks them in sequence. The arithmetic
/// is CHIMERA's, operation for operation, on the same values: only where they are read from
/// differs.
#[derive(Clone, Debug, Default)]
pub struct TransferSolver {
    adj_start: Vec<usize>,
    adj: Vec<usize>,
    fill: Vec<usize>,
    /// BFS order from the injection node (node indices)
    order: Vec<usize>,
    parent: Vec<isize>,
    parent_branch: Vec<usize>,
    /// a node's position in `order`
    pos: Vec<usize>,
    // the reached nodes' elements, by position in `order`
    /// the parent's position (0 at the root, unused)
    up: Vec<usize>,
    c: Vec<f64>,
    wall_area: Vec<f64>,
    wall_m: Vec<f64>,
    wall_r: Vec<f64>,
    wall_k: Vec<f64>,
    wall_rad: Vec<f64>,
    fixed: Vec<bool>,
    /// the radiator's area, NaN where there is none
    rad_area: Vec<f64>,
    /// R and L of the branch to the parent (0 at the root, unused)
    br: Vec<f64>,
    bl: Vec<f64>,
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
    /// Builds the elimination tree for `el` from `inject`, without the branches in `cut`, and
    /// takes the reached elements' values.
    pub fn prepare(&mut self, el: &LinearElements, inject: usize, cut: &[usize]) {
        let n = el.n_nodes;
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
        // the reached nodes' elements, in order
        self.pos.clear();
        self.pos.resize(n, usize::MAX);
        for (q, &k) in self.order.iter().enumerate() {
            self.pos[k] = q;
        }
        self.up.clear();
        self.br.clear();
        self.bl.clear();
        self.c.clear();
        self.wall_area.clear();
        self.wall_m.clear();
        self.wall_r.clear();
        self.wall_k.clear();
        self.wall_rad.clear();
        self.fixed.clear();
        self.rad_area.clear();
        for (q, &k) in self.order.iter().enumerate() {
            let (up, r, l) = if q == 0 {
                (0, 0.0, 0.0)
            } else {
                let j = self.parent_branch[k];
                (self.pos[self.parent[k] as usize], el.r[j], el.l[j])
            };
            self.up.push(up);
            self.br.push(r);
            self.bl.push(l);
            self.c.push(el.c[k]);
            self.wall_area.push(el.wall_area[k]);
            self.wall_m.push(el.wall_m[k]);
            self.wall_r.push(el.wall_r[k]);
            self.wall_k.push(el.wall_k[k]);
            self.wall_rad.push(el.wall_rad[k]);
            self.fixed.push(el.fixed[k]);
            self.rad_area.push(f64::NAN);
        }
        for (r, &node) in el.rad_nodes.iter().enumerate() {
            let q = self.pos[node];
            if q != usize::MAX {
                self.rad_area[q] = el.rad_area[r];
            }
        }
        let m = self.order.len();
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
            v.resize(m, 0.0);
        }
    }

    /// |U_rad / U_in| and the radiated pressure (dB) at `freq`, for the elements the solver was
    /// prepared with.
    pub fn eval(&mut self, freq: f64) -> (f64, f64) {
        let w = 2.0 * PI * js_max(freq, 1e-3);
        let mag = self.eval_mag(freq);
        (mag, 20.0 * log10(mag * w + 1e-30))
    }

    /// |U_rad / U_in| at `freq` (see [`TransferSolver::eval`]).
    pub fn eval_mag(&mut self, freq: f64) -> f64 {
        let m = self.order.len();
        let w = 2.0 * PI * js_max(freq, 1e-3);
        let (yr, yi) = (&mut self.yr[..m], &mut self.yi[..m]);
        let (er, ei) = (&mut self.er[..m], &mut self.ei[..m]);
        let (pr, pi) = (&mut self.pr[..m], &mut self.pi[..m]);
        let (rad_yr, rad_yi) = (&mut self.rad_yr[..m], &mut self.rad_yi[..m]);
        let (w_yr, w_yi) = (&mut self.w_yr[..m], &mut self.w_yi[..m]);
        let (c, wall_area) = (&self.c[..m], &self.wall_area[..m]);
        let (wall_m, wall_r, wall_k) = (&self.wall_m[..m], &self.wall_r[..m], &self.wall_k[..m]);
        let (wall_rad, fixed) = (&self.wall_rad[..m], &self.fixed[..m]);
        let rad_area = &self.rad_area[..m];
        let (up, br, bl) = (&self.up[..m], &self.br[..m], &self.bl[..m]);
        // shunt admittances
        for q in 0..m {
            let mut gr = 0.0;
            let mut gi = w * c[q];
            w_yr[q] = 0.0;
            w_yi[q] = 0.0;
            if wall_area[q] > 0.0 {
                // Y = S / (r + j(ωm − k/ω))
                let zr = wall_r[q];
                let zi = w * wall_m[q] - wall_k[q] / w;
                let d = zr * zr + zi * zi;
                w_yr[q] = (wall_area[q] * zr) / d;
                w_yi[q] = (-wall_area[q] * zi) / d;
                gr += w_yr[q];
                gi += w_yi[q];
            }
            rad_yr[q] = 0.0;
            rad_yi[q] = 0.0;
            let a = rad_area[q];
            if !a.is_nan() {
                let lr = (8.0 * RHO) / (3.0 * PI * sqrt(PI * a));
                let rr = (128.0 * RHO * C_SOUND) / (9.0 * PI * PI * a);
                rad_yr[q] = 1.0 / rr;
                rad_yi[q] = -1.0 / (w * lr);
                gr += rad_yr[q];
                gi += rad_yi[q];
            }
            if fixed[q] {
                gr += 1e30;
            }
            yr[q] = gr;
            yi[q] = gi;
        }
        // eliminate leaves → root
        for q in (1..m).rev() {
            // Y_eq = Y / (1 + Z·Y), Z = R + jωL
            let zr = br[q];
            let zi = w * bl[q];
            let dr = 1.0 + zr * yr[q] - zi * yi[q];
            let di = zr * yi[q] + zi * yr[q];
            let dd = dr * dr + di * di;
            er[q] = (yr[q] * dr + yi[q] * di) / dd;
            ei[q] = (yi[q] * dr - yr[q] * di) / dd;
            let p = up[q];
            yr[p] += er[q];
            yi[p] += ei[q];
        }
        // root pressure for unit injected flow
        {
            let dd = yr[0] * yr[0] + yi[0] * yi[0];
            pr[0] = yr[0] / dd;
            pi[0] = -yi[0] / dd;
        }
        // back-substitute root → leaves: I = p_m·Y_eq(k); p_k = p_m − Z·I
        let mut ur = 0.0;
        let mut ui = 0.0;
        for q in 0..m {
            if q > 0 {
                let p = up[q];
                let ir = pr[p] * er[q] - pi[p] * ei[q];
                let ii = pr[p] * ei[q] + pi[p] * er[q];
                let zr = br[q];
                let zi = w * bl[q];
                pr[q] = pr[p] - (zr * ir - zi * ii);
                pi[q] = pi[p] - (zr * ii + zi * ir);
            }
            if !rad_area[q].is_nan() {
                ur += pr[q] * rad_yr[q] - pi[q] * rad_yi[q];
                ui += pr[q] * rad_yi[q] + pi[q] * rad_yr[q];
            }
            if wall_rad[q] > 0.0 && wall_area[q] > 0.0 {
                ur += wall_rad[q] * (pr[q] * w_yr[q] - pi[q] * w_yi[q]);
                ui += wall_rad[q] * (pr[q] * w_yi[q] + pi[q] * w_yr[q]);
            }
        }
        hypot(ur, ui)
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
