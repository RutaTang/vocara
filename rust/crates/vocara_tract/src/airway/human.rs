//! The human airway's network (port of CHIMERA's `voice/human_airway.rs`, the part that builds
//! the network and applies an articulation to it):
//!
//! ```text
//! lungs → trachea → glottis → pharynx ─┬─ oral → lips
//!                                      └─ velum port → nasal (+maxillary sinus) → nostrils
//! ```
//!
//! plus the piriform fossae (a closed side branch above the epilarynx). An articulation becomes
//! geometry ([`HumanVocalTract::compute`]), an area function resampled into the pharynx and
//! oral tubes ([`HumanVocalTract::resample`]), and the tubes' and velum port's targets. The node
//! counts follow the sample rate as CHIMERA sizes them; Vocara analyses the airway as CHIMERA's
//! UI does, at 96 kHz ([`ANALYSIS_RATE`]). The synthesizer's slew limiting and turbulence noise
//! are left out.

use crate::airway::network::{AirwayNetwork, Area, PortSpec, Tube, TubeSpec};
use crate::anatomy::{HumanArtic, HumanBody, HumanFrame, HumanVocalTract};
use crate::math::{js_max, js_min, js_round};
use crate::physics::{C_SOUND, RHO, WALL_NASAL, WALL_SOFT, WALL_TRACHEA};

/// The sample rate the airway is sized for when it is analysed (CHIMERA's UI and calibration
/// build theirs at 96 kHz): the node counts, and so the formants, depend on it.
pub const ANALYSIS_RATE: f64 = 96000.0;

#[derive(Clone, Debug)]
pub struct HumanAirway {
    pub net: AirwayNetwork,
    pub tract: HumanVocalTract,
    pub body: HumanBody,
    pub trachea: Tube,
    pub pharynx: Tube,
    pub oral: Tube,
    pub nasal: Tube,
    pub fossa: Tube,
    pub lungs: usize,
    pub lung_port: usize,
    pub glottis_port: usize,
    pub velum_port: usize,
    pub rad_lips: usize,
    pub rad_nose: usize,
    pub n_ph: usize,
    pub n_or: usize,
    pub ph_areas: Vec<f64>,
    pub or_areas: Vec<f64>,
    pub ph_min: Vec<f64>,
    pub or_min: Vec<f64>,
    /// the frame of the last [`HumanAirway::apply`]
    pub frame: HumanFrame,
    pub ph_len: f64,
    pub or_len: f64,
    /// shortest section (cm) the airway's time step allows, at Courant number 0.97: a pharynx or
    /// oral tube is never shorter than its node count times this
    pub dx_stable: f64,
}

