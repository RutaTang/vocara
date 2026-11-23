//! Per-frame voice-timbre metrics and the summaries derived from them (port
//! of `lib/domain/models/voice_metrics.dart`).
//!
//! The curves are `Float32List`s in Dart: values are stored as `f32` and read
//! back widened to `f64`, exactly where the Dart code stores and reads them,
//! so every derived number matches bit for bit. Like the spectrogram, these
//! types compare by identity in Dart: no value equality here, [`Arc`]s where
//! identity matters.

use std::collections::BTreeMap;
use std::f64::consts::{LN_10, LN_2, SQRT_2};
use std::sync::Arc;

use super::spectrogram::{merge_frame_of, merged_span_is_legal, AnalysisFraming, Spectrogram};
use crate::util::dart_num::{
    ceil_to_i64, clamp_f64, clamp_i64, compare_f64, floor_to_i64, min_f64, pow_f64, round_to_i64,
};

/// Per-frame voice-timbre metrics derived from a take's STFT, aligned to the
/// same framing as its [`Spectrogram`] ([hop] / [fft_size] / [sample_rate]; see
/// [`AnalysisFraming`] for where each frame sits in time). Every curve has
/// length [frames]; formant/F0 values are in Hz, or NaN on unvoiced/low-energy
/// frames. See `computeVoiceMetrics` for how each is computed.
#[derive(Clone, Debug)]
pub struct VoiceMetrics {
    pub frames: usize,
    pub hop: i64,
    /// The analysis window length in samples. Frame times are window-CENTRED,
    /// so the mapping needs it as much as [hop].
    pub fft_size: i64,
    pub sample_rate: i64,
    /// Singing Power Ratio (F18): peak dB in 2–4 kHz minus peak dB in 0–2 kHz.
    /// Higher (closer to 0) = more "ring" / projection.
    pub ring: Vec<f32>,
    /// Spectral tilt / alpha ratio (F19): 10·log10(energy>1kHz / energy<1kHz), dB.
    /// Higher = brighter / more pressed; lower = darker / breathier.
    pub tilt: Vec<f32>,
    /// Cepstral Peak Prominence (F20), dB. Higher = cleaner, well-closed tone;
    /// lower = breathier / leakier.
    pub cpp: Vec<f32>,
    /// Per-frame loudness (F9): RMS level in dBFS, normally ≤ 0 (float takes can
    /// exceed 0 when the record gain drives past full scale; chart axes clamp).
    /// The dynamics/phrasing envelope — drops at phrase ends, swells on
    /// crescendos.
    pub loudness: Vec<f32>,
    /// Fundamental frequency F0 in Hz per frame (the pitch / lowest harmonic);
    /// NaN on unvoiced frames.
    pub f0: Vec<f32>,
    /// Formant frequencies in Hz per frame (F17); NaN where not estimated.
    pub f1: Vec<f32>,
    pub f2: Vec<f32>,
    pub f3: Vec<f32>,
    /// H1−H2: level of the 1st harmonic minus the 2nd (dB), per voiced frame; NaN
    /// when unvoiced. A classic register cue — low/negative in chest voice (strong
    /// H2), rising/positive toward head voice & falsetto (H1 dominant), so a step
    /// up as pitch climbs marks the passaggio.
    pub h1h2: Vec<f32>,
    /// 1 where the frame carries enough energy to be voice (drives the averages).
    pub active: Vec<u8>,
    /// Averages over the VOICED frames; NaN when no frame is voiced (breath, a
    /// whisper, a muted mic), which every readout shows as "—".
    pub ring_avg: f64,
    pub tilt_avg: f64,
    pub cpp_avg: f64,
    /// Peak sample level of the clip in dBFS (0 = full scale). NaN when unknown
    /// (the Dart default).
    pub peak_dbfs: f64,
    /// Fraction of samples inside a flat-topped clipped run (0..1). Above ~0.001
    /// the recording is audibly clipped and the spectral metrics are unreliable.
    pub clipped_fraction: f64,
    /// Rough signal-to-noise ratio (dB): the median loudness of voiced frames over
    /// the noise floor (10th-percentile frame loudness). NaN when undetermined
    /// (the Dart default).
    pub snr_db: f64,
}

impl AnalysisFraming for VoiceMetrics {
    fn frames(&self) -> usize {
        self.frames
    }
    fn hop(&self) -> i64 {
        self.hop
    }
    fn fft_size(&self) -> i64 {
        self.fft_size
    }
    fn sample_rate(&self) -> i64 {
        self.sample_rate
    }
}

impl VoiceMetrics {
    /// Resident bytes one frame costs: the nine Float32 curves below plus the
    /// one-byte [active] mask. Everything that budgets metrics by size measures
    /// with this, so adding a curve here is what moves those budgets — there is
    /// no second copy of the number to forget.
    pub const BYTES_PER_FRAME: usize = 9 * 4 + 1;

    /// True when the recording is clipped enough to distort the analysis.
    pub fn is_clipped(&self) -> bool {
        self.clipped_fraction > 0.001
    }

    /// True when the signal-to-noise ratio is too low to trust the timbre metrics.
    pub fn is_low_snr(&self) -> bool {
        self.snr_db.is_finite() && self.snr_db < 10.0
    }

