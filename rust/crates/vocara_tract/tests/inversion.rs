//! The inversion: round trips through the forward model, smoothness, unvoiced gaps,
//! determinism, and what a frame costs.
#![allow(clippy::needless_range_loop)] // postures and formants, indexed
use std::time::Instant;
use vocara_tract::anatomy::{BodyMix, BodyName};
use vocara_tract::components::TractComponents;
use vocara_tract::forward::TractModel;
use vocara_tract::inversion::{
    suggest_body, FormantFrame, InversionConfig, Inverter, TractEstimate,
};
use vocara_tract::posture::{sung_vowels, Posture, JAW, LIP_PROT, N_FIT, TB_X, TB_Y};
use vocara_tract::vowels::vowel;

const BODIES: [BodyName; 3] = [BodyName::Male, BodyName::Female, BodyName::Child];
/// The live analysis' frame period (48 kHz, hop 512).
const DT: f64 = 512.0 / 48000.0;

fn f0_of(b: BodyName) -> f64 {
    match b {
        BodyName::Male => 130.0,
        BodyName::Female => 220.0,
        BodyName::Child => 280.0,
    }
}

/// F1–F3 the model gives a posture.
fn formants(model: &mut TractModel, p: &Posture) -> [f64; 3] {
    let f = model.lowest_formants(&p.articulation(), 3);
    assert_eq!(f.count, 3);
    [f.freq[0], f.freq[1], f.freq[2]]
}

/// Holds `formants` for `frames` frames from `t0`; returns the last estimate.
fn hold(inv: &mut Inverter, t0: f64, frames: usize, f0: f64, formants: [f64; 3]) -> TractEstimate {
    for i in 0..frames {
        inv.update(&FormantFrame {
            time: t0 + i as f64 * DT,
            f0,
            formants,
            confidence: 1.0,
        });
    }
    *inv.estimate()
}

/// A deterministic sequence in [−1, 1) (xorshift).
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// The sung-vowel postures a round trip runs through, none of them a codebook entry (the
/// codebook holds the calibrated vowels themselves): each of CHIMERA's calibrated sung vowels
/// of `b` in the fitted subspace, moved by up to ±0.15 cm (±0.1 the lips' protrusion), and the
/// midpoints of the corner vowels.
fn round_trip_postures(b: BodyName) -> Vec<(String, Posture)> {
    let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
    let vowels: Vec<(String, Posture)> = sung_vowels(b)
        .map(|v| (v.ipa.to_string(), Posture::of(&v.artic)))
        .collect();
    let mut out: Vec<(String, Posture)> = vowels
        .iter()
        .map(|(name, p)| {
            let mut q = *p;
            for j in 0..N_FIT {
                let reach = if j == LIP_PROT { 0.1 } else { 0.15 };
                q.0[j] += reach * noise.next();
            }
            (format!("{name}'"), q.clamped())
        })
        .collect();
    let corner = |ipa: &str| vowels.iter().find(|(n, _)| n == ipa).unwrap().1;
    let (i, a, u) = (corner("i"), corner("ɑ"), corner("u"));
    for (name, p, q) in [("i–ɑ", i, a), ("ɑ–u", a, u), ("u–i", u, i)] {
        let mut m = [0.0; N_FIT];
        for j in 0..N_FIT {
            m[j] = 0.5 * (p.0[j] + q.0[j]);
        }
        out.push((name.to_string(), Posture(m).clamped()));
    }
    out
}

/// A round trip's errors: the formants (max over F1–F3, relative) and the well-determined
/// components (0–1 scales; the jaw in mm).
#[derive(Default)]
struct Errors {
    formant: Vec<f64>,
    jaw_mm: Vec<f64>,
    height: Vec<f64>,
    front: Vec<f64>,
    round: Vec<f64>,
}

fn max(v: &[f64]) -> f64 {
    v.iter().copied().fold(0.0, f64::max)
}
fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

