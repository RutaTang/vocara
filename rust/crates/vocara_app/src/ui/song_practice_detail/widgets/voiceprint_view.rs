//! Port of `ui/song_practice_detail/widgets/voiceprint_view.dart`: the
//! bottom dock's voiceprint tabs — Spectrogram, Power, Vowel, Pitch, Timbre,
//! LTAS, Vibrato and Register — each showing the selected take (or the
//! technique screen's clip) or, with the Take/Live toggle on Live, the mic.
//!
//! Every tab reads an [`AnalysisSource`] and nothing else, so the song dock
//! and the technique page show the same views. A tab is a function from the
//! source to an [`Element`] emitting [`Message`]s; the host maps them: a
//! [`Message::Seek`] goes to its playback, a [`Message::SelectLive`] to its
//! live-monitor switch, and a [`Message::ImageRendered`] back to its
//! [`VoiceprintState`].
//!
//! How the Dart structure maps:
//!
//! - The Spectrogram tab's take image: Dart's `SpectrogramImage` widget
//!   rendered and held it in its own state. iced views hold none, so the host
//!   keeps a [`VoiceprintState`] per place it shows the tab, calls
//!   [`VoiceprintState::sync`] after each update while the tab is on screen
//!   and runs the render it returns off the UI thread.
//! - `ValueListenableBuilder<Duration>(vm.position)` /
//!   `ListenableBuilder(monitor)`: an iced view is rebuilt from state on
//!   every message, so each readout reads [`AnalysisSource::position`] or the
//!   monitor when it is built.
//! - `RepaintBoundary` around the still layers (the image, the grid, the
//!   formant overlay, the timbre curves, the register map): each is a canvas
//!   whose cache is keyed by what it shows — the analyses by `Arc` identity,
//!   the spans and ranges by value — never by the playhead, which is its own
//!   canvas on top. A playhead tick or any other notification of the screen
//!   redraws the playhead alone.
//! - The seek `GestureDetector(onTapDown, onHorizontalDragUpdate)`: a canvas
//!   over the take spectrogram that seeks on the press and on every
//!   horizontal move of a drag.
//!
//! Where iced has no widget of Flutter's:
//!
//! - The live spectrogram's `ClipRRect(borderRadius: 6)`: iced clips to
//!   rectangles, so the panel draws its four corners outside the rounded
//!   rectangle over everything in it, in the color both its hosts lay
//!   behind it (the dock's and the technique page's analysis box,
//!   `Studio.surface`, opaque): what the clip let show of the backdrop
//!   there ([`RoundedCorners`]), the corners' antialiasing the clip's
//!   complement.
//! - The `Tooltip`s are `material_tooltip`'s, fading as Material's did,
//!   below their chip or above it when only there they fit
//!   (`positionDependentBox`), kept 10 px inside the window.

use std::f32::consts::FRAC_PI_2;
use std::f64::consts::LN_2;
use std::sync::Arc;
use std::time::Duration;

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::canvas::{self, gradient, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::text::Alignment;
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, responsive, row, stack, Row, Space,
};
use iced::{
    mouse, Background, Border, Color, Element, Length, Padding, Pixels, Point, Rectangle, Renderer,
    Shadow, Size, Task, Theme, Vector,
};
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::spectrogram::{AnalysisFraming, Spectrogram};
use vocara_model::models::voice_metrics::{
    analyze_vibrato_live, note_label, note_of, register_score, Ltas, RegisterMap, RegisterOptions,
    Vibrato, VibratoOptions, VoiceMetrics, LIVE_VIBRATO_WINDOW_SEC,
};
use vocara_model::util::dart_num::{clamp_f64, floor_to_i64, round_to_i64};
use vocara_services::monitor_service::{MonitorService, MonitorState};

use super::detail_transport_bar::flexible_row;
use super::live_spectrogram::live_spectrogram;
use super::pitch_lane::{note_vibrato_of, pitch_lane, pitch_notes_of, pitch_range_of};
use crate::ui::core::theme::{self, boxed, fill, font, inset, Studio};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::painter::{KeyedCache, Same};
use crate::ui::core::widgets::spectrogram_painter::{
    FormantTrackPainter, FrequencyGridPainter, LiveFormantPainter, PowerSpectrumPainter,
    RenderRequest, Rendered, SpectrogramImage, VowelSpacePainter, F0_COLOR, FORMANT_COLORS,
};
use crate::ui::core::widgets::studio_hint::studio_hint;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::waveform_painter::PlayheadPainter;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::util::to_string_as_fixed;

#[cfg(test)]
mod tests;

/// The width of the frequency axis left of the spectra; the tab headers are
/// indented past it.
const AXIS_WIDTH: f32 = 46.0;

// Per-metric colors for the Timbre tab's time curves.
const TIMBRE_RING: Color = Color::from_rgb8(0xE0, 0xA0, 0x50);
const TIMBRE_TILT: Color = Color::from_rgb8(0x5B, 0x8D, 0xEF);
const TIMBRE_CPP: Color = Color::from_rgb8(0x52, 0xB7, 0x88);
const TIMBRE_H1H2: Color = Color::from_rgb8(0xD9, 0x8A, 0x8A);

const CHEST_COLOR: Color = Studio::REGISTER_CHEST; // warm = chest
const HEAD_COLOR: Color = Studio::REGISTER_HEAD; // cool = head

/// In-tune color for the live tuner (green); off-pitch ramps toward red.
const TUNER_GREEN: Color = Studio::IN_TUNE;

/// The hint of a take-based tab with no recorded take selected.
pub const SELECT_TAKE_HINT: &str = "Select a recorded take to analyze.";

/// The hint (with a spinner) while the half of the analysis a tab reads is
/// computing.
pub const ANALYZING_HINT: &str = "Analyzing…";

/// The hint of a spectrogram tab whose take has no spectrogram.
pub const NO_SPECTROGRAM_HINT: &str = "Analysis is available for recorded takes (WAV).\n\
                                       Switch to Live to watch your voice as you sing.";

/// The hint of a metrics tab whose take has no (or empty) metrics.
pub const NO_METRICS_HINT: &str = "Analysis is available for recorded takes (WAV).";

/// What the voiceprint tabs ask their host to do.
#[derive(Debug, Clone)]
pub enum Message {
    /// A tap or a horizontal drag on the take spectrogram: move the shared
    /// playhead here (absolute timeline time; Dart's `vm.seek`).
    Seek(Duration),
    /// The Take/Live toggle: show the live mic (`true`) or the take
    /// (`SourceToggle.onSelect`).
    SelectLive(bool),
    /// The Spectrogram tab's take image finished rendering; hand it to
    /// [`VoiceprintState::rendered`].
    ImageRendered(Rendered),
}

// ------------------------------------------------------------------- state

/// What the voiceprint tabs keep between views: the Spectrogram tab's take
/// image. The host keeps one per place it shows the tab (the dock, the
/// technique page).
#[derive(Default)]
pub struct VoiceprintState {
    image: SpectrogramImage,
    /// The spectrogram [`Self::image`] shows, so a view can tell whether it
    /// is the source's.
    shown: Option<Arc<Spectrogram>>,
}

impl VoiceprintState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Brings the take image up to what the Spectrogram tab shows from `src`.
    /// Call it after every update while the tab is on screen (Dart's
    /// `SpectrogramImage` lived while the tab was built), and run the render
    /// it returns; [`Message::ImageRendered`] brings it back. While the tab
    /// shows no take spectrogram (live, a hint, a spinner) the image is let
    /// go, as Dart's went with its widget.
    pub fn sync(&mut self, src: &dyn AnalysisSource) -> Task<Message> {
        match self.sync_request(src) {
            Some(request) => request.into_task(Message::ImageRendered),
            None => Task::none(),
        }
    }

    /// [`Self::sync`], handing back the render to run instead of a task.
    pub fn sync_request(&mut self, src: &dyn AnalysisSource) -> Option<RenderRequest> {
        let Some(spec) = shown_take_spectrogram(src) else {
            self.clear();
            return None;
        };
        self.shown = Some(Arc::clone(&spec));
        self.image.show(&spec)
    }

    /// Lands a render ([`Message::ImageRendered`]). A render for a take the
    /// tab no longer shows is dropped.
    pub fn rendered(&mut self, done: Rendered) {
        self.image.rendered(done);
    }

    /// Lets go of the take image: the tab left the screen.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether the image (rendered or rendering) is `spec`'s.
    fn shows(&self, spec: &Arc<Spectrogram>) -> bool {
        self.shown.as_ref().is_some_and(|s| Arc::ptr_eq(s, spec))
    }
}

/// The spectrogram the Spectrogram tab draws for `src`: the take's, when the
/// tab shows the take (not live, a recorded take selected, its spectrogram
/// computed).
fn shown_take_spectrogram(src: &dyn AnalysisSource) -> Option<Arc<Spectrogram>> {
    if src.live_monitor() || src.spectrogram_loading() {
        return None;
    }
    src.selected_track().filter(|t| !t.is_backing())?;
    src.selected_spectrogram()
}

