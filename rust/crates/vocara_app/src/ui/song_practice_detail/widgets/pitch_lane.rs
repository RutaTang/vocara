//! Port of `ui/song_practice_detail/widgets/pitch_lane.dart`: the
//! Logic/Cubase-style pitch sub-lane folded out under a vocal take — the
//! take's pitch as note blocks (quantized to semitones, tinted green→red by
//! how in tune) with the fine F0 contour inside, an in-tune % score and a
//! readout of the note under the playhead — and the pitch analysis the pitch
//! views share: note segmentation, the semitone range, the flagged notes,
//! per-note vibrato (each memoized per take), and the visible-frame window
//! every analysis lane painter walks.
//!
//! Read-only — a reference, not pitch correction: the lane takes no input
//! (Dart wrapped it in `IgnorePointer`; an iced canvas with no `update` takes
//! none).
//!
//! The playhead readout: Dart's `_PlayheadNoteReadout` followed the position
//! notifier itself and rebuilt only when its text changed, because a
//! per-tick rebuild there re-laid-out the whole screen. An iced view is
//! rebuilt from state on every message anyway; the readout is a plain text
//! computed from the position the screen passes in ([`playhead_note_text`]).
//! The note blocks keep their own cache, cleared only when what they show
//! changes (Dart's `RepaintBoundary` + `shouldRepaint`), so a playhead tick
//! redraws only the playhead.
//!
//! An F0 the analysis never produces — infinite — reads as unvoiced here; in
//! Dart it reached `round()` / `ceil()`, which threw.

use std::collections::{BTreeSet, HashMap};
use std::f64::consts::LN_2;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use iced::alignment::Vertical;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::text::LineHeight;
use iced::widget::{canvas as canvas_widget, column, container, rich_text, row, span, stack, text};
use iced::{
    mouse, Color, Element, Length, Padding, Pixels, Point, Rectangle, Renderer, Size, Theme,
};

use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::spectrogram::AnalysisFraming;
use vocara_model::models::voice_metrics::{analyze_vibrato, Vibrato, VibratoOptions, VoiceMetrics};
use vocara_model::util::dart_num::{ceil_to_i64, compare_f64, floor_to_i64, round_to_i64};

use crate::ui::core::theme::{alpha, fill, font, Studio};
use crate::ui::core::widgets::painter::{KeyedCache, Same};
use crate::ui::core::widgets::studio_hint::lane_hint;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::waveform_painter::PlayheadPainter;

#[cfg(test)]
mod tests;

/// Height of the inline pitch sub-lane that folds out under a vocal take
/// (`kPitchLaneHeight`).
pub const PITCH_LANE_HEIGHT: f32 = 96.0;

/// In-tune color (green); off-pitch ramps toward `Studio.record`.
const IN_TUNE: Color = Studio::IN_TUNE;

/// A sung note within ±this many cents of its semitone counts as "in tune".
pub const IN_TUNE_CENTS: f64 = 25.0;

const SEMI_NAMES: [&str; 12] = [
    "C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B",
];

fn hz_to_midi(hz: f64) -> f64 {
    69.0 + 12.0 * ((hz / 440.0).ln() / LN_2)
}

/// A frame's F0 as a sung pitch, or `None` for an unvoiced frame (NaN, ≤ 0
/// or — never produced by the analysis — infinite; see the module docs).
fn voiced_hz(hz: f32) -> Option<f64> {
    let hz = f64::from(hz);
    (hz.is_finite() && hz > 0.0).then_some(hz)
}

/// Frame `fr`'s F0 (NaN past the curve: the curves are `frames` long, so
/// only a malformed analysis gets there).
fn f0_at(m: &VoiceMetrics, fr: usize) -> f32 {
    m.f0.get(fr).copied().unwrap_or(f32::NAN)
}

/// `C4`, `F♯3`, ... for a MIDI semitone (Dart's `%` is never negative, its
/// `~/` truncates).
fn semi_name(semi: i64) -> String {
    format!(
        "{}{}",
        SEMI_NAMES[semi.rem_euclid(12) as usize],
        semi / 12 - 1
    )
}

/// `+12¢` / `−7¢` (a true minus sign).
fn cents_str(cents: f64) -> String {
    let c = round_to_i64(cents);
    format!("{}{}¢", if c >= 0 { '+' } else { '−' }, c.unsigned_abs())
}

// ------------------------------------------------------------------ analysis

/// The semitone range a pitch view shows (`({int lo, int hi})`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemitoneRange {
    pub lo: i64,
    pub hi: i64,
}

/// C3..C5: the range shown with nothing voiced (and with no analysis).
pub const DEFAULT_SEMITONE_RANGE: SemitoneRange = SemitoneRange { lo: 48, hi: 72 };

