//! Synthetic singing-over-backing bench for the AEC (`aec.rs`): the double-talk detector's
//! (DTD) behaviour on realistic material, measured instead of asserted from first principles.
//!
//! The signals are built to stress what the production path actually sees:
//!   * a SINGER: phrases of sung syllables — harmonic source with portamento and a delayed
//!     vibrato, vowel formants, fricative onsets, attack/release envelopes and breath gaps — in
//!     the SAME key as the music backing (so it shares the backing's partials);
//!   * two BACKINGS: music-like harmonic material (pad + plucked chords + bass on a chord
//!     progression) and noise-like broadband, non-stationary material (a drum loop over a
//!     modulated noise wash — the case the old mean-of-ratios path model was biased on);
//!   * a laptop-like ECHO PATH: 20 ms bulk delay (the duplex round trip), a band-limited small
//!     speaker, early reflections and an exponentially decaying diffuse tail — and a reverberant
//!     one (diffuse tail 8 dB above the direct path) for the harmonic backing;
//!   * an ECHO-PATH CHANGE: the laptop moved and the volume went up (new reflections, +1 ms,
//!     louder direct path).
//!
//! One timeline per backing: far-end only (converge, ERLE) -> double-talk (singer over the
//! backing) -> far-end only (did double-talk corrupt the filter?) -> path change (re-convergence);
//! plus the singer alone (near-end only). The metrics:
//!   * ERLE (dB) over far-end-only windows;
//!   * the singer's SNR in the output vs the input during double-talk (the echo counts as noise
//!     on the way in; residual echo AND any voice damage count on the way out), plus the voice
//!     retention (projection of the output onto the clean voice; 1.0 = full level);
//!   * ERLE in windows after the path change.
//!
//! The real-time AEC runs here exactly as in production (250 ms tail, 256-frame callbacks).

use vocara_engine::aec::Aec;

const SR: f32 = 48_000.0;
/// The AEC's one-block algorithmic latency (aec.rs `BLOCK`): out[n] cancels mic[n - BLOCK].
const LAT: usize = 256;
/// Production tail (vpio.rs `AEC_TAIL_MS`).
const TAIL_MS: f32 = 250.0;
/// Production-sized callbacks.
const CHUNK: usize = 256;

const fn secs(s: f32) -> usize {
    (s * SR) as usize
}

// ---------------------------------------------------------------------------------------------
// Deterministic noise + small DSP helpers (no external crates).
// ---------------------------------------------------------------------------------------------

/// xorshift32 (mirrors the AEC unit tests).
struct Rng(u32);
impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in [-1, 1).
    fn uni(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
    /// Approximately unit-variance Gaussian (sum of 4 uniforms, scaled).
    fn gauss(&mut self) -> f32 {
        (self.uni() + self.uni() + self.uni() + self.uni()) * 0.866
    }
    /// Uniform index in [0, n).
    fn pick(&mut self, n: usize) -> usize {
        (self.next_u32() as usize) % n
    }
}