fn round_trip(b: BodyName) -> Errors {
    let body: BodyMix = b.into();
    let mut model = TractModel::new(body);
    let mut inv = Inverter::new(body);
    let mut r = Errors::default();
    for (name, p) in round_trip_postures(b) {
        let want = formants(&mut model, &p);
        inv.reset();
        let e = hold(&mut inv, 0.0, 40, f0_of(b), want);
        assert!(e.fitted, "{b:?} {name}");
        let ef = (0..3)
            .map(|i| (e.model_formants[i] / want[i] - 1.0).abs())
            .fold(0.0, f64::max);
        let (got, truth) = (
            TractComponents::of(&e.articulation, body),
            TractComponents::of(&p.articulation(), body),
        );
        println!(
            "{b:?} {name:>4}: formants {:.1} %, jaw {:+.1} mm, height {:+.2}, front {:+.2}, rounding {:+.2}, confidence {:.2}",
            100.0 * ef,
            got.jaw_mm - truth.jaw_mm,
            got.tongue_height - truth.tongue_height,
            got.tongue_frontness - truth.tongue_frontness,
            got.lip_rounding - truth.lip_rounding,
            e.confidence
        );
        r.formant.push(ef);
        r.jaw_mm.push((got.jaw_mm - truth.jaw_mm).abs());
        r.height
            .push((got.tongue_height - truth.tongue_height).abs());
        r.front
            .push((got.tongue_frontness - truth.tongue_frontness).abs());
        r.round.push((got.lip_rounding - truth.lip_rounding).abs());
    }
    r
}

/// Round trip: sung vowels' formants (the model's), inverted, give back the formants within a
/// few percent and the well-determined components close.
///
/// Tolerances, over each body's 26 postures: formants within 1 % on average and 3 % at worst
/// (measured: 0.15–0.19 % and 0.75 %); on average the jaw within 3 mm (1.5–1.7), the tongue's
/// height and frontness within 0.15 of their 0–1 scales (0.07–0.08 and 0.05–0.07), the lips'
/// rounding within 0.25 (0.11–0.20). A single posture can miss by more (up to 7 mm of jaw, 0.6
/// of the tongue's scales, the whole rounding scale): three formants leave four directions to
/// the priors, and a posture with an unusual combination (a raised tongue with an open jaw, an
/// unrounded high central vowel) is fitted as the more usual one with the same formants.
#[test]
fn a_round_trip_recovers_the_formants_and_the_main_articulators() {
    for b in BODIES {
        let r = round_trip(b);
        println!(
            "{b:?}: formants mean {:.2} % max {:.2} %; jaw mean {:.1} max {:.1} mm; height mean {:.2} max {:.2}; front mean {:.2} max {:.2}; rounding mean {:.2} max {:.2}",
            100.0 * mean(&r.formant),
            100.0 * max(&r.formant),
            mean(&r.jaw_mm),
            max(&r.jaw_mm),
            mean(&r.height),
            max(&r.height),
            mean(&r.front),
            max(&r.front),
            mean(&r.round),
            max(&r.round)
        );
        assert!(max(&r.formant) < 0.03, "{b:?} formants");
        assert!(mean(&r.formant) < 0.01, "{b:?} formants");
        assert!(mean(&r.jaw_mm) < 3.0, "{b:?} jaw");
        assert!(mean(&r.height) < 0.15, "{b:?} tongue height");
        assert!(mean(&r.front) < 0.15, "{b:?} tongue frontness");
        assert!(mean(&r.round) < 0.25, "{b:?} lip rounding");
    }
}

/// A held ɑ, a glide to i, a held i: each frame's F1–F3 the model's along the way, times
/// 1 ± 3 % noise. Returns the frames.
fn noisy_glide(body: BodyMix, f0: f64) -> Vec<FormantFrame> {
    let b = BodyName::Male;
    let start = Posture::of(&vowel(b, "ɑ").unwrap().artic);
    let end = Posture::of(&vowel(b, "i").unwrap().artic);
    let mut model = TractModel::new(body);
    let mut noise = Noise(7);
    let (hold_n, glide_n) = (40, 24);
    (0..2 * hold_n + glide_n)
        .map(|i| {
            let u = (i as f64 - hold_n as f64).clamp(0.0, glide_n as f64) / glide_n as f64;
            let mut p = [0.0; N_FIT];
            for j in 0..N_FIT {
                p[j] = start.0[j] + (end.0[j] - start.0[j]) * u;
            }
            let f = formants(&mut model, &Posture(p));
            FormantFrame {
                time: i as f64 * DT,
                f0,
                formants: f.map(|x| x * (1.0 + 0.03 * noise.next())),
                confidence: 1.0,
            }
        })
        .collect()
}

/// RMS frame-to-frame change (cm) of the jaw, the tongue body and the lips' protrusion over
/// `range` of a track.
fn jitter(track: &[TractEstimate], range: std::ops::Range<usize>) -> f64 {
    let mut sum = 0.0;
    let mut n = 0;
    for i in range.start + 1..range.end {
        for j in [JAW, TB_X, TB_Y, LIP_PROT] {
            let d = track[i].posture.0[j] - track[i - 1].posture.0[j];
            sum += d * d;
            n += 1;
        }
    }
    (sum / n as f64).sqrt()
}