/// The semitone range to display for `m`'s F0: the voiced min/max ± 2
/// semitones, widened to at least one octave. Falls back to C3–C5 when
/// nothing is voiced.
pub fn pitch_semitone_range(m: &VoiceMetrics) -> SemitoneRange {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &hz in &m.f0 {
        let Some(hz) = voiced_hz(hz) else { continue };
        let s = hz_to_midi(hz);
        if s < lo {
            lo = s;
        }
        if s > hi {
            hi = s;
        }
    }
    if lo > hi {
        return DEFAULT_SEMITONE_RANGE; // no voiced frames → C3..C5
    }
    let (mut lo_i, mut hi_i) = (floor_to_i64(lo) - 2, ceil_to_i64(hi) + 2);
    while hi_i - lo_i < 12 {
        lo_i -= 1;
        hi_i += 1;
    }
    SemitoneRange { lo: lo_i, hi: hi_i }
}

/// An inclusive range of frame indices; empty when `first > last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameWindow {
    pub first: i64,
    pub last: i64,
}

impl FrameWindow {
    pub fn is_empty(&self) -> bool {
        self.first > self.last
    }
}

/// [`lane_visible_frames`]' default margin: stroke caps and miter joins
/// reach past their vertex.
pub const LANE_MARGIN_PX: f64 = 8.0;

/// The frames of `m` whose drawing can reach a lane `width` px wide that
/// shows `dur_ms` of timeline from `view_start_ms`, for frames placed from
/// `start_ms`: the frames under the viewport plus `margin_px` each side
/// (stroke caps and miter joins reach past their vertex), clamped to the
/// take. The lane painters clip to their box, so skipping frames outside this
/// range changes nothing on screen — it keeps a zoomed-in long take from
/// walking (and building paths through) every one of its frames. Empty when
/// `first > last`.
pub fn lane_visible_frames(
    m: &(impl AnalysisFraming + ?Sized),
    start_ms: i64,
    view_start_ms: i64,
    dur_ms: i64,
    width: f64,
    margin_px: f64,
) -> FrameWindow {
    let frames = m.frames() as i64;
    if frames == 0 {
        return FrameWindow { first: 0, last: -1 };
    }
    if dur_ms <= 0 || width <= 0.0 {
        return FrameWindow {
            first: 0,
            last: frames - 1,
        };
    }
    let margin_ms = margin_px * dur_ms as f64 / width;
    // Through the shared frame↔time mapping, so the window brackets the frames
    // whose CENTRES — where the painters draw them — fall in view; the extra
    // frame each side carries the segment that crosses the box edge.
    let lo = floor_to_i64(
        m.frame_position((view_start_ms as f64 - margin_ms - start_ms as f64) / 1000.0),
    ) - 1;
    let hi = ceil_to_i64(m.frame_position(
        (view_start_ms.wrapping_add(dur_ms) as f64 + margin_ms - start_ms as f64) / 1000.0,
    )) + 1;
    FrameWindow {
        first: lo.max(0),
        last: hi.min(frames - 1),
    }
}

/// A detected sung note: a run of voiced frames quantized to one
/// [`semitone`](Self::semitone), with the median cents deviation of the
/// take's actual pitch within it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchNote {
    pub start_frame: usize,
    pub end_frame: usize,
    pub semitone: i64,
    pub median_cents: f64,
}

impl PitchNote {
    pub fn frames(&self) -> usize {
        self.end_frame - self.start_frame + 1
    }

    pub fn in_tune(&self) -> bool {
        self.median_cents.abs() <= IN_TUNE_CENTS
    }
}

/// [`segment_pitch_notes`]' default shortest note, in frames.
pub const MIN_NOTE_FRAMES: usize = 7;

/// [`segment_pitch_notes`]' default longest dropout a note bridges, in
/// frames.
pub const MAX_NOTE_GAP: usize = 3;

/// Sticky band: a frame stays in the current note while within ±(0.5 + 0.1)
/// semitone of it. Without the hysteresis, a note sung right at a semitone
/// boundary (~quarter-tone off) makes round() flip-flop frame-to-frame,
/// fragmenting it into sub-min_frames runs that all get dropped — so the
/// worst notes would vanish and bias the in-tune% upward.
const BAND: f64 = 0.6;

/// The note being built while [`segment_pitch_notes`] walks the frames.
struct Segmenter {
    out: Vec<PitchNote>,
    /// -1 while no note is open.
    start_fr: i64,
    end_fr: i64,
    semi: i64,
    gap: usize,
    cents: Vec<f64>,
    min_frames: usize,
}

