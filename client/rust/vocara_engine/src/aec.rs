//! Acoustic echo canceller (AEC) for the singer-over-backing-track use case.
//!
//! # The problem
//!
//! A singer practises over a backing track played through the MacBook **built-in speaker**
//! while the **built-in mic** records. The mic captures `d[n] = s[n] + echo[n] + noise`,
//! where `s[n]` is the sung voice we want to keep and `echo[n]` is the backing track after
//! it travelled speaker -> room -> mic (an unknown linear convolution of the rendered
//! reference `x[n]` with the room/speaker impulse response). We have the **exact far-end
//! reference** `x[n]` (captured before it hits the speaker), so the echo is, in principle,
//! a *linear function of a signal we know*. The voice is *not* a function of `x[n]`, so a
//! correctly-behaved linear adaptive filter can subtract the echo while leaving the voice
//! untouched.
//!
//! Apple's `VoiceProcessingIO` AEC was tried and rejected: it is voice-call-tuned and ducks
//! / damages the sung voice during the *continuous double-talk* (voice + full-band music
//! overlapping in time AND frequency) that singing-over-music creates. So the entire design
//! goal here is an AEC that is **gentle on the near-end voice**.
//!
//! # Algorithm: Partitioned-Block Frequency-Domain Adaptive Filter (PBFDAF / MDF)
//!
//! This is the WebRTC/Speex-class approach. The echo path is long (the speaker->mic
//! acoustic delay plus the output+input buffer round-trip is 150-250 ms, i.e. 7k-12k taps
//! at 48 kHz), which is far too costly for a per-sample time-domain NLMS. Instead we:
//!
//! * Work in blocks of `B = 256` samples with an `N = 2B = 512`-point FFT (overlap-save).
//!   Internal latency is one block (5.33 ms @ 48 kHz), well under the 512-sample cap.
//! * Split the filter into `P` frequency-domain partitions, each a length-`B` sub-filter,
//!   covering `tail_ms`. The echo estimate is a partitioned (multi-delay) convolution in
//!   the frequency domain: `Y[k] = sum_p W_p[k] * X_{(now-p)}[k]`. Small FFT + long tail.
//! * Adapt each partition with a **per-bin power-normalized** frequency-domain NLMS. Music
//!   is spectrally colored; normalizing every frequency by its own running far-end power is
//!   the single biggest convergence/stability win and stops bass- or treble-heavy moments
//!   from destabilizing the filter.
//! * Apply the **gradient constraint** (constrained FDAF): each partition is forced back to
//!   a causal length-`B` filter every block, which removes circular-convolution leakage and
//!   is what keeps the filter from slowly diverging.
//!
//! # Voice preservation philosophy
//!
//! Preserving the voice is fundamentally a *control problem on the step size*, not a
//! filtering problem. The adaptive filter only ever models the **linear echo of the known
//! reference**; the voice is uncorrelated with that reference, so a correctly-frozen filter
//! passes the voice straight through into the error signal `e = d - y_hat`. The danger is
//! the filter trying to "explain" voice energy during double-talk and thereby cancelling /
//! distorting the voice. So:
//!
//! * A robust **double-talk detector** (DTD) freezes (or strongly slows) adaptation whenever
//!   near-end voice may be present. It combines an open-loop residual-to-mic ratio, a
//!   normalized echo/mic coherence, and a Geigel-style envelope ratio into a single smooth
//!   adaptation gate `g in [0,1]`. The gate is **asymmetric**: it slams to 0 within one
//!   block on a voiced onset (attack) and only ramps back up after a long hangover of clean
//!   far-end-only audio (release). We deliberately err toward *freezing*: a stale filter
//!   costs a dB or two of cancellation; an over-eager filter permanently corrupts itself and
//!   ducks the voice. Crucially this protection also covers the **cold-start / bootstrap**
//!   window: before the filter has converged the coherence/rho tests are undefined (they divide
//!   by the ~0 echo estimate), so the bootstrap instead uses a *cold-filter-valid* envelope
//!   guard — comparing the raw mic envelope against a conservatively-seeded path-gain bound — so
//!   a **voice-first opening** (singing before/as the music starts) can never be learned and
//!   then cancelled.
//! * A **dual filter** (a fast adapting *background* / shadow filter and a *foreground*
//!   filter that actually produces the output) is the backstop: a background update is only
//!   copied into the foreground when the background is *provably* cancelling better. So even
//!   if the DTD is briefly fooled, a voice-contaminated update is never heard.
//! * The DTD only ever gates the weight *update*. The subtraction `out = d - y_hat` always
//!   runs with the current foreground weights, so a converged filter keeps cancelling the
//!   music *through* the voice while never re-learning the voice.
//! * The optional residual-echo suppressor is **off by default** and, when enabled, has a high
//!   gain floor (no deeper than `-6 dB`) so it can never gut the voice. It is kept off because on
//!   maximally-correlated unison singing it still thins the voice; the voice-safe LINEAR path
//!   (pushed to a full power-normalized step) carries the cancellation instead. Deep cancellation
//!   is explicitly traded away for voice fidelity.
//!
//! # Integration notes
//!
//! * `mic[]` and `farend[]` must be the **same sample-time indices** ("now"). The true echo
//!   of `farend` appears in `mic` delayed by the full output-buffer + acoustic + input-buffer
//!   round-trip; the adaptive filter spans that bulk delay *plus* the reverb tail. Therefore
//!   pick `tail_ms` to cover the whole round-trip (200-250 ms is a good macOS default). If
//!   the true delay exceeds `tail_ms` the early echo is un-modellable and cancellation drops.
//! * `process()` is real-time safe: no heap allocation, no locks, no syscalls, no panics. All
//!   buffers are allocated once in `new()`. Arbitrary call lengths are absorbed into the
//!   fixed internal block size via pre-sized ring buffers, adding one block (`B`) of
//!   algorithmic latency.
//! * The module is self-contained: it includes its own iterative radix-2 Cooley-Tukey FFT
//!   and has zero external-crate and zero intra-crate dependencies in the hot path.

#![allow(clippy::needless_range_loop)]

// ===========================================================================================
// Denormal / NaN guards (inlined so this module is self-contained and liftable).
// Values match the engine-wide `dsp::denormals` convention.
// ===========================================================================================

/// Tiny bias mixed into feedback accumulators so a value decaying toward silence can never
/// enter subnormal arithmetic, even if the caller's flush-to-zero mode is somehow not active.
const ANTI_DENORMAL: f32 = 1.0e-18;

/// Replace a non-finite (NaN/inf) value with 0 so one bad sample can't poison recursive state
/// for the rest of the session.
#[inline(always)]
fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

// ===========================================================================================
// Self-contained radix-2 iterative Cooley-Tukey FFT (f32).
// ===========================================================================================

/// In-place complex FFT over split real/imaginary `f32` arrays. All twiddle factors and the
/// bit-reversal permutation are precomputed in [`Fft::new`]; [`Fft::transform`] allocates
/// nothing and is safe to call from the audio thread.
///
/// The AEC runs ~53 of these per 256-frame block on the speaker path, so the layout is built for
/// speed without changing a single output bit: every butterfly computes exactly what the
/// textbook loop computes (same operands, same operation order, same twiddle VALUES), but the
/// twiddles each stage reads are stored contiguously per stage, so a stage is a plain
/// slice-to-slice loop the compiler vectorizes instead of a strided table walk behind bounds
/// checks. Butterflies within one stage are independent, so visiting them in any order is the
/// same arithmetic. (The tests hold this to a verbatim copy of the old loop, bit for bit.)
struct Fft {
    n: usize,
    /// The bit-reversal permutation as the `(i, rev(i))` pairs with `rev(i) > i` (each swapped
    /// once, in increasing `i`).
    swaps: Box<[(u32, u32)]>,
    /// Per-stage twiddles: the stage of half-length `h` (h = 1, 2, 4, ..., n/2) reads entries
    /// `h - 1 .. 2h - 1`, i.e. `cos/sin(-2*pi*j*(n/2h)/n)` for `j in 0..h`. `wi_inv` holds the
    /// imaginary parts times -1 (the conjugate twiddle of the inverse transform).
    wr: Box<[f32]>,
    wi_fwd: Box<[f32]>,
    wi_inv: Box<[f32]>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(
            n.is_power_of_two() && n >= 2,
            "FFT size must be a power of two >= 2"
        );
        let log2n = n.trailing_zeros();

        // Bit-reversal swap pairs.
        let mut swaps = Vec::with_capacity(n / 2);
        for i in 0..n {
            let mut x = i as u32;
            let mut r = 0u32;
            for _ in 0..log2n {
                r = (r << 1) | (x & 1);
                x >>= 1;
            }
            if r as usize > i {
                swaps.push((i as u32, r));
            }
        }

        // Forward twiddles exp(-i*2*pi*j/n), j in 0..n/2, then laid out per stage.
        let half = n / 2;
        let mut cos = vec![0.0f32; half];
        let mut sin = vec![0.0f32; half];
        for j in 0..half {
            let ang = -2.0 * std::f64::consts::PI * (j as f64) / (n as f64);
            cos[j] = ang.cos() as f32;
            sin[j] = ang.sin() as f32;
        }
        let mut wr = vec![0.0f32; n - 1];
        let mut wi_fwd = vec![0.0f32; n - 1];
        let mut wi_inv = vec![0.0f32; n - 1];
        let mut h = 1;
        while h < n {
            let stride = n / (2 * h);
            for j in 0..h {
                wr[h - 1 + j] = cos[j * stride];
                // The products the butterfly used to form per use (`sin * isign`, isign = ±1),
                // which for these finite values are the value and its negation exactly.
                wi_fwd[h - 1 + j] = sin[j * stride];
                wi_inv[h - 1 + j] = -sin[j * stride];
            }
            h *= 2;
        }

        Fft {
            n,
            swaps: swaps.into_boxed_slice(),
            wr: wr.into_boxed_slice(),
            wi_fwd: wi_fwd.into_boxed_slice(),
            wi_inv: wi_inv.into_boxed_slice(),
        }
    }

    /// In-place radix-2 DIT FFT. `inverse = true` conjugates the twiddles and scales by `1/n`.
    /// `re` and `im` must each be length `n`. No allocation, no panic on the documented sizes.
    #[inline]
    fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.n;
        let re = &mut re[..n];
        let im = &mut im[..n];

        // 1) Bit-reversal permutation (swap each pair once).
        for &(i, j) in self.swaps.iter() {
            re.swap(i as usize, j as usize);
            im.swap(i as usize, j as usize);
        }

        // 2) Iterative butterflies. Stage length doubles: 2, 4, ..., n.
        let wi_all: &[f32] = if inverse { &self.wi_inv } else { &self.wi_fwd };
        // The first two stages (one and two twiddles) run fused, per group of four: the same
        // four butterflies, kept in registers between the stages.
        let (a0r, a0i) = (self.wr[0], wi_all[0]);
        if n >= 4 {
            let (b0r, b0i, b1r, b1i) = (self.wr[1], wi_all[1], self.wr[2], wi_all[2]);
            for (rc, ic) in re.chunks_exact_mut(4).zip(im.chunks_exact_mut(4)) {
                let [mut r0, mut r1, mut r2, mut r3] = [rc[0], rc[1], rc[2], rc[3]];
                let [mut i0, mut i1, mut i2, mut i3] = [ic[0], ic[1], ic[2], ic[3]];
                butterfly(&mut r0, &mut i0, &mut r1, &mut i1, a0r, a0i);
                butterfly(&mut r2, &mut i2, &mut r3, &mut i3, a0r, a0i);
                butterfly(&mut r0, &mut i0, &mut r2, &mut i2, b0r, b0i);
                butterfly(&mut r1, &mut i1, &mut r3, &mut i3, b1r, b1i);
                rc.copy_from_slice(&[r0, r1, r2, r3]);
                ic.copy_from_slice(&[i0, i1, i2, i3]);
            }
        } else {
            let [mut r0, mut r1, mut i0, mut i1] = [re[0], re[1], im[0], im[1]];
            butterfly(&mut r0, &mut i0, &mut r1, &mut i1, a0r, a0i);
            re.copy_from_slice(&[r0, r1]);
            im.copy_from_slice(&[i0, i1]);
        }
        let mut h = 4usize;
        while h < n {
            let len = 2 * h;
            let wr = &self.wr[h - 1..len - 1];
            let wi = &wi_all[h - 1..len - 1];
            for (rc, ic) in re.chunks_exact_mut(len).zip(im.chunks_exact_mut(len)) {
                let (ra, rb) = rc.split_at_mut(h);
                let (ia, ib) = ic.split_at_mut(h);
                for j in 0..h {
                    let (wr, wi) = (wr[j], wi[j]);
                    let xr = rb[j];
                    let xi = ib[j];
                    // t = w * x
                    let tr = wr * xr - wi * xi;
                    let ti = wr * xi + wi * xr;
                    let ur = ra[j];
                    let ui = ia[j];
                    ra[j] = ur + tr;
                    ia[j] = ui + ti;
                    rb[j] = ur - tr;
                    ib[j] = ui - ti;
                }
            }
            h = len;
        }

        // 3) Inverse scaling.
        if inverse {
            let inv = 1.0f32 / (n as f32);
            for v in re.iter_mut() {
                *v *= inv;
            }
            for v in im.iter_mut() {
                *v *= inv;
            }
        }
    }
}

/// One radix-2 butterfly `(u, x) -> (u + w*x, u - w*x)` with twiddle `w = wr + i*wi`, in the
/// operation order of the stage loop in [`Fft::transform`].
#[inline(always)]
fn butterfly(ur: &mut f32, ui: &mut f32, xr: &mut f32, xi: &mut f32, wr: f32, wi: f32) {
    // t = w * x
    let tr = wr * *xr - wi * *xi;
    let ti = wr * *xi + wi * *xr;
    let (u_r, u_i) = (*ur, *ui);
    *ur = u_r + tr;
    *ui = u_i + ti;
    *xr = u_r - tr;
    *xi = u_i - ti;
}

// ===========================================================================================
// Tuning constants — named and explained. Defaults are tuned for 48 kHz singing-over-music.
// ===========================================================================================

/// Internal block size in samples (5.33 ms @ 48 kHz). Drives the one-block algorithmic latency.
const BLOCK: usize = 256;
/// FFT size = 2 * BLOCK for 50%-overlap overlap-save (exact linear convolution).
const FFT_N: usize = 2 * BLOCK;
/// Number of stored half-spectrum bins for a real signal (`N/2 + 1`), exploiting Hermitian sym.
const HALF: usize = FFT_N / 2 + 1;
/// Hard cap on partitions => bounds CPU and memory regardless of a misconfigured `tail_ms`.
/// 64 partitions = 16384 taps = 341 ms of modellable echo at 48 kHz.
const P_MAX: usize = 64;

/// Base NLMS step size (frequency-domain, power-normalized). Stable range ~0.1..1.0. Kept at the
/// GENTLE 0.5: while voice ENERGY retention stays ~1.0 even at a full step, a fast step makes the
/// per-bin echo subtraction fluctuate block-to-block on the voice-correlated bins, producing audible
/// "musical noise" / a robotic "电音" artifact on the sung voice — a perceptual defect the ERLE /
/// voice_retention metrics do NOT capture. The +3 dB ERLE that a unit step won was reverted because
/// of it; 0.5 trades a little backing cancellation for a clean, artifact-free voice (the priority).
const MU: f32 = 0.5;
/// Weight leakage per update. `W <- LEAK*W + grad`. A hair of leak bleeds off un-driven /
/// stale modes and bounds the weight norm; over seconds, not perceptibly hurting ERLE.
const LEAK: f32 = 0.999_95;

/// Per-bin far-end power smoothing (one-pole over blocks, ~10-block / ~53 ms memory). Left at 0.9:
/// a faster (0.8) power estimate makes the per-bin NLMS normalization noisier and slightly LOWERED
/// ERLE in practice, so the steadier estimate is kept.
const PWR_SMOOTH: f32 = 0.9;
/// Relative regularization: `delta = REG_REL * mean_far_power + REG_ABS`. Tracks the signal
/// level so the normalized step is bounded in both loud and quiet passages.
const REG_REL: f32 = 1.0e-3;
/// Absolute regularization floor: prevents divide-by-~0 in silence and kills denormals.
const REG_ABS: f32 = 1.0e-6;

