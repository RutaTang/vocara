//! The FX panel's interactive EQ (`_EqGraph`): a live input/output spectrum
//! analyzer behind the combined response curve, plus a draggable dot per
//! band (x = freq, y = gain), and the scroll wheel widening or narrowing the
//! nearest band's Q: a scroll signal's (`onPointerSignal`, a mouse wheel's,
//! or a precise device's without gesture phases), never a trackpad's
//! two-finger swipe, which reached Flutter as a pan-zoom gesture the
//! band's drag (`kPointerDragDevices`) left to the panel's scroll, nor the
//! momentum after it (`pan_zoom`).
//!
//! The math is pure and tested: the RBJ biquads (mirroring the Rust engine,
//! so the curve matches the sound), the log-frequency and dB mappings, and
//! the analyzer's octave smoothing, tilt and ballistics ([`spectrum_tick`]).
//!
//! The analyzer's ticker: Dart ran a `Ticker` that stopped once a frame
//! changed nothing, and woke it on the playhead, the analysis caches and any
//! rebuild. Here the canvas ticks on each `RedrawRequested` and asks for the
//! next frame only while something moves; any redraw wakes it (every
//! rebuild redraws, and a playhead or analysis change is a rebuild), and the
//! first tick of a wake counts its frame time afresh, as a restarted ticker
//! did. The input column is the one the host passed in at its last view
//! (`vm.analysis.eqInputColumn` under the playhead).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use iced::advanced::text::{self as core_text, Paragraph as _};
use iced::alignment::Vertical;
use iced::border::Radius;
use iced::widget::canvas::{self, Frame, Geometry, LineJoin, Path, Stroke};
use iced::widget::text::Alignment;
use iced::{
    keyboard, mouse, window, Color, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector,
};
use vocara_model::models::audio_fx::{EqBand, EqBandKind};
use vocara_model::util::dart_num::{clamp_f64, floor_to_i64, pow_f64};

use super::Message;
use crate::ui::core::theme::{alpha, font, Studio};
use crate::ui::core::widgets::gesture::{Gesture, PanDoubleTap};
use crate::ui::core::widgets::painter::KeyedCache;
use crate::ui::core::widgets::pan_zoom::{self, Scroll};
use crate::util::to_string_as_fixed;

pub const EQ_MIN_HZ: f64 = 20.0;
pub const EQ_MAX_HZ: f64 = 18000.0;
pub const EQ_MAX_DB: f64 = 15.0;
pub const GRAPH_SR: f64 = 48000.0;

/// The graph's height: the effect module's 92 px well (`DESIGN.md`).
pub const EQ_HEIGHT: f32 = 92.0;

/// The wheel's Q debounce: a wheel edit persists this long after its last
/// tick.
pub const Q_COMMIT_DELAY: Duration = Duration::from_millis(400);

// --- RBJ EQ magnitude response (mirrors the Rust engine so the curve matches the sound) ---

/// A band's normalized biquad coefficients `(b0, b1, b2, a1, a2)`.
pub fn coeffs(band: &EqBand) -> (f64, f64, f64, f64, f64) {
    let f0 = clamp_f64(band.freq, 10.0, GRAPH_SR * 0.45);
    let q = band.q.max(0.05);
    let a = pow_f64(10.0, band.gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * f0 / GRAPH_SR;
    let cosw = w0.cos();
    let sinw = w0.sin();
    let alpha = sinw / (2.0 * q);
    let (b0, b1, b2, a0, a1, a2) = match band.kind {
        EqBandKind::Peaking => (
            1.0 + alpha * a,
            -2.0 * cosw,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cosw,
            1.0 - alpha / a,
        ),
        EqBandKind::LowShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cosw + s),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cosw),
                a * ((a + 1.0) - (a - 1.0) * cosw - s),
                (a + 1.0) + (a - 1.0) * cosw + s,
                -2.0 * ((a - 1.0) + (a + 1.0) * cosw),
                (a + 1.0) + (a - 1.0) * cosw - s,
            )
        }
        EqBandKind::HighShelf => {
            let s = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cosw + s),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cosw),
                a * ((a + 1.0) + (a - 1.0) * cosw - s),
                (a + 1.0) - (a - 1.0) * cosw + s,
                2.0 * ((a - 1.0) - (a + 1.0) * cosw),
                (a + 1.0) - (a - 1.0) * cosw - s,
            )
        }
    };
    (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)
}

