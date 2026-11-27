//! The branched airway's lumped elements (port of the linearised part of CHIMERA's
//! `airway/network.rs`).
//!
//! CHIMERA simulates the airway as a staggered-grid FDTD transmission-line network: nodes carry
//! pressure and a volume A·Δ, branches carry volume velocity between two nodes, tubes are chains
//! of sections, ports are ducts with their own area (glottis, velum), cavities are one-node
//! tubes, and radiation loads sit at the open ends. Vocara never synthesizes sound, so this port
//! keeps only what the frequency-domain analysis needs: the network's topology, the targets the
//! geometry sets, and [`AirwayNetwork::linear_elements`], the linearised element values at the
//! target geometry (the same formulas as CHIMERA's, in the same order). The time stepping, the
//! noise sources and the slew limiting are left out.

// CHIMERA's loops as written (see `anatomy`).
#![allow(clippy::needless_range_loop)]

use crate::math::{cbrt, js_max, js_min, sqrt, PI};
use crate::physics::{WallParams, MU, RHO, RHO_C2};

/// Initial cross-sectional area(s) of a tube.
#[derive(Clone, Debug)]
pub enum Area {
    Uniform(f64),
    Profile(Vec<f64>),
}

#[derive(Clone, Debug)]
pub struct TubeSpec {
    pub name: &'static str,
    /// number of sections (pressure nodes)
    pub n: usize,
    /// total length (cm); may change with the geometry
    pub length: f64,
    pub area: Area,
    /// yielding wall parameters, or None for rigid
    pub wall: Option<WallParams>,
    /// cross-section perimeter relative to a circle of equal area (≥1)
    pub perimeter_factor: f64,
    /// multiplier on viscous resistance
    pub viscous_factor: f64,
    /// fraction of wall volume velocity radiated as sound (skin radiation)
    pub wall_radiation: f64,
}

impl TubeSpec {
    pub fn new(name: &'static str, n: usize, length: f64, area: Area) -> Self {
        TubeSpec {
            name,
            n,
            length,
            area,
            wall: None,
            perimeter_factor: 1.0,
            viscous_factor: 1.0,
            wall_radiation: 0.0,
        }
    }
    pub fn wall(mut self, w: Option<WallParams>) -> Self {
        self.wall = w;
        self
    }
    pub fn perimeter(mut self, f: f64) -> Self {
        self.perimeter_factor = f;
        self
    }
    pub fn viscous(mut self, f: f64) -> Self {
        self.viscous_factor = f;
        self
    }
    pub fn radiating(mut self, f: f64) -> Self {
        self.wall_radiation = f;
        self
    }
}

/// A tube's place in the network (its nodes are `first..first + n`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tube {
    pub index: usize,
    pub n: usize,
    pub first: usize,
    /// index of the internal branch between node 0 and 1
    pub branch_base: usize,
}

impl Tube {
    #[inline]
    pub fn node(&self, i: usize) -> usize {
        self.first + i
    }
    #[inline]
    pub fn first_node(&self) -> usize {
        self.first
    }
    #[inline]
    pub fn last_node(&self) -> usize {
        self.first + self.n - 1
    }
    #[inline]
    pub fn branch(&self, i: usize) -> usize {
        self.branch_base + i
    }
}

const KIND_SERIES: u8 = 0; // inertance from adjacent half-sections
const KIND_PORT: u8 = 1; // own duct (area, length) in series with adjacent half-sections

#[derive(Clone, Copy, Debug)]
pub struct PortSpec {
    /// duct area (cm²)
    pub area: f64,
    /// duct length (cm)
    pub length: f64,
    /// kinetic loss coefficient for flow entering the duct (contraction); the linear analysis
    /// leaves kinetic losses out, as CHIMERA's does
    pub k_entrance: f64,
    /// kinetic loss coefficient for flow leaving the duct (jet expansion)
    pub k_exit: f64,
    /// slit-viscous width factor: if >0, viscous R = 12μ·len·w²/A³ (glottis-like slit of length w)
    pub slit_width: f64,
    /// include the adjacent half-section inertances
    pub include_half_sections: bool,
    /// additional constant series resistance (CGS acoustic ohms)
    pub extra_resistance: f64,
}