// ------------------------------------------------------------ time mapping

/// `position.inMilliseconds`.
fn millis(position: Duration) -> i64 {
    i64::try_from(position.as_millis()).unwrap_or(i64::MAX)
}

/// The absolute time a seek at `dx` in a `width`-wide panel lands on. The
/// panel shows the subject in subject-local time [0, dur]; map the click
/// back to the absolute timeline (start + local) so seeking lands on it.
/// (A `Duration` is never negative; a subject starts at or after 0.)
pub fn seek_target(dx: f64, width: f64, duration_ms: i64, start_ms: i64) -> Duration {
    let ms = round_to_i64(start_ms as f64 + clamp_f64(dx / width, 0.0, 1.0) * duration_ms as f64);
    Duration::from_millis(ms.max(0) as u64)
}

/// The absolute playhead `position` as subject-local seconds within the
/// analysis subject's `duration_ms` span (which begins at `start_ms` on the
/// shared timeline), or `None` when the playhead is outside it. Callers pass
/// [`AnalysisSource::analysis_start_ms`] /
/// [`AnalysisSource::analysis_duration_ms`] — the span the shown metrics /
/// spectrogram actually cover: the lane span for a whole-lane subject (a
/// multi-clip aggregate places each clip's frames at its lane-relative
/// offset), or the clip's own span when one clip of a multi-clip lane is
/// selected (its analysis is clip-local).
///
/// `None`, not clamped: a clamped time read the take's first or last frame
/// as if it were under the playhead (a stale edge), so the readouts showed a
/// spectrum, vowel and metrics for audio that is not playing. Outside the
/// span every readout shows its empty state, as the pitch lane does.
pub fn local_seconds(position: Duration, duration_ms: i64, start_ms: i64) -> Option<f64> {
    let ms = millis(position) - start_ms;
    if ms < 0 || ms > duration_ms {
        return None;
    }
    Some(ms as f64 / 1000.0)
}

/// Curve `c`'s value at frame `fr` (NaN past its end: only a malformed
/// analysis gets there).
fn value_at(c: &[f32], fr: usize) -> f64 {
    c.get(fr).map_or(f64::NAN, |&v| f64::from(v))
}

/// The spectrum column and formants the Power tab shows for the take at
/// `position`: an empty column (paints nothing) and no formants outside the
/// subject's span.
pub fn power_readout(
    position: Duration,
    duration_ms: i64,
    start_ms: i64,
    spec: &Spectrogram,
    metrics: Option<&VoiceMetrics>,
) -> (Arc<[f32]>, Vec<f64>) {
    let seconds = local_seconds(position, duration_ms, start_ms);
    // Guard frames > 0: an empty take's f1/f2/f3 are empty lists.
    let fr = match (metrics, seconds) {
        (Some(m), Some(s)) if m.frames > 0 => Some((m, m.frame_at_time(s))),
        _ => None,
    };
    let formants = fr.map_or_else(Vec::new, |(m, fr)| {
        vec![
            value_at(&m.f1, fr),
            value_at(&m.f2, fr),
            value_at(&m.f3, fr),
        ]
    });
    let column: Arc<[f32]> = match seconds {
        None => Arc::from(Vec::new()),
        Some(s) => Arc::from(spec.column_at_time(s)),
    };
    (column, formants)
}

/// The F1 and F2 the Vowel tab marks for the take at `position` (NaN, no
/// dot, outside the subject's span). `m` has frames.
pub fn vowel_formants(
    position: Duration,
    duration_ms: i64,
    start_ms: i64,
    m: &VoiceMetrics,
) -> (f64, f64) {
    match local_seconds(position, duration_ms, start_ms) {
        None => (f64::NAN, f64::NAN),
        Some(s) => {
            let fr = m.frame_at_time(s);
            (value_at(&m.f1, fr), value_at(&m.f2, fr))
        }
    }
}

/// The vibrato of the note under the playhead at `position`, memoized per
/// note: a take's vibrato doesn't change as the playhead moves, so it is not
/// recomputed on position ticks.
pub fn playhead_vibrato(
    position: Duration,
    duration_ms: i64,
    start_ms: i64,
    m: &Arc<VoiceMetrics>,
) -> Option<Arc<Vibrato>> {
    let fr = local_seconds(position, duration_ms, start_ms).map(|s| m.frame_at_time(s))?;
    pitch_notes_of(m)
        .iter()
        .find(|n| fr >= n.start_frame && fr <= n.end_frame)
        .map(|n| note_vibrato_of(m, n))
}

// ------------------------------------------------------------- the guards

/// Hint/loading guard for take-based tabs that need the take's spectrogram.
fn with_spectrogram<'a>(
    src: &dyn AnalysisSource,
    build: impl FnOnce(Arc<Spectrogram>, Option<Arc<VoiceMetrics>>) -> Element<'a, Message>,
) -> Element<'a, Message> {
    if !src.selected_track().is_some_and(|t| !t.is_backing()) {
        return studio_hint(SELECT_TAKE_HINT).into();
    }
    if src.spectrogram_loading() {
        return studio_hint(ANALYZING_HINT).spinner(true).into();
    }
    let Some(spec) = src.selected_spectrogram() else {
        return studio_hint(NO_SPECTROGRAM_HINT).into();
    };
    build(spec, src.selected_metrics())
}

/// Hint/loading guard for take-based tabs that need voice metrics.
fn with_metrics<'a>(
    src: &dyn AnalysisSource,
    build: impl FnOnce(Arc<VoiceMetrics>, Arc<AudioTrack>) -> Element<'a, Message>,
) -> Element<'a, Message> {
    let track = match src.selected_track() {
        Some(track) if !track.is_backing() => track,
        _ => return studio_hint(SELECT_TAKE_HINT).into(),
    };
    if src.metrics_loading() {
        return studio_hint(ANALYZING_HINT).spinner(true).into();
    }
    match src.selected_metrics() {
        Some(m) if m.frames > 0 => build(m, track),
        _ => studio_hint(NO_METRICS_HINT).into(),
    }
}

// ------------------------------------------------------------- the tabs

/// `SpectrogramTab`: the 0–5 kHz spectrogram with the F0/formant overlay,
/// from the selected take (seekable, under the playhead) or the live mic.
/// `state` holds the take image (see [`VoiceprintState::sync`]).
pub fn spectrogram_tab<'a>(
    src: &'a dyn AnalysisSource,
    state: &VoiceprintState,
) -> Element<'a, Message> {
    if src.live_monitor() {
        return live_spectrogram_panel(src.monitor());
    }
    with_spectrogram(src, |spec, metrics| {
        column![
            tab_header("Spectrogram", Some(formant_legend())),
            axis_row(spec.max_hz, take_spectro_stack(src, state, &spec, metrics)),
        ]
        .padding(ltrb(16.0, 12.0, 20.0, 16.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// `PowerTab`: the instantaneous power spectrum (take playhead or live).
pub fn power_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let strip = {
            let m = src.monitor().read();
            SpectrumStrip {
                column: m.latest().cloned().unwrap_or_else(|| Arc::from(Vec::new())),
                bin_hz: m.bin_hz(),
                max_hz: m.max_hz(),
                formants: vec![m.current_f1(), m.current_f2(), m.current_f3()],
                min_ceil_db: Some(48.0),
                label: "Power spectrum",
            }
        };
        return padded(spectrum_strip(strip), ltrb(16.0, 12.0, 20.0, 16.0));
    }
    with_spectrogram(src, |spec, metrics| {
        let (column, formants) = power_readout(
            src.position(),
            src.analysis_duration_ms(),
            src.analysis_start_ms(),
            &spec,
            metrics.as_deref(),
        );
        let strip = SpectrumStrip {
            column,
            bin_hz: spec.bin_hz(),
            max_hz: spec.max_hz,
            formants,
            min_ceil_db: None,
            label: "Power spectrum",
        };
        padded(spectrum_strip(strip), ltrb(16.0, 12.0, 20.0, 16.0))
    })
}

/// `VowelTab`: the F1×F2 vowel chart (take playhead or live).
pub fn vowel_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let (f1, f2) = {
            let m = src.monitor().read();
            (m.current_f1(), m.current_f2())
        };
        return vowel_panel(f1, f2);
    }
    with_metrics(src, |m, _| {
        let (f1, f2) = vowel_formants(
            src.position(),
            src.analysis_duration_ms(),
            src.analysis_start_ms(),
            &m,
        );
        vowel_panel(f1, f2)
    })
}

