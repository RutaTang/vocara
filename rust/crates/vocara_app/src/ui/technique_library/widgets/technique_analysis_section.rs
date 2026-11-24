//! Port of `ui/technique_library/widgets/technique_analysis_section.dart`:
//! the inline analysis for ONE example (the currently-selected/expanded
//! card) — the dimension tabs and the analysis box, plus, in the read view,
//! a practice recorder and the gap comparison against that example. While a
//! recording is in flight the box shows the live mic instead. The header and
//! the selector live on the card (the examples section).
//!
//! The analysis dimensions reuse the song-practice dock views
//! (`voiceprint_view`), driven through [`TechniqueDetailViewModel`]'s
//! [`AnalysisSource`].
//!
//! # How the technique page composes it
//!
//! The Dart widget took the view model and called it from its taps. Here
//! [`view`] reads the view model and reports [`Message`]s; the page hands
//! each to [`update`], which makes the view model call the Dart tap made (a
//! tap reaches nothing where the Dart control was inert) and returns what
//! the view model asks of the page. A practice start's error comes back as
//! the view model's `Answer::PracticeStart`, which the page toasts, as
//! `_Controls._record` did.
//!
//! The Spectrogram tab's take image is the page's [`VoiceprintState`]: the
//! page syncs it after each update while the panel is on screen, and
//! [`update`] lands its renders. Dart's post-frame `ensureReferenceAnalysis`
//! is the page's too: it asks after each update while the panel is built.
//!
//! # Differences from Flutter
//!
//! The Dart level meter's fill had no height (a childless `ColoredBox` under
//! `Align` takes the smallest size), so it never showed; here it fills the
//! meter's 8 px, as intended — as in the examples section's record row.

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{
    button, canvas as canvas_widget, column, container, row, scrollable, Row, Space,
};
use iced::{mouse, Element, Length, Padding, Point, Rectangle, Renderer, Size, Theme};
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_model::util::dart_num::round_to_i64;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::studio_button::studio_button;
use crate::ui::core::widgets::studio_hint::studio_hint;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::ui::song_practice_detail::widgets::voice_profile_view;
use crate::ui::song_practice_detail::widgets::voiceprint_view::{
    self as voiceprint, VoiceprintState,
};
use crate::ui::technique_library::view_model::technique_detail_view_model::{
    Next, TechniqueAnalysisSubject, TechniqueDetailViewModel,
};
use crate::util::to_string_as_fixed;

#[cfg(test)]
mod tests;

/// The analysis dimensions shown for a technique, in the bar's order
/// (Dart's `_dimensions`).
pub const DIMENSIONS: [&str; 10] = [
    "Spectrogram",
    "Power",
    "Vowel",
    "Pitch",
    "Timbre",
    "LTAS",
    "Vibrato",
    "Register",
    "Range",
    "Dynamics",
];

/// The analysis box's height (a fixed frame for the selected view).
pub const BOX_HEIGHT: f32 = 320.0;

/// The practice recorder's idle button.
pub const RECORD_PRACTICE: &str = "Record practice";
/// The practice recorder's button once a take has an analysis.
pub const RE_RECORD: &str = "Re-record";
/// The practice recorder's button while the take records.
pub const STOP: &str = "Stop";
/// While the take's analysis runs.
pub const ANALYZING: &str = "Analyzing…";
/// While the practice take's start is pending (the permission prompt, the
/// device opening).
pub const STARTING: &str = "Starting the microphone…";
/// Discards the practice take.
pub const CLEAR: &str = "Clear";
/// The subject switch's segments.
pub const REFERENCE: &str = "Reference";
pub const YOUR_PRACTICE: &str = "Your practice";
/// The box while the example's analysis runs.
pub const ANALYZING_EXAMPLE: &str = "Analyzing the example…";
/// The box while the practice take's analysis runs.
pub const ANALYZING_TAKE: &str = "Analyzing your take…";
/// The box when the example's analysis failed with no stated cause.
pub const EXAMPLE_FAILED: &str = "Couldn't analyze this example clip.";
/// The box when the practice take's analysis failed with no stated cause.
pub const TAKE_FAILED: &str = "Couldn't analyze your recording. Try again.";
/// The practice box before any take.
pub const RECORD_YOURSELF: &str = "Record yourself to compare against the reference.";
/// The retry under a failed example analysis.
pub const TRY_AGAIN: &str = "Try again";

