//! Regression tests for the AEC path-change deadlock breaker (`aec.rs`).
//!
//! A sudden loudspeaker/room gain INCREASE leaves the AEC's tracked path model (gxx) stale
//! and under-predicting the now-louder echo. The shaped near-end test then trips every block
//! (any `nearend_ratio` above ~(NEAREND_TRIP+1)/2 = 2 does), re-arms the double-talk hangover
//! indefinitely, and freezes adaptation PERMANENTLY: gxx only refreshes under far-only
//! evidence, which requires `hang == 0`. The breaker must detect that pinned-hangover +
//! high-coherence signature and re-seed the model.
//!
//! The ORIGINAL breaker armed only on `nearend_ratio > NEAREND_TRIP` (3.0) with a
//! reset-to-zero-on-miss debounce, so:
//!   * a jump whose post-jump ratio lands in the (2, 3] band froze forever and could NEVER
//!     arm the breaker (the dead band), and
//!   * on non-stationary material a single below-trip block reset the 96-consecutive-blocks
//!     debounce, starving the breaker even above the trip.
//!
//! These tests pin the reworked arming (hangover held + mic coherent with the echo estimate,
//! drain-on-miss debounce): the deadlock must engage, the breaker must fire, and ERLE must
//! recover — for a power jump INSIDE the old dead band (2.5x) and above it (4x) — while
//! genuine double-talk must never fire it.
//!
//! The far-end is a MULTITONE with one tone per (bin-aligned) FFT bin: per-bin block powers
//! are then stable, so the per-bin gxx ratio tracking is unbiased and the post-jump
//! nearend_ratio sits predictably at ~jump/1.2 (one far-only gxx step slips in before the
//! hangover pins). White noise would NOT reproduce the deadlock: its per-bin power jitter
//! biases gxx high (mean-of-ratios), the ratio never reaches the trip band, and the filter
//! just re-adapts — verified while building this harness.

use vocara_engine::aec::Aec;

/// Mirrors the AEC's internal constants (aec.rs `BLOCK`/`FFT_N`): one block of algorithmic
/// latency; bins spaced sr/FFT_N apart.
const BLOCK: usize = 256;
const FFT_N: usize = 2 * BLOCK;

/// Deterministic white-ish noise via xorshift32 (no rand crate; mirrors aec.rs's unit tests).
struct Rng(u32);
impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// A plausible room/speaker impulse response: a bulk delay, a few discrete reflections, and
/// an exponentially-decaying diffuse tail (mirrors aec.rs's unit-test path).
fn make_echo_path(len: usize, bulk_delay: usize) -> Vec<f32> {
    let mut h = vec![0.0f32; len];
    let mut rng = Rng::new(0xC0FFEE);
    for (i, tap) in h.iter_mut().enumerate().skip(bulk_delay) {
        let t = (i - bulk_delay) as f32;
        let decay = (-t / 1500.0).exp();
        *tap = 0.15 * decay * rng.next_f32();
    }
    h[bulk_delay] = 0.5;
    h[bulk_delay + 80] = -0.3;
    h[bulk_delay + 200] = 0.2;
    h
}

fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0f32; x.len()];
    for n in 0..x.len() {
        let mut acc = 0.0f32;
        let kmax = h.len().min(n + 1);
        for k in 0..kmax {
            acc += h[k] * x[n - k];
        }
        y[n] = acc;
    }
    y
}

/// Backing-track stand-in: 20 tones on distinct bin-aligned frequencies (multiples of the
/// FFT bin spacing => stable per-bin block powers), fixed pseudo-random phases.
fn multitone_far(total: usize, sr: f32) -> Vec<f32> {
    let bin_hz = sr / FFT_N as f32;
    let mut rng = Rng::new(0xBEEF);
    let tones: Vec<(f32, f32)> = (1..=20)
        .map(|i| {
            (
                bin_hz * (i * 3) as f32,
                rng.next_f32() * std::f32::consts::PI,
            )
        })
        .collect();
    (0..total)
        .map(|n| {
            let t = n as f32 / sr;
            tones
                .iter()
                .map(|&(f, ph)| 0.06 * (2.0 * std::f32::consts::PI * f * t + ph).sin())
                .sum()
        })
        .collect()
}

/// Drive the AEC over `[lo, hi)` in non-block-aligned chunks (exercises the ring buffering).
fn run_range(aec: &mut Aec, mic: &[f32], far: &[f32], out: &mut [f32], lo: usize, hi: usize) {
    let chunk = 200usize;
    let mut i = lo;
    while i < hi {
        let end = (i + chunk).min(hi);
        aec.process(&mic[i..end], &far[i..end], &mut out[i..end]);
        i = end;
    }
}