/// One band's response (dB) at `f` Hz; 0 when it is off or flat.
pub fn band_db(band: &EqBand, f: f64) -> f64 {
    if !band.enabled || band.gain_db == 0.0 {
        return 0.0;
    }
    let (b0, b1, b2, a1, a2) = coeffs(band);
    let w = 2.0 * std::f64::consts::PI * f / GRAPH_SR;
    let (cosw, cos2w) = (w.cos(), (2.0 * w).cos());
    let (sinw, sin2w) = (w.sin(), (2.0 * w).sin());
    let num_r = b0 + b1 * cosw + b2 * cos2w;
    let num_i = -(b1 * sinw + b2 * sin2w);
    let den_r = 1.0 + a1 * cosw + a2 * cos2w;
    let den_i = -(a1 * sinw + a2 * sin2w);
    let num = (num_r * num_r + num_i * num_i).sqrt();
    let den = (den_r * den_r + den_i * den_i).sqrt();
    if den == 0.0 {
        return 0.0;
    }
    20.0 * ((num / den).ln() / std::f64::consts::LN_10)
}

/// The whole EQ's response (dB) at `f` Hz; 0 when the EQ is off.
pub fn total_db(bands: &[EqBand], on: bool, f: f64) -> f64 {
    if !on {
        return 0.0;
    }
    bands.iter().map(|b| band_db(b, f)).sum()
}

pub fn freq_to_x(f: f64, w: f64) -> f64 {
    ((f / EQ_MIN_HZ).ln() / (EQ_MAX_HZ / EQ_MIN_HZ).ln()) * w
}

pub fn x_to_freq(x: f64, w: f64) -> f64 {
    EQ_MIN_HZ * pow_f64(EQ_MAX_HZ / EQ_MIN_HZ, clamp_f64(x / w, 0.0, 1.0))
}

pub fn db_to_y(db: f64, h: f64) -> f64 {
    h / 2.0 - (db / EQ_MAX_DB) * (h / 2.0)
}

pub fn y_to_db(y: f64, h: f64) -> f64 {
    clamp_f64((h / 2.0 - y) / (h / 2.0) * EQ_MAX_DB, -EQ_MAX_DB, EQ_MAX_DB)
}

/// The band whose handle column is nearest `x` (the first of a tie).
pub fn nearest_band(bands: &[EqBand], x: f64, w: f64) -> usize {
    let mut best = 0;
    let mut best_dx = f64::INFINITY;
    for (i, b) in bands.iter().enumerate() {
        let dx = (freq_to_x(b.freq, w) - x).abs();
        if dx < best_dx {
            best_dx = dx;
            best = i;
        }
    }
    best
}

/// A band's Q after a wheel tick of `scroll_dy` (Flutter's
/// `scrollDelta.dy`, positive scrolling down): scrolling up raises the Q
/// (narrows the band), down lowers it, within 0.3..14.
pub fn q_after_wheel(q: f64, scroll_dy: f64) -> f64 {
    clamp_f64(q * (-scroll_dy * 0.0022).exp(), 0.3, 14.0)
}

/// A band moved to `(x, y)` of a graph `w` wide (and [`EQ_HEIGHT`] tall):
/// the frequency under x (kept inside the engine's range) and the gain at
/// y, and on.
pub fn band_dragged_to(band: &EqBand, x: f64, y: f64, w: f64) -> EqBand {
    EqBand {
        freq: clamp_f64(x_to_freq(x, w), EQ_MIN_HZ, GRAPH_SR * 0.45),
        gain_db: y_to_db(y, f64::from(EQ_HEIGHT)),
        enabled: true,
        ..*band
    }
}