/// What the analysis panel's controls report.
#[derive(Debug, Clone)]
pub enum Message {
    /// The Reference / Your practice switch.
    Subject(TechniqueAnalysisSubject),
    /// "Record practice" / "Re-record".
    Record,
    /// The practice take's Stop.
    Stop,
    /// Discard the practice take.
    Clear,
    /// A dimension in the bar.
    Dimension(usize),
    /// "Try again" under a failed example analysis.
    Retry,
    /// The analysis views (a seek, the Spectrogram tab's render).
    Voiceprint(voiceprint::Message),
}

/// What the switch's Practice segment needs: a take that has (or is
/// getting, or failed to get) an analysis — a failed take too: its panel
/// says why it has no analysis.
fn practice_enabled(vm: &TechniqueDetailViewModel) -> bool {
    vm.practice_analysis().is_some()
        || vm.practice_analysis_loading()
        || vm.practice_analysis_failed()
}

/// Whether the box offers "Try again" (an example's failed analysis that
/// may yet succeed).
fn offers_retry(vm: &TechniqueDetailViewModel) -> bool {
    let practice = vm.analysis_subject() == TechniqueAnalysisSubject::Practice;
    !vm.live_monitor()
        && !vm.spectrogram_loading()
        && !practice
        && vm.reference_analysis().is_none()
        && vm.can_retry_reference_analysis()
}

/// The practice recorder's state, as `_Controls` picked its right-hand side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecorderState {
    Recording,
    Analyzing,
    Starting,
    Idle,
}

fn recorder_state(vm: &TechniqueDetailViewModel) -> RecorderState {
    if vm.is_recording_practice() {
        RecorderState::Recording
    } else if vm.practice_analysis_loading() {
        RecorderState::Analyzing
    } else if vm.starting_practice() {
        RecorderState::Starting
    } else {
        RecorderState::Idle
    }
}

/// Handles a message of the panel shown with `allow_practice` (the read
/// view's): the view model call the Dart tap made, if the control it came
/// from is live; what the view model asks of the page.
pub fn update(
    vm: &mut TechniqueDetailViewModel,
    voiceprint: &mut VoiceprintState,
    message: Message,
    allow_practice: bool,
) -> Next {
    match message {
        Message::Subject(subject) => {
            let live = allow_practice
                && (subject == TechniqueAnalysisSubject::Reference || practice_enabled(vm));
            if live {
                vm.show_analysis_subject(subject);
            }
            Next::default()
        }
        Message::Record => {
            let live = allow_practice
                && recorder_state(vm) == RecorderState::Idle
                && vm.can_record_practice();
            if live {
                vm.start_practice()
            } else {
                Next::default()
            }
        }
        Message::Stop => {
            if allow_practice && recorder_state(vm) == RecorderState::Recording {
                vm.stop_practice()
            } else {
                Next::default()
            }
        }
        Message::Clear => {
            if allow_practice
                && recorder_state(vm) == RecorderState::Idle
                && vm.practice_analysis().is_some()
            {
                vm.clear_practice();
            }
            Next::default()
        }
        Message::Dimension(index) => {
            if index < DIMENSIONS.len() {
                vm.set_analysis_dim(index);
            }
            Next::default()
        }
        Message::Retry => {
            if offers_retry(vm) {
                vm.retry_reference_analysis()
            } else {
                Next::default()
            }
        }
        Message::Voiceprint(voiceprint::Message::Seek(to)) => vm.seek(to),
        Message::Voiceprint(voiceprint::Message::ImageRendered(done)) => {
            voiceprint.rendered(done);
            Next::default()
        }
        // The Take/Live toggle is the song dock's; this panel shows none.
        Message::Voiceprint(voiceprint::Message::SelectLive(_)) => Next::default(),
    }
}