    /// The value of a curve at the frame centred nearest [seconds] (NaN if there
    /// are no frames).
    pub fn sample_at(&self, curve: &[f32], seconds: f64) -> f64 {
        if self.frames == 0 {
            f64::NAN
        } else {
            f64::from(curve[self.frame_at_time(seconds)])
        }
    }
}

/// One clip's metrics and its start relative to the lane's span start
/// (Dart's `({int offsetMs, VoiceMetrics metrics})`).
#[derive(Clone, Debug)]
pub struct VoiceMetricsPart {
    pub offset_ms: i64,
    pub metrics: Arc<VoiceMetrics>,
}

/// Merges several clips' [`VoiceMetrics`] into one lane-spanning analysis for a
/// multi-clip take. Each part carries [offset_ms] — the clip's start relative to
/// the LANE's span start (clip.start_ms − track.start_ms, so ≥ 0) — and its
/// frames are placed at that offset, so frame f of the result's window always
/// starts f·hop/sample_rate seconds after the lane start — its time_of_frame
/// is lane-relative exactly as each part's is clip-relative. Gaps between
/// clips become NaN frames (inactive), which every curve consumer already
/// treats as "no voice"; where clips overlap, the later-starting clip wins.
///
/// Parts must share [hop], [fft_size] and [sample_rate] — they do when computed
/// by the same SpectrogramService defaults; the window length matters because
/// it places every frame in time — and any part whose framing differs from the
/// first is skipped, as are frameless parts. Returns `None` when nothing
/// survives or when two or more parts span past the merged-lane budget, and a
/// lone survivor starting at the lane origin unchanged (the very same [`Arc`]).
/// [ring_avg]/[tilt_avg]/[cpp_avg] are recomputed over the placed voiced frames
/// and the input-quality gate keeps the worst clip's values. Pure.
pub fn merge_voice_metrics(parts: &[VoiceMetricsPart]) -> Option<Arc<VoiceMetrics>> {
    debug_assert!(
        parts.iter().all(|p| p.offset_ms >= 0),
        "offsets are lane-relative and must be >= 0"
    );
    let first = &parts.first()?.metrics;
    // Skip frameless parts too: they carry nothing to place, and must not
    // stretch the output span with pure gap.
    let kept: Vec<&VoiceMetricsPart> = parts
        .iter()
        .filter(|p| {
            let m = &p.metrics;
            m.frames > 0
                && m.hop == first.hop
                && m.fft_size == first.fft_size
                && m.sample_rate == first.sample_rate
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    let frame_of = |offset_ms: i64| merge_frame_of(offset_ms, first.sample_rate, first.hop);
    // A lone part at the lane origin IS the whole lane's analysis — unchanged.
    if kept.len() == 1 && frame_of(kept[0].offset_ms) == 0 {
        return Some(Arc::clone(&kept[0].metrics));
    }

    // Place parts in ascending start order (stable on ties), so a later-starting
    // clip's frames overwrite an earlier clip's where they overlap.
    let mut order: Vec<usize> = (0..kept.len()).collect();
    order.sort_by_key(|&i| kept[i].offset_ms);

    let mut total: i64 = 0;
    for p in &kept {
        let end = frame_of(p.offset_ms).saturating_add(p.metrics.frames as i64);
        if end > total {
            total = end;
        }
    }

    // Nine span-sized Float32 curves plus the active mask follow, so the same
    // span budget the spectrogram merge of this lane honors has to be paid here
    // FIRST — this is the merge that allocates first, which is what let a
    // corrupt or hostile offset size an allocation the sibling merge would have
    // refused. Past it there is no merged view; the caller shows the aggregate
    // as unavailable.
    if !merged_span_is_legal(total) {
        return None;
    }
    let total = total as usize;

    // Pre-fill the curves with NaN (and leave `active` 0) so the frames no clip
    // covers read as gaps, exactly like unvoiced silence within a clip.
    let gap_filled = || vec![f32::NAN; total];
    let mut ring = gap_filled();
    let mut tilt = gap_filled();
    let mut cpp = gap_filled();
    let mut loudness = gap_filled();
    let mut f0 = gap_filled();
    let mut f1 = gap_filled();
    let mut f2 = gap_filled();
    let mut f3 = gap_filled();
    let mut h1h2 = gap_filled();
    let mut active = vec![0u8; total];

    for i in order {
        let p = &kept[i].metrics;
        let off = frame_of(kept[i].offset_ms) as usize;
        let n = p.frames;
        let place = |dst: &mut [f32], src: &[f32]| dst[off..off + n].copy_from_slice(&src[..n]);
        place(&mut ring, &p.ring);
        place(&mut tilt, &p.tilt);
        place(&mut cpp, &p.cpp);
        place(&mut loudness, &p.loudness);
        place(&mut f0, &p.f0);
        place(&mut f1, &p.f1);
        place(&mut f2, &p.f2);
        place(&mut f3, &p.f3);
        place(&mut h1h2, &p.h1h2);
        active[off..off + n].copy_from_slice(&p.active[..n]);
    }

    // Recompute the timbre averages over the placed frames, gated on VOICED
    // frames (valid F0), to match computeVoiceMetrics's single-clip averages.
    // `active` alone is the looser AUDIBLE set; averaging ring/tilt/cpp over it
    // would dilute them with breath/consonant/onset noise, so the same audio
    // would read differently as a multi-clip aggregate than as one clip. f0 is
    // NaN on unvoiced frames AND in the gaps, so `f0 finite && > 0` closely
    // tracks the voiced gate and gap frames never count.
    let avg_voiced = |c: &[f32]| {
        let mut sum = 0.0;
        let mut n = 0;
        for i in 0..total {
            let f = f64::from(f0[i]);
            let v = f64::from(c[i]);
            if active[i] == 1 && f.is_finite() && f > 0.0 && v.is_finite() {
                sum += v;
                n += 1;
            }
        }
        // Nothing voiced: no average (NaN, rendered "—"), as for a single clip.
        if n == 0 {
            f64::NAN
        } else {
            sum / f64::from(n)
        }
    };

    // Aggregate the quality gate across clips so a multi-clip take still flags a
    // clipped / noisy take (otherwise the merged metrics keep the NaN/0 defaults
    // and the quality banner never fires). Be protective: the loudest peak, the
    // worst clipping, and the worst (lowest) SNR drive the warning.
    let mut peak = f64::NEG_INFINITY;
    let mut worst_clip = 0.0;
    let mut worst_snr = f64::INFINITY;
    for part in &kept {
        let p = &part.metrics;
        if p.peak_dbfs.is_finite() && p.peak_dbfs > peak {
            peak = p.peak_dbfs;
        }
        if p.clipped_fraction > worst_clip {
            worst_clip = p.clipped_fraction;
        }
        if p.snr_db.is_finite() && p.snr_db < worst_snr {
            worst_snr = p.snr_db;
        }
    }

    let ring_avg = avg_voiced(&ring);
    let tilt_avg = avg_voiced(&tilt);
    let cpp_avg = avg_voiced(&cpp);
    Some(Arc::new(VoiceMetrics {
        frames: total,
        hop: first.hop,
        fft_size: first.fft_size,
        sample_rate: first.sample_rate,
        ring,
        tilt,
        cpp,
        loudness,
        f0,
        f1,
        f2,
        f3,
        h1h2,
        active,
        ring_avg,
        tilt_avg,
        cpp_avg,
        peak_dbfs: if peak.is_finite() { peak } else { f64::NAN },
        clipped_fraction: worst_clip,
        snr_db: if worst_snr.is_finite() {
            worst_snr
        } else {
            f64::NAN
        },
    }))
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
];

/// A note name with its octave, and the signed cents deviation from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub name: String,
    pub cents: f64,
}

fn name_for_midi(m: i64) -> String {
    format!("{}{}", NOTE_NAMES[m.rem_euclid(12) as usize], m / 12 - 1)
}

/// The nearest equal-tempered note (name + octave) and signed cents deviation
/// for [hz] (A4=440), or `None` when [hz] isn't a valid pitch. e.g. 223 Hz →
/// (name: 'A3', cents: +24.x). (Dart throws on an infinite [hz]; `None` here.)
pub fn note_of(hz: f64) -> Option<Note> {
    if hz.is_nan() || hz <= 0.0 || hz.is_infinite() {
        return None;
    }
    let midi = 69.0 + 12.0 * ((hz / 440.0).ln() / LN_2);
    let m = round_to_i64(midi);
    Some(Note {
        name: name_for_midi(m),
        cents: (midi - m as f64) * 100.0,
    })
}

/// Formats a frequency as a note name + cents, e.g. 223 Hz → "A3 +24¢";
/// "—" for NaN/≤0.
pub fn note_label(hz: f64) -> String {
    let Some(n) = note_of(hz) else {
        return "—".to_string();
    };
    let c = round_to_i64(n.cents);
    format!("{} {}{}¢", n.name, if c >= 0 { '+' } else { '−' }, c.abs())
}

/// The note name + octave for an integer MIDI number, e.g. 57 → "A3".
pub fn note_name_for_midi(midi: i64) -> String {
    name_for_midi(midi)
}

/// A whole-take voice summary derived from [`VoiceMetrics`]: the sung range and
/// comfortable tessitura, an estimated register break (passaggio), and the
/// dynamic span. MIDI fields are `None` when there isn't enough voiced audio.
#[derive(Clone, Debug)]
pub struct VoiceProfile {
    /// Sung range — 5th/95th percentile of voiced F0 (MIDI); `None` if no pitch.
    pub low_midi: Option<i64>,
    pub high_midi: Option<i64>,
    /// Comfortable tessitura — the central 50% (25th/75th percentile) of voiced
    /// F0 where the singer spends most time (MIDI).
    pub tess_lo_midi: Option<i64>,
    pub tess_hi_midi: Option<i64>,
    /// Estimated register break (MIDI), or `None` if not confidently detected.
    pub passaggio_midi: Option<i64>,
    /// How abrupt the break is (the H1−H2 jump in dB at the passaggio); 0 if none.
    pub passaggio_jump_db: f64,
    /// Dynamic span — soft (5th pct), loud (95th pct) and median loudness (dBFS)
    /// over audible frames; NaN when there are no audible frames.
    pub soft_db: f64,
    pub loud_db: f64,
    pub median_db: f64,
}

impl VoiceProfile {
    pub fn has_range(&self) -> bool {
        self.low_midi.is_some() && self.high_midi.is_some()
    }

    pub fn has_passaggio(&self) -> bool {
        self.passaggio_midi.is_some()
    }

    pub fn has_dynamics(&self) -> bool {
        !self.soft_db.is_nan() && !self.loud_db.is_nan()
    }

    pub fn dynamic_range_db(&self) -> f64 {
        if self.has_dynamics() {
            self.loud_db - self.soft_db
        } else {
            f64::NAN
        }
    }
}

/// Linear-interpolated [p]-quantile (0..1) of an already-sorted list.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = clamp_f64(
        p * (sorted.len() - 1) as f64,
        0.0,
        sorted.len() as f64 - 1.0,
    );
    let lo = floor_to_i64(idx) as usize;
    let hi = ceil_to_i64(idx) as usize;
    if lo == hi {
        return sorted[lo];
    }
    sorted[lo] * (hi as f64 - idx) + sorted[hi] * (idx - lo as f64)
}

