//! Port of `ui/core/widgets/spectrogram_painter.dart`: the take spectrogram
//! image (pixels, the column cap, the shared render cache and the widget that
//! places it on the time axis), the live spectrogram's incremental raster,
//! and the analysis overlays — frequency grid, power spectrum, formant and F0
//! tracks (take and live) and the vowel chart.
//!
//! The painters read the analysis models (`vocara_model::models::spectrogram`
//! and `voice_metrics`) through two narrow traits, [`SpectrogramData`] and
//! [`FormantTracks`], holding exactly the fields they draw; frame times come
//! from the models' own [`AnalysisFraming`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use iced::alignment::Vertical;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{canvas as canvas_widget, Space};
use iced::{
    mouse, Color, Element, Length, Pixels, Point, Rectangle, Renderer, Size, Task, Theme, Vector,
};

use super::painter::{KeyedCache, Same};
use crate::ui::core::theme::{alpha, font, Studio};

// ------------------------------------------------------------- model seam

pub use vocara_model::models::spectrogram::AnalysisFraming;
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::VoiceMetrics;

/// What the spectrogram painters read of a [`Spectrogram`]: a
/// [`frames`](AnalysisFraming::frames) × [`bins`](Self::bins) grid of dB
/// values, row-major (`db[frame * bins + bin]`), and its peak.
pub trait SpectrogramData: AnalysisFraming + Send + Sync {
    fn bins(&self) -> usize;
    fn db(&self) -> &[f32];
    fn max_db(&self) -> f64;
}

impl SpectrogramData for Spectrogram {
    fn bins(&self) -> usize {
        self.bins
    }
    fn db(&self) -> &[f32] {
        &self.db
    }
    fn max_db(&self) -> f64 {
        self.max_db
    }
}

/// What the formant overlay reads of a [`VoiceMetrics`]: the F0 and F1–F3
/// curves, one value per frame (Hz; NaN or ≤ 0 where unvoiced).
pub trait FormantTracks: AnalysisFraming + Send + Sync {
    fn f0(&self) -> &[f32];
    fn f1(&self) -> &[f32];
    fn f2(&self) -> &[f32];
    fn f3(&self) -> &[f32];
}

impl FormantTracks for VoiceMetrics {
    fn f0(&self) -> &[f32] {
        &self.f0
    }
    fn f1(&self) -> &[f32] {
        &self.f1
    }
    fn f2(&self) -> &[f32] {
        &self.f2
    }
    fn f3(&self) -> &[f32] {
        &self.f3
    }
}

// ------------------------------------------------------------------ colours

/// Colors for the first three formants (F1 blue, F2 green, F3 amber) —
/// matches the Estill Voiceprint convention and reads on the dark
/// spectrogram.
pub const FORMANT_COLORS: [Color; 3] = [
    Color::from_rgb8(0x5B, 0x8D, 0xEF),
    Color::from_rgb8(0x52, 0xB7, 0x88),
    Color::from_rgb8(0xE0, 0xA0, 0x50),
];

/// The F0 (pitch) overlay color — bright white, distinct from the formants.
pub const F0_COLOR: Color = Color::from_rgb8(0xF2, 0xF2, 0xF8);

/// For the 2048-pt Hanning STFT used app-wide, a full-scale tone reads ~54 dB
/// (20·log10(fftSize/4)). The color ceiling is clamped to at least this so an
/// idle/near-silent input keeps its noise floor dark instead of being
/// stretched to full brightness; once there's real voice the peak rises above
/// it and the scale tracks the signal normally. Fixes the "all bright until
/// you speak" live startup artifact.
pub const MIN_CEIL_DB: f64 = 48.0;

/// Heat colormap stops (dark -> indigo -> purple -> coral -> amber -> white).
const STOPS: [[u8; 3]; 6] = [
    [14, 14, 18],
    [28, 26, 60],
    [92, 40, 112],
    [224, 122, 106],
    [251, 191, 36],
    [255, 255, 255],
];

/// Writes the heat color for `t` ∈ [0, 1] at `px[idx..idx + 4]`.
fn heat(t: f64, px: &mut [u8], idx: usize) {
    let n = STOPS.len() - 1;
    // A NON-FINITE t fails BOTH comparisons of the clamp and would reach
    // `floor()` as NaN, which threw in Dart and took the whole dock down. Map
    // it to the bottom of the scale — the "nothing measurable here" colour,
    // which is what an unusable bin should look like.
    let clamped = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let scaled = clamped * n as f64;
    let mut i = scaled.floor() as usize;
    if i >= n {
        i = n - 1;
    }
    let f = scaled - i as f64;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    for ch in 0..3 {
        let (a, b) = (f64::from(a[ch]), f64::from(b[ch]));
        px[idx + ch] = (a + (b - a) * f).round() as u8;
    }
    px[idx + 3] = 255;
}

// ------------------------------------------------------------ take image

/// The widest take spectrogram image, in columns. One column per analysis
/// frame is ~94 a second, so a take past ~3 minutes is wider than a GPU
/// texture holds (16384 on the Macs this runs on), and the engine then
/// scales the decoded image to fit — which cut a 10-minute take's 215
/// frequency rows to 62 and dropped frames to a plain linear resample.
/// Longer takes are max-pooled to this many columns instead: every frequency
/// row stays, and the loudest frame of each group survives.
pub const MAX_SPECTROGRAM_IMAGE_COLUMNS: usize = 16384;

