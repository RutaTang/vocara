//! Port of `ui/song_practice_detail/widgets/voice_profile_view.dart`: the
//! bottom dock's Range and Dynamics tabs — the selected take's sung range,
//! comfortable tessitura and estimated register break, and its loudness
//! contour with its dynamic span (or, with the dock on Live, the mic's
//! rolling dynamics).
//!
//! Each tab is a function of the [`AnalysisSource`] and nothing else, so the
//! song dock and the technique page show the same views; neither reports
//! anything, so each is an `Element` of whatever message the host draws it
//! in. Dart's `ListenableBuilder(vm)` / `ListenableBuilder(monitor)` become
//! a view rebuilt on every message, reading the source (and the monitor)
//! as it is then. The loudness curve is the timeline's
//! [`LoudnessLanePainter`], its drawing cached until the take changes; the
//! playhead is its own canvas on top.

use std::sync::Arc;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, gradient, Frame, Geometry, LineCap, Path, Stroke, Style};
use iced::widget::text::{Alignment, LineHeight};
use iced::widget::{canvas as canvas_widget, column, container, row, stack, text, Space};
use iced::{mouse, Color, Element, Length, Padding, Pixels, Point, Rectangle, Renderer, Theme};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::voice_metrics::{note_name_for_midi, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_num::{clamp_f64, round_to_i64};
use vocara_services::monitor_service::MonitorService;

use super::analysis_lanes::LoudnessLanePainter;
use crate::ui::core::theme::{alpha, boxed, font, Studio};
use crate::ui::core::widgets::studio_hint::studio_hint;
use crate::ui::core::widgets::studio_text::{studio_text, StudioText};
use crate::ui::core::widgets::waveform_painter::PlayheadPainter;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;

#[cfg(test)]
mod tests;

/// The hint of a profile tab with no recorded take selected.
pub const SELECT_TAKE_HINT: &str = "Select a recorded take to analyze.";

/// The hint (with a spinner) while the take's metrics compute.
pub const ANALYZING_HINT: &str = "Analyzing…";

/// The Range tab on Live.
pub const RANGE_LIVE_HINT: &str = "Range is computed from a recorded take, not the live mic.";

/// The Range tab with no range to show.
pub const RANGE_EMPTY_HINT: &str = "Sing a passage to map your range.";

/// The Dynamics tab with no dynamics to show.
pub const DYNAMICS_EMPTY_HINT: &str = "Sing a passage to see your dynamics.";

/// The Dynamics tab on Live before the mic has heard enough.
pub const LIVE_DYNAMICS_HINT: &str = "Sing into the mic to measure your live dynamics.";

/// The live hint `_profileGuard` would show the Dynamics tab, which shows the
/// live panel instead before it gets there.
const DYNAMICS_LIVE_HINT: &str = "Dynamics are computed from a recorded take, not the live mic.";

/// `_profileGuard`: the shared hint / loading guard of the take-only Range
/// and Dynamics tabs.
fn profile_guard<'a, M: 'a>(
    src: &dyn AnalysisSource,
    live_hint: &str,
    empty_hint: &str,
    build: impl FnOnce(Arc<VoiceProfile>, Arc<VoiceMetrics>, Arc<AudioTrack>) -> Element<'a, M>,
) -> Element<'a, M> {
    if src.live_monitor() {
        return studio_hint(live_hint).into();
    }
    let track = match src.selected_track() {
        Some(track) if !track.is_backing() => track,
        _ => return studio_hint(SELECT_TAKE_HINT).into(),
    };
    if src.metrics_loading() {
        return studio_hint(ANALYZING_HINT).spinner(true).into();
    }
    match (src.selected_profile(), src.selected_metrics()) {
        (Some(p), Some(m)) => build(p, m, track),
        _ => studio_hint(empty_hint).into(),
    }
}

// ------------------------------------------------------------- the tabs