fn hz_to_midi(hz: f64) -> f64 {
    69.0 + 12.0 * ((hz / 440.0).ln() / LN_2)
}

/// Dart's `List<double>.sort()` (by `compareTo`).
fn sort_doubles(v: &mut [f64]) {
    v.sort_by(|a, b| compare_f64(*a, *b));
}

/// The tuning knobs of [`analyze_voice_profile`], at Dart's defaults.
#[derive(Clone, Copy, Debug)]
pub struct VoiceProfileOptions {
    pub passaggio_min_frames_per_bin: usize,
    pub passaggio_min_span_semis: i64,
    pub passaggio_min_jump_db: f64,
    pub min_frames: usize,
}

impl Default for VoiceProfileOptions {
    fn default() -> Self {
        VoiceProfileOptions {
            passaggio_min_frames_per_bin: 8,
            passaggio_min_span_semis: 6,
            passaggio_min_jump_db: 3.0,
            min_frames: 8,
        }
    }
}

/// Summarizes a take's [`VoiceMetrics`] into a [`VoiceProfile`]: range/tessitura
/// from voiced F0, a passaggio estimate from the largest upward H1−H2 step
/// between adjacent semitone bins, and dynamics from the audible loudness. Pure.
pub fn analyze_voice_profile(m: &VoiceMetrics, opts: VoiceProfileOptions) -> VoiceProfile {
    // Range + tessitura from voiced F0 (in continuous MIDI). Needs enough voiced
    // frames to be meaningful — a 1-frame take is not a "range".
    let mut pitches = Vec::new();
    for i in 0..m.frames {
        let hz = f64::from(m.f0[i]);
        if !hz.is_nan() && hz > 0.0 {
            pitches.push(hz_to_midi(hz));
        }
    }
    let (mut low_midi, mut high_midi, mut tess_lo, mut tess_hi) = (None, None, None, None);
    if pitches.len() >= opts.min_frames {
        sort_doubles(&mut pitches);
        low_midi = Some(round_to_i64(percentile(&pitches, 0.05)));
        high_midi = Some(round_to_i64(percentile(&pitches, 0.95)));
        tess_lo = Some(round_to_i64(percentile(&pitches, 0.25)));
        tess_hi = Some(round_to_i64(percentile(&pitches, 0.75)));
    }

    // Passaggio: median H1−H2 per semitone bin, then the largest upward jump
    // between adjacent (≤3-semitone-apart) sufficiently-populated bins. Bins are
    // confined to the reported [low_midi, high_midi] range so an outlier excursion
    // can't put the marker off the range bar.
    let mut bin_vals: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    if let (Some(lo), Some(hi)) = (low_midi, high_midi) {
        for i in 0..m.frames {
            let hz = f64::from(m.f0[i]);
            let h = f64::from(m.h1h2[i]);
            if hz.is_nan() || hz <= 0.0 || h.is_nan() {
                continue;
            }
            let mr = round_to_i64(hz_to_midi(hz));
            if mr < lo || mr > hi {
                continue;
            }
            bin_vals.entry(mr).or_default().push(h);
        }
    }
    let mut bins: BTreeMap<i64, f64> = BTreeMap::new();
    for (midi, vals) in bin_vals.iter_mut() {
        if vals.len() >= opts.passaggio_min_frames_per_bin {
            sort_doubles(vals);
            bins.insert(*midi, vals[vals.len() / 2]);
        }
    }
    let mut passaggio_midi = None;
    let mut passaggio_jump = 0.0;
    if bins.len() >= 2 {
        // BTreeMap keys are already in ascending order (Dart sorts them).
        let keys: Vec<i64> = bins.keys().copied().collect();
        let (first_key, last_key) = (keys[0], keys[keys.len() - 1]);
        if last_key - first_key >= opts.passaggio_min_span_semis {
            for i in 1..keys.len() {
                if keys[i] - keys[i - 1] > 3 {
                    continue; // skip across big gaps
                }
                // A "break" touching the very top/bottom of the sung range is usually a
                // sparse-bin / low-F0 artifact — but a genuine edge break (e.g. a high
                // belt flip) is real, so accept an edge boundary only when BOTH its bins
                // are well populated (≥ 2× the per-bin minimum), instead of excluding it
                // outright.
                if keys[i - 1] == first_key || keys[i] == last_key {
                    let lo_n = bin_vals.get(&keys[i - 1]).map_or(0, Vec::len);
                    let hi_n = bin_vals.get(&keys[i]).map_or(0, Vec::len);
                    if lo_n < opts.passaggio_min_frames_per_bin * 2
                        || hi_n < opts.passaggio_min_frames_per_bin * 2
                    {
                        continue;
                    }
                }
                let jump = bins[&keys[i]] - bins[&keys[i - 1]];
                if jump > passaggio_jump {
                    passaggio_jump = jump;
                    passaggio_midi = Some(keys[i]);
                }
            }
            if passaggio_jump < opts.passaggio_min_jump_db {
                passaggio_midi = None;
                passaggio_jump = 0.0;
            }
        }
    }

    // Dynamics from audible loudness.
    let mut louds = Vec::new();
    for i in 0..m.frames {
        if m.active[i] == 1 {
            let v = f64::from(m.loudness[i]);
            if v.is_finite() {
                louds.push(v);
            }
        }
    }
    let (mut soft_db, mut loud_db, mut med_db) = (f64::NAN, f64::NAN, f64::NAN);
    if louds.len() >= opts.min_frames {
        sort_doubles(&mut louds);
        soft_db = percentile(&louds, 0.05);
        loud_db = percentile(&louds, 0.95);
        med_db = percentile(&louds, 0.5);
    }

    VoiceProfile {
        low_midi,
        high_midi,
        tess_lo_midi: tess_lo,
        tess_hi_midi: tess_hi,
        passaggio_midi,
        passaggio_jump_db: passaggio_jump,
        soft_db,
        loud_db,
        median_db: med_db,
    }
}