impl HumanAirway {
    /// `fs`: internal sample rate (Hz), which sizes the tubes' sections.
    pub fn new(fs: f64, body: HumanBody) -> Self {
        let k = body.scale;
        let tract = HumanVocalTract::new(body);
        let mut net = AirwayNetwork::new(fs);
        let dx_min = (C_SOUND / fs) / 0.93;
        // node counts from the shortest expected lengths (larynx raised, lips retracted)
        let ph_min_len = 8.7 * k * (0.55 + 0.45 * body.pharynx) - 1.6 * k;
        let or_min_len = 8.3 * k - 1.0 * k;
        let n_ph = (8.0f64).max((ph_min_len / dx_min).floor()) as usize;
        let n_or = (8.0f64).max((or_min_len / dx_min).floor()) as usize;

        let tr_len = 12.0 * k;
        let trachea = net.add_tube(
            TubeSpec::new(
                "trachea",
                js_max(6.0, (tr_len / js_max(dx_min, 0.9 * k)).floor()) as usize,
                tr_len,
                Area::Uniform(2.5 * k * k),
            )
            .wall(Some(WALL_TRACHEA)),
        );
        let pharynx = net.add_tube(
            TubeSpec::new("pharynx", n_ph, 8.8 * k, Area::Uniform(3.0 * k * k))
                .wall(Some(WALL_SOFT))
                .radiating(0.25)
                .perimeter(1.25),
        );
        let oral = net.add_tube(
            TubeSpec::new("oral", n_or, 8.3 * k, Area::Uniform(3.0 * k * k))
                .wall(Some(WALL_SOFT))
                .radiating(0.25)
                .perimeter(1.5),
        );
        let na_len = 11.4 * k;
        let n_na = js_max(8.0, (na_len / js_max(dx_min, 0.5 * k)).floor()) as usize;
        let na_areas: Vec<f64> = (0..n_na)
            .map(|i| {
                let u = (i as f64 + 0.5) / n_na as f64;
                let a = if u < 0.18 {
                    2.6
                } else if u < 0.75 {
                    2.0 - 0.6 * (u - 0.18)
                } else if u < 0.9 {
                    1.4
                } else {
                    1.0
                };
                a * k * k
            })
            .collect();
        let nasal = net.add_tube(
            TubeSpec::new("nasal", n_na, na_len, Area::Profile(na_areas))
                .wall(Some(WALL_NASAL))
                .perimeter(3.2)
                .viscous(2.5),
        );
        // three sections where they fit the time step
        let n_fo = js_max(1.0, js_min(3.0, (2.0 * k / dx_min).floor())) as usize;
        let fossa = net.add_tube(
            TubeSpec::new("piriform", n_fo, 2.0 * k, Area::Uniform(0.9 * k * k))
                .wall(Some(WALL_SOFT))
                .perimeter(1.3),
        );

        let lungs = net.add_pressure_node("lungs");
        let tr_a = 2.5 * k * k;
        // half the trachea's characteristic impedance terminates its sound
        let r_ac = (0.5 * RHO * C_SOUND) / tr_a;
        let lung_port = net.add_port(
            lungs,
            trachea.first_node(),
            PortSpec::new(tr_a, 1.0).kinetic(0.0, 0.0).resistance(r_ac),
        );
        let glottis_port = net.add_port(
            trachea.last_node(),
            pharynx.first_node(),
            PortSpec::new(0.0, 0.3 * k).kinetic(0.37, 1.0).slit(1.4 * k),
        );
        net.connect(pharynx.last_node(), oral.first_node());
        let velum_port = net.add_port(
            pharynx.last_node(),
            nasal.first_node(),
            PortSpec::new(0.0, 1.0 * k).kinetic(0.3, 0.8),
        );
        // piriform fossae branch off ≈2.6 cm above the glottis
        let fossa_node =
            pharynx.node(((n_ph - 1) as f64).min(js_round((2.6 / 8.8) * n_ph as f64)) as usize);
        net.connect(fossa_node, fossa.first_node());
        // maxillary sinus (Helmholtz) on the nasal cavity
        let sinus = net.add_cavity("sinus", 14.0 * k * k * k, None);
        net.add_port(
            nasal.node(js_round(n_na as f64 * 0.45) as usize),
            sinus,
            PortSpec::new(0.12 * k * k, 0.5 * k)
                .kinetic(0.0, 0.0)
                .no_half_sections(),
        );
        let rad_lips = net.add_radiation(oral.last_node());
        let rad_nose = net.add_radiation(nasal.last_node());
        net.finalize();
        HumanAirway {
            net,
            tract,
            body,
            trachea,
            pharynx,
            oral,
            nasal,
            fossa,
            lungs,
            lung_port,
            glottis_port,
            velum_port,
            rad_lips,
            rad_nose,
            n_ph,
            n_or,
            ph_areas: vec![0.0; n_ph],
            or_areas: vec![0.0; n_or],
            ph_min: vec![0.0; n_ph],
            or_min: vec![0.0; n_or],
            frame: HumanFrame::with_capacity(),
            ph_len: 8.8,
            or_len: 8.3,
            dx_stable: (C_SOUND / fs) / 0.97,
        }
    }

    /// Geometry → the network's targets for articulation `a` (CHIMERA's `apply(a, 0.0)`: no
    /// slew limiting). Returns the frame, which stays readable as [`HumanAirway::frame`].
    pub fn apply(&mut self, a: &HumanArtic) -> &HumanFrame {
        self.tract.compute(a, &mut self.frame);
        let (ph_len, or_len) = self.tract.resample(
            a,
            &self.frame,
            &mut self.ph_areas,
            &mut self.or_areas,
            Some(&mut self.ph_min),
            Some(&mut self.or_min),
        );
        // A pose past the expected extremes made sections shorter than one time step allows;
        // such a tube is stretched in the network (CHIMERA issue #50), while ph_len/or_len stay
        // anatomical.
        self.ph_len = ph_len;
        self.or_len = or_len;
        let ph_sim = js_max(ph_len, self.n_ph as f64 * self.dx_stable);
        let or_sim = js_max(or_len, self.n_or as f64 * self.dx_stable);
        self.net
            .set_tube_targets(self.pharynx, &self.ph_areas, Some(ph_sim));
        self.net
            .set_tube_targets(self.oral, &self.or_areas, Some(or_sim));
        self.net
            .set_port_target(self.velum_port, self.frame.velum_port);
        &self.frame
    }
}
