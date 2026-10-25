//! DSP node correctness — the golden-file/sanity surface for the four kernels.

mod common;
use common::*;

use vocara_engine::dsp::biquad::{BandKind, BandSpec, EqNode};
use vocara_engine::dsp::compressor::{param as cparam, CompressorNode};
use vocara_engine::dsp::delay::{param as dlparam, DelayNode};
use vocara_engine::dsp::doubler::{param as dbparam, DoublerNode};
use vocara_engine::dsp::gain_pan::{param as gparam, GainPanNode};
use vocara_engine::dsp::reverb::ReverbNode;
use vocara_engine::{Node, ProcessIo, CHANNELS};

const SR: f64 = 48_000.0;

/// Drive an already-prepared node over one phase of interleaved-stereo input WITHOUT
/// re-preparing (`run_node` calls `prepare`, which reallocates the delay lines and would wipe
/// exactly the cross-phase ring state the bypass-ghost tests below must observe).
fn run_phase(node: &mut dyn Node, input: &[f32], sr: f64, block: usize) -> Vec<f32> {
    let total_frames = input.len() / CHANNELS;
    let mut out = vec![0.0; input.len()];
    let mut f = 0;
    while f < total_frames {
        let n = block.min(total_frames - f);
        let in_slice = &input[f * CHANNELS..(f + n) * CHANNELS];
        let mut out_buf = vec![0.0; n * CHANNELS];
        {
            let inputs: [&[f32]; 1] = [in_slice];
            let mut outs: [&mut [f32]; 1] = [&mut out_buf[..]];
            let mut io = ProcessIo {
                inputs: &inputs,
                outputs: &mut outs[..],
                frames: n,
                tl: f as i64,
                sr,
            };
            node.process(&mut io);
        }
        out[f * CHANNELS..(f + n) * CHANNELS].copy_from_slice(&out_buf);
        f += n;
    }
    out
}