impl Segmenter {
    fn flush(&mut self) {
        if self.start_fr >= 0
            && (self.end_fr - self.start_fr + 1) >= self.min_frames as i64
            && !self.cents.is_empty()
        {
            self.cents.sort_by(|a, b| compare_f64(*a, *b));
            self.out.push(PitchNote {
                start_frame: self.start_fr as usize,
                end_frame: self.end_fr as usize,
                semitone: self.semi,
                median_cents: self.cents[self.cents.len() / 2],
            });
        }
        self.start_fr = -1;
        self.cents.clear();
        self.gap = 0;
    }

    fn start(&mut self, fr: usize, midi: f64) {
        self.start_fr = fr as i64;
        self.end_fr = fr as i64;
        self.semi = round_to_i64(midi);
        self.gap = 0;
        self.cents.clear();
        self.cents.push((midi - self.semi as f64) * 100.0);
    }
}

/// Segments the F0 contour into discrete sung notes: consecutive voiced
/// frames rounded to the same semitone (tolerating dropouts of up to
/// `max_gap` frames), dropping runs shorter than `min_frames`. Each note's
/// [`median_cents`](PitchNote::median_cents) is the median deviation from its
/// semitone — robust to vibrato (which swings around it). The defaults are
/// [`MIN_NOTE_FRAMES`] and [`MAX_NOTE_GAP`].
pub fn segment_pitch_notes(m: &VoiceMetrics, min_frames: usize, max_gap: usize) -> Vec<PitchNote> {
    let mut s = Segmenter {
        out: Vec::new(),
        start_fr: -1,
        end_fr: -1,
        semi: 0,
        gap: 0,
        cents: Vec::new(),
        min_frames,
    };
    for fr in 0..m.frames {
        let Some(hz) = voiced_hz(f0_at(m, fr)) else {
            if s.start_fr >= 0 {
                s.gap += 1;
                if s.gap > max_gap {
                    s.flush();
                }
            }
            continue;
        };
        let midi = hz_to_midi(hz);
        if s.start_fr < 0 {
            s.start(fr, midi);
        } else if (midi - s.semi as f64).abs() < BAND {
            s.end_fr = fr as i64;
            s.gap = 0;
            s.cents.push((midi - s.semi as f64) * 100.0);
        } else {
            s.flush();
            s.start(fr, midi);
        }
    }
    s.flush();
    s.out
}

/// Duration-weighted fraction (0–100) of sung notes that are in tune; NaN if
/// there are no notes.
pub fn pitch_in_tune_percent(notes: &[PitchNote]) -> f64 {
    let (mut total, mut good) = (0usize, 0usize);
    for n in notes {
        total += n.frames();
        if n.in_tune() {
            good += n.frames();
        }
    }
    if total == 0 {
        f64::NAN
    } else {
        100.0 * good as f64 / total as f64
    }
}

/// The (up to 3) most off-pitch notes worth flagging — indices into `notes`.
/// (Dart's sort is stable up to 32 elements, as Rust's always is; past that,
/// notes with exactly equal deviations could tie in another order.)
fn worst_notes(notes: &[PitchNote]) -> BTreeSet<usize> {
    let mut idx: Vec<usize> = (0..notes.len()).collect();
    idx.sort_by(|&a, &b| compare_f64(notes[b].median_cents.abs(), notes[a].median_cents.abs()));
    idx.into_iter()
        .filter(|&i| notes[i].median_cents.abs() > 30.0)
        .take(3)
        .collect()
}

// --------------------------------------------------------------------- memos

// Per-take memos of what the pitch views derive from a take's metrics. Each
// derivation walks every frame (segmentation is several ms for a 10-minute
// take), and the views calling them rebuild on every view-model notify — a
// fader drag, a trackpad pan, a clip move. Metrics are never mutated after
// they are computed or merged (a new analysis is a new `Arc`), so identity is
// the key; the key is held weakly (Dart's `Expando`), so each entry goes with
// its take. A weak key also keeps its allocation's address from being reused
// while the entry exists.

type Table<K, V> = Mutex<Vec<(Weak<K>, V)>>;

static NOTES_MEMO: Table<VoiceMetrics, Arc<[PitchNote]>> = Mutex::new(Vec::new());
static RANGE_MEMO: Table<VoiceMetrics, SemitoneRange> = Mutex::new(Vec::new());
static WORST_MEMO: Table<[PitchNote], Arc<BTreeSet<usize>>> = Mutex::new(Vec::new());
type VibratoByNote = HashMap<(usize, usize), Arc<Vibrato>>;
static VIBRATO_MEMO: Table<VoiceMetrics, VibratoByNote> = Mutex::new(Vec::new());