/// RGBA pixels and their size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels8 {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// The RGBA pixels of `s` as an image (low frequency at the bottom),
/// auto-scaled to a `dynamic_range_db` window below the peak (72 dB in the
/// app): one column per frame, or — past `max_columns` frames —
/// `max_columns` columns, column `c` holding the per-bin maximum of frames
/// `[c·frames/w, (c+1)·frames/w)`, so each still covers an equal share of the
/// take. Pure, so it runs on a worker thread; `s` must be non-empty.
pub fn spectrogram_pixels(
    s: &dyn SpectrogramData,
    dynamic_range_db: f64,
    max_columns: usize,
) -> Pixels8 {
    let frames = s.frames();
    let h = s.bins();
    let db = s.db();
    let w = if frames <= max_columns {
        frames
    } else {
        max_columns
    };
    let mut pixels = vec![0u8; w * h * 4];
    // A non-finite peak would make ceil, floor and range all NaN, so every
    // bin would come out non-finite rather than just the offending ones.
    let peak = if s.max_db().is_finite() {
        s.max_db()
    } else {
        MIN_CEIL_DB
    };
    let ceil = if peak < MIN_CEIL_DB {
        MIN_CEIL_DB
    } else {
        peak
    };
    let floor = ceil - dynamic_range_db;
    let range = if ceil - floor <= 0.0 {
        1.0
    } else {
        ceil - floor
    };

    if w == frames {
        for f in 0..w {
            let base = f * h;
            for b in 0..h {
                let t = (f64::from(db[base + b]) - floor) / range;
                let idx = ((h - 1 - b) * w + f) * 4; // flip: low freq at bottom
                heat(t, &mut pixels, idx);
            }
        }
        return Pixels8 {
            pixels,
            width: w,
            height: h,
        };
    }

    // Max-pool. A NaN bin loses every comparison, so a group of only NaN
    // stays at -infinity — non-finite, which `heat` paints like the NaN.
    let mut max_db = vec![f64::NEG_INFINITY; h];
    for c in 0..w {
        max_db.fill(f64::NEG_INFINITY);
        let last = (c + 1) * frames / w;
        for f in c * frames / w..last {
            let base = f * h;
            for (b, m) in max_db.iter_mut().enumerate() {
                let v = f64::from(db[base + b]);
                if v > *m {
                    *m = v;
                }
            }
        }
        for (b, m) in max_db.iter().enumerate() {
            heat((m - floor) / range, &mut pixels, ((h - 1 - b) * w + c) * 4);
        }
    }
    Pixels8 {
        pixels,
        width: w,
        height: h,
    }
}

/// Decodes `width` × `height` RGBA `pixels` into an image handle. A
/// zero-size image becomes a 1 × 1 transparent placeholder (Flutter's
/// decoder never completed on a zero-size buffer; iced's is as unhappy).
pub fn decode_rgba_image(pixels: Vec<u8>, width: usize, height: usize) -> Handle {
    if width == 0 || height == 0 {
        return Handle::from_rgba(1, 1, vec![0u8; 4]);
    }
    Handle::from_rgba(width as u32, height as u32, pixels)
}

/// Renders a spectrogram to an image (low frequency at the bottom),
/// auto-scaled to a 72 dB window below the peak, at most
/// [`MAX_SPECTROGRAM_IMAGE_COLUMNS`] wide. An empty spectrogram (a
/// not-yet-analysed or silent take) renders a 1 × 1 transparent placeholder.
/// A colour lookup per bin per frame — ~12M for a 10-minute take — so call
/// it off the UI thread.
pub fn render_spectrogram_image(s: &dyn SpectrogramData) -> Handle {
    if s.frames() == 0 || s.bins() == 0 {
        return decode_rgba_image(Vec::new(), 0, 0);
    }
    let px = spectrogram_pixels(s, 72.0, MAX_SPECTROGRAM_IMAGE_COLUMNS);
    decode_rgba_image(px.pixels, px.width, px.height)
}

/// How many rendered take images the cache keeps its own handle on.
const SPECTROGRAM_IMAGE_CACHE_SIZE: usize = 4;

type SharedRender = Arc<OnceLock<Handle>>;

/// The most recently requested renders, oldest first, keyed WEAKLY by
/// spectrogram identity: the analysis cache owns a spectrogram's lifetime
/// under its byte budget, and a strong key here kept every recently shown one
/// (tens to hundreds of MB each) resident after the cache let it go — and
/// after the practice was closed.
static RECENT_IMAGES: Mutex<Vec<(Weak<dyn SpectrogramData>, SharedRender)>> =
    Mutex::new(Vec::new());

/// A handle on `s`'s rendered image (see [`render_spectrogram_image`]),
/// shared with every other caller for the same spectrogram (analyses are
/// never mutated; a new one is a new `Arc`): the dock tab and the take's
/// inline lane show the same one, and switching tabs or folding a lane open
/// does not render it again. Blocks while rendering, so call it off the UI
/// thread; concurrent callers for the same spectrogram wait for the one
/// render. Handles already given out stay valid after the cache lets its own
/// go.
pub fn spectrogram_image_for<S: SpectrogramData + 'static>(s: &Arc<S>) -> Handle {
    let s: Arc<dyn SpectrogramData> = s.clone();
    cached_image(&s)
}