impl PortSpec {
    pub fn new(area: f64, length: f64) -> Self {
        PortSpec {
            area,
            length,
            k_entrance: 0.4,
            k_exit: 1.0,
            slit_width: 0.0,
            include_half_sections: true,
            extra_resistance: 0.0,
        }
    }
    pub fn kinetic(mut self, k_entrance: f64, k_exit: f64) -> Self {
        self.k_entrance = k_entrance;
        self.k_exit = k_exit;
        self
    }
    pub fn slit(mut self, w: f64) -> Self {
        self.slit_width = w;
        self
    }
    pub fn no_half_sections(mut self) -> Self {
        self.include_half_sections = false;
        self
    }
    pub fn resistance(mut self, r: f64) -> Self {
        self.extra_resistance = r;
        self
    }
}

/// Linearised element values for frequency-domain analysis.
#[derive(Clone, Debug, Default)]
pub struct LinearElements {
    pub n_nodes: usize,
    pub n_branches: usize,
    pub a: Vec<usize>,
    pub b: Vec<usize>,
    pub l: Vec<f64>,
    pub r: Vec<f64>,
    pub open: Vec<bool>,
    pub c: Vec<f64>,
    pub wall_area: Vec<f64>,
    pub wall_m: Vec<f64>,
    pub wall_r: Vec<f64>,
    pub wall_k: Vec<f64>,
    pub wall_rad: Vec<f64>,
    pub fixed: Vec<bool>,
    pub rad_nodes: Vec<usize>,
    pub rad_area: Vec<f64>,
}

/// The network: its topology and target geometry (see the module docs).
#[derive(Clone, Debug)]
pub struct AirwayNetwork {
    pub fs: f64,
    pub tubes: Vec<Tube>,
    /// current total length of each tube (cm)
    pub tube_length: Vec<f64>,
    tube_specs: Vec<TubeSpec>,
    finalized: bool,

    // --- nodes ---
    pub n_nodes: usize,
    area_target: Vec<f64>,
    dx_target: Vec<f64>,
    fixed: Vec<bool>,
    wall_m: Vec<f64>,
    wall_r: Vec<f64>,
    wall_k: Vec<f64>,
    /// perimeter factor·2√π (multiply by √A·Δ for wall area)
    wall_perim: Vec<f64>,
    wall_rad: Vec<f64>,
    has_wall: Vec<bool>,
    visc_f: Vec<f64>,

    // --- branches ---
    pub n_branches: usize,
    b_a: Vec<usize>,
    b_b: Vec<usize>,
    b_kind: Vec<u8>,
    port_specs: Vec<Option<PortSpec>>,
    port_area_target: Vec<f64>,
    port_len: Vec<f64>,
    port_slit: Vec<f64>,
    port_half: Vec<bool>,
    port_extra_r: Vec<f64>,

    // --- radiation and boundaries ---
    rad_nodes: Vec<usize>,
    pending_fixed: Vec<usize>,
    /// closure threshold (cm²): below this a branch passes no flow
    pub closed_area: f64,
    /// node area floor for volume/compliance (cm²)
    pub min_node_area: f64,
    /// global viscous loss scale
    pub viscous_scale: f64,
    /// reference angular frequency for frequency-independent viscous loss
    pub viscous_omega: f64,
}

impl AirwayNetwork {
    pub fn new(fs: f64) -> Self {
        AirwayNetwork {
            fs,
            tubes: Vec::new(),
            tube_length: Vec::new(),
            tube_specs: Vec::new(),
            finalized: false,
            n_nodes: 0,
            area_target: Vec::new(),
            dx_target: Vec::new(),
            fixed: Vec::new(),
            wall_m: Vec::new(),
            wall_r: Vec::new(),
            wall_k: Vec::new(),
            wall_perim: Vec::new(),
            wall_rad: Vec::new(),
            has_wall: Vec::new(),
            visc_f: Vec::new(),
            n_branches: 0,
            b_a: Vec::new(),
            b_b: Vec::new(),
            b_kind: Vec::new(),
            port_specs: Vec::new(),
            port_area_target: Vec::new(),
            port_len: Vec::new(),
            port_slit: Vec::new(),
            port_half: Vec::new(),
            port_extra_r: Vec::new(),
            rad_nodes: Vec::new(),
            pending_fixed: Vec::new(),
            closed_area: 2e-4,
            min_node_area: 0.02,
            viscous_scale: 1.0,
            viscous_omega: 2.0 * PI * 700.0,
        }
    }

