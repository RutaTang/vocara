//! Capture-path conditioning regressions: the path must be TIME-INVARIANT and LINEAR.
//!
//! An adaptive expander used to live here and its noise-floor estimate chased held notes — the
//! recorded "pumping" bug (onsets loud, sustains sagging by over 10 dB, snapping back after
//! every breath). A tanh soft-knee ceiling then baked distortion into loud notes. The product
//! call is a fully static, fully linear capture path: a 40 Hz second-order high-pass + a pure
//! linear gain, nothing else. These tests pin that: the same voice must come out at the same
//! level regardless of phrasing history, gaps must pass untouched, and the filter must spare
//! the low male voice (the reason the corner sits at 40 Hz, not the classic 75-80).

use vocara_engine::dsp::capture_cleanup::HighPass;

const SR: usize = 48_000;
const HISS_AMP: f32 = 0.002; // built-in-mic-like hiss (uniform white, +-amp)
const VOICE_AMP: f32 = 0.05; // quiet raw capture level the record gain is tuned for

/// Deterministic uniform noise in [-1, 1) (LCG; no external crates).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32 / (1u64 << 30) as f32) - 1.0
    }
}

/// A sung-vowel stand-in: 220 Hz with 5.5 Hz / 10% vibrato AM, hiss mixed in.
fn vowel(t: f64, rng: &mut Rng) -> f32 {
    let vib = 1.0 + 0.10 * ((2.0 * std::f64::consts::PI * 5.5 * t).sin() as f32);
    VOICE_AMP * vib * ((2.0 * std::f64::consts::PI * 220.0 * t).sin() as f32)
        + HISS_AMP * rng.next()
}

fn hiss(rng: &mut Rng) -> f32 {
    HISS_AMP * rng.next()
}