/// [`spectrogram_image_for`] on a type-erased spectrogram (the same
/// allocation, so the same identity).
fn cached_image(s: &Arc<dyn SpectrogramData>) -> Handle {
    let shared = {
        let mut recent = RECENT_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
        // A dead key cannot match: its address may be a new spectrogram's.
        let key = Arc::downgrade(s);
        let found = recent
            .iter()
            .position(|(k, _)| k.strong_count() > 0 && Weak::ptr_eq(k, &key));
        let shared = match found {
            Some(i) => recent.remove(i).1,
            None => Arc::new(OnceLock::new()),
        };
        recent.push((key, Arc::clone(&shared))); // most recently used
        while recent.len() > SPECTROGRAM_IMAGE_CACHE_SIZE {
            // Handles already cloned off it stay valid: an image's memory
            // goes with its last handle.
            recent.remove(0);
        }
        shared
    };
    // Rendered outside the lock. A render that panics leaves the cell empty,
    // so the next caller tries again (a failed render isn't kept).
    shared
        .get_or_init(|| render_spectrogram_image(s.as_ref()))
        .clone()
}

/// Lets go of the cache's own image handles when the screen that showed
/// them closes (the song workspace, the technique page). Their spectrograms
/// go with that screen's analyses, so no lookup could reach them again, yet
/// up to four textures (~14 MB each for a 3-minute take) stayed resident
/// until as many newer takes were rendered. An image still on screen keeps
/// its own handle; one asked for again renders afresh.
pub fn release_spectrogram_images() {
    RECENT_IMAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Where a spectrogram's image goes in a `size` box spanning
/// `timeline_duration_ms` of the spectrogram's own time from its left edge:
/// over its frames' cells only (`cell_start(0)` to `cell_end(frames - 1)`),
/// so every column sits over the moment its window is centred on — the same
/// mapping the overlays, the playhead and a seek use. Stretching the image
/// over the whole box instead spreads the columns over the take's full
/// length, off the time axis. A max-pooled image spans the same cells:
/// pooled column c covers frames [c·frames/w, (c+1)·frames/w), exactly its
/// share of that extent. `None` when there is nothing to place.
pub fn spectrogram_image_rect(
    s: &dyn SpectrogramData,
    timeline_duration_ms: i64,
    size: Size,
) -> Option<Rectangle> {
    let frames = s.frames();
    if frames == 0 || timeline_duration_ms <= 0 {
        return None;
    }
    let dur = timeline_duration_ms as f64;
    let start = s.cell_start(0) * 1000.0 / dur;
    let end = s.cell_end(frames as i64 - 1) * 1000.0 / dur;
    let w = f64::from(size.width);
    Some(Rectangle::new(
        Point::new((start * w) as f32, 0.0),
        Size::new(((end - start) * w) as f32, size.height),
    ))
}

/// `SpectrogramImage`: renders a spectrogram's image off the UI thread and
/// shows it on its time axis (see [`spectrogram_image_rect`]). The screen
/// owns one per place it shows a spectrogram, calls [`show`](Self::show)
/// with the spectrogram it should show (every view, or when it changes),
/// runs the returned request as a task and hands the result to
/// [`rendered`](Self::rendered).
#[derive(Default)]
pub struct SpectrogramImage {
    spectrogram: Option<Arc<dyn SpectrogramData>>,
    image: Option<Handle>,
    generation: u64,
}

/// A render [`SpectrogramImage::show`] asked for.
pub struct RenderRequest {
    generation: u64,
    spectrogram: Arc<dyn SpectrogramData>,
}

/// A finished render, for [`SpectrogramImage::rendered`]. `image` is `None`
/// when the render failed: nothing to show, and a later show renders again.
#[derive(Debug, Clone)]
pub struct Rendered {
    generation: u64,
    image: Option<Handle>,
}

impl RenderRequest {
    /// Renders on the current thread (through the shared cache).
    pub fn run(self) -> Rendered {
        let image = crate::util::run_caught::<_, ()>(|| Ok(cached_image(&self.spectrogram))).ok();
        Rendered {
            generation: self.generation,
            image,
        }
    }

    /// Renders on a blocking thread and maps the result to a message.
    pub fn into_task<M: Send + 'static>(
        self,
        on_done: impl FnOnce(Rendered) -> M + Send + 'static,
    ) -> Task<M> {
        let generation = self.generation;
        Task::perform(crate::util::blocking(move || self.run()), move |joined| {
            on_done(joined.unwrap_or(Rendered {
                generation,
                image: None,
            }))
        })
    }
}