/// Smoothness: a formant track with ±3 % noise gives an articulation track that holds still
/// on the held vowels (its frame-to-frame change under half a millimetre, a third of what
/// fitting each frame alone gives) and still follows the glide.
#[test]
fn a_noisy_formant_track_gives_a_smooth_articulation_track() {
    let body = BodyMix::MALE;
    let frames = noisy_glide(body, 130.0);
    let run = |config: InversionConfig| -> Vec<TractEstimate> {
        let mut inv = Inverter::with_config(body, config);
        frames.iter().map(|f| *inv.update(f)).collect()
    };
    let smooth = run(InversionConfig::default());
    // without the smoothness: articulators free to move 10 km/s
    let free = run(InversionConfig {
        speed: [1e6; N_FIT],
        ..InversionConfig::default()
    });
    // the holds, past the first frames' convergence
    let (mut s_all, mut f_all) = (0.0, 0.0);
    for h in [10..40, 74..104] {
        let (s, f) = (jitter(&smooth, h.clone()), jitter(&free, h.clone()));
        println!("hold {h:?}: jitter {s:.4} cm smoothed, {f:.4} cm frame by frame");
        assert!(s < 0.05, "smoothed jitter {s}");
        s_all += s * s;
        f_all += f * f;
    }
    assert!(s_all * 8.0 < f_all, "smoothed {s_all} vs free {f_all}");
    // it follows: ɑ low and back, i high and front
    let c = |e: &TractEstimate| TractComponents::of(&e.articulation, body);
    let (a, i) = (c(&smooth[39]), c(&smooth[103]));
    assert!(a.tongue_height < 0.3 && a.tongue_frontness < 0.5, "{a:?}");
    assert!(i.tongue_height > 0.7 && i.tongue_frontness > 0.7, "{i:?}");
    // and fits: formants within the noise
    for e in &smooth[74..104] {
        assert!(e.residual < 0.05, "{}", e.residual);
    }
}

