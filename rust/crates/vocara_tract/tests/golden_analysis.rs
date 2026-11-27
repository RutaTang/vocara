//! The frequency-domain oracle against CHIMERA's fixtures (port of the parts of its
//! `tests/golden_analysis.rs` this crate carries): the transfer function and formants of the
//! human airway (CHIMERA's UI pipeline: 360 log-spaced frequencies from 60 Hz to 8 kHz, injected
//! at the first pharynx node, the glottis cut, peaks above 150 Hz), simple tube networks, the
//! peak finder and Nelder–Mead (iterations and evaluations exactly), to 1e-9 relative.
mod common;
use common::*;
use serde_json::Value;
use vocara_tract::airway::frequency::{find_peaks, linspace, transfer_function, Peak};
use vocara_tract::airway::human::HumanAirway;
use vocara_tract::airway::network::{AirwayNetwork, Area, PortSpec, TubeSpec};
use vocara_tract::anatomy::BodyName;
use vocara_tract::optimize::{nelder_mead, NelderMeadOpts};
use vocara_tract::physics::{RHO, WALL_SOFT};

const TOL: f64 = 1e-9;

/// Worst error per area, printed at the end.
#[derive(Default)]
struct Report(Vec<(String, f64)>);
impl Report {
    fn check(&mut self, area: &str, label: &str, want: &[f64], got: &[f64], floor: f64) {
        let w = worst(label, want, got, floor);
        assert!(w < TOL, "{label}: worst relative difference {w:e}");
        match self.0.iter_mut().find(|(a, _)| a == area) {
            Some((_, x)) => *x = x.max(w),
            None => self.0.push((area.to_string(), w)),
        }
    }
    fn peaks(&mut self, area: &str, label: &str, want: &Value, got: &[Peak]) {
        let want = want.as_array().unwrap();
        assert_eq!(want.len(), got.len(), "{label}: peak count");
        for (w, g) in want.iter().zip(got) {
            let w = nums(w);
            self.check(area, label, &w[..2], &[g.freq, g.bw], 1.0);
            self.check(area, label, &w[2..], &[g.db], 1e-6);
        }
    }
    fn print(&self) {
        for (a, w) in &self.0 {
            println!("{a:>24}: worst relative difference {w:e}");
        }
    }
}

fn body(name: &str) -> BodyName {
    match name {
        "female" => BodyName::Female,
        "child" => BodyName::Child,
        _ => BodyName::Male,
    }
}

/// CHIMERA's test networks, in its fixtures' order: (net, inject, cut).
fn networks() -> Vec<(AirwayNetwork, usize, Vec<usize>)> {
    let mut out = Vec::new();
    for walls in [false, true] {
        let mut net = AirwayNetwork::new(96000.0);
        let t = net.add_tube(
            TubeSpec::new("tract", 40, 17.5, Area::Uniform(3.0)).wall(if walls {
                Some(WALL_SOFT)
            } else {
                None
            }),
        );
        net.add_radiation(t.last_node());
        net.finalize();
        out.push((net, t.first_node(), vec![]));
    }
    {
        let mut net = AirwayNetwork::new(48000.0);
        let a = net.add_tube(
            TubeSpec::new(
                "a",
                12,
                8.0,
                Area::Profile((0..12).map(|i| 1.0 + 0.3 * i as f64).collect()),
            )
            .wall(Some(WALL_SOFT))
            .radiating(0.25),
        );
        let b = net.add_tube(TubeSpec::new("b", 10, 7.0, Area::Uniform(2.0)));
        let c =
            net.add_tube(TubeSpec::new("c", 14, 11.0, Area::Uniform(1.2)).wall(Some(WALL_SOFT)));
        net.connect(a.last_node(), b.first_node());
        net.connect(a.last_node(), c.first_node());
        net.add_radiation(b.last_node());
        net.add_radiation(c.last_node());
        net.finalize();
        out.push((net, a.first_node(), vec![]));
    }
    {
        let mut net = AirwayNetwork::new(48000.0);
        let lung = net.add_pressure_node("lung");
        let t =
            net.add_tube(TubeSpec::new("t", 20, 15.0, Area::Uniform(2.5)).wall(Some(WALL_SOFT)));
        let port = net.add_port(
            lung,
            t.first_node(),
            PortSpec::new(0.4, 0.5).resistance((0.5 * RHO * 35000.0) / 0.4),
        );
        let cav = net.add_cavity("sinus", 12.0, None);
        net.add_port(
            t.node(8),
            cav,
            PortSpec::new(0.15, 0.6)
                .kinetic(0.0, 0.0)
                .no_half_sections(),
        );
        net.add_radiation(t.last_node());
        net.finalize();
        out.push((net.clone(), t.first_node(), vec![]));
        out.push((net, t.first_node(), vec![port]));
    }
    out
}