/// RBJ-cookbook biquad (direct form I), used to shape noise and the speaker response.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new(b: [f32; 3], a: [f32; 3]) -> Biquad {
        Biquad {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
    fn wq(f: f32, q: f32) -> (f32, f32, f32) {
        let w = 2.0 * std::f32::consts::PI * f / SR;
        (w.cos(), w.sin(), w.sin() / (2.0 * q))
    }
    fn lowpass(f: f32, q: f32) -> Biquad {
        let (c, _, al) = Self::wq(f, q);
        Biquad::new(
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }
    fn highpass(f: f32, q: f32) -> Biquad {
        let (c, _, al) = Self::wq(f, q);
        Biquad::new(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }
    fn bandpass(f: f32, q: f32) -> Biquad {
        let (c, _, al) = Self::wq(f, q);
        Biquad::new([al, 0.0, -al], [1.0 + al, -2.0 * c, 1.0 - al])
    }
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// In-place radix-2 complex FFT (f64), for the bench's own long convolutions.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        for k in 0..len / 2 {
            let (wi, wr) = (ang * k as f64).sin_cos();
            let mut a = k;
            while a < n {
                let b = a + len / 2;
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                a += len;
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        re.iter_mut().for_each(|v| *v *= s);
        im.iter_mut().for_each(|v| *v *= s);
    }
}

/// Linear convolution `x * h` truncated to `x.len()` (overlap-add FFT; the direct form is far
/// too slow for a 8k-tap room response over 16 s in a debug test build).
fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    let n = (2 * h.len()).next_power_of_two().max(4096);
    let seg = n - h.len() + 1;
    let mut hr = vec![0.0f64; n];
    let mut hi = vec![0.0f64; n];
    for (d, &s) in hr.iter_mut().zip(h) {
        *d = s as f64;
    }
    fft(&mut hr, &mut hi, false);
    let mut y = vec![0.0f64; x.len() + n];
    let mut start = 0;
    while start < x.len() {
        let end = (start + seg).min(x.len());
        let mut re = vec![0.0f64; n];
        let mut im = vec![0.0f64; n];
        for (d, &s) in re.iter_mut().zip(&x[start..end]) {
            *d = s as f64;
        }
        fft(&mut re, &mut im, false);
        for k in 0..n {
            let (a, b) = (re[k], im[k]);
            re[k] = a * hr[k] - b * hi[k];
            im[k] = a * hi[k] + b * hr[k];
        }
        fft(&mut re, &mut im, true);
        for (i, v) in re.iter().enumerate() {
            y[start + i] += v;
        }
        start = end;
    }
    y[..x.len()].iter().map(|&v| v as f32).collect()
}

fn rms(s: &[f32]) -> f32 {
    let e: f64 = s.iter().map(|&v| (v as f64) * (v as f64)).sum();
    (e / s.len().max(1) as f64).sqrt() as f32
}

fn normalize(s: &mut [f32], target_rms: f32) {
    let g = target_rms / rms(s).max(1e-9);
    s.iter_mut().for_each(|v| *v *= g);
}

fn midi_hz(m: f32) -> f32 {
    440.0 * 2f32.powf((m - 69.0) / 12.0)
}

// ---------------------------------------------------------------------------------------------
// Echo path.
// ---------------------------------------------------------------------------------------------

/// A laptop-speaker room response: `bulk` samples of round-trip delay, a direct path of gain
/// `direct`, discrete early reflections `(ms after the direct path, gain)`, an exponentially
/// decaying diffuse tail (`rt60_ms`, direct-to-reverberant ratio `drr_db`), all band-limited by
/// a small speaker (nothing much below ~200 Hz, rolled off above ~9 kHz).
fn room_ir(
    seed: u32,
    bulk: usize,
    direct: f32,
    reflections: &[(f32, f32)],
    rt60_ms: f32,
    drr_db: f32,
) -> Vec<f32> {
    let tail = secs(0.15);
    let len = bulk + tail;
    let mut raw = vec![0.0f32; len];
    raw[bulk] = direct;
    for &(ms, g) in reflections {
        raw[bulk + secs(ms * 1e-3)] += g;
    }
    // Diffuse energy sigma^2 * sum(exp(-13.8 t / T60)) ~= sigma^2 * T60 / 13.8 (in samples).
    let t60 = rt60_ms * 1e-3 * SR;
    let sigma = direct * (10f32.powf(-drr_db / 10.0) * 13.8 / t60).sqrt();
    let onset = bulk + secs(0.002);
    let mut rng = Rng::new(seed);
    let mut air = Biquad::lowpass(6000.0, 0.7);
    for (i, tap) in raw.iter_mut().enumerate().skip(onset) {
        let t = (i - onset) as f32;
        *tap += air.run(sigma * rng.gauss() * (-6.91 * t / t60).exp());
    }
    let mut hp = Biquad::highpass(200.0, 0.7);
    let mut lp = Biquad::lowpass(9000.0, 0.7);
    raw.iter().map(|&v| lp.run(hp.run(v))).collect()
}

fn path_a() -> Vec<f32> {
    room_ir(
        0xA11CE,
        960, // 20 ms round trip
        0.5,
        &[(1.2, -0.25), (3.5, 0.18), (7.9, -0.12), (13.0, 0.08)],
        300.0,
        6.0,
    )
}

/// After the change: the laptop was moved (new reflections, +1 ms) and turned up (+1.6 dB).
fn path_b() -> Vec<f32> {
    room_ir(
        0xB0B,
        1008,
        0.6,
        &[(0.8, 0.3), (2.6, -0.2), (5.5, 0.15), (11.0, -0.1)],
        300.0,
        5.0,
    )
}

// ---------------------------------------------------------------------------------------------
// Signals.
// ---------------------------------------------------------------------------------------------

/// Vowel formants (centre Hz, bandwidth Hz, gain).
const VOWELS: [[(f32, f32, f32); 3]; 5] = [
    [
        (800.0, 80.0, 1.0),
        (1150.0, 90.0, 0.5),
        (2900.0, 120.0, 0.25),
    ], // a
    [
        (400.0, 60.0, 1.0),
        (1600.0, 80.0, 0.3),
        (2700.0, 120.0, 0.2),
    ], // e
    [
        (300.0, 50.0, 1.0),
        (2300.0, 100.0, 0.25),
        (3000.0, 120.0, 0.2),
    ], // i
    [(450.0, 70.0, 1.0), (800.0, 80.0, 0.6), (2830.0, 100.0, 0.1)], // o
    [(325.0, 50.0, 1.0), (700.0, 60.0, 0.3), (2530.0, 170.0, 0.1)], // u
];

fn formant_gain(v: usize, f: f32) -> f32 {
    let mut g = 0.03;
    for &(fc, bw, a) in &VOWELS[v] {
        let x = (f - fc) / (0.5 * bw);
        g += a / (1.0 + x * x).sqrt();
    }
    g
}

/// C major scale (MIDI) over the singer's range, for an in-key melody.
const SCALE: [f32; 11] = [
    57.0, 59.0, 60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0, 74.0,
];

/// One sung syllable.
struct Syllable {
    start: usize,
    len: usize,
    midi: f32,
    vowel: usize,
    fricative: bool,
}

/// The singer over `[from, to)`: phrases of 3-6 legato syllables separated by breaths.
fn singer(total: usize, from: usize, to: usize, seed: u32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut syl = Vec::new();
    let mut t = from;
    let mut deg = 4usize;
    'phrases: loop {
        let n = 3 + rng.pick(4);
        for _ in 0..n {
            let len = secs([0.22, 0.3, 0.42, 0.6][rng.pick(4)]);
            if t + len > to {
                break 'phrases;
            }
            let step = [-2i32, -1, -1, 1, 1, 2, 0][rng.pick(7)];
            deg = (deg as i32 + step).clamp(0, SCALE.len() as i32 - 1) as usize;
            syl.push(Syllable {
                start: t,
                len,
                midi: SCALE[deg],
                vowel: rng.pick(VOWELS.len()),
                fricative: rng.pick(2) == 0,
            });
            t += len;
        }
        t += secs([0.18, 0.25, 0.35][rng.pick(3)]); // breath
    }

    let two_pi = 2.0 * std::f32::consts::PI;
    let mut out = vec![0.0f32; total];
    let mut phase = 0.0f32;
    let mut prev_midi = syl.first().map_or(64.0, |s| s.midi);
    let mut fric = Biquad::highpass(3500.0, 0.7);
    let mut harm = [0.0f32; 32];
    for s in &syl {
        let attack = secs(0.025);
        let release = secs(0.05);
        let glide = secs(0.04);
        let fric_len = if s.fricative { secs(0.05) } else { 0 };
        for i in 0..s.len {
            let n = s.start + i;
            // Portamento from the previous note, then a vibrato that develops after ~150 ms.
            let g = (i as f32 / glide as f32).min(1.0);
            let midi = prev_midi + (s.midi - prev_midi) * g;
            let vib_depth = 0.35 * ((i as f32 / SR - 0.15) / 0.2).clamp(0.0, 1.0);
            let vib = vib_depth * (two_pi * 5.5 * n as f32 / SR).sin();
            let f0 = midi_hz(midi + vib);
            phase = (phase + two_pi * f0 / SR) % two_pi;
            // Harmonic weights refresh every 64 samples (f0 and vowel move slowly).
            if i % 64 == 0 {
                for (k, h) in harm.iter_mut().enumerate() {
                    let f = f0 * (k + 1) as f32;
                    *h = if f < 5000.0 {
                        formant_gain(s.vowel, f) / (k + 1) as f32
                    } else {
                        0.0
                    };
                }
            }
            let mut v = 0.0;
            for (k, &h) in harm.iter().enumerate() {
                if h > 0.0 {
                    v += h * ((k + 1) as f32 * phase).sin();
                }
            }
            // Envelope: the vowel enters after the fricative, attacks, and releases at the end.
            let j = i.saturating_sub(fric_len);
            let env_a = if i < fric_len {
                0.0
            } else {
                0.5 - 0.5 * (std::f32::consts::PI * (j as f32 / attack as f32).min(1.0)).cos()
            };
            let env_r = ((s.len - i) as f32 / release as f32).min(1.0);
            let mut y = v * env_a * env_r;
            if i < fric_len {
                let e = (std::f32::consts::PI * i as f32 / fric_len as f32).sin();
                y += 0.25 * e * fric.run(rng.gauss());
            }
            out[n] = y;
        }
        prev_midi = s.midi;
    }
    out
}

