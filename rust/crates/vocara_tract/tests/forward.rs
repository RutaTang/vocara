//! The forward model: its fast formant search against CHIMERA's reference pipeline (the
//! transfer function on 360 log-spaced frequencies and `find_peaks`), the calibrated vowels'
//! formants, the area function, and what a frame costs.
use std::time::Instant;
use vocara_tract::airway::frequency::{find_peaks, transfer_function};
use vocara_tract::airway::human::{HumanAirway, ANALYSIS_RATE};
use vocara_tract::anatomy::{BodyMix, BodyName, HumanArtic, NEUTRAL};
use vocara_tract::forward::{AreaFunction, TractModel};
use vocara_tract::vowels::calibrated;

const BODIES: [BodyName; 3] = [BodyName::Male, BodyName::Female, BodyName::Child];

fn ui_freqs() -> Vec<f64> {
    (0..360)
        .map(|i| 60.0 * libm::pow(8000.0 / 60.0, i as f64 / 359.0))
        .collect()
}

/// CHIMERA's UI peaks (above 150 Hz on its 360-point grid): frequency and bandwidth.
fn reference(air: &mut HumanAirway, a: &HumanArtic) -> Vec<(f64, f64)> {
    air.apply(a);
    let el = air.net.linear_elements();
    let freqs = ui_freqs();
    let tf = transfer_function(&el, air.pharynx.first_node(), &freqs, &[air.glottis_port]);
    find_peaks(&freqs, &tf.mag, 150.0)
        .iter()
        .map(|p| (p.freq, p.bw))
        .collect()
}

#[test]
fn the_bandwidths_are_the_peaks_widths() {
    // against each peak's −3 dB width on a fine grid (0.015 % apart; the reference's 1.4 %
    // grid cannot resolve the narrow ones)
    let mut worst = 0.0f64;
    for b in BODIES {
        let mut model = TractModel::new(b.into());
        let mut air = HumanAirway::new(ANALYSIS_RATE, b.body());
        for ipa in ["i", "ɑ", "u", "ə"] {
            let a = vocara_tract::vowels::vowel(b, ipa).unwrap().artic;
            air.apply(&a);
            let el = air.net.linear_elements();
            let got = model.formants(&a);
            for i in 0..3 {
                let f0 = got.freq[i];
                let fs: Vec<f64> = (0..2000)
                    .map(|j| f0 * (0.85 + 0.3 * j as f64 / 1999.0))
                    .collect();
                let tf = transfer_function(&el, air.pharynx.first_node(), &fs, &[air.glottis_port]);
                let fine = find_peaks(&fs, &tf.mag, 0.0)
                    .into_iter()
                    .min_by(|p, q| (p.freq - f0).abs().total_cmp(&(q.freq - f0).abs()))
                    .unwrap();
                let e = (got.bw[i] - fine.bw).abs() / fine.bw.max(10.0);
                assert!(
                    e < 0.12,
                    "{b:?} {ipa} B{}: {} vs {}",
                    i + 1,
                    got.bw[i],
                    fine.bw
                );
                worst = worst.max(e);
            }
        }
    }
    println!("bandwidths vs −3 dB widths: worst {:.1} %", 100.0 * worst);
}

#[test]
fn the_fast_formants_are_chimeras() {
    let mut worst_f = 0.0f64;
    let mut worst_f4 = 0.0f64;
    for b in BODIES {
        let mut model = TractModel::new(b.into());
        let mut air = HumanAirway::new(ANALYSIS_RATE, b.body());
        let postures = calibrated(b).iter().map(|v| (v.ipa, v.artic));
        for (name, a) in std::iter::once(("neutral", NEUTRAL)).chain(postures) {
            let want = reference(&mut air, &a);
            let got = model.formants(&a);
            assert_eq!(got.count, 4, "{b:?} {name}: {got:?}");
            for (i, (f, _)) in want.iter().take(3).enumerate() {
                let e = (got.freq[i] / f - 1.0).abs();
                // the grid's own peak sits within its parabola's error of the true maximum
                assert!(e < 0.004, "{b:?} {name} F{}: {} vs {f}", i + 1, got.freq[i]);
                worst_f = worst_f.max(e);
            }
            // Past F3 the piriform fossae's zero and its narrow resonance crowd the spectrum
            // (a female ɔ has a 7 dB bump, a zero and an 8 Hz-wide peak within 4 %): F4 is one
            // of the reference's next peaks, whichever the two grids resolve first
            let e4 = want[3..want.len().min(6)]
                .iter()
                .map(|(f, _)| (got.freq[3] / f - 1.0).abs())
                .fold(f64::INFINITY, f64::min);
            assert!(e4 < 0.03, "{b:?} {name} F4: {} vs {want:?}", got.freq[3]);
            worst_f4 = worst_f4.max(e4);
        }
    }
    println!(
        "fast formants vs CHIMERA's: F1–F3 worst {:.3} %, F4 {:.1} %",
        100.0 * worst_f,
        100.0 * worst_f4
    );
}