#[test]
fn eq_flat_is_identity() {
    // A peaking/shelf band at 0 dB has numerator == denominator → exact pass-through.
    let mut eq = EqNode::default_strip();
    let input = to_stereo(&sine_mono(1000.0, SR, 4096, 0.5));
    let out = run_node(&mut eq, &input, SR, 256);
    let max_err = input
        .iter()
        .zip(out.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    // Mathematically identity (numerator == denominator at 0 dB); ~1e-5 f32 rounding
    // across the 4-band cascade is inaudible (< -90 dB).
    assert!(
        max_err < 1e-4,
        "flat EQ should be ~identity, max err {max_err}"
    );
}

#[test]
fn eq_peak_boosts_at_center() {
    let mut eq = EqNode::new(&[BandSpec::peaking(1000.0, 1.0, 12.0)]);
    let input = to_stereo(&sine_mono(1000.0, SR, 8192, 0.3));
    let out = run_node(&mut eq, &input, SR, 256);
    // Measure the settled tail to skip the filter's onset transient.
    let tail = &out[out.len() / 2..];
    let in_tail = &input[input.len() / 2..];
    let gain = rms(tail) / rms(in_tail);
    // +12 dB ≈ 4x; allow generous slack for Q/leakage.
    assert!(gain > 2.5, "peak should boost ~4x at center, got {gain}");
}

#[test]
fn eq_lowpass_attenuates_highs() {
    let mut eq = EqNode::new(&[BandSpec {
        kind: BandKind::LowPass,
        freq: 1000.0,
        q: 0.707,
        gain_db: 0.0,
        enabled: true,
    }]);
    let input = to_stereo(&sine_mono(10_000.0, SR, 8192, 0.5));
    let out = run_node(&mut eq, &input, SR, 256);
    let g = rms(&out[out.len() / 2..]) / rms(&input[input.len() / 2..]);
    assert!(
        g < 0.2,
        "10k through a 1k LP should be heavily attenuated, got {g}"
    );
    assert!(all_finite(&out));
}

#[test]
fn eq_high_shelf_boosts_highs_not_lows() {
    // +12 dB high-shelf at 4 kHz: a 10 kHz tone should rise ~4x, a 200 Hz tone barely move.
    let shelf = || {
        EqNode::new(&[BandSpec {
            kind: BandKind::HighShelf,
            freq: 4000.0,
            q: 0.707,
            gain_db: 12.0,
            enabled: true,
        }])
    };
    let hi = to_stereo(&sine_mono(10_000.0, SR, 8192, 0.3));
    let lo = to_stereo(&sine_mono(200.0, SR, 8192, 0.3));
    let hi_out = run_node(&mut shelf(), &hi, SR, 256);
    let lo_out = run_node(&mut shelf(), &lo, SR, 256);
    let hi_gain = rms(&hi_out[hi_out.len() / 2..]) / rms(&hi[hi.len() / 2..]);
    let lo_gain = rms(&lo_out[lo_out.len() / 2..]) / rms(&lo[lo.len() / 2..]);
    assert!(
        hi_gain > 3.0,
        "high-shelf should lift 10k ~4x, got {hi_gain}"
    );
    assert!(
        lo_gain < 1.3,
        "high-shelf should barely touch 200 Hz, got {lo_gain}"
    );
}

#[test]
fn compressor_reduces_loud_passes_quiet() {
    // Loud sine well above a -18 dB threshold gets reduced.
    let mut comp = CompressorNode::new(-18.0, 4.0, 6.0, 5.0, 80.0, 0.0);
    let loud = to_stereo(&sine_mono(220.0, SR, 16_000, 0.9));
    let out = run_node(&mut comp, &loud, SR, 256);
    let g = rms(&out[out.len() / 2..]) / rms(&loud[loud.len() / 2..]);
    assert!(g < 0.85, "loud signal should be compressed, ratio {g}");
    assert!(
        comp.gain_reduction_db() > 1.0,
        "GR meter should register reduction"
    );

    // Quiet sine below threshold passes ~unchanged.
    let mut comp2 = CompressorNode::new(-18.0, 4.0, 6.0, 5.0, 80.0, 0.0);
    let quiet = to_stereo(&sine_mono(220.0, SR, 16_000, 0.02)); // ~-34 dB
    let out2 = run_node(&mut comp2, &quiet, SR, 256);
    let g2 = rms(&out2[out2.len() / 2..]) / rms(&quiet[quiet.len() / 2..]);
    assert!(g2 > 0.95, "quiet signal should pass ~unity, ratio {g2}");
}

#[test]
fn compressor_makeup_adds_gain() {
    let mut comp = CompressorNode::new(-60.0, 1.0, 0.0, 1.0, 50.0, 6.0); // ratio 1 = no compression, +6 dB makeup
    let input = to_stereo(&sine_mono(220.0, SR, 8000, 0.1));
    let out = run_node(&mut comp, &input, SR, 256);
    let g = rms(&out[out.len() / 2..]) / rms(&input[input.len() / 2..]);
    assert!((g - 2.0).abs() < 0.2, "+6 dB makeup ≈ 2x, got {g}");
}

#[test]
fn compressor_bypass_is_identity() {
    let mut comp = CompressorNode::new(-18.0, 4.0, 6.0, 5.0, 80.0, 12.0);
    comp.set_param(cparam::BYPASS, 1.0);
    let input = to_stereo(&sine_mono(220.0, SR, 4000, 0.9));
    let out = run_node(&mut comp, &input, SR, 256);
    let max_err = input
        .iter()
        .zip(out.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_err < 1e-6,
        "bypassed compressor must be identity, err {max_err}"
    );
}

#[test]
fn reverb_impulse_produces_decaying_finite_tail() {
    let mut rev = ReverbNode::default_bus();
    let frames = 48_000; // 1 s
    let mut input = vec![0.0f32; frames * 2];
    input[0] = 1.0;
    input[1] = 1.0;
    let out = run_node(&mut rev, &input, SR, 256);

    assert!(all_finite(&out), "reverb must never produce NaN/inf");
    // Tail energy well after the impulse is present...
    let early = rms(&out[200..20_000]);
    let late = rms(&out[80_000..96_000]);
    assert!(
        early > 1e-5,
        "reverb should ring after the impulse, got {early}"
    );
    // ...and decays over time.
    assert!(
        late < early,
        "reverb tail should decay: early {early} late {late}"
    );
}

#[test]
fn reverb_silence_in_silence_out_no_denormal_blowup() {
    let mut rev = ReverbNode::default_bus();
    let input = vec![0.0f32; 48_000 * 2];
    let out = run_node(&mut rev, &input, SR, 512);
    assert!(all_finite(&out));
    assert!(rms(&out) < 1e-6, "silence in must give silence out");
}

#[test]
fn gain_pan_scales_and_pans() {
    // Gain 0.5, center. Attenuate-only balance law: center is UNITY on both channels, so a
    // mono-duplicated signal measures exactly the 0.5 gain per channel. (Center unity is
    // load-bearing for the graph: one pan node per routing hop must not stack -3 dB per hop,
    // and the un-panned shared reverb return must level-match the panned dry path.)
    let mut g = GainPanNode::new(0.5, 0.0);
    let input = to_stereo(&sine_mono(440.0, SR, 4000, 0.8));
    let out = run_node(&mut g, &input, SR, 256);
    let ratio = rms(&out[out.len() / 2..]) / rms(&input[input.len() / 2..]);
    assert!(
        (ratio - 0.5).abs() < 0.02,
        "gain 0.5 at center-unity pan ≈ 0.5, got {ratio}"
    );

    // Hard left: right channel silent, left stays at unity (attenuate-only: the kept side is
    // never cut). Pan ramps over ~10 ms (click-free), so measure the settled tail (last 10%)
    // rather than mid-buffer where the smoother is still moving.
    let mut gl = GainPanNode::new(1.0, 0.0);
    gl.set_param(gparam::PAN, -1.0);
    let out2 = run_node(&mut gl, &input, SR, 256);
    let tail = &out2[out2.len() * 9 / 10..];
    let in_tail = &input[input.len() * 9 / 10..];
    let r = rms(&right(tail));
    assert!(
        r < 1e-3,
        "hard-left pan should silence the right channel, got {r}"
    );
    let l_ratio = rms(&left(tail)) / rms(&left(in_tail));
    assert!(
        (l_ratio - 1.0).abs() < 0.02,
        "hard-left pan keeps the left channel at unity, got {l_ratio}"
    );
}

/// Toggling the delay to bypass and back must not replay pre-bypass ring audio as a one-shot
/// ghost echo. The bypassed process() progressively zeroes the ring (bounded per-block work —
/// set_param runs on the audio thread, so it cannot memset the ~2 s line itself); after one
/// full line length of bypassed frames the ring is clean.
#[test]
fn delay_bypass_toggle_does_not_replay_stale_tail() {
    let block = 256usize;
    let mut d = DelayNode::new();
    d.prepare(SR, block);
    d.set_param(dlparam::BYPASS, 0.0); // active (the node constructs bypassed)
    d.set_param(dlparam::TIME_MS, 50.0);
    d.set_param(dlparam::FEEDBACK, 0.0);
    d.set_param(dlparam::MIX, 1.0); // full wet: any stale ring content is fully audible

    // Phase A (active): fill the ring with a loud tone.
    let tone = to_stereo(&sine_mono(440.0, SR, 24_000, 0.8));
    run_phase(&mut d, &tone, SR, block);

    // Phase B (bypassed): dry passthrough must stay bit-exact while the ring is being
    // cleared. One full line length (2 s at 48 kHz) of bypassed frames cleans it entirely.
    d.set_param(dlparam::BYPASS, 1.0);
    let dry = to_stereo(&sine_mono(440.0, SR, 96_000, 0.8));
    let out_b = run_phase(&mut d, &dry, SR, block);
    let max_err = dry
        .iter()
        .zip(out_b.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_err < 1e-6,
        "bypassed delay must pass the dry input through, err {max_err}"
    );

    // Phase C (re-activated, silent input): the wet path must be silent. Before the fix the
    // ring still held phase A's tone and replayed ~TIME_MS of it as a ghost echo.
    d.set_param(dlparam::BYPASS, 0.0);
    let out_c = run_phase(&mut d, &vec![0.0f32; 24_000 * CHANNELS], SR, block);
    let ghost = rms(&out_c);
    assert!(
        ghost < 1e-6,
        "stale pre-bypass audio replayed as a ghost echo, rms={ghost}"
    );
}

/// One NaN input sample must not permanently poison the delay's feedback ring: the write
/// `buf[pos] = in + delayed*fb` is sanitized (like reverb/biquad/compressor/gate), so the
/// output recovers to finite once the poisoned frame has passed. Unsanitized, NaN*fb was
/// re-injected into the ring every TIME_MS for the rest of the session.
#[test]
fn delay_recovers_after_a_nan_input() {
    let block = 256usize;
    let mut d = DelayNode::new();
    d.prepare(SR, block);
    d.set_param(dlparam::BYPASS, 0.0); // active (the node constructs bypassed)
    d.set_param(dlparam::TIME_MS, 50.0); // 2400-frame line @ 48 kHz
    d.set_param(dlparam::FEEDBACK, 0.5);
    d.set_param(dlparam::MIX, 0.5);

    // 1 s of tone with a single NaN frame near the start.
    let mut input = to_stereo(&sine_mono(440.0, SR, 48_000, 0.5));
    input[1000 * CHANNELS] = f32::NAN;
    input[1000 * CHANNELS + 1] = f32::NAN;
    let out = run_phase(&mut d, &input, SR, block);

    // Sanity: the wet path is live (out != dry input), so the ring is actually exercised —
    // a bypassed delay would pass this test trivially.
    let wet_diff = input[100 * CHANNELS..200 * CHANNELS]
        .iter()
        .zip(&out[100 * CHANNELS..200 * CHANNELS])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        wet_diff > 1e-3,
        "the wet path must be active for this test to be meaningful"
    );

    // The NaN input frame itself is unavoidably NaN (the dry path passes it through), but
    // every later frame must be finite. Unsanitized, the poisoned ring slot re-emerged at
    // frame 1000 + 2400 (and every 2400 frames after that, forever).
    assert!(
        all_finite(&out[1001 * CHANNELS..]),
        "delay output must recover to finite after a single NaN input"
    );
}