/// `PitchTab`: pitch — the live tuner, or the take's Logic-style pitch map.
pub fn pitch_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let tuner = live_tuner(&src.monitor().read());
        return padded(column![tuner], ltrb(16.0, 16.0, 20.0, 16.0));
    }
    with_metrics(src, |m, track| {
        let start_ms = src.analysis_start_ms();
        let span_ms = src.analysis_duration_ms();
        let subject = pitch_subject(&track, start_ms, span_ms);
        let position_ms = millis(src.position());
        responsive(move |size| {
            pitch_lane(
                &subject,
                span_ms,
                position_ms,
                132.0,
                false,
                Some(Arc::clone(&m)),
            )
            .height(size.height)
            .take_local(true)
            .into()
        })
        .into()
    })
}

/// The track the Pitch tab hands its lane. The lane derives the metrics'
/// absolute origin from the track's start (its take-local playhead mapping
/// and note readout both subtract it). When the analysis subject is one clip
/// of a multi-clip lane, the metrics are clip-local, so it gets a single-clip
/// view spanning the subject axis instead of the whole lane. A whole-lane
/// subject's axis equals the lane span, so the original track passes through
/// untouched there.
pub fn pitch_subject(track: &Arc<AudioTrack>, start_ms: i64, span_ms: i64) -> Arc<AudioTrack> {
    if start_ms == track.start_ms() && span_ms == track.duration_ms() {
        return Arc::clone(track);
    }
    let mut subject = AudioTrack::clone(track);
    subject.clips = vec![AudioClip {
        start_ms,
        ..AudioClip::new(
            format!("{}-analysis-subject", track.id),
            track
                .primary_clip()
                .map_or("", |c| c.audio_file_path.as_str()),
            span_ms,
        )
    }];
    Arc::new(subject)
}

/// `TimbreTab`: timbre — RING/TILT/CPP/H1−H2 numbers and their time curves
/// (take), or the live numbers.
pub fn timbre_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let bar = live_metrics_bar(&src.monitor().read());
        return padded(
            column![
                bar,
                Space::new().height(16),
                text("Timbre curves are available for recorded takes.")
                    .size(12)
                    .color(Studio::TEXT_FAINT),
            ],
            ltrb(16.0, 16.0, 20.0, 16.0),
        );
    }
    with_metrics(src, |m, _| {
        let span_ms = src.analysis_duration_ms();
        let start_ms = src.analysis_start_ms();
        let position = src.position();
        // The curves walk every frame of the take and keep their own cache;
        // the metrics bar above changes on each playhead tick, and the
        // playhead is its own layer.
        let curves = stack![
            canvas_widget(TimbreCurvesPainter {
                metrics: Arc::clone(&m),
                timeline_duration_ms: span_ms,
            })
            .width(Length::Fill)
            .height(Length::Fill),
            playhead(position, span_ms, start_ms, false),
        ];
        column![
            metrics_bar(position, &m, span_ms, start_ms),
            Space::new().height(12),
            timbre_legend(),
            Space::new().height(6),
            container(curves)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &Theme| boxed(Some(Studio::INSET), 6.0, None)),
        ]
        .padding(ltrb(16.0, 12.0, 20.0, 16.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// `LtasTab`: the long-term average spectrum (the take's spectral
/// "fingerprint" — brightness + singer's-formant cluster), or the live
/// running average.
pub fn ltas_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let ltas = src.monitor().read().live_ltas();
        return match ltas {
            Some(ltas) if ltas.has_data() => ltas_body(&ltas, true),
            _ => studio_hint("Sing into the mic to build your live average spectrum.").into(),
        };
    }
    with_spectrogram(src, |_, _| {
        // Read before the metrics land, the LTAS is built without its voiced
        // gate, then rebuilt with it: a spectrum that changes under the
        // singer.
        if src.metrics_loading() {
            return studio_hint(ANALYZING_HINT).spinner(true).into();
        }
        match src.selected_ltas() {
            Some(ltas) if ltas.has_data() => ltas_body(&ltas, false),
            _ => studio_hint("Sing a passage to see your average spectrum.").into(),
        }
    })
}

/// `_dbOrDash`: a stat that cannot be measured reads as a dash.
fn db_or_dash(v: f64) -> String {
    if v.is_finite() {
        format!("{} dB", round_to_i64(v))
    } else {
        "—".to_string()
    }
}

fn ltas_body<'a>(ltas: &Ltas, live: bool) -> Element<'a, Message> {
    let mut stats = Row::new().align_y(Vertical::Center);
    if live {
        stats = stats.push(live_dot()).push(Space::new().width(14));
    }
    // Guarded: Dart's round() threw on NaN / Infinity, which swapped the
    // whole tab for an error widget; an unmeasurable stat reads as a dash
    // like the dock's other readouts.
    stats = stats
        .push(stat_inline(
            "Singer's formant",
            db_or_dash(ltas.singers_formant_db),
            Studio::TEXT_PRIMARY,
        ))
        .push(Space::new().width(22))
        .push(stat_inline(
            "Brightness α",
            db_or_dash(ltas.alpha_ratio),
            Studio::TEXT_PRIMARY,
        ));
    let strip = SpectrumStrip {
        column: Arc::from(ltas.db.as_slice()),
        bin_hz: ltas.bin_hz,
        max_hz: ltas.max_hz,
        formants: Vec::new(),
        min_ceil_db: None,
        label: if live {
            "LTAS · running average"
        } else {
            "LTAS · average spectrum"
        },
    };
    column![
        container(stats).padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 6.0)),
        spectrum_strip(strip),
    ]
    .padding(ltrb(16.0, 12.0, 20.0, 16.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// `VibratoTab`: the vibrato of the note under the playhead (take), or of
/// the trailing live window.
pub fn vibrato_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let v = {
            let m = src.monitor().read();
            analyze_vibrato_live(
                m.f0_history(),
                m.frame_rate(),
                LIVE_VIBRATO_WINDOW_SEC,
                VibratoOptions::default(),
            )
        };
        return vibrato_body(
            Some(Arc::new(v)),
            true,
            "Hold a sustained note into the mic to measure its vibrato.",
        );
    }
    with_metrics(src, |m, _| {
        let v = playhead_vibrato(
            src.position(),
            src.analysis_duration_ms(),
            src.analysis_start_ms(),
            &m,
        );
        vibrato_body(
            v,
            false,
            "Hold a sustained note on the playhead to measure its vibrato.",
        )
    })
}

fn vibrato_body<'a>(v: Option<Arc<Vibrato>>, live: bool, empty_hint: &str) -> Element<'a, Message> {
    let header = vibrato_header(v.as_deref(), live);
    let content: Element<'a, Message> = match v {
        Some(v) if v.deviation_cents.len() > 4 => canvas_widget(VibratoPainter { v })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        _ => center(
            text(empty_hint.to_string())
                .size(12)
                .color(Studio::TEXT_FAINT),
        )
        .into(),
    };
    column![
        header,
        Space::new().height(14),
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| boxed(Some(Studio::INSET), 6.0, None)),
    ]
    .padding(ltrb(20.0, 16.0, 20.0, 18.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn vibrato_header<'a>(v: Option<&Vibrato>, live: bool) -> Element<'a, Message> {
    let v = match v {
        Some(v) if v.present => v,
        _ => {
            let dot: Element<'a, Message> = if live {
                container(live_dot())
                    .padding(ltrb(0.0, 0.0, 6.0, 0.0))
                    .into()
            } else {
                Space::new().into()
            };
            return row![
                dot,
                stat_inline(
                    "Vibrato",
                    "none / straight tone".to_string(),
                    Studio::TEXT_PRIMARY
                ),
            ]
            .align_y(Vertical::Center)
            .into();
        }
    };
    let healthy = v.rate_hz >= 5.0 && v.rate_hz <= 7.0;
    let mut stats = Row::new().spacing(22).align_y(Vertical::Center);
    if live {
        stats = stats.push(live_dot());
    }
    stats = stats
        .push(stat_inline(
            "Rate",
            format!("{} Hz", to_string_as_fixed(v.rate_hz, 1)),
            if healthy { TUNER_GREEN } else { Studio::RECORD },
        ))
        .push(stat_inline(
            "Extent",
            format!("±{}¢", round_to_i64(v.extent_cents)),
            Studio::TEXT_PRIMARY,
        ))
        .push(stat_inline(
            "Regularity",
            format!("{}%", round_to_i64(v.regularity * 100.0)),
            Studio::TEXT_PRIMARY,
        ));
    if !v.jitter.is_nan() {
        stats = stats.push(stat_inline(
            "Jitter",
            format!("{}%", round_to_i64(v.jitter * 100.0)),
            if v.jitter <= 0.1 {
                TUNER_GREEN
            } else {
                Studio::RECORD
            },
        ));
    }
    if !v.onset_ms.is_nan() {
        stats = stats.push(stat_inline(
            "Onset",
            format!("{} ms", round_to_i64(v.onset_ms)),
            Studio::TEXT_PRIMARY,
        ));
    }
    stats.wrap().vertical_spacing(8).into()
}

/// `RegisterTab`: the pitch contour colored by register (chest↔head) — an
/// indicator from H1−H2, not a verdict — or the live register meter.
pub fn register_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        return live_register_panel(&src.monitor().read());
    }
    with_metrics(src, |m, _| {
        let reg = match src.selected_register() {
            Some(reg) if reg.has_data() => reg,
            _ => return studio_hint("Sing a passage to map your registers.").into(),
        };
        let range = pitch_range_of(&m);
        let span_ms = src.analysis_duration_ms();
        let start_ms = src.analysis_start_ms();
        let swatches = row![
            reg_swatch(
                CHEST_COLOR,
                format!("chest {}%", round_to_i64(reg.chest_pct))
            ),
            reg_swatch(
                lerp_color(CHEST_COLOR, HEAD_COLOR, 0.5),
                format!("mix {}%", round_to_i64(reg.mix_pct)),
            ),
            reg_swatch(HEAD_COLOR, format!("head {}%", round_to_i64(reg.head_pct))),
        ]
        .spacing(18)
        .align_y(Vertical::Center)
        .wrap()
        .vertical_spacing(6);
        let map = stack![
            canvas_widget(RegisterMapPainter {
                metrics: Arc::clone(&m),
                register: reg,
                lo_semi: range.lo,
                hi_semi: range.hi,
                duration_ms: span_ms,
            })
            .width(Length::Fill)
            .height(Length::Fill),
            playhead(src.position(), span_ms, start_ms, false),
        ];
        column![
            container(swatches).padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 6.0)),
            container(map)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &Theme| boxed(Some(Studio::INSET), 6.0, None)),
        ]
        .padding(ltrb(16.0, 12.0, 20.0, 16.0))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// `SourceToggle(live: ..., onSelect: ...)`: the dock's Take | Live