/// `read` of `key`'s entry in `table`, made by `compute` the first time.
fn memo<K: ?Sized, V, R>(
    table: &Table<K, V>,
    key: &Arc<K>,
    compute: impl FnOnce() -> V,
    read: impl FnOnce(&mut V) -> R,
) -> R {
    let mut table = table.lock().unwrap_or_else(|e| e.into_inner());
    // An entry whose take is gone goes with it.
    table.retain(|(k, _)| k.strong_count() > 0);
    let at = match table
        .iter()
        .position(|(k, _)| std::ptr::addr_eq(k.as_ptr(), Arc::as_ptr(key)))
    {
        Some(at) => at,
        None => {
            table.push((Arc::downgrade(key), compute()));
            table.len() - 1
        }
    };
    read(&mut table[at].1)
}

/// [`segment_pitch_notes`] at its default thresholds, computed once per take.
pub fn pitch_notes_of(m: &Arc<VoiceMetrics>) -> Arc<[PitchNote]> {
    memo(
        &NOTES_MEMO,
        m,
        || segment_pitch_notes(m, MIN_NOTE_FRAMES, MAX_NOTE_GAP).into(),
        |v| Arc::clone(v),
    )
}

/// [`pitch_semitone_range`], computed once per take.
pub fn pitch_range_of(m: &Arc<VoiceMetrics>) -> SemitoneRange {
    memo(&RANGE_MEMO, m, || pitch_semitone_range(m), |v| *v)
}

/// The notes to flag in `notes` (the up-to-3 most off-pitch ones, beyond
/// 30¢), computed once per list — pass the memoized [`pitch_notes_of`] list.
pub fn worst_notes_of(notes: &Arc<[PitchNote]>) -> Arc<BTreeSet<usize>> {
    memo(
        &WORST_MEMO,
        notes,
        || Arc::new(worst_notes(notes)),
        |v| Arc::clone(v),
    )
}

/// [`analyze_vibrato`] over `note` (one of [`pitch_notes_of`]`(m)`) at its
/// default thresholds, computed once per note.
pub fn note_vibrato_of(m: &Arc<VoiceMetrics>, note: &PitchNote) -> Arc<Vibrato> {
    memo(&VIBRATO_MEMO, m, HashMap::new, |by_note| {
        Arc::clone(
            by_note
                .entry((note.start_frame, note.end_frame))
                .or_insert_with(|| {
                    Arc::new(analyze_vibrato(
                        m,
                        note.start_frame as i64,
                        note.end_frame as i64,
                        VibratoOptions::default(),
                    ))
                }),
        )
    })
}

/// The empty note list shown while loading or with no analysis (Dart's
/// `const <PitchNote>[]`): one list, so its flagged-notes memo is one entry.
fn no_notes() -> Arc<[PitchNote]> {
    static EMPTY: OnceLock<Arc<[PitchNote]>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::from(Vec::new())))
}

fn note_at_frame(notes: &[PitchNote], fr: usize) -> Option<&PitchNote> {
    notes
        .iter()
        .find(|n| fr >= n.start_frame && fr <= n.end_frame)
}

/// The pitch header's readout of the note under the playhead at
/// `position_ms` (`_PlayheadNoteReadout`): `A3 +4¢`, or `—` off the take.
pub fn playhead_note_text(
    m: &VoiceMetrics,
    notes: &[PitchNote],
    track_start_ms: i64,
    position_ms: i64,
) -> String {
    // Only read a note when the playhead is actually over this take's frames
    // (frame_at_time would otherwise clamp to a stale edge) — up to the end
    // of the last frame's cell.
    let t = position_ms.wrapping_sub(track_start_ms) as f64 / 1000.0;
    let n = if m.frames > 0 && t >= 0.0 && t <= m.cell_end(m.frames as i64 - 1) {
        note_at_frame(notes, m.frame_at_time(t))
    } else {
        None
    };
    match n {
        None => "—".to_string(),
        Some(n) => format!("{} {}", semi_name(n.semitone), cents_str(n.median_cents)),
    }
}

// ------------------------------------------------------------------ painter

/// How far right of a flagged note's start its cents label can reach.
const LABEL_REACH_PX: f64 = 80.0;

/// The labels' text size.
const LABEL_SIZE: f32 = 9.0;

/// Inter's line box (ascent + descent ≈ 1.21 em): the height Flutter's
/// `TextPainter` gave a label, which `_label` centred on its y.
const LABEL_LINE: f32 = 1.21;

/// One thing [`PitchLanePainter`] draws, in lane coordinates.
#[derive(Debug, Clone, PartialEq)]
pub enum PitchOp {
    /// A semitone grid line across the lane, 1 px: `Studio.gridStrong` on a
    /// C, `Studio.grid` at 50 % otherwise.
    GridLine { y: f64, color: Color },
    /// 9 px text whose left edge is at `x`, centred vertically on `y`.
    Label {
        text: String,
        x: f64,
        y: f64,
        color: Color,
    },
    /// Note `index`'s block: a radius-3 rounded rectangle filled with `fill`
    /// and stroked `stroke_width` wide in `stroke`.
    Block {
        index: usize,
        left: f64,
        top: f64,
        right: f64,
        bottom: f64,
        fill: Color,
        stroke: Color,
        stroke_width: f32,
    },
    /// One segment of the fine F0 contour (1.3 px, round caps).
    Contour { from: (f64, f64), to: (f64, f64) },
}