/// `_fmtHz`: "120 Hz", "2.5 kHz".
pub fn fmt_hz(f: f64) -> String {
    if f >= 1000.0 {
        format!("{} kHz", to_string_as_fixed(f / 1000.0, 1))
    } else {
        format!("{} Hz", f.round() as i64)
    }
}

/// `_ReadoutPill`'s text: the dragged band's "freq  ·  ±gain".
pub fn readout_pill_text(band: &EqBand) -> String {
    let g = band.gain_db;
    let sign = if g >= 0.0 { '+' } else { '−' };
    format!(
        "{}  ·  {sign}{} dB",
        fmt_hz(band.freq),
        to_string_as_fixed(g.abs(), 1)
    )
}

// --- Spectrum analyzer (FabFilter-style input/output overlay behind the curve) ---

/// Render resolution of the spectrum across the width — log-spaced points,
/// each covering a constant fraction of an octave (so averaging bins per
/// point gives free octave smoothing).
pub const SPECTRUM_POINTS: usize = 168;

/// Lift highs so pink-ish reads flat.
const TILT_DB_PER_OCT: f64 = 4.5;
const FLOOR_DB: f64 = -90.0;
/// Spectrum fall speed.
const RELEASE_DB_PER_SEC: f64 = 34.0;
const CEIL_FALL_PER_SEC: f64 = 5.0;

/// Centre frequency of spectrum render point `i`.
pub fn spectrum_point_freq(i: usize) -> f64 {
    EQ_MIN_HZ
        * pow_f64(
            EQ_MAX_HZ / EQ_MIN_HZ,
            (i as f64 + 0.5) / SPECTRUM_POINTS as f64,
        )
}

/// The EQ's response (dB) at each spectrum render point: summed once per
/// set of bands rather than per frame (Dart's `_responseOf` memo).
pub fn response_of(bands: &[EqBand]) -> Vec<f64> {
    (0..SPECTRUM_POINTS)
        .map(|i| total_db(bands, true, spectrum_point_freq(i)))
        .collect()
}

/// The pre-EQ input spectrum column under the playhead: dB per bin, and Hz
/// per bin (`({Float32List col, double binHz})`).
#[derive(Debug, Clone, Copy)]
pub struct EqInput<'a> {
    pub col: &'a [f32],
    pub bin_hz: f64,
}

/// The analyzer's per-frame state (`_SpectrumData`).
#[derive(Debug, Clone)]
pub struct SpectrumData {
    /// Smoothed input level (dB, display-tilted) per render point.
    pub pre: [f32; SPECTRUM_POINTS],
    /// Adaptive top-of-scale reference (dB) — rises fast to peaks, falls
    /// slowly, so the display fills the box regardless of the take's
    /// absolute level.
    pub ceil: f64,
    /// True once any real input has been seen, so an idle EQ stays blank.
    pub has_signal: bool,
}

impl Default for SpectrumData {
    fn default() -> Self {
        Self {
            pre: [-200.0; SPECTRUM_POINTS],
            ceil: -30.0,
            has_signal: false,
        }
    }
}