    pub fn add_tube(&mut self, spec: TubeSpec) -> Tube {
        assert!(!self.finalized, "network finalized");
        let t = Tube {
            index: self.tubes.len(),
            n: spec.n,
            first: self.n_nodes,
            branch_base: self.b_a.len(),
        };
        self.tubes.push(t);
        self.tube_length.push(spec.length);
        self.n_nodes += spec.n;
        // internal branches
        for i in 0..spec.n.saturating_sub(1) {
            self.push_branch(t.node(i), t.node(i + 1), KIND_SERIES, None);
        }
        self.tube_specs.push(spec);
        t
    }

    /// Connect two nodes with series half-section inertance (e.g. tube end to tube end).
    pub fn connect(&mut self, a: usize, b: usize) -> usize {
        self.push_branch(a, b, KIND_SERIES, None)
    }

    /// Connect two nodes through a duct with its own area: glottis, velum port, necks.
    pub fn add_port(&mut self, a: usize, b: usize, spec: PortSpec) -> usize {
        self.push_branch(a, b, KIND_PORT, Some(spec))
    }

    /// Add a lumped cavity node (Helmholtz resonator volume) — returns node index.
    pub fn add_cavity(
        &mut self,
        name: &'static str,
        volume: f64,
        wall: Option<WallParams>,
    ) -> usize {
        // cavity = 1-node tube with area·length = volume (use a cube-ish proportion)
        let len = cbrt(volume);
        let t = self.add_tube(
            TubeSpec::new(name, 1, len, Area::Uniform(volume / len))
                .wall(wall)
                .perimeter(1.2),
        );
        t.first_node()
    }

    /// Prescribed-pressure boundary node (lungs). Returns node index.
    pub fn add_pressure_node(&mut self, name: &'static str) -> usize {
        let t = self.add_tube(TubeSpec::new(name, 1, 1.0, Area::Uniform(100.0)));
        self.pending_fixed.push(t.first_node());
        t.first_node()
    }

    /// Attach a radiation load at an open end node. Returns radiator index.
    pub fn add_radiation(&mut self, node: usize) -> usize {
        self.rad_nodes.push(node);
        self.rad_nodes.len() - 1
    }

    fn push_branch(&mut self, a: usize, b: usize, kind: u8, spec: Option<PortSpec>) -> usize {
        self.b_a.push(a);
        self.b_b.push(b);
        self.b_kind.push(kind);
        self.port_specs.push(spec);
        self.b_a.len() - 1
    }

    pub fn finalize(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;
        let n = self.n_nodes;
        let b = self.b_a.len();
        self.n_branches = b;
        let zf = |len: usize| vec![0.0f64; len];
        self.area_target = zf(n);
        self.dx_target = zf(n);
        self.fixed = vec![false; n];
        self.wall_m = zf(n);
        self.wall_r = zf(n);
        self.wall_k = zf(n);
        self.wall_perim = zf(n);
        self.wall_rad = zf(n);
        self.has_wall = vec![false; n];
        self.visc_f = zf(n);
        for ti in 0..self.tubes.len() {
            let t = self.tubes[ti];
            let spec = &self.tube_specs[ti];
            let dx = spec.length / spec.n as f64;
            for i in 0..t.n {
                let k = t.node(i);
                let a = match &spec.area {
                    Area::Uniform(a) => *a,
                    Area::Profile(v) => v[i],
                };
                self.area_target[k] = a;
                self.dx_target[k] = dx;
                self.visc_f[k] = spec.viscous_factor;
                if let Some(w) = spec.wall {
                    self.has_wall[k] = true;
                    self.wall_m[k] = w.mass;
                    self.wall_r[k] = w.resistance;
                    self.wall_k[k] = w.stiffness;
                }
                self.wall_perim[k] = spec.perimeter_factor * 2.0 * sqrt(PI);
                self.wall_rad[k] = spec.wall_radiation;
            }
        }
        for &k in &self.pending_fixed {
            self.fixed[k] = true;
        }
        self.port_area_target = zf(b);
        self.port_len = zf(b);
        self.port_slit = zf(b);
        self.port_half = vec![false; b];
        self.port_extra_r = zf(b);
        for j in 0..b {
            if let Some(s) = self.port_specs[j] {
                self.port_area_target[j] = s.area;
                self.port_len[j] = s.length;
                self.port_slit[j] = s.slit_width;
                self.port_half[j] = s.include_half_sections;
                self.port_extra_r[j] = s.extra_resistance;
            }
        }
    }