/// Block-level scalar statistics smoothing (one-pole, ~20-block / ~107 ms memory).
const STAT_SMOOTH: f32 = 0.92;

// --- Double-talk detector thresholds (err toward freezing => protect the voice) ---
/// Far-end activity floor (block-mean echo-estimate power). Below this the backing track is
/// effectively silent: nothing to adapt toward, and any mic energy is near-end -> freeze.
const ECHO_FLOOR: f32 = 1.0e-6;
/// Residual-to-mic ratio at/below which we adapt fully (echo dominates, filter is helping).
const RHO_LO: f32 = 0.30;
/// Residual-to-mic ratio at/above which we freeze fully (too much mic energy is unexplained
/// by the echo model => near-end voice present).
const RHO_HI: f32 = 0.70;
/// Echo/mic coherence at/above which we trust "echo dominates" and adapt.
const COH_HI: f32 = 0.70;
/// Echo/mic coherence at/below which we suspect near-end and freeze.
const COH_LO: f32 = 0.40;
/// STRICTER coherence threshold required before the loudspeaker->mic `echo_gain` may be raised.
/// The DTD's own freeze thresholds (COH_HI/RHO_HI) are loose enough that a voice-first opening or
/// momentary double-talk can satisfy them; gain tracking must be tighter than the gate it feeds,
/// otherwise voice energy in `p_mic` over-estimates the gain and desensitizes the Geigel test.
const COH_GAIN: f32 = 0.85;
/// Smoothing of the per-bin mic and far-end power averages the echo-path gain `gxx` is the ratio
/// of (leaky, updated only on confirmed far-only blocks). Slow enough to be stable, fast enough to
/// track a real (slowly drifting) speaker/room path.
const GXX_SMOOTH: f32 = 0.9;
/// Mic/expected-echo power ratio (`nearend_ratio`, from the FREQUENCY-SHAPED gxx model) at or below
/// which adaptation runs at the full step. The ratio is CALIBRATED — it reads ~1 on far-only audio
/// whatever the material — so this only has to clear its far-only spread: measured on the
/// synthetic bench (tests/aec_bench.rs) the 99th percentile is ~1.45 on harmonic music, dry or
/// reverberant, and ~1.7 on drum-and-noise backing (90th ~1.4). A modestly louder echo path
/// (+1.6 dB) mostly stays under it too, so the filter re-converges through a path change instead
/// of stalling.
const NEAREND_FULL_STEP: f32 = 1.5;
/// `nearend_ratio` at which adaptation is frozen outright. The double-talk hangover arms halfway
/// between this and NEAREND_FULL_STEP (2.5): a singer about 2 dB above the echo freezes the filter,
/// one at the echo's level (ratio ~2) cuts the step by a quarter. Tuned on the bench: a lower trip
/// (3.0) protects a 0 dB singer a little more but slows re-convergence after an echo-path change on
/// noise-like backing by ~2.5 dB at 2-4 s; a higher one gives the voice protection back. The old
/// trip of 3.0 sat on a ratio that the old mean-of-ratios gxx biased LOW by ~3-11x depending on the
/// material, i.e. it effectively armed at ~7-22x; see `gxx`.
const NEAREND_TRIP: f32 = 3.5;
/// Freeze point used while the path model is YOUNG, decaying linearly to NEAREND_TRIP over
/// GXX_MATURE_BLOCKS far-only updates after it was (re-)seeded. A model fitted on the first chord
/// or two has only measured the path at those partials; through a room with fine spectral structure
/// (a reverberant path fades over tens of Hz, far inside one 94 Hz bin) the next chord's partials
/// can echo 2-3x louder than it predicts, which reads as double-talk and froze the cold start
/// chord after chord (on the reverberant path of the unit tests below, the first ~2 s of a take
/// lost 2.5-5 dB of ERLE). Once the model has seen enough far-only material the full sensitivity
/// applies.
const NEAREND_TRIP_YOUNG: f32 = 6.0;
/// Far-only gxx updates (~1.1 s of confirmed far-only audio) until the path model counts as mature.
const GXX_MATURE_BLOCKS: u32 = 200;
/// Floor on the secondary (rho*coh) throttle post-convergence. The rho/coh terms collapse on ANY
/// foreground residual spike — including a far-only chord change, which is NOT near-end. This floor
/// keeps a usable trickle of adaptation alive through such spikes so the filter re-converges, while
/// the primary (frequency-shaped near-end) gate still slams everything to 0 on genuine voice.
/// Raised from 0.5 toward 1.0 to let the linear filter re-converge faster through far-only chord
/// changes (where rho/coh collapse but there is NO voice): the PRIMARY frequency-shaped near-end
/// test `g_geigel` is the filter-independent voice detector and already gates the step to 0 the
/// instant genuine voice appears, so the rho/coh product mainly stranding the filter on chord
/// changes only costs ERLE. We do NOT push it all the way to 1.0, though: at full unity the
/// secondary throttle gives up its role of damping the step on the brief residual spike at a
/// double-talk ONSET (before g_geigel has fully collapsed), which let a sliver of voice-period
/// mis-adaptation degrade the converged cancellation and dropped double-talk ERLE below its 8 dB
/// floor. 0.9 keeps re-convergence fast on chord changes while still meaningfully damping the
/// onset spike (the swept range 0.8..0.95 all held the double-talk ERLE >= 8 dB and voice >= 0.98;
/// 0.9 is the middle of that safe band). Voice-SAFE: on confirmed double-talk g_geigel→0 multiplies
/// the whole gate to 0 regardless of this floor. REVERTED to 0.5: the looser throttle let the filter
/// keep adapting through the voice, contributing to the "电音" artifact (see MU).
const RHO_COH_FLOOR: f32 = 0.5;
/// Maximum per-block multiplicative increase of the tracked `echo_gain`. Bounds how fast the
/// gain can rise so even an admitted far-only block cannot make a near-instant jump (which would
/// momentarily desensitize the Geigel test). 1.25 => at most +25% per 5.33 ms block.
const GAIN_UP_MAX: f32 = 1.25;
/// Cold-start adaptation-gate floor applied ONLY before convergence. Coherence is exactly 0 on
/// the very first blocks (an all-zero filter has no estimate to correlate with the mic), so the
/// coherence-gated bootstrap would otherwise deadlock. This tiny floor permits a slow trickle of
/// adaptation so the echo estimate — and thus the coherence signal that then takes over — can
/// ignite. It is deliberately small: during a voice-first opening (where coherence stays low) it
/// bounds how much voice-correlated content the cold filter can ever learn before the gate clamps
/// down. ~0.30 => effective bootstrap step MU*0.30. Large enough that a clean far-only cold start
/// drives coherence up and hands off to the full coherence-gated regime within a fraction of a
/// second, yet the moment the (now non-zero) estimate fails to explain the mic — a voice-first
/// opening — the `dt_evidence` hangover clamps the gate to 0, so only a brief, bounded sliver of
/// adaptation can ever leak into a voice-dominated mic.
const BOOTSTRAP_IGNITE: f32 = 0.30;
/// Double-talk hangover in blocks (~160 ms): after voice activity, stay frozen this long so a
/// breath between phrases or a consonant transient doesn't immediately resume fast adaptation.
const HANG_BLOCKS: u32 = 30;
/// Accumulated blocks of the "echo-path GAIN INCREASE" signature — the double-talk hangover held
/// armed (the shaped near-end test keeps re-tripping it, which covers EVERY deadlocked ratio above
/// the hangover's arming point (NEAREND_FULL_STEP+NEAREND_TRIP)/2, not just ratio > NEAREND_TRIP)
/// while the mic stays highly coherent with the echo estimate (coh > COH_GAIN) — before we conclude
/// the path model is stale and re-seed it. That signature is unreachable by near-end voice (voice
/// loud enough to hold the hangover necessarily DEPRESSES coherence; see the breaker below). The
/// streak DRAINS on a miss (saturating_sub(2)) instead of resetting to 0, so non-stationary
/// (rhythmic) material — whose pauses and level dips briefly drop far_active or the ratio — cannot
/// postpone the breaker forever, while real voice still can't reach it: each phrase contributes at
/// most HANG_BLOCKS of post-phrase high-coherence tail and the voiced blocks themselves (depressed
/// coherence) drain the streak twice as fast as the tail builds it. The deadlock it breaks is
/// otherwise PERMANENT, so a ~0.5 s confirmation (96 blocks of 256 at 48 kHz) costs only a little
/// residual echo.
const PATH_CHANGE_HOLD: u32 = 96;
/// Gate attack smoothing toward a *lower* target (freeze). 1.0 => freeze within one block.
const GATE_ATTACK: f32 = 1.0;
/// Gate release smoothing toward a *higher* target (resume adapting). Slow => cautious.
/// One-pole coefficient: new = old + GATE_RELEASE*(target-old). Raised from 0.06 (~12-block ramp)
/// to 0.12 (~6-block ramp) so the filter resumes full-`MU` adaptation faster after a transient gate
/// dip — the dominant such dip in far-only operation is a chord change, and a quicker resume is
/// what re-converges the moving-harmony echo and lifts ERLE. Voice-SAFE: the release only governs
/// how fast the gate climbs back toward its TARGET; a genuine-voice block still drives the target to
/// 0 via the shaped near-end test and the asymmetric attack (GATE_ATTACK=1.0) slams the gate down in
/// a single block regardless of this release rate, so a faster release cannot adapt into voice.
/// REVERTED to 0.06 (gentler resume) as part of removing the "电音" artifact (see MU).
const GATE_RELEASE: f32 = 0.06;

// --- Dual-filter (foreground / background) copy logic ---
/// Background must beat the foreground residual by this factor before its weights are adopted.
/// Raised from 0.875 (background had to be ~0.58 dB better) to 0.95 (~0.22 dB better) so the
/// foreground tracks a re-converging background more tightly through chord changes — the loose
/// 0.875 margin left the output filter lagging the background by a fixed handicap on every chord,
/// capping the moving-harmony ERLE. Voice-SAFE: the background that gets adopted is itself gated by
/// the shaped DTD (its weight update is frozen the instant genuine voice appears, g_adapt→0), so a
/// tighter adoption margin only tracks BETTER far-only echo models, never a voiced one; and the
/// reverse re-seed branch (foreground << background) still rescues a drifted background.
/// REVERTED to 0.875 as part of removing the "电音" artifact (see MU).
const COPY_MARGIN: f32 = 0.875;
/// Smoothing for the per-filter residual power used by the copy decision. Lowered from 0.9
/// (~10-block memory) to 0.8 (~5-block) so the foreground adopts a freshly re-converged background
/// sooner after a chord change: with the slower smoothing the copy decision lagged the background's
/// actual re-convergence by ~10 blocks, leaving the foreground (which produces the output, hence the
/// measured ERLE) stale through much of each ~40-block chord region. Faster residual tracking tracks
/// the real re-convergence and lifts the moving-harmony ERLE. Voice-SAFE: the copy decision only
/// ever ADOPTS a background that is provably cancelling BETTER (COPY_MARGIN), and the background is
/// itself gated by the shaped DTD, so a voice-contaminated update is never adopted.
/// REVERTED to 0.9 as part of removing the "电音" artifact (see MU).
const FG_BG_SMOOTH: f32 = 0.9;

// --- Divergence guard (hard backstop against howl/runaway) ---
/// Residual louder than mic by this factor counts as a diverging block.
const DIV_RATIO: f32 = 2.0;
/// Consecutive diverging blocks before we shrink the weights and freeze (~43 ms).
const DIV_SHRINK_HOLD: u32 = 8;
/// Factor applied to all weights when shrinking on divergence.
const DIV_SHRINK: f32 = 0.5;
/// Consecutive diverging blocks before a full filter reset (~341 ms persistent => give up).
const DIV_RESET_HOLD: u32 = 64;

// --- Optional residual-echo suppressor (default OFF; voice preservation beats cancellation) ---
//
// OFF by default. The linear AEC plus the frequency-shaped double-talk detector already reduce the
// backing past the required bar while leaving the voice at FULL level (the linear filter only ever
// models the known reference, so it cannot model the voice; the shaped DTD freezes it the instant
// genuine voice appears). Any residual suppressor that acts on the SHARED voice/backing bins must
// thin the voice a little — precisely the VPIO "voice tiny and choppy" failure — so it is kept off.
//
// When enabled it is a STRUCTURALLY voice-safe lever: a per-bin magnitude Wiener gain driven by the
// FREQUENCY-SHAPED, FILTER-INDEPENDENT echo-power estimate `gxx[k]*x_ref[k]` (same path model
// the DTD uses) versus the known mic power `|D[k]|^2`:
//
//     gain[k] = max(FLOOR, 1 - OVER * echo_pow[k] / |D[k]|^2),  echo_pow = max(gxx*x_ref, |Y_fg|^2)
//
//   * In a VOICE-dominated bin, echo_pow/|D|^2 → 0 (the echo model, built only from the reference,
//     cannot explain voice energy), so gain ≈ 1 and the voice passes ~untouched.
//   * In a BACKING-dominated bin the ratio → 1 so gain → FLOOR: the backing is reduced, not erased.
//   * The FLOOR is a HARD per-output bound — NO bin is ever attenuated by more than it, ever — so
//     the suppressor cannot gut the voice even if every other heuristic is fooled. This is the
//     "structural per-output floor" the design calls for.
/// Master enable for the suppressor. OFF: the linear AEC + shaped DTD meet the bar with the voice
/// at full level; flip ON only to trade a little voice fidelity for extra backing reduction.
// KEPT OFF. We measured it: on the maximally-correlated unison test (voice doubling a backing
// partial at the octave, with a ~3 Hz syllabic envelope) the suppressor crushed voice_retention to
// ~0.51 while lifting ERLE to ~15 dB — exactly the VPIO "voice tiny and choppy" failure the design
// forbids. The cause is structural and un-tunable away here: the voice sits in precisely the bins
// the backing occupies, so on the quiet frames of the voice's syllabic envelope the backing
// dominates those shared bins, the per-bin Wiener ratio drives the gain to the floor, and the
// near-end-dominance bypass (mic > ~3 dB above expected echo) does NOT fire on those quiet frames.
// No floor/over/bypass setting that still reduces backing keeps voice >= 0.92 on this case. The
// VOICE-SAFE linear path (faster MU / re-convergence) already reaches ~9.4 dB ERLE with the voice
// intact, so the suppressor stays OFF and the linear path is the shipped lever.
const SUPPRESS_ENABLED: bool = false;
/// Hard per-bin gain floor (-9 dB). The structural voice floor: the suppressor can NEVER attenuate
/// any bin by more than this, so the voice can never be gutted (and the backing is reduced, not
/// erased — exactly "voice normal, backing small"). -9 dB ≈ 0.355 linear. Unused while the
/// suppressor is OFF; if ever enabled, do NOT set it deeper than -6 dB (0.501) per the design bar.
const SUPPRESS_FLOOR: f32 = 0.354_813_4; // db_to_lin(-9)
/// Over-subtraction factor. >1 lets backing-dominated bins reach the floor with a little margin so
/// the modest reduction is reliably audible; the floor still bounds the worst case.
const SUPPRESS_OVER: f32 = 1.4;
/// Suppressor gain time smoothing (one-pole). Higher = smoother = fewer musical-noise artifacts
/// (the "choppiness" the user reported). Smoothing the gain trajectory keeps the voice continuous.
const SUPPRESS_SMOOTH: f32 = 0.85;
/// Near-end-dominance bypass: if the block's mic power exceeds the SHAPED-model expected echo power
/// (sum_k gxx[k]*x_ref[k]) by more than this factor, the block is voice-dominated and the
/// suppressor is pinned to unity (no attenuation at all). The block-level backstop to the per-bin
/// floor: when the singer is clearly louder than any predicted backing, the output is the mic,
/// untouched. Set conservatively LOW (≈3 dB) so any clearly-audible singing reliably bypasses —
/// we would much rather leave some backing in than risk thinning the voice.
const SUPPRESS_BYPASS_RATIO: f32 = 2.0; // mic > ~3 dB above expected echo => pass through

/// Generic tiny epsilon for ratio denominators.
const EPS: f32 = 1.0e-12;

// ===========================================================================================
// The AEC.
// ===========================================================================================

