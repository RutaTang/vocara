//! DSP node correctness — the golden-file/sanity surface for the four kernels.

mod common;
use common::*;

use vocara_engine::dsp::biquad::{BandKind, BandSpec, EqNode};
use vocara_engine::dsp::compressor::{param as cparam, CompressorNode};
use vocara_engine::dsp::gain_pan::{param as gparam, GainPanNode};
use vocara_engine::dsp::reverb::ReverbNode;
use vocara_engine::Node;

const SR: f64 = 48_000.0;

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
    assert!(max_err < 1e-4, "flat EQ should be ~identity, max err {max_err}");
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
    assert!(g < 0.2, "10k through a 1k LP should be heavily attenuated, got {g}");
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
    assert!(hi_gain > 3.0, "high-shelf should lift 10k ~4x, got {hi_gain}");
    assert!(lo_gain < 1.3, "high-shelf should barely touch 200 Hz, got {lo_gain}");
}

#[test]
fn compressor_reduces_loud_passes_quiet() {
    // Loud sine well above a -18 dB threshold gets reduced.
    let mut comp = CompressorNode::new(-18.0, 4.0, 6.0, 5.0, 80.0, 0.0);
    let loud = to_stereo(&sine_mono(220.0, SR, 16_000, 0.9));
    let out = run_node(&mut comp, &loud, SR, 256);
    let g = rms(&out[out.len() / 2..]) / rms(&loud[loud.len() / 2..]);
    assert!(g < 0.85, "loud signal should be compressed, ratio {g}");
    assert!(comp.gain_reduction_db() > 1.0, "GR meter should register reduction");

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
    assert!(max_err < 1e-6, "bypassed compressor must be identity, err {max_err}");
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
    assert!(early > 1e-5, "reverb should ring after the impulse, got {early}");
    // ...and decays over time.
    assert!(late < early, "reverb tail should decay: early {early} late {late}");
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
    // Gain 0.5, center. Equal-power (-3 dB) pan law: center => 0.707/channel, so a
    // mono-duplicated signal measures 0.5 * 0.7071 ≈ 0.3536 per channel.
    let mut g = GainPanNode::new(0.5, 0.0);
    let input = to_stereo(&sine_mono(440.0, SR, 4000, 0.8));
    let out = run_node(&mut g, &input, SR, 256);
    let ratio = rms(&out[out.len() / 2..]) / rms(&input[input.len() / 2..]);
    assert!((ratio - 0.3536).abs() < 0.02, "gain 0.5 * -3dB center ≈ 0.3536, got {ratio}");

    // Hard left: right channel silent. Pan ramps over ~10 ms (click-free), so measure the
    // settled tail (last 10%) rather than mid-buffer where the smoother is still moving.
    let mut gl = GainPanNode::new(1.0, 0.0);
    gl.set_param(gparam::PAN, -1.0);
    let out2 = run_node(&mut gl, &input, SR, 256);
    let r = rms(&right(&out2[out2.len() * 9 / 10..]));
    assert!(r < 1e-3, "hard-left pan should silence the right channel, got {r}");
}