/// Everything [`PitchLanePainter`] draws in a box, in paint order: the
/// semitone grid (with its octave labels), the note blocks (with the flagged
/// notes' cents labels), then the contour.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PitchLanePlan {
    pub grid: Vec<PitchOp>,
    pub notes: Vec<PitchOp>,
    pub contour: Vec<PitchOp>,
}

impl PitchLanePlan {
    /// Every op, in paint order.
    pub fn ops(&self) -> impl Iterator<Item = &PitchOp> {
        self.grid.iter().chain(&self.notes).chain(&self.contour)
    }
}

/// `PitchLanePainter`: the semitone grid, the detected note blocks (tinted by
/// how in-tune, the worst few outlined + labeled), and the fine F0 contour
/// inside each note. `start_ms` places the take's frames on the timeline;
/// `view_start_ms` is the absolute time at the left edge of the viewport.
#[derive(Debug, Clone)]
pub struct PitchLanePainter {
    pub metrics: Arc<VoiceMetrics>,
    pub notes: Arc<[PitchNote]>,
    pub worst: Arc<BTreeSet<usize>>,
    pub timeline_duration_ms: i64,
    pub start_ms: i64,
    pub view_start_ms: i64,
    pub lo_semi: i64,
    pub hi_semi: i64,
}

/// The note colour for a deviation: green in tune, ramping to
/// `Studio.record` at 50¢ (`Color.lerp`).
fn cents_color(abs_cents: f64) -> Color {
    let t = (abs_cents / 50.0).clamp(0.0, 1.0) as f32;
    let lerp = |a: f32, b: f32| a * (1.0 - t) + b * t;
    Color {
        r: lerp(IN_TUNE.r, Studio::RECORD.r),
        g: lerp(IN_TUNE.g, Studio::RECORD.g),
        b: lerp(IN_TUNE.b, Studio::RECORD.b),
        a: lerp(IN_TUNE.a, Studio::RECORD.a),
    }
}

/// The lane's vertical and horizontal mapping in a box.
struct Axes<'a> {
    p: &'a PitchLanePainter,
    width: f64,
    height: f64,
    span: f64,
}

impl Axes<'_> {
    fn y_of(&self, semi: f64) -> f64 {
        (1.0 - (semi - self.p.lo_semi as f64) / self.span) * self.height
    }

    /// Absolute timeline ms into the viewport; off-screen parts are clipped,
    /// so no clamp (which would pin them to the edge).
    fn x_of(&self, ms: f64) -> f64 {
        (ms - self.p.view_start_ms as f64) / self.p.timeline_duration_ms as f64 * self.width
    }

    /// A frame sits at its window's centre — where the waveform shows the
    /// audio it measured — not at the window's start, half a window early.
    fn x_of_frame(&self, fr: usize) -> f64 {
        self.x_of(self.p.start_ms as f64 + self.p.metrics.time_of_frame(fr as i64) * 1000.0)
    }
}