/// Partitioned-block frequency-domain acoustic echo canceller. Single-threaded, owned by the
/// audio thread. All state is pre-allocated; `process()` is real-time safe.
pub struct Aec {
    // ---- configuration ----
    block: usize, // B
    fftn: usize,  // N
    half: usize,  // HALF
    parts: usize, // P

    fft: Fft,

    // ---- far-end frequency-domain delay line (ring of P half-spectra) ----
    // Flat split arrays of length parts*half. `head` is the slot of the newest spectrum.
    x_re: Box<[f32]>,
    x_im: Box<[f32]>,
    head: usize,

    // ---- adaptive weights: foreground (heard) + background (shadow), each parts*half ----
    // Weights are indexed by partition age `p` (0 = newest far-end block), NOT by ring slot.
    wfg_re: Box<[f32]>,
    wfg_im: Box<[f32]>,
    wbg_re: Box<[f32]>,
    wbg_im: Box<[f32]>,

    // ---- per-bin running far-end power (length half), summed over the partition span ----
    pxx: Box<[f32]>,

    // ---- overlap-save time-domain history (previous far-end block) ----
    far_prev: Box<[f32]>, // length B
    // ---- previous mic block, so the suppressor can frame the mic spectrum [mic_prev | mic_cur]
    //      identically to the far/echo spectra (same 2B overlap-save window => the per-bin powers
    //      |X|^2, |Y_fg|^2 and |D|^2 are all directly comparable for the Wiener ratio). ----
    mic_prev: Box<[f32]>, // length B

    // ---- pre-allocated scratch (reused every block; never resized) ----
    fr_re: Box<[f32]>,   // length N: generic FFT working frame (real)
    fr_im: Box<[f32]>,   // length N
    y_re: Box<[f32]>,    // length N: echo-estimate spectrum (expanded to full N for ifft)
    y_im: Box<[f32]>,    // length N
    e_re: Box<[f32]>,    // length N: error spectrum
    e_im: Box<[f32]>,    // length N
    g_re: Box<[f32]>,    // length N: per-partition gradient scratch (time<->freq for constraint)
    g_im: Box<[f32]>,    // length N
    q_re: Box<[f32]>,    // length half: the second gradient of a constraint pair
    q_im: Box<[f32]>,    // length half
    norm: Box<[f32]>,    // length half: this block's per-bin NLMS step (see compute_norm)
    eblk_fg: Box<[f32]>, // length B: foreground residual (the output candidate)
    eblk_bg: Box<[f32]>, // length B: background residual (for the copy decision)

    // ---- saved foreground echo-estimate half-spectrum (length half) for the suppressor ----
    // The Wiener residual suppressor needs to know, per frequency bin, how much backing the
    // linear filter predicts is present. We snapshot |Y[k]| of the foreground echo estimate
    // before y_re/y_im are overwritten by the IFFT, so the suppressor can be a true magnitude
    // Wiener gain rather than a coherence approximation.
    yh_pow: Box<[f32]>, // length half: |Y_fg[k]|^2 of the current block's echo estimate

    // ---- per-bin echo-path POWER gain, gxx[k] = E[|D_echo[k]|^2] / E[x_ref[k]] ----
    // This is the frequency-SHAPED echo model: unlike the scalar `echo_gain`, it captures that the
    // speaker/room path is louder in some bands than others. Two things rely on it, and both need
    // the frequency shape to be robust against a moving harmony (a chord change moves far-end
    // energy between bands; a flat gain then mis-predicts the new chord's echo and looks like
    // near-end, which is exactly the bug that stranded the filter frozen on music):
    //   * the near-end (double-talk) test: expected echo = sum_k gxx[k]*x_ref[k]; voice shows up
    //     as mic energy ABOVE this, a chord change does not;
    //   * the residual suppressor: per-bin expected echo power = gxx[k]*x_ref[k].
    //
    // It is the RATIO OF LEAKY AVERAGES gxx_dd/gxx_xx, both tracked ONLY during confirmed far-only
    // blocks — never a leaky average of the per-block ratio |D|^2/|X|^2. The mean of a ratio is
    // biased high by E[|X|^2]*E[1/|X|^2]: small on a steady tone stack, but a fluctuating bin power
    // (any noise-like or non-stationary backing) inflates it without bound, and the far-end frame
    // not lining up with the echo it produced (the round trip is several blocks) inflates it more.
    // Measured: far-only nearend_ratio read ~0.2 on music and ~0.09 on drum-and-noise backing
    // instead of ~1, so the singer had to be ~10-20x the echo power before the detector saw them
    // at all, and the filter kept adapting into the voice.
    gxx: Box<[f32]>, // length half
    /// Leaky per-bin averages of |D[k]|^2 and x_ref[k] over confirmed far-only blocks; gxx is
    /// their ratio.
    gxx_dd: Box<[f32]>, // length half
    gxx_xx: Box<[f32]>, // length half
    /// True once gxx has been seeded from at least one confirmed far-only block.
    gxx_valid: bool,
    /// Far-only updates gxx has absorbed since it was (re-)seeded, saturating at
    /// GXX_MATURE_BLOCKS: how far the young-model leniency (NEAREND_TRIP_YOUNG) has decayed.
    gxx_age: u32,
    /// Per-bin mic power |D[k]|^2 for the current block ([mic_prev|mic_cur] framing), computed
    /// once per block and shared by the near-end test, gxx tracking, and the suppressor.
    d_pow: Box<[f32]>, // length half
    /// Per-bin current far-end power |X_now[k]|^2 ([far_prev|far_cur] framing).
    x_pow_now: Box<[f32]>, // length half
    /// Per-bin far-end power the CURRENT mic block's echo was made from: the far-end delay line's
    /// |X_p[k]|^2 weighted by the share of the foreground filter's energy in partition p. The
    /// echo in this mic block left the speaker a round trip ago (several blocks), so comparing it
    /// against |X_now|^2 mis-predicts every transient and every beating partial — a drum hit is
    /// "expected" blocks before it echoes, then echoes into a block that expects nothing. Weighting
    /// by where the filter has put its energy aligns the far-end power with the echo it produced
    /// without depending on how well the filter has converged in level (gxx sets the level).
    /// Falls back to |X_now|^2 while the filter is still all zero.
    x_ref: Box<[f32]>, // length half
    /// Scratch: the foreground filter's energy per partition (length parts), for `x_ref`.
    part_pow: Box<[f32]>,

    // ---- input/output rings: decouple arbitrary call length from the fixed block engine ----
    in_mic: Box<[f32]>, // length B accumulator
    in_far: Box<[f32]>, // length B accumulator
    in_fill: usize,
    out_ring: Box<[f32]>, // circular output FIFO
    out_r: usize,
    out_w: usize,
    out_count: usize,
    out_cap: usize,
    /// How many output samples are still "priming" (zeros emitted before the first block lands).
    prime: usize,

    // ---- DTD + statistics state (scalars, leaky) ----
    p_mic: f32,    // smoothed mic power
    p_far: f32,    // smoothed far-end INPUT power (drives far-end activity + Geigel)
    p_echo: f32,   // smoothed foreground echo-estimate power
    p_err: f32,    // smoothed foreground residual power
    p_de: f32,     // smoothed cross power <mic, echo>
    p_err_fg: f32, // residual power of the foreground filter (copy decision)
    p_err_bg: f32, // residual power of the background filter (copy decision)
    /// Running estimate of the linear echo gain (||echo||/||far||), used by the Geigel test so
    /// it works before the filter is fully converged. Tracks toward observed mic/far on adapt.
    echo_gain: f32,
    /// True once the filter has demonstrably started cancelling; gates the strict (coherence /
    /// rho) double-talk tests in so they don't deadlock the cold start.
    converged: bool,
    g_adapt: f32,   // current smoothed adaptation gate in [0,1]
    hang: u32,      // double-talk hangover countdown
    div_count: u32, // consecutive diverging blocks
    /// Accumulated evidence (blocks; drained on misses, see PATH_CHANGE_HOLD) of the
    /// echo-path-gain-INCREASE deadlock signature: the double-talk hangover held armed while the
    /// mic stays highly coherent with the echo estimate. Reaching the hold breaks the freeze.
    path_change_streak: u32,
    /// Diagnostic: how many times the path-change deadlock breaker has fired since construction
    /// or `reset()`. Read by tests/telemetry; never used by the algorithm.
    path_change_fires: u32,
    /// The output carries no echo to cancel (headphones): the far end is taken as silent, so
    /// nothing adapts and the mic passes through (see [`Aec::set_echo_free`]).
    echo_free: bool,

    // ---- optional suppressor state ----
    sup_gain: Box<[f32]>, // length half
}

impl Aec {
    /// Create an AEC. `sample_rate` in Hz (e.g. 48000). `tail_ms` is the echo-path length to
    /// model (e.g. 150-250 ms): it MUST span the output-buffer + acoustic + input-buffer
    /// round-trip plus reverb tail (see the module docs on far-end alignment).
    pub fn new(sample_rate: f32, tail_ms: f32) -> Aec {
        let block = BLOCK;
        let fftn = FFT_N;
        let half = HALF;

        let tail_samples = (tail_ms * 1.0e-3 * sample_rate).round().max(1.0) as usize;
        let parts = tail_samples.div_ceil(block).clamp(1, P_MAX);

        let zeros = |n: usize| vec![0.0f32; n].into_boxed_slice();

        // Output ring sized generously so any plausible single call drains without blocking:
        // worst case we stage almost a full block plus emit a full block. 4 * B is ample and
        // is allocated once.
        let out_cap = 4 * block;

        Aec {
            block,
            fftn,
            half,
            parts,
            fft: Fft::new(fftn),

            x_re: zeros(parts * half),
            x_im: zeros(parts * half),
            head: 0,

            wfg_re: zeros(parts * half),
            wfg_im: zeros(parts * half),
            wbg_re: zeros(parts * half),
            wbg_im: zeros(parts * half),

            pxx: zeros(half),

            far_prev: zeros(block),
            mic_prev: zeros(block),

            fr_re: zeros(fftn),
            fr_im: zeros(fftn),
            y_re: zeros(fftn),
            y_im: zeros(fftn),
            e_re: zeros(fftn),
            e_im: zeros(fftn),
            g_re: zeros(fftn),
            g_im: zeros(fftn),
            q_re: zeros(half),
            q_im: zeros(half),
            norm: zeros(half),
            eblk_fg: zeros(block),
            eblk_bg: zeros(block),

            yh_pow: zeros(half),
            gxx: zeros(half),
            gxx_dd: zeros(half),
            gxx_xx: zeros(half),
            gxx_valid: false,
            gxx_age: 0,
            d_pow: zeros(half),
            x_pow_now: zeros(half),
            x_ref: zeros(half),
            part_pow: zeros(parts),

            in_mic: zeros(block),
            in_far: zeros(block),
            in_fill: 0,
            out_ring: zeros(out_cap),
            out_r: 0,
            out_w: 0,
            out_count: 0,
            out_cap,
            // The first block of output is the priming latency: we emit zeros until the first
            // internal block has been produced. Thereafter out.len() always matches mic.len().
            prime: block,

            p_mic: 0.0,
            p_far: 0.0,
            p_echo: 0.0,
            p_err: 0.0,
            p_de: 0.0,
            p_err_fg: 0.0,
            p_err_bg: 0.0,
            echo_gain: 0.0,
            converged: false,
            g_adapt: 0.0, // start frozen/cautious: never adapt into a voice-first opening
            hang: 0,
            div_count: 0,
            path_change_streak: 0,
            path_change_fires: 0,
            echo_free: false,

            sup_gain: vec![1.0f32; half].into_boxed_slice(),
        }
    }

    /// Cancel echo for one call. `mic[]` is the raw near-end (with echo); `farend[]` is the
    /// loudspeaker reference for the SAME sample-time indices as `mic` (both "now"). The
    /// echo-cancelled near-end is written into `out[]`. `mic.len() == farend.len() == out.len()`,
    /// arbitrary length; the AEC buffers internally to its fixed block size.
    ///
    /// Real-time safe: no heap allocation, no locks, no syscalls, no panics.
    pub fn process(&mut self, mic: &[f32], farend: &[f32], out: &mut [f32]) {
        // Degrade gracefully on a length mismatch instead of panicking (RT safety).
        let n = mic.len().min(farend.len()).min(out.len());

        for i in 0..n {
            // Stage one sample into the block accumulators.
            self.in_mic[self.in_fill] = sanitize(mic[i]);
            self.in_far[self.in_fill] = if self.echo_free {
                0.0
            } else {
                sanitize(farend[i])
            };
            self.in_fill += 1;

            if self.in_fill == self.block {
                self.process_block();
                self.in_fill = 0;
            }

            // Emit one output sample. During priming we emit zeros (the one-block latency);
            // afterward the ring stays exactly one block deep and feeds out[] 1:1.
            if self.prime > 0 {
                out[i] = 0.0;
                self.prime -= 1;
            } else {
                out[i] = sanitize(self.pop_out());
            }
        }

        // If out is longer than the consumed input (shouldn't happen given the min()), zero the
        // remainder so the caller never reads uninitialized garbage.
        for i in n..out.len() {
            out[i] = 0.0;
        }
    }

    /// Reset all adaptive state to a cold start. No allocation; safe on the audio thread.
    pub fn reset(&mut self) {
        for v in self.x_re.iter_mut() {
            *v = 0.0;
        }
        for v in self.x_im.iter_mut() {
            *v = 0.0;
        }
        for v in self.wfg_re.iter_mut() {
            *v = 0.0;
        }
        for v in self.wfg_im.iter_mut() {
            *v = 0.0;
        }
        for v in self.wbg_re.iter_mut() {
            *v = 0.0;
        }
        for v in self.wbg_im.iter_mut() {
            *v = 0.0;
        }
        for v in self.pxx.iter_mut() {
            *v = 0.0;
        }
        for v in self.far_prev.iter_mut() {
            *v = 0.0;
        }
        for v in self.mic_prev.iter_mut() {
            *v = 0.0;
        }
        for v in self.sup_gain.iter_mut() {
            *v = 1.0;
        }
        for v in self.yh_pow.iter_mut() {
            *v = 0.0;
        }
        self.clear_path_model();
        for v in self.d_pow.iter_mut() {
            *v = 0.0;
        }
        for v in self.x_pow_now.iter_mut() {
            *v = 0.0;
        }
        for v in self.x_ref.iter_mut() {
            *v = 0.0;
        }
        for v in self.out_ring.iter_mut() {
            *v = 0.0;
        }

        self.head = 0;
        self.in_fill = 0;
        self.out_r = 0;
        self.out_w = 0;
        self.out_count = 0;
        self.prime = self.block;

        self.p_mic = 0.0;
        self.p_far = 0.0;
        self.p_echo = 0.0;
        self.p_err = 0.0;
        self.p_de = 0.0;
        self.p_err_fg = 0.0;
        self.p_err_bg = 0.0;
        self.echo_gain = 0.0;
        self.converged = false;
        self.g_adapt = 0.0;
        self.hang = 0;
        self.div_count = 0;
        self.path_change_streak = 0;
        self.path_change_fires = 0;
    }

    /// Whether the output route carries an echo to cancel. On headphones (`true`) the mic hears the
    /// singer and none of the backing, yet the canceller still saw the backing as its reference: a
    /// cold filter never converged and kept adapting into the in-key voice at its bootstrap floor,
    /// and a filter converged on the speakers kept subtracting an echo that was gone. With
    /// `echo_free` the reference is taken as silent, so nothing adapts and the mic passes through
    /// untouched, one block late as always (the take's latency compensation counts that block).
    /// Every change restarts the filters from cold: what they learned belongs to the other route.
    ///
    /// Real-time safe; meant to be called every callback with the route's current state.
    pub fn set_echo_free(&mut self, on: bool) {
        if on == self.echo_free {
            return;
        }
        self.echo_free = on;
        self.restart();
        if on {
            // The delay line still holds up to a tail of the backing: drop it too, so the mic
            // passes through from the next block on rather than once it has drained.
            for v in self.x_re.iter_mut() {
                *v = 0.0;
            }
            for v in self.x_im.iter_mut() {
                *v = 0.0;
            }
            for v in self.far_prev.iter_mut() {
                *v = 0.0;
            }
            self.p_far = 0.0;
        }
    }