impl SpectrogramImage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows `s`. A different spectrogram (by identity) drops the image on
    /// screen at once — the old take's image must not stand in meanwhile:
    /// it would sit over the NEW take's frames, under the new take's
    /// overlays, for as long as the render takes — and returns the render to
    /// run. The same one returns `None`.
    pub fn show<S: SpectrogramData + 'static>(&mut self, s: &Arc<S>) -> Option<RenderRequest> {
        let s: Arc<dyn SpectrogramData> = s.clone();
        if self
            .spectrogram
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &s))
        {
            return None;
        }
        self.image = None;
        self.spectrogram = Some(Arc::clone(&s));
        self.generation += 1;
        Some(RenderRequest {
            generation: self.generation,
            spectrogram: s,
        })
    }

    /// Lands a render. A stale one — a newer `show` started since (rapid
    /// take switches, whose renders don't necessarily complete in order) — is
    /// dropped: without the generation check an earlier render completing
    /// last would persistently show the wrong take.
    pub fn rendered(&mut self, done: Rendered) {
        if done.generation != self.generation {
            return;
        }
        if let Some(image) = done.image {
            self.image = Some(image);
        }
    }

    /// The image on screen, if its render has landed.
    pub fn image(&self) -> Option<&Handle> {
        self.image.as_ref()
    }

    /// The image placed on a `timeline_duration_ms` axis filling the parent;
    /// nothing while it renders, for an empty spectrogram or with no axis.
    pub fn view<'a, M: 'a>(&self, timeline_duration_ms: i64) -> Element<'a, M> {
        match (&self.image, &self.spectrogram) {
            (Some(image), Some(s)) if s.frames() > 0 && timeline_duration_ms > 0 => {
                canvas_widget(SpectrogramImagePainter {
                    image: image.clone(),
                    spectrogram: Arc::clone(s),
                    timeline_duration_ms,
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            }
            _ => Space::new().width(Length::Fill).height(Length::Fill).into(),
        }
    }
}

struct SpectrogramImagePainter {
    image: Handle,
    spectrogram: Arc<dyn SpectrogramData>,
    timeline_duration_ms: i64,
}

impl<M> canvas::Program<M> for SpectrogramImagePainter {
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
        if let Some(dst) = spectrogram_image_rect(
            self.spectrogram.as_ref(),
            self.timeline_duration_ms,
            bounds.size(),
        ) {
            frame.draw_image(
                dst,
                canvas::Image::new(self.image.clone()).filter_method(FilterMethod::Linear),
            );
        }
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------------ live image

/// Brightness multiplier applied to unvoiced live columns — a voiceprint
/// should read dark when there is no voice, so room noise/breath that scrolls
/// by is pushed toward black instead of glowing at the auto-scaled floor.
const IDLE_COLUMN_DIM: f64 = 0.32;

/// The colour ceiling for a live window: its loudest bin, but at least
/// [`MIN_CEIL_DB`].
fn live_ceil(column_peaks: impl Iterator<Item = f64>) -> f64 {
    let mut ceil = -1e9;
    for v in column_peaks {
        if v > ceil {
            ceil = v;
        }
    }
    if ceil < MIN_CEIL_DB {
        MIN_CEIL_DB
    } else {
        ceil
    }
}

/// A live column's loudest bin (-1e9 when it has none).
fn column_peak(col: &[f32]) -> f64 {
    let mut peak = -1e9;
    for &v in col {
        let v = f64::from(v);
        if v > peak {
            peak = v;
        }
    }
    peak
}

/// Whether live column `f` is unvoiced (0 in `active`).
fn unvoiced(active: Option<&[u8]>, f: usize) -> bool {
    active.is_some_and(|a| f < a.len() && a[f] == 0)
}

/// Paints live column `col` into image column `x` of `pixels` (`w` wide,
/// one row per bin, low frequency at the bottom), dimmed when unvoiced.
#[allow(clippy::too_many_arguments)]
fn paint_live_column(
    pixels: &mut [u8],
    w: usize,
    bins: usize,
    x: usize,
    col: &[f32],
    unvoiced: bool,
    floor: f64,
    range: f64,
) {
    let dim = if unvoiced { IDLE_COLUMN_DIM } else { 1.0 };
    for (b, &v) in col[..bins].iter().enumerate() {
        let t = (f64::from(v) - floor) / range * dim;
        heat(t, pixels, ((bins - 1 - b) * w + x) * 4);
    }
}

/// One live spectrogram column: dB per bin.
pub type LiveColumn = Arc<[f32]>;

/// The RGBA pixels of a live window: `columns` (each `bins` long) oldest on
/// the left, auto-scaled below the peak (72 dB in the app), unvoiced columns
/// (0 in `active`) dimmed. `columns` must be non-empty.
pub fn live_columns_pixels<C: AsRef<[f32]>>(
    columns: &[C],
    bins: usize,
    dynamic_range_db: f64,
    active: Option<&[u8]>,
) -> Vec<u8> {
    let w = columns.len();
    let mut pixels = vec![0u8; w * bins * 4];
    let ceil = live_ceil(columns.iter().map(|c| column_peak(c.as_ref())));
    let floor = ceil - dynamic_range_db;
    let range = if ceil - floor <= 0.0 {
        1.0
    } else {
        ceil - floor
    };
    for (f, col) in columns.iter().enumerate() {
        paint_live_column(
            &mut pixels,
            w,
            bins,
            f,
            col.as_ref(),
            unvoiced(active, f),
            floor,
            range,
        );
    }
    pixels
}

/// Renders a live rolling window of dB `columns` (each `bins` long) to an
/// image, oldest column on the left (see [`live_columns_pixels`]). No
/// columns, or no bins, render the 1 × 1 placeholder (the two renderers
/// cannot disagree about it).
pub fn render_columns_image<C: AsRef<[f32]>>(
    columns: &[C],
    bins: usize,
    active: Option<&[u8]>,
) -> Handle {
    if columns.is_empty() || bins == 0 {
        return decode_rgba_image(Vec::new(), 0, 0);
    }
    decode_rgba_image(
        live_columns_pixels(columns, bins, 72.0, active),
        columns.len(),
        bins,
    )
}

/// [`live_columns_pixels`] for the live monitor's rolling window, kept up to
/// date incrementally. The window only ever gains columns on the right and
/// loses them on the left, and while its colour ceiling (its loudest bin)
/// holds, a column already on screen keeps its colours — so an update shifts
/// the previous pixels left and colours only the new columns, instead of the
/// whole window (720 × 215 bins) on every frame. When the ceiling moves, or
/// the window isn't a continuation of the last one, it repaints in full.
/// Either way the pixels are exactly [`live_columns_pixels`]'.
///
/// Columns are compared by identity (the monitor's columns are stable
/// buffers: a column is appended once and evicted once), as Dart compared the
/// `Float32List`s.
pub struct LiveColumnsRaster {
    dynamic_range_db: f64,
    columns: Vec<LiveColumn>,
    active: Option<Vec<u8>>,
    bins: usize,
    ceil: f64,
    pixels: Vec<u8>,
    /// Each column's loudest bin, computed once per column (keyed by the
    /// buffer's address; only the window's own columns, which `columns`
    /// keeps alive, are kept).
    peaks: HashMap<usize, f64>,
    /// How many columns the last update coloured (the rest were shifted).
    coloured: usize,
}

impl Default for LiveColumnsRaster {
    fn default() -> Self {
        Self::new(72.0)
    }
}

impl LiveColumnsRaster {
    pub fn new(dynamic_range_db: f64) -> Self {
        Self {
            dynamic_range_db,
            columns: Vec::new(),
            active: None,
            bins: 0,
            ceil: f64::NAN,
            pixels: Vec::new(),
            peaks: HashMap::new(),
            coloured: 0,
        }
    }

