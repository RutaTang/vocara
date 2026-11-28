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

use crate::ui::core::text;
use iced::alignment::Vertical;
use iced::widget::canvas::{self, gradient, Frame, Geometry, LineCap, Path, Stroke, Style};
use iced::widget::text::Alignment;
use iced::widget::{canvas as canvas_widget, center, column, container, row, stack, Space};
use iced::{mouse, Color, Element, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::voice_metrics::{note_name_for_midi, VoiceMetrics, VoiceProfile};
use vocara_model::util::dart_num::{clamp_f64, round_to_i64};
use vocara_services::monitor_service::MonitorService;

use super::analysis_lanes::LoudnessLanePainter;
use super::voiceprint_view::{
    header_row, plot_well, readout, scrolls_when_short, Readout, Swatch, READOUT_GAP, TAB_PADDING,
};
use crate::ui::core::theme::{alpha, boxed, font, type_scale, Studio};
use crate::ui::core::widgets::studio_hint::studio_hint;
use crate::ui::core::widgets::studio_text::studio_text;
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
/// break (passaggio) for the selected take: their readouts, then the range
/// on a pitch axis filling the tab (scrolling on a short dock:
/// [`scrolls_when_short`]).
pub fn range_view<'a, M: 'a>(src: &dyn AnalysisSource) -> Element<'a, M> {
    profile_guard(src, RANGE_LIVE_HINT, RANGE_EMPTY_HINT, |p, _, _| {
        let (Some(low), Some(high)) = (p.low_midi, p.high_midi) else {
            return studio_hint(RANGE_EMPTY_HINT).into();
        };
        scrolls_when_short(move || range_body(&p, low, high))
    })
}

fn range_body<'a, M: 'a>(p: &Arc<VoiceProfile>, low: i64, high: i64) -> Element<'a, M> {
    let mut stats = row![readout(
        Readout::new(
            "Range",
            format!("{} – {}", note_name_for_midi(low), note_name_for_midi(high)),
        )
        .whole()
        .swatch(Swatch::Block(RANGE_BAND))
        .sub(format!("{} semitones", high - low)),
    )]
    .spacing(READOUT_GAP);
    if let (Some(lo), Some(hi)) = (p.tess_lo_midi, p.tess_hi_midi) {
        stats = stats.push(readout(
            Readout::new(
                "Tessitura",
                format!("{} – {}", note_name_for_midi(lo), note_name_for_midi(hi)),
            )
            .whole()
            .swatch(Swatch::Block(TESSITURA_BAND))
            .sub("comfortable zone"),
        ));
    }
    if let Some(passaggio) = p.passaggio_midi {
        stats = stats.push(readout(
            Readout::new("Passaggio", note_name_for_midi(passaggio))
                .whole()
                .swatch(Swatch::Line(PASSAGGIO))
                .sub(passaggio_shift(p.passaggio_jump_db)),
        ));
    }
    column![
        header_row("Sung range on a pitch axis", None, 0.0),
        stats.wrap().vertical_spacing(8),
        Space::new().height(12),
        plot_well(
            canvas_widget(RangeBarPainter { p: Arc::clone(p) })
                .width(Length::Fill)
                .height(Length::Fill)
        ),
    ]
    .padding(TAB_PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
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
        // Scrolling on a short dock, where the chart was crushed to a line.
        scrolls_when_short(move || {
            let curve = plot_well(stack![
                LoudnessLanePainter {
                    metrics: Arc::clone(&m),
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
            ]);
            column![
                header_row("Loudness over time", None, 0.0),
                dynamics_stats(p.dynamic_range_db(), p.soft_db, p.loud_db),
                Space::new().height(12),
                curve,
                Space::new().height(10),
                span_row(p.soft_db, p.loud_db),
            ]
            .padding(TAB_PADDING)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        })
    })
}

/// The dynamic span on the −60..0 dBFS scale: its label, the bar, and the
/// scale's ends under it.
fn span_row<'a, M: 'a>(soft_db: f64, loud_db: f64) -> Element<'a, M> {
    let end = |s: &'static str| {
        studio_text(s)
            .size(type_scale::AXIS)
            .line_height(Pixels(14.0))
            .tabular()
            .color(Studio::TEXT_DISABLED)
    };
    row![
        container(
            studio_text("Span")
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_SECONDARY)
        )
        .width(48),
        column![
            canvas_widget(DynBarPainter { soft_db, loud_db })
                .width(Length::Fill)
                .height(8),
            row![end("−60"), Space::new().width(Length::Fill), end("0 dBFS")],
        ]
        .spacing(2)
        .width(Length::Fill),
    ]
    .align_y(Vertical::Top)
    .into()
}