/// Additive saw-ish tone (`h` harmonics, 1/k), phase from absolute time.
fn saw(f: f32, t: f32, h: usize) -> f32 {
    let two_pi = 2.0 * std::f32::consts::PI;
    (1..=h)
        .map(|k| (two_pi * f * k as f32 * t).sin() / k as f32)
        .sum()
}

/// Music-like harmonic backing in C (100 BPM): a sustained pad on a I-vi-IV-V progression
/// (a chord per 2 beats), plucked chord tones an octave up on every beat, and a plucked bass.
fn backing_music(total: usize) -> Vec<f32> {
    let chords: [[f32; 3]; 4] = [
        [60.0, 64.0, 67.0],
        [57.0, 60.0, 64.0],
        [53.0, 57.0, 60.0],
        [55.0, 59.0, 62.0],
    ];
    let beat = secs(0.6);
    let mut out = vec![0.0f32; total];
    for (n, o) in out.iter_mut().enumerate() {
        let t = n as f32 / SR;
        let b = n / beat;
        let chord = &chords[(b / 2) % chords.len()];
        let since = (n % beat) as f32 / SR;
        let ramp = (since / 0.01).min(1.0); // de-click the beat edges
        let pluck = (-since / 0.35).exp() * ramp;
        let mut s = 0.0;
        for &m in chord {
            s += 0.22 * saw(midi_hz(m), t, 8); // pad
            s += 0.30 * pluck * saw(midi_hz(m + 12.0), t, 5); // piano-ish
        }
        let bass_since = (n % (2 * beat)) as f32 / SR;
        let bass_env = (-bass_since / 0.5).exp() * (bass_since / 0.01).min(1.0);
        let fb = midi_hz(chord[0] - 24.0);
        s += 0.8 * bass_env * ((2.0 * std::f32::consts::PI * fb * t).sin() + 0.5 * saw(fb, t, 3));
        *o = s;
    }
    normalize(&mut out, 0.15);
    out
}

