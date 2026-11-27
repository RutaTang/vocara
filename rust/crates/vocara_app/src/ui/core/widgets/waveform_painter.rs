//! Port of `ui/core/widgets/waveform_painter.dart`: the timeline's painters
//! — a clip's waveform silhouette, the live take's fixed-scale waveform, the
//! time grid, the ruler and the playhead — and the pure functions they are
//! built on (tick spacing, the viewport slice, the outlines), which the tests
//! check without a window.
//!
//! They draw `rust/DESIGN.md`'s timeline: waveforms are filled, symmetric
//! peak envelopes, one value per pixel column (the loudest peak the column
//! covers), with no centre axis; the ruler has major ticks with `m:ss`
//! labels and minor ticks between them; the grid lines fall on the major
//! ticks; the playhead is a crisp 1 px line.
//!
//! Each painter is a `canvas::Program`. The ones whose drawing is costly
//! keep a cache cleared only when their inputs change (`shouldRepaint`);
//! the playhead redraws on every position change, as Dart's repainted on
//! every tick. Peaks are shared `Arc<[f64]>` buffers compared by identity,
//! as Dart compared the lists.

use std::ops::Range;
use std::sync::Arc;

use iced::alignment::Vertical;
use iced::font::Weight;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{canvas as canvas_widget, Canvas};
use iced::{mouse, Color, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

use super::painter::{color_key, KeyedCache, Same};
use super::studio_text::fill_tabular_text;
use crate::ui::core::system_font;
use crate::ui::core::theme::{clip, faded};
pub use vocara_model::util::format::format_clock_ms;

/// Picks a readable time-grid interval (in ms) so labels/lines land roughly
/// every `min_px` pixels (72 by default).
pub fn nice_time_interval_ms(duration_ms: i64, width: f64) -> i64 {
    nice_time_interval_ms_min(duration_ms, width, 72.0)
}

/// [`nice_time_interval_ms`] with an explicit minimum spacing.
pub fn nice_time_interval_ms_min(duration_ms: i64, width: f64, min_px: f64) -> i64 {
    if duration_ms <= 0 {
        return 1000;
    }
    if width <= 0.0 {
        return duration_ms; // nothing to space out: one tick at most
    }
    let min_ms = min_px / (width / duration_ms as f64);
    const CANDIDATES: [i64; 12] = [
        1000, 2000, 5000, 10000, 15000, 30000, 60000, 120000, 300000, 600000,
        // Multi-hour audio: capped at 10 minutes, a long backing at fit-all on
        // a narrow timeline packed its labels a few pixels apart.
        1800000, 3600000,
    ];
    for c in CANDIDATES {
        if c as f64 >= min_ms {
            return c;
        }
    }
    // Past an hour, whole hours, as many as keep the ticks apart: held at one,
    // a timeline of 2^62 ms (a peer's, before the sync door bounded it) drew a
    // line and a label per hour, and the screen froze. Never past the whole
    // timeline (one tick at most), where the hours overflowed an int.
    let hours = (min_ms / 3600000.0).ceil() * 3600000.0;
    if hours >= duration_ms as f64 {
        duration_ms
    } else {
        hours as i64
    }
}

/// The whole-peak slice of a peak list visible through a window, and the
/// [`WaveformPainter`] mapping that anchors it to the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakSlice {
    /// The peaks in view, as a range of the list's indices.
    pub range: Range<usize>,
    /// [`WaveformPainter::index_offset`] for the slice.
    pub index_offset: f64,
    /// [`WaveformPainter::index_span`] for the slice.
    pub index_span: f64,
}

/// Cuts the whole-peak slice of `n` peaks (spanning `span_start_ms` for
/// `span_dur_ms` on the timeline) visible through the window
/// [`visible_start_ms`, `visible_end_ms`], together with the
/// `index_offset`/`index_span` mapping that anchors it to the timeline. The
/// cut is at WHOLE peak indices while the window edge is not, so the slice's
/// first peak belongs up to one peak interval BEFORE the window; painting the
/// slice without this mapping shifts the whole waveform against
/// ruler/grid/playhead by that fraction. Every viewport-sliced
/// [`WaveformPainter`] must go through here — the lanes share one ruler, so
/// they must share one anchoring.
pub fn visible_peak_slice(
    n: usize,
    span_start_ms: i64,
    span_dur_ms: i64,
    visible_start_ms: i64,
    visible_end_ms: i64,
) -> PeakSlice {
    if n == 0 || span_dur_ms <= 0 || visible_end_ms <= visible_start_ms {
        return PeakSlice {
            range: 0..0,
            index_offset: 0.0,
            index_span: 1.0,
        };
    }
    let nf = n as f64;
    let start = (visible_start_ms - span_start_ms) as f64 / span_dur_ms as f64 * nf;
    let end = (visible_end_ms - span_start_ms) as f64 / span_dur_ms as f64 * nf;
    let i0 = clamp_i(start.floor(), 0, n as i64 - 1);
    // One peak PAST the window's last one: the painter draws each peak as a
    // vertex at its bucket's start, so the last bucket in view only has a
    // right edge to draw to from the vertex at or beyond the window end.
    // Stopping at ceil(end) left the strip after the last vertex blank — and
    // a window inside a single bucket (a long take at max zoom) one vertex,
    // which draws nothing.
    let i1 = clamp_i(end.ceil() + 1.0, i0 + 1, n as i64);
    PeakSlice {
        range: i0 as usize..i1 as usize,
        index_offset: start - i0 as f64,
        index_span: end - start,
    }
}