impl PitchLanePainter {
    fn axes(&self, size: Size) -> Option<Axes<'_>> {
        if self.timeline_duration_ms <= 0 || self.hi_semi <= self.lo_semi {
            return None;
        }
        Some(Axes {
            p: self,
            width: f64::from(size.width),
            height: f64::from(size.height),
            span: (self.hi_semi - self.lo_semi) as f64,
        })
    }

    /// Everything the painter draws in a box of `size` (nothing with no
    /// timeline or an empty range).
    pub fn plan(&self, size: Size) -> PitchLanePlan {
        PitchLanePlan {
            grid: self.grid_ops(size),
            notes: self.note_ops(size),
            contour: self.contour_ops(size),
        }
    }

    /// The semitone grid (octave C lines bolder + labeled).
    pub fn grid_ops(&self, size: Size) -> Vec<PitchOp> {
        let Some(a) = self.axes(size) else {
            return Vec::new();
        };
        let mut ops = Vec::new();
        for s in self.lo_semi..=self.hi_semi {
            let is_c = s.rem_euclid(12) == 0;
            let y = a.y_of(s as f64);
            ops.push(PitchOp::GridLine {
                y,
                color: if is_c {
                    Studio::GRID_STRONG
                } else {
                    alpha(Studio::GRID, 0.5)
                },
            });
            if is_c {
                ops.push(PitchOp::Label {
                    text: format!("C{}", s / 12 - 1),
                    x: 3.0,
                    y,
                    color: Studio::TEXT_FAINT,
                });
            }
        }
        ops
    }

    /// The note blocks, and the flagged notes' cents labels.
    pub fn note_ops(&self, size: Size) -> Vec<PitchOp> {
        let Some(a) = self.axes(size) else {
            return Vec::new();
        };
        let semi_h = a.height / a.span;
        let block_h = semi_h * 0.62;
        // Only the notes that can reach the box are drawn (it is clipped): a
        // block with its stroke, and for the flagged ones the cents label
        // right of its start. Zoomed in on a long take that is a handful of
        // the notes.
        let off_box = |i: usize, x0: f64, x1: f64| {
            if x0 - 2.0 > a.width {
                return true;
            }
            if x1 + 2.0 >= 0.0 {
                return false;
            }
            !self.worst.contains(&i) || x0 + 2.0 + LABEL_REACH_PX < 0.0
        };
        let mut ops = Vec::new();
        for (i, n) in self.notes.iter().enumerate() {
            let x0 = a.x_of_frame(n.start_frame);
            let x1 = a.x_of_frame(n.end_frame).max(x0 + 2.0);
            if off_box(i, x0, x1) {
                continue;
            }
            let yc = a.y_of(n.semitone as f64);
            let col = cents_color(n.median_cents.abs());
            let is_worst = self.worst.contains(&i);
            ops.push(PitchOp::Block {
                index: i,
                left: x0,
                top: yc - block_h / 2.0,
                right: x1,
                bottom: yc + block_h / 2.0,
                fill: alpha(col, 0.26),
                stroke: if is_worst {
                    Studio::RECORD
                } else {
                    alpha(col, 0.7)
                },
                stroke_width: if is_worst { 1.6 } else { 1.0 },
            });
            if is_worst {
                let mut ly = yc - block_h / 2.0 - 11.0;
                if ly < 1.0 {
                    ly = yc + block_h / 2.0 + 2.0;
                }
                ops.push(PitchOp::Label {
                    text: cents_str(n.median_cents),
                    x: x0 + 2.0,
                    y: ly + 5.0,
                    color: Studio::RECORD,
                });
            }
        }
        ops
    }

    /// The fine F0 contour, drawn only inside notes (so octave glitches and
    /// unvoiced gaps between notes don't clutter the view), over the frames
    /// that can reach the box.
    pub fn contour_ops(&self, size: Size) -> Vec<PitchOp> {
        let Some(a) = self.axes(size) else {
            return Vec::new();
        };
        let m = self.metrics.as_ref();
        let window = lane_visible_frames(
            m,
            self.start_ms,
            self.view_start_ms,
            self.timeline_duration_ms,
            a.width,
            LANE_MARGIN_PX,
        );
        let mut ops = Vec::new();
        for n in self.notes.iter() {
            if (n.end_frame as i64) < window.first || n.start_frame as i64 > window.last {
                continue;
            }
            let mut prev: Option<(f64, f64)> = None;
            let from = n.start_frame.max(window.first as usize);
            let to = n.end_frame.min(window.last as usize);
            for fr in from..=to {
                let Some(hz) = voiced_hz(f0_at(m, fr)) else {
                    prev = None;
                    continue;
                };
                let p = (a.x_of_frame(fr), a.y_of(hz_to_midi(hz)));
                if let Some(q) = prev {
                    ops.push(PitchOp::Contour { from: q, to: p });
                }
                prev = Some(p);
            }
        }
        ops
    }

    /// The painter as three stacked canvases filling the parent — the grid,
    /// the notes, the contour — so each draws over the text of the one below
    /// as Flutter's single canvas did (iced draws a canvas's text above its
    /// shapes). Each keeps a cache cleared only when what it shows changes
    /// (`shouldRepaint`).
    pub fn view<'a, M: 'a>(self) -> Element<'a, M> {
        let painter = Arc::new(self);
        let layer = |layer| {
            Element::from(
                canvas_widget(PitchLayer {
                    painter: Arc::clone(&painter),
                    layer,
                })
                .width(Length::Fill)
                .height(Length::Fill),
            )
        };
        stack![
            layer(Layer::Grid),
            layer(Layer::Notes),
            layer(Layer::Contour)
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

/// Draws `ops` into `frame`. The frame clips to its own box (Dart clipped
/// with `canvas.clipRect`: zoomed in, most of the take maps outside the box,
/// and the notes left of the viewport painted over the track-header column).
fn draw_ops(frame: &mut Frame, size: Size, ops: &[PitchOp]) {
    let mut contour = Vec::new();
    for op in ops {
        match op {
            PitchOp::GridLine { y, color } => {
                let y = *y as f32;
                frame.stroke(
                    &Path::line(Point::new(0.0, y), Point::new(size.width, y)),
                    Stroke::default().with_color(*color).with_width(1.0),
                );
            }
            PitchOp::Label { text, x, y, color } => {
                frame.fill_text(canvas::Text {
                    content: text.clone(),
                    position: Point::new(*x as f32, *y as f32 - LABEL_SIZE * LABEL_LINE / 2.0),
                    color: *color,
                    size: Pixels(LABEL_SIZE),
                    line_height: LineHeight::Relative(LABEL_LINE),
                    font: font::REGULAR,
                    align_y: Vertical::Top,
                    ..canvas::Text::default()
                });
            }
            PitchOp::Block {
                left,
                top,
                right,
                bottom,
                fill,
                stroke,
                stroke_width,
                ..
            } => {
                let (w, h) = ((right - left) as f32, (bottom - top) as f32);
                // Flutter's RRect shrinks radii that don't fit the rect.
                let radius = 3.0f32.min(w / 2.0).min(h / 2.0).max(0.0);
                let block = Path::rounded_rectangle(
                    Point::new(*left as f32, *top as f32),
                    Size::new(w, h),
                    radius.into(),
                );
                frame.fill(&block, *fill);
                frame.stroke(
                    &block,
                    Stroke::default()
                        .with_color(*stroke)
                        .with_width(*stroke_width),
                );
            }
            PitchOp::Contour { from, to } => contour.push((*from, *to)),
        }
    }
    if !contour.is_empty() {
        // One path of one-segment subpaths: each segment keeps its own round
        // caps, as Dart's drawLine per segment did, in one tessellation.
        let path = Path::new(|b| {
            for (from, to) in &contour {
                b.move_to(Point::new(from.0 as f32, from.1 as f32));
                b.line_to(Point::new(to.0 as f32, to.1 as f32));
            }
        });
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(alpha(Studio::TEXT_PRIMARY, 0.85))
                .with_width(1.3)
                .with_line_cap(LineCap::Round),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Grid,
    Notes,
    Contour,
}

struct PitchLayer {
    painter: Arc<PitchLanePainter>,
    layer: Layer,
}

/// What a layer was drawn for. notes/worst are derived from metrics, so
/// metrics identity covers them in Dart's `shouldRepaint` — which avoids
/// repainting on every unrelated view-model notify (volume/move/zoom); the
/// memoized lists keep their identity across those too, so keying on them as
/// well costs nothing.
type PitchKey = (
    Same<VoiceMetrics>,
    Same<[PitchNote]>,
    Same<BTreeSet<usize>>,
    [i64; 5],
);

impl<M> canvas::Program<M> for PitchLayer {
    type State = KeyedCache<PitchKey>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let p = &self.painter;
        let key = (
            Same(Arc::clone(&p.metrics)),
            Same(Arc::clone(&p.notes)),
            Same(Arc::clone(&p.worst)),
            [
                p.timeline_duration_ms,
                p.start_ms,
                p.view_start_ms,
                p.lo_semi,
                p.hi_semi,
            ],
        );
        let size = bounds.size();
        vec![state.draw(renderer, size, key, |frame| {
            let ops = match self.layer {
                Layer::Grid => p.grid_ops(size),
                Layer::Notes => p.note_ops(size),
                Layer::Contour => p.contour_ops(size),
            };
            draw_ops(frame, size, &ops);
        })]
    }
}

// ---------------------------------------------------------------- the lane

/// `PitchLane(...)`: the sub-lane under a vocal take. Shares the timeline
/// and the header-column width with the parent track. Build it with
/// [`pitch_lane`], configure it, and convert it into an [`Element`].
#[derive(Debug, Clone)]
pub struct PitchLane {
    track_start_ms: i64,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
    header_width: f32,
    loading: bool,
    metrics: Option<Arc<VoiceMetrics>>,
    height: f32,
    take_local: bool,
}

/// The lane for `track` on a `timeline_duration_ms` timeline, its header
/// `header_width` wide, with the playhead at `position_ms` (absolute), while
/// its analysis is `loading` or once it is `metrics`. The viewport starts at
/// 0, the lane is [`PITCH_LANE_HEIGHT`] tall and on the shared timeline.
pub fn pitch_lane(
    track: &AudioTrack,
    timeline_duration_ms: i64,
    position_ms: i64,
    header_width: f32,
    loading: bool,
    metrics: Option<Arc<VoiceMetrics>>,
) -> PitchLane {
    PitchLane {
        track_start_ms: track.start_ms(),
        timeline_duration_ms,
        view_start_ms: 0,
        position_ms,
        header_width,
        loading,
        metrics,
        height: PITCH_LANE_HEIGHT,
        take_local: false,
    }
}

impl PitchLane {
    /// Absolute time (ms) at the left edge of the viewport (0 in take-local
    /// mode).
    pub fn view_start_ms(mut self, ms: i64) -> Self {
        self.view_start_ms = ms;
        self
    }

    /// Lane height; defaults to the inline-lane height but the Pitch dock tab
    /// passes the full available height so it fills the dock.
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// When true (the Pitch dock tab), the take fills the panel in take-local
    /// time [0, duration_ms] instead of being placed at its start_ms on a
    /// shared timeline — so `timeline_duration_ms` should be the take's own
    /// duration.
    pub fn take_local(mut self, take_local: bool) -> Self {
        self.take_local = take_local;
        self
    }
}

/// A 1 px line in `Studio.border`: one side of a `Border(...)` box.
fn rule<'a, M: 'a>(horizontal: bool) -> Element<'a, M> {
    let line = container(iced::widget::Space::new()).style(fill(Studio::BORDER));
    if horizontal {
        line.width(Length::Fill).height(1).into()
    } else {
        line.width(1).height(Length::Fill).into()
    }
}