/// Noise-like broadband, non-stationary backing (100 BPM): four-on-the-floor kick, snare on 2
/// and 4, 8th-note hi-hats (open on the last 8th of the bar), over a slowly modulated noise wash
/// standing in for distorted guitars / cymbals.
fn backing_noise(total: usize) -> Vec<f32> {
    let mut rng = Rng::new(0xD2D);
    let eighth = secs(0.3);
    let mut snare_bp = Biquad::bandpass(1800.0, 0.6);
    let mut hat_hp = Biquad::highpass(7000.0, 0.7);
    let mut wash_lp = Biquad::lowpass(2500.0, 0.5);
    let mut wash_hp = Biquad::highpass(120.0, 0.7);
    let two_pi = 2.0 * std::f32::consts::PI;
    let mut kick_phase = 0.0f32;
    let mut out = vec![0.0f32; total];
    for (n, o) in out.iter_mut().enumerate() {
        let e8 = n / eighth;
        let since = (n % eighth) as f32 / SR;
        let pos = e8 % 8; // 8ths in a 4/4 bar
        let mut s = 0.0;
        // Kick on every beat: a pitch-dropping sine plus a short click.
        if pos.is_multiple_of(2) {
            let f = 45.0 + 70.0 * (-since / 0.03).exp();
            kick_phase = (kick_phase + two_pi * f / SR) % two_pi;
            s += 1.0 * (-since / 0.15).exp() * kick_phase.sin();
            if since < 0.004 {
                s += 0.4 * rng.uni();
            }
        }
        // Snare on beats 2 and 4: broadband noise burst plus a body tone.
        let w = rng.gauss();
        if pos == 2 || pos == 6 {
            s += 1.2 * (-since / 0.12).exp() * snare_bp.run(w);
            s += 0.3 * (-since / 0.06).exp() * (two_pi * 190.0 * since).sin();
        } else {
            snare_bp.run(w);
        }
        // Hats: closed on every 8th, open on the last one.
        let hat_tau = if pos == 7 { 0.18 } else { 0.035 };
        s += 0.5 * (-since / hat_tau).exp() * hat_hp.run(rng.gauss());
        // Wash: band-limited noise, slowly amplitude-modulated.
        let am = 0.8 + 0.2 * (two_pi * 0.3 * n as f32 / SR).sin();
        s += 0.35 * am * wash_hp.run(wash_lp.run(rng.gauss()));
        *o = s;
    }
    normalize(&mut out, 0.15);
    out
}

