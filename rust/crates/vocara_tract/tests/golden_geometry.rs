//! The anatomy model against CHIMERA's fixtures (port of its `tests/golden_geometry.rs`): grids,
//! area functions, contacts and outlines for 114 postures of the three bodies, to 1e-9
//! relative.
mod common;
use common::*;
use vocara_tract::anatomy::{BodyName, HumanFrame, HumanVocalTract};
use vocara_tract::geom::Vec2;

fn flat(ps: &[Vec2]) -> Vec<f64> {
    ps.iter().flat_map(|p| [p.x, p.y]).collect()
}

fn body(name: &str) -> BodyName {
    match name {
        "female" => BodyName::Female,
        "child" => BodyName::Child,
        _ => BodyName::Male,
    }
}

#[test]
fn geometry_matches_chimera() {
    let cases = load("geometry");
    let mut tracts: std::collections::HashMap<String, HumanVocalTract> = Default::default();
    let mut f = HumanFrame::default();
    let mut worst_all = 0.0f64;
    for (ci, c) in cases.as_array().unwrap().iter().enumerate() {
        let b = c["body"].as_str().unwrap();
        let a = artic(&c["artic"]);
        let t = tracts
            .entry(b.to_string())
            .or_insert_with(|| HumanVocalTract::new(body(b).body()));
        t.compute(&a, &mut f);
        let label = format!("case {ci} ({b})");
        let grid = c["grid"].as_array().unwrap();
        assert_eq!(grid.len(), f.grid.len(), "{label}: grid size");
        let mut w = 0.0f64;
        for (g, r) in grid.iter().zip(&f.grid) {
            let e = nums(g);
            assert_eq!(e[0] as u8, r.region, "{label}: region");
            w = w.max(worst(&label, &e[1..], &[r.d, r.area, r.s], 1e-6));
        }
        let s = nums(&c["scalars"]);
        let mine = [
            f.length,
            f.pharynx_length,
            f.teeth_s,
            f.lip_s,
            f.velum_port,
            f.lip_area,
            f.tongue_radius,
            f.tip.x,
            f.tip.y,
            f.split_index as f64,
            t.contact_tip,
            t.contact_body,
            t.contact_tip_n.x,
            t.contact_tip_n.y,
            t.contact_body_n.x,
            t.contact_body_n.y,
        ];
        w = w.max(worst(&format!("{label} scalars"), &s, &mine, 1e-6));
        for (key, pts) in [
            ("outer", &f.outer),
            ("inner", &f.inner),
            ("tongue", &f.tongue),
            ("velum", &f.velum),
            ("upperLip", &f.upper_lip),
            ("lowerLip", &f.lower_lip),
        ] {
            if c[key].is_array() {
                w = w.max(worst(
                    &format!("{label} {key}"),
                    &nums(&c[key]),
                    &flat(pts),
                    1e-6,
                ));
            }
        }
        let rs = &c["resample"];
        let (n_ph, n_or) = (18, 18);
        let (mut ph, mut or, mut ph_min, mut or_min) = (
            vec![0.0; n_ph],
            vec![0.0; n_or],
            vec![0.0; n_ph],
            vec![0.0; n_or],
        );
        let (ph_len, or_len) = t.resample(
            &a,
            &f,
            &mut ph,
            &mut or,
            Some(&mut ph_min),
            Some(&mut or_min),
        );
        w = w.max(worst(
            &format!("{label} lengths"),
            &[num(&rs["phLen"]), num(&rs["orLen"])],
            &[ph_len, or_len],
            1e-6,
        ));
        for (key, arr) in [
            ("ph", &ph),
            ("or", &or),
            ("phMin", &ph_min),
            ("orMin", &or_min),
        ] {
            w = w.max(worst(&format!("{label} {key}"), &nums(&rs[key]), arr, 1e-6));
        }
        assert!(w < 1e-9, "{label}: worst relative difference {w:e}");
        worst_all = worst_all.max(w);
    }
    println!(
        "geometry: {} cases, worst relative difference {worst_all:e}",
        cases.as_array().unwrap().len()
    );
}