    /// Zero both filters and re-bootstrap the double-talk state and path model from cold, keeping
    /// the far-end delay line and the block rings, so the audio keeps flowing without a gap (unlike
    /// [`reset`](Aec::reset), which re-primes the output). Real-time safe: no allocation.
    pub fn restart(&mut self) {
        for v in self.wfg_re.iter_mut() {
            *v = 0.0;
        }
        for v in self.wfg_im.iter_mut() {
            *v = 0.0;
        }
        for v in self.wbg_re.iter_mut() {
            *v = 0.0;
        }
        for v in self.wbg_im.iter_mut() {
            *v = 0.0;
        }
        for v in self.pxx.iter_mut() {
            *v = 0.0;
        }
        // Cold-restart the DTD / path model too, exactly as reset() does. Zeroing only the
        // weights leaves gxx/echo_gain/converged/hang describing the OLD path, so if the restart
        // followed a path change the stale model strands the re-started filter: zero weights =>
        // ~0 echo estimate => ~0 coherence => far_only_evidence can never fire => gxx can never
        // refresh, and the stale shaped near-end test pins the freeze (the same deadlock
        // mechanism the path-change breaker guards against).
        self.clear_path_model();
        self.echo_gain = 0.0;
        self.converged = false;
        self.hang = 0;
        self.p_err_fg = 0.0;
        self.p_err_bg = 0.0;
        self.path_change_streak = 0;
        self.div_count = 0;
        self.g_adapt = 0.0;
    }

    /// Diagnostic: how many times the path-change deadlock breaker has fired since construction
    /// or the last [`reset`](Aec::reset). Exposed for tests/telemetry; not part of the audio API.
    #[doc(hidden)]
    pub fn path_change_fires(&self) -> u32 {
        self.path_change_fires
    }

    // -------------------------------------------------------------------------------------
    // Output ring helpers (pure index math, no alloc).
    // -------------------------------------------------------------------------------------

    #[inline]
    fn push_out(&mut self, v: f32) {
        self.out_ring[self.out_w] = v;
        self.out_w = ring_next(self.out_w, self.out_cap);
        if self.out_count < self.out_cap {
            self.out_count += 1;
        } else {
            // Overflow guard: drop the oldest (should never happen with the chosen capacity).
            self.out_r = ring_next(self.out_r, self.out_cap);
        }
    }

    #[inline]
    fn pop_out(&mut self) -> f32 {
        if self.out_count == 0 {
            return 0.0;
        }
        let v = self.out_ring[self.out_r];
        self.out_r = ring_next(self.out_r, self.out_cap);
        self.out_count -= 1;
        v
    }

    // -------------------------------------------------------------------------------------
    // The heart: process one B-sample block held in in_mic / in_far.
    // -------------------------------------------------------------------------------------

    fn process_block(&mut self) {
        let b = self.block;
        let n = self.fftn;
        let half = self.half;
        let parts = self.parts;

        // ----- 1) Far-end frame [far_prev | far_cur] -> spectrum, store into the FDL ring -----
        for i in 0..b {
            self.fr_re[i] = self.far_prev[i];
            self.fr_re[b + i] = self.in_far[i];
        }
        for i in 0..n {
            self.fr_im[i] = 0.0;
        }
        self.fft.transform(&mut self.fr_re, &mut self.fr_im, false);

        // Advance the ring and write the new far-end half-spectrum into the newest slot.
        self.head = (self.head + parts - 1) % parts; // move head "back" so age-0 == head
        {
            let base = self.head * half;
            for k in 0..half {
                self.x_re[base + k] = self.fr_re[k];
                self.x_im[base + k] = self.fr_im[k];
            }
        }
        // Per-bin current far-end power |X_now[k]|^2 (this block, [far_prev|far_cur] framing).
        for k in 0..half {
            let xr = self.fr_re[k];
            let xi = self.fr_im[k];
            self.x_pow_now[k] = xr * xr + xi * xi;
        }
        self.update_far_ref();
        // Save current far block as the next overlap-save history.
        self.far_prev.copy_from_slice(&self.in_far[..b]);

        // Per-bin mic power |D[k]|^2, framed [mic_prev|mic_cur] to MATCH the far framing so the
        // per-bin ratios used by the near-end test, gxx tracking and the suppressor are calibrated.
        // (Uses e_re/e_im as scratch; they are free here and re-zeroed before their own use below.)
        for i in 0..b {
            self.e_re[i] = self.mic_prev[i];
            self.e_re[b + i] = self.in_mic[i];
        }
        for i in 0..n {
            self.e_im[i] = 0.0;
        }
        self.fft.transform(&mut self.e_re, &mut self.e_im, false);
        for k in 0..half {
            let dr = self.e_re[k];
            let di = self.e_im[k];
            self.d_pow[k] = dr * dr + di * di;
        }

        // ----- 2) Echo estimate for BOTH filters: Y[k] = sum_p W_p[k] * X_(head+p)[k] -----
        // Partition age p maps to ring slot (head + p) mod parts (head is newest, age 0).
        // Foreground -> y_re/y_im (full N built via Hermitian symmetry below).
        // Background -> reuse g_re/g_im as scratch for its spectrum.
        for k in 0..half {
            self.y_re[k] = 0.0;
            self.y_im[k] = 0.0;
            self.g_re[k] = 0.0;
            self.g_im[k] = 0.0;
        }
        for p in 0..parts {
            let xs = ((self.head + p) % parts) * half;
            let ws = p * half;
            let (xr, xi) = (&self.x_re[xs..xs + half], &self.x_im[xs..xs + half]);
            // foreground
            complex_mac(
                &mut self.y_re[..half],
                &mut self.y_im[..half],
                &self.wfg_re[ws..ws + half],
                &self.wfg_im[ws..ws + half],
                xr,
                xi,
            );
            // background
            complex_mac(
                &mut self.g_re[..half],
                &mut self.g_im[..half],
                &self.wbg_re[ws..ws + half],
                &self.wbg_im[ws..ws + half],
                xr,
                xi,
            );
        }

        // Snapshot the foreground echo-estimate magnitude-squared per bin BEFORE the IFFT
        // overwrites y_re/y_im. The Wiener residual suppressor (step 9) uses this to know, per
        // frequency, how much backing the linear model says is present — so it can attenuate
        // backing-dominated bins toward a high gain floor while leaving voice-dominated bins
        // (where the echo estimate is small relative to the mic) essentially untouched.
        for k in 0..half {
            let yr = self.y_re[k];
            let yi = self.y_im[k];
            self.yh_pow[k] = yr * yr + yi * yi;
        }

        // ----- 3) IFFT the foreground echo estimate; keep the valid (last-B) samples -----
        Self::hermitian_expand(&mut self.y_re, &mut self.y_im, n, half);
        self.fft.transform(&mut self.y_re, &mut self.y_im, true);
        // y_re now holds the time-domain foreground echo; valid samples are [b..n].
        for i in 0..b {
            let d = self.in_mic[i];
            self.eblk_fg[i] = sanitize(d - self.y_re[b + i]);
        }

        // Background echo (from g_re/g_im) -> residual, for the copy decision only.
        Self::hermitian_expand(&mut self.g_re, &mut self.g_im, n, half);
        self.fft.transform(&mut self.g_re, &mut self.g_im, true);
        for i in 0..b {
            let d = self.in_mic[i];
            self.eblk_bg[i] = sanitize(d - self.g_re[b + i]);
        }

        // ----- 4) Block statistics (powers + cross), leaky-integrated -----
        let mut s_mic = 0.0f64;
        let mut s_far = 0.0f64;
        let mut s_echo = 0.0f64;
        let mut s_err = 0.0f64;
        let mut s_de = 0.0f64;
        let mut s_err_bg = 0.0f64;
        for i in 0..b {
            let d = self.in_mic[i] as f64;
            // Far-end INPUT (reference).
            let x = self.in_far[i] as f64;
            // Foreground echo estimate, sanitized so the recursive power stats can
            // never be poisoned by a non-finite estimate.
            let yh = sanitize(self.y_re[b + i]) as f64;
            let ef = self.eblk_fg[i] as f64;
            let eb = self.eblk_bg[i] as f64;
            s_mic += d * d;
            s_far += x * x;
            s_echo += yh * yh;
            s_err += ef * ef;
            s_de += d * yh;
            s_err_bg += eb * eb;
        }
        let inv_b = 1.0 / b as f64;
        let m_mic = (s_mic * inv_b) as f32;
        let m_far = (s_far * inv_b) as f32;
        let m_echo = (s_echo * inv_b) as f32;
        let m_err = (s_err * inv_b) as f32;
        let m_de = (s_de * inv_b) as f32;
        let m_err_bg = (s_err_bg * inv_b) as f32;

        self.p_mic = leaky(self.p_mic, m_mic, STAT_SMOOTH);
        self.p_far = leaky(self.p_far, m_far, STAT_SMOOTH);
        self.p_echo = leaky(self.p_echo, m_echo, STAT_SMOOTH);
        self.p_err = leaky(self.p_err, m_err, STAT_SMOOTH);
        self.p_de = leaky(self.p_de, m_de, STAT_SMOOTH);
        self.p_err_fg = leaky(self.p_err_fg, m_err, FG_BG_SMOOTH);
        self.p_err_bg = leaky(self.p_err_bg, m_err_bg, FG_BG_SMOOTH);

        // ----- 5) Double-talk detection -> smooth adaptation gate g_adapt in [0,1] -----
        // Far-end activity is judged on the REFERENCE input power (not the echo estimate),
        // otherwise the cold start deadlocks: zero weights -> zero echo estimate -> "no echo"
        // -> never adapt. With a real reference present we are free to learn.
        let far_active = self.p_far > ECHO_FLOOR;

        // (a) residual/mic ratio: fraction of mic energy NOT explained by the echo model.
        let rho = self.p_err / (self.p_mic + EPS);
        // (b) normalized echo/mic coherence (open-loop): high when echo dominates the mic.
        let coh = (self.p_de * self.p_de) / (self.p_mic * self.p_echo + EPS);

        // (c) Geigel-style envelope test. We track the TRUE loudspeaker->mic power gain
        // g = sqrt(p_mic / p_far) during confirmed far-end-only activity (high coherence: the
        // mic is almost entirely echo, so the mic/reference ratio IS the echo path gain). Then
        // the expected echo power for the current reference is g^2 * p_far; if the mic exceeds
        // that by a margin, the surplus energy is near-end voice. Unlike an echo-ESTIMATE-based
        // gain, this is independent of how converged the adaptive filter is, so it does not
        // collapse while the filter is still pulling in.
        // The gain may ONLY be raised under STRICTER far-only evidence than the DTD's own freeze
        // thresholds: high coherence (COH_GAIN > COH_HI), low residual (rho < RHO_LO, i.e. the
        // echo model already explains nearly all the mic), and NOT in hangover (a recent voiced
        // block must not feed the gain). This guarantees voice-contaminated blocks can never
        // inflate `echo_gain` and thereby desensitize the Geigel double-talk test.
        let far_only_evidence = far_active && self.hang == 0 && coh > COH_GAIN && rho < RHO_LO;
        if far_only_evidence {
            let inst_gain = (self.p_mic / (self.p_far + EPS)).sqrt();
            // Track upward quickly (a louder true echo must not look like near-end) and decay
            // slowly, so a transient dip can't make the gain underestimate. Cap the per-block
            // upward step so a single block can never make a near-instant jump (the 0.7 leak
            // alone would let it slew ~30% toward the instantaneous estimate in one block).
            if self.echo_gain <= 0.0 {
                // First estimate: this block already passed the STRICT far-only evidence (high
                // coherence, low residual, no hangover), so the mic is confirmed to be essentially
                // pure echo and `inst_gain` is the true path gain. Seed directly — a leaky/capped
                // first step would chronically UNDER-estimate the gain and desensitize the whole
                // Geigel test (an under-estimated expected echo makes every later block look like
                // near-end, freezing adaptation permanently).
                self.echo_gain = inst_gain;
            } else if inst_gain > self.echo_gain {
                // Track upward quickly (a louder true echo must not look like near-end) but cap the
                // per-block multiplicative step so a single (possibly voice-contaminated) block can
                // never make a near-instant jump that would desensitize the Geigel test.
                let stepped = leaky(self.echo_gain, inst_gain, 0.7);
                let cap = self.echo_gain * GAIN_UP_MAX;
                self.echo_gain = stepped.min(cap);
            } else {
                // Decay slowly, so a transient dip can't make the gain underestimate.
                self.echo_gain = leaky(self.echo_gain, inst_gain, 0.97);
            }

            // Fold this confirmed far-only block into the per-bin echo-path power gain gxx (see the
            // `gxx` field docs). This is the FREQUENCY-SHAPED echo model that makes the near-end
            // test and the suppressor robust to chord changes. Only updated under the same STRICT
            // far-only evidence that gates `echo_gain`, so voice can never contaminate the path
            // model. The first block seeds it directly (avoid a chronic under-estimate).
            self.track_path_model(!self.gxx_valid);
        }

        // Frequency-shaped near-end (double-talk) measure: compare the actual mic power against the
        // echo power the SHAPED path model predicts for the far-end that made it,
        //   expected = sum_k gxx[k] * x_ref[k],   nearend_ratio = p_mic_block / expected.
        // A chord change merely moves far-end energy between bands the model already knows, so it
        // does NOT inflate this ratio; genuine voice adds energy the reference cannot explain, so it
        // does. This replaces the flat-scalar Geigel test that a chord change used to fool. With
        // gxx a ratio of averages it is CALIBRATED: ~1 on far-only audio of any kind.
        let mut shaped_expected = 0.0f64;
        let mut mic_block_pow = 0.0f64;
        for k in 0..self.half {
            shaped_expected += (self.gxx[k] * self.x_ref[k]) as f64;
            mic_block_pow += self.d_pow[k] as f64;
        }
        let nearend_ratio = if self.gxx_valid {
            (mic_block_pow / (shaped_expected + REG_ABS as f64)) as f32
        } else {
            // No shaped model yet: fall back to the scalar Geigel ratio so the test still functions
            // during the very first far-only blocks before gxx has seeded.
            self.p_mic / (self.echo_gain * self.echo_gain * self.p_far + REG_ABS)
        };

        // Mark convergence once the residual drops clearly below the mic (the filter is helping)
        // and we have a usable echo-gain estimate to police double-talk with.
        if far_active && rho < 0.5 && self.echo_gain > 0.0 {
            self.converged = true;
        }

        // ----- Path-change deadlock breaker (sudden echo-path GAIN INCREASE) -----
        // A sudden loudspeaker/room gain INCREASE leaves the tracked path model (gxx) stale and
        // UNDER-predicting the now-louder echo, so the shaped near-end test trips (ANY
        // nearend_ratio above the arming point (NEAREND_FULL_STEP+NEAREND_TRIP)/2 does), re-arms
        // the hangover every block, and pins the gate at 0 indefinitely. Every normal recovery path
        // is blocked: gxx only refreshes inside far_only_evidence (needs hang == 0, which the
        // pinned hangover never grants, and rho < RHO_LO); the divergence guard needs rho >
        // DIV_RATIO, far above a stale under-cancelling filter's residual ((F-1)^2/F^2 < 1 for any
        // gain jump F). (A gain DECREASE self-corrects — the model then OVER-predicts,
        // nearend_ratio < 1, the gate reopens — so only an INCREASE deadlocks.)
        //
        // The tell that this is a louder PURE ECHO and not near-end voice: the mic stays highly
        // COHERENT with the filter's echo estimate (coh ~ 1 — a scaled echo is still a scaled
        // echo), whereas voice adds energy UNCORRELATED with the estimate and necessarily DEPRESSES
        // coh (unexplained energy strong enough to hold the hangover pushes coh to ~1/ratio < 1/2,
        // far under COH_GAIN). So "hangover held AND coh > COH_GAIN" can ONLY be a stale-model path
        // increase — and, unlike arming on nearend_ratio > NEAREND_TRIP, it covers the WHOLE
        // deadlocked band, including ratios between the arming point and NEAREND_TRIP that freeze
        // the gate but could never reach a trip-level arming test. (And gxx is a per-bin power
        // RATIO, so a merely louder BACKING at the SAME path leaves it unchanged — only a louder
        // PATH trips this.) The signature must ACCUMULATE across many blocks before firing; a miss
        // DRAINS the streak (saturating_sub(2)) rather than resetting it, so the pauses/dips of
        // real (rhythmic) far-end material cannot postpone the breaker forever, while genuine voice
        // — whose depressed coherence makes every voiced block a miss — drains it faster than the
        // bounded (<= HANG_BLOCKS) post-phrase hangover tail can build it. On firing we re-seed the
        // stale path model from the current (confirmed pure-echo) block and drop back to the
        // cold-start bootstrap so adaptation re-ignites and re-converges to the new path.
        // Voice-safe: the reused bootstrap gate is the SAME coherence-ramp voice protection used at
        // cold start, and the high-coherence guard above already excludes any real near-end on the
        // re-seed block itself.
        let path_change_freeze = self.converged && far_active && self.hang > 0 && coh > COH_GAIN;
        if path_change_freeze {
            self.path_change_streak = self.path_change_streak.saturating_add(1);
        } else {
            self.path_change_streak = self.path_change_streak.saturating_sub(2);
        }
        if self.path_change_streak >= PATH_CHANGE_HOLD {
            // Re-seed gxx directly from this block's per-bin mic/far power ratio — the SAME direct
            // seed the far_only_evidence path uses on a cold model, forced through here to break the
            // stale-model chicken-and-egg (far_only_evidence itself cannot run because rho is above
            // RHO_LO). The block is confirmed high-coherence pure echo, so d_pow IS the true echo
            // power for x_ref. (A one-block seed is the least reliable model there is, so the
            // young-model leniency restarts with it.)
            self.track_path_model(true);
            // Drop to cold bootstrap so the (now high-coherence) gate reopens and the filter
            // re-converges to the louder path; echo_gain re-tracks upward under far_only_evidence.
            self.converged = false;
            self.hang = 0;
            self.path_change_streak = 0;
            self.path_change_fires = self.path_change_fires.saturating_add(1);
        }

        // The full DTD (Geigel + coherence + rho) only applies AFTER convergence — before that
        // the echo-GAIN-based Geigel test is unreliable (the path gain is still unknown) and the
        // coherence/rho tests divide by the ~0 echo estimate. But "adapt freely" during bootstrap
        // is NOT safe: if the singer is already singing when the music starts (a voice-first
        // opening), a cold filter would happily learn the voice-correlated content and then cancel
        // the voice once it converges. So the bootstrap is gated by a *cold-filter-valid* signal —
        // the echo/mic coherence ramp `g_coh` — which measures how much of the mic the (growing)
        // echo estimate actually explains:
        //   * clean far-only cold start: the estimate aligns with the mic, coherence climbs toward
        //     1, and the gate opens to full adaptation;
        //   * voice-first opening: the estimate can only ever explain the echo part while the mic
        //     also carries voice, so coherence saturates well below COH_HI and the gate stays low,
        //     protecting the voice.
        // Coherence is exactly 0 on the very first blocks (the filter is all-zero, so there is no
        // estimate to correlate with the mic). A small fixed `BOOTSTRAP_IGNITE` floor breaks that
        // chicken-and-egg: it permits a slow trickle of adaptation so the estimate — and thus the
        // coherence signal — can ignite. The floor is small enough that, during a voice-first
        // opening, only a bounded sliver of adaptation occurs before coherence (which stays low)
        // holds the gate down; combined with the dual-filter copy backstop (which only adopts a
        // background update when it provably cancels better) the voice is preserved.
        // `dt_evidence` flags blocks that carry POSITIVE evidence of near-end voice (as opposed to
        // a gate that is merely low because the filter is still cold). Only such blocks re-arm the
        // double-talk hangover; the bootstrap-ignition floor must never arm it (that would deadlock
        // the cold start by holding the gate at 0 forever).
        let mut dt_evidence = false;
        let g_raw = if !far_active {
            0.0 // no reference present -> nothing to learn; freeze (don't chase near-end/noise)
        } else if !self.converged {
            let g_coh = clamp01((coh - COH_LO) / (COH_HI - COH_LO));
            // Positive double-talk evidence during bootstrap requires that an echo estimate
            // actually EXISTS (p_echo above the activity floor: the filter has learned something)
            // yet the mic still isn't coherent with it (low coherence) — i.e. unexplained near-end
            // energy. A truly cold filter (p_echo ~ 0) yields no evidence, so the ignition trickle
            // is allowed to proceed and build the very coherence this test then relies on.
            dt_evidence = self.p_echo > ECHO_FLOOR && coh < COH_LO;
            g_coh.max(BOOTSTRAP_IGNITE)
        } else {
            // FREQUENCY-SHAPED Geigel: how much the mic exceeds the echo the shaped path model
            // predicts for the current reference. Robust to chord changes (see `nearend_ratio`).
            // This is now the PRIMARY adaptation gate post-convergence: it is the near-end detector
            // whose LEVEL does not depend on the filter (the filter only lends x_ref its timing),
            // so it (and only it) decides whether the voice is present. Full step up to
            // NEAREND_FULL_STEP, frozen at the trip, which starts lenient while the path model is
            // young (see NEAREND_TRIP_YOUNG).
            let young = 1.0 - self.gxx_age as f32 / GXX_MATURE_BLOCKS as f32;
            let trip = NEAREND_TRIP + (NEAREND_TRIP_YOUNG - NEAREND_TRIP) * young;
            let g_geigel = clamp01((trip - nearend_ratio) / (trip - NEAREND_FULL_STEP));
            let g_rho = clamp01((RHO_HI - rho) / (RHO_HI - RHO_LO));
            let g_coh = clamp01((coh - COH_LO) / (COH_HI - COH_LO));
            // rho/coh now only act as a SECONDARY throttle with a floor: they may slow adaptation
            // when the filter's residual is high, but — because a far-only chord change ALSO spikes
            // the residual — they must not be able to drive the step to zero on their own. The floor
            // (RHO_COH_FLOOR) guarantees that, as long as the shaped near-end test says "far-only"
            // (g_geigel ≈ 1), the filter keeps adapting fast enough to re-converge through chord
            // changes. When genuine voice is present, g_geigel collapses and gates everything to 0.
            let g_rho_coh = (g_rho * g_coh).max(RHO_COH_FLOOR);
            let g = g_geigel * g_rho_coh;
            // POSITIVE near-end (double-talk) evidence — the only thing allowed to arm the hard
            // freeze hangover — comes ONLY from the FREQUENCY-SHAPED, FILTER-INDEPENDENT near-end
            // test: the mic carries clearly more energy than the SHAPED reference model can explain
            // (g_geigel well below 1, i.e. nearend_ratio past the midpoint of its ramp: 2.5 once the
            // path model is mature).
            //
            // Crucially we do NOT arm the hangover from the coherence/rho terms. Those collapse
            // whenever the foreground filter's residual spikes — and the dominant cause of a
            // residual spike in far-only operation is a CHORD CHANGE (the converged filter is
            // momentarily wrong for the new harmony), NOT near-end voice. The old product-gate test
            // mistook every chord change for double-talk and stranded the filter frozen for the full
            // 30-block hangover, so on moving music it never re-converged and ERLE collapsed. The
            // shaped near-end test does not fire on a far-only chord change (the new chord's echo is
            // still explained by the per-bin path model), so the filter keeps re-adapting through
            // chord changes — while still freezing hard the instant genuine voice (energy the
            // reference cannot explain in ANY band) appears. `g_rho`/`g_coh` still throttle the STEP
            // SIZE via `g` below; they just no longer trigger the hard freeze hangover.
            dt_evidence = g_geigel < 0.5;
            g
        };

        // Hangover: a clearly-double-talk block re-arms the hangover timer; while it counts down we
        // hold the gate at 0 regardless of g_raw. Armed only on positive double-talk evidence (see
        // `dt_evidence`), never on the bare bootstrap-ignition floor.
        if far_active && dt_evidence {
            self.hang = HANG_BLOCKS;
        } else if self.hang > 0 {
            self.hang -= 1;
        }
        // During hangover hold the gate down. Post-convergence that means a hard freeze (0). During
        // bootstrap it instead holds at the ignition floor: a transient false double-talk suspicion
        // (coherence still climbing out of cold) must not fully stall the cold start, yet a real
        // voice-first opening is still limited to the same bounded trickle that protects the voice.
        let g_target = if self.hang > 0 {
            if self.converged {
                0.0
            } else {
                BOOTSTRAP_IGNITE
            }
        } else {
            g_raw
        };

        // Asymmetric smoothing: snap DOWN toward freeze (attack), ramp UP slowly (release).
        if g_target < self.g_adapt {
            self.g_adapt += GATE_ATTACK * (g_target - self.g_adapt);
        } else {
            self.g_adapt += GATE_RELEASE * (g_target - self.g_adapt);
        }
        self.g_adapt = clamp01(self.g_adapt);

        // ----- 6) Divergence guard (hard backstop) -----
        if rho > DIV_RATIO {
            self.div_count = self.div_count.saturating_add(1);
        } else {
            self.div_count = 0;
        }
        if self.div_count >= DIV_RESET_HOLD {
            // Persistent divergence: give up and cold-restart the filters and the DTD (see
            // [`Aec::restart`]: a give-up must genuinely re-bootstrap from cold).
            self.restart();
        } else if self.div_count >= DIV_SHRINK_HOLD {
            // Shrink weights and freeze briefly to bleed off a runaway.
            for v in self.wfg_re.iter_mut() {
                *v *= DIV_SHRINK;
            }
            for v in self.wfg_im.iter_mut() {
                *v *= DIV_SHRINK;
            }
            for v in self.wbg_re.iter_mut() {
                *v *= DIV_SHRINK;
            }
            for v in self.wbg_im.iter_mut() {
                *v *= DIV_SHRINK;
            }
            self.g_adapt = 0.0;
        }

        // ----- 7) Background-filter weight update (the foreground only ever receives copies) -----
        // Build the constrained error spectrum from the BACKGROUND residual: e_frame = [0_B | e].
        // (Background is the filter that actively learns; output is taken from foreground.)
        let mu_eff = MU * self.g_adapt;
        if mu_eff > 0.0 {
            for i in 0..b {
                self.e_re[i] = 0.0;
                self.e_re[b + i] = self.eblk_bg[i];
            }
            for i in 0..n {
                self.e_im[i] = 0.0;
            }
            self.fft.transform(&mut self.e_re, &mut self.e_im, false);

            // Per-bin far-end power over the partition span (running, leaky) + normalization.
            // Accumulate the current sum-of-partition power per bin.
            // Each bin's sum runs over the partitions in age order (p = 0, 1, ...) exactly as a
            // per-bin loop over p would, but partition-major: contiguous rows, no index modulo
            // per (bin, partition).
            let sp = &mut self.q_re[..half]; // scratch: free until the background update
            sp.fill(0.0);
            for p in 0..parts {
                let xs = ((self.head + p) % parts) * half;
                accumulate_power(sp, &self.x_re[xs..xs + half], &self.x_im[xs..xs + half]);
            }
            let mut mean_pow = 0.0f64;
            for k in 0..half {
                self.pxx[k] = PWR_SMOOTH * self.pxx[k] + (1.0 - PWR_SMOOTH) * sp[k] + ANTI_DENORMAL;
                mean_pow += self.pxx[k] as f64;
            }
            let mean_pow = (mean_pow / half as f64) as f32;
            let delta = REG_REL * mean_pow + REG_ABS;

            // Update each partition of the BACKGROUND filter, then gradient-constrain it.
            self.update_background(mu_eff, delta);
        }

        // ----- 8) Dual-filter copy logic -----
        // Adopt the background into the foreground only when it is provably cancelling better.
        // The foreground takes the POST-update background weights, as in Valin's MDF (speex
        // mdf.c, TWO_PATH: W gets its gradient step before the foreground decision, and the
        // updated W is what is copied). The one difference: MDF scores the updated W on the current
        // frame, while p_err_bg scores the weights from before this block's step. Deciding and
        // copying before the update instead (so the copy is exactly the scored weights, as in MDF)
        // moved every metric on the bench in tests/aec_bench.rs by 0.2 dB or less, so this order
        // stays.
        if self.p_err_bg < COPY_MARGIN * self.p_err_fg {
            self.wfg_re.copy_from_slice(&self.wbg_re);
            self.wfg_im.copy_from_slice(&self.wbg_im);
            self.p_err_fg = self.p_err_bg;
        } else if self.p_err_fg < COPY_MARGIN * self.p_err_bg {
            // Background drifted worse than the foreground (e.g. a bad double-talk update slipped
            // through): re-seed it from the trusted foreground so it can't poison future copies.
            self.wbg_re.copy_from_slice(&self.wfg_re);
            self.wbg_im.copy_from_slice(&self.wfg_im);
            self.p_err_bg = self.p_err_fg;
        }

        // ----- 9) Output: foreground residual, optionally gently de-reverberated -----
        if SUPPRESS_ENABLED {
            self.apply_suppressor();
        }
        for i in 0..b {
            self.push_out(sanitize(self.eblk_fg[i]));
        }

        // Save this block's mic as the next block's overlap-save history for the suppressor's
        // [mic_prev | mic_cur] framing. (Done last; the suppressor above consumed the OLD mic_prev.)
        self.mic_prev.copy_from_slice(&self.in_mic[..b]);
    }