/// `TechniqueExampleAnalysis(vm: ..., allowPractice: ...)`: the controls
/// (read view only), the dimension bar, the analysis box, then — for a
/// static take — the quality warning and the gap table. `voiceprint` holds
/// the Spectrogram tab's take image.
pub fn view<'a>(
    vm: &'a TechniqueDetailViewModel,
    voiceprint: &'a VoiceprintState,
    allow_practice: bool,
) -> Element<'a, Message> {
    let live = vm.live_monitor();
    // The selected dimension lives on the view model so it survives the
    // panel moving between cards or in/out of the live (recording) slot.
    let dim = vm.analysis_dim().min(DIMENSIONS.len() - 1);
    let mut content = column![];
    if allow_practice {
        content = content.push(controls(vm)).push(Space::new().height(10));
    }
    content = content
        .push(dimension_bar(dim))
        .push(Space::new().height(8))
        .push(analysis_box(vm, || dimension(vm, voiceprint, dim)));
    // Static-take extras: hidden while the live mic feed is showing.
    if !live {
        if let Some(warning) = vm.selected_metrics().and_then(|m| quality_warning(&m)) {
            content = content.push(warning_banner(warning));
        }
    }
    if allow_practice && !live {
        if let (Some(reference), Some(practice)) = (vm.reference_analysis(), vm.practice_analysis())
        {
            content = content
                .push(Space::new().height(14))
                .push(gap_table(&gap_rows(&reference, practice)));
        }
    }
    content.width(Length::Fill).into()
}

/// The view of dimension `dim` (Dart's `_dimensions[dim].$2(vm)`).
fn dimension<'a>(
    vm: &'a TechniqueDetailViewModel,
    voiceprint: &'a VoiceprintState,
    dim: usize,
) -> Element<'a, Message> {
    let src: &'a dyn AnalysisSource = vm;
    let view = match dim {
        0 => voiceprint::spectrogram_tab(src, voiceprint),
        1 => voiceprint::power_tab(src),
        2 => voiceprint::vowel_tab(src),
        3 => voiceprint::pitch_tab(src),
        4 => voiceprint::timbre_tab(src),
        5 => voiceprint::ltas_tab(src),
        6 => voiceprint::vibrato_tab(src),
        7 => voiceprint::register_tab(src),
        8 => voice_profile_view::range_view(src),
        _ => voice_profile_view::dynamics_view(src),
    };
    view.map(Message::Voiceprint)
}

// ------------------------------------------------------------ the controls

/// `_Controls`: the Reference/Practice subject switch, then — flush right —
/// the record / stop control.
fn controls(vm: &TechniqueDetailViewModel) -> Element<'_, Message> {
    let has_practice = vm.practice_analysis().is_some();
    let mut bar = Row::new()
        .push(subject_toggle(vm.analysis_subject(), practice_enabled(vm)))
        .push(Space::new().width(Length::Fill))
        .align_y(Vertical::Center);
    bar = match recorder_state(vm) {
        RecorderState::Recording => bar
            .push(
                row![
                    icons::icon(Icon::Mic, 13.0, Studio::RECORD),
                    Space::new().width(6),
                    canvas_widget(LevelMeter {
                        level: vm.record_level() as f32,
                    })
                    .width(Length::Fill)
                    .height(METER_HEIGHT),
                ]
                .width(90)
                .align_y(Vertical::Center),
            )
            .push(Space::new().width(10))
            .push(rec_button(STOP, Icon::Square, true, Some(Message::Stop))),
        RecorderState::Analyzing => bar.push(rec_button(ANALYZING, Icon::Loader, false, None)),
        // As the example's record row: while the mic starts (the permission
        // prompt, the device opening) it said "Record practice" and ignored
        // every tap, as it did whenever a take could not start.
        RecorderState::Starting => bar.push(rec_button(STARTING, Icon::Loader, false, None)),
        RecorderState::Idle => {
            if has_practice {
                bar = bar.push(text_button(CLEAR, Message::Clear));
            }
            bar.push(Space::new().width(8)).push(rec_button(
                if has_practice {
                    RE_RECORD
                } else {
                    RECORD_PRACTICE
                },
                Icon::Mic,
                false,
                vm.can_record_practice().then_some(Message::Record),
            ))
        }
    };
    bar.width(Length::Fill).into()
}