/// Same ghost-echo guard for the doubler's modulated delay lines (its bypass branch mirrors
/// the delay's progressive clear).
#[test]
fn doubler_bypass_toggle_does_not_replay_stale_tail() {
    let block = 256usize;
    let mut db = DoublerNode::new();
    db.prepare(SR, block);
    db.set_param(dbparam::BYPASS, 0.0); // active (the node constructs bypassed)
    db.set_param(dbparam::DELAY_MS, 30.0);
    db.set_param(dbparam::DEPTH_MS, 0.0); // fixed read tap: the ghost window is deterministic
    db.set_param(dbparam::MIX, 1.0);

    // Phase A (active): fill the lines with a loud tone.
    let tone = to_stereo(&sine_mono(440.0, SR, 12_000, 0.8));
    run_phase(&mut db, &tone, SR, block);

    // Phase B (bypassed): one full line length (0.1 s => 4800 frames) clears the lines.
    db.set_param(dbparam::BYPASS, 1.0);
    run_phase(&mut db, &vec![0.0f32; 4_800 * CHANNELS], SR, block);

    // Phase C (re-activated, silent input): the wet copy must be silent, not phase A's tone.
    db.set_param(dbparam::BYPASS, 0.0);
    let out_c = run_phase(&mut db, &vec![0.0f32; 9_600 * CHANNELS], SR, block);
    let ghost = rms(&out_c);
    assert!(
        ghost < 1e-6,
        "stale pre-bypass audio replayed as a ghost, rms={ghost}"
    );
}