impl<'a, M: 'a> From<PitchLane> for Element<'a, M> {
    fn from(lane: PitchLane) -> Self {
        let notes = match &lane.metrics {
            Some(m) if !lane.loading => pitch_notes_of(m),
            _ => no_notes(),
        };
        let in_tune = pitch_in_tune_percent(&notes);
        let worst = worst_notes_of(&notes);
        let range = lane
            .metrics
            .as_ref()
            .map_or(DEFAULT_SEMITONE_RANGE, pitch_range_of);

        let header = header(&lane, &notes, in_tune);
        let body = body(&lane, notes, worst, range);
        // `Border(bottom: Studio.border)`: the line takes its pixel from the
        // lane's height, as a decoration's border pads its child.
        column![
            row![
                header,
                container(body).width(Length::Fill).height(Length::Fill)
            ]
            .height(Length::Fill),
            rule(true),
        ]
        .width(Length::Fill)
        .height(lane.height)
        .into()
    }
}

fn header<'a, M: 'a>(lane: &PitchLane, notes: &[PitchNote], in_tune: f64) -> Element<'a, M> {
    let mut content = column![studio_text("PITCH")
        .size(10)
        .font(font::BOLD)
        .letter_spacing(0.8)
        .color(Studio::TEXT_FAINT)];
    if let (Some(m), false) = (&lane.metrics, lane.loading) {
        let score = if in_tune.is_nan() {
            "—".to_string()
        } else {
            format!("{}%", round_to_i64(in_tune))
        };
        content = content
            .push(iced::widget::Space::new().height(7))
            .push(rich_text::<(), M, Theme, Renderer>([
                span(score)
                    .size(14)
                    .font(font::BOLD)
                    .color(Studio::TEXT_PRIMARY),
                span("  in tune").size(9).color(Studio::TEXT_FAINT),
            ]))
            .push(iced::widget::Space::new().height(3))
            // Live readout of the note under the playhead.
            .push(
                text(playhead_note_text(
                    m,
                    notes,
                    lane.track_start_ms,
                    lane.position_ms,
                ))
                .size(11)
                .font(font::SEMIBOLD)
                .color(Studio::TEXT_SECONDARY),
            );
    }
    container(row![
        container(content)
            .padding(Padding {
                top: 8.0,
                right: 8.0,
                bottom: 8.0,
                left: 30.0,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true),
        rule(false),
    ])
    .width(lane.header_width)
    .height(Length::Fill)
    .style(fill(Studio::PANEL))
    .into()
}