/// `RangeView`: the sung range, comfortable tessitura and estimated register
/// break (passaggio) for the selected take.
pub fn range_view<'a, M: 'a>(src: &dyn AnalysisSource) -> Element<'a, M> {
    profile_guard(src, RANGE_LIVE_HINT, RANGE_EMPTY_HINT, |p, _, _| {
        let (Some(low), Some(high)) = (p.low_midi, p.high_midi) else {
            return studio_hint(RANGE_EMPTY_HINT).into();
        };
        let mut stats = row![stat(
            "RANGE",
            format!("{} – {}", note_name_for_midi(low), note_name_for_midi(high)),
            format!("{} semitones", high - low),
            Studio::TEXT_PRIMARY,
        )]
        .spacing(28);
        if let (Some(lo), Some(hi)) = (p.tess_lo_midi, p.tess_hi_midi) {
            stats = stats.push(stat(
                "TESSITURA",
                format!("{} – {}", note_name_for_midi(lo), note_name_for_midi(hi)),
                "comfortable zone".to_string(),
                Studio::TEXT_PRIMARY,
            ));
        }
        if let Some(passaggio) = p.passaggio_midi {
            stats = stats.push(stat(
                "PASSAGGIO",
                note_name_for_midi(passaggio),
                passaggio_shift(p.passaggio_jump_db).to_string(),
                Studio::RECORD,
            ));
        }
        container(
            column![
                stats
                    .wrap()
                    .vertical_spacing(12)
                    .align_x(Horizontal::Center),
                Space::new().height(18),
                cap_label("RANGE  ·  tessitura highlighted, passaggio marked"),
                Space::new().height(6),
                canvas_widget(RangeBarPainter { p })
                    .width(Length::Fill)
                    .height(Length::Fill),
            ]
            .align_x(Horizontal::Center)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .padding(ltrb(20.0, 28.0, 20.0, 18.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// How abrupt a register break of `jump_db` (the H1−H2 jump at it) reads.
pub fn passaggio_shift(jump_db: f64) -> &'static str {
    if jump_db >= 6.0 {
        "abrupt shift"
    } else {
        "gradual shift"
    }
}

/// `DynamicsView`: the take's loudness contour over time and its dynamic
/// span — or the live mic's, with the dock on Live.
pub fn dynamics_view<'a, M: 'a>(src: &dyn AnalysisSource) -> Element<'a, M> {
    if src.live_monitor() {
        return live_dynamics_panel(src.monitor());
    }
    profile_guard(src, DYNAMICS_LIVE_HINT, DYNAMICS_EMPTY_HINT, |p, m, _| {
        if m.frames == 0 || !p.has_dynamics() {
            return studio_hint(DYNAMICS_EMPTY_HINT).into();
        }
        // `m` covers the analysis SUBJECT's span (one clip of a multi-clip
        // lane when a specific clip is selected), not always the whole lane
        // — map the loudness curve and playhead over that subject axis, like
        // the other tabs (see the Pitch and Timbre tabs).
        let start_ms = src.analysis_start_ms();
        let span_ms = src.analysis_duration_ms();
        let position_ms = i64::try_from(src.position().as_millis()).unwrap_or(i64::MAX);
        let curve = container(stack![
            LoudnessLanePainter {
                metrics: m,
                timeline_duration_ms: span_ms,
                start_ms: 0,
                view_start_ms: 0,
            }
            .view(),
            canvas_widget(PlayheadPainter {
                position_ms,
                timeline_duration_ms: span_ms,
                color: Studio::PLAYHEAD,
                show_head: false,
                start_ms,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| boxed(Some(Studio::INSET), 6.0, None));
        let faint = |s: &'static str| text(s).size(9).color(Studio::TEXT_FAINT);
        container(
            column![
                dynamics_stats(p.dynamic_range_db(), p.soft_db, p.loud_db, None),
                Space::new().height(16),
                cap_label("LOUDNESS OVER TIME"),
                Space::new().height(6),
                curve,
                Space::new().height(10),
                row![
                    cap_label("SPAN"),
                    Space::new().width(12),
                    canvas_widget(DynBarPainter {
                        soft_db: p.soft_db,
                        loud_db: p.loud_db,
                    })
                    .width(Length::Fill)
                    .height(12),
                    Space::new().width(12),
                    faint("−60"),
                    Space::new().width(Length::Fill),
                    faint("0 dBFS"),
                ]
                .align_y(Vertical::Center),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .padding(ltrb(20.0, 16.0, 20.0, 18.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// The DYNAMIC RANGE / SOFTEST / LOUDEST stats, rounded to whole dB; the
/// live panel leads the first with `lead` (its red dot).
fn dynamics_stats<'a, M: 'a>(
    range_db: f64,
    soft_db: f64,
    loud_db: f64,
    lead: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let range = stat(
        "DYNAMIC RANGE",
        format!("{} dB", round_to_i64(range_db)),
        "soft → loud".to_string(),
        Studio::TEXT_PRIMARY,
    );
    let first: Element<'a, M> = match lead {
        Some(lead) => row![lead, Space::new().width(14), range]
            .align_y(Vertical::Center)
            .into(),
        None => range,
    };
    row![
        first,
        stat(
            "SOFTEST",
            format!("{} dBFS", round_to_i64(soft_db)),
            "5th pct".to_string(),
            Studio::TEXT_PRIMARY,
        ),
        stat(
            "LOUDEST",
            format!("{} dBFS", round_to_i64(loud_db)),
            "95th pct".to_string(),
            Studio::TEXT_PRIMARY,
        ),
    ]
    .spacing(28)
    .align_y(Vertical::Center)
    .wrap()
    .vertical_spacing(12)
    .into()
}

/// `_LiveDynamicsPanel`: the live (mic) dynamics — a rolling loudness curve
/// and soft / loud / range over the recent voiced window. Mirrors the take's
/// [`dynamics_view`] for Live mode.
fn live_dynamics_panel<'a, M: 'a>(monitor: &MonitorService) -> Element<'a, M> {
    let (d, loud) = {
        let m = monitor.read();
        (m.live_dynamics(), m.loudness_history().to_vec())
    };
    if d.range_db.is_nan() {
        return studio_hint(LIVE_DYNAMICS_HINT).into();
    }
    let dot = container(Space::new())
        .width(8)
        .height(8)
        .style(|_| boxed(Some(Studio::RECORD), 4.0, None));
    container(
        column![
            dynamics_stats(d.range_db, d.soft_db, d.loud_db, Some(dot.into())),
            Space::new().height(16),
            cap_label("LOUDNESS (LIVE)"),
            Space::new().height(6),
            container(
                canvas_widget(LiveLoudnessPainter { loud })
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| boxed(Some(Studio::INSET), 6.0, None)),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .padding(ltrb(20.0, 16.0, 20.0, 18.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ----------------------------------------------------------- the pieces

/// `EdgeInsets.fromLTRB`.
fn ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

/// `_CapLabel`: a 9 px w700 faint caption with 0.6 letter spacing, as
/// written (not upper-cased).
fn cap_label<'a>(label: &str) -> StudioText<'a> {
    studio_text(label.to_string())
        .size(9)
        .font(font::BOLD)
        .letter_spacing(0.6)
        .color(Studio::TEXT_FAINT)
}

/// `_Stat`: a labelled summary value (RANGE  A3 – E5  /  14 semitones).
fn stat<'a, M: 'a>(label: &str, value: String, sub: String, accent: Color) -> Element<'a, M> {
    column![
        cap_label(label),
        Space::new().height(3),
        text(value).size(17).font(font::EXTRA_BOLD).color(accent),
        text(sub).size(10).color(Studio::TEXT_FAINT),
    ]
    .into()
}

/// Inter's line box (ascent + descent ≈ 1.21 em): the height Flutter's
/// `TextPainter` gave a painted label.
const LABEL_LINE: f32 = 1.21;

/// `_profileText`: `s` in w600 at `size`, its top at `y`, its left edge at
/// `x` (or centred on it, or ending at it).
fn profile_text(
    s: String,
    x: f32,
    y: f32,
    color: Color,
    size: f32,
    align: Alignment,
) -> canvas::Text {
    canvas::Text {
        content: s,
        position: Point::new(x, y),
        color,
        size: Pixels(size),
        line_height: LineHeight::Relative(LABEL_LINE),
        font: font::SEMIBOLD,
        align_x: align,
        align_y: Vertical::Top,
        ..canvas::Text::default()
    }
}

/// What [`RangeBarPainter`] draws in a box: x positions on its pitch axis.
#[derive(Debug, Clone, PartialEq)]
pub struct RangeBar {
    /// The bar's y (half the height).
    pub bar_y: f64,
    /// A C-octave gridline's x, for each C on the axis.
    pub grid: Vec<f64>,
    /// The full sung range.
    pub range: (f64, f64),
    /// The tessitura band, when known.
    pub tessitura: Option<(f64, f64)>,
    /// The passaggio tick, when one was found.
    pub passaggio: Option<f64>,
}

/// The geometry of `p`'s range bar in a `width` × `height` box, or `None`
/// without a range: an axis from two semitones under the low note to two
/// over the high one, every x clamped to the box (never off-panel).
pub fn range_bar(p: &VoiceProfile, width: f64, height: f64) -> Option<RangeBar> {
    let (low, high) = (p.low_midi?, p.high_midi?);
    let (min_midi, max_midi) = (low - 2, high + 2);
    let span = (max_midi - min_midi).clamp(1, 1 << 30) as f64;
    let x = |midi: i64| clamp_f64((midi - min_midi) as f64 / span * width, 0.0, width.max(0.0));
    // `(minMidi / 12).ceil() * 12`.
    let mut grid = Vec::new();
    let mut mm = (min_midi as f64 / 12.0).ceil() as i64 * 12;
    while mm <= max_midi {
        grid.push(x(mm));
        mm += 12;
    }
    Some(RangeBar {
        bar_y: height * 0.5,
        grid,
        range: (x(low), x(high)),
        tessitura: match (p.tess_lo_midi, p.tess_hi_midi) {
            (Some(lo), Some(hi)) => Some((x(lo), x(hi))),
            _ => None,
        },
        passaggio: p.passaggio_midi.map(x),
    })
}

/// `_RangeBarPainter`: a horizontal pitch axis — the full sung range as a
/// bar, the tessitura brighter, the passaggio as a labelled tick, with
/// C-octave gridlines and the range's end notes.
struct RangeBarPainter {
    p: Arc<VoiceProfile>,
}

impl<M> canvas::Program<M> for RangeBarPainter {
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
        let (w, h) = (bounds.width, bounds.height);
        let p = &self.p;
        let (Some(bar), Some(low), Some(high)) = (
            range_bar(p, f64::from(w), f64::from(h)),
            p.low_midi,
            p.high_midi,
        ) else {
            return vec![frame.into_geometry()];
        };
        let bar_y = bar.bar_y as f32;
        let line = |frame: &mut Frame, x0: f64, y0: f32, x1: f64, y1: f32, stroke: Stroke<'_>| {
            frame.stroke(
                &Path::line(Point::new(x0 as f32, y0), Point::new(x1 as f32, y1)),
                stroke,
            );
        };
        // C-octave gridlines.
        let grid = Stroke::default()
            .with_color(alpha(Studio::GRID_STRONG, 0.25))
            .with_width(1.0);
        for &gx in &bar.grid {
            line(&mut frame, gx, bar_y - h * 0.32, gx, bar_y + h * 0.32, grid);
        }
        // Full sung range (faint), then the brighter tessitura band.
        let band = |color: Color| {
            Stroke::default()
                .with_color(color)
                .with_width(9.0)
                .with_line_cap(LineCap::Round)
        };
        line(
            &mut frame,
            bar.range.0,
            bar_y,
            bar.range.1,
            bar_y,
            band(alpha(Studio::ACCENT, 0.45)),
        );
        if let Some((lo, hi)) = bar.tessitura {
            line(
                &mut frame,
                lo,
                bar_y,
                hi,
                bar_y,
                band(Studio::ACCENT_BRIGHT),
            );
        }
        // Passaggio marker (constrained to the range, so always on the bar).
        if let (Some(px), Some(midi)) = (bar.passaggio, p.passaggio_midi) {
            line(
                &mut frame,
                px,
                bar_y - 13.0,
                px,
                bar_y + 13.0,
                Stroke::default().with_color(Studio::RECORD).with_width(2.0),
            );
            frame.fill_text(profile_text(
                note_name_for_midi(midi),
                px as f32,
                bar_y - 28.0,
                Studio::RECORD,
                10.0,
                Alignment::Center,
            ));
        }
        // Range end labels.
        frame.fill_text(profile_text(
            note_name_for_midi(low),
            bar.range.0 as f32,
            bar_y + 14.0,
            Studio::TEXT_SECONDARY,
            11.0,
            Alignment::Left,
        ));
        frame.fill_text(profile_text(
            note_name_for_midi(high),
            bar.range.1 as f32,
            bar_y + 14.0,
            Studio::TEXT_SECONDARY,
            11.0,
            Alignment::Right,
        ));
        vec![frame.into_geometry()]
    }
}

/// The −60..0 dBFS scale of the span bar.
const DYN_LO: f64 = -60.0;
const DYN_HI: f64 = 0.0;

/// Where `db` sits on the span bar `width` wide.
pub fn dyn_bar_x(db: f64, width: f64) -> f64 {
    clamp_f64((db - DYN_LO) / (DYN_HI - DYN_LO), 0.0, 1.0) * width
}

/// `_DynBarPainter`: the soft → loud span on a −60..0 dBFS track, shaded
/// from secondary text to the bright accent across the track.
struct DynBarPainter {
    soft_db: f64,
    loud_db: f64,
}

impl<M> canvas::Program<M> for DynBarPainter {
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
        let (w, mid_y) = (bounds.width, bounds.height / 2.0);
        frame.stroke(
            &Path::line(Point::new(0.0, mid_y), Point::new(w, mid_y)),
            Stroke::default()
                .with_color(Studio::BORDER)
                .with_width(5.0)
                .with_line_cap(LineCap::Round),
        );
        let x0 = dyn_bar_x(self.soft_db, f64::from(w)) as f32;
        let x1 = dyn_bar_x(self.loud_db, f64::from(w)) as f32;
        // The gradient spans the whole box, left to right.
        let shade = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(w, 0.0))
            .add_stop(0.0, Studio::TEXT_SECONDARY)
            .add_stop(1.0, Studio::ACCENT_BRIGHT);
        frame.stroke(
            &Path::line(Point::new(x0, mid_y), Point::new(x1, mid_y)),
            Stroke {
                style: Style::Gradient(shade.into()),
                ..Stroke::default()
                    .with_width(5.0)
                    .with_line_cap(LineCap::Round)
            },
        );
        vec![frame.into_geometry()]
    }
}

/// The runs of the live loudness contour in a `width` × `height` box: the
/// readings (dBFS, clamped to −60..0) spread left to right, broken at each
/// non-finite one. Fewer than two readings draw nothing.
pub fn live_loudness_runs(loud: &[f64], width: f64, height: f64) -> Vec<Vec<(f64, f64)>> {
    let n = loud.len();
    if n < 2 {
        return Vec::new();
    }
    let (min_db, max_db) = (-60.0, 0.0);
    let y = |db: f64| (1.0 - (clamp_f64(db, min_db, max_db) - min_db) / (max_db - min_db)) * height;
    let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut started = false;
    for (i, &v) in loud.iter().enumerate() {
        if !v.is_finite() {
            started = false;
            continue;
        }
        let point = (i as f64 / (n - 1) as f64 * width, y(v));
        match runs.last_mut() {
            Some(run) if started => run.push(point),
            _ => runs.push(vec![point]),
        }
        started = true;
    }
    runs
}

/// `_LiveLoudnessPainter`: the rolling live loudness contour (dBFS,
/// −60..0) left to right.
struct LiveLoudnessPainter {
    loud: Vec<f64>,
}

impl<M> canvas::Program<M> for LiveLoudnessPainter {
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
        let runs = live_loudness_runs(
            &self.loud,
            f64::from(bounds.width),
            f64::from(bounds.height),
        );
        if !runs.is_empty() {
            let path = Path::new(|b| {
                for run in &runs {
                    let mut points = run.iter();
                    if let Some(&(x, y)) = points.next() {
                        b.move_to(Point::new(x as f32, y as f32));
                    }
                    for &(x, y) in points {
                        b.line_to(Point::new(x as f32, y as f32));
                    }
                }
            });
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Studio::ACCENT_BRIGHT)
                    .with_width(1.4),
            );
        }
        vec![frame.into_geometry()]
    }
}