/// Unvoiced frames hold the posture and let it relax towards neutral, the confidence fading;
/// low-confidence frames are held too; the next voiced frames fit again at once.
#[test]
fn unvoiced_frames_hold_and_decay() {
    let body = BodyMix::FEMALE;
    let mut model = TractModel::new(body);
    let a = formants(
        &mut model,
        &Posture::of(&vowel(BodyName::Female, "ɑ").unwrap().artic),
    );
    let i = formants(
        &mut model,
        &Posture::of(&vowel(BodyName::Female, "i").unwrap().artic),
    );
    let mut inv = Inverter::new(body);
    let voiced = hold(&mut inv, 0.0, 30, 220.0, a);
    assert!(voiced.fitted && voiced.confidence > 0.9);
    let neutral = Posture::NEUTRAL;
    let distance = |p: &Posture| -> f64 {
        (0..N_FIT)
            .map(|j| (p.0[j] - neutral.0[j]).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    let mut prev = voiced;
    let t0 = 30.0 * DT;
    let gap = 56; // 0.6 s
    for k in 0..gap {
        // a few frames of breath noise the analysis is unsure of, then silence
        let frame = if k < 5 {
            FormantFrame {
                time: t0 + k as f64 * DT,
                f0: 300.0,
                formants: [900.0, 2500.0, 3500.0],
                confidence: 0.2,
            }
        } else {
            FormantFrame {
                time: t0 + k as f64 * DT,
                f0: f64::NAN,
                formants: [f64::NAN; 3],
                confidence: 0.0,
            }
        };
        let e = *inv.update(&frame);
        assert!(!e.fitted);
        assert!(e.confidence < prev.confidence, "frame {k}");
        assert!(distance(&e.posture) < distance(&prev.posture), "frame {k}");
        assert!(e.model_formants[0].is_nan() && e.residual.is_nan());
        prev = e;
    }
    // gently: after 0.6 s still mostly the held posture, but no longer trusted
    let held = distance(&prev.posture) / distance(&voiced.posture);
    assert!(held > 0.6 && held < 0.75, "{held}");
    assert!(prev.confidence < 0.15, "{}", prev.confidence);
    // the voice comes back on another vowel: fitted from the first frame, settled within 10
    let t1 = t0 + gap as f64 * DT;
    let first = *inv.update(&FormantFrame {
        time: t1,
        f0: 220.0,
        formants: i,
        confidence: 1.0,
    });
    assert!(first.fitted);
    let settled = hold(&mut inv, t1 + DT, 10, 220.0, i);
    for n in 0..3 {
        assert!(
            (settled.model_formants[n] / i[n] - 1.0).abs() < 0.03,
            "F{}",
            n + 1
        );
    }
}

#[test]
fn the_suggested_body_follows_the_singers_f3() {
    for b in BODIES {
        let f3: Vec<f64> = sung_vowels(b)
            .flat_map(|v| std::iter::repeat_n(v.formants[2], 3))
            .collect();
        let s = suggest_body(f3).unwrap();
        match b {
            BodyName::Male => assert!(s.female < 0.05 && s.child == 0.0, "{s:?}"),
            BodyName::Female => assert!(s.female > 0.95 && s.child < 0.05, "{s:?}"),
            BodyName::Child => assert!(s.child > 0.95, "{s:?}"),
        }
    }
    // between: a part-female body
    let mid = suggest_body([2620.0; 40]).unwrap();
    assert!(
        mid.female > 0.2 && mid.female < 0.8 && mid.child == 0.0,
        "{mid:?}"
    );
    assert!(suggest_body([2500.0; 10]).is_none());
}

#[test]
fn a_seek_back_starts_afresh() {
    let body = BodyMix::MALE;
    let mut model = TractModel::new(body);
    let u = formants(
        &mut model,
        &Posture::of(&vowel(BodyName::Male, "u").unwrap().artic),
    );
    let mut fresh = Inverter::new(body);
    let mut used = Inverter::new(body);
    hold(&mut used, 0.0, 20, 130.0, u);
    let frame = FormantFrame {
        time: 0.05,
        f0: 130.0,
        formants: u,
        confidence: 1.0,
    };
    assert_eq!(fresh.update(&frame), used.update(&frame));
}

#[test]
fn the_same_frames_give_the_same_estimates() {
    let run = || {
        let mut inv = Inverter::new(BodyMix::FEMALE);
        noisy_glide(BodyMix::FEMALE, 220.0)
            .iter()
            .flat_map(|f| {
                let e = inv.update(f);
                let mut bits: Vec<u64> = e.posture.0.map(f64::to_bits).to_vec();
                bits.push(e.confidence.to_bits());
                bits.extend(e.model_formants.map(f64::to_bits));
                bits
            })
            .collect::<Vec<u64>>()
    };
    assert_eq!(run(), run());
}

/// What a frame costs: a minute of vowels sung one after another, each held half a second,
/// with ±3 % formant noise and breaths between some (this machine, optimised: see the
/// printout; the report quotes it).
#[test]
#[ignore]
fn timing_inversion() {
    for b in BODIES {
        let body: BodyMix = b.into();
        let mut model = TractModel::new(body);
        let targets: Vec<[f64; 3]> = sung_vowels(b)
            .map(|v| formants(&mut model, &Posture::of(&v.artic)))
            .collect();
        let mut inv = Inverter::new(body); // builds the codebook, outside the timing
        let mut noise = Noise(11);
        let frames: Vec<FormantFrame> = (0..5600)
            .map(|i| {
                let vowel = i / 47;
                let breath = vowel % 5 == 4 && i % 47 < 12;
                let f = targets[(vowel * 7) % targets.len()];
                FormantFrame {
                    time: i as f64 * DT,
                    f0: f0_of(b),
                    formants: if breath {
                        [f64::NAN; 3]
                    } else {
                        f.map(|x| x * (1.0 + 0.03 * noise.next()))
                    },
                    confidence: if breath { 0.0 } else { 1.0 },
                }
            })
            .collect();
        let mut times: Vec<f64> = Vec::with_capacity(frames.len());
        let mut evals = [0usize; 16];
        for f in &frames {
            let t = Instant::now();
            std::hint::black_box(inv.update(f));
            times.push(t.elapsed().as_secs_f64() * 1e3);
            evals[inv.last_evaluations().min(15)] += 1;
        }
        let total: f64 = times.iter().sum();
        times.sort_by(f64::total_cmp);
        println!(
            "{b:?}: {} frames, mean {:.3} ms, median {:.3} ms, 99th percentile {:.3} ms, max {:.3} ms",
            times.len(),
            total / times.len() as f64,
            times[times.len() / 2],
            times[times.len() * 99 / 100],
            times[times.len() - 1]
        );
        println!("{b:?}: frames by model evaluations {evals:?}");
    }
}