#[test]
fn the_calibrated_vowels_have_their_formants() {
    // CHIMERA's table holds the formants its calibration read (another grid, and the geometry
    // as it was then, which has moved a little since): the model's F1–F3 are the table's within
    // 3 %, almost all within 1 %
    let mut worst = 0.0f64;
    let mut over_1 = 0;
    let mut n = 0;
    for b in BODIES {
        let mut model = TractModel::new(b.into());
        for v in calibrated(b) {
            let got = model.formants(&v.artic);
            for i in 0..3 {
                let e = (got.freq[i] / v.formants[i] - 1.0).abs();
                assert!(
                    e < 0.03,
                    "{b:?} {} F{}: {} vs {}",
                    v.ipa,
                    i + 1,
                    got.freq[i],
                    v.formants[i]
                );
                worst = worst.max(e);
                over_1 += usize::from(e > 0.01);
                n += 1;
            }
        }
    }
    assert!(
        over_1 * 20 < n,
        "{over_1} of {n} formants more than 1 % off"
    );
    println!(
        "calibrated vowels: worst {:.2} %, {over_1} of {n} over 1 %",
        100.0 * worst
    );
}

#[test]
fn a_blended_body_lies_between_its_presets() {
    let a = NEUTRAL;
    let m = TractModel::new(BodyMix::MALE).formants(&a);
    let f = TractModel::new(BodyMix::FEMALE).formants(&a);
    let half = TractModel::new(BodyMix::new(0.5, 0.0)).formants(&a);
    for i in 0..3 {
        let (lo, hi) = (m.freq[i].min(f.freq[i]), m.freq[i].max(f.freq[i]));
        assert!(
            half.freq[i] > lo && half.freq[i] < hi,
            "F{}: {} not in {lo}..{hi}",
            i + 1,
            half.freq[i]
        );
    }
}

#[test]
fn the_area_function_runs_glottis_to_lips() {
    let mut model = TractModel::new(BodyMix::MALE);
    let mut af = AreaFunction::default();
    let a = calibrated(BodyName::Male)[0].artic; // i
    model.area_function(&a, &mut af);
    assert!(!af.samples.is_empty() && af.sections.len() > 20);
    assert!((af.sections.last().unwrap().s1 - af.length).abs() < 1e-9);
    assert!(af.samples.windows(2).all(|w| w[1].s >= w[0].s));
    assert!(af.sections.iter().all(|s| s.area >= 0.0 && s.s1 > s.s0));
    assert!(af.length > 14.0 && af.length < 19.0, "length {}", af.length);
    assert_eq!(af.velum_port, 0.0);
    // [i]: a narrow palatal constriction in front of a wide pharynx
    let narrow = af
        .sections
        .iter()
        .map(|s| s.area)
        .fold(f64::INFINITY, f64::min);
    let wide = af.sections[..10].iter().map(|s| s.area).fold(0.0, f64::max);
    assert!(
        narrow < 0.6 && wide > 3.0,
        "narrowest {narrow}, pharynx {wide}"
    );
}

#[test]
fn a_warm_model_computes_the_same_frame_twice() {
    let mut model = TractModel::new(BodyMix::FEMALE);
    let v = calibrated(BodyName::Female);
    let bits = |f: vocara_tract::forward::Formants| {
        f.freq
            .iter()
            .chain(&f.bw)
            .map(|x| x.to_bits())
            .collect::<Vec<_>>()
    };
    let first: Vec<_> = v.iter().map(|v| bits(model.formants(&v.artic))).collect();
    let again: Vec<_> = v.iter().map(|v| bits(model.formants(&v.artic))).collect();
    assert_eq!(first, again);
}

/// What a frame of the forward model costs (release, or this crate's optimised dev profile).
#[test]
#[ignore]
fn timing_forward() {
    let mut model = TractModel::new(BodyMix::MALE);
    let v = calibrated(BodyName::Male);
    for x in v {
        model.formants(&x.artic);
    }
    let n = 2000;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(model.apply(&v[i % v.len()].artic));
    }
    let geometry = t.elapsed().as_secs_f64() / n as f64;
    let t = Instant::now();
    for i in 0..n {
        std::hint::black_box(model.formants(&v[i % v.len()].artic));
    }
    let formants = t.elapsed().as_secs_f64() / n as f64;
    println!(
        "geometry {:.1} µs, geometry + formants {:.1} µs",
        geometry * 1e6,
        formants * 1e6
    );
}