fn body<'a, M: 'a>(
    lane: &PitchLane,
    notes: Arc<[PitchNote]>,
    worst: Arc<BTreeSet<usize>>,
    range: SemitoneRange,
) -> Element<'a, M> {
    if lane.loading {
        return lane_hint("Analyzing pitch…");
    }
    let Some(m) = &lane.metrics else {
        return lane_hint("Pitch is available for recorded WAV takes.");
    };
    // The notes are their own layers, under the playhead: a playhead tick
    // redraws only the playhead.
    let painter = PitchLanePainter {
        metrics: Arc::clone(m),
        notes,
        worst,
        timeline_duration_ms: lane.timeline_duration_ms,
        start_ms: if lane.take_local {
            0
        } else {
            lane.track_start_ms
        },
        view_start_ms: lane.view_start_ms,
        lo_semi: range.lo,
        hi_semi: range.hi,
    };
    let playhead = canvas_widget(PlayheadPainter {
        position_ms: lane.position_ms,
        timeline_duration_ms: lane.timeline_duration_ms,
        color: Studio::PLAYHEAD,
        show_head: false,
        start_ms: if lane.take_local {
            lane.track_start_ms
        } else {
            lane.view_start_ms
        },
    })
    .width(Length::Fill)
    .height(Length::Fill);
    container(stack![painter.view(), playhead])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(fill(Studio::INSET))
        .into()
}