/// `_SubjectToggle`: two segments in an inset well.
fn subject_toggle<'a>(
    subject: TechniqueAnalysisSubject,
    practice_enabled: bool,
) -> Element<'a, Message> {
    let segments = row![
        segment(
            REFERENCE,
            subject == TechniqueAnalysisSubject::Reference,
            true,
            Message::Subject(TechniqueAnalysisSubject::Reference),
        ),
        segment(
            YOUR_PRACTICE,
            subject == TechniqueAnalysisSubject::Practice,
            practice_enabled,
            Message::Subject(TechniqueAnalysisSubject::Practice),
        ),
    ];
    // 2 inside the 1 px border.
    container(segments)
        .padding(3)
        .style(|_| boxed(Some(Studio::INSET), 7.0, Some(Studio::BORDER)))
        .into()
}

fn segment<'a>(
    label: &'a str,
    selected: bool,
    enabled: bool,
    on_tap: Message,
) -> Element<'a, Message> {
    let fg = if selected {
        Studio::BACKGROUND
    } else if enabled {
        Studio::TEXT_SECONDARY
    } else {
        Studio::TEXT_FAINT
    };
    let face = container(text(label).size(12).font(font::SEMIBOLD).color(fg))
        .padding([5, 12])
        .style(move |_| boxed(selected.then_some(Studio::ACCENT), 5.0, None));
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press_maybe(enabled.then_some(on_tap))
        .into()
}

/// `_RecButton`: a filled pill (coral, or red for `danger`), dark label;
/// grey and inert with no `on_tap`.
fn rec_button<'a>(
    label: &'a str,
    icon: Icon,
    danger: bool,
    on_tap: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let fill = match (enabled, danger) {
        (false, _) => Studio::SURFACE_RAISED,
        (true, true) => Studio::RECORD,
        (true, false) => Studio::ACCENT,
    };
    let fg = if enabled {
        Studio::BACKGROUND
    } else {
        Studio::TEXT_FAINT
    };
    let face = container(
        row![
            icons::icon(icon, 14.0, fg),
            Space::new().width(6),
            text(label).size(12).font(font::BOLD).color(fg),
        ]
        .align_y(Vertical::Center),
    )
    .padding([7, 12])
    .style(move |_| boxed(Some(fill), 7.0, None));
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press_maybe(on_tap)
        .into()
}

/// `_TextButton`: a plain secondary label.
fn text_button<'a>(label: &'a str, on_tap: Message) -> Element<'a, Message> {
    button(
        text(label)
            .size(12)
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_SECONDARY),
    )
    .padding([7, 8])
    .style(bare_button)
    .on_press(on_tap)
    .into()
}

/// The level meter's height.
const METER_HEIGHT: f32 = 8.0;

/// `_Level`'s track: inset, radius 4, filled in the record red from the left
/// to the level (clamped to 0..1).
struct LevelMeter {
    level: f32,
}

/// How wide the meter's fill is for `level` in a `width`-wide track.
pub fn meter_fill(level: f32, width: f32) -> f32 {
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    level * width
}