/// ERLE (dB) of `out` against the raw mic over `[lo, hi)`, latency-aligned (out[n] cancels
/// mic[n - BLOCK]).
fn erle_db(mic: &[f32], out: &[f32], lo: usize, hi: usize) -> f64 {
    let lat = BLOCK;
    let mut mic_e = 0.0f64;
    let mut out_e = 0.0f64;
    for n in lo..hi {
        mic_e += (mic[n - lat] as f64).powi(2);
        out_e += (out[n] as f64).powi(2);
    }
    10.0 * (mic_e / (out_e + 1e-12)).log10()
}

/// Converge on the multitone through a synthetic echo path, then multiply the echo-path gain
/// by `power_jump` (in POWER; post-jump `nearend_ratio` reads ~power_jump/1.2). Asserts, in
/// order: no false fire during convergence; the deadlock genuinely ENGAGES (ERLE stuck at the
/// frozen-filter level right after the jump); the breaker fires within ~1 s; and ERLE
/// recovers to a sane converged level within 1.5 s of the jump (measured over the following
/// second). `max_frozen_erle_db` is just above the stuck-filter ERLE 10*log10(J/(sqrt(J)-1)^2)
/// for the given jump — if the freeze no longer engages, that assert flags the scenario as
/// stale rather than letting the test pass vacuously.
fn run_path_jump(power_jump: f32, max_frozen_erle_db: f64) {
    let sr = 48_000.0f32;
    let mut aec = Aec::new(sr, 120.0);
    let amp_jump = power_jump.sqrt();

    let conv_len = BLOCK * 282; // ~1.5 s far-only convergence
    let post_len = BLOCK * 470; // ~2.5 s after the jump
    let total = conv_len + post_len;

    let far = multitone_far(total, sr);
    let h = make_echo_path(600, 30);
    let echo = convolve(&far, &h);
    // Echo-path GAIN JUMP at conv_len: identical path shape, louder by amp_jump. The stale
    // gxx model then under-predicts by ~power_jump, the shaped near-end test pins the
    // hangover, and only the breaker can recover.
    let mic: Vec<f32> = (0..total)
        .map(|n| {
            if n < conv_len {
                echo[n]
            } else {
                amp_jump * echo[n]
            }
        })
        .collect();

    let mut out = vec![0.0f32; total];

    // Phase 1: convergence. Plain far-only material must never fire the breaker.
    run_range(&mut aec, &mic, &far, &mut out, 0, conv_len);
    assert_eq!(
        aec.path_change_fires(),
        0,
        "breaker must not fire on ordinary far-only convergence (jump {power_jump}x)"
    );
    let erle_before = erle_db(&mic, &out, conv_len - BLOCK * 94, conv_len);
    assert!(
        erle_before >= 15.0,
        "harness sanity: pre-jump ERLE too low: {erle_before:.2} dB (jump {power_jump}x)"
    );

    // Phase 2a: the freeze must ENGAGE (this is what makes the scenario a deadlock at all):
    // right after the jump the gate is pinned to 0, so ERLE is stuck at the frozen-filter
    // level. Measured over blocks ~16..88 post-jump — inside the freeze, before the breaker
    // can have fired (PATH_CHANGE_HOLD = 96 blocks).
    let detect = conv_len + BLOCK * 188; // ~1 s after the jump
    run_range(&mut aec, &mic, &far, &mut out, conv_len, detect);
    let erle_frozen = erle_db(&mic, &out, conv_len + BLOCK * 16, conv_len + BLOCK * 88);
    assert!(
        erle_frozen <= max_frozen_erle_db,
        "deadlock did not engage after a {power_jump}x power jump: frozen-window ERLE \
         {erle_frozen:.2} dB > {max_frozen_erle_db:.1} dB (test scenario is stale)"
    );

    // Phase 2b: ~1 s after the jump the breaker must already have fired (bounded detection:
    // the signature accumulates to PATH_CHANGE_HOLD = 96 blocks in ~0.55 s here).
    assert!(
        aec.path_change_fires() >= 1,
        "path-change breaker never fired within ~1 s of a {power_jump}x power jump \
         (the frozen path model deadlock was not broken)"
    );

    // Phase 2c: after 1.5 s of recovery, ERLE over the following second must be back to a
    // sane converged level.
    run_range(&mut aec, &mic, &far, &mut out, detect, total);
    let erle_after = erle_db(&mic, &out, conv_len + BLOCK * 282, total);
    assert!(
        erle_after >= 12.0,
        "ERLE did not recover after a {power_jump}x power jump: {erle_after:.2} dB \
         (breaker fired {} time(s) but re-convergence failed)",
        aec.path_change_fires()
    );

    for &v in &out {
        assert!(v.is_finite(), "non-finite output after the path jump");
    }
}