/// segmented switch; a segment's tap is [`Message::SelectLive`].
pub fn source_toggle<'a>(live: bool) -> Element<'a, Message> {
    container(row![
        toggle_segment("Take", !live, Message::SelectLive(false)),
        toggle_segment("Live", live, Message::SelectLive(true)),
    ])
    .padding(2)
    .style(inset(7.0))
    .into()
}

fn toggle_segment<'a>(
    label: &'static str,
    selected: bool,
    on_tap: Message,
) -> Element<'a, Message> {
    let fill_color = if label == "Live" {
        Studio::RECORD
    } else {
        Studio::ACCENT
    };
    button(
        text(label)
            .size(12)
            .font(font::SEMIBOLD)
            .color(if selected {
                Studio::BACKGROUND
            } else {
                Studio::TEXT_SECONDARY
            }),
    )
    .padding([5, 12])
    .style(move |_: &Theme, _| button::Style {
        background: selected.then_some(Background::Color(fill_color)),
        text_color: Studio::TEXT_PRIMARY,
        border: Border {
            radius: Radius::from(5.0),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: false,
    })
    .on_press(on_tap)
    .into()
}

// ------------------------------------------------------ the take spectrogram

/// The take spectrogram stack: image + grid + formant overlay + seek +
/// playhead. The image, the overlay, the seek and the playhead all map the
/// same subject axis [start, start + span], so a column, its dots, the
/// moment a tap seeks to and the frame the readouts show agree.
///
/// The still layers keep their own caches: every notification of the
/// screen rebuilds this view, and a playhead tick or a fader tick on any
/// lane must not re-record the overlay — ~225k dots for a 10-minute take —
/// at pointer rate.
fn take_spectro_stack<'a>(
    src: &dyn AnalysisSource,
    state: &VoiceprintState,
    spec: &Arc<Spectrogram>,
    metrics: Option<Arc<VoiceMetrics>>,
) -> Element<'a, Message> {
    let span_ms = src.analysis_duration_ms();
    let start_ms = src.analysis_start_ms();
    // The image of ANOTHER take must not stand in: it would sit over this
    // take's frames, under this take's overlay.
    let image = if state.shows(spec) {
        state.image.view(span_ms)
    } else {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    };
    let mut still = stack![
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(Studio::INSET)),
        image,
        canvas_widget(FrequencyGridPainter::new(spec.max_hz))
            .width(Length::Fill)
            .height(Length::Fill),
    ];
    if let Some(m) = metrics {
        still = still.push(
            canvas_widget(FormantTrackPainter {
                metrics: m,
                max_hz: spec.max_hz,
                timeline_duration_ms: span_ms,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        );
    }
    stack![
        still,
        canvas_widget(SeekArea {
            duration_ms: span_ms,
            start_ms,
        })
        .width(Length::Fill)
        .height(Length::Fill),
        playhead(src.position(), span_ms, start_ms, true),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// `kPrecisePointerHitSlop`: how far a mouse press travels horizontally
/// before a horizontal drag recognizer accepts it.
const PRECISE_HIT_SLOP: f32 = 1.0;

/// The seek gesture over the take spectrogram (Dart's `GestureDetector`,
/// `HitTestBehavior.opaque`): a press seeks to where it lands (its tap-down;
/// Flutter sent it after 100 ms or on release, unless a drag won first), and
/// once the pointer has moved past the hit slop horizontally, every move of
/// the drag seeks to the pointer — inside the panel or past its edges
/// (clamped to the span).
struct SeekArea {
    duration_ms: i64,
    start_ms: i64,
}

/// A press in progress: where it landed, and whether it became a drag.
#[derive(Debug, Default)]
struct SeekDrag {
    down: Option<Point>,
    dragging: bool,
}

impl SeekArea {
    fn seek(&self, x: f32, width: f32) -> Message {
        Message::Seek(seek_target(
            f64::from(x),
            f64::from(width),
            self.duration_ms,
            self.start_ms,
        ))
    }
}

impl canvas::Program<Message> for SeekArea {
    type State = SeekDrag;

    fn update(
        &self,
        state: &mut SeekDrag,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let at = cursor.position_in(bounds)?;
                *state = SeekDrag {
                    down: Some(at),
                    dragging: false,
                };
                Some(canvas::Action::publish(self.seek(at.x, bounds.width)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let down = state.down?;
                let at = cursor.position_from(bounds.position())?;
                if !state.dragging {
                    if (at.x - down.x).abs() <= PRECISE_HIT_SLOP {
                        return None;
                    }
                    state.dragging = true;
                }
                Some(canvas::Action::publish(self.seek(at.x, bounds.width)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.down.take()?;
                state.dragging = false;
                Some(canvas::Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &SeekDrag,
        _: &Renderer,
        _: &Theme,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        Vec::new()
    }
}

/// `PlayheadLayer(position, timelineDurationMs: span, color:
/// Studio.playhead, showHead, startMs)`.
fn playhead<'a>(
    position: Duration,
    span_ms: i64,
    start_ms: i64,
    show_head: bool,
) -> Element<'a, Message> {
    canvas_widget(PlayheadPainter {
        position_ms: millis(position),
        timeline_duration_ms: span_ms,
        color: Studio::PLAYHEAD,
        show_head,
        start_ms,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// --------------------------------------------------------- the live panels

/// `_LiveSpectrogramPanel`: the live-mic spectrogram (the Spectrogram tab in
/// Live mode) — the LIVE badge, the input level and the legend over the
/// rolling spectrogram and its formant overlay.
fn live_spectrogram_panel<'a>(monitor: &'a MonitorService) -> Element<'a, Message> {
    // Read and let go before the spectrogram reads the monitor itself.
    let (level, overlay) = {
        let m = monitor.read();
        (
            m.level(),
            LiveFormantPainter {
                f0: m.f0_history().to_vec(),
                f1: m.f1_history().to_vec(),
                f2: m.f2_history().to_vec(),
                f3: m.f3_history().to_vec(),
                max_hz: m.max_hz(),
            },
        )
    };
    let max_hz = overlay.max_hz;
    let header = row![
        live_dot(),
        Space::new().width(12),
        canvas_widget(MonitorLevel { level }).width(80).height(8),
        Space::new().width(Length::Fill),
        formant_legend(),
    ]
    .align_y(Vertical::Center);
    let panel = stack![
        live_spectrogram(monitor),
        canvas_widget(overlay)
            .width(Length::Fill)
            .height(Length::Fill),
        // `ClipRRect(borderRadius: BorderRadius.circular(6))` over both.
        canvas_widget(RoundedCorners {
            radius: LIVE_CLIP_RADIUS,
            backdrop: Studio::SURFACE,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    ];
    column![
        container(header).padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 6.0)),
        axis_row(max_hz, panel),
    ]
    .padding(ltrb(16.0, 12.0, 20.0, 16.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The live panel's `ClipRRect` radius.
const LIVE_CLIP_RADIUS: f32 = 6.0;

/// A rounded clip over what lies under it in a stack, drawn as its
/// complement: the four corners outside a rectangle rounded by `radius`
/// (circular arcs, an `RRect`), filled with the opaque `backdrop` that
/// shows there around the clipped box. Takes no input.
pub(crate) struct RoundedCorners {
    pub radius: f32,
    pub backdrop: Color,
}

impl RoundedCorners {
    /// The corners of a `size` box, as one path.
    fn path(&self, size: Size) -> Path {
        let r = self.radius.min(size.width / 2.0).min(size.height / 2.0);
        let (w, h) = (size.width, size.height);
        Path::new(|p| {
            // Each corner: along one edge, round the arc's tangent from
            // the corner, back to it.
            for (corner, along, to) in [
                (Point::ORIGIN, Point::new(r, 0.0), Point::new(0.0, r)),
                (Point::new(w, 0.0), Point::new(w, r), Point::new(w - r, 0.0)),
                (Point::new(w, h), Point::new(w - r, h), Point::new(w, h - r)),
                (Point::new(0.0, h), Point::new(0.0, h - r), Point::new(r, h)),
            ] {
                p.move_to(corner);
                p.line_to(along);
                p.arc_to(corner, to, r);
                p.close();
            }
        })
    }
}

impl<M> canvas::Program<M> for RoundedCorners {
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
        if self.radius > 0.0 {
            frame.fill(&self.path(bounds.size()), self.backdrop);
        }
        vec![frame.into_geometry()]
    }
}

/// `_LiveRegisterPanel`: the running chest/mix/head split plus a real-time
/// chest↔head meter driven by the current frame's H1−H2.
fn live_register_panel<'a>(m: &MonitorState) -> Element<'a, Message> {
    let total = m.register_voiced();
    let inv = if total == 0 {
        0.0
    } else {
        100.0 / total as f64
    };
    let h = m.current_h1h2();
    let has_now = h.is_finite();
    let score = if has_now {
        register_score(h, RegisterOptions::default())
    } else {
        f64::NAN
    };
    let label = if !has_now {
        "—"
    } else if score < 0.4 {
        "CHEST"
    } else if score < 0.6 {
        "MIX"
    } else {
        "HEAD"
    };
    if total == 0 && !has_now {
        return studio_hint("Sing into the mic to map your registers live.").into();
    }
    let pct = |n: usize| round_to_i64(n as f64 * inv);
    let swatches = row![
        live_dot(),
        reg_swatch(CHEST_COLOR, format!("chest {}%", pct(m.register_chest()))),
        reg_swatch(
            lerp_color(CHEST_COLOR, HEAD_COLOR, 0.5),
            format!("mix {}%", pct(m.register_mix())),
        ),
        reg_swatch(HEAD_COLOR, format!("head {}%", pct(m.register_head()))),
    ]
    .spacing(18)
    .align_y(Vertical::Center)
    .wrap()
    .vertical_spacing(6);
    let label_color = if has_now {
        lerp_color(CHEST_COLOR, HEAD_COLOR, clamp_f64(score, 0.0, 1.0) as f32)
    } else {
        Studio::TEXT_FAINT
    };
    let readout = column![
        studio_text(label)
            .size(24)
            .font(font::EXTRA_BOLD)
            .letter_spacing(1.5)
            .color(label_color),
        Space::new().height(4),
        text(if has_now {
            format!("H1−H2  {} dB", to_string_as_fixed(h, 1))
        } else {
            "Hold a sustained note".to_string()
        })
        .size(11)
        .color(Studio::TEXT_FAINT),
        // 20 apart in Dart; the meter's marker reaches 3 px above its bar,
        // so the meter's box starts 3 px higher here.
        Space::new().height(20.0 - METER_OVERHANG),
        register_meter(score),
    ]
    .align_x(Horizontal::Center);
    column![
        container(swatches).padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 6.0)),
        center(readout),
    ]
    .padding(ltrb(16.0, 12.0, 20.0, 16.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// How far the register meter's marker reaches above and below its bar.
const METER_OVERHANG: f32 = 3.0;

/// `_RegisterMeter`: a horizontal chest↔head gradient bar with a marker at
/// the current `score` (NaN hides the marker), labelled chest / head.
fn register_meter<'a>(score: f64) -> Element<'a, Message> {
    column![
        canvas_widget(RegisterMeterPainter { score })
            .width(Length::Fill)
            .height(14.0 + 2.0 * METER_OVERHANG),
        Space::new().height(6.0 - METER_OVERHANG),
        container(row![
            text("chest").size(9).color(Studio::TEXT_FAINT),
            Space::new().width(Length::Fill),
            text("head").size(9).color(Studio::TEXT_FAINT),
        ])
        .padding([0, 2]),
    ]
    .width(280)
    .into()
}

/// `_LiveTuner`: a prominent real-time tuner — the nearest note and a needle
/// that swings left/right with how flat/sharp the live pitch is, centering
/// green when in tune. Driven by the monitor's smoothed tuner F0.
fn live_tuner<'a>(m: &MonitorState) -> Element<'a, Message> {
    let content: Element<'a, Message> = match note_of(m.tuner_f0()) {
        None => center(
            text("Sing a sustained note to tune…")
                .size(13)
                .color(Studio::TEXT_FAINT),
        )
        .into(),
        Some(note) => {
            let cents = note.cents;
            let color = lerp_color(
                TUNER_GREEN,
                Studio::RECORD,
                clamp_f64(cents.abs() / 35.0, 0.0, 1.0) as f32,
            );
            row![
                container(text(note.name).size(24).font(font::EXTRA_BOLD).color(color)).width(56),
                canvas_widget(TunerDialPainter { cents, color })
                    .width(Length::Fill)
                    .height(Length::Fill),
                container(
                    text(format!(
                        "{}{}¢",
                        if cents >= 0.0 { '+' } else { '−' },
                        round_to_i64(cents.abs())
                    ))
                    .size(15)
                    .font(font::BOLD)
                    .color(color)
                    .align_x(Horizontal::Right)
                )
                .width(60)
                .align_x(Horizontal::Right),
            ]
            .align_y(Vertical::Center)
            .height(Length::Fill)
            .into()
        }
    };
    container(content)
        .width(Length::Fill)
        .height(62)
        .padding([8, 14])
        .style(|_: &Theme| boxed(Some(Studio::SURFACE), 8.0, Some(Studio::BORDER)))
        .into()
}

/// `_LiveMetricsBar`: the live RING / TILT / CPP readouts with their
/// session averages. F0 is shown prominently by the tuner, so it's dropped
/// here.
fn live_metrics_bar<'a>(m: &MonitorState) -> Element<'a, Message> {
    row![
        metric_chip(
            "RING",
            "Singer's-formant ring (3 kHz projection)",
            m.ring(),
            m.ring_avg(),
        ),
        Space::new().width(8),
        metric_chip(
            "TILT",
            "Spectral tilt: higher = brighter/pressed",
            m.tilt(),
            m.tilt_avg(),
        ),
        Space::new().width(8),
        metric_chip(
            "CPP",
            "Cepstral peak prominence: higher = cleaner",
            m.cpp(),
            m.cpp_avg(),
        ),
        Space::new().width(Length::Fill),
        formant_legend(),
    ]
    .align_y(Vertical::Center)
    .into()
}

// ------------------------------------------------------------ the readouts

/// `_MetricsBar`: a compact readout of the take's timbre metrics — ring,
/// tilt and CPP — each showing the value at the playhead plus the take
/// average, after the F0 at the playhead.
fn metrics_bar<'a>(
    position: Duration,
    m: &VoiceMetrics,
    duration_ms: i64,
    start_ms: i64,
) -> Element<'a, Message> {
    let t = local_seconds(position, duration_ms, start_ms);
    // Outside the subject's span every chip reads "—" (NaN).
    let at = |series: &[f32]| t.map_or(f64::NAN, |t| m.sample_at(series, t));
    row![
        f0_chip(at(&m.f0)),
        Space::new().width(8),
        metric_chip(
            "RING",
            "Singer's-formant ring (3 kHz projection): higher carries more",
            at(&m.ring),
            m.ring_avg,
        ),
        Space::new().width(8),
        metric_chip(
            "TILT",
            "Spectral tilt: higher = brighter/pressed, lower = darker/breathy",
            at(&m.tilt),
            m.tilt_avg,
        ),
        Space::new().width(8),
        metric_chip(
            "CPP",
            "Cepstral peak prominence: higher = cleaner, lower = breathier",
            at(&m.cpp),
            m.cpp_avg,
        ),
        Space::new().width(Length::Fill),
        formant_legend(),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// The height of a readout chip (5 px padding around a 12 px line), for its
/// tooltip's placement.
const CHIP_HEIGHT: f32 = 26.0;

/// A Material tooltip below `target`: its top 24 px below the target's
/// centre.
fn with_tip<'a>(target: Element<'a, Message>, tip: &'static str) -> Element<'a, Message> {
    material_tooltip(target, tip, 24.0 - CHIP_HEIGHT / 2.0).into()
}

/// `_F0Chip`: the F0 (pitch) readout — frequency plus the equal-tempered
/// note name and cents.
fn f0_chip<'a>(hz: f64) -> Element<'a, Message> {
    let has = !hz.is_nan() && hz > 0.0;
    let mut content = row![
        dot(F0_COLOR, 8.0),
        Space::new().width(6),
        studio_text("F0")
            .size(9)
            .font(font::BOLD)
            .letter_spacing(0.6)
            .color(Studio::TEXT_FAINT),
        Space::new().width(6),
        text(if has {
            format!("{} Hz", round_to_i64(hz))
        } else {
            "—".to_string()
        })
        .size(12)
        .font(font::BOLD)
        .color(Studio::TEXT_PRIMARY),
    ]
    .align_y(Vertical::Center);
    if has {
        content = content.push(Space::new().width(6)).push(
            text(note_label(hz))
                .size(11)
                .font(font::SEMIBOLD)
                .color(Studio::ACCENT_BRIGHT),
        );
    }
    with_tip(
        container(content).padding([5, 10]).style(inset(6.0)).into(),
        "Fundamental pitch (F0) at the playhead",
    )
}

/// `_Metric`: one timbre readout — the value at the playhead (or live) and
/// the average.
fn metric_chip<'a>(
    label: &'static str,
    tip: &'static str,
    value: f64,
    avg: f64,
) -> Element<'a, Message> {
    let fmt = |v: f64| {
        if v.is_nan() {
            "—".to_string()
        } else {
            to_string_as_fixed(v, 1)
        }
    };
    let content = row![
        studio_text(label)
            .size(9)
            .font(font::BOLD)
            .letter_spacing(0.6)
            .color(Studio::TEXT_FAINT),
        Space::new().width(6),
        text(format!("{} dB", fmt(value)))
            .size(12)
            .font(font::BOLD)
            .color(Studio::TEXT_PRIMARY),
        Space::new().width(5),
        text(format!("avg {}", fmt(avg)))
            .size(9)
            .color(Studio::TEXT_FAINT),
    ]
    .align_y(Vertical::Center);
    with_tip(
        container(content).padding([5, 10]).style(inset(6.0)).into(),
        tip,
    )
}

/// `_StatInline`: a small inline "LABEL value" stat used in tab headers.
fn stat_inline<'a>(label: &str, value: String, accent: Color) -> Element<'a, Message> {
    row![
        studio_text(format!("{}  ", label.to_uppercase()))
            .size(9)
            .font(font::BOLD)
            .letter_spacing(0.5)
            .color(Studio::TEXT_FAINT),
        text(value).size(12).font(font::BOLD).color(accent),
    ]
    .align_y(Vertical::Center)
    .into()
}

// -------------------------------------------------------------- the chrome

/// `EdgeInsets.fromLTRB`.
fn ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

/// `Padding(padding: ..., child: ...)` filling the tab.
fn padded<'a>(content: impl Into<Element<'a, Message>>, padding: Padding) -> Element<'a, Message> {
    container(content)
        .padding(padding)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// `Color.lerp(a, b, t)`.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Color {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
}

/// A `size` px circle in `color`.
fn dot<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container(Space::new())
        .width(size)
        .height(size)
        .style(move |_: &Theme| boxed(Some(color), size / 2.0, None))
        .into()
}

/// `_LiveDot`: a small red "● LIVE" badge for live tab headers.
fn live_dot<'a>() -> Element<'a, Message> {
    row![
        dot(Studio::RECORD, 8.0),
        Space::new().width(6),
        studio_text("LIVE")
            .size(10)
            .font(font::BOLD)
            .letter_spacing(0.5)
            .color(Studio::RECORD),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// `_regSwatch`: a register's colour dot and its share.
fn reg_swatch<'a>(color: Color, label: String) -> Element<'a, Message> {
    row![
        dot(color, 9.0),
        Space::new().width(5),
        text(label).size(10).color(Studio::TEXT_SECONDARY),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// `_TabHeader`: a small uppercase header row for a tab, indented past the
/// frequency axis, with an optional `trailing` widget after it.
fn tab_header<'a>(label: &str, trailing: Option<Element<'a, Message>>) -> Element<'a, Message> {
    // The label is `Flexible`: it ellipsizes in what `trailing` leaves it.
    let mut header: Vec<Element<'a, Message>> = vec![studio_text(label.to_uppercase())
        .size(10)
        .font(font::BOLD)
        .letter_spacing(0.8)
        .color(Studio::TEXT_FAINT)
        .ellipsis(1)
        .into()];
    if let Some(trailing) = trailing {
        header.push(Space::new().width(10).into());
        header.push(trailing);
    }
    container(flexible_row(header, 0))
        .padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 6.0))
        .into()
}

/// `_PanelLabel`: a small uppercase panel caption (POWER SPECTRUM, LTAS …),
/// indented past the frequency axis so the panels read consistently.
fn panel_label<'a>(label: &str) -> Element<'a, Message> {
    container(
        studio_text(label.to_uppercase())
            .size(10)
            .font(font::BOLD)
            .letter_spacing(0.8)
            .color(Studio::TEXT_FAINT),
    )
    .padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 4.0))
    .into()
}

/// `_FormantLegend`: the color key for the F0 and F1/F2/F3 tracks drawn on
/// the spectrogram.
fn formant_legend<'a>() -> Element<'a, Message> {
    let entry = |color: Color, label: &'static str| -> Element<'a, Message> {
        container(
            row![
                dot(color, 8.0),
                Space::new().width(4),
                text(label).size(10).color(Studio::TEXT_SECONDARY),
            ]
            .align_y(Vertical::Center),
        )
        .padding(ltrb(10.0, 0.0, 0.0, 0.0))
        .into()
    };
    row![
        entry(F0_COLOR, "F0"),
        entry(FORMANT_COLORS[0], "F1"),
        entry(FORMANT_COLORS[1], "F2"),
        entry(FORMANT_COLORS[2], "F3"),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// `_TimbreLegend`: the color key for the timbre time curves.
fn timbre_legend<'a>() -> Element<'a, Message> {
    let entry = |color: Color, label: &'static str| -> Element<'a, Message> {
        container(
            row![
                container(Space::new())
                    .width(12)
                    .height(2.5)
                    .style(fill(color)),
                Space::new().width(5),
                text(label).size(10).color(Studio::TEXT_SECONDARY),
            ]
            .align_y(Vertical::Center),
        )
        .padding(ltrb(0.0, 0.0, 16.0, 0.0))
        .into()
    };
    row![
        entry(TIMBRE_RING, "RING"),
        entry(TIMBRE_TILT, "TILT"),
        entry(TIMBRE_CPP, "CPP"),
        entry(TIMBRE_H1H2, "H1−H2"),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// `_AxisRow`: a frequency axis on the left and `child` on the right.
fn axis_row<'a>(max_hz: f64, child: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        canvas_widget(FreqAxisPainter { max_hz })
            .width(AXIS_WIDTH)
            .height(Length::Fill),
        container(child).width(Length::Fill).height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// `_vowelPanel`: a large centered F1×F2 vowel chart filling the tab, 1.3
/// wide for 1 tall.
fn vowel_panel<'a>(f1: f64, f2: f64) -> Element<'a, Message> {
    let chart = responsive(move |size| {
        let (w, h) = aspect_fit(size, 1.3);
        center(
            container(
                canvas_widget(VowelSpacePainter { f1, f2 })
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .padding(12)
            .width(w)
            .height(h)
            .style(|_: &Theme| boxed(Some(Studio::INSET), 8.0, None)),
        )
        .into()
    });
    column![
        studio_text("VOWEL  F1×F2")
            .size(10)
            .font(font::BOLD)
            .letter_spacing(0.8)
            .color(Studio::TEXT_FAINT),
        Space::new().height(8),
        chart,
    ]
    .padding(ltrb(20.0, 14.0, 20.0, 18.0))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// `AspectRatio(aspectRatio: ratio)` in a box of `size`: as wide as fits,
/// then as tall as the ratio allows.
fn aspect_fit(size: Size, ratio: f32) -> (f32, f32) {
    let w = size.width.min(size.height * ratio).max(0.0);
    (w, w / ratio)
}

/// `_SpectrumStrip`'s parameters.
struct SpectrumStrip {
    column: Arc<[f32]>,
    bin_hz: f64,
    max_hz: f64,
    formants: Vec<f64>,
    min_ceil_db: Option<f64>,
    label: &'static str,
}

/// `_SpectrumStrip`: a captioned power-spectrum panel — a rotated "dB" in
/// the axis column, the spectrum and its kHz labels in an inset well.
fn spectrum_strip<'a>(s: SpectrumStrip) -> Element<'a, Message> {
    let mut painter = PowerSpectrumPainter::new(s.column, s.bin_hz, s.max_hz);
    painter.formants = s.formants;
    painter.min_ceil_db = s.min_ceil_db;
    let well = stack![
        canvas_widget(painter)
            .width(Length::Fill)
            .height(Length::Fill),
        canvas_widget(FreqAxisLabels { max_hz: s.max_hz })
            .width(Length::Fill)
            .height(Length::Fill),
    ];
    column![
        panel_label(s.label),
        row![
            canvas_widget(RotatedLabel { label: "dB" })
                .width(AXIS_WIDTH)
                .height(Length::Fill),
            container(well)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_: &Theme| boxed(Some(Studio::INSET), 6.0, None)),
        ]
        .height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// --------------------------------------------------------------- painters

/// A canvas text at `position`, aligned as given.
fn label_text(
    content: String,
    position: Point,
    size: f32,
    color: Color,
    align_x: Alignment,
    align_y: Vertical,
) -> canvas::Text {
    canvas::Text {
        content,
        position,
        color,
        size: Pixels(size),
        font: font::REGULAR,
        align_x,
        align_y,
        ..canvas::Text::default()
    }
}

/// `_FreqAxisPainter`: the frequency axis — `1k`, `2k`, … right-aligned 6 px
/// from its edge at each kHz's height.
struct FreqAxisPainter {
    max_hz: f64,
}

impl<M> canvas::Program<M> for FreqAxisPainter {
    type State = KeyedCache<u64>;

    fn draw(
        &self,
        state: &KeyedCache<u64>,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        vec![state.draw(renderer, size, self.max_hz.to_bits(), |frame| {
            // A non-finite top would never end the walk.
            if !self.max_hz.is_finite() {
                return;
            }
            let mut hz = 1000.0;
            while hz <= self.max_hz {
                let y = f64::from(size.height) * (1.0 - hz / self.max_hz);
                frame.fill_text(label_text(
                    format!("{}k", round_to_i64(hz / 1000.0)),
                    Point::new(size.width - 6.0, y as f32),
                    10.0,
                    Studio::TEXT_FAINT,
                    Alignment::Right,
                    Vertical::Center,
                ));
                hz += 1000.0;
            }
        })]
    }
}

/// Where `_FreqAxisLabels` puts each kHz label under a `width`-wide
/// spectrum: `(k, x, alignment)` for k = 0 ..= ⌊max_hz / 1000⌋, at each kHz's
/// actual position (0 → left edge, `max_hz` → right edge) so they line up
/// with the curve instead of being crammed together; the first starts at its
/// x, the last ends at it, the rest centre on it.
pub fn freq_axis_labels(max_hz: f64, width: f64) -> Vec<(i64, f64, Alignment)> {
    // A non-finite top would never end the walk.
    if !max_hz.is_finite() {
        return Vec::new();
    }
    let max_khz = floor_to_i64(max_hz / 1000.0);
    (0..=max_khz)
        .map(|k| {
            let x = clamp_f64(k as f64 * 1000.0 / max_hz * width, 0.0, width);
            let align = if k == 0 {
                Alignment::Left
            } else if k == max_khz {
                Alignment::Right
            } else {
                Alignment::Center
            };
            (k, x, align)
        })
        .collect()
}

/// `_FreqAxisLabels`: the kHz labels along the power spectrum's bottom edge
/// (see [`freq_axis_labels`]), 2 px above it.
struct FreqAxisLabels {
    max_hz: f64,
}

impl<M> canvas::Program<M> for FreqAxisLabels {
    type State = KeyedCache<u64>;

    fn draw(
        &self,
        state: &KeyedCache<u64>,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        vec![state.draw(renderer, size, self.max_hz.to_bits(), |frame| {
            for (k, x, align) in freq_axis_labels(self.max_hz, f64::from(size.width)) {
                frame.fill_text(label_text(
                    format!("{k} kHz"),
                    Point::new(x as f32, size.height - 2.0),
                    9.0,
                    Studio::TEXT_FAINT,
                    align,
                    Vertical::Bottom,
                ));
            }
        })]
    }
}

/// `RotatedBox(quarterTurns: 3, child: Text(label))`, centred: the label
/// reads bottom to top.
struct RotatedLabel {
    label: &'static str,
}

impl<M> canvas::Program<M> for RotatedLabel {
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
        frame.translate(Vector::new(bounds.width / 2.0, bounds.height / 2.0));
        frame.rotate(-FRAC_PI_2);
        frame.fill_text(label_text(
            self.label.to_string(),
            Point::ORIGIN,
            10.0,
            Studio::TEXT_FAINT,
            Alignment::Center,
            Vertical::Center,
        ));
        vec![frame.into_geometry()]
    }
}

/// `_MonitorLevel`: the live input level as a record-red bar filling its
/// share of a rounded inset track (clipped to the track's rounded corners).
struct MonitorLevel {
    level: f64,
}

impl<M> canvas::Program<M> for MonitorLevel {
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
        let r = 4.0f32.min(size.height / 2.0);
        frame.fill(
            &Path::rounded_rectangle(Point::ORIGIN, size, Radius::from(r)),
            Studio::INSET,
        );
        let w = clamp_f64(self.level, 0.0, 1.0) as f32 * size.width;
        if w > 0.0 {
            // The track's rounded clip: the bar's left corners follow the
            // track's; its right ones only where it reaches the track's.
            let left = r.min(w / 2.0);
            let right = (r - (size.width - w)).clamp(0.0, left);
            frame.fill(
                &Path::rounded_rectangle(
                    Point::ORIGIN,
                    Size::new(w, size.height),
                    Radius {
                        top_left: left,
                        top_right: right,
                        bottom_right: right,
                        bottom_left: left,
                    },
                ),
                Studio::RECORD,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// `_TimbreCurvesPainter`: RING/TILT/CPP/H1−H2 over time, each auto-scaled to
/// its own range so all four are visible together (shape matters, not
/// absolute level).
struct TimbreCurvesPainter {
    metrics: Arc<VoiceMetrics>,
    timeline_duration_ms: i64,
}

/// The polylines of curve `c` of `m` in a `size` box spanning `duration_ms`:
/// one per run of finite values (a non-finite value breaks the line), each
/// point at its frame's time, the curve scaled to its own finite range with
/// 7% top/bottom padding so it doesn't touch the edges. Nothing for a flat
/// or empty curve.
pub fn timbre_curve(m: &VoiceMetrics, c: &[f32], duration_ms: i64, size: Size) -> Vec<Vec<Point>> {
    let frames = m.frames;
    if frames == 0 || duration_ms <= 0 {
        return Vec::new();
    }
    let (mut mn, mut mx) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in c {
        let v = f64::from(v);
        if v.is_finite() {
            mn = mn.min(v);
            mx = mx.max(v);
        }
    }
    // Never NaN (only finite values are taken): no range is a flat curve,
    // or none at all.
    if mx <= mn {
        return Vec::new();
    }
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    let mut runs: Vec<Vec<Point>> = Vec::new();
    let mut started = false;
    for fr in 0..frames {
        let v = value_at(c, fr);
        if !v.is_finite() {
            started = false;
            continue;
        }
        let x = clamp_f64(
            m.time_of_frame(fr as i64) * 1000.0 / duration_ms as f64,
            0.0,
            1.0,
        ) * w;
        let y = (1.0 - (v - mn) / (mx - mn)) * h * 0.86 + h * 0.07;
        if !started {
            runs.push(Vec::new());
            started = true;
        }
        if let Some(run) = runs.last_mut() {
            run.push(Point::new(x as f32, y as f32));
        }
    }
    runs
}

#[cfg(test)]
thread_local! {
    /// How many times the timbre curves and the register map recorded their
    /// geometry, on this thread.
    static STILL_LAYER_DRAWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts a still layer's recording (tests only).
fn recorded() {
    #[cfg(test)]
    STILL_LAYER_DRAWS.with(|n| n.set(n.get() + 1));
}

impl<M> canvas::Program<M> for TimbreCurvesPainter {
    type State = KeyedCache<(Same<VoiceMetrics>, i64)>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let key = (Same(Arc::clone(&self.metrics)), self.timeline_duration_ms);
        vec![state.draw(renderer, size, key, |frame| {
            recorded();
            let m = &self.metrics;
            for (curve, color) in [
                (&m.cpp, TIMBRE_CPP),
                (&m.tilt, TIMBRE_TILT),
                (&m.ring, TIMBRE_RING),
                (&m.h1h2, TIMBRE_H1H2),
            ] {
                let runs = timbre_curve(m, curve, self.timeline_duration_ms, size);
                if runs.is_empty() {
                    continue;
                }
                let path = Path::new(|b| {
                    for run in &runs {
                        let mut points = run.iter();
                        if let Some(&first) = points.next() {
                            b.move_to(first);
                        }
                        for &p in points {
                            b.line_to(p);
                        }
                    }
                });
                frame.stroke(&path, Stroke::default().with_color(color).with_width(1.4));
            }
        })]
    }
}

/// `_VibratoPainter`: the vibrato pitch-deviation waveform (cents around 0).
struct VibratoPainter {
    v: Arc<Vibrato>,
}

impl<M> canvas::Program<M> for VibratoPainter {
    type State = KeyedCache<Same<Vibrato>>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        vec![
            state.draw(renderer, size, Same(Arc::clone(&self.v)), |frame| {
                let dev = &self.v.deviation_cents;
                let n = dev.len();
                if n < 2 {
                    return;
                }
                let (w, h) = (f64::from(size.width), f64::from(size.height));
                let mid_y = h / 2.0;
                let extent = self.v.extent_cents;
                let amp = (if extent <= 0.0 { 50.0 } else { extent }) * 1.6;
                let y = |cents: f64| mid_y - clamp_f64(cents / amp, -1.0, 1.0) * (h * 0.42);
                frame.stroke(
                    &Path::line(
                        Point::new(0.0, mid_y as f32),
                        Point::new(w as f32, mid_y as f32),
                    ),
                    Stroke::default().with_color(Studio::BORDER).with_width(1.0),
                );
                let path = Path::new(|b| {
                    for (i, &c) in dev.iter().enumerate() {
                        let p = Point::new(
                            (i as f64 / (n - 1) as f64 * w) as f32,
                            y(f64::from(c)) as f32,
                        );
                        if i == 0 {
                            b.move_to(p);
                        } else {
                            b.line_to(p);
                        }
                    }
                });
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Studio::ACCENT_BRIGHT)
                        .with_width(1.5),
                );
            }),
        ]
    }
}

/// Colour steps along the chest→head gradient: finer than the eye tells
/// apart at a 1.4 px dot.
const REGISTER_SHADES: usize = 32;

/// One batch of the register map: the dots of one shade, in frame order.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisterBatch {
    pub color: Color,
    pub dots: Vec<Point>,
}

/// The dots `_RegisterMapPainter` draws in a `size` box: each voiced frame
/// (an F0 and a register score) at its pitch (semitone Y between `lo_semi`
/// and `hi_semi`) and time, coloured by its register score along the
/// chest→head gradient — one batch per shade, from chest to head. A batch is
/// one fill: a 10-minute take is ~56k voiced frames, and one circle each (a
/// paint and a colour lerp each) every paint of the tab — every frame of a
/// dock resize — cost tens of ms to record.
pub fn register_batches(
    m: &VoiceMetrics,
    register: &RegisterMap,
    lo_semi: i64,
    hi_semi: i64,
    duration_ms: i64,
    size: Size,
) -> Vec<RegisterBatch> {
    let frames = m.frames;
    if frames == 0 || duration_ms <= 0 {
        return Vec::new();
    }
    let span = (hi_semi - lo_semi).clamp(1, 1 << 30) as f64;
    let (w, h) = (f64::from(size.width), f64::from(size.height));
    let mut by_shade: Vec<Vec<Point>> = vec![Vec::new(); REGISTER_SHADES];
    for fr in 0..frames {
        let hz = value_at(&m.f0, fr);
        let s = value_at(&register.score, fr);
        if hz.is_nan() || hz <= 0.0 || s.is_nan() {
            continue;
        }
        let midi = 69.0 + 12.0 * ((hz / 440.0).ln() / LN_2);
        // Time-based x (matches the playhead), not fr / frames — the frame
        // count is ~fft_size / hop short of len / hop, which would drift the
        // dots right.
        let x = clamp_f64(
            m.time_of_frame(fr as i64) * 1000.0 / duration_ms as f64,
            0.0,
            1.0,
        ) * w;
        let y = clamp_f64(1.0 - (midi - lo_semi as f64) / span, 0.0, 1.0) * h;
        let shade = round_to_i64(clamp_f64(s, 0.0, 1.0) * (REGISTER_SHADES - 1) as f64) as usize;
        by_shade[shade].push(Point::new(x as f32, y as f32));
    }
    by_shade
        .into_iter()
        .enumerate()
        .filter(|(_, dots)| !dots.is_empty())
        .map(|(shade, dots)| RegisterBatch {
            color: lerp_color(
                CHEST_COLOR,
                HEAD_COLOR,
                shade as f32 / (REGISTER_SHADES - 1) as f32,
            ),
            dots,
        })
        .collect()
}

/// `_RegisterMapPainter`: see [`register_batches`]. Each dot is a
/// round-capped point 2.8 px wide: a 1.4 px circle.
struct RegisterMapPainter {
    metrics: Arc<VoiceMetrics>,
    register: Arc<RegisterMap>,
    lo_semi: i64,
    hi_semi: i64,
    duration_ms: i64,
}

type RegisterKey = (Same<VoiceMetrics>, Same<RegisterMap>, i64, i64, i64);

impl<M> canvas::Program<M> for RegisterMapPainter {
    type State = KeyedCache<RegisterKey>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        let key = (
            Same(Arc::clone(&self.metrics)),
            Same(Arc::clone(&self.register)),
            self.lo_semi,
            self.hi_semi,
            self.duration_ms,
        );
        vec![state.draw(renderer, size, key, |frame| {
            recorded();
            for batch in register_batches(
                &self.metrics,
                &self.register,
                self.lo_semi,
                self.hi_semi,
                self.duration_ms,
                size,
            ) {
                let path = Path::new(|b| {
                    for &p in &batch.dots {
                        b.circle(p, 1.4);
                    }
                });
                frame.fill(&path, batch.color);
            }
        })]
    }
}

/// `_TunerDialPainter`: the tuner's ±50¢ dial — baseline, the ±12¢ in-tune
/// zone, ticks every 25¢ and the needle at `cents`.
struct TunerDialPainter {
    cents: f64,
    color: Color,
}

impl<M> canvas::Program<M> for TunerDialPainter {
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
        let mid_y = size.height / 2.0;
        let w = size.width;
        let x_of = |c: f64| ((clamp_f64(c, -50.0, 50.0) + 50.0) / 100.0) as f32 * w;
        let line =
            |frame: &mut Frame, from: Point, to: Point, color: Color, width: f32, round: bool| {
                let mut stroke = Stroke::default().with_color(color).with_width(width);
                if round {
                    stroke = stroke.with_line_cap(LineCap::Round);
                }
                frame.stroke(&Path::line(from, to), stroke);
            };
        // Baseline.
        line(
            &mut frame,
            Point::new(0.0, mid_y),
            Point::new(w, mid_y),
            Studio::GRID_STRONG,
            2.0,
            true,
        );
        // In-tune green zone (±12¢).
        line(
            &mut frame,
            Point::new(x_of(-12.0), mid_y),
            Point::new(x_of(12.0), mid_y),
            theme::alpha(TUNER_GREEN, 0.55),
            5.0,
            true,
        );
        // Ticks at −50/−25/0/+25/+50.
        for c in [-50.0, -25.0, 0.0, 25.0, 50.0] {
            let x = x_of(c);
            let centre = c == 0.0;
            let h = if centre { 11.0 } else { 6.0 };
            line(
                &mut frame,
                Point::new(x, mid_y - h),
                Point::new(x, mid_y + h),
                if centre {
                    Studio::TEXT_SECONDARY
                } else {
                    Studio::TEXT_FAINT
                },
                if centre { 2.0 } else { 1.0 },
                false,
            );
        }
        // Needle.
        let nx = x_of(self.cents);
        line(
            &mut frame,
            Point::new(nx, mid_y - 15.0),
            Point::new(nx, mid_y + 15.0),
            self.color,
            3.0,
            true,
        );
        frame.fill(&Path::circle(Point::new(nx, mid_y), 4.5), self.color);
        vec![frame.into_geometry()]
    }
}

/// Where the register meter's marker starts in a `width`-wide bar (it is
/// 3 px wide, centred on the score and kept inside the bar); `None` for a
/// NaN score.
pub fn register_marker_left(score: f64, width: f32) -> Option<f32> {
    if score.is_nan() {
        return None;
    }
    let x = clamp_f64(score, 0.0, 1.0) as f32 * width - 1.5;
    Some(x.max(0.0).min(width - 3.0))
}

/// The register meter's bar and marker: the bar 14 px tall, radius 7, a
/// chest→head gradient; the marker 3 × 20 px, radius 2, reaching
/// [`METER_OVERHANG`] above and below the bar.
struct RegisterMeterPainter {
    score: f64,
}

impl<M> canvas::Program<M> for RegisterMeterPainter {
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
        let top = METER_OVERHANG;
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(0.0, top),
                Size::new(size.width, 14.0),
                Radius::from(7.0),
            ),
            gradient::Linear::new(Point::new(0.0, top), Point::new(size.width, top))
                .add_stop(0.0, CHEST_COLOR)
                .add_stop(1.0, HEAD_COLOR),
        );
        if let Some(left) = register_marker_left(self.score, size.width) {
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(left, 0.0),
                    Size::new(3.0, 14.0 + 2.0 * METER_OVERHANG),
                    Radius::from(2.0),
                ),
                Studio::TEXT_PRIMARY,
            );
        }
        vec![frame.into_geometry()]
    }
}