    /// How many columns the last [`update`](Self::update) coloured; the
    /// others it carried over from the previous pixels.
    pub fn coloured_columns(&self) -> usize {
        self.coloured
    }

    /// Pixels for `columns` (non-empty) as [`live_columns_pixels`] would
    /// paint them, in a buffer the caller owns.
    pub fn update(
        &mut self,
        columns: &[LiveColumn],
        bins: usize,
        active: Option<&[u8]>,
    ) -> Vec<u8> {
        let w = columns.len();
        let addr = |c: &LiveColumn| c.as_ptr() as usize;
        for c in columns {
            self.peaks.entry(addr(c)).or_insert_with(|| column_peak(c));
        }
        let ceil = live_ceil(columns.iter().map(|c| self.peaks[&addr(c)]));
        let floor = ceil - self.dynamic_range_db;
        let range = if ceil - floor <= 0.0 {
            1.0
        } else {
            ceil - floor
        };

        let kept = if bins == self.bins && ceil == self.ceil {
            self.continuation(columns, active)
        } else {
            None
        };
        let mut from = 0;
        let mut pixels = match kept {
            None => vec![0u8; w * bins * 4],
            Some((evicted, overlap)) => {
                // Shift the columns still on screen to the left edge, row by row.
                let old_w = self.columns.len();
                let mut pixels = if w == old_w {
                    std::mem::take(&mut self.pixels)
                } else {
                    vec![0u8; w * bins * 4]
                };
                for row in 0..bins {
                    let src = (row * old_w + evicted) * 4..(row * old_w + evicted + overlap) * 4;
                    let dst = row * w * 4;
                    if w == old_w {
                        pixels.copy_within(src, dst);
                    } else {
                        pixels[dst..dst + overlap * 4].copy_from_slice(&self.pixels[src]);
                    }
                }
                from = overlap;
                pixels
            }
        };
        for (f, col) in columns.iter().enumerate().skip(from) {
            paint_live_column(
                &mut pixels,
                w,
                bins,
                f,
                col,
                unvoiced(active, f),
                floor,
                range,
            );
        }
        self.coloured = w - from;

        self.columns = columns.to_vec();
        self.active = active.map(<[u8]>::to_vec);
        self.bins = bins;
        self.ceil = ceil;
        let live: std::collections::HashSet<usize> = self.columns.iter().map(addr).collect();
        self.peaks.retain(|k, _| live.contains(k));
        self.pixels = pixels;
        self.pixels.clone()
    }