/// Dart's `double.floor()/ceil()` to int, then `int.clamp(lo, hi)`.
fn clamp_i(v: f64, lo: i64, hi: i64) -> i64 {
    (v as i64).clamp(lo, hi)
}

/// The filled silhouette [`WaveformPainter`] paints.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformShape {
    /// The width the clip covers (`size.width × widthFraction`).
    pub draw_width: f64,
    /// The axis' y (half the height).
    pub mid: f64,
    /// Whether the drawing is clipped to `draw_width` (a viewport slice).
    pub clip: bool,
    /// The closed outline: the top edge left to right, then the bottom edge
    /// right to left, starting at `(0, mid)`.
    pub outline: Vec<(f64, f64)>,
}

/// How much of the half-height a full-scale peak reaches.
pub const PEAK_REACH: f64 = 0.92;

/// The half-height a silent column keeps: a hairline, so a clip's silence
/// still reads as part of the clip (the envelope has no centre axis).
pub const SILENCE_HALF_HEIGHT: f64 = 0.35;

/// The silhouette of `peaks` in a `width` × `height` box, as
/// [`WaveformPainter`] paints it; `None` when it draws nothing.
///
/// A true peak envelope, one value per pixel column: peak `k` stands for
/// the stretch of audio from its vertex to the next one (index `k` to
/// `k + 1`, mapped as `(index − index_offset) / index_span × draw width`;
/// the spread-all mapping puts the first peak at the left edge and the last
/// at the right), and each column takes the loudest peak whose stretch it
/// covers. Zoomed out, a column stands for every peak in it, so a burst
/// between two columns' centres is never dropped; zoomed in, a peak spans
/// several columns and draws as a flat step, not a ramp to the next one.
/// Past the last peak (a slice that reaches the clip's end has no peak
/// after it), the last one holds out to the edge.
pub fn waveform_shape(
    peaks: &[f64],
    width: f64,
    height: f64,
    width_fraction: f64,
    index_offset: f64,
    index_span: Option<f64>,
) -> Option<WaveformShape> {
    if peaks.is_empty() {
        return None;
    }
    // `widthFraction <= 0 ? 0 : (widthFraction > 1 ? 1 : widthFraction)`.
    let fraction = width_fraction.clamp(0.0, 1.0);
    let draw_width = width * fraction;
    if draw_width <= 0.0 {
        return None;
    }
    let mid = height / 2.0;
    let n = peaks.len();
    let span = index_span.unwrap_or((n - 1) as f64);
    // A slice's edge peaks map outside the box by design (see
    // `index_offset`); keep them from bleeding into the neighboring padding.
    let clip = index_offset != 0.0 || index_span.is_some();
    let last_index = n as i64 - 1;
    // The peak index at x.
    let index_at = |x: f64| {
        if span <= 0.0 {
            0.0
        } else {
            index_offset + x / draw_width * span
        }
    };
    let columns = (draw_width.ceil() as usize).clamp(1, 1 << 20);
    let reach = mid * PEAK_REACH;
    let half_heights: Vec<f64> = (0..columns)
        .map(|c| {
            let from = index_at(c as f64).floor();
            // The column's right edge belongs to the next column.
            let to = (index_at((c + 1) as f64).ceil() - 1.0).max(from);
            let lo = clamp_i(from, 0, last_index);
            let hi = clamp_i(to, lo, last_index);
            let loudest = peaks[lo as usize..=hi as usize]
                .iter()
                .fold(0.0f64, |m, &p| if p > m { p } else { m });
            (loudest.clamp(0.0, 1.0) * reach).max(SILENCE_HALF_HEIGHT)
        })
        .collect();

    // The top edge left to right, then the bottom edge back, one flat step
    // per run of equal columns.
    let right = |c: usize| ((c + 1) as f64).min(draw_width);
    let mut outline = Vec::with_capacity(columns * 4 + 2);
    outline.push((0.0, mid));
    let mut c = 0;
    while c < columns {
        let h = half_heights[c];
        let mut end = c;
        while end + 1 < columns && half_heights[end + 1] == h {
            end += 1;
        }
        outline.push((c as f64, mid - h));
        outline.push((right(end), mid - h));
        c = end + 1;
    }
    outline.push((draw_width, mid));
    let mut c = columns;
    while c > 0 {
        let h = half_heights[c - 1];
        let mut start = c - 1;
        while start > 0 && half_heights[start - 1] == h {
            start -= 1;
        }
        outline.push((right(c - 1), mid + h));
        outline.push((start as f64, mid + h));
        c = start;
    }
    Some(WaveformShape {
        draw_width,
        mid,
        clip,
        outline,
    })
}