impl canvas::Program<Message> for LevelMeter {
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
        let track = Path::rounded_rectangle(Point::ORIGIN, bounds.size(), 4.0.into());
        frame.fill(&track, Studio::INSET);
        let filled = meter_fill(self.level, bounds.width);
        if filled > 0.0 {
            // The track's rounded shape, cut at the fill (`Clip.antiAlias`).
            frame.with_clip(
                Rectangle::new(Point::ORIGIN, Size::new(filled, bounds.height)),
                |frame| frame.fill(&track, Studio::RECORD),
            );
        }
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------- the dimension bar

/// `_DimensionBar`: a 28 px horizontal strip of the dimensions, the active
/// one tinted.
fn dimension_bar<'a>(active: usize) -> Element<'a, Message> {
    let mut chips = Row::new().spacing(6).height(Length::Fill);
    for (i, label) in DIMENSIONS.iter().enumerate() {
        let on = i == active;
        let (fill, border, fg) = if on {
            (
                alpha(Studio::ACCENT, 0.18),
                alpha(Studio::ACCENT, 0.6),
                Studio::ACCENT_BRIGHT,
            )
        } else {
            (
                Studio::SURFACE_RAISED,
                Studio::BORDER,
                Studio::TEXT_SECONDARY,
            )
        };
        // 11 across inside the 1 px border.
        let face = container(text(*label).size(11).font(font::SEMIBOLD).color(fg))
            .height(Length::Fill)
            .padding([0, 12])
            .align_y(Vertical::Center)
            .style(move |_| boxed(Some(fill), 6.0, Some(border)));
        chips = chips.push(
            button(face)
                .height(Length::Fill)
                .padding(0)
                .style(bare_button)
                .on_press(Message::Dimension(i)),
        );
    }
    scrollable(chips)
        .direction(Direction::Horizontal(Scrollbar::hidden()))
        .width(Length::Fill)
        .height(28)
        .into()
}

// --------------------------------------------------------- the analysis box

/// What the analysis box shows (`_AnalysisBox`'s cases, in its order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxBody {
    /// The selected view (the live mic feed while recording, or the
    /// analysis).
    View,
    /// A spinner and this hint while the subject's analysis runs.
    Loading(&'static str),
    /// A failed example analysis that may yet succeed: its cause (or the
    /// generic copy) and "Try again".
    Retry(String),
    /// Nothing to show: this hint.
    Hint(String),
}

/// What the box shows for the view model now. Its own loading / empty
/// states, so the reused dock views never show take-specific hints.
pub fn box_body(vm: &TechniqueDetailViewModel) -> BoxBody {
    let practice = vm.analysis_subject() == TechniqueAnalysisSubject::Practice;
    let loading = vm.spectrogram_loading();
    let has_analysis = if practice {
        vm.practice_analysis().is_some()
    } else {
        vm.reference_analysis().is_some()
    };
    let failed = if practice {
        vm.practice_analysis_failed()
    } else {
        vm.reference_analysis_failed()
    };
    if vm.live_monitor() {
        // While recording, the reused dock views render the live mic feed.
        BoxBody::View
    } else if loading {
        BoxBody::Loading(if practice {
            ANALYZING_TAKE
        } else {
            ANALYZING_EXAMPLE
        })
    } else if !has_analysis && !practice && vm.can_retry_reference_analysis() {
        // A read error says what to check; a hiccup with no stated cause
        // gets the generic copy.
        BoxBody::Retry(
            vm.reference_analysis_failure_message()
                .unwrap_or_else(|| EXAMPLE_FAILED.to_string()),
        )
    } else if !has_analysis {
        BoxBody::Hint(if failed {
            // Prefer the specific cause when the analysis refused the clip
            // for a presentable reason (e.g. the length cap).
            if practice {
                vm.practice_analysis_failure_message()
                    .map_or_else(|| TAKE_FAILED.to_string(), str::to_string)
            } else {
                vm.reference_analysis_failure_message()
                    .unwrap_or_else(|| EXAMPLE_FAILED.to_string())
            }
        } else {
            RECORD_YOURSELF.to_string()
        })
    } else {
        BoxBody::View
    }
}

/// `_AnalysisBox`: a fixed-height frame for the selected analysis view
/// (built by `child` only when it shows), with its own loading / empty
/// states.
fn analysis_box<'a>(
    vm: &'a TechniqueDetailViewModel,
    child: impl FnOnce() -> Element<'a, Message>,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match box_body(vm) {
        BoxBody::View => child(),
        BoxBody::Loading(hint) => studio_hint(hint)
            .spinner(true)
            .font_size(12.5)
            .padding(20)
            .into(),
        BoxBody::Retry(cause) => container(
            column![
                text(cause)
                    .size(12.5)
                    .line_height(1.4)
                    .align_x(Horizontal::Center)
                    .color(Studio::TEXT_FAINT),
                Space::new().height(12),
                studio_button(TRY_AGAIN, Message::Retry),
            ]
            .align_x(Horizontal::Center),
        )
        .center(Length::Fill)
        .into(),
        BoxBody::Hint(hint) => studio_hint(hint).font_size(12.5).padding(20).into(),
    };
    container(body)
        .width(Length::Fill)
        .height(BOX_HEIGHT)
        .clip(true)
        .style(|_| boxed(Some(Studio::SURFACE), 8.0, Some(Studio::BORDER)))
        .into()
}