// ---------------------------------------------------------------------------------------------
// The bench.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Backing {
    /// Harmonic backing through the laptop-like path.
    Music,
    /// Drum-and-noise backing through the laptop-like path.
    Noise,
    /// Harmonic backing through a reverberant path (an external speaker across the room).
    MusicReverb,
}

/// A reverberant path: diffuse tail 8 dB above the direct path, RT60 400 ms. The room's fine
/// spectral structure (fading over tens of Hz) makes a chord's partials echo at gains the
/// per-bin path model cannot resolve.
fn path_reverb() -> Vec<f32> {
    room_ir(0x2EF, 960, 0.5, &[(1.7, -0.3), (4.2, 0.2)], 400.0, -8.0)
}

/// Timeline (seconds).
const T_DT: f32 = 4.0; // far-end only before this; double-talk from here
const T_DT_END: f32 = 10.0; // double-talk ends; far-end only again
const T_CHANGE: f32 = 12.0; // echo-path change (to `path_b`)
const T_END: f32 = 16.0;

/// Singer seed used by the regression tests.
const SINGER: u32 = 0x5EED;

struct Metrics {
    /// Far-end-only ERLE (dB) over [2, 4) s, converged.
    erle_far: f64,
    /// Singer SNR (dB) in the mic and in the output over the double-talk span: the echo is the
    /// noise on the way in; residual echo AND any damage to the voice are on the way out.
    snr_in: f64,
    snr_out: f64,
    /// Projection of the output onto the clean voice over the double-talk span (1.0 = the voice
    /// at full level; an adapting filter that models the in-key voice pulls it under).
    retention: f64,
    /// Far-end-only ERLE (dB) over [10.5, 12) s: what the double-talk left of the filter.
    erle_post_dt: f64,
    /// ERLE (dB) over [0, .5), [.5, 1), [1, 2), [2, 4) s after the path change.
    erle_change: [f64; 4],
}

impl Metrics {
    fn snr_gain(&self) -> f64 {
        self.snr_out - self.snr_in
    }
}

fn erle(echo: &[f32], out: &[f32], lo: usize, hi: usize) -> f64 {
    let (mut e, mut o) = (0.0f64, 0.0f64);
    for n in lo..hi {
        e += (echo[n - LAT] as f64).powi(2);
        o += (out[n] as f64).powi(2);
    }
    10.0 * (e / (o + 1e-15)).log10()
}