/// The live take's outline (`LiveWaveformPainter.paint`): `(outline, mid)`,
/// or `None` when it draws nothing. One-pixel columns anchored at the left
/// edge, each the loudest sample binned into it at the fixed
/// `pixels_per_sample`, starting `sample_offset` samples in.
pub fn live_waveform_outline(
    peaks: &[f64],
    width: f64,
    height: f64,
    pixels_per_sample: f64,
    sample_offset: f64,
) -> Option<(Vec<(f64, f64)>, f64)> {
    let n = peaks.len() as i64;
    if n == 0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let mid = height / 2.0;
    let cols = (width.floor() as i64).clamp(1, 1 << 20);
    // Samples per 1-pixel column, from the absolute scale (not n/width), so
    // the bin for a given column is frozen as the take grows.
    let spc = if pixels_per_sample <= 0.0 {
        1.0
    } else {
        1.0 / pixels_per_sample
    };
    let amp_at = |c: i64| {
        let mut lo = (c as f64 * spc + sample_offset).floor() as i64;
        let mut hi = ((c + 1) as f64 * spc + sample_offset).ceil() as i64;
        if lo >= n {
            lo = n - 1;
        }
        if lo < 0 {
            lo = 0;
        }
        if hi > n {
            hi = n;
        }
        if hi <= lo {
            hi = lo + 1;
        }
        let mut m = 0.0;
        for &a in &peaks[lo as usize..hi as usize] {
            if a > m {
                m = a;
            }
        }
        m
    };
    let half = |a: f64| (a.clamp(0.0, 1.0) * mid * PEAK_REACH).max(SILENCE_HALF_HEIGHT);
    let amps: Vec<f64> = (0..=cols).map(amp_at).collect();
    let mut outline = Vec::with_capacity(amps.len() * 2 + 1);
    outline.push((0.0, mid));
    for (c, a) in amps.iter().enumerate() {
        outline.push((c as f64, mid - half(*a)));
    }
    for (c, a) in amps.iter().enumerate().rev() {
        outline.push((c as f64, mid + half(*a)));
    }
    Some((outline, mid))
}

/// How far apart (px, at least) the ruler's major ticks and labels, and the
/// arrange's bar lines on them, fall: a 42 s song across a 670 px arrange
/// gets one every 10 s, as `DESIGN.md`'s ruler has them.
pub const MAJOR_TICK_MIN_PX: f64 = 96.0;

/// The ruler's major tick interval (ms) for `duration_ms` across `width`
/// px: [`nice_time_interval_ms_min`] at [`MAJOR_TICK_MIN_PX`].
pub fn ruler_interval_ms(duration_ms: i64, width: f64) -> i64 {
    nice_time_interval_ms_min(duration_ms, width, MAJOR_TICK_MIN_PX)
}

/// How many parts the ruler's minor ticks cut a major `interval` (ms) into:
/// whole, round steps (10 s into 2 s, 30 s into 5 s, a minute into 10 s).
pub fn minor_divisions(interval_ms: i64) -> i64 {
    match interval_ms {
        2000 | 120_000 | 3_600_000 => 4,
        30_000 | 60_000 | 1_800_000 => 6,
        _ => 5,
    }
}

/// The closest the ruler's minor ticks come (px): closer, they are left out.
pub const MINOR_TICK_MIN_PX: f64 = 6.0;

/// The x of every minor tick the ruler draws: the steps between its major
/// ticks (not on them), across the view; none when they would crowd closer
/// than [`MINOR_TICK_MIN_PX`].
pub fn minor_tick_xs(timeline_duration_ms: i64, start_ms: i64, width: f64) -> Vec<f64> {
    if timeline_duration_ms <= 0 || width <= 0.0 {
        return Vec::new();
    }
    let interval = ruler_interval_ms(timeline_duration_ms, width);
    let step = (interval / minor_divisions(interval)).max(1);
    if step as f64 / timeline_duration_ms as f64 * width < MINOR_TICK_MIN_PX {
        return Vec::new();
    }
    let end = start_ms.saturating_add(timeline_duration_ms);
    let mut t = (start_ms / step).saturating_mul(step);
    if t < start_ms {
        t = t.saturating_add(step);
    }
    let mut xs = Vec::new();
    while t < end {
        if t % interval != 0 {
            xs.push((t - start_ms) as f64 / timeline_duration_ms as f64 * width);
        }
        match t.checked_add(step) {
            Some(next) => t = next,
            None => break,
        }
    }
    xs
}