    /// When `columns` continues the last window — its first columns are the
    /// last window's, from some point on, in order, with the same voicing —
    /// how many columns were evicted from the left and how many carry over.
    fn continuation(
        &self,
        columns: &[LiveColumn],
        active: Option<&[u8]>,
    ) -> Option<(usize, usize)> {
        let old = &self.columns;
        let first = columns.first()?;
        if old.is_empty() {
            return None;
        }
        let evicted = old.iter().position(|c| Arc::ptr_eq(c, first))?;
        let overlap = old.len() - evicted;
        if overlap > columns.len() {
            return None;
        }
        let old_active = self.active.as_deref();
        for i in 0..overlap {
            if !Arc::ptr_eq(&old[evicted + i], &columns[i]) {
                return None;
            }
            if unvoiced(old_active, evicted + i) != unvoiced(active, i) {
                return None;
            }
        }
        Some((evicted, overlap))
    }
}

// ------------------------------------------------------------- overlays

/// `FrequencyGridPainter`: horizontal frequency grid lines every kHz below
/// `max_hz`, and the Singer's Formant band (2.6–3.8 kHz by default) shaded,
/// drawn over the spectrogram.
#[derive(Debug, Clone)]
pub struct FrequencyGridPainter {
    pub max_hz: f64,
    pub singers_formant_low: f64,
    pub singers_formant_high: f64,
}

impl FrequencyGridPainter {
    pub fn new(max_hz: f64) -> Self {
        Self {
            max_hz,
            singers_formant_low: 2600.0,
            singers_formant_high: 3800.0,
        }
    }

    fn y(&self, hz: f64, height: f32) -> f32 {
        (f64::from(height) * (1.0 - hz / self.max_hz)) as f32
    }
}

impl<M> canvas::Program<M> for FrequencyGridPainter {
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
        let top = self.y(self.singers_formant_high, bounds.height);
        let bottom = self.y(self.singers_formant_low, bounds.height);
        frame.fill_rectangle(
            Point::new(0.0, top),
            Size::new(bounds.width, bottom - top),
            alpha(Studio::ACCENT, 0.10),
        );
        let line = Stroke::default()
            .with_color(alpha(Studio::TEXT_PRIMARY, 0.12))
            .with_width(1.0);
        let mut hz = 1000.0;
        while hz < self.max_hz {
            let y = self.y(hz, bounds.height);
            frame.stroke(
                &Path::line(Point::new(0.0, y), Point::new(bounds.width, y)),
                line,
            );
            hz += 1000.0;
        }
        vec![frame.into_geometry()]
    }
}

/// `PowerSpectrumPainter`: the instantaneous power spectrum (frequency X, dB
/// Y) of one column, with a smoothed formant envelope, the Singer's Formant
/// region shaded, and the estimated formants (`formants`, Hz) as labelled
/// markers. It auto-scales to its own column's peak so the envelope is
/// always visible; live input sets `min_ceil_db` so near-silence reads as a
/// low flat line instead of its noise floor stretched to fill the panel.
#[derive(Debug, Clone)]
pub struct PowerSpectrumPainter {
    pub column: Arc<[f32]>,
    pub bin_hz: f64,
    pub max_hz: f64,
    pub formants: Vec<f64>,
    pub dynamic_range_db: f64,
    pub min_ceil_db: Option<f64>,
}

/// The geometry [`PowerSpectrumPainter`] draws in a `size` box.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerSpectrumShape {
    /// The Singer's Formant shading: left and right x.
    pub band: (f64, f64),
    /// The filled spectrum outline, closed along the bottom.
    pub area: Vec<(f64, f64)>,
    /// The ±6-bin moving-average envelope.
    pub envelope: Vec<(f64, f64)>,
    /// Formant markers: x and which formant (0 = F1).
    pub markers: Vec<(f64, usize)>,
}

impl PowerSpectrumPainter {
    pub fn new(column: Arc<[f32]>, bin_hz: f64, max_hz: f64) -> Self {
        Self {
            column,
            bin_hz,
            max_hz,
            formants: Vec::new(),
            dynamic_range_db: 78.0,
            min_ceil_db: None,
        }
    }

    /// The geometry in a `width` × `height` box; `None` for an empty column.
    pub fn shape(&self, width: f64, height: f64) -> Option<PowerSpectrumShape> {
        let column = &self.column;
        let n = column.len();
        if n == 0 {
            return None;
        }
        let mut ceil = -1e9;
        for &v in column.iter() {
            let v = f64::from(v);
            if v > ceil {
                ceil = v;
            }
        }
        if let Some(min) = self.min_ceil_db {
            if ceil < min {
                ceil = min;
            }
        }
        let floor = ceil - self.dynamic_range_db;
        let range = if ceil - floor <= 0.0 {
            1.0
        } else {
            ceil - floor
        };
        let x = |bin: f64| (bin * self.bin_hz / self.max_hz).clamp(0.0, 1.0) * width;
        let y = |db: f64| height * (1.0 - ((db - floor) / range).clamp(0.0, 1.0));

        let band = (
            x((2600.0 / self.bin_hz).round()),
            x((3800.0 / self.bin_hz).round()),
        );
        let mut area = vec![(0.0, height)];
        for (b, &v) in column.iter().enumerate() {
            area.push((x(b as f64), y(f64::from(v))));
        }
        area.push((x((n - 1) as f64), height));

        const HALF: i64 = 6;
        let envelope = (0..n as i64)
            .map(|b| {
                let mut sum = 0.0;
                let mut count = 0;
                for k in b - HALF..=b + HALF {
                    if k >= 0 && k < n as i64 {
                        sum += f64::from(column[k as usize]);
                        count += 1;
                    }
                }
                (x(b as f64), y(sum / f64::from(count)))
            })
            .collect();

        let markers = self
            .formants
            .iter()
            .take(3)
            .enumerate()
            .filter(|(_, hz)| !hz.is_nan() && **hz > 0.0)
            .map(|(i, hz)| ((hz / self.max_hz).clamp(0.0, 1.0) * width, i))
            .collect();
        Some(PowerSpectrumShape {
            band,
            area,
            envelope,
            markers,
        })
    }
}

