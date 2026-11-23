//! Every line of `golden/voice_metrics.txt` is the Flutter app's own analysis
//! summary of a deterministic synthetic input (written by
//! `golden/voice_metrics.gen.dart` on the Dart SDK): the Rust port must give
//! the same numbers, bit for bit (NaN for NaN).

use std::sync::Arc;

use serde_json::Value;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::*;

fn num(v: &Value) -> f64 {
    v.as_str().unwrap().parse::<f64>().unwrap()
}

fn nums(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(num).collect()
}

fn f32s(v: &Value) -> Vec<f32> {
    nums(v).into_iter().map(|x| x as f32).collect()
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

fn assert_same(actual: f64, expected: &Value, what: &str) {
    let e = num(expected);
    assert!(same(actual, e), "{what}: rust {actual:?} dart {e:?}");
}

fn assert_curve(actual: &[f32], expected: &Value, what: &str) {
    let e = nums(expected);
    assert_eq!(actual.len(), e.len(), "{what} length");
    for (i, (a, b)) in actual.iter().zip(&e).enumerate() {
        assert!(
            same(f64::from(*a), *b),
            "{what}[{i}]: rust {a:?} dart {b:?}"
        );
    }
}

fn opt_int(v: &Value) -> Option<i64> {
    v.as_i64()
}

fn metrics(j: &Value) -> VoiceMetrics {
    let nan = || vec![f32::NAN; j["frames"].as_u64().unwrap() as usize];
    VoiceMetrics {
        frames: j["frames"].as_u64().unwrap() as usize,
        hop: j["hop"].as_i64().unwrap(),
        fft_size: 2048,
        sample_rate: j["sampleRate"].as_i64().unwrap(),
        ring: f32s(&j["ring"]),
        tilt: f32s(&j["tilt"]),
        cpp: f32s(&j["cpp"]),
        loudness: f32s(&j["loudness"]),
        f0: f32s(&j["f0"]),
        f1: nan(),
        f2: nan(),
        f3: nan(),
        h1h2: f32s(&j["h1h2"]),
        active: j["active"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_u64().unwrap() as u8)
            .collect(),
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: num(&j["peakDbfs"]),
        clipped_fraction: num(&j["clippedFraction"]),
        snr_db: num(&j["snrDb"]),
    }
}

fn check_vibrato(v: &Vibrato, out: &Value) {
    assert_eq!(v.present, out["present"].as_bool().unwrap());
    assert_same(v.rate_hz, &out["rateHz"], "rateHz");
    assert_same(v.extent_cents, &out["extentCents"], "extentCents");
    assert_same(v.regularity, &out["regularity"], "regularity");
    assert_same(v.jitter, &out["jitter"], "jitter");
    assert_same(v.onset_ms, &out["onsetMs"], "onsetMs");
    assert_curve(&v.deviation_cents, &out["deviationCents"], "deviationCents");
}

fn check_ltas(l: &Ltas, out: &Value) {
    assert_curve(&l.db, &out["db"], "ltas db");
    assert_same(l.bin_hz, &out["binHz"], "binHz");
    assert_same(l.max_db, &out["maxDb"], "maxDb");
    assert_same(l.alpha_ratio, &out["alphaRatio"], "alphaRatio");
    assert_same(
        l.singers_formant_db,
        &out["singersFormantDb"],
        "singersFormantDb",
    );
    assert_eq!(l.frame_count as u64, out["frameCount"].as_u64().unwrap());
}

#[test]
fn rust_analysis_summaries_match_dart_bit_for_bit() {
    let golden = include_str!("golden/voice_metrics.txt");
    let mut cases = 0;
    for line in golden.lines().filter(|l| !l.is_empty()) {
        let j: Value = serde_json::from_str(line).unwrap();
        let out = &j["out"];
        match j["case"].as_str().unwrap() {
            "vibrato" => {
                let m = metrics(&j["metrics"]);
                let v = analyze_vibrato(
                    &m,
                    j["lo"].as_i64().unwrap(),
                    j["hi"].as_i64().unwrap(),
                    VibratoOptions::default(),
                );
                check_vibrato(&v, out);
            }
            "vibratoLive" => {
                let hist = nums(&j["history"]);
                let v = analyze_vibrato_live(
                    &hist,
                    num(&j["frameRate"]),
                    1.6,
                    VibratoOptions::default(),
                );
                check_vibrato(&v, out);
            }
            "profile" => {
                let m = metrics(&j["metrics"]);
                let p = analyze_voice_profile(&m, VoiceProfileOptions::default());
                assert_eq!(p.low_midi, opt_int(&out["lowMidi"]));
                assert_eq!(p.high_midi, opt_int(&out["highMidi"]));
                assert_eq!(p.tess_lo_midi, opt_int(&out["tessLoMidi"]));
                assert_eq!(p.tess_hi_midi, opt_int(&out["tessHiMidi"]));
                assert_eq!(p.passaggio_midi, opt_int(&out["passaggioMidi"]));
                assert_same(p.passaggio_jump_db, &out["passaggioJumpDb"], "jump");
                assert_same(p.soft_db, &out["softDb"], "softDb");
                assert_same(p.loud_db, &out["loudDb"], "loudDb");
                assert_same(p.median_db, &out["medianDb"], "medianDb");
                let r = analyze_register(&m, RegisterOptions::default());
                assert_curve(&r.score, &out["score"], "register score");
                assert_same(r.chest_pct, &out["chestPct"], "chestPct");
                assert_same(r.mix_pct, &out["mixPct"], "mixPct");
                assert_same(r.head_pct, &out["headPct"], "headPct");
                assert_eq!(
                    r.voiced_frames as u64,
                    out["voicedFrames"].as_u64().unwrap()
                );
            }
            "ltas" => {
                let frames = j["frames"].as_u64().unwrap() as usize;
                let s = Spectrogram {
                    frames,
                    bins: j["bins"].as_u64().unwrap() as usize,
                    max_hz: num(&j["maxHz"]),
                    sample_rate: 44100,
                    fft_size: 2048,
                    hop: 512,
                    db: f32s(&j["db"]),
                    min_db: -90.0,
                    max_db: 0.0,
                };
                let active: Vec<u8> = j["active"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_u64().unwrap() as u8)
                    .collect();
                let m = VoiceMetrics {
                    frames,
                    hop: 512,
                    fft_size: 2048,
                    sample_rate: 44100,
                    ring: vec![0.0; frames],
                    tilt: vec![0.0; frames],
                    cpp: vec![0.0; frames],
                    loudness: vec![0.0; frames],
                    f0: vec![0.0; frames],
                    f1: vec![f32::NAN; frames],
                    f2: vec![f32::NAN; frames],
                    f3: vec![f32::NAN; frames],
                    h1h2: vec![f32::NAN; frames],
                    active,
                    ring_avg: 0.0,
                    tilt_avg: 0.0,
                    cpp_avg: 0.0,
                    peak_dbfs: f64::NAN,
                    clipped_fraction: 0.0,
                    snr_db: f64::NAN,
                };
                check_ltas(&compute_ltas(&s, Some(&m)), out);
                check_ltas(&compute_ltas(&s, None), &j["outUngated"]);
            }
            "mergeMetrics" => {
                let parts: Vec<VoiceMetricsPart> = j["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| VoiceMetricsPart {
                        offset_ms: p["offsetMs"].as_i64().unwrap(),
                        metrics: Arc::new(metrics(&p["metrics"])),
                    })
                    .collect();
                let r = merge_voice_metrics(&parts).unwrap();
                assert_eq!(r.frames as u64, out["frames"].as_u64().unwrap());
                for (curve, key) in [
                    (&r.f0, "f0"),
                    (&r.ring, "ring"),
                    (&r.tilt, "tilt"),
                    (&r.cpp, "cpp"),
                    (&r.loudness, "loudness"),
                    (&r.h1h2, "h1h2"),
                ] {
                    assert_curve(curve, &out[key], key);
                }
                let active: Vec<u8> = out["active"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_u64().unwrap() as u8)
                    .collect();
                assert_eq!(r.active, active);
                assert_same(r.ring_avg, &out["ringAvg"], "ringAvg");
                assert_same(r.tilt_avg, &out["tiltAvg"], "tiltAvg");
                assert_same(r.cpp_avg, &out["cppAvg"], "cppAvg");
                assert_same(r.peak_dbfs, &out["peakDbfs"], "peakDbfs");
                assert_same(r.clipped_fraction, &out["clippedFraction"], "clipped");
                assert_same(r.snr_db, &out["snrDb"], "snrDb");
            }
            "notes" => {
                for entry in out.as_array().unwrap() {
                    let (input, label) = entry.as_str().unwrap().split_once('=').unwrap();
                    let actual = match input.strip_prefix("midi") {
                        Some(midi) => note_name_for_midi(midi.parse().unwrap()),
                        None => note_label(input.parse().unwrap()),
                    };
                    assert_eq!(actual, label, "{input}");
                }
            }
            other => panic!("unknown case {other}"),
        }
        cases += 1;
    }
    assert!(cases >= 20, "only {cases} cases");
}