/// A 2.5x POWER jump lands the post-jump nearend_ratio at ~2.05 — INSIDE the old (2, 3] dead
/// band where the original `nearend_ratio > NEAREND_TRIP` arming could never fire and the
/// freeze was permanent (this is the discriminating regression case: the pre-fix breaker
/// leaves ERLE stuck at ~9.5 dB forever here). Frozen-filter ERLE is
/// 10*log10(2.5/(sqrt(2.5)-1)^2) ~ 8.9 dB; the engage-check bound sits just above it.
#[test]
fn path_gain_jump_in_old_dead_band_recovers() {
    run_path_jump(2.5, 11.0);
}

/// A 4x POWER jump (post-jump ratio ~3.3, above the old trip) must also recover — this
/// covers the general mechanism including the drain-on-miss debounce. Frozen-filter ERLE is
/// 10*log10(4) ~ 6 dB.
#[test]
fn path_gain_jump_above_trip_recovers() {
    run_path_jump(4.0, 9.0);
}

/// Genuine double-talk — an INDEPENDENT near-end voice with NO path change — must never fire
/// the breaker, and adaptation must stay frozen so the voice survives at full level. The
/// voice power is ~1.5x the echo, putting nearend_ratio at ~2.5: squarely in the
/// hangover-pinning band the breaker watches (verified: hang stays pinned at 30 during the
/// pulses). It is PULSED (~250 ms on / ~150 ms off, gaps shorter than the hangover) — the
/// adversarial case for a false fire, because the hangover stays armed across the gaps while
/// the smoothed coherence recovers between phrases; the voiced blocks' depressed coherence
/// must drain the streak faster than those gap tails can build it.
#[test]
fn double_talk_does_not_fire_breaker() {
    let sr = 48_000.0f32;
    let mut aec = Aec::new(sr, 120.0);

    let conv_len = BLOCK * 282; // ~1.5 s far-only convergence
    let dt_len = BLOCK * 376; // ~2 s of pulsed double-talk
    let total = conv_len + dt_len;

    let far = multitone_far(total, sr);
    let h = make_echo_path(600, 30);
    let echo = convolve(&far, &h);

    // Pulsed sung tone: 47 blocks on / 28 blocks off, 330 Hz (between the far tones).
    let two_pi = 2.0 * std::f32::consts::PI;
    let mut voice = vec![0.0f32; total];
    for (n, v) in voice.iter_mut().enumerate().skip(conv_len) {
        let blk = (n - conv_len) / BLOCK;
        if blk % 75 < 47 {
            let t = n as f32 / sr;
            *v = 0.8 * (two_pi * 330.0 * t).sin();
        }
    }
    let mic: Vec<f32> = (0..total).map(|n| echo[n] + voice[n]).collect();

    let mut out = vec![0.0f32; total];
    run_range(&mut aec, &mic, &far, &mut out, 0, total);

    // 1) The breaker must never fire on genuine double-talk: voiced blocks depress the
    //    mic/echo-estimate coherence, so the signature can never accumulate to the hold.
    assert_eq!(
        aec.path_change_fires(),
        0,
        "path-change breaker fired on genuine double-talk (would re-open adaptation into voice)"
    );

    // 2) Adaptation stayed frozen: the voice passes essentially at full level (a filter that
    //    resumed adapting during the voice would model and duck it). Projection of the output
    //    onto the latency-aligned clean voice ~ amplitude ratio.
    let lat = BLOCK;
    let mut corr = 0.0f64;
    let mut ven = 0.0f64;
    for n in (conv_len + lat)..total {
        let v = voice[n - lat] as f64;
        corr += out[n] as f64 * v;
        ven += v * v;
    }
    let retention = (corr / (ven + 1e-12)) as f32;
    assert!(
        retention > 0.85,
        "voice ducked during double-talk: retention={retention:.3} (adaptation not frozen?)"
    );

    // 3) The converged (frozen) filter keeps cancelling the echo through the double-talk:
    //    strip the known (latency-aligned) voice and the residual echo must stay well down.
    let mut echo_e = 0.0f64;
    let mut resid_e = 0.0f64;
    for n in (conv_len + lat)..total {
        let raw_echo = echo[n - lat] as f64;
        let resid = out[n] as f64 - voice[n - lat] as f64;
        echo_e += raw_echo * raw_echo;
        resid_e += resid * resid;
    }
    let dt_erle = 10.0 * (echo_e / (resid_e + 1e-12)).log10();
    assert!(
        dt_erle >= 8.0,
        "echo not cancelled through double-talk: ERLE={dt_erle:.2} dB (should be >= 8)"
    );

    for &v in &out {
        assert!(v.is_finite(), "non-finite output during double-talk");
    }
}