#[test]
fn an_eq_band_edit_sent_field_by_field_still_crossfades() {
    // ve_set_eq_band sends FREQ, Q, GAIN_DB and ENABLED as four commands the
    // audio thread applies back to back before the block renders. Each later
    // field used to commit the fade the previous one had just started, so the
    // band swapped coefficients instantly (a click on every knob drag). The
    // burst must sound exactly like one set_band call.
    use vocara_engine::dsp::biquad::param as eqparam;
    let block = 256;
    let input = to_stereo(&sine_mono(700.0, SR, 20 * block, 0.5));
    let mut burst = EqNode::default_strip();
    let mut single = EqNode::default_strip();
    burst.prepare(SR, block);
    single.prepare(SR, block);
    let (pre, post) = input.split_at(10 * block * CHANNELS);
    let a0 = run_phase(&mut burst, pre, SR, block);
    let b0 = run_phase(&mut single, pre, SR, block);
    assert_eq!(a0, b0);
    let band = 2u16 << eqparam::FIELD_BITS; // the 700 Hz peaking band
    burst.set_param(band | eqparam::FREQ, 700.0);
    burst.set_param(band | eqparam::Q, 1.0);
    burst.set_param(band | eqparam::GAIN_DB, 12.0);
    burst.set_param(band | eqparam::ENABLED, 1.0);
    single.set_band(2, BandSpec::peaking(700.0, 1.0, 12.0));
    let a = run_phase(&mut burst, post, SR, block);
    let b = run_phase(&mut single, post, SR, block);
    let max_diff = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff < 1e-6,
        "the field-by-field edit must cross-fade like a single one, diff {max_diff}"
    );
}

