//! The STFT framing every analysis shares, and the spectrogram (port of
//! `lib/domain/models/spectrogram.dart`).
//!
//! The data-heavy analysis types have no value equality (as in Dart, where
//! they compare by identity): they travel behind [`Arc`]s, and identity is
//! [`Arc::ptr_eq`].

use std::sync::Arc;

use crate::util::dart_num::{clamp_i64, round_to_i64};

/// The STFT framing every per-frame analysis is laid out on, and the ONE
/// frame↔time mapping all of them share. Frame f analyses the [fft_size]-sample
/// window that STARTS at sample f·[hop] — the spectrogram column and every
/// voice metric of that frame (YIN and LPC run on windows centred on the same
/// point) describe the audio around the window's CENTRE, f·hop + fftSize/2.
///
/// So a frame is placed at its centre, never at its start: a start-anchored
/// frame reads every onset half a window (~21 ms at 2048/48 kHz) EARLY, so the
/// pitch, loudness and spectrogram views led the waveform and the playhead
/// read a frame two hops ahead of the audio under it. Everything that turns a
/// frame into a time, or a time into a frame — painters, readouts, seeks —
/// goes through here so drawing and reading cannot disagree.
pub trait AnalysisFraming {
    fn frames(&self) -> usize;
    fn hop(&self) -> i64;
    fn fft_size(&self) -> i64;
    fn sample_rate(&self) -> i64;

    /// Seconds from the analysed audio's start to the centre of [frame]'s
    /// analysis window — the moment that frame describes.
    fn time_of_frame(&self, frame: i64) -> f64 {
        ((frame * self.hop()) as f64 + self.fft_size() as f64 / 2.0) / self.sample_rate() as f64
    }

    /// The fractional, unclamped frame index whose window centre sits at
    /// [seconds] — the exact inverse of [`AnalysisFraming::time_of_frame`].
    /// For callers that need a floor or ceiling (a visible frame window)
    /// rather than the nearest frame.
    fn frame_position(&self, seconds: f64) -> f64 {
        (seconds * self.sample_rate() as f64 - self.fft_size() as f64 / 2.0) / self.hop() as f64
    }

    /// The frame whose window centre is nearest [seconds], clamped to the frame
    /// range (0 when there are no frames).
    fn frame_at_time(&self, seconds: f64) -> usize {
        let frames = self.frames();
        if frames == 0 {
            return 0;
        }
        clamp_i64(
            round_to_i64(self.frame_position(seconds)),
            0,
            frames as i64 - 1,
        ) as usize
    }

    /// Where [frame]'s cell begins (seconds) when frames are drawn as
    /// contiguous hop-wide columns: centred on [`AnalysisFraming::time_of_frame`], so a column's
    /// middle is the moment it describes, and [`AnalysisFraming::frame_at_time`] of any time inside
    /// the cell answers [frame]. The spectrogram image spans
    /// `cell_start(0)..cell_end(frames - 1)` — NOT the take's whole length, which
    /// would stretch the columns and drift them along the time axis.
    fn cell_start(&self, frame: i64) -> f64 {
        self.time_of_frame(frame) - self.hop() as f64 / 2.0 / self.sample_rate() as f64
    }

    /// Where [frame]'s cell ends (see [`AnalysisFraming::cell_start`]).
    fn cell_end(&self, frame: i64) -> f64 {
        self.time_of_frame(frame) + self.hop() as f64 / 2.0 / self.sample_rate() as f64
    }
}

/// A computed spectrogram: a [frames] × [bins] grid of magnitudes in dB,
/// covering 0..[max_hz]. Row-major: `db[frame * bins + bin]`. Frequency is
/// linear in bin index (bin b ≈ b * sample_rate / fft_size Hz). Frame times
/// follow [`AnalysisFraming`].
#[derive(Clone, Debug)]
pub struct Spectrogram {
    pub frames: usize,
    pub bins: usize,
    pub max_hz: f64,
    pub sample_rate: i64,
    pub fft_size: i64,
    pub hop: i64,
    /// Magnitudes in dB, row-major (`frame * bins + bin`).
    pub db: Vec<f32>,
    pub min_db: f64,
    pub max_db: f64,
}