/// The input's level (dB, tilted, floored) at each render point: the
/// average power across the bins spanning the point's octave slice.
fn spectrum_target(input: Option<EqInput<'_>>) -> [f32; SPECTRUM_POINTS] {
    let mut target = [FLOOR_DB as f32; SPECTRUM_POINTS];
    let Some(input) = input else {
        return target;
    };
    let col = input.col;
    let bins = col.len() as i64;
    let ratio = EQ_MAX_HZ / EQ_MIN_HZ;
    for (i, slot) in target.iter_mut().enumerate() {
        let f_lo = EQ_MIN_HZ * pow_f64(ratio, i as f64 / SPECTRUM_POINTS as f64);
        let f_hi = EQ_MIN_HZ * pow_f64(ratio, (i + 1) as f64 / SPECTRUM_POINTS as f64);
        let mut b_lo = floor_to_i64(f_lo / input.bin_hz);
        let mut b_hi = (f_hi / input.bin_hz).ceil() as i64;
        if b_lo < 0 {
            b_lo = 0;
        }
        if b_hi <= b_lo {
            b_hi = b_lo + 1;
        }
        if b_hi > bins {
            b_hi = bins;
        }
        if b_lo >= bins {
            *slot = FLOOR_DB as f32;
            continue;
        }
        let mut power = 0.0;
        let mut n = 0;
        for b in b_lo..b_hi {
            power += pow_f64(10.0, f64::from(col[b as usize]) / 10.0);
            n += 1;
        }
        let mut db = if n == 0 {
            FLOOR_DB
        } else {
            10.0 * (power / f64::from(n)).ln() / std::f64::consts::LN_10
        };
        db += TILT_DB_PER_OCT * ((spectrum_point_freq(i) / 1000.0).ln() / std::f64::consts::LN_2);
        *slot = (if db < FLOOR_DB { FLOOR_DB } else { db }) as f32;
    }
    target
}

/// One analyzer frame `dt` seconds after the last: samples `input`,
/// octave-smooths and tilts it, applies the ballistics (instant attack,
/// linear release) and adapts the top of the scale. Whether anything moved:
/// a frame that moved nothing (with time passing) is the settled one, after
/// which the ticker stops.
pub fn spectrum_tick(data: &mut SpectrumData, input: Option<EqInput<'_>>, dt: f64) -> bool {
    let mut changed = false;
    if input.is_some() {
        if !data.has_signal {
            changed = true;
        }
        data.has_signal = true;
    }
    let target = spectrum_target(input);

    // Ballistics: instant attack, linear release; track the running peak.
    let fall = RELEASE_DB_PER_SEC * dt;
    let mut max_db = FLOOR_DB;
    for (pre, &tv) in data.pre.iter_mut().zip(&target) {
        let tv = f64::from(tv);
        let was = f64::from(*pre);
        *pre = (if tv >= was { tv } else { tv.max(was - fall) }) as f32;
        if f64::from(*pre) != was {
            changed = true;
        }
        if f64::from(*pre) > max_db {
            max_db = f64::from(*pre);
        }
    }
    // Adapt the top-of-scale only while there's real input; otherwise hold it
    // so the decaying spectrum falls to the bottom (not into the middle of
    // the box).
    if input.is_some() {
        let desired = max_db + 6.0;
        let was = data.ceil;
        data.ceil = if desired > was {
            desired
        } else {
            desired.max(was - CEIL_FALL_PER_SEC * dt)
        };
        if data.ceil != was {
            changed = true;
        }
    }
    changed
}

/// The analyzer's ticker: running or stopped, and its frame clock (the
/// elapsed time since it started, as a Flutter `Ticker` reports it).
#[derive(Debug, Default, Clone, Copy)]
pub struct SpectrumTicker {
    running: Option<(Instant, Duration)>,
}

impl SpectrumTicker {
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// A frame at `now`: wakes a stopped ticker (its clock starting over),
    /// then the frame's `dt` (0.016 on a ticker's first frames, else the
    /// time since the last, up to 0.1 s).
    pub fn frame(&mut self, now: Instant) -> f64 {
        let (start, last) = *self.running.get_or_insert((now, Duration::ZERO));
        let elapsed = now.saturating_duration_since(start);
        let dt = if last == Duration::ZERO {
            0.016
        } else {
            elapsed.saturating_sub(last).as_secs_f64().clamp(0.0, 0.1)
        };
        self.running = Some((start, elapsed));
        dt
    }

    pub fn stop(&mut self) {
        self.running = None;
    }
}

// ------------------------------------------------------------------ graph