#[test]
fn re_enabling_a_compressor_does_not_duck_with_a_stale_envelope() {
    // Bypass during a loud passage, re-enable on a quiet one: the envelope
    // froze at the loud passage's reduction, and the quiet audio was ducked
    // by it until the release recovered.
    let block = 256;
    let mut comp = CompressorNode::new(-18.0, 4.0, 6.0, 5.0, 200.0, 0.0);
    comp.prepare(SR, block);
    let loud = to_stereo(&sine_mono(220.0, SR, 24_000, 0.9));
    let _ = run_phase(&mut comp, &loud, SR, block);
    assert!(
        comp.gain_reduction_db() > 6.0,
        "the loud passage compresses"
    );
    comp.set_param(cparam::BYPASS, 1.0);
    let quiet = to_stereo(&sine_mono(220.0, SR, 4 * block, 0.02)); // ~-34 dB
    let _ = run_phase(&mut comp, &quiet, SR, block);
    comp.set_param(cparam::BYPASS, 0.0);
    let out = run_phase(&mut comp, &quiet, SR, block);
    let g = rms(&out[..block * CHANNELS]) / rms(&quiet[..block * CHANNELS]);
    assert!(
        g > 0.95,
        "below-threshold audio must pass at ~unity right after re-enabling, got {g}"
    );
}

/// Steady-state level change (dB) of a `freq` sine of amplitude `amp` through a fresh
/// de-esser, measured over the settled second half.
fn deesser_gain_db(freq: f64, amp: f32, threshold_db: f32, range_db: f32) -> f32 {
    use vocara_engine::dsp::deesser::{param as deparam, DeEsserNode};
    let block = 256;
    let mut de = DeEsserNode::new();
    de.prepare(SR, block);
    de.set_param(deparam::FREQ, 6500.0);
    de.set_param(deparam::THRESHOLD_DB, threshold_db);
    de.set_param(deparam::RANGE_DB, range_db);
    de.set_param(deparam::BYPASS, 0.0);
    let input = to_stereo(&sine_mono(freq, SR, 24_000, amp));
    let out = run_phase(&mut de, &input, SR, block);
    let h = input.len() / 2;
    20.0 * (rms(&out[h..]) / rms(&input[h..])).log10()
}

#[test]
fn a_de_esser_reduction_never_boosts_any_frequency() {
    // The high band used to be split off as `x - HP2(x)`: the 2nd-order high-pass's phase
    // shift made that "low band" peak around the crossover, so cutting the high band BOOSTED
    // 3–7 kHz by up to ~2 dB. At full reduction (every tone far over the threshold), no
    // frequency may come out louder than it went in; below the threshold the response is
    // flat in magnitude (the crossover is allowed an allpass phase).
    let mut f = 1000.0;
    while f <= 16_000.0 {
        let cut = deesser_gain_db(f, 0.3, -120.0, 24.0);
        assert!(
            cut <= 0.1,
            "full reduction boosted {f:.0} Hz by {cut:+.2} dB"
        );
        let flat = deesser_gain_db(f, 0.01, 0.0, 24.0);
        assert!(
            flat.abs() <= 0.1,
            "below threshold {f:.0} Hz moved {flat:+.2} dB"
        );
        f *= 1.19;
    }
}