/// Long-term average spectrum: the per-bin average power (in dB) over a take's
/// voiced frames — the take's overall spectral signature (brightness + the
/// singer's-formant cluster), independent of the moment-to-moment notes/vowels.
#[derive(Clone, Debug)]
pub struct Ltas {
    /// Average magnitude per bin in dB (same 20·log10 convention as the
    /// spectrogram); bin b ≈ b·[bin_hz] Hz.
    pub db: Vec<f32>,
    pub bin_hz: f64,
    pub max_hz: f64,
    pub max_db: f64,
    /// Alpha ratio: 10·log10(energy>1 kHz / energy<1 kHz). Higher = brighter.
    pub alpha_ratio: f64,
    /// Singer's-formant prominence: peak LTAS level in 2.6–3.8 kHz minus the peak
    /// below 2 kHz (dB). Higher = a stronger carrying "ring" cluster.
    pub singers_formant_db: f64,
    /// Number of frames averaged (voiced where possible).
    pub frame_count: usize,
}

impl Ltas {
    pub fn has_data(&self) -> bool {
        self.frame_count > 0 && !self.db.is_empty()
    }
}

/// Averages a take's [`Spectrogram`] over its voiced frames (gated by
/// [m].active, falling back to all frames if too few are voiced) into an [`Ltas`].
/// Averaging is done in linear power (10^(dB/10)) then converted back to dB.
pub fn compute_ltas(s: &Spectrogram, m: Option<&VoiceMetrics>) -> Ltas {
    let bins = s.bins;
    let frames = match m {
        None => s.frames,
        Some(m) => s.frames.min(m.frames),
    };
    let mut pow_sum = vec![0.0f64; bins];

    let mut accumulate = |include: &dyn Fn(usize) -> bool| {
        pow_sum.iter_mut().for_each(|p| *p = 0.0);
        let mut n = 0;
        for fr in 0..frames {
            if !include(fr) {
                continue;
            }
            let base = fr * bins;
            for (b, sum) in pow_sum.iter_mut().enumerate() {
                // db = 20·log10(mag) ⇒ mag² = 10^(db/10) = power.
                *sum += pow_f64(10.0, f64::from(s.db[base + b]) / 10.0);
            }
            n += 1;
        }
        n
    };

    let mut n = match m {
        None => 0,
        Some(m) => accumulate(&|fr| m.active[fr] == 1),
    };
    if n < 8 {
        n = accumulate(&|_| true);
    }

    finalize_ltas(&pow_sum, n, bins, s.bin_hz(), s.max_hz)
}