impl AnalysisFraming for Spectrogram {
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

impl Spectrogram {
    pub fn freq_of_bin(&self, bin: usize) -> f64 {
        (bin as i64 * self.sample_rate) as f64 / self.fft_size as f64
    }

    pub fn bin_hz(&self) -> f64 {
        self.sample_rate as f64 / self.fft_size as f64
    }

    /// dB value at a frame/bin.
    pub fn value_at(&self, frame: usize, bin: usize) -> f64 {
        f64::from(self.db[frame * self.bins + bin])
    }

    /// The column (dB per bin) whose window is centred nearest [seconds]; empty
    /// if no frames.
    pub fn column_at_time(&self, seconds: f64) -> &[f32] {
        if self.frames == 0 {
            return &[];
        }
        let frame = self.frame_at_time(seconds);
        &self.db[frame * self.bins..frame * self.bins + self.bins]
    }
}

/// The frame a part starting [offset_ms] after the lane's span start occupies
/// on the merged grid, for a [sample_rate]/[hop] framing. An OFFSET, so no
/// half-window term: every frame of the part keeps its own window-centre
/// placement, shifted whole by the part's start. The spectrogram and
/// voice-metrics merges of one take must round this identically: their outputs
/// are read against a single lane axis, so a one-frame disagreement slides the
/// pitch/formant overlays off the columns they annotate.
pub fn merge_frame_of(offset_ms: i64, sample_rate: i64, hop: i64) -> i64 {
    round_to_i64(offset_ms as f64 / 1000.0 * sample_rate as f64 / hop as f64)
}

/// The longest lane span a merged analysis may materialize, in frames. A merge
/// is sized by the lane SPAN, not by the audio in it, so two short clips placed
/// hours apart — or an offset that arrived corrupt from persistence or a sync
/// peer — would allocate the whole gap. At 48 kHz with hop 512 this admits
/// 53 minutes of lane, far past the 10-minute per-clip analysis ceiling that
/// bounds any legal take, and costs ~11 MB of voice-metrics curves or ~257 MB
/// of 214-bin spectrogram.
pub const MAX_MERGED_LANE_FRAMES: i64 = 300 * 1000;

/// Whether a merged lane span of [frames] frames may be materialized. BOTH
/// lane merges ask before they allocate, so an illegal span cannot be admitted
/// by whichever merge a caller happens to run first. Non-positive spans are
/// illegal too: they only arise from offsets that overflowed the frame
/// conversion, and a merge must refuse rather than index by them.
pub fn merged_span_is_legal(frames: i64) -> bool {
    frames > 0 && frames <= MAX_MERGED_LANE_FRAMES
}

/// One clip's spectrogram and its start relative to the lane's span start
/// (Dart's `({int offsetMs, Spectrogram spectrogram})`).
#[derive(Clone, Debug)]
pub struct SpectrogramPart {
    pub offset_ms: i64,
    pub spectrogram: Arc<Spectrogram>,
}

/// Merges several clips' [`Spectrogram`]s into one lane-spanning analysis for a
/// multi-clip take. Each part carries [offset_ms] — the clip's start relative to
/// the LANE's span start (clip.start_ms − track.start_ms, so ≥ 0) — and its
/// frames are placed at that offset, so frame f of the result's window always
/// starts f·hop/sample_rate seconds after the lane start — its time_of_frame
/// is lane-relative exactly as each part's is clip-relative. Gaps between
/// clips become floor (merged min_db) columns, which the heat renderer draws
/// as silence; where clips overlap, the later-starting clip wins.
///
/// Parts must share [bins], [fft_size], [sample_rate] and [hop]; any part whose
/// framing differs from the first is skipped, as are frameless parts. Returns
/// `None` when nothing survives or when two or more parts span past
/// [`MAX_MERGED_LANE_FRAMES`] (or past the cell budget), and a lone survivor
/// starting at the lane origin unchanged (the very same [`Arc`]). Pure.
pub fn merge_spectrograms(parts: &[SpectrogramPart]) -> Option<Arc<Spectrogram>> {
    debug_assert!(
        parts.iter().all(|p| p.offset_ms >= 0),
        "offsets are lane-relative and must be >= 0"
    );
    let first = &parts.first()?.spectrogram;
    // Skip frameless parts too: they carry nothing to place, and must not
    // stretch the output span with pure gap.
    let kept: Vec<&SpectrogramPart> = parts
        .iter()
        .filter(|p| {
            let s = &p.spectrogram;
            s.frames > 0
                && s.bins == first.bins
                && s.fft_size == first.fft_size
                && s.sample_rate == first.sample_rate
                && s.hop == first.hop
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    let frame_of = |offset_ms: i64| merge_frame_of(offset_ms, first.sample_rate, first.hop);
    // A lone part at the lane origin IS the whole lane's analysis — unchanged.
    if kept.len() == 1 && frame_of(kept[0].offset_ms) == 0 {
        return Some(Arc::clone(&kept[0].spectrogram));
    }

    // Place parts in ascending start order (stable on ties), so a later-starting
    // clip's frames overwrite an earlier clip's where they overlap.
    let mut order: Vec<usize> = (0..kept.len()).collect();
    order.sort_by_key(|&i| kept[i].offset_ms);

    let bins = first.bins;
    let mut total: i64 = 0;
    let mut min_db = f64::INFINITY;
    let mut max_db = f64::NEG_INFINITY;
    for p in &kept {
        let end = frame_of(p.offset_ms).saturating_add(p.spectrogram.frames as i64);
        if end > total {
            total = end;
        }
        if p.spectrogram.min_db < min_db {
            min_db = p.spectrogram.min_db;
        }
        if p.spectrogram.max_db > max_db {
            max_db = p.spectrogram.max_db;
        }
    }
    // The merged array is sized by the lane SPAN, not the audio in it: two
    // short clips hours apart would allocate gigabytes of gap columns. Two
    // fail-soft bounds (no merged view rather than an OOM): the shared span
    // budget, which the voice-metrics merge of the same lane applies
    // identically so neither merge can admit a lane the other refuses, and a
    // cell budget for the WIDTH — a wide-bin display merge reaches 256 MB well
    // inside a legal span. Span first, so a saturated frame count cannot wrap
    // `total * bins` negative and sail past the cell budget.
    const MAX_MERGED_CELLS: i64 = 64 * 1000 * 1000; // Float32 cells ≈ 256 MB
    if !merged_span_is_legal(total) || total * bins as i64 > MAX_MERGED_CELLS {
        return None;
    }
    let floor_db = if min_db.is_finite() { min_db } else { 0.0 };

    // Pre-fill with the merged floor so gap columns read as silence (the darkest
    // level any clip carries) instead of a default of 0 dB, which the
    // auto-scaled heat map would render as bright noise.
    let cells = total as usize * bins;
    let mut db = vec![floor_db as f32; cells];
    for i in order {
        let p = &kept[i].spectrogram;
        let at = frame_of(kept[i].offset_ms) as usize * bins;
        let n = (p.frames * bins).min(p.db.len());
        db[at..at + n].copy_from_slice(&p.db[..n]);
    }
    Some(Arc::new(Spectrogram {
        frames: total as usize,
        bins,
        max_hz: first.max_hz,
        sample_rate: first.sample_rate,
        fft_size: first.fft_size,
        hop: first.hop,
        db,
        min_db: floor_db,
        max_db: if max_db.is_finite() { max_db } else { 0.0 },
    }))
}