/// Run the whole timeline for `backing` with the singer (seed `seed`) at `ver_db` voice-to-echo
/// ratio: voice RMS over its voiced samples vs the echo RMS over the double-talk span.
fn run(backing: Backing, ver_db: f32, seed: u32) -> Metrics {
    let total = secs(T_END);
    let far = match backing {
        Backing::Music | Backing::MusicReverb => backing_music(total),
        Backing::Noise => backing_noise(total),
    };
    let first_path = match backing {
        Backing::MusicReverb => path_reverb(),
        Backing::Music | Backing::Noise => path_a(),
    };
    let echo_a = convolve(&far, &first_path);
    let echo_b = convolve(&far, &path_b());
    let change = secs(T_CHANGE);
    let echo: Vec<f32> = (0..total)
        .map(|n| if n < change { echo_a[n] } else { echo_b[n] })
        .collect();

    let (dt0, dt1) = (secs(T_DT), secs(T_DT_END));
    let mut voice = singer(total, dt0, dt1, seed);
    let voiced: Vec<f32> = voice[dt0..dt1]
        .iter()
        .copied()
        .filter(|v| v.abs() > 1e-4)
        .collect();
    let target = rms(&echo[dt0..dt1]) * 10f32.powf(ver_db / 20.0);
    let g = target / rms(&voiced).max(1e-9);
    voice.iter_mut().for_each(|v| *v *= g);

    let mic: Vec<f32> = (0..total).map(|n| echo[n] + voice[n]).collect();
    let mut out = vec![0.0f32; total];
    let mut aec = Aec::new(SR, TAIL_MS);
    let mut i = 0;
    while i < total {
        let end = (i + CHUNK).min(total);
        aec.process(&mic[i..end], &far[i..end], &mut out[i..end]);
        i = end;
    }
    assert!(
        out.iter().all(|v| v.is_finite()),
        "{backing:?}: non-finite output"
    );

    // Double-talk span, latency-aligned.
    let (mut vv, mut ee, mut rr, mut vo) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for n in (dt0 + LAT)..(dt1 + LAT) {
        let v = voice[n - LAT] as f64;
        let e = echo[n - LAT] as f64;
        let o = out[n] as f64;
        vv += v * v;
        ee += e * e;
        rr += (o - v) * (o - v);
        vo += o * v;
    }
    let w = |a: f32, b: f32| erle(&echo, &out, secs(a), secs(b));
    let m = Metrics {
        erle_far: w(2.0, T_DT),
        snr_in: 10.0 * (vv / ee).log10(),
        snr_out: 10.0 * (vv / (rr + 1e-15)).log10(),
        retention: vo / vv,
        erle_post_dt: w(T_DT_END + 0.5, T_CHANGE),
        erle_change: [
            w(T_CHANGE, T_CHANGE + 0.5),
            w(T_CHANGE + 0.5, T_CHANGE + 1.0),
            w(T_CHANGE + 1.0, T_CHANGE + 2.0),
            w(T_CHANGE + 2.0, T_END),
        ],
    };
    // The table the tuning was done on (`cargo test --release --test aec_bench -- --nocapture`).
    eprintln!(
        "{backing:?} @ {ver_db:+.0} dB: far-only ERLE {:.1} | double-talk SNR {:.1} -> {:.1} \
         ({:+.1}), retention {:.3} | post-DT ERLE {:.1} | after path change {:.1} {:.1} {:.1} {:.1}",
        m.erle_far,
        m.snr_in,
        m.snr_out,
        m.snr_gain(),
        m.retention,
        m.erle_post_dt,
        m.erle_change[0],
        m.erle_change[1],
        m.erle_change[2],
        m.erle_change[3],
    );
    m
}

// ---------------------------------------------------------------------------------------------
// Regression tests. Each runs one backing's whole timeline at a singer level equal to the echo
// (0 dB): the level where the old double-talk detector's biased path model left adaptation
// running through the singing. Numbers are for the default singer; over six singer seeds the
// means moved (old -> now) music +6.7 -> +9.7 dB SNR gain and 0.83 -> 0.92 retention, reverberant
// +6.5 -> +9.6 dB and 0.83 -> 0.92, noise +18.0 -> +21.7 dB, with far-only ERLE and path-change
// recovery unchanged within ~0.6 dB.
// ---------------------------------------------------------------------------------------------

/// Guards shared by every backing: the re-tuned detector must not buy the voice back with the
/// cancellation (far-only ERLE, what the double-talk leaves of the filter, re-convergence after
/// the path change). The floors sit under both the old and the new tuning.
fn assert_cancellation(b: Backing, m: &Metrics, far: f64, post_dt: f64, change: f64) {
    assert!(
        m.erle_far >= far,
        "{b:?}: far-only ERLE {:.2} dB < {far} dB",
        m.erle_far
    );
    assert!(
        m.erle_post_dt >= post_dt,
        "{b:?}: far-only ERLE after the double-talk {:.2} dB < {post_dt} dB (the singing \
         corrupted the filter)",
        m.erle_post_dt
    );
    assert!(
        m.erle_change[3] >= change,
        "{b:?}: ERLE 2-4 s after the echo-path change {:.2} dB < {change} dB (re-convergence \
         stalled)",
        m.erle_change[3]
    );
}