/// `_EqGraph`: see the module docs.
pub struct EqGraph<'a> {
    pub bands: &'a [EqBand],
    pub on: bool,
    pub input: Option<EqInput<'a>>,
    pub on_changed: Box<dyn Fn(usize, EqBand) -> Message + 'a>,
    pub on_commit: Message,
}

/// What the graph keeps across frames.
#[derive(Default)]
pub struct EqState {
    gesture: PanDoubleTap,
    /// The band a pan drags (survives the rebuilds each live edit causes).
    dragging: Option<usize>,
    /// When the last wheel Q edit is due to persist.
    q_commit_at: Option<Instant>,
    /// Shift is held (macOS turns the wheel sideways under it).
    shift: bool,
    spectrum: SpectrumData,
    ticker: SpectrumTicker,
    /// The response at the render points, for the bands it was summed for.
    response: RefCell<Option<(Vec<EqBand>, Vec<f64>)>>,
    grid: KeyedCache<()>,
    curve: KeyedCache<(Vec<EqBand>, bool)>,
}

impl EqState {
    #[cfg(test)]
    pub fn spectrum(&self) -> &SpectrumData {
        &self.spectrum
    }

    #[cfg(test)]
    pub fn ticking(&self) -> bool {
        self.ticker.running()
    }
}

/// The cursor's position, even over a layer above (a held drag goes on
/// under a tooltip).
fn pointer(cursor: mouse::Cursor) -> Option<Point> {
    match cursor {
        mouse::Cursor::Available(p) | mouse::Cursor::Levitating(p) => Some(p),
        mouse::Cursor::Unavailable => None,
    }
}

impl canvas::Program<Message> for EqGraph<'_> {
    type State = EqState;

    fn update(
        &self,
        state: &mut EqState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let w = f64::from(bounds.width);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;
                state.gesture.press(p, Instant::now());
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = pointer(cursor)?;
                let local = p - bounds.position();
                match state.gesture.moved(p)? {
                    Gesture::PanStart => {
                        state.dragging = Some(nearest_band(self.bands, f64::from(local.x), w));
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    Gesture::PanUpdate(_) => {
                        let i = state.dragging?;
                        let band = self.bands.get(i)?;
                        let moved =
                            band_dragged_to(band, f64::from(local.x), f64::from(local.y), w);
                        Some(canvas::Action::publish((self.on_changed)(i, moved)).and_capture())
                    }
                    _ => None,
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                match state.gesture.release(Instant::now())? {
                    Gesture::PanEnd => {
                        state.dragging = None;
                        Some(canvas::Action::publish(self.on_commit.clone()).and_capture())
                    }
                    _ => None,
                }
            }
            canvas::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.shift = modifiers.shift();
                None
            }
            // Scroll over a band to widen / narrow its Q (bandwidth); the
            // scroll is consumed here and doesn't also scroll the FX list.
            // A scroll signal only (`scrollDelta.dy`, Shift's swap on macOS
            // put back): a trackpad's two-finger swipe scrolls the panel,
            // as it reached Flutter as a pan-zoom gesture, never as a
            // pointer scroll, and its momentum reached nothing.
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if pan_zoom::current() != Scroll::Signal {
                    return None;
                }
                let p = cursor.position_in(bounds)?;
                if self.bands.is_empty() {
                    return None;
                }
                let i = nearest_band(self.bands, f64::from(p.x), w);
                let b = self.bands[i];
                let scroll_dy = f64::from(pan_zoom::scroll_delta(*delta, state.shift).y);
                let next = EqBand {
                    q: q_after_wheel(b.q, scroll_dy),
                    enabled: true,
                    ..b
                };
                state.q_commit_at = Some(Instant::now() + Q_COMMIT_DELAY);
                Some(canvas::Action::publish((self.on_changed)(i, next)).and_capture())
            }
            canvas::Event::Window(window::Event::RedrawRequested(now)) => {
                let now = *now;
                // The analyzer's frame.
                let dt = state.ticker.frame(now);
                let changed = spectrum_tick(&mut state.spectrum, self.input, dt);
                let ticking = if !changed && dt > 0.0 {
                    state.ticker.stop();
                    false
                } else {
                    true
                };
                // The wheel's debounced commit.
                if let Some(at) = state.q_commit_at {
                    if now >= at {
                        state.q_commit_at = None;
                        return Some(canvas::Action::publish(self.on_commit.clone()));
                    }
                    if !ticking {
                        return Some(canvas::Action::request_redraw_at(at));
                    }
                }
                ticking.then(canvas::Action::request_redraw)
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &EqState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let grid = state
            .grid
            .draw(renderer, size, (), |frame| draw_grid(frame, size));
        let mut spectrum = Frame::new(renderer, size);
        if state.spectrum.has_signal {
            let mut memo = state.response.borrow_mut();
            let response = if self.on {
                if memo
                    .as_ref()
                    .is_none_or(|(b, _)| b.as_slice() != self.bands)
                {
                    *memo = Some((self.bands.to_vec(), response_of(self.bands)));
                }
                memo.as_ref().map(|(_, r)| r.as_slice())
            } else {
                None
            };
            draw_spectrum(&mut spectrum, size, &state.spectrum, response);
        }
        let curve = state
            .curve
            .draw(renderer, size, (self.bands.to_vec(), self.on), |frame| {
                draw_curve(frame, size, self.bands, self.on);
            });
        let mut layers = vec![grid, spectrum.into_geometry(), curve];
        // Live freq/gain readout for the band being dragged.
        if let Some(band) = state.dragging.and_then(|i| self.bands.get(i)) {
            let mut pill = Frame::new(renderer, size);
            draw_readout_pill(&mut pill, &readout_pill_text(band));
            layers.push(pill.into_geometry());
        }
        layers
    }
}