/// Converts an accumulated per-bin power sum ([pow_sum] over [n] frames) into an
/// [`Ltas`] (per-bin dB, alpha ratio, singer's-formant prominence). Shared by the
/// take ([`compute_ltas`]) and the live running accumulator.
pub fn finalize_ltas(pow_sum: &[f64], n: usize, bins: usize, bin_hz: f64, max_hz: f64) -> Ltas {
    let mut db = vec![0.0f32; bins];
    let mut max_db = 0.0;
    if n > 0 {
        max_db = -1e9;
        for b in 0..bins {
            db[b] = (10.0 * ((pow_sum[b] / n as f64 + 1e-12).ln() / LN_10)) as f32;
            if f64::from(db[b]) > max_db {
                max_db = f64::from(db[b]);
            }
        }
    }

    // Dart throws (an inverted clamp range) for a zero-bin spectrum; here every
    // band below is simply empty.
    let bin_of = |hz: f64| clamp_i64(round_to_i64(hz / bin_hz), 0, bins as i64 - 1);
    let band_pow = |lo: i64, hi: i64| {
        let mut e = 0.0;
        let mut b = lo;
        while b < hi && b < bins as i64 {
            e += pow_sum[b as usize];
            b += 1;
        }
        if n > 0 {
            e / n as f64
        } else {
            0.0
        }
    };

    let e_low = band_pow(bin_of(50.0), bin_of(1000.0));
    let e_high = band_pow(bin_of(1000.0), bin_of(min_f64(5000.0, max_hz)));
    let alpha = 10.0 * (((e_high + 1e-12) / (e_low + 1e-12)).ln() / LN_10);

    let peak_db = |lo: i64, hi: i64| {
        let mut p = -1e9;
        let mut b = lo;
        while b < hi && b < bins as i64 {
            if f64::from(db[b as usize]) > p {
                p = f64::from(db[b as usize]);
            }
            b += 1;
        }
        p
    };

    let sf_hi = peak_db(bin_of(2600.0), bin_of(3800.0));
    // From 50 Hz, as the alpha ratio's low band: from bin 0, a DC offset (a
    // cheap interface's, a mic's bias) was read as the voice's low peak, and the
    // singer's formant came out several dB low.
    let sf_lo = peak_db(bin_of(50.0), bin_of(2000.0));
    let sf = if sf_hi <= -1e8 || sf_lo <= -1e8 {
        0.0
    } else {
        sf_hi - sf_lo
    };

    Ltas {
        db,
        bin_hz,
        max_hz,
        max_db,
        alpha_ratio: alpha,
        singers_formant_db: sf,
        frame_count: n,
    }
}