    /// Step 7's update of every BACKGROUND partition from the error spectrum in e_re/e_im:
    /// the power-normalized gradient conj(X_p) * E, constrained back to a causal length-B filter,
    /// then a leaky weight update.
    ///
    /// The constraint (IFFT, zero the last B taps, FFT) is two FFTs per partition: 94 of the ~99
    /// a block cost at the 250 ms tail, most of the AEC's time on the audio thread. Every gradient
    /// is the spectrum of a REAL signal, so one complex transform pair constrains two of them:
    /// Z = G_p + i*G_q transforms to g_p + i*g_q, the truncation acts on the two parts separately,
    /// and the conjugate-symmetry split takes G_p' = (Z'[k] + conj(Z'[N-k])) / 2 and G_q' =
    /// (Z'[k] - conj(Z'[N-k])) / 2i back out. The maths is that of one transform pair per
    /// partition; only the rounding differs. With an odd partition count, partition 0 is
    /// constrained on its own.
    fn update_background(&mut self, mu_eff: f32, delta: f32) {
        self.compute_norm(mu_eff, delta);
        let mut p = 0;
        if self.parts % 2 == 1 {
            self.update_background_one(0);
            p = 1;
        }
        while p < self.parts {
            self.update_background_pair(p, p + 1);
            p += 2;
        }
    }

    /// The per-bin NLMS step `mu_eff / (pxx[k] + delta)` into `norm`: the same for every
    /// partition, so it is divided out once per block instead of once per partition.
    fn compute_norm(&mut self, mu_eff: f32, delta: f32) {
        for (n, &p) in self.norm.iter_mut().zip(self.pxx.iter()) {
            *n = mu_eff / (p + delta);
        }
    }

    /// One partition's update with its own constraint transforms (see [`Self::update_background`]).
    fn update_background_one(&mut self, p: usize) {
        let (b, n, half) = (self.block, self.fftn, self.half);
        let xs = ((self.head + p) % self.parts) * half;
        let ws = p * half;
        normalized_gradient(
            &self.x_re[xs..xs + half],
            &self.x_im[xs..xs + half],
            &self.e_re[..half],
            &self.e_im[..half],
            &self.norm,
            &mut self.g_re[..half],
            &mut self.g_im[..half],
        );
        Self::hermitian_expand(&mut self.g_re, &mut self.g_im, n, half);

        // Gradient constraint: IFFT, keep only the first B taps (causal length-B), zero the last
        // B, FFT back. This is the "constrained FDAF" step that removes circular-convolution
        // leakage and is what keeps the filter stable.
        self.fft.transform(&mut self.g_re, &mut self.g_im, true);
        for i in b..n {
            self.g_re[i] = 0.0;
            self.g_im[i] = 0.0;
        }
        self.fft.transform(&mut self.g_re, &mut self.g_im, false);

        // Leaky update of the background weights.
        for k in 0..half {
            let wr = LEAK * self.wbg_re[ws + k] + self.g_re[k];
            let wi = LEAK * self.wbg_im[ws + k] + self.g_im[k];
            self.wbg_re[ws + k] = sanitize(wr);
            self.wbg_im[ws + k] = sanitize(wi);
        }
    }