fn polyline(points: &[(f64, f64)], close: bool) -> Path {
    Path::new(|b| {
        let mut it = points.iter();
        if let Some(&(x, y)) = it.next() {
            b.move_to(Point::new(x as f32, y as f32));
        }
        for &(x, y) in it {
            b.line_to(Point::new(x as f32, y as f32));
        }
        if close {
            b.close();
        }
    })
}

impl<M> canvas::Program<M> for PowerSpectrumPainter {
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
        if let Some(shape) = self.shape(f64::from(bounds.width), f64::from(bounds.height)) {
            let (l, r) = shape.band;
            frame.fill_rectangle(
                Point::new(l as f32, 0.0),
                Size::new((r - l) as f32, bounds.height),
                alpha(Studio::ACCENT, 0.08),
            );
            frame.fill(&polyline(&shape.area, true), alpha(Studio::WAVEFORM, 0.35));
            frame.stroke(
                &polyline(&shape.envelope, false),
                Stroke::default()
                    .with_color(alpha(Studio::ACCENT, 0.5))
                    .with_width(1.5),
            );
            for (fx, i) in shape.markers {
                let fx = fx as f32;
                let color = FORMANT_COLORS[i];
                frame.stroke(
                    &Path::line(Point::new(fx, 0.0), Point::new(fx, bounds.height)),
                    Stroke::default()
                        .with_color(alpha(color, 0.85))
                        .with_width(1.5),
                );
                frame.fill_text(canvas::Text {
                    content: format!("F{}", i + 1),
                    position: Point::new(fx + 2.0, 2.0),
                    color,
                    size: Pixels(9.0),
                    font: font::BOLD,
                    align_y: Vertical::Top,
                    ..canvas::Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }
}

/// One overlay dot: centre, radius and colour.
pub type Dot = (Point, f32, Color);

/// The dots [`FormantTrackPainter`] draws in a `size` box: F1, F2, F3 (1.1
/// px, [`FORMANT_COLORS`] at 90 %) then F0 on top (1.4 px, [`F0_COLOR`]), one
/// per frame with a frequency in (0, `max_hz`], at the frame's window centre
/// on the time axis — the middle of the image column [`SpectrogramImage`]
/// draws for the same frame, so dots sit on the ridges AND under the playhead
/// at the moment they mark (not `(fr + 0.5) / frames`: the frame count is not
/// the box length).
pub fn formant_dots(
    metrics: &dyn FormantTracks,
    max_hz: f64,
    timeline_duration_ms: i64,
    size: Size,
) -> Vec<Dot> {
    let frames = metrics.frames();
    let dur = timeline_duration_ms;
    let mut dots = Vec::new();
    if frames == 0 || dur <= 0 {
        return dots;
    }
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    let mut track = |f: &[f32], color: Color, radius: f32| {
        for (fr, &hz) in f.iter().enumerate().take(frames) {
            let hz = f64::from(hz);
            if hz.is_nan() || hz <= 0.0 || hz > max_hz {
                continue;
            }
            let x = metrics.time_of_frame(fr as i64) * 1000.0 / dur as f64 * w;
            let y = (1.0 - hz / max_hz) * h;
            dots.push((Point::new(x as f32, y as f32), radius, alpha(color, 0.9)));
        }
    };
    track(metrics.f1(), FORMANT_COLORS[0], 1.1);
    track(metrics.f2(), FORMANT_COLORS[1], 1.1);
    track(metrics.f3(), FORMANT_COLORS[2], 1.1);
    track(metrics.f0(), F0_COLOR, 1.4); // F0 on top, slightly larger
    dots
}

/// Fills `dots` one batch per colour run (a 10-minute take is ~56k frames
/// per track; one path per track, as Dart drew one point batch per track).
fn fill_dots(frame: &mut Frame, dots: &[Dot]) {
    let mut start = 0;
    while start < dots.len() {
        let color = dots[start].2;
        let mut end = start;
        while end < dots.len() && dots[end].2 == color {
            end += 1;
        }
        let path = Path::new(|b| {
            for &(c, r, _) in &dots[start..end] {
                b.circle(c, r);
            }
        });
        frame.fill(&path, color);
        start = end;
    }
}

/// `FormantTrackPainter`: the F0 pitch track and the F1/F2/F3 formant tracks
/// over the spectrogram (see [`formant_dots`]). The box spans
/// `timeline_duration_ms` of the metrics' own time, exactly like the
/// [`SpectrogramImage`] under it.
pub struct FormantTrackPainter {
    pub metrics: Arc<dyn FormantTracks>,
    pub max_hz: f64,
    pub timeline_duration_ms: i64,
}

impl<M> canvas::Program<M> for FormantTrackPainter {
    type State = KeyedCache<(Same<dyn FormantTracks>, u64, i64)>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let key = (
            Same(Arc::clone(&self.metrics)),
            self.max_hz.to_bits(),
            self.timeline_duration_ms,
        );
        vec![state.draw(renderer, bounds.size(), key, |frame| {
            let dots = formant_dots(
                self.metrics.as_ref(),
                self.max_hz,
                self.timeline_duration_ms,
                bounds.size(),
            );
            fill_dots(frame, &dots);
        })]
    }
}

/// The dots [`LiveFormantPainter`] draws: each rolling history (one value
/// per spectrogram column) spread across the width, column c at
/// `(c + 0.5) / n`.
pub fn live_formant_dots(
    f0: &[f64],
    f1: &[f64],
    f2: &[f64],
    f3: &[f64],
    max_hz: f64,
    size: Size,
) -> Vec<Dot> {
    let mut dots = Vec::new();
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    let mut track = |f: &[f64], color: Color, radius: f32| {
        let n = f.len();
        for (c, &hz) in f.iter().enumerate() {
            if hz.is_nan() || hz <= 0.0 || hz > max_hz {
                continue;
            }
            let x = (c as f64 + 0.5) / n as f64 * w;
            let y = (1.0 - hz / max_hz) * h;
            dots.push((Point::new(x as f32, y as f32), radius, alpha(color, 0.9)));
        }
    };
    track(f1, FORMANT_COLORS[0], 1.1);
    track(f2, FORMANT_COLORS[1], 1.1);
    track(f3, FORMANT_COLORS[2], 1.1);
    track(f0, F0_COLOR, 1.4);
    dots
}

/// `LiveFormantPainter`: the live equivalent of [`FormantTrackPainter`],
/// rolling F0/F1/F2/F3 histories over the scrolling live spectrogram. Its
/// histories change every tick, so it always redraws.
#[derive(Debug, Clone)]
pub struct LiveFormantPainter {
    pub f0: Vec<f64>,
    pub f1: Vec<f64>,
    pub f2: Vec<f64>,
    pub f3: Vec<f64>,
    pub max_hz: f64,
}

impl<M> canvas::Program<M> for LiveFormantPainter {
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
        let dots = live_formant_dots(
            &self.f0,
            &self.f1,
            &self.f2,
            &self.f3,
            self.max_hz,
            bounds.size(),
        );
        fill_dots(&mut frame, &dots);
        vec![frame.into_geometry()]
    }
}

/// `VowelSpacePainter`: an F1×F2 vowel chart — reference vowels as faint
/// labels plus a coral dot at the current frame's formants. High F2 sits
/// left and high F1 sits low, the usual IPA-style layout.
#[derive(Debug, Clone, Copy)]
pub struct VowelSpacePainter {
    pub f1: f64,
    pub f2: f64,
}

const F1_LO: f64 = 200.0;
const F1_HI: f64 = 1000.0;
const F2_LO: f64 = 600.0;
const F2_HI: f64 = 2700.0;

/// Reference vowels (label, F1, F2) — neutral adult values.
pub const VOWEL_REFS: [(&str, f64, f64); 5] = [
    ("i", 300.0, 2300.0),
    ("e", 500.0, 1900.0),
    ("a", 750.0, 1150.0),
    ("o", 450.0, 850.0),
    ("u", 350.0, 750.0),
];

/// Where a vowel with formants (`f1`, `f2`) sits on a `size` chart.
pub fn vowel_position(f1: f64, f2: f64, size: Size) -> Point {
    let x = (1.0 - (f2 - F2_LO) / (F2_HI - F2_LO)).clamp(0.0, 1.0) * f64::from(size.width);
    let y = ((f1 - F1_LO) / (F1_HI - F1_LO)).clamp(0.0, 1.0) * f64::from(size.height);
    Point::new(x as f32, y as f32)
}

impl<M> canvas::Program<M> for VowelSpacePainter {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let mut frame = Frame::new(renderer, size);
        frame.stroke(
            &Path::rectangle(Point::ORIGIN, size),
            Stroke::default().with_color(Studio::BORDER).with_width(1.0),
        );
        for (label, rf1, rf2) in VOWEL_REFS {
            let p = vowel_position(rf1, rf2, size);
            frame.fill(&Path::circle(p, 2.0), alpha(Studio::TEXT_FAINT, 0.5));
            frame.fill_text(canvas::Text {
                content: label.to_string(),
                position: p + Vector::new(3.0, -4.0),
                color: Studio::TEXT_FAINT,
                size: Pixels(9.0),
                font: font::REGULAR,
                align_y: Vertical::Top,
                ..canvas::Text::default()
            });
        }
        if !self.f1.is_nan() && !self.f2.is_nan() && self.f1 > 0.0 && self.f2 > 0.0 {
            let dot = Path::circle(vowel_position(self.f1, self.f2, size), 4.0);
            frame.fill(&dot, Studio::ACCENT);
            frame.stroke(
                &dot,
                Stroke::default()
                    .with_color(Studio::BACKGROUND)
                    .with_width(1.2),
            );
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests;