/// Vibrato measured on one sustained note: the pitch oscillation's rate, extent
/// and regularity. [present] is false for short or straight notes.
#[derive(Clone, Debug)]
pub struct Vibrato {
    pub present: bool,
    /// Oscillation rate (Hz); ~5–7 is healthy.
    pub rate_hz: f64,
    /// Half the peak-to-peak pitch swing (± cents).
    pub extent_cents: f64,
    /// 0..1 — how regular the oscillation is (normalized autocorrelation peak).
    pub regularity: f64,
    /// Cycle-to-cycle rate jitter: the coefficient of variation of successive
    /// vibrato-cycle periods (0 = perfectly even rate; ~0.05 is a steady trained
    /// vibrato, > ~0.15 reads as uneven/wobbly). NaN when no vibrato is present.
    pub jitter: f64,
    /// Delay from the note start to vibrato onset (ms), or NaN.
    pub onset_ms: f64,
    /// Per-(voiced)-frame pitch deviation from the note median (cents) — the
    /// vibrato waveform, for drawing.
    pub deviation_cents: Vec<f32>,
}

fn no_vibrato() -> Vibrato {
    Vibrato {
        present: false,
        rate_hz: f64::NAN,
        extent_cents: 0.0,
        regularity: 0.0,
        jitter: f64::NAN,
        onset_ms: f64::NAN,
        deviation_cents: Vec::new(),
    }
}

/// Cycle-to-cycle rate jitter of the detrended vibrato oscillation [osc]: the
/// coefficient of variation (std/mean) of the periods between successive
/// rising zero-crossings. NaN when fewer than ~3 cycles are present.
fn vibrato_jitter(osc: &[f64]) -> f64 {
    let mut crossings = Vec::new();
    for i in 1..osc.len() {
        if osc[i - 1] < 0.0 && osc[i] >= 0.0 {
            crossings.push(i);
        }
    }
    if crossings.len() < 4 {
        return f64::NAN; // need ≥3 full periods
    }
    let mut sum = 0.0;
    let mut periods = Vec::new();
    for i in 1..crossings.len() {
        let p = (crossings[i] - crossings[i - 1]) as f64;
        periods.push(p);
        sum += p;
    }
    let mean = sum / periods.len() as f64;
    if mean <= 0.0 {
        return f64::NAN;
    }
    let mut v = 0.0;
    for p in &periods {
        v += (p - mean) * (p - mean);
    }
    (v / periods.len() as f64).sqrt() / mean
}

/// Removes a best-fit linear trend from [y] into [out] — so a scoop/slide into
/// or out of a sustained note (and slow drift) doesn't contaminate the vibrato.
fn linear_detrend(y: &[f64], out: &mut [f64]) {
    let n = y.len();
    if n < 2 {
        out.iter_mut().take(n).for_each(|o| *o = 0.0);
        return;
    }
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (i, yi) in y.iter().enumerate() {
        let x = i as f64;
        sx += x;
        sy += yi;
        sxx += x * x;
        sxy += x * yi;
    }
    let nf = n as f64;
    let d = nf * sxx - sx * sx;
    let b = if d.abs() < 1e-12 {
        0.0
    } else {
        (nf * sxy - sx * sy) / d
    };
    let a = (sy - b * sx) / nf;
    for i in 0..n {
        out[i] = y[i] - (a + b * i as f64);
    }
}

/// The vibrato detector's knobs, at Dart's defaults.
#[derive(Clone, Copy, Debug)]
pub struct VibratoOptions {
    pub min_rate_hz: f64,
    pub max_rate_hz: f64,
    pub min_extent_cents: f64,
    pub min_regularity: f64,
}

impl Default for VibratoOptions {
    fn default() -> Self {
        VibratoOptions {
            min_rate_hz: 3.5,
            max_rate_hz: 9.0,
            min_extent_cents: 20.0,
            min_regularity: 0.55,
        }
    }
}