#[test]
fn analysis_matches_chimera() {
    let g = load("analysis");
    let mut rep = Report::default();

    // ---- the transfer function and formants of the human airway
    let ui_freqs: Vec<f64> = (0..360)
        .map(|i| 60.0 * libm::pow(8000.0 / 60.0, i as f64 / 359.0))
        .collect();
    rep.check("ui freqs", "uiFreqs", &nums(&g["uiFreqs"]), &ui_freqs, 1.0);
    let mut n_formants = 0;
    for h in g["human"].as_array().unwrap() {
        let b = h["body"].as_str().unwrap().to_string();
        let mut airway = HumanAirway::new(96000.0, body(&b).body());
        for c in h["cases"].as_array().unwrap() {
            let label = format!("{b} {}", c["name"].as_str().unwrap());
            airway.apply(&artic(&c["artic"]));
            let el = airway.net.linear_elements();
            let tf = transfer_function(
                &el,
                airway.pharynx.first_node(),
                &ui_freqs,
                &[airway.glottis_port],
            );
            let peaks = find_peaks(&ui_freqs, &tf.mag, 150.0);
            let formants: Vec<f64> = peaks.iter().map(|p| p.freq).take(5).collect();
            n_formants += formants.len();
            rep.check("human |H|", &label, &nums(&c["mag"]), &tf.mag, 1e-12);
            rep.peaks("human peaks", &label, &c["peaks"], &peaks);
            rep.check(
                "human formants",
                &label,
                &nums(&c["formants"]),
                &formants,
                1.0,
            );
        }
    }
    assert!(n_formants >= 90, "formant readouts: {n_formants}");

    // ---- simple networks
    let net_freqs = linspace(20.0, 6000.0, 700);
    rep.check(
        "linspace",
        "netFreqs",
        &nums(&g["netFreqs"]),
        &net_freqs,
        1.0,
    );
    let nets = g["nets"].as_array().unwrap();
    let built = networks();
    assert_eq!(nets.len(), built.len());
    for (c, (net, inject, cut)) in nets.iter().zip(&built) {
        let label = c["name"].as_str().unwrap();
        let tf = transfer_function(&net.linear_elements(), *inject, &net_freqs, cut);
        rep.check("network |H|", label, &nums(&c["mag"]), &tf.mag, 1e-12);
        rep.check(
            "network pressure dB",
            label,
            &nums(&c["pressureDb"]),
            &tf.pressure_db,
            1e-6,
        );
        rep.peaks(
            "network peaks",
            label,
            &c["peaks"],
            &find_peaks(&net_freqs, &tf.mag, 80.0),
        );
    }

    // ---- findPeaks edge cases
    for c in g["peakCases"].as_array().unwrap() {
        let label = c["name"].as_str().unwrap();
        rep.peaks(
            "findPeaks",
            label,
            &c["peaks"],
            &find_peaks(&nums(&c["freqs"]), &nums(&c["mag"]), num(&c["minFreq"])),
        );
    }

    // ---- Nelder–Mead
    for c in g["nelderMead"].as_array().unwrap() {
        let label = c["name"].as_str().unwrap();
        let mut calls = 0usize;
        let pow = libm::pow;
        let rosen = |x: &[f64]| pow(1.0 - x[0], 2.0) + 100.0 * pow(x[1] - pow(x[0], 2.0), 2.0);
        let bowl = |x: &[f64]| {
            pow(x[0] - 3.0, 2.0)
                + 2.0 * pow(x[1] + 1.0, 2.0)
                + 0.5 * pow(x[2] - 0.2, 2.0)
                + libm::sin(3.0 * x[0]) * 0.1
        };
        let f = |x: &[f64]| {
            calls += 1;
            if label.starts_with("rosenbrock") {
                rosen(x)
            } else {
                bowl(x)
            }
        };
        let o = &c["opts"];
        let opt_vec = |v: &Value| v.is_array().then(|| nums(v));
        let (step, lo, hi) = (opt_vec(&o["step"]), opt_vec(&o["lo"]), opt_vec(&o["hi"]));
        let opts = NelderMeadOpts {
            step: step.as_deref(),
            lo: lo.as_deref(),
            hi: hi.as_deref(),
            max_iter: o["maxIter"].as_u64().map(|x| x as usize),
            tol: o["tol"].as_f64(),
        };
        let r = nelder_mead(f, &nums(&c["x0"]), opts);
        assert_eq!(
            r.iters as u64,
            c["iters"].as_u64().unwrap(),
            "{label}: iterations"
        );
        assert_eq!(
            calls as u64,
            c["calls"].as_u64().unwrap(),
            "{label}: evaluations"
        );
        rep.check("nelderMead", label, &nums(&c["x"]), r.x(), 1e-12);
        rep.check("nelderMead", label, &[num(&c["fx"])], &[r.fx], 1e-12);
    }
    rep.print();
}