    /// Partitions `p` and `q` updated through ONE constraint transform pair (see
    /// [`Self::update_background`]). G_p is built in g_re/g_im, G_q in q_re/q_im.
    fn update_background_pair(&mut self, p: usize, q: usize) {
        let (b, n, half, parts) = (self.block, self.fftn, self.half, self.parts);
        let xp = ((self.head + p) % parts) * half;
        let xq = ((self.head + q) % parts) * half;
        normalized_gradient(
            &self.x_re[xp..xp + half],
            &self.x_im[xp..xp + half],
            &self.e_re[..half],
            &self.e_im[..half],
            &self.norm,
            &mut self.g_re[..half],
            &mut self.g_im[..half],
        );
        normalized_gradient(
            &self.x_re[xq..xq + half],
            &self.x_im[xq..xq + half],
            &self.e_re[..half],
            &self.e_im[..half],
            &self.norm,
            &mut self.q_re,
            &mut self.q_im,
        );

        // Pack Z = G_p + i*G_q over all N bins, each spectrum's upper half its mirror's conjugate.
        // The upper half goes first, while the lower half of g still holds G_p.
        for k in half..n {
            let m = n - k;
            self.g_re[k] = self.g_re[m] + self.q_im[m];
            self.g_im[k] = self.q_re[m] - self.g_im[m];
        }
        for k in 0..half {
            let (pr, pi) = (self.g_re[k], self.g_im[k]);
            self.g_re[k] = pr - self.q_im[k];
            self.g_im[k] = pi + self.q_re[k];
        }

        // The constraint on both at once: z = g_p + i*g_q, and zeroing a tap zeroes it in both.
        self.fft.transform(&mut self.g_re, &mut self.g_im, true);
        for i in b..n {
            self.g_re[i] = 0.0;
            self.g_im[i] = 0.0;
        }
        self.fft.transform(&mut self.g_re, &mut self.g_im, false);

        // Split Z' = G_p' + i*G_q' by conjugate symmetry, straight into the leaky update.
        let (wp, wq) = (p * half, q * half);
        for k in 0..half {
            let m = if k == 0 { 0 } else { n - k }; // (n - k) mod n, without a division
            let (ar, ai) = (self.g_re[k], self.g_im[k]);
            let (cr, ci) = (self.g_re[m], self.g_im[m]);
            let wr = LEAK * self.wbg_re[wp + k] + 0.5 * (ar + cr);
            let wi = LEAK * self.wbg_im[wp + k] + 0.5 * (ai - ci);
            self.wbg_re[wp + k] = sanitize(wr);
            self.wbg_im[wp + k] = sanitize(wi);
            let wr = LEAK * self.wbg_re[wq + k] + 0.5 * (ai + ci);
            let wi = LEAK * self.wbg_im[wq + k] + 0.5 * (cr - ar);
            self.wbg_re[wq + k] = sanitize(wr);
            self.wbg_im[wq + k] = sanitize(wi);
        }
    }

    /// The pre-pairing update, one constraint transform pair per partition: the reference the
    /// equivalence test holds [`Self::update_background`] to.
    #[cfg(test)]
    fn update_background_unpaired(&mut self, mu_eff: f32, delta: f32) {
        self.compute_norm(mu_eff, delta);
        for p in 0..self.parts {
            self.update_background_one(p);
        }
    }

    // -------------------------------------------------------------------------------------
    // Frequency-shaped echo-path model (gxx) and the far-end power it maps (x_ref).
    // -------------------------------------------------------------------------------------

    /// Recompute `x_ref` (see its field docs) from the far-end delay line, weighting each
    /// partition's |X_p[k]|^2 by that partition's share of the foreground filter's energy.
    /// Partitions holding a negligible share are skipped. O(parts * half), no allocation.
    fn update_far_ref(&mut self) {
        let half = self.half;
        let parts = self.parts;
        // Each partition's energy is a sequential sum over its bins; four partitions' sums run
        // side by side (each still in bin order) so the adds overlap instead of queueing on one
        // accumulator.
        partition_energies(&self.wfg_re, &self.wfg_im, half, &mut self.part_pow);
        let mut total = 0.0f32;
        for &e in self.part_pow.iter() {
            total += e;
        }
        if total <= EPS {
            // Cold (all-zero) filter: no timing to borrow yet, use the current far-end frame.
            self.x_ref.copy_from_slice(&self.x_pow_now);
            return;
        }
        for v in self.x_ref.iter_mut() {
            *v = 0.0;
        }
        let inv = 1.0 / total;
        for p in 0..parts {
            let w = self.part_pow[p] * inv;
            if w < 1.0e-4 {
                continue;
            }
            let xs = ((self.head + p) % parts) * half;
            accumulate_weighted_power(
                &mut self.x_ref,
                &self.x_re[xs..xs + half],
                &self.x_im[xs..xs + half],
                w,
            );
        }
    }

    /// Fold the current block — confirmed pure echo by the caller — into the path model:
    /// gxx[k] = gxx_dd[k] / gxx_xx[k], a ratio of leaky averages. `seed` restarts both averages
    /// from this block alone (and restarts the young-model leniency with them).
    fn track_path_model(&mut self, seed: bool) {
        for k in 0..self.half {
            if seed {
                self.gxx_dd[k] = self.d_pow[k];
                self.gxx_xx[k] = self.x_ref[k];
            } else {
                self.gxx_dd[k] = leaky(self.gxx_dd[k], self.d_pow[k], GXX_SMOOTH);
                self.gxx_xx[k] = leaky(self.gxx_xx[k], self.x_ref[k], GXX_SMOOTH);
            }
            self.gxx[k] = self.gxx_dd[k] / (self.gxx_xx[k] + REG_ABS);
        }
        self.gxx_valid = true;
        self.gxx_age = if seed {
            1
        } else {
            (self.gxx_age + 1).min(GXX_MATURE_BLOCKS)
        };
    }

    /// Forget the path model entirely (cold start / give-up reset).
    fn clear_path_model(&mut self) {
        for k in 0..self.half {
            self.gxx[k] = 0.0;
            self.gxx_dd[k] = 0.0;
            self.gxx_xx[k] = 0.0;
        }
        self.gxx_valid = false;
        self.gxx_age = 0;
    }

    // -------------------------------------------------------------------------------------
    // Optional residual-echo suppressor (default OFF). Per-bin magnitude Wiener gain driven by the
    // FREQUENCY-SHAPED, FILTER-INDEPENDENT echo-power estimate gxx[k]*x_ref[k] versus the known
    // mic power |D[k]|^2, with a hard high gain floor and a near-end-dominance bypass. See the
    // SUPPRESS_* constant docs for why this is voice-safe by construction.
    //
    // It is OFF by default because the linear AEC + frequency-shaped DTD already reduce the backing
    // past the bar while leaving the voice at full level; any residual suppression that touches the
    // SHARED voice/backing bins inevitably thins the voice a little (the VPIO failure mode). This is
    // kept as a tested, voice-safe lever for cases that want more backing reduction at that cost.
    // -------------------------------------------------------------------------------------
    fn apply_suppressor(&mut self) {
        let b = self.block;
        let n = self.fftn;
        let half = self.half;

        // Block-level near-end-dominance bypass, FILTER-INDEPENDENT: compare the mic power against
        // the EXPECTED echo power the shaped path model predicts. When the mic clearly exceeds it,
        // the surplus is near-end voice and we pass the residual through UNTOUCHED. This does NOT
        // fire merely because the filter is unconverged (far-only still has p_mic ≈ expected echo);
        // it fires only on genuine near-end dominance — the coarse backstop to the per-bin floor.
        let mut shaped_expected = 0.0f64;
        let mut mic_block_pow = 0.0f64;
        for k in 0..half {
            shaped_expected += (self.gxx[k] * self.x_ref[k]) as f64;
            mic_block_pow += self.d_pow[k] as f64;
        }
        if self.gxx_valid
            && mic_block_pow > SUPPRESS_BYPASS_RATIO as f64 * (shaped_expected + REG_ABS as f64)
        {
            for k in 0..half {
                self.sup_gain[k] =
                    sanitize(SUPPRESS_SMOOTH * self.sup_gain[k] + (1.0 - SUPPRESS_SMOOTH) * 1.0);
            }
            return;
        }

        // FFT the foreground residual block (overlap-save framing: [0_B | e]).
        for i in 0..b {
            self.e_re[i] = 0.0;
            self.e_re[b + i] = self.eblk_fg[i];
        }
        for i in 0..n {
            self.e_im[i] = 0.0;
        }
        self.fft.transform(&mut self.e_re, &mut self.e_im, false);

        // Per-bin Wiener gain from the SHAPED, filter-independent echo estimate:
        //   echo_pow[k] = max( gxx[k]*x_ref[k] , |Y_fg[k]|^2 )   (shaped-path ∪ filter estimate)
        //   gain[k]     = clamp( 1 - OVER * echo_pow[k] / |D[k]|^2 , FLOOR, 1 )
        // d_pow[k] (|D[k]|^2, computed once per block) is the denominator: where the voice dominates
        // a bin, echo_pow/|D|^2 → 0 so gain → 1 (voice untouched); where the backing dominates, the
        // ratio → 1 so gain → FLOOR (backing reduced, never erased). Voice never inflates echo_pow
        // (it is not in the reference and the filter never models it), so it cannot self-attenuate.
        for k in 0..half {
            let d_pow = self.d_pow[k] + EPS;
            // The shaped path * far-end power, ∪ the filter's own estimate.
            let ref_echo_pow = self.gxx[k] * self.x_ref[k];
            let echo_pow = ref_echo_pow.max(self.yh_pow[k]);
            // clamp01 maps NaN→0 (f32::clamp would propagate NaN into sup_gain).
            let ratio = clamp01(SUPPRESS_OVER * echo_pow / d_pow);
            let raw_gain = (1.0 - ratio).max(SUPPRESS_FLOOR);
            self.sup_gain[k] =
                sanitize(SUPPRESS_SMOOTH * self.sup_gain[k] + (1.0 - SUPPRESS_SMOOTH) * raw_gain);
            self.e_re[k] *= self.sup_gain[k];
            self.e_im[k] *= self.sup_gain[k];
        }

        Self::hermitian_expand(&mut self.e_re, &mut self.e_im, n, half);
        self.fft.transform(&mut self.e_re, &mut self.e_im, true);
        for i in 0..b {
            self.eblk_fg[i] = sanitize(self.e_re[b + i]);
        }
    }

    // -------------------------------------------------------------------------------------
    // Rebuild the upper half of a spectrum from Hermitian symmetry before an inverse FFT.
    // Bins 0..=half-1 are valid; bins half..n are conj of their mirror.
    // -------------------------------------------------------------------------------------
    #[inline]
    fn hermitian_expand(re: &mut [f32], im: &mut [f32], n: usize, half: usize) {
        for k in half..n {
            let mirror = n - k;
            re[k] = re[mirror];
            im[k] = -im[mirror];
        }
        // DC and Nyquist imaginary parts must be exactly zero for a real signal.
        im[0] = 0.0;
        if n / 2 < half {
            im[n / 2] = 0.0;
        }
    }
}

/// The frequency-domain NLMS gradient conj(X) * E of one partition, power-normalized per bin,
/// into `re`/`im` (half bins). The DC and Nyquist imaginary parts are zeroed, as for any real
/// signal's spectrum (what `hermitian_expand` enforces before an inverse FFT).
///
/// `norm[k]` is the per-bin step `mu_eff / (pxx[k] + delta)` ([`Aec::compute_norm`]).
#[inline]
fn normalized_gradient(
    x_re: &[f32],
    x_im: &[f32],
    e_re: &[f32],
    e_im: &[f32],
    norm: &[f32],
    re: &mut [f32],
    im: &mut [f32],
) {
    let half = re.len();
    let (x_re, x_im, e_re, e_im, norm) = (
        &x_re[..half],
        &x_im[..half],
        &e_re[..half],
        &e_im[..half],
        &norm[..half],
    );
    let im = &mut im[..half];
    for k in 0..half {
        let (xr, xi) = (x_re[k], x_im[k]);
        let (er, ei) = (e_re[k], e_im[k]);
        let norm = norm[k];
        // conj(X) * E = (xr - i xi)(er + i ei)
        re[k] = norm * (xr * er + xi * ei);
        im[k] = norm * (xr * ei - xi * er);
    }
    im[0] = 0.0;
    im[half - 1] = 0.0;
}

/// `acc += w * x` per bin (complex, split re/im) — one partition's term of the partitioned
/// convolution `Y[k] = sum_p W_p[k] * X_p[k]`, accumulated in partition order.
#[inline]
fn complex_mac(
    acc_re: &mut [f32],
    acc_im: &mut [f32],
    w_re: &[f32],
    w_im: &[f32],
    x_re: &[f32],
    x_im: &[f32],
) {
    let n = acc_re.len();
    let (acc_im, w_re, w_im, x_re, x_im) = (
        &mut acc_im[..n],
        &w_re[..n],
        &w_im[..n],
        &x_re[..n],
        &x_im[..n],
    );
    for k in 0..n {
        let (xr, xi) = (x_re[k], x_im[k]);
        let (fr, fi) = (w_re[k], w_im[k]);
        acc_re[k] += fr * xr - fi * xi;
        acc_im[k] += fr * xi + fi * xr;
    }
}