// ---------------------------------------------------------- the gap table

/// One row of the gap table: a metric, the reference's value and yours.
#[derive(Debug, Clone, PartialEq)]
pub struct GapRow {
    pub label: &'static str,
    pub reference: f64,
    pub you: f64,
    pub unit: &'static str,
    /// Within this the gap reads as close (green).
    pub tolerance: f64,
}

impl GapRow {
    fn new(label: &'static str, reference: f64, you: f64) -> Self {
        Self {
            label,
            reference,
            you,
            unit: "dB",
            tolerance: 2.0,
        }
    }

    /// Whether both values are known.
    pub fn has(&self) -> bool {
        self.reference.is_finite() && self.you.is_finite()
    }

    /// Whether the gap is within the tolerance.
    pub fn close(&self) -> bool {
        self.has() && (self.you - self.reference).abs() <= self.tolerance
    }

    /// A value with its unit (`—` when unknown).
    pub fn value_text(&self, v: f64) -> String {
        let shown = if v.is_finite() {
            to_string_as_fixed(v, 1)
        } else {
            "—".to_string()
        };
        format!("{shown} {}", self.unit)
    }

    /// The signed gap (you − reference) with its unit, or `—`.
    pub fn delta_text(&self) -> String {
        if !self.has() {
            return "—".to_string();
        }
        let delta = self.you - self.reference;
        let sign = if delta >= 0.0 { '+' } else { '−' };
        format!("{sign}{} {}", to_string_as_fixed(delta.abs(), 1), self.unit)
    }
}

/// The per-metric gaps between the reference and the practice take
/// (`_GapTable`'s rows): the timbre averages, then the singer's formant,
/// the range and the dynamics where both takes have them.
pub fn gap_rows(r: &TechniqueAnalysis, p: &TechniqueAnalysis) -> Vec<GapRow> {
    let mut rows = vec![
        GapRow::new("RING", r.metrics.ring_avg, p.metrics.ring_avg),
        GapRow::new("TILT", r.metrics.tilt_avg, p.metrics.tilt_avg),
        GapRow::new("CPP", r.metrics.cpp_avg, p.metrics.cpp_avg),
    ];
    if r.ltas.has_data() && p.ltas.has_data() {
        rows.push(GapRow {
            tolerance: 3.0,
            ..GapRow::new(
                "SINGER'S FORMANT",
                r.ltas.singers_formant_db,
                p.ltas.singers_formant_db,
            )
        });
    }
    let span = |a: &TechniqueAnalysis| match (a.profile.low_midi, a.profile.high_midi) {
        (Some(low), Some(high)) => (high - low) as f64,
        _ => f64::NAN,
    };
    if r.profile.has_range() && p.profile.has_range() {
        rows.push(GapRow {
            unit: "st",
            ..GapRow::new("RANGE", span(r), span(p))
        });
    }
    if r.profile.has_dynamics() && p.profile.has_dynamics() {
        rows.push(GapRow {
            tolerance: 4.0,
            ..GapRow::new(
                "DYNAMICS",
                r.profile.dynamic_range_db(),
                p.profile.dynamic_range_db(),
            )
        });
    }
    rows
}

/// A cell `flex` parts wide, right-aligned.
fn cell<'a>(content: Element<'a, Message>, flex: u16) -> Element<'a, Message> {
    container(content)
        .width(Length::FillPortion(flex))
        .align_x(Horizontal::Right)
        .into()
}