/// The dynamic range / softest / loudest readouts, rounded to whole dB.
fn dynamics_stats<'a, M: 'a>(range_db: f64, soft_db: f64, loud_db: f64) -> Element<'a, M> {
    row![
        readout(
            Readout::new("Dynamic range", format!("{} dB", round_to_i64(range_db)))
                .sub("soft → loud"),
        ),
        readout(
            Readout::new("Softest", format!("{} dBFS", round_to_i64(soft_db))).sub("5th pct"),
        ),
        readout(
            Readout::new("Loudest", format!("{} dBFS", round_to_i64(loud_db))).sub("95th pct"),
        ),
    ]
    .spacing(READOUT_GAP)
    .align_y(Vertical::Top)
    .wrap()
    .vertical_spacing(8)
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
        // Titled and marked Live as it will be, the hint in its well, as
        // the other live tabs wait (a bare hint read as another state).
        return column![
            header_row("Loudness (live)", Some(live_badge()), 0.0),
            plot_well(center(
                text(LIVE_DYNAMICS_HINT)
                    .size(type_scale::BODY)
                    .color(Studio::TEXT_FAINT),
            )),
        ]
        .padding(TAB_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    }
    scrolls_when_short(move || {
        column![
            header_row("Loudness (live)", Some(live_badge()), 0.0),
            dynamics_stats(d.range_db, d.soft_db, d.loud_db),
            Space::new().height(12),
            plot_well(
                canvas_widget(LiveLoudnessPainter { loud: loud.clone() })
                    .width(Length::Fill)
                    .height(Length::Fill),
            ),
        ]
        .padding(TAB_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

// ----------------------------------------------------------- the pieces

/// The range axis's colours: the whole sung range a neutral band, the
/// tessitura the good-range green over it, the passaggio the bright accent.
const RANGE_BAND: Color = Color::from_rgb8(0x4A, 0x4A, 0x54);
const TESSITURA_BAND: Color = Studio::GOOD;
const PASSAGGIO: Color = Studio::ACCENT_BRIGHT;

/// The "● Live" badge of a live panel's header.
fn live_badge<'a, M: 'a>() -> Element<'a, M> {
    row![
        container(Space::new().width(6).height(6)).style(|_| boxed(
            Some(Studio::RECORD),
            3.0,
            None
        )),
        studio_text("Live")
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .font(font::MEDIUM)
            .color(Studio::RECORD_TEXT),
    ]
    .spacing(6)
    .align_y(Vertical::Center)
    .into()
}

/// A label on the range axis: `s` in Inter at `size` (`font`), its top at
/// `y`, its left edge at `x` (or centred on it, or ending at it).
fn profile_text(
    s: String,
    x: f32,
    y: f32,
    color: Color,
    size: f32,
    font: iced::Font,
    align: Alignment,
) -> canvas::Text {
    canvas::Text {
        content: s,
        position: Point::new(x, y),
        color,
        size: Pixels(size),
        line_height: text::NATURAL,
        font,
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
    /// A C-octave gridline's x for each C on the axis, and the note named
    /// under it: none where a range end's label names that note already
    /// (C5 read twice).
    pub grid: Vec<(f64, Option<i64>)>,
    /// The full sung range.
    pub range: (f64, f64),
    /// The tessitura band, when known.
    pub tessitura: Option<(f64, f64)>,
    /// The passaggio tick, when one was found.
    pub passaggio: Option<f64>,
}

/// The geometry of `p`'s range bar in a `width` × `height` box, or `None`
/// without a range: an axis of whole octaves, from the C at or under two
/// semitones below the low note to the C at or over two above the high
/// one, so the range shows where it sits between at least two named Cs
/// (two semitones each side gave a narrow range an axis of one gridline);
/// every x clamped to the box (never off-panel).
pub fn range_bar(p: &VoiceProfile, width: f64, height: f64) -> Option<RangeBar> {
    let (low, high) = (p.low_midi?, p.high_midi?);
    let min_midi = (low - 2).div_euclid(12) * 12;
    let max_midi = (high + 2 + 11).div_euclid(12) * 12;
    let span = (max_midi - min_midi).clamp(1, 1 << 30) as f64;
    let x = |midi: i64| clamp_f64((midi - min_midi) as f64 / span * width, 0.0, width.max(0.0));
    let grid = (min_midi..=max_midi)
        .step_by(12)
        .map(|c| (x(c), (c != low && c != high).then_some(c)))
        .collect();
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

/// `_RangeBarPainter`: a horizontal pitch axis in the well, 20 px in from
/// its sides — the full sung range as a neutral band, the tessitura over it
/// in the good-range green, the passaggio as a labelled accent tick, with
/// C-octave gridlines (`GRID`) and their names (10 px `TEXT_DISABLED`) and
/// the range's end notes (11 px `TEXT_SECONDARY`).
struct RangeBarPainter {
    p: Arc<VoiceProfile>,
}

/// The range axis's inset from the well's sides.
const RANGE_INSET: f32 = 20.0;

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
        let (w, h) = (bounds.width - 2.0 * RANGE_INSET, bounds.height);
        let p = &self.p;
        let (Some(bar), Some(low), Some(high)) = (
            range_bar(p, f64::from(w.max(0.0)), f64::from(h)),
            p.low_midi,
            p.high_midi,
        ) else {
            return vec![frame.into_geometry()];
        };
        frame.translate(iced::Vector::new(RANGE_INSET, 0.0));
        let bar_y = (bar.bar_y as f32).round();
        let line = |frame: &mut Frame, x0: f64, y0: f32, x1: f64, y1: f32, stroke: Stroke<'_>| {
            frame.stroke(
                &Path::line(Point::new(x0 as f32, y0), Point::new(x1 as f32, y1)),
                stroke,
            );
        };
        // C-octave gridlines and their names along the foot.
        let grid = Stroke::default().with_color(Studio::GRID).with_width(1.0);
        for &(gx, name) in &bar.grid {
            let gx = (gx as f32).round() + 0.5;
            line(
                &mut frame,
                f64::from(gx),
                8.0,
                f64::from(gx),
                h - 22.0,
                grid,
            );
            if let Some(c) = name {
                frame.fill_text(profile_text(
                    note_name_for_midi(c),
                    gx,
                    h - 18.0,
                    Studio::TEXT_DISABLED,
                    type_scale::AXIS,
                    font::REGULAR,
                    Alignment::Center,
                ));
            }
        }
        // Full sung range, then the tessitura over it.
        let band = |color: Color| {
            Stroke::default()
                .with_color(color)
                .with_width(8.0)
                .with_line_cap(LineCap::Round)
        };
        line(
            &mut frame,
            bar.range.0,
            bar_y,
            bar.range.1,
            bar_y,
            band(RANGE_BAND),
        );
        if let Some((lo, hi)) = bar.tessitura {
            line(
                &mut frame,
                lo,
                bar_y,
                hi,
                bar_y,
                band(alpha(TESSITURA_BAND, 0.9)),
            );
        }
        // Passaggio marker (constrained to the range, so always on the bar).
        if let (Some(px), Some(midi)) = (bar.passaggio, p.passaggio_midi) {
            line(
                &mut frame,
                px,
                bar_y - 12.0,
                px,
                bar_y + 12.0,
                Stroke::default()
                    .with_color(PASSAGGIO)
                    .with_width(2.0)
                    .with_line_cap(LineCap::Round),
            );
            frame.fill_text(profile_text(
                note_name_for_midi(midi),
                px as f32,
                bar_y - 30.0,
                PASSAGGIO,
                type_scale::AXIS,
                font::MEDIUM,
                Alignment::Center,
            ));
        }
        // Range end labels.
        for (x, midi, align) in [
            (bar.range.0, low, Alignment::Left),
            (bar.range.1, high, Alignment::Right),
        ] {
            frame.fill_text(profile_text(
                note_name_for_midi(midi),
                x as f32,
                bar_y + 12.0,
                Studio::TEXT_SECONDARY,
                type_scale::LABEL,
                font::MEDIUM,
                align,
            ));
        }
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

/// `_DynBarPainter`: the soft → loud span on a −60..0 dBFS track: a 4 px
/// `CONTROL_ACTIVE` track, the span over it shaded from a quiet grey to
/// the light data colour across the track (louder reads lighter).
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
        let (w, mid_y) = (bounds.width, (bounds.height / 2.0).round());
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, mid_y - 2.0),
                Size::new(w, 4.0),
                iced::border::Radius::from(2.0),
            ),
            Studio::CONTROL_ACTIVE,
        );
        let x0 = dyn_bar_x(self.soft_db, f64::from(w)) as f32;
        let x1 = dyn_bar_x(self.loud_db, f64::from(w)) as f32;
        // The gradient spans the whole box, left to right.
        let shade = gradient::Linear::new(Point::new(0.0, 0.0), Point::new(w, 0.0))
            .add_stop(0.0, Color::from_rgb8(0x5A, 0x5A, 0x66))
            .add_stop(1.0, Color::from_rgb8(0xC4, 0xC4, 0xCE));
        frame.stroke(
            &Path::line(Point::new(x0, mid_y), Point::new(x1, mid_y)),
            Stroke {
                style: Style::Gradient(shade.into()),
                ..Stroke::default()
                    .with_width(4.0)
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
                    .with_color(Color::from_rgb8(0xC4, 0xC4, 0xCE))
                    .with_width(1.4),
            );
        }
        vec![frame.into_geometry()]
    }
}