/// A ruler label: `m:ss` (`0:10`, `12:48`), `h:mm:ss` past an hour.
pub fn ruler_label(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `x` moved to the middle of its pixel column, where a 1 px line draws
/// crisp.
pub fn crisp(x: f32) -> f32 {
    x.floor() + 0.5
}

/// The x of every grid line `TimelineGridPainter` draws: one per major
/// ruler tick strictly inside the view (the left edge's own line is the
/// ruler's).
pub fn grid_line_xs(timeline_duration_ms: i64, start_ms: i64, width: f64) -> Vec<f64> {
    if timeline_duration_ms <= 0 {
        return Vec::new();
    }
    let interval = ruler_interval_ms(timeline_duration_ms, width);
    // Dart's ints wrap; an end past i64 is unreachable for a real timeline.
    let end = start_ms.saturating_add(timeline_duration_ms);
    let mut xs = Vec::new();
    let mut t = (start_ms / interval + 1).saturating_mul(interval);
    while t < end {
        xs.push((t - start_ms) as f64 / timeline_duration_ms as f64 * width);
        match t.checked_add(interval) {
            Some(next) => t = next,
            None => break,
        }
    }
    xs
}

/// Every major ruler tick `TimeRulerPainter` draws: `(x, ms)`, from the
/// first interval at or after the view's left edge.
pub fn ruler_ticks(timeline_duration_ms: i64, start_ms: i64, width: f64) -> Vec<(f64, i64)> {
    if timeline_duration_ms <= 0 {
        return Vec::new();
    }
    let interval = ruler_interval_ms(timeline_duration_ms, width);
    let end = start_ms.saturating_add(timeline_duration_ms);
    let mut t = (start_ms / interval).saturating_mul(interval);
    if t < start_ms {
        t = t.saturating_add(interval);
    }
    let mut ticks = Vec::new();
    while t < end {
        ticks.push((
            (t - start_ms) as f64 / timeline_duration_ms as f64 * width,
            t,
        ));
        match t.checked_add(interval) {
            Some(next) => t = next,
            None => break,
        }
    }
    ticks
}

/// Where `PlayheadPainter` draws the playhead in a `width`-wide view, or
/// `None` when the position is outside the visible window (it is simply
/// not here — off-screen left/right, never pinned to the edge).
pub fn playhead_x(
    position_ms: i64,
    start_ms: i64,
    timeline_duration_ms: i64,
    width: f64,
) -> Option<f64> {
    if timeline_duration_ms <= 0 {
        return None;
    }
    let raw = (position_ms - start_ms) as f64 / timeline_duration_ms as f64;
    if !(0.0..=1.0).contains(&raw) {
        return None;
    }
    Some(raw * width)
}

fn outline_path(outline: &[(f64, f64)]) -> Path {
    Path::new(|b| {
        let mut points = outline.iter();
        if let Some(&(x, y)) = points.next() {
            b.move_to(Point::new(x as f32, y as f32));
        }
        for &(x, y) in points {
            b.line_to(Point::new(x as f32, y as f32));
        }
        b.close();
    })
}

// ------------------------------------------------------------------ painters

/// `WaveformPainter`: a track's peaks as a filled, symmetric peak envelope
/// ([`waveform_shape`]) in its colour at [`WaveformPainter::opacity`]
/// (`DESIGN.md`: the clip's waveform is the track colour at 92 %, 45 % when
/// muted). A track shorter than the shared timeline only fills
/// `width_fraction` of the width so lanes align on time.
#[derive(Debug, Clone)]
pub struct WaveformPainter {
    pub peaks: Arc<[f64]>,
    /// The peaks drawn: all of them, or a viewport slice
    /// ([`visible_peak_slice`]'s `range`).
    pub range: Range<usize>,
    pub color: Color,
    pub width_fraction: f64,
    /// Viewport mapping for a whole-peak slice, in PEAK-INDEX units: the
    /// index position at the draw area's left edge, and the index distance
    /// its width covers (`index_span`; `None` = `peaks - 1`, the classic
    /// spread-all-vertices mapping). A slice cut at whole-peak bounds starts
    /// up to one peak interval EARLIER than the viewport edge; anchoring
    /// vertex k at `(k − index_offset) / index_span` keeps every peak at its
    /// true timeline x — aligned with ruler/grid/playhead — instead of
    /// shifting the waveform by the discarded fraction. Vertices landing
    /// outside the box are clipped.
    pub index_offset: f64,
    pub index_span: Option<f64>,
    /// The fill's opacity (of `color`'s own alpha).
    pub opacity: f32,
}

impl WaveformPainter {
    /// All of `peaks`, spread over the full width.
    pub fn new(peaks: Arc<[f64]>, color: Color) -> Self {
        let range = 0..peaks.len();
        Self {
            peaks,
            range,
            color,
            width_fraction: 1.0,
            index_offset: 0.0,
            index_span: None,
            opacity: clip::WAVEFORM,
        }
    }

    /// The viewport slice of `peaks` [`visible_peak_slice`] cut.
    pub fn sliced(peaks: Arc<[f64]>, slice: PeakSlice, color: Color) -> Self {
        Self {
            peaks,
            range: slice.range,
            color,
            width_fraction: 1.0,
            index_offset: slice.index_offset,
            index_span: Some(slice.index_span),
            opacity: clip::WAVEFORM,
        }
    }

    pub fn width_fraction(mut self, fraction: f64) -> Self {
        self.width_fraction = fraction;
        self
    }

    /// The fill's opacity ([`clip::WAVEFORM`] unless set).
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// The painter as a canvas filling its parent.
    pub fn into_canvas<M>(self) -> Canvas<Self, M> {
        canvas_widget(self).width(Length::Fill).height(Length::Fill)
    }

    /// The shape it paints in a box of `size`.
    pub fn shape(&self, size: Size) -> Option<WaveformShape> {
        let peaks = self.peaks.get(self.range.clone()).unwrap_or(&[]);
        waveform_shape(
            peaks,
            f64::from(size.width),
            f64::from(size.height),
            self.width_fraction,
            self.index_offset,
            self.index_span,
        )
    }
}

type WaveformKey = (Same<[f64]>, Range<usize>, [u32; 4], [u64; 3], bool, u32);

impl<M> canvas::Program<M> for WaveformPainter {
    type State = KeyedCache<WaveformKey>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let key = (
            Same(Arc::clone(&self.peaks)),
            self.range.clone(),
            color_key(self.color),
            [
                self.width_fraction.to_bits(),
                self.index_offset.to_bits(),
                self.index_span.unwrap_or(f64::NAN).to_bits(),
            ],
            self.index_span.is_some(),
            self.opacity.to_bits(),
        );
        vec![state.draw(renderer, bounds.size(), key, |frame| {
            let Some(shape) = self.shape(bounds.size()) else {
                return;
            };
            let fill = faded(self.color, self.opacity);
            let paint = |frame: &mut Frame| {
                frame.fill(&outline_path(&shape.outline), fill);
            };
            if shape.clip {
                frame.with_clip(
                    Rectangle::new(
                        Point::ORIGIN,
                        Size::new(shape.draw_width as f32, bounds.height),
                    ),
                    paint,
                );
            } else {
                paint(frame);
            }
        })]
    }
}