/// A gap table caption (`_capStyle`: 9 px w700, 0.5 letter spacing).
fn caption<'a>(label: &'a str) -> Element<'a, Message> {
    studio_text(label)
        .size(9)
        .font(font::BOLD)
        .letter_spacing(0.5)
        .color(Studio::TEXT_FAINT)
        .into()
}

/// `_GapTable`: the header (REFERENCE / YOU / GAP), then a row per metric.
fn gap_table<'a>(rows: &[GapRow]) -> Element<'a, Message> {
    let mut table = column![row![
        Space::new().width(Length::FillPortion(4)),
        cell(caption("REFERENCE"), 3),
        cell(caption("YOU"), 3),
        cell(caption("GAP"), 3),
    ]
    .padding(Padding {
        bottom: 8.0,
        ..Padding::ZERO
    })];
    for r in rows {
        let number = |s: String| -> Element<'a, Message> {
            text(s)
                .size(12)
                .font(font::BOLD)
                .color(Studio::TEXT_PRIMARY)
                .into()
        };
        let delta_color = if !r.has() {
            Studio::TEXT_FAINT
        } else if r.close() {
            Studio::GOOD
        } else {
            Studio::ACCENT_BRIGHT
        };
        table = table.push(
            row![
                container(
                    text(r.label)
                        .size(11)
                        .font(font::SEMIBOLD)
                        .color(Studio::TEXT_SECONDARY),
                )
                .width(Length::FillPortion(4)),
                cell(number(r.value_text(r.reference)), 3),
                cell(number(r.value_text(r.you)), 3),
                cell(
                    text(r.delta_text())
                        .size(12)
                        .font(font::BOLD)
                        .color(delta_color)
                        .into(),
                    3,
                ),
            ]
            .align_y(Vertical::Center)
            .padding(Padding {
                bottom: 6.0,
                ..Padding::ZERO
            }),
        );
    }
    // 14/12/14/6 inside the 1 px border.
    container(table.width(Length::Fill))
        .width(Length::Fill)
        .padding(Padding {
            top: 13.0,
            right: 15.0,
            bottom: 7.0,
            left: 15.0,
        })
        .style(|_| boxed(Some(Studio::INSET), 8.0, Some(Studio::BORDER)))
        .into()
}

// ------------------------------------------------------- the quality banner

/// `_QualityWarning`'s message: the shown clip is clipped, or too noisy to
/// trust — so a bad practice recording is flagged rather than silently
/// mis-analyzed. `None` for a clean clip.
pub fn quality_warning(metrics: &VoiceMetrics) -> Option<String> {
    if metrics.is_clipped() {
        Some(format!(
            "Clipped ({}% of samples). Lower the mic gain and re-record — the spectral \
             metrics are unreliable on a clipped take.",
            to_string_as_fixed(metrics.clipped_fraction * 100.0, 1)
        ))
    } else if metrics.is_low_snr() {
        Some(format!(
            "Low signal-to-noise ({} dB). Record closer to the mic, or somewhere quieter, \
             for a trustworthy analysis.",
            round_to_i64(metrics.snr_db)
        ))
    } else {
        None
    }
}

/// The red banner under the box.
fn warning_banner<'a>(message: String) -> Element<'a, Message> {
    let banner = container(
        row![
            icons::icon(Icon::TriangleAlert, 15.0, Studio::RECORD),
            Space::new().width(8),
            text(message)
                .size(12)
                .line_height(1.3)
                .color(Studio::TEXT_SECONDARY)
                .width(Length::Fill),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    // 12 / 9 inside the 1 px border.
    .padding([10, 13])
    .style(|_| {
        boxed(
            Some(alpha(Studio::RECORD, 0.12)),
            8.0,
            Some(alpha(Studio::RECORD, 0.4)),
        )
    });
    container(banner)
        .padding(Padding {
            top: 10.0,
            ..Padding::ZERO
        })
        .into()
}
