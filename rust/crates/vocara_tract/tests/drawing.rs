//! The drawing data: the 2D cut-away's outlines and the 3D head's lines, for the three bodies
//! and for postures that move what each structure should show moving.
use vocara_tract::anatomy::{BodyMix, BodyName, HumanArtic, NEUTRAL};
use vocara_tract::components::VocalFolds;
use vocara_tract::forward::TractModel;
use vocara_tract::lines::{Lines3d, Outlines2d, Structure};
use vocara_tract::vowels::vowel;

const BODIES: [BodyName; 3] = [BodyName::Male, BodyName::Female, BodyName::Child];

const VOICED: VocalFolds = VocalFolds {
    f0_hz: 220.0,
    breathiness: 0.0,
    register: 0.5,
};

fn lowest_y2(o: &Outlines2d, s: Structure) -> f32 {
    o.of(s)
        .flat_map(|l| l.points.iter().map(|p| p[1]))
        .fold(f32::INFINITY, f32::min)
}

fn lowest_y3(o: &Lines3d, s: Structure) -> f32 {
    o.of(s)
        .flat_map(|l| l.points.iter().map(|p| p[1]))
        .fold(f32::INFINITY, f32::min)
}

fn widest_z3(o: &Lines3d, s: Structure) -> f32 {
    o.of(s)
        .flat_map(|l| l.points.iter().map(|p| p[2].abs()))
        .fold(0.0, f32::max)
}

#[test]
fn the_cut_away_has_every_structure_in_the_head() {
    for b in BODIES {
        let mut model = TractModel::new(b.into());
        let mut out = Outlines2d::default();
        model.outlines(&NEUTRAL, &mut out);
        let k = b.body().scale as f32;
        for s in Structure::ALL {
            let only_3d = matches!(s, Structure::Ear | Structure::Nose | Structure::Mouth);
            assert_eq!(out.of(s).next().is_some(), !only_3d, "{b:?} {s:?}");
        }
        for l in out.lines() {
            assert!(l.points.len() >= 2, "{b:?} {:?}", l.structure);
            for p in l.points {
                assert!(p.iter().all(|x| x.is_finite()), "{b:?} {:?}", l.structure);
                // the head, from the neck's base to the crown, the back of the skull to the nose
                // (CHIMERA draws the trachea down to 4.8 cm below the glottis on every body)
                assert!(
                    p[0] > -10.0 * k && p[0] < 10.5 * k,
                    "{b:?} {:?} {p:?}",
                    l.structure
                );
                assert!(
                    p[1] > -6.0 && p[1] < 24.0 * k,
                    "{b:?} {:?} {p:?}",
                    l.structure
                );
            }
        }
        // the tongue's outline is the model's
        let frame_tongue: Vec<[f32; 2]> = model
            .frame()
            .tongue
            .iter()
            .map(|p| [p.x as f32, p.y as f32])
            .collect();
        assert_eq!(
            out.of(Structure::Tongue).next().unwrap().points,
            frame_tongue
        );
    }
}

#[test]
fn the_cut_away_moves_with_the_articulators() {
    let mut model = TractModel::new(BodyMix::MALE);
    let mut closed = Outlines2d::default();
    let mut open = Outlines2d::default();
    model.outlines(
        &HumanArtic {
            jaw: 0.2,
            ..NEUTRAL
        },
        &mut closed,
    );
    model.outlines(
        &HumanArtic {
            jaw: 1.8,
            ..NEUTRAL
        },
        &mut open,
    );
    // the jaw opens 16 mm at the incisors: the chin drops about as much
    let drop = lowest_y2(&closed, Structure::Jaw) - lowest_y2(&open, Structure::Jaw);
    assert!(drop > 0.8 && drop < 2.0, "{drop}");
    // the larynx carries the hyoid and the vocal folds
    let mut low = Outlines2d::default();
    model.outlines(
        &HumanArtic {
            larynx: -1.0,
            ..NEUTRAL
        },
        &mut low,
    );
    let fold_drop =
        lowest_y2(&closed, Structure::VocalFolds) - lowest_y2(&low, Structure::VocalFolds);
    assert!((fold_drop - 1.0).abs() < 0.05, "{fold_drop}");
    // the velum, lowered, leaves its closed position
    let mut nasal = Outlines2d::default();
    model.outlines(
        &HumanArtic {
            velum: 1.0,
            ..NEUTRAL
        },
        &mut nasal,
    );
    assert_ne!(
        nasal.of(Structure::Velum).next().unwrap().points,
        closed.of(Structure::Velum).next().unwrap().points
    );
}