/// Vibrato of the note spanning frames [start_frame]..[end_frame] (inclusive).
pub fn analyze_vibrato(
    m: &VoiceMetrics,
    start_frame: i64,
    end_frame: i64,
    opts: VibratoOptions,
) -> Vibrato {
    let lo = start_frame.max(0);
    let frames = m.frames as i64;
    let hi = if end_frame >= frames {
        frames - 1
    } else {
        end_frame
    };
    if hi < lo {
        return no_vibrato();
    }
    vibrato_of(
        |i| f64::from(m.f0[i]),
        lo as usize,
        hi as usize,
        m.sample_rate as f64 / m.hop as f64,
        opts,
    )
}

/// Live vibrato over the trailing [window_sec] (Dart's default: 1.6) of a
/// rolling per-frame F0 history.
pub fn analyze_vibrato_live(
    f0_history: &[f64],
    frame_rate: f64,
    window_sec: f64,
    opts: VibratoOptions,
) -> Vibrato {
    let n = f0_history.len() as i64;
    if n == 0 {
        return no_vibrato();
    }
    let w = round_to_i64(frame_rate * window_sec);
    let lo = if n - w < 0 { 0 } else { n - w };
    vibrato_of(
        |i| f0_history[i],
        lo as usize,
        (n - 1) as usize,
        frame_rate,
        opts,
    )
}

/// Core: vibrato of the per-frame F0 in [f0] over [lo]..[hi] (NaN = unvoiced;
/// uniform frame spacing). Builds the cents deviation on the full grid
/// (interpolating short unvoiced gaps so the time axis is uniform), removes a
/// linear trend (scoop/drift), then finds the dominant oscillation in the
/// [min_rate_hz]..[max_rate_hz] band by normalized autocorrelation (parabolic-
/// interpolated, with a subharmonic guard). Needs ≥ 1 s, ≥ 70 % voiced. Pure.
fn vibrato_of(
    f0: impl Fn(usize) -> f64,
    lo: usize,
    hi: usize,
    frame_rate: f64,
    opts: VibratoOptions,
) -> Vibrato {
    let span = hi - lo + 1;
    if (span as i64) < round_to_i64(frame_rate * 1.0) {
        return no_vibrato(); // need ≥ ~1 s
    }

    // Cents deviation (NaN on unvoiced) relative to the voiced median.
    let mut voiced = Vec::new();
    let mut dev = vec![f64::NAN; span];
    for i in 0..span {
        let hz = f0(lo + i);
        if !hz.is_nan() && hz > 0.0 {
            voiced.push(hz);
        }
    }
    if (voiced.len() as i64) < round_to_i64(span as f64 * 0.7) {
        return no_vibrato(); // too gappy
    }
    let mut sorted = voiced.clone();
    sort_doubles(&mut sorted);
    let med = sorted[sorted.len() / 2];
    for (i, d) in dev.iter_mut().enumerate() {
        let hz = f0(lo + i);
        if !hz.is_nan() && hz > 0.0 {
            *d = 1200.0 * ((hz / med).ln() / LN_2);
        }
    }

    // Interpolate unvoiced gaps so the series is uniform; edges hold the nearest.
    let mut first_v = 0usize;
    while first_v < span && dev[first_v].is_nan() {
        first_v += 1;
    }
    let mut last_v = span as i64 - 1;
    while last_v >= 0 && dev[last_v as usize].is_nan() {
        last_v -= 1;
    }
    if first_v as i64 > last_v {
        return no_vibrato();
    }
    let last_v = last_v as usize;
    for i in 0..first_v {
        dev[i] = dev[first_v];
    }
    for i in last_v + 1..span {
        dev[i] = dev[last_v];
    }
    let mut g = first_v;
    while g <= last_v {
        if !dev[g].is_nan() {
            g += 1;
            continue;
        }
        let mut j = g;
        while j <= last_v && dev[j].is_nan() {
            j += 1;
        }
        let (a, b) = (dev[g - 1], dev[j]);
        // dev[k] = a + (b - a) * (k - g + 1) / (j - g + 1), for k in g..j.
        for (rel, d) in dev[g..j].iter_mut().enumerate() {
            *d = a + (b - a) * (rel + 1) as f64 / (j - g + 1) as f64;
        }
        g = j;
    }

    // Detrend (remove scoop/slide) → the oscillatory signal.
    let mut osc = vec![0.0; span];
    linear_detrend(&dev, &mut osc);

    // (Dart throws on the inverted range when span < 2; the bound below
    // returns no vibrato for it either way.)
    let lag_min = clamp_i64(
        floor_to_i64(frame_rate / opts.max_rate_hz),
        1,
        span as i64 - 1,
    );
    let hi_bound = span as i64 / 2;
    if hi_bound < lag_min + 1 {
        return no_vibrato();
    }
    let lag_max = clamp_i64(
        ceil_to_i64(frame_rate / opts.min_rate_hz),
        lag_min + 1,
        hi_bound,
    );
    let (lag_min, lag_max) = (lag_min as usize, lag_max as usize);

    let mut norm = vec![0.0; lag_max + 1];
    let mut best_lag = lag_min;
    let mut best_norm = -1e30;
    for lag in lag_min..=lag_max {
        let (mut r, mut e1, mut e2) = (0.0, 0.0, 0.0);
        let mut t = 0;
        while t + lag < span {
            r += osc[t] * osc[t + lag];
            e1 += osc[t] * osc[t];
            e2 += osc[t + lag] * osc[t + lag];
            t += 1;
        }
        norm[lag] = if e1 > 0.0 && e2 > 0.0 {
            r / (e1 * e2).sqrt()
        } else {
            0.0
        };
        if norm[lag] > best_norm {
            best_norm = norm[lag];
            best_lag = lag;
        }
    }
    if best_norm <= 0.0 {
        return no_vibrato();
    }

    // Subharmonic guard: prefer the shortest lag that is nearly as strong (the
    // true period rather than an integer multiple of it).
    if let Some(lag) = (lag_min..best_lag).find(|&lag| norm[lag] >= best_norm * 0.92) {
        best_lag = lag;
    }
    // Parabolic interpolation for sub-lag rate precision.
    let mut lag_f = best_lag as f64;
    if best_lag > lag_min && best_lag < lag_max {
        let (y0, y1, y2) = (norm[best_lag - 1], norm[best_lag], norm[best_lag + 1]);
        let den = y0 - 2.0 * y1 + y2;
        if den.abs() > 1e-12 {
            lag_f = best_lag as f64 + 0.5 * (y0 - y2) / den;
        }
    }

    // Regularity is the strength of the STRONGEST periodicity peak; the subharmonic
    // guard above only re-points [best_lag] for the rate, so a true vibrato can't be
    // dropped by the (≥0.92) guard lowering the reported self-similarity.
    let regularity = clamp_f64(best_norm, 0.0, 1.0);
    let raw_rate = frame_rate / lag_f;
    let mut sq = 0.0;
    for v in &osc {
        sq += v * v;
    }
    let extent = (sq / span as f64).sqrt() * SQRT_2;

    // A small tolerance keeps a true vibrato sitting on a band edge from being
    // dropped by lag quantization; the displayed rate is clamped into the band.
    let in_band = raw_rate >= opts.min_rate_hz - 0.6 && raw_rate <= opts.max_rate_hz + 0.6;
    let present = regularity >= opts.min_regularity && extent >= opts.min_extent_cents && in_band;
    let rate_hz = clamp_f64(raw_rate, opts.min_rate_hz, opts.max_rate_hz);

    let mut onset_ms = f64::NAN;
    if present {
        let mut t = 0;
        while t + best_lag < span {
            let mut a = 0.0;
            for k in 0..best_lag {
                let av = osc[t + k].abs();
                if av > a {
                    a = av;
                }
            }
            if a >= extent * 0.5 {
                onset_ms = t as f64 / frame_rate * 1000.0;
                break;
            }
            t += 1;
        }
    }

    Vibrato {
        present,
        rate_hz,
        extent_cents: extent,
        regularity,
        jitter: if present {
            vibrato_jitter(&osc)
        } else {
            f64::NAN
        },
        onset_ms,
        deviation_cents: osc.iter().map(|v| *v as f32).collect(),
    }
}