/// In-key singer over harmonic backing. The old detector read far-only material at ~0.2 of its
/// calibrated ratio, so a singer at the echo's level never froze adaptation: the filter modelled
/// the shared partials and pulled the voice under (old: retention 0.880, SNR gain +7.3 dB; now
/// 0.950, +11.0 dB).
#[test]
fn double_talk_over_music_keeps_the_singer() {
    let b = Backing::Music;
    let m = run(b, 0.0, SINGER);
    assert!(
        m.retention >= 0.92,
        "voice pulled under during double-talk: retention {:.3} < 0.92",
        m.retention
    );
    assert!(
        m.snr_gain() >= 9.5,
        "singer SNR gain {:+.2} dB < +9.5 dB",
        m.snr_gain()
    );
    assert_cancellation(b, &m, 10.5, 18.0, 9.5);
}

/// Drum-and-noise backing: the case the old mean-of-ratios model was most biased on (far-only
/// ratio ~0.09 of calibrated). The singer is not correlated with the backing, so the voice level
/// survives either way; what the blind detector cost was a filter adapting through the singing —
/// garbled cancellation during it (old: SNR gain +15.5 dB; now +21.8 dB) and a corrupted filter
/// after it (old: 16.9 dB far-only ERLE after the double-talk; now 26.7 dB).
#[test]
fn double_talk_over_noise_backing_keeps_the_cancellation() {
    let b = Backing::Noise;
    let m = run(b, 0.0, SINGER);
    assert!(
        m.retention >= 0.99,
        "voice pulled under during double-talk: retention {:.3} < 0.99",
        m.retention
    );
    assert!(
        m.snr_gain() >= 19.0,
        "singer SNR gain {:+.2} dB < +19 dB (adaptation ran through the singing)",
        m.snr_gain()
    );
    assert_cancellation(b, &m, 15.5, 22.0, 9.0);
}

/// Harmonic backing through a reverberant path (old: retention 0.852, SNR gain +6.3 dB; now
/// 0.931, +10.0 dB). Also the regime where a calibrated detector is most at risk of freezing on
/// far-only chord changes, which the far-only floor guards.
#[test]
fn double_talk_through_a_reverberant_path_keeps_the_singer() {
    let b = Backing::MusicReverb;
    let m = run(b, 0.0, SINGER);
    assert!(
        m.retention >= 0.90,
        "voice pulled under during double-talk: retention {:.3} < 0.90",
        m.retention
    );
    assert!(
        m.snr_gain() >= 8.5,
        "singer SNR gain {:+.2} dB < +8.5 dB",
        m.snr_gain()
    );
    assert_cancellation(b, &m, 10.0, 16.0, 7.0);
}

/// Near-end only: the singer with the backing stopped. With no reference there is nothing to
/// adapt toward, so the voice must come out as it went in (one block late).
#[test]
fn a_singer_alone_passes_untouched() {
    let total = secs(3.0);
    let voice: Vec<f32> = singer(total, secs(0.2), total, SINGER)
        .iter()
        .map(|v| 0.1 * v)
        .collect();
    let far = vec![0.0f32; total];
    let mut out = vec![0.0f32; total];
    let mut aec = Aec::new(SR, TAIL_MS);
    let mut i = 0;
    while i < total {
        let end = (i + CHUNK).min(total);
        aec.process(&voice[i..end], &far[i..end], &mut out[i..end]);
        i = end;
    }
    let (mut vv, mut dd) = (0.0f64, 0.0f64);
    for n in LAT..total {
        let v = voice[n - LAT] as f64;
        vv += v * v;
        dd += (out[n] as f64 - v).powi(2);
    }
    let snr = 10.0 * (vv / (dd + 1e-30)).log10();
    eprintln!("singer alone: output SNR {snr:.1} dB");
    assert!(snr >= 60.0, "near-end-only voice altered: SNR {snr:.1} dB");
}