#[test]
fn the_head_has_every_structure_and_is_symmetric() {
    for b in BODIES {
        let mut model = TractModel::new(b.into());
        let mut out = Lines3d::default();
        model.head3d(&vowel(b, "ɑ").unwrap().artic, &VOICED, &mut out);
        for s in Structure::ALL {
            let only_2d = matches!(s, Structure::Eye | Structure::TongueMuscle);
            assert_eq!(out.of(s).next().is_some(), !only_2d, "{b:?} {s:?}");
        }
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for l in out.lines() {
            assert!(l.points.len() >= 2, "{b:?} {:?}", l.structure);
            for p in l.points {
                assert!(p.iter().all(|x| x.is_finite()), "{b:?} {:?}", l.structure);
                if l.section {
                    assert_eq!(p[2], 0.0, "{b:?} {:?}", l.structure);
                }
                lo = lo.min(p[2]);
                hi = hi.max(p[2]);
            }
        }
        // as wide to the left as to the right, about a head's width
        let k = b.body().scale as f32;
        assert!((lo + hi).abs() < 1e-3, "{b:?} {lo} {hi}");
        assert!(hi > 6.0 * k && hi < 8.5 * k, "{b:?} {hi}");
        // the eyes either side of the nose, the head's middle above the glottis
        let head = model.head();
        let [l, r] = head.eyes();
        assert!(l[2] > 0.0 && r[2] < 0.0 && (l[2] + r[2]).abs() < 1e-6);
        assert!(head.center()[1] > 5.0 * k && head.eye_radius() > 0.5 * k);
    }
}

#[test]
fn the_head_moves_with_the_articulators() {
    let b = BodyName::Female;
    let mut model = TractModel::new(b.into());
    let mut out = Lines3d::default();
    let mut look = |a: &HumanArtic, folds: &VocalFolds| {
        model.head3d(a, folds, &mut out);
        out.clone()
    };
    let i = vowel(b, "i").unwrap().artic;
    let a = vowel(b, "ɑ").unwrap().artic;
    let (head_i, head_a) = (look(&i, &VOICED), look(&a, &VOICED));
    // the mandible and the lower teeth turn down with the jaw
    assert!(
        lowest_y3(&head_a, Structure::LowerTeeth) < lowest_y3(&head_i, Structure::LowerTeeth) - 0.5
    );
    assert!(lowest_y3(&head_a, Structure::Jaw) < lowest_y3(&head_i, Structure::Jaw) - 0.5);
    // the airway is as wide as its area function: wide pharynx for i, wide mouth for ɑ, each
    // ring no wider than 3.5 cm at the body's size
    let k = b.body().scale as f32;
    assert!(widest_z3(&head_a, Structure::Airway) <= 3.5 * k + 1e-4);
    // the mouth: the lips' opening is a closed line once the lips part, one line when they
    // touch
    let shut = look(&HumanArtic { lip_ap: -0.1, ..i }, &VOICED);
    assert!(head_a.of(Structure::Mouth).any(|l| l.closed));
    assert!(!shut.of(Structure::Mouth).any(|l| l.closed));
    // the vocal folds: apart for breathing, together (but for a chink) when voiced
    let breathing = look(&a, &VocalFolds::SILENT);
    let breathy = look(
        &a,
        &VocalFolds {
            breathiness: 1.0,
            ..VOICED
        },
    );
    let gap = |h: &Lines3d| {
        h.of(Structure::VocalFolds)
            .filter(|l| l.emphasis == vocara_tract::lines::Emphasis::Outline)
            .flat_map(|l| l.points.iter().map(|p| p[2].abs()))
            .fold(0.0, f32::max)
    };
    assert!(gap(&breathing) > gap(&breathy) && gap(&breathy) > gap(&head_a));
}

#[test]
fn a_mixed_body_draws_between_its_presets() {
    let a = NEUTRAL;
    let height = |body: BodyMix| {
        let mut model = TractModel::new(body);
        let mut out = Outlines2d::default();
        model.outlines(&a, &mut out);
        out.of(Structure::Skin)
            .flat_map(|l| l.points.iter().map(|p| p[1]))
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let (m, c, half) = (
        height(BodyMix::MALE),
        height(BodyMix::CHILD),
        height(BodyMix::new(0.0, 0.5)),
    );
    assert!(half < m && half > c, "{c} {half} {m}");
}

/// What drawing a frame costs (this machine, optimised: see the printout).
#[test]
#[ignore]
fn timing_drawing() {
    let b = BodyName::Male;
    let mut model = TractModel::new(b.into());
    let postures: Vec<HumanArtic> = vocara_tract::vowels::calibrated(b)
        .iter()
        .map(|v| v.artic)
        .collect();
    let (mut flat, mut head) = (Outlines2d::default(), Lines3d::default());
    for a in &postures {
        model.outlines(a, &mut flat);
        model.head3d(a, &VOICED, &mut head);
    }
    let n = 2000;
    let t = std::time::Instant::now();
    for i in 0..n {
        model.outlines(&postures[i % postures.len()], &mut flat);
    }
    let flat_ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
    let t = std::time::Instant::now();
    for i in 0..n {
        model.head3d(&postures[i % postures.len()], &VOICED, &mut head);
    }
    let head_ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
    println!(
        "2D cut-away {flat_ms:.3} ms ({} lines, {} points); 3D head {head_ms:.3} ms ({} lines, {} points)",
        flat.len(),
        flat.points().len(),
        head.len(),
        head.points().len()
    );
}