fn rms(buf: &[f32]) -> f64 {
    (buf.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / buf.len() as f64).sqrt()
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

fn seconds(s: f64) -> usize {
    (s * SR as f64) as usize
}

/// Run `input` through a fresh capture high-pass, returning the output.
fn run(input: &[f32]) -> Vec<f32> {
    let mut hp = HighPass::new();
    input.iter().map(|&x| hp.process(x)).collect()
}

/// Steady-state gain of the high-pass at `freq` (RMS over the settled tail of a pure tone).
fn tone_gain(freq: f64) -> f64 {
    let input: Vec<f32> = (0..seconds(2.0))
        .map(|i| {
            let t = i as f64 / SR as f64;
            0.1 * ((2.0 * std::f64::consts::PI * freq * t).sin() as f32)
        })
        .collect();
    let out = run(&input);
    rms(&out[seconds(0.5)..]) / rms(&input[seconds(0.5)..])
}

#[test]
fn capture_gain_is_time_invariant_across_phrasing() {
    // The pumping-bug regression: 1 s ambience, an 8 s held vowel, a breath, four staccato
    // notes, another sustain. Every voiced window — early sustain, late sustain, staccato,
    // second sustain — must come out at the same level (the old expander had them differing by
    // up to ~16 dB depending on what preceded them).
    let mut rng = Rng(0x1234_5678);
    let input: Vec<f32> = (0..seconds(16.0))
        .map(|i| {
            let t = i as f64 / SR as f64;
            if t < 1.0 {
                hiss(&mut rng)
            } else if t < 9.0 {
                vowel(t, &mut rng)
            } else if t < 9.4 {
                hiss(&mut rng)
            } else if t < 11.8 {
                let phase = (t - 9.4) % 0.6;
                if phase < 0.3 {
                    vowel(t, &mut rng)
                } else {
                    hiss(&mut rng)
                }
            } else {
                vowel(t, &mut rng)
            }
        })
        .collect();
    let out = run(&input);

    let windows = [
        ("sustain early", seconds(1.2), seconds(2.2)),
        ("sustain late", seconds(8.0), seconds(9.0)),
        ("staccato note", seconds(10.05), seconds(10.25)),
        ("second sustain", seconds(15.0), seconds(16.0)),
    ];
    let reference = rms(&out[windows[0].1..windows[0].2]) / rms(&input[windows[0].1..windows[0].2]);
    for (name, a, b) in windows {
        let gain = rms(&out[a..b]) / rms(&input[a..b]);
        let diff_db = db(gain / reference);
        assert!(
            diff_db.abs() < 0.5,
            "{name}: capture gain must not depend on phrasing history, differs by {diff_db:.2} dB"
        );
    }
}

#[test]
fn quiet_gaps_pass_untouched() {
    // No expander/gate: the hiss between phrases is NOT attenuated (the accepted tradeoff for a
    // transparent record path). Broadband hiss loses only its sub-40 Hz sliver to the high-pass.
    let mut rng = Rng(0xD15C_0BA1);
    let input: Vec<f32> = (0..seconds(6.0))
        .map(|i| {
            let t = i as f64 / SR as f64;
            if t < 0.5 {
                hiss(&mut rng)
            } else if t < 3.0 {
                vowel(t, &mut rng)
            } else {
                hiss(&mut rng)
            }
        })
        .collect();
    let out = run(&input);

    let in_gap = rms(&input[seconds(5.0)..seconds(6.0)]);
    let out_gap = rms(&out[seconds(5.0)..seconds(6.0)]);
    let ratio = out_gap / in_gap;
    assert!(
        (0.95..=1.02).contains(&ratio),
        "gap must pass essentially unchanged (static path), got out/in {ratio:.3}"
    );
}

#[test]
fn low_male_voice_is_spared() {
    // The reason the corner is 40 Hz: a low male fundamental must pass essentially untouched.
    // E2 = 82 Hz (a bass's low note) loses under 0.5 dB; A2 = 110 Hz under 0.2 dB — the old
    // one-pole 80 Hz corner lost ~0.9 dB and ~1.8 dB there and biased H1-H2 audibly.
    let e2 = db(tone_gain(82.0));
    assert!(e2 > -0.5, "E2 (82 Hz) must lose < 0.5 dB, got {e2:.2} dB");
    let a2 = db(tone_gain(110.0));
    assert!(a2 > -0.2, "A2 (110 Hz) must lose < 0.2 dB, got {a2:.2} dB");
    let a3 = db(tone_gain(220.0));
    assert!(a3 > -0.1, "A3 (220 Hz) must be untouched, got {a3:.2} dB");
}

#[test]
fn rumble_and_dc_are_removed() {
    // 30 Hz rumble (desk thump / room LF) is attenuated at least as hard as the old 80 Hz
    // one-pole managed (~-4 dB), and a DC offset is fully blocked.
    let g30 = db(tone_gain(30.0));
    assert!(
        g30 < -5.0,
        "30 Hz rumble should be attenuated, got {g30:.2} dB"
    );

    let dc: Vec<f32> = vec![0.3; seconds(1.0)];
    let out_dc = run(&dc);
    let tail = rms(&out_dc[seconds(0.5)..]);
    assert!(tail < 1e-3, "DC must be blocked, residual RMS {tail}");
}

#[test]
fn path_is_linear_no_baked_ceiling() {
    // The tanh soft knee is gone: scaling the input must scale the output EXACTLY (a float
    // recording stores over-0 dBFS peaks losslessly, so nothing may compress them on the way).
    let tone: Vec<f32> = (0..seconds(1.0))
        .map(|i| {
            let t = i as f64 / SR as f64;
            ((2.0 * std::f64::consts::PI * 220.0 * t).sin() as f32) * 0.02
        })
        .collect();
    let loud: Vec<f32> = tone.iter().map(|&x| x * 50.0).collect(); // peaks at 1.0
    let quiet_out = rms(&run(&tone)[seconds(0.25)..]);
    let loud_out = rms(&run(&loud)[seconds(0.25)..]);
    let ratio = loud_out / quiet_out;
    assert!(
        (ratio - 50.0).abs() < 0.05,
        "path must be exactly linear (x50 in -> x50 out), got x{ratio:.3}"
    );
}

#[test]
fn output_is_always_finite() {
    let mut rng = Rng(0xA11F_1717);
    let input: Vec<f32> = (0..seconds(3.0))
        .map(|i| {
            let t = i as f64 / SR as f64;
            if t < 0.5 {
                0.0
            } else if t < 2.0 {
                vowel(t, &mut rng)
            } else {
                0.0
            }
        })
        .collect();
    let out = run(&input);
    assert!(out.iter().all(|x| x.is_finite()));
}