/// `LiveWaveformPainter`: a recording-in-progress waveform that grows
/// rightward at a FIXED pixels-per-sample, so audio already drawn never
/// shifts or rescales as more is captured — the wobble-free way Logic / Pro
/// Tools / Cubase draw a live take.
///
/// Unlike [`WaveformPainter`], which re-spreads every peak across the clip's
/// *current* width, this bins samples into fixed pixel columns anchored at
/// the left edge: a given sample always lands in the same column. The
/// mapping uses an ABSOLUTE `pixels_per_sample` (the timeline's fixed
/// pixels-per-ms × the record interval), not `n / width`, so column c covers
/// the exact same sample indices no matter how many more samples arrive.
/// `sample_offset` is the sample-index position at the box's left edge (the
/// fraction a whole-sample viewport slice discarded), which keeps the mapping
/// glued to the TIMELINE, not the box, while the view pans.
#[derive(Debug, Clone)]
pub struct LiveWaveformPainter {
    pub peaks: Arc<[f64]>,
    pub color: Color,
    pub pixels_per_sample: f64,
    pub sample_offset: f64,
}

impl<M> canvas::Program<M> for LiveWaveformPainter {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        if let Some((outline, _)) = live_waveform_outline(
            &self.peaks,
            f64::from(bounds.width),
            f64::from(bounds.height),
            self.pixels_per_sample,
            self.sample_offset,
        ) {
            frame.fill(&outline_path(&outline), faded(self.color, clip::WAVEFORM));
        }
        vec![frame.into_geometry()]
    }
}

/// `TimelineGridPainter`: vertical time-grid lines drawn behind a lane's
/// waveform; `timeline_duration_ms` is the visible span mapped across the
/// width, `start_ms` the absolute time at its left edge.
#[derive(Debug, Clone)]
pub struct TimelineGridPainter {
    pub timeline_duration_ms: i64,
    pub color: Color,
    pub start_ms: i64,
}

impl<M> canvas::Program<M> for TimelineGridPainter {
    type State = KeyedCache<(i64, i64, [u32; 4])>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let key = (
            self.timeline_duration_ms,
            self.start_ms,
            color_key(self.color),
        );
        vec![state.draw(renderer, bounds.size(), key, |frame| {
            for x in grid_line_xs(
                self.timeline_duration_ms,
                self.start_ms,
                f64::from(bounds.width),
            ) {
                let x = crisp(x as f32);
                frame.stroke(
                    &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
                    Stroke::default().with_color(self.color).with_width(1.0),
                );
            }
        })]
    }
}

/// `TimeRulerPainter`: the time ruler above the tracks (`DESIGN.md`,
/// Ruler) — a full-height 1 px line per major tick in `tick_color` with its
/// `m:ss` label ([`ruler_label`]: 10 px tabular figures in `text_color`, 5 px
/// right of the line, 7 px from the top), and 5 px minor ticks along the
/// bottom in `minor_color` between them.
#[derive(Debug, Clone)]
pub struct TimeRulerPainter {
    pub timeline_duration_ms: i64,
    pub tick_color: Color,
    pub minor_color: Color,
    pub text_color: Color,
    pub start_ms: i64,
}

/// A minor tick's length.
pub const MINOR_TICK_HEIGHT: f32 = 5.0;

impl<M> canvas::Program<M> for TimeRulerPainter {
    type State = KeyedCache<(i64, i64, [[u32; 4]; 3])>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let key = (
            self.timeline_duration_ms,
            self.start_ms,
            [
                color_key(self.tick_color),
                color_key(self.minor_color),
                color_key(self.text_color),
            ],
        );
        vec![state.draw(renderer, bounds.size(), key, |frame| {
            let h = bounds.height;
            let w = f64::from(bounds.width);
            let line = |frame: &mut Frame, x: f32, top: f32, color: Color| {
                let x = crisp(x);
                frame.stroke(
                    &Path::line(Point::new(x, top), Point::new(x, h)),
                    Stroke::default().with_color(color).with_width(1.0),
                );
            };
            for x in minor_tick_xs(self.timeline_duration_ms, self.start_ms, w) {
                line(frame, x as f32, h - MINOR_TICK_HEIGHT, self.minor_color);
            }
            for (x, t) in ruler_ticks(self.timeline_duration_ms, self.start_ms, w) {
                let x = x as f32;
                line(frame, x, 0.0, self.tick_color);
                // A `TextPainter`'s line (no height: the font's own, a
                // whole pixel tall), in the system UI font (no family).
                fill_tabular_text(
                    frame,
                    canvas::Text {
                        content: ruler_label(t),
                        position: Point::new(crisp(x) + 4.5, 7.0),
                        color: self.text_color,
                        size: Pixels(10.0),
                        line_height: crate::ui::core::text::NATURAL,
                        font: system_font::font(Weight::Normal),
                        align_y: Vertical::Top,
                        ..canvas::Text::default()
                    },
                );
            }
        })]
    }
}

/// `PlayheadPainter` (mounted as Dart's `PlayheadLayer`: its own layer,
/// ignoring pointers — an iced canvas with no `update` takes no input): the
/// playback playhead as a crisp 1 px vertical line, with the ruler's 11 × 8
/// marker ([`playhead_marker`]) on top when `show_head`. `start_ms` is
/// subtracted from the absolute position before mapping (a take's start,
/// when a panel shows that take in take-local time).
#[derive(Debug, Clone)]
pub struct PlayheadPainter {
    pub position_ms: i64,
    pub timeline_duration_ms: i64,
    pub color: Color,
    pub show_head: bool,
    pub start_ms: i64,
}