/// A per-frame vocal-register estimate from H1−H2: 0 = chest, 1 = head/falsetto
/// (NaN where unvoiced), plus the chest/mix/head time fractions. An *indicator*,
/// not a verdict — H1−H2 is individual and also moves with vowel/dynamics.
#[derive(Clone, Debug)]
pub struct RegisterMap {
    /// Per-frame register score in [0,1] (0 = chest, 1 = head); NaN unvoiced.
    pub score: Vec<f32>,
    pub chest_pct: f64,
    pub mix_pct: f64,
    pub head_pct: f64,
    pub voiced_frames: usize,
}

impl RegisterMap {
    pub fn has_data(&self) -> bool {
        self.voiced_frames > 0
    }
}

/// The register mapping's centre and scale (dB), at Dart's defaults
/// (3.5 / 4.0).
#[derive(Clone, Copy, Debug)]
pub struct RegisterOptions {
    pub center_db: f64,
    pub scale_db: f64,
}

impl Default for RegisterOptions {
    fn default() -> Self {
        RegisterOptions {
            center_db: 3.5,
            scale_db: 4.0,
        }
    }
}

/// Maps an H1−H2 value (dB) to a register score in [0,1] (0 = chest, 1 = head)
/// via a soft absolute sigmoid centered near the chest↔head boundary.
pub fn register_score(h1h2: f64, opts: RegisterOptions) -> f64 {
    clamp_f64(
        1.0 / (1.0 + (-(h1h2 - opts.center_db) / opts.scale_db).exp()),
        0.0,
        1.0,
    )
}

/// Per-frame register from H1−H2 via [`register_score`] (absolute mapping, so a
/// take sung entirely in one register isn't artificially split). Pure.
pub fn analyze_register(m: &VoiceMetrics, opts: RegisterOptions) -> RegisterMap {
    let n = m.frames;
    let mut score = vec![f32::NAN; n];
    let (mut chest, mut mix, mut head) = (0usize, 0usize, 0usize);
    for (i, slot) in score.iter_mut().enumerate() {
        let h = f64::from(m.h1h2[i]);
        if !(h.is_finite() && f64::from(m.f0[i]) > 0.0) {
            continue;
        }
        let s = register_score(h, opts);
        *slot = s as f32;
        if s < 0.4 {
            chest += 1;
        } else if s < 0.6 {
            mix += 1;
        } else {
            head += 1;
        }
    }
    let v = chest + mix + head;
    let inv = if v == 0 { 0.0 } else { 100.0 / v as f64 };
    RegisterMap {
        score,
        chest_pct: chest as f64 * inv,
        mix_pct: mix as f64 * inv,
        head_pct: head as f64 * inv,
        voiced_frames: v,
    }
}