#[test]
fn a_de_esser_cuts_a_loud_sibilant_by_most_of_its_range() {
    // A loud "ess" above the 6.5 kHz crossover (the default/preset settings) must actually
    // come down. Before the phase-coherent split a 10 kHz tone lost only 2 dB of a 12 dB
    // range and a 7 kHz one came out LOUDER.
    let at_10k = deesser_gain_db(10_000.0, 0.3, -28.0, 12.0);
    assert!(at_10k <= -8.0, "10 kHz sibilant only moved {at_10k:+.2} dB");
    let at_7k = deesser_gain_db(7_000.0, 0.3, -28.0, 12.0);
    assert!(at_7k <= -4.0, "7 kHz sibilant only moved {at_7k:+.2} dB");
}

/// Amplitude of the `k`-th harmonic of `f0` in a mono signal spanning a whole number of
/// periods (plain DFT correlation; no window needed).
fn harmonic_amp(x: &[f32], f0: f64, k: usize) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f0 * k as f64 / SR;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &s) in x.iter().enumerate() {
        re += s as f64 * (w * i as f64).cos();
        im += s as f64 * (w * i as f64).sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

#[test]
fn the_master_safety_clip_leaves_normal_levels_untouched() {
    // The master's soft clip ran every sample through `K*(u - u^3/3)`, which has no linear
    // region: a -6 dBFS mix picked up ~1% THD and lost 0.3 dB, and a mastered backing
    // track at -0.1 dBFS ~4% — on every playback, with nothing actually clipping.
    use vocara_engine::dsp::master::MasterNode;
    let amp = 10f32.powf(-6.0 / 20.0);
    let input = to_stereo(&sine_mono(1000.0, SR, 4800, amp)); // 100 whole periods
    let out = run_node(&mut MasterNode::new(1.0, true), &input, SR, 256);
    let l = left(&out);
    let fund = harmonic_amp(&l, 1000.0, 1);
    let harm: f64 = (2..=10)
        .map(|k| harmonic_amp(&l, 1000.0, k).powi(2))
        .sum::<f64>()
        .sqrt();
    let thd = 100.0 * harm / fund;
    assert!(thd < 0.05, "-6 dBFS through the master clip: THD {thd:.3}%");
    let level_db = 20.0 * (fund / amp as f64).log10();
    assert!(
        level_db.abs() < 0.05,
        "-6 dBFS level moved {level_db:+.3} dB"
    );

    // A hot input is still held at the ceiling.
    let hot = to_stereo(&sine_mono(1000.0, SR, 4800, 10f32.powf(12.0 / 20.0)));
    let out = run_node(&mut MasterNode::new(1.0, true), &hot, SR, 256);
    let peak = out.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    assert!(peak <= 1.0, "+12 dBFS escaped the ceiling: {peak}");
}

#[test]
fn saturation_matches_its_waveshaper_formula() {
    // Pins the output to `dry*(1-mix) + tanh(d*x)/tanh(d)*mix` (d = drive as a linear gain)
    // across drives, so hoisting the per-block constants out of the sample loop cannot
    // change what the user hears.
    use vocara_engine::dsp::saturation::{param as sparam, SaturationNode};
    let input = to_stereo(&sine_mono(220.0, SR, 4096, 0.6));
    for &(drive_db, mix) in &[(0.0f32, 1.0f32), (9.0, 0.3), (20.0, 0.65), (36.0, 1.0)] {
        let mut sat = SaturationNode::new();
        sat.prepare(SR, 256);
        sat.set_param(sparam::DRIVE_DB, drive_db);
        sat.set_param(sparam::MIX, mix);
        sat.set_param(sparam::BYPASS, 0.0);
        let out = run_phase(&mut sat, &input, SR, 256);
        let d = 10f32.powf(drive_db / 20.0).max(1.0);
        let max_err = input
            .iter()
            .zip(&out)
            .map(|(&x, &y)| (x * (1.0 - mix) + (d * x).tanh() / d.tanh() * mix - y).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_err < 1e-6,
            "drive {drive_db} dB mix {mix}: err {max_err}"
        );
    }
}