impl<M> canvas::Program<M> for PlayheadPainter {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        if let Some(x) = playhead_x(
            self.position_ms,
            self.start_ms,
            self.timeline_duration_ms,
            f64::from(bounds.width),
        ) {
            let x = crisp(x as f32);
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
                Stroke::default().with_color(self.color).with_width(1.0),
            );
            if self.show_head {
                frame.fill(&playhead_marker(x), self.color);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The playhead's marker on the ruler (`DESIGN.md`: a white 11 × 8
/// triangle), its point down at `x` 8 px below the top.
pub fn playhead_marker(x: f32) -> Path {
    Path::new(|b| {
        b.move_to(Point::new(x - 5.5, 0.0));
        b.line_to(Point::new(x + 5.5, 0.0));
        b.line_to(Point::new(x, 8.0));
        b.close();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------ time_grid_interval_test

    fn spacing_px(duration_ms: i64, width: f64) -> f64 {
        nice_time_interval_ms(duration_ms, width) as f64 * width / duration_ms as f64
    }

    #[test]
    fn a_two_hour_timeline_keeps_its_labels_apart() {
        assert!(spacing_px(2 * 3600000, 800.0) >= 72.0);
    }

    #[test]
    fn a_three_hour_timeline_on_a_narrow_pane_keeps_its_labels_apart() {
        assert!(spacing_px(3 * 3600000, 400.0) >= 72.0);
    }

    #[test]
    fn a_timeline_of_years_keeps_its_labels_apart_too() {
        // Held at an hour a step, 2^62 ms drew a line per hour: the screen froze.
        assert!(spacing_px(1 << 62, 800.0) >= 72.0);
        assert!(spacing_px(30 * 24 * 3600000, 800.0) >= 72.0);
    }

    #[test]
    fn the_step_is_never_zero_negative_or_past_the_timeline() {
        // At a few pixels wide, the hours for 2^62 ms overflowed to a negative
        // step, and the grid's loop never ended; at zero width, 1 s a step.
        for width in [0.0, 1.0, 36.0, 800.0] {
            let step = nice_time_interval_ms(1 << 62, width);
            assert!((1..=1i64 << 62).contains(&step), "{width} px");
        }
    }

    #[test]
    fn a_song_length_timeline_keeps_its_fine_steps() {
        assert_eq!(nice_time_interval_ms(180000, 800.0), 30000);
        assert_eq!(nice_time_interval_ms(10000, 800.0), 1000);
    }

    // ------------------------------------------------------ waveform_slice_test

    fn ramp(n: usize) -> Vec<f64> {
        (0..n).map(|i| i as f64 / n as f64).collect()
    }

    #[test]
    fn a_window_not_aligned_to_a_bucket_keeps_the_discarded_fraction() {
        // 1000 buckets over 180 000 ms: one bucket = 180 ms. 4590 ms = bucket
        // 25.5, mid-bucket.
        let peaks = ramp(1000);
        let cut = visible_peak_slice(1000, 0, 180000, 4590, 5590);
        assert_eq!(peaks[cut.range.start], peaks[25]);
        assert!((cut.index_offset - 0.5).abs() < 1e-9);
        // 1000 ms window = 1000/180 buckets wide.
        assert!((cut.index_span - 1000.0 / 180.0).abs() < 1e-9);
    }

    #[test]
    fn a_bucket_aligned_window_has_zero_offset() {
        let cut = visible_peak_slice(1000, 0, 180000, 1800, 3600);
        assert!(cut.index_offset.abs() < 1e-9);
        // Buckets 10..19 are in view, plus bucket 20: the painter draws each
        // peak as a vertex at its bucket's START, so the last in-view bucket
        // only gets a right edge to draw to from the vertex at or past the
        // window's end.
        assert_eq!(cut.range.len(), 11);
        assert!((cut.index_span - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_span_not_starting_at_zero_re_bases_the_window_first() {
        // Clip spanning 5000..10000 ms with 100 peaks (50 ms each). Viewport
        // intersection 5075..6075: starts 1.5 buckets in.
        let cut = visible_peak_slice(100, 5000, 5000, 5075, 6075);
        assert_eq!(cut.range.start, 1);
        assert!((cut.index_offset - 0.5).abs() < 1e-9);
        assert!((cut.index_span - 20.0).abs() < 1e-9);
    }

    #[test]
    fn the_window_clamped_at_the_span_end_still_covers_the_last_bucket() {
        let cut = visible_peak_slice(1000, 0, 180000, 179000, 180000);
        assert_eq!(cut.range.end, 1000);
        assert!(cut.index_offset + cut.index_span <= 1000.0 + 1e-9);
    }

    #[test]
    fn degenerate_inputs_yield_an_empty_harmless_slice() {
        for cut in [
            visible_peak_slice(0, 0, 1000, 0, 500),
            visible_peak_slice(1000, 0, 0, 0, 500),
            visible_peak_slice(1000, 0, 1000, 500, 500),
        ] {
            assert!(cut.range.is_empty());
            assert_eq!(cut.index_offset, 0.0);
            assert_eq!(cut.index_span, 1.0);
        }
    }

    /// Whether `(x, y)` lies inside the closed `outline` (non-zero winding,
    /// Flutter's default fill rule).
    fn inside(outline: &[(f64, f64)], x: f64, y: f64) -> bool {
        let mut winding = 0;
        for i in 0..outline.len() {
            let (x0, y0) = outline[i];
            let (x1, y1) = outline[(i + 1) % outline.len()];
            if y0 <= y {
                if y1 > y && (x1 - x0) * (y - y0) - (x - x0) * (y1 - y0) > 0.0 {
                    winding += 1;
                }
            } else if y1 <= y && (x1 - x0) * (y - y0) - (x - x0) * (y1 - y0) < 0.0 {
                winding -= 1;
            }
        }
        winding != 0
    }

    /// The leftmost and rightmost painted pixel on the row a quarter of the
    /// way down (inside the body of a full-amplitude waveform, above its
    /// axis), sampling pixel centres: Dart's rasterized `_paintedRange`.
    fn painted_range(painter: &WaveformPainter, size: Size) -> (usize, usize) {
        let shape = painter.shape(size).expect("something painted");
        let y = (size.height as usize / 4) as f64 + 0.5;
        let mut lo = None;
        let mut hi = None;
        for px in 0..size.width as usize {
            let x = px as f64 + 0.5;
            if shape.clip && x >= shape.draw_width {
                continue;
            }
            if inside(&shape.outline, x, y) {
                lo.get_or_insert(px);
                hi = Some(px);
            }
        }
        (lo.expect("nothing painted"), hi.expect("nothing painted"))
    }

    /// A burst shorter than one pixel's worth of peaks still draws: decimated
    /// to the peak nearest each vertex, one that fell between vertices
    /// vanished.
    #[test]
    fn a_burst_between_decimated_vertices_still_draws() {
        let mut peaks = vec![0.0; 1000];
        for p in &mut peaks[403..=407] {
            *p = 1.0;
        }
        let painter = WaveformPainter::new(peaks.into(), Color::WHITE);
        let (lo, hi) = painted_range(&painter, Size::new(100.0, 40.0));
        assert!((38..=42).contains(&lo), "{lo}");
        assert!((38..=42).contains(&hi), "{hi}");
    }

    // What the slice is FOR: a full-amplitude clip must paint the whole box
    // it is visible in. The cut stopped one peak short of the window's right
    // edge and the painter drew nothing past the last vertex, so the strip
    // after it stayed blank — the clip's final bucket never drew, and a
    // window inside one bucket (a long take at max zoom) drew nothing at all.
    fn sliced_range(
        span_dur_ms: i64,
        visible_start_ms: i64,
        visible_end_ms: i64,
    ) -> (usize, usize) {
        let full: Arc<[f64]> = vec![1.0; 1000].into();
        let cut = visible_peak_slice(1000, 0, span_dur_ms, visible_start_ms, visible_end_ms);
        let painter = WaveformPainter::sliced(full, cut, Color::WHITE);
        painted_range(&painter, Size::new(500.0, 40.0))
    }

    #[test]
    fn a_sliced_window_ending_mid_clip_paints_out_to_the_edges() {
        // 180 ms buckets; 4590..5750 ms = buckets 25.5 .. 31.94.
        assert_eq!(sliced_range(180000, 4590, 5750), (0, 499));
    }

    #[test]
    fn a_bucket_aligned_slice_paints_out_to_the_right_edge() {
        assert_eq!(sliced_range(180000, 1800, 3600).1, 499);
    }

    #[test]
    fn a_window_ending_at_the_clip_end_draws_the_final_bucket() {
        // No peak past the clip's last one to draw to: the painter holds the
        // last peak out to the edge instead.
        assert_eq!(sliced_range(180000, 179000, 180000).1, 499);
    }

    #[test]
    fn a_window_inside_one_bucket_paints_the_whole_box() {
        // A 20-minute clip: 1200 ms buckets, viewed through a 1 s window.
        assert_eq!(sliced_range(1200000, 600100, 601100), (0, 499));
    }

    #[test]
    fn nothing_to_draw_draws_nothing() {
        assert!(waveform_shape(&[], 100.0, 40.0, 1.0, 0.0, None).is_none());
        assert!(waveform_shape(&[1.0], 100.0, 40.0, 0.0, 0.0, None).is_none());
        // A lone peak fills the box instead of drawing nothing.
        let shape = waveform_shape(&[1.0], 100.0, 40.0, 1.0, 0.0, None).unwrap();
        assert!(shape.outline.iter().any(|&(x, _)| x == 100.0));
    }

    /// The top edge's height above the middle at x (`None` past it).
    fn top_at(shape: &WaveformShape, x: f64) -> Option<f64> {
        let top: Vec<(f64, f64)> = shape
            .outline
            .iter()
            .copied()
            .take_while(|&(_, y)| y <= shape.mid)
            .collect();
        top.windows(2)
            .find(|w| w[0].0 <= x && x < w[1].0 && w[0].1 == w[1].1)
            .map(|w| shape.mid - w[0].1)
    }

    #[test]
    fn each_column_takes_the_loudest_peak_it_covers() {
        // Ten peaks a column, one loud in the fourth column.
        let mut peaks = vec![0.1; 1001];
        peaks[35] = 1.0;
        let shape = waveform_shape(&peaks, 100.0, 40.0, 1.0, 0.0, None).unwrap();
        let reach = 20.0 * PEAK_REACH;
        assert!((top_at(&shape, 3.5).unwrap() - reach).abs() < 1e-9);
        assert!((top_at(&shape, 2.5).unwrap() - 0.1 * reach).abs() < 1e-9);
        assert!((top_at(&shape, 4.5).unwrap() - 0.1 * reach).abs() < 1e-9);
        // Symmetric: the bottom edge mirrors the top.
        let lowest = shape.outline.iter().map(|&(_, y)| y).fold(0.0, f64::max);
        assert!((lowest - (20.0 + reach)).abs() < 1e-9);
    }

    #[test]
    fn zoomed_in_a_peak_is_a_flat_step_not_a_ramp() {
        // Four peaks over 100 px: each holds its level across its stretch.
        let shape = waveform_shape(&[0.2, 1.0, 0.2, 0.2], 100.0, 40.0, 1.0, 0.0, None).unwrap();
        let reach = 20.0 * PEAK_REACH;
        for x in [34.0, 40.0, 50.0, 60.0, 66.0] {
            assert!((top_at(&shape, x).unwrap() - reach).abs() < 1e-9, "{x}");
        }
        assert!((top_at(&shape, 20.0).unwrap() - 0.2 * reach).abs() < 1e-9);
    }

    #[test]
    fn silence_keeps_a_hairline() {
        let shape = waveform_shape(&[0.0; 10], 100.0, 40.0, 1.0, 0.0, None).unwrap();
        assert!((top_at(&shape, 50.0).unwrap() - SILENCE_HALF_HEIGHT).abs() < 1e-9);
    }

    #[test]
    fn a_short_track_fills_only_its_fraction() {
        let shape = waveform_shape(&[0.5, 0.5, 0.5], 200.0, 40.0, 0.25, 0.0, None).unwrap();
        assert_eq!(shape.draw_width, 50.0);
        assert!(shape.outline.iter().all(|&(x, _)| x <= 50.0));
        assert!(!shape.clip);
    }

    // ------------------------------------------------------ live waveform

    #[test]
    fn a_live_column_keeps_its_samples_as_the_take_grows() {
        let short: Vec<f64> = (0..50).map(|i| (i % 7) as f64 / 7.0).collect();
        let mut long = short.clone();
        long.extend((0..50).map(|_| 1.0));
        let (a, _) = live_waveform_outline(&short, 100.0, 40.0, 2.0, 0.0).unwrap();
        let (b, _) = live_waveform_outline(&long, 100.0, 40.0, 2.0, 0.0).unwrap();
        // The first 25 columns (50 samples at 2 px each) are pixel-identical.
        assert_eq!(a[..26], b[..26]);
    }

    // ------------------------------------------------------ grid, ruler, playhead

    #[test]
    fn the_grid_skips_the_left_edge_and_the_ruler_does_not() {
        // 80 px a second: majors every 2 s keep them 96 px apart or more.
        let xs = grid_line_xs(10000, 0, 800.0);
        assert_eq!(xs.first().copied(), Some(160.0));
        assert_eq!(xs.len(), 4);
        let ticks = ruler_ticks(10000, 0, 800.0);
        assert_eq!(ticks.first().copied(), Some((0.0, 0)));
        assert_eq!(ticks.len(), 5);
        // A view starting mid-interval: the ruler's first tick is the next one.
        let ticks = ruler_ticks(10000, 500, 800.0);
        assert_eq!(ticks[0].1, 2000);
        assert!((ticks[0].0 - 120.0).abs() < 1e-9);
    }

    #[test]
    fn a_song_across_the_arrange_has_a_major_tick_every_10_s() {
        // The design's window: a 42 s song over a 670 px arrange.
        assert_eq!(ruler_interval_ms(42_000, 670.0), 10_000);
        let ticks: Vec<i64> = ruler_ticks(42_000, 0, 670.0)
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(ticks, [0, 10_000, 20_000, 30_000, 40_000]);
        // 2 s minor ticks between them, never on them.
        let minors = minor_tick_xs(42_000, 0, 670.0);
        assert_eq!(minors.len(), 21 - 4 - 1);
        let px_per_ms = 670.0 / 42_000.0;
        assert!((minors[0] - 2000.0 * px_per_ms).abs() < 1e-9);
        assert!(minors
            .iter()
            .all(|x| (x / px_per_ms).round() as i64 % 10_000 != 0));
    }

    #[test]
    fn minor_ticks_too_close_to_read_are_left_out() {
        // A 10-minute song in 300 px: 5-minute majors, minute minors 30 px
        // apart.
        assert_eq!(minor_tick_xs(600_000, 0, 300.0).len(), 8);
        // Majors keep 96 px apart, so minors crowd only where the whole
        // timeline is one tick on a ruler squeezed to a few pixels.
        assert!(minor_tick_xs(1_000_000_000, 0, 20.0).is_empty());
    }

    #[test]
    fn ruler_labels_read_minutes_and_seconds() {
        assert_eq!(ruler_label(0), "0:00");
        assert_eq!(ruler_label(10_000), "0:10");
        assert_eq!(ruler_label(768_000), "12:48");
        assert_eq!(ruler_label(3_723_000), "1:02:03");
        assert_eq!(ruler_label(-5), "0:00");
    }

    #[test]
    fn a_crisp_line_sits_in_the_middle_of_its_pixel() {
        assert_eq!(crisp(10.0), 10.5);
        assert_eq!(crisp(10.9), 10.5);
    }

    #[test]
    fn format_clock_reads_minutes_and_seconds() {
        assert_eq!(format_clock_ms(0), "00:00");
        assert_eq!(format_clock_ms(-5), "00:00");
        assert_eq!(format_clock_ms(61999), "01:01");
        assert_eq!(format_clock_ms(6_000_000), "100:00");
    }

    #[test]
    fn the_playhead_is_not_drawn_outside_its_window() {
        assert_eq!(playhead_x(500, 0, 1000, 200.0), Some(100.0));
        assert_eq!(playhead_x(1500, 1000, 1000, 200.0), Some(100.0));
        assert_eq!(playhead_x(-1, 0, 1000, 200.0), None);
        assert_eq!(playhead_x(1001, 0, 1000, 200.0), None);
        assert_eq!(playhead_x(1000, 0, 1000, 200.0), Some(200.0));
        assert_eq!(playhead_x(10, 0, 0, 200.0), None);
    }
}