/// The decade lines of the graph and their labels (the 20 Hz label sits at
/// the left edge with no line).
const DECADES: [(f64, &str); 3] = [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")];

/// The well's inset outline (`#222228`).
const WELL_OUTLINE: Color = Color::from_rgb8(0x22, 0x22, 0x28);
/// The decade lines (`#1C1C22`).
const DECADE_LINE: Color = Color::from_rgb8(0x1C, 0x1C, 0x22);
/// The 0 dB line (`#24242B`).
const ZERO_LINE: Color = Color::from_rgb8(0x24, 0x24, 0x2B);
/// The response curve (`#E3A094`) and its fill (the coral track colour at
/// 10 %).
const CURVE: Color = Color::from_rgb8(0xE3, 0xA0, 0x94);
const CURVE_FILL: Color = Color {
    a: 0.10,
    ..Studio::TRACK_CORAL
};
/// The axis labels' size.
const AXIS_LABEL: f32 = 9.0;
/// The axis labels' inset from the well's sides and its bottom.
const AXIS_INSET: f32 = 6.0;

/// The well (`DESIGN.md`, Effect module: `#0F0F12`, radius 6, an inset
/// outline) and its grid (behind the spectrum): the decade lines and the
/// axis labels at the bottom ("20" at the left edge, "100" and "1k" on
/// their lines, "10k" against the right edge).
fn draw_grid(frame: &mut Frame, size: Size) {
    let (w, h) = (f64::from(size.width), size.height);
    frame.fill(
        &Path::rounded_rectangle(Point::ORIGIN, size, Radius::from(6.0)),
        Studio::WELL,
    );
    frame.stroke(
        &Path::rounded_rectangle(
            Point::new(0.5, 0.5),
            Size::new(size.width - 1.0, size.height - 1.0),
            Radius::from(5.5),
        ),
        Stroke::default().with_color(WELL_OUTLINE).with_width(1.0),
    );
    let line = Stroke::default().with_color(DECADE_LINE).with_width(1.0);
    let label = |content: &str, x: f32, align_x: Alignment| canvas::Text {
        content: content.to_string(),
        position: Point::new(x, h - 3.0),
        color: Studio::TEXT_DISABLED,
        size: Pixels(AXIS_LABEL),
        line_height: Pixels(12.0).into(),
        font: font::REGULAR,
        align_x,
        align_y: Vertical::Bottom,
        ..canvas::Text::default()
    };
    frame.fill_text(label("20", AXIS_INSET, Alignment::Left));
    for (f, text) in DECADES {
        let x = (freq_to_x(f, w) as f32).round() + 0.5;
        frame.stroke(
            &Path::line(Point::new(x, 1.0), Point::new(x, h - 1.0)),
            line,
        );
        frame.fill_text(if f >= 10000.0 {
            label(text, size.width - AXIS_INSET, Alignment::Right)
        } else {
            label(text, x, Alignment::Center)
        });
    }
}

/// The input (pre) and output (post = input + response) spectra, drawn
/// faintly behind the curve.
fn draw_spectrum(frame: &mut Frame, size: Size, data: &SpectrumData, response: Option<&[f64]>) {
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    // The ceiling maps near the top of the box; 80 dB shown vertically.
    const TOP: f64 = 0.10;
    const RANGE: f64 = 80.0;
    let y_of = |db: f64| {
        let t = clamp_f64((db - (data.ceil - RANGE)) / RANGE, 0.0, 1.0);
        (h - t * (h * (1.0 - TOP))) as f32
    };
    let mut pre = Vec::with_capacity(SPECTRUM_POINTS);
    let mut post = Vec::with_capacity(SPECTRUM_POINTS);
    for i in 0..SPECTRUM_POINTS {
        let x = freq_to_x(spectrum_point_freq(i), w) as f32;
        let pre_db = f64::from(data.pre[i]);
        pre.push(Point::new(x, y_of(pre_db)));
        // Output = input × EQ, i.e. input dB + response dB.
        let r = response.and_then(|r| r.get(i)).copied().unwrap_or(0.0);
        post.push(Point::new(x, y_of(pre_db + r)));
    }
    let bottom = h as f32;
    let fill_under = |frame: &mut Frame, pts: &[Point], color: Color| {
        let path = Path::new(|b| {
            b.move_to(pts[0]);
            for p in &pts[1..] {
                b.line_to(*p);
            }
            b.line_to(Point::new(pts[pts.len() - 1].x, bottom));
            b.line_to(Point::new(pts[0].x, bottom));
            b.close();
        });
        frame.fill(&path, color);
    };
    let stroke = |frame: &mut Frame, pts: &[Point], color: Color, width: f32| {
        frame.stroke(
            &polyline(pts),
            Stroke::default()
                .with_color(color)
                .with_width(width)
                .with_line_join(LineJoin::Round),
        );
    };
    // Inside the well's outline.
    frame.with_clip(
        Rectangle::new(
            Point::new(1.0, 1.0),
            Size::new(size.width - 2.0, size.height - 2.0),
        ),
        |frame| {
            // Input (pre): the faintest; output (post): a touch brighter.
            fill_under(frame, &pre, alpha(Color::WHITE, 0.02));
            stroke(frame, &pre, alpha(Color::WHITE, 0.07), 1.0);
            fill_under(frame, &post, alpha(Color::WHITE, 0.035));
            stroke(frame, &post, alpha(Color::WHITE, 0.16), 1.0);
        },
    );
}

fn polyline(pts: &[Point]) -> Path {
    Path::new(|b| {
        if let Some((first, rest)) = pts.split_first() {
            b.move_to(*first);
            for p in rest {
                b.line_to(*p);
            }
        }
    })
}

/// The response curve's points: every 2 px across.
pub fn curve_points(bands: &[EqBand], on: bool, size: Size) -> Vec<Point> {
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    let mut pts = Vec::new();
    let mut px = 0;
    while f64::from(px) <= w {
        let x = f64::from(px);
        let f = x_to_freq(x, w);
        pts.push(Point::new(
            x as f32,
            db_to_y(total_db(bands, on, f), h) as f32,
        ));
        px += 2;
    }
    pts
}

/// The 0 dB line, the response curve (1.5 px `#E3A094` over its 10 % fill
/// to the 0 dB line) and the band nodes (7 px `TEXT_PRIMARY` discs with a
/// dark ring). (The well, its grid and the spectrum are drawn behind this.)
fn draw_curve(frame: &mut Frame, size: Size, bands: &[EqBand], on: bool) {
    let (w, h) = (size.width, size.height);
    let cy = (h / 2.0).round() + 0.5;
    frame.stroke(
        &Path::line(Point::new(1.0, cy), Point::new(w - 1.0, cy)),
        Stroke::default().with_color(ZERO_LINE).with_width(1.0),
    );
    let pts = curve_points(bands, on, size);
    if pts.is_empty() {
        return;
    }
    if on {
        let fill = Path::new(|b| {
            b.move_to(pts[0]);
            for p in &pts[1..] {
                b.line_to(*p);
            }
            b.line_to(Point::new(w, h / 2.0));
            b.line_to(Point::new(0.0, h / 2.0));
            b.close();
        });
        frame.fill(&fill, CURVE_FILL);
    }
    frame.stroke(
        &polyline(&pts),
        Stroke::default()
            .with_color(if on {
                CURVE
            } else {
                alpha(Studio::TEXT_FAINT, 0.5)
            })
            .with_width(1.5)
            .with_line_join(LineJoin::Round),
    );
    // Band nodes.
    for b in bands {
        let x = freq_to_x(clamp_f64(b.freq, EQ_MIN_HZ, EQ_MAX_HZ), f64::from(w)) as f32;
        let y = db_to_y(b.gain_db, f64::from(h)) as f32;
        let dot = Path::circle(Point::new(x, y), 3.5);
        frame.fill(
            &dot,
            if on {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_FAINT
            },
        );
        frame.stroke(
            &dot,
            Stroke::default().with_color(Studio::WELL).with_width(1.5),
        );
    }
}

/// `_ReadoutPill` at (8, 6): the dragged band's reading on a tooltip's
/// look (`SURFACE`, radius 5, an inset `BORDER_STRONG` outline), 10 px
/// medium tabular `TEXT_PRIMARY`.
fn draw_readout_pill(frame: &mut Frame, label: &str) {
    let line_height = core_text::LineHeight::Absolute(Pixels(14.0));
    let measured = <Renderer as core_text::Renderer>::Paragraph::with_text(core_text::Text {
        content: label,
        bounds: Size::INFINITE,
        size: Pixels(10.0),
        line_height,
        font: font::MEDIUM,
        align_x: core_text::Alignment::Left,
        align_y: Vertical::Top,
        shaping: core_text::Shaping::Advanced,
        wrapping: core_text::Wrapping::None,
    })
    .min_bounds();
    let origin = Point::new(8.0, 6.0);
    let pill = Size::new(measured.width + 12.0, measured.height + 4.0);
    let shape = Path::rounded_rectangle(origin, pill, Radius::from(5.0));
    frame.fill(&shape, Studio::SURFACE);
    frame.stroke(
        &Path::rounded_rectangle(
            origin + Vector::new(0.5, 0.5),
            Size::new(pill.width - 1.0, pill.height - 1.0),
            Radius::from(4.5),
        ),
        Stroke::default()
            .with_color(Studio::BORDER_STRONG)
            .with_width(1.0),
    );
    frame.fill_text(canvas::Text {
        content: label.to_string(),
        position: Point::new(origin.x + 6.0, origin.y + 2.0),
        color: Studio::TEXT_PRIMARY,
        size: Pixels(10.0),
        line_height,
        font: font::MEDIUM,
        ..canvas::Text::default()
    });
}