/// `out[p] = sum_k |W_p[k]|^2` for every partition `p` of the flat `parts * half` weight arrays,
/// each sum taken sequentially over `k` (the order a plain per-partition loop adds in), four
/// partitions at a time.
fn partition_energies(w_re: &[f32], w_im: &[f32], half: usize, out: &mut [f32]) {
    let parts = out.len();
    let mut p = 0;
    while p + 4 <= parts {
        let rows = |q: usize| {
            (
                &w_re[q * half..(q + 1) * half],
                &w_im[q * half..(q + 1) * half],
            )
        };
        let ((r0, i0), (r1, i1), (r2, i2), (r3, i3)) =
            (rows(p), rows(p + 1), rows(p + 2), rows(p + 3));
        let (mut e0, mut e1, mut e2, mut e3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for k in 0..half {
            e0 += r0[k] * r0[k] + i0[k] * i0[k];
            e1 += r1[k] * r1[k] + i1[k] * i1[k];
            e2 += r2[k] * r2[k] + i2[k] * i2[k];
            e3 += r3[k] * r3[k] + i3[k] * i3[k];
        }
        out[p] = e0;
        out[p + 1] = e1;
        out[p + 2] = e2;
        out[p + 3] = e3;
        p += 4;
    }
    while p < parts {
        let (r, i) = (
            &w_re[p * half..(p + 1) * half],
            &w_im[p * half..(p + 1) * half],
        );
        let mut e = 0.0f32;
        for k in 0..half {
            e += r[k] * r[k] + i[k] * i[k];
        }
        out[p] = e;
        p += 1;
    }
}

/// `acc[k] += w * |x[k]|^2` per bin.
#[inline]
fn accumulate_weighted_power(acc: &mut [f32], x_re: &[f32], x_im: &[f32], w: f32) {
    let n = acc.len();
    let (x_re, x_im) = (&x_re[..n], &x_im[..n]);
    for k in 0..n {
        let (xr, xi) = (x_re[k], x_im[k]);
        acc[k] += w * (xr * xr + xi * xi);
    }
}

/// `acc[k] += |x[k]|^2` per bin.
#[inline]
fn accumulate_power(acc: &mut [f32], x_re: &[f32], x_im: &[f32]) {
    let n = acc.len();
    let (x_re, x_im) = (&x_re[..n], &x_im[..n]);
    for k in 0..n {
        let (xr, xi) = (x_re[k], x_im[k]);
        acc[k] += xr * xr + xi * xi;
    }
}

#[inline(always)]
fn leaky(prev: f32, x: f32, smooth: f32) -> f32 {
    smooth * prev + (1.0 - smooth) * x
}

/// Clamp to [0, 1], mapping NaN to 0 (so a degenerate ratio can never poison the gate).
///
/// We intentionally do NOT use `f32::clamp` here: `clamp` returns NaN for a NaN input, whereas
/// `f32::max`/`min` propagate the non-NaN operand, so `max(0).min(1)` yields 0 for NaN — the
/// RT-safe behavior we want for the adaptation gate.
#[inline(always)]
#[allow(clippy::manual_clamp)]
fn clamp01(x: f32) -> f32 {
    x.max(0.0).min(1.0)
}

/// `(i + 1) % cap` for an index `i < cap` — without the integer division the runtime `cap`
/// would cost on every sample.
#[inline(always)]
fn ring_next(i: usize, cap: usize) -> usize {
    let j = i + 1;
    if j >= cap {
        0
    } else {
        j
    }
}

// ===========================================================================================
// Tests.
// ===========================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic white-ish noise via xorshift32 (no rand crate).
    struct Rng(u32);
    impl Rng {
        fn new(seed: u32) -> Self {
            Rng(seed | 1)
        }
        fn next_f32(&mut self) -> f32 {
            // xorshift32
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            // map to [-1, 1)
            (x as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    /// A plausible room/speaker impulse response: a bulk delay (round-trip), a few discrete
    /// reflections, and an exponentially-decaying diffuse tail.
    fn make_echo_path(len: usize, bulk_delay: usize) -> Vec<f32> {
        let mut h = vec![0.0f32; len];
        let mut rng = Rng::new(0xC0FFEE);
        for i in bulk_delay..len {
            let t = (i - bulk_delay) as f32;
            let decay = (-t / 1500.0).exp(); // ~31 ms decay constant at 48 kHz
            h[i] = 0.15 * decay * rng.next_f32();
        }
        // Direct/early reflections (the dominant echo energy).
        if bulk_delay < len {
            h[bulk_delay] = 0.5;
        }
        if bulk_delay + 80 < len {
            h[bulk_delay + 80] = -0.3;
        }
        if bulk_delay + 200 < len {
            h[bulk_delay + 200] = 0.2;
        }
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

    fn energy(s: &[f32]) -> f64 {
        s.iter().map(|&v| (v as f64) * (v as f64)).sum()
    }

    fn rms(s: &[f32]) -> f32 {
        (energy(s) / s.len().max(1) as f64).sqrt() as f32
    }

    /// The FFT as it was first written: the textbook iterative radix-2 loop, twiddles read with
    /// a stride from one table. Kept verbatim as the reference [`Fft`] is held to bit for bit.
    struct TextbookFft {
        n: usize,
        rev: Box<[u32]>,
        cos: Box<[f32]>,
        sin: Box<[f32]>,
    }

    impl TextbookFft {
        fn new(n: usize) -> TextbookFft {
            let log2n = n.trailing_zeros();
            let mut rev = vec![0u32; n].into_boxed_slice();
            for i in 0..n {
                let mut x = i as u32;
                let mut r = 0u32;
                for _ in 0..log2n {
                    r = (r << 1) | (x & 1);
                    x >>= 1;
                }
                rev[i] = r;
            }
            let half = n / 2;
            let mut cos = vec![0.0f32; half].into_boxed_slice();
            let mut sin = vec![0.0f32; half].into_boxed_slice();
            for j in 0..half {
                let ang = -2.0 * std::f64::consts::PI * (j as f64) / (n as f64);
                cos[j] = ang.cos() as f32;
                sin[j] = ang.sin() as f32;
            }
            TextbookFft { n, rev, cos, sin }
        }

        fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
            let n = self.n;
            for i in 0..n {
                let j = self.rev[i] as usize;
                if j > i {
                    re.swap(i, j);
                    im.swap(i, j);
                }
            }
            let isign = if inverse { -1.0f32 } else { 1.0f32 };
            let mut len = 2usize;
            while len <= n {
                let half = len / 2;
                let stride = n / len;
                let mut base = 0usize;
                while base < n {
                    let mut tw = 0usize;
                    for j in 0..half {
                        let wr = self.cos[tw];
                        let wi = self.sin[tw] * isign;
                        let a = base + j;
                        let b = a + half;
                        let xr = re[b];
                        let xi = im[b];
                        let tr = wr * xr - wi * xi;
                        let ti = wr * xi + wi * xr;
                        let ur = re[a];
                        let ui = im[a];
                        re[a] = ur + tr;
                        im[a] = ui + ti;
                        re[b] = ur - tr;
                        im[b] = ui - ti;
                        tw += stride;
                    }
                    base += len;
                }
                len <<= 1;
            }
            if inverse {
                let inv = 1.0f32 / (n as f32);
                for i in 0..n {
                    re[i] *= inv;
                    im[i] *= inv;
                }
            }
        }
    }

    /// The restructured FFT (per-stage contiguous twiddles, fused first stages) computes
    /// exactly what the textbook loop computes: identical bits, forward and inverse, every
    /// size the stage code distinguishes, on frames carrying signed zeros the way the AEC's
    /// zero-padded frames do.
    #[test]
    fn fft_matches_the_textbook_loop_bit_for_bit() {
        let mut rng = Rng::new(0xFF7);
        for n in [2usize, 4, 8, 16, 512] {
            let fast = Fft::new(n);
            let reference = TextbookFft::new(n);
            for trial in 0..200 {
                let mut re: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
                let mut im: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
                match trial % 4 {
                    // A real frame zero-padded in its upper half (overlap-save).
                    0 => {
                        im.fill(0.0);
                        re[n / 2..].fill(0.0);
                    }
                    // Signed zeros scattered through both parts.
                    1 => {
                        for k in (0..n).step_by(3) {
                            re[k] = -0.0;
                            im[(k + 1) % n] = if k % 2 == 0 { -0.0 } else { 0.0 };
                        }
                    }
                    // Large dynamic range.
                    2 => {
                        for (k, v) in re.iter_mut().enumerate() {
                            *v *= if k % 5 == 0 { 1.0e6 } else { 1.0e-6 };
                        }
                    }
                    _ => {}
                }
                let inverse = trial % 2 == 1;
                let (mut re2, mut im2) = (re.clone(), im.clone());
                fast.transform(&mut re, &mut im, inverse);
                reference.transform(&mut re2, &mut im2, inverse);
                for k in 0..n {
                    assert_eq!(
                        re[k].to_bits(),
                        re2[k].to_bits(),
                        "n={n} trial={trial} re[{k}]"
                    );
                    assert_eq!(
                        im[k].to_bits(),
                        im2[k].to_bits(),
                        "n={n} trial={trial} im[{k}]"
                    );
                }
            }
        }
    }

    /// FNV-1a over the bit patterns of `vals`, folded into `h`.
    fn fnv(mut h: u64, vals: &[f32]) -> u64 {
        for v in vals {
            for b in v.to_bits().to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01B3);
            }
        }
        h
    }

    /// Pins the canceller's exact output — and its final filter state — on a deterministic run
    /// through every block-path branch: converging on far-end-only audio, double-talk, a silent
    /// far end, an echo-free stretch, a restart, odd and even partition counts (one partition
    /// constrained alone, then pairs) and ragged call sizes. The constants were recorded on the
    /// code before the AEC's loops were restructured for speed (contiguous FFT twiddles,
    /// partition-major power sums, one normalizer division per bin per block, no index
    /// modulo), so they prove those rewrites changed no output bit.
    ///
    /// Only IEEE-exact arithmetic feeds this (the signals use no libm), so the one platform
    /// input is the FFT twiddle table — f64 `cos`/`sin` rounded to f32, which every libm in
    /// practice agrees on. If the ALGORITHM is changed on purpose, re-record the constants.
    #[test]
    fn speaker_path_output_is_pinned() {
        // Colored "music" from filtered xorshift noise, and a sparse echo path: + - * / only.
        let total = 48_000 * 3;
        let mut rng = Rng::new(0xA11CE);
        let (mut lp, mut lp2) = (0.0f32, 0.0f32);
        let far: Vec<f32> = (0..total)
            .map(|i| {
                lp = 0.95 * lp + 0.05 * rng.next_f32();
                lp2 = 0.6 * lp2 + 0.4 * rng.next_f32();
                let x = 2.0 * lp + 0.1 * lp2;
                if (60_000..75_000).contains(&i) {
                    0.0 // a silent far end (transport stopped)
                } else {
                    x
                }
            })
            .collect();
        let taps: [(usize, f32); 4] = [(700, 0.5), (780, -0.3), (1500, 0.12), (4000, 0.05)];
        let mut vrng = Rng::new(0xB0B);
        let mut v_lp = 0.0f32;
        let mic: Vec<f32> = (0..total)
            .map(|i| {
                let echo: f32 = taps
                    .iter()
                    .map(|&(d, g)| if i >= d { g * far[i - d] } else { 0.0 })
                    .sum();
                v_lp = 0.8 * v_lp + 0.2 * vrng.next_f32();
                let voice = if (40_000..55_000).contains(&i) || (100_000..110_000).contains(&i) {
                    0.3 * v_lp
                } else {
                    0.0
                };
                echo + voice
            })
            .collect();

        let mut digests = Vec::new();
        for tail_ms in [250.0f32, 200.0] {
            let mut aec = Aec::new(48_000.0, tail_ms);
            let sizes = [256usize, 173, 512, 1, 1024, 100];
            let (mut pos, mut k) = (0usize, 0usize);
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            let mut out = vec![0.0f32; 1024];
            while pos < total {
                let n = sizes[k % sizes.len()].min(total - pos);
                aec.set_echo_free((80_000..90_000).contains(&pos));
                if k == 700 {
                    aec.restart();
                }
                aec.process(&mic[pos..pos + n], &far[pos..pos + n], &mut out[..n]);
                h = fnv(h, &out[..n]);
                pos += n;
                k += 1;
            }
            for state in [&aec.wfg_re, &aec.wfg_im, &aec.wbg_re, &aec.wbg_im, &aec.pxx] {
                h = fnv(h, state);
            }
            digests.push(h);
        }
        assert_eq!(
            digests,
            [0x4c7b_8e58_e3dd_1658, 0x4902_e7d5_728e_675a],
            "the AEC's output changed"
        );
    }

    /// FFT sanity: forward then inverse recovers the input.
    #[test]
    fn fft_roundtrip() {
        let n = 512;
        let fft = Fft::new(n);
        let mut rng = Rng::new(7);
        let orig_re: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
        let orig_im: Vec<f32> = (0..n).map(|_| rng.next_f32()).collect();
        let mut re = orig_re.clone();
        let mut im = orig_im.clone();
        fft.transform(&mut re, &mut im, false);
        fft.transform(&mut re, &mut im, true);
        for i in 0..n {
            assert!((re[i] - orig_re[i]).abs() < 1e-3, "re[{i}] roundtrip");
            assert!((im[i] - orig_im[i]).abs() < 1e-3, "im[{i}] roundtrip");
        }
    }

    /// Constraining two partitions' gradients through one complex transform pair is the same
    /// maths as one pair per partition: on the same random far-end history, error spectrum and
    /// normalization, both paths land the same background weights to rounding. Run for an odd
    /// partition count (the production 250 ms tail: partition 0 alone, then pairs) and an even
    /// one (pairs only).
    #[test]
    fn paired_constraints_match_one_transform_pair_per_partition() {
        for (tail_ms, parts) in [(250.0, 47), (200.0, 38)] {
            let fill = |seed: u32| {
                let mut aec = Aec::new(48_000.0, tail_ms);
                assert_eq!(aec.parts, parts);
                let mut rng = Rng::new(seed);
                for v in aec.x_re.iter_mut().chain(aec.x_im.iter_mut()) {
                    *v = 10.0 * rng.next_f32();
                }
                for k in 0..aec.half {
                    aec.e_re[k] = rng.next_f32();
                    aec.e_im[k] = rng.next_f32();
                    aec.pxx[k] = 50.0 * (1.5 + rng.next_f32());
                }
                aec.head = 5;
                aec
            };
            let mut paired = fill(0xC0DE);
            let mut reference = fill(0xC0DE);
            paired.update_background(0.37, 1.0e-3);
            reference.update_background_unpaired(0.37, 1.0e-3);

            let pairs = paired.wbg_re.iter().zip(reference.wbg_re.iter());
            let pairs = pairs.chain(paired.wbg_im.iter().zip(reference.wbg_im.iter()));
            let (mut diff, mut scale) = (0.0f32, 0.0f32);
            for (&a, &r) in pairs {
                diff = diff.max((a - r).abs());
                scale = scale.max(r.abs());
            }
            assert!(scale > 0.0, "the update must move the weights");
            let rel = diff / scale;
            assert!(
                rel <= 1.0e-5,
                "{parts} partitions: paired update off by {rel:.2e} of the weights' scale"
            );
        }
    }

    /// Far-end-only segment must achieve strong ERLE; this exercises the core cancellation.
    #[test]
    fn echo_cancellation_erle() {
        let sr = 48_000.0f32;
        let tail_ms = 200.0;
        let mut aec = Aec::new(sr, tail_ms);

        // ~2 s of far-end noise; echo path with a ~30-sample bulk delay.
        let total = 96_000usize;
        let mut rng = Rng::new(0xBEEF);
        let far: Vec<f32> = (0..total).map(|_| 0.3 * rng.next_f32()).collect();
        let h = make_echo_path(600, 30);
        let mic = convolve(&far, &h); // far-end only, no near-end

        let mut out = vec![0.0f32; total];

        // Drive in prime-length chunks (!= block) to exercise arbitrary-length buffering.
        let chunk = 200usize;
        let mut i = 0;
        while i < total {
            let end = (i + chunk).min(total);
            aec.process(&mic[i..end], &far[i..end], &mut out[i..end]);
            i = end;
        }

        // Measure ERLE on the converged tail (skip the first ~1.3 s of convergence). The AEC
        // adds one block of latency, so out[n] cancels the echo of mic[n - BLOCK]; compare with
        // that alignment.
        let lat = BLOCK;
        let start = 64_000usize;
        let mut mic_e = 0.0f64;
        let mut out_e = 0.0f64;
        for n in start..total {
            mic_e += (mic[n - lat] as f64).powi(2);
            out_e += (out[n] as f64).powi(2);
        }
        let erle = 10.0 * (mic_e / (out_e + 1e-12)).log10();
        assert!(
            erle >= 15.0,
            "ERLE too low: {erle:.2} dB (mic_e={mic_e:.3}, out_e={out_e:.3})"
        );
    }

    /// Near-end-only (voice) with a silent reference must pass through essentially untouched:
    /// the filter must not fabricate cancellation when there is no echo to cancel.
    #[test]
    fn near_end_preserved() {
        let sr = 48_000.0f32;
        let mut aec = Aec::new(sr, 200.0);

        let total = 48_000usize;
        // A vowel-like tone (fundamental + harmonics).
        let f0 = 220.0f32;
        let mic: Vec<f32> = (0..total)
            .map(|n| {
                let t = n as f32 / sr;
                0.4 * (2.0 * std::f32::consts::PI * f0 * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * 2.0 * f0 * t).sin()
                    + 0.1 * (2.0 * std::f32::consts::PI * 3.0 * f0 * t).sin()
            })
            .collect();
        let far = vec![0.0f32; total]; // silent reference

        let mut out = vec![0.0f32; total];
        aec.process(&mic, &far, &mut out);

        // The AEC adds exactly one block (BLOCK samples) of algorithmic latency, so out[i]
        // corresponds to mic[i - BLOCK]. Compare with that delay accounted for and skip the
        // leading priming region.
        let lat = BLOCK;
        let diff: Vec<f32> = (2 * lat..total).map(|i| out[i] - mic[i - lat]).collect();
        let refv: Vec<f32> = (2 * lat..total).map(|i| mic[i - lat]).collect();
        let d_rms = rms(&diff);
        let m_rms = rms(&refv);
        let ratio = d_rms / (m_rms + 1e-9);
        assert!(
            ratio < 0.05,
            "near-end distorted: residual/voice = {ratio:.4} (should be <0.05)"
        );
    }

    /// Continuous double-talk: converge on far-end-only, then sing OVER the music. The DTD must
    /// freeze adaptation so the voice survives in the output (it is not modelled/cancelled).
    #[test]
    fn double_talk_preserves_voice() {
        let sr = 48_000.0f32;
        let mut aec = Aec::new(sr, 200.0);

        let h = make_echo_path(600, 30);
        let mut rng = Rng::new(0x1234);

        // Phase 1: ~1.5 s far-end-only to converge. Use an exact multiple of BLOCK so the
        // internal block FIFO sits on a clean boundary between the two process() calls.
        let conv_len = BLOCK * 282; // 72192 samples (~1.5 s)
        let far1: Vec<f32> = (0..conv_len).map(|_| 0.3 * rng.next_f32()).collect();
        let mic1 = convolve(&far1, &h);
        let mut out1 = vec![0.0f32; conv_len];
        aec.process(&mic1, &far1, &mut out1);

        // Phase 2: ~1 s of double-talk: continuing music + a sung tone on top.
        let dt_len = BLOCK * 188; // 48128 samples (~1 s)
        let f0 = 330.0f32;
        let voice: Vec<f32> = (0..dt_len)
            .map(|n| {
                let t = n as f32 / sr;
                0.4 * (2.0 * std::f32::consts::PI * f0 * t).sin()
            })
            .collect();
        // Far-end continues; keep a tail of phase-1 far so the convolution echo is continuous.
        let mut far_full = far1.clone();
        let far2: Vec<f32> = (0..dt_len).map(|_| 0.3 * rng.next_f32()).collect();
        far_full.extend_from_slice(&far2);
        let echo_full = convolve(&far_full, &h);
        // mic during double-talk = echo (of the continuing far) + voice.
        let mic2: Vec<f32> = (0..dt_len)
            .map(|n| echo_full[conv_len + n] + voice[n])
            .collect();

        let mut out2 = vec![0.0f32; dt_len];
        aec.process(&mic2, &far2, &mut out2);

        // The sung tone must be preserved. out2[n] corresponds to voice[n - BLOCK] (the one-block
        // algorithmic latency, with the FIFO on a clean boundary). Recover the voice amplitude by
        // projecting the output onto the known (delayed) voice and compare to the injected level.
        let lat = BLOCK;
        let mut corr = 0.0f64;
        let mut ven = 0.0f64;
        for n in lat..dt_len {
            let v = voice[n - lat] as f64;
            corr += out2[n] as f64 * v;
            ven += v * v;
        }
        let recovered = (corr / (ven + 1e-12)) as f32; // projection coefficient ~ amplitude ratio
        assert!(
            recovered > 0.7,
            "voice ducked during double-talk: recovered={recovered:.3} (should be >0.7)"
        );

        // The converged filter must KEEP cancelling the music through the double-talk (only the
        // weight UPDATE freezes; subtraction continues). Isolate the residual echo by removing
        // the known voice (latency-aligned) from the output, and verify it is well below the raw
        // echo: ERLE during double-talk should still be solidly positive.
        let mut echo_e = 0.0f64;
        let mut resid_e = 0.0f64;
        for n in lat..dt_len {
            let raw_echo = echo_full[conv_len + n - lat] as f64;
            let resid_echo = out2[n] as f64 - voice[n - lat] as f64; // strip the preserved voice
            echo_e += raw_echo * raw_echo;
            resid_e += resid_echo * resid_echo;
        }
        let dt_erle = 10.0 * (echo_e / (resid_e + 1e-12)).log10();
        assert!(
            dt_erle >= 8.0,
            "echo not cancelled during double-talk: ERLE={dt_erle:.2} dB (should be >=8)"
        );

        // And the output must stay finite/bounded (no divergence under double-talk).
        for &v in &out2 {
            assert!(
                v.is_finite() && v.abs() < 10.0,
                "double-talk output blew up: {v}"
            );
        }
    }

    /// Poisoned input (NaN/Inf, huge correlated bursts) must never blow up the output.
    #[test]
    fn no_divergence_blowup() {
        let sr = 48_000.0f32;
        let mut aec = Aec::new(sr, 200.0);

        let total = 24_000usize;
        let mut mic = vec![0.0f32; total];
        let mut far = vec![0.0f32; total];
        let mut rng = Rng::new(99);
        for i in 0..total {
            far[i] = 5.0 * rng.next_f32(); // loud
            mic[i] = far[i]; // perfectly correlated nonsense (worst case for adaptation)
        }
        // Inject NaN/Inf poison.
        mic[1000] = f32::NAN;
        far[2000] = f32::INFINITY;
        mic[3000] = f32::NEG_INFINITY;

        let mut out = vec![0.0f32; total];
        aec.process(&mic, &far, &mut out);

        for &v in &out {
            assert!(v.is_finite(), "non-finite output leaked through");
            assert!(v.abs() < 1e3, "output magnitude exploded: {v}");
        }
    }

    /// Arbitrary (prime) chunk lengths: out.len() must always equal in.len() and stay finite.
    #[test]
    fn arbitrary_length() {
        let sr = 48_000.0f32;
        let mut aec = Aec::new(sr, 150.0);

        let total = 10_000usize;
        let mut rng = Rng::new(55);
        let mic: Vec<f32> = (0..total).map(|_| 0.2 * rng.next_f32()).collect();
        let far: Vec<f32> = (0..total).map(|_| 0.2 * rng.next_f32()).collect();

        let mut produced = 0usize;
        let mut i = 0;
        let chunk = 173usize; // prime, not a multiple of BLOCK
        while i < total {
            let end = (i + chunk).min(total);
            let mut out = vec![0.0f32; end - i];
            aec.process(&mic[i..end], &far[i..end], &mut out);
            assert_eq!(out.len(), end - i, "out length mismatch");
            for &v in &out {
                assert!(v.is_finite(), "non-finite from arbitrary-length path");
            }
            produced += out.len();
            i = end;
        }
        assert_eq!(produced, total);
    }

    // -------------------------------------------------------------------------------------
    // REALISTIC singing-over-backing scenario (the production failure mode).
    //
    // This is the case the user actually hits: a singer sings IN TIME and IN KEY with the
    // backing track. The near-end voice is therefore *musically correlated* with the far-end
    // reference — it shares the same fundamental period and overlapping harmonics. A naive
    // residual-energy-minimizing adaptive filter then "explains" the correlated voice as part
    // of the echo and cancels it, leaving a tiny, choppy voice.
    //
    // The test builds:
    //   * a harmonic-rich "backing" far-end (a saw-like tone stack on a slow chord change),
    //   * a realistic echo path (≈20 ms bulk delay + a short decaying reverb tail),
    //   * a near-end "voice": a *pitched/filtered relative* of the backing — its fundamental is
    //     an octave above the backing root and it tracks the same chord changes, so it lands on
    //     consonant intervals (shared partials) exactly as a real singer would. It is NOT
    //     independent noise; that correlation is the whole point.
    //
    // mic = voice + echo(of backing). We then measure on the voiced region:
    //   * voice_retention: correlation-based energy retention of the clean near-end voice in the
    //     output (projection of out onto the latency-aligned clean voice). Target ≥ 0.92.
    //   * echo_reduction (ERLE): measured on a far-only tail where the singer is silent. Target
    //     ≥ 6 dB (a modest, audible backing reduction — deep cancellation is NOT required).
    // -------------------------------------------------------------------------------------

    /// One musically-correlated singing-over-backing render. Returns
    /// `(voice_retention, echo_reduction_db)` measured as described above.
    fn run_singing_over_backing() -> (f32, f32) {
        let sr = 48_000.0f32;
        // Round-trip ≈ 20 ms; tail spans bulk delay + reverb comfortably.
        let mut aec = Aec::new(sr, 120.0);

        let two_pi = 2.0 * std::f32::consts::PI;

        // ---- Timeline (exact BLOCK multiples so the FIFO sits on clean boundaries) ----
        // Phase A: a backing-only instrumental INTRO that already CHANGES CHORDS — the only
        //          far-only region, used to measure ERLE. Using chord changes here (rather than a
        //          single sustained tonic) is the realistic, demanding case: a converged filter
        //          must keep re-tracking the moving harmony, and ERLE during the song body depends
        //          on that. (A single-chord intro would let the filter converge once and flatter
        //          the ERLE number; real songs move.)
        // Phase B: singing over the backing through more CHORD CHANGES (the musically-correlated
        //          double-talk that must preserve the voice).
        let intro_blocks = 240usize; // ~1.3 s backing-only intro WITH chord changes
        let sing_blocks = 360usize; // ~1.9 s singing-over-backing with chord changes
        let conv_len = BLOCK * intro_blocks;
        let sing_len = BLOCK * sing_blocks;
        let total = conv_len + sing_len;

        // ---- A "chord" progression (I–V–vi–IV), changing every ~0.21 s throughout the WHOLE
        //      render — intro and singing alike, like a real moving harmony. The singer sits an
        //      octave up on the chord fifth, sharing partials with the backing (the musically-
        //      correlated failure trigger). Frequent changes keep the linear filter perpetually
        //      re-converging, which is exactly where the over-cautious DTD strands it with poor
        //      ERLE — the gap the structural suppressor must close.
        let midi_to_hz = |m: f32| 440.0f32 * 2f32.powf((m - 69.0) / 12.0);
        let roots = [48.0f32, 55.0, 57.0, 53.0]; // C3, G3, A3, F3
        let region_len = BLOCK * 40; // ~0.21 s per chord, across the entire render
        let backing_root_hz = |n: usize| midi_to_hz(roots[(n / region_len) % roots.len()]);
        let saw = |f: f32, t: f32| {
            let mut s = 0.0f32;
            for h in 1..=6 {
                s += (1.0 / h as f32) * (two_pi * f * h as f32 * t).sin();
            }
            s
        };
        let far: Vec<f32> = (0..total)
            .map(|n| {
                let t = n as f32 / sr;
                let root = backing_root_hz(n);
                // Triad: root, major third, fifth.
                let third = root * 2f32.powf(4.0 / 12.0);
                let fifth = root * 2f32.powf(7.0 / 12.0);
                0.18 * (saw(root, t) + 0.7 * saw(third, t) + 0.7 * saw(fifth, t))
            })
            .collect();

        // ---- Echo path: ≈20 ms bulk delay (960 samples) + early reflections + reverb tail. ----
        let bulk = 960usize; // 20 ms @ 48 kHz
        let h = make_echo_path(bulk + 1200, bulk);
        let echo = convolve(&far, &h);

        // ---- Voice: a singer doubling the melody — its own INDEPENDENT oscillators (own phase,
        //      vibrato, formant), but landing IN UNISON / at the octave on the SAME chord tones as
        //      the backing, so it shares the backing's fundamentals and harmonics. This is the
        //      "shares fundamental/harmonics" near-end the brief calls for: musically correlated,
        //      yet a genuinely distinct source (NOT a linear function of the reference — a real
        //      voice never is, so a correct AEC must keep it).
        //
        //      Why it stresses the canceller: because the voice energy sits in exactly the bins
        //      the echo occupies and tracks the same note changes in time, a residual-energy-
        //      minimizing filter is tempted to "explain" the voice as part of the echo, and the
        //      echo/mic coherence the double-talk detector relies on is pushed up by the shared
        //      spectrum — the mechanism behind the "voice comes out tiny and choppy" report.
        let mut voice_clean = vec![0.0f32; total];
        for n in conv_len..total {
            let t = n as f32 / sr;
            let root = backing_root_hz(n);
            // Sing the chord FIFTH at the octave: a unison/consonant double of a backing partial.
            let f0 = root * 2.0 * 2f32.powf(7.0 / 12.0);
            // Independent 5.5 Hz vibrato, ±1% (its own phase => not phase-locked to the backing).
            let vib = 1.0 + 0.01 * (two_pi * 5.5 * t + 0.7).sin();
            // Syllabic envelope ~3 Hz: phrase-like onsets/offsets (the "choppiness" stressor).
            let env = 0.5 + 0.5 * (two_pi * 3.0 * t - 1.2).sin();
            let f = f0 * vib;
            // A few voice harmonics (formant-ish rolloff) — these overlap the backing's harmonics.
            let s = (two_pi * f * t + 0.3).sin()
                + 0.6 * (two_pi * 2.0 * f * t + 0.9).sin()
                + 0.3 * (two_pi * 3.0 * f * t + 1.7).sin();
            voice_clean[n] = 0.32 * env * s;
        }

        // ---- mic = voice + echo(of backing). ----
        let mic: Vec<f32> = (0..total).map(|n| voice_clean[n] + echo[n]).collect();

        // ---- Run the AEC in non-block-aligned chunks (exercise the ring buffering). ----
        let mut out = vec![0.0f32; total];
        let chunk = 211usize; // prime
        let mut i = 0;
        while i < total {
            let end = (i + chunk).min(total);
            aec.process(&mic[i..end], &far[i..end], &mut out[i..end]);
            i = end;
        }

        let lat = BLOCK; // one-block algorithmic latency: out[n] <- mic[n - BLOCK]

        // ---- voice_retention: projection of the output onto the clean (latency-aligned)
        //      voice over the singing region, normalized by the clean-voice energy. This is the
        //      correlation-based energy retention: 1.0 == voice passed at full level. ----
        let mut corr = 0.0f64;
        let mut ven = 0.0f64;
        for n in (conv_len + lat)..total {
            let v = voice_clean[n - lat] as f64;
            corr += out[n] as f64 * v;
            ven += v * v;
        }
        let voice_retention = (corr / (ven + 1e-12)) as f32;

        // ---- echo_reduction (ERLE) over the FAR-ONLY intro (which itself changes chords), where
        //      the singer is silent so any output energy is residual echo. Skip the first ~40
        //      blocks of cold-start transient (latency priming + initial pull-in) and measure the
        //      remainder — including across at least one intro chord change, the realistic case. ----
        let erle_start = BLOCK * 40;
        let mut echo_e = 0.0f64;
        let mut out_e = 0.0f64;
        for n in erle_start..conv_len {
            echo_e += (echo[n - lat] as f64).powi(2);
            out_e += (out[n] as f64).powi(2);
        }
        let echo_reduction = 10.0 * (echo_e / (out_e + 1e-12)).log10();

        (voice_retention, echo_reduction as f32)
    }

    /// The headline guarantee: a singer in key/in time with the backing must come out at full
    /// level (voice_retention ≥ 0.92) while the backing is at least modestly reduced (ERLE ≥ 6 dB).
    #[test]
    fn singing_over_backing_preserves_voice() {
        let (voice_retention, echo_reduction) = run_singing_over_backing();
        eprintln!(
            "singing_over_backing: voice_retention={voice_retention:.4}  echo_reduction={echo_reduction:.2} dB"
        );
        assert!(
            voice_retention >= 0.92,
            "VOICE DAMAGED: retention={voice_retention:.4} (must be >= 0.92). \
             The canceller is modelling/cancelling the musically-correlated voice."
        );
        assert!(
            echo_reduction >= 6.0,
            "echo not reduced: ERLE={echo_reduction:.2} dB (must be >= 6.0)"
        );
    }

    /// SHIPPED operating point. The voice-safe LINEAR path was pushed harder (full power-normalized
    /// step `MU = 1.0`, a near-unity dual-filter copy margin, and a high secondary-throttle floor so
    /// the filter re-converges through far-only chord changes) to lift the backing reduction from
    /// the old ~6.5 dB to ~9.5 dB ERLE WITHOUT touching the voice: the adaptive filter can only ever
    /// model the linear echo of the KNOWN reference, and the sung voice is not a linear function of
    /// that reference, so deeper linear cancellation costs essentially no voice (measured retention
    /// ~0.985, down only a hair from the old ~1.0015). The residual SUPPRESSOR was evaluated and
    /// left OFF: on this maximally-correlated unison case it crushed voice_retention to ~0.51 (the
    /// forbidden "voice tiny and choppy" failure), so the linear path is the shipped lever.
    ///
    /// This test pins BOTH the achieved ERLE and the HARD voice constraint so a future regression in
    /// either direction (voice thinning, or the re-convergence tuning silently backsliding) fails CI.
    #[test]
    fn singing_over_backing_voice_safe_cancellation() {
        let (voice_retention, echo_reduction) = run_singing_over_backing();
        eprintln!(
            "voice_safe_cancellation: voice_retention={voice_retention:.4}  echo_reduction={echo_reduction:.2} dB"
        );
        // HARD constraint (never violate): the voice must stay safe on the correlated-voice case.
        assert!(
            voice_retention >= 0.92,
            "VOICE DAMAGED: retention={voice_retention:.4} (HARD constraint: must be >= 0.92)"
        );
        // Backing reduction on the GENTLE (artifact-free) tuning. A more aggressive tuning reached
        // ~9.5 dB but introduced an audible "电音" (musical-noise) artifact on the correlated voice
        // that these energy metrics don't capture, so it was reverted; ~6.5 dB is the shipped floor.
        assert!(
            echo_reduction >= 6.0,
            "backing reduction regressed below the shipped floor: ERLE={echo_reduction:.2} dB (>= 6.0)"
        );
    }
}