    /// Set target areas (and optionally total length) of a tube.
    pub fn set_tube_targets(&mut self, t: Tube, areas: &[f64], length: Option<f64>) {
        for i in 0..t.n {
            self.area_target[t.node(i)] = js_max(0.0, areas[i]);
        }
        if let Some(length) = length {
            self.tube_length[t.index] = length;
            let d = length / t.n as f64;
            for i in 0..t.n {
                self.dx_target[t.node(i)] = d;
            }
        }
    }

    pub fn set_port_target(&mut self, branch: usize, area: f64) {
        self.port_area_target[branch] = js_max(0.0, area);
    }

    /// Linearised element values at the target geometry, for frequency-domain analysis.
    /// Allocates; see [`AirwayNetwork::linear_elements_into`].
    pub fn linear_elements(&self) -> LinearElements {
        let mut el = LinearElements::default();
        self.linear_elements_into(&mut el);
        el
    }

    /// Linearised element values at the *target* geometry, into `el` (its buffers are reused,
    /// so this allocates nothing once `el` has held this network's elements). Same formulas as
    /// CHIMERA's time stepping; kinetic losses are omitted (they vanish for small signals about
    /// zero mean flow).
    pub fn linear_elements_into(&self, el: &mut LinearElements) {
        let n = self.n_nodes;
        let nb = self.n_branches;
        let visc = sqrt((RHO * MU * self.viscous_omega) / 2.0) * self.viscous_scale;
        let a_t = &self.area_target;
        let dx = &self.dx_target;
        let reset = |v: &mut Vec<f64>, len: usize| {
            v.clear();
            v.resize(len, 0.0);
        };
        reset(&mut el.l, nb);
        reset(&mut el.r, nb);
        el.open.clear();
        el.open.resize(nb, false);
        for j in 0..nb {
            let a = self.b_a[j];
            let b = self.b_b[j];
            let ia = 1.0 / js_max(a_t[a], 1e-6);
            let ib = 1.0 / js_max(a_t[b], 1e-6);
            if self.b_kind[j] == KIND_SERIES {
                if js_min(a_t[a], a_t[b]) < self.closed_area {
                    continue;
                }
                el.open[j] = true;
                el.l[j] = 0.5 * RHO * (dx[a] * ia + dx[b] * ib);
                el.r[j] = 0.5
                    * visc
                    * (self.wall_perim[a] * dx[a] * sqrt(a_t[a]) * ia * ia * self.visc_f[a]
                        + self.wall_perim[b] * dx[b] * sqrt(a_t[b]) * ib * ib * self.visc_f[b]);
            } else {
                let ap = self.port_area_target[j];
                if ap < self.closed_area {
                    continue;
                }
                el.open[j] = true;
                let ip = 1.0 / ap;
                let len = self.port_len[j];
                el.l[j] = RHO * len * ip
                    + if self.port_half[j] {
                        0.5 * RHO * (dx[a] * ia + dx[b] * ib)
                    } else {
                        0.0
                    };
                let w = self.port_slit[j];
                el.r[j] = (if w > 0.0 {
                    12.0 * MU * len * w * w * ip * ip * ip
                } else {
                    visc * 2.0 * sqrt(PI) * len * sqrt(ap) * ip * ip
                }) + self.port_extra_r[j];
            }
        }
        reset(&mut el.c, n);
        reset(&mut el.wall_area, n);
        for k in 0..n {
            let a = js_max(a_t[k], self.min_node_area);
            el.c[k] = (a * dx[k]) / RHO_C2;
            el.wall_area[k] = if self.has_wall[k] {
                self.wall_perim[k] * sqrt(a) * dx[k]
            } else {
                0.0
            };
        }
        el.n_nodes = n;
        el.n_branches = nb;
        let copy = |dst: &mut Vec<f64>, src: &[f64]| {
            dst.clear();
            dst.extend_from_slice(src);
        };
        el.a.clear();
        el.a.extend_from_slice(&self.b_a);
        el.b.clear();
        el.b.extend_from_slice(&self.b_b);
        copy(&mut el.wall_m, &self.wall_m);
        copy(&mut el.wall_r, &self.wall_r);
        copy(&mut el.wall_k, &self.wall_k);
        copy(&mut el.wall_rad, &self.wall_rad);
        el.fixed.clear();
        el.fixed.extend_from_slice(&self.fixed);
        el.rad_nodes.clear();
        el.rad_nodes.extend_from_slice(&self.rad_nodes);
        el.rad_area.clear();
        el.rad_area
            .extend(self.rad_nodes.iter().map(|&k| js_max(a_t[k], 1e-4)));
    }
}
