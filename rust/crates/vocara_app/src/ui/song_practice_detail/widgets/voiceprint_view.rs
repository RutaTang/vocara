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
//! - The spectrograms' `ClipRRect(borderRadius: 6)` (the take's and the
//!   live one's wells): iced clips to rectangles, so the panel draws its
//!   four corners outside the rounded rectangle over everything in it, in
//!   the colour its host lays behind it ([`VoiceprintState::with_backdrop`];
//!   the dock's [`BACKDROP`] unless set): what the clip let show of the
//!   backdrop there ([`RoundedCorners`]), the corners' antialiasing the
//!   clip's complement.
//!
//! The look is `DESIGN.md`'s: each tab a sentence-case 11 px header with
//! its legend (small swatches and 11 px text) at the right, readouts of an
//! 11 px label over an 18 px tabular value with its 11 px unit, and its
//! chart in a `WELL` plot well (radius 6, an inset outline); axis labels
//! 10 px tabular in `TEXT_FAINT` / `TEXT_DISABLED`; data in the muted
//! track and status colours, the accent kept for meaning.
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

use super::left_panel::split_unit;
use super::live_spectrogram::live_spectrogram;
use super::pitch_lane::{note_vibrato_of, pitch_lane, pitch_notes_of, pitch_range_of};
use crate::ui::core::theme::{self, boxed, fill, font, type_scale, Studio};
use crate::ui::core::widgets::cap_label::cap_label;
use crate::ui::core::widgets::controls::{labelled, meter_color};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::painter::{KeyedCache, Same};
use crate::ui::core::widgets::spectrogram_painter::{
    FormantTrackPainter, FrequencyGridPainter, LiveFormantPainter, PowerSpectrumPainter,
    RenderRequest, Rendered, SpectrogramImage, VowelSpacePainter, F0_COLOR, FORMANT_COLORS,
    SINGERS_FORMANT_BAND, SPECTRUM_ENVELOPE, SPECTRUM_FILL,
};
use crate::ui::core::widgets::studio_hint::studio_hint;
use crate::ui::core::widgets::studio_text::{studio_text, tabular_font};
use crate::ui::core::widgets::waveform_painter::PlayheadPainter;
use crate::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use crate::util::to_string_as_fixed;

#[cfg(test)]
mod tests;

/// The width of the frequency axis left of the spectra; the tab headers are
/// indented past it.
const AXIS_WIDTH: f32 = 36.0;

// Per-metric colors for the Timbre tab's time curves: the track colours,
// muted and of one lightness.
const TIMBRE_RING: Color = Studio::TRACK_OCHRE;
const TIMBRE_TILT: Color = Studio::TRACK_PERIWINKLE;
const TIMBRE_CPP: Color = Studio::TRACK_SAGE;
const TIMBRE_H1H2: Color = Studio::TRACK_CORAL;

const CHEST_COLOR: Color = Studio::REGISTER_CHEST; // warm = chest
const HEAD_COLOR: Color = Studio::REGISTER_HEAD; // cool = head

/// In-tune color for the live tuner (green); off-pitch ramps toward the
/// out-of-tune orange.
const TUNER_GREEN: Color = Studio::IN_TUNE;
const TUNER_OFF: Color = Studio::OUT_OF_TUNE;

/// A chart's data line where it is not a formant or a register: neutral
/// (`DESIGN.md`: the accent is for meaning, not data).
const DATA_LINE: Color = Color::from_rgb8(0xC4, 0xC4, 0xCE);

/// What the dock lays behind the tabs (its content background), which a
/// rounded well's corners show: see [`RoundedCorners`].
pub const BACKDROP: Color = super::bottom_dock::CONTENT_BACKGROUND;

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
/// image, and the colour its host lays behind the tabs. The host keeps one
/// per place it shows the tab (the dock, the technique page).
pub struct VoiceprintState {
    image: SpectrogramImage,
    /// The spectrogram [`Self::image`] shows, so a view can tell whether it
    /// is the source's.
    shown: Option<Arc<Spectrogram>>,
    /// What shows around the spectrogram wells' rounded corners: the host's
    /// background ([`BACKDROP`], the dock's, unless set).
    backdrop: Color,
}

impl Default for VoiceprintState {
    fn default() -> Self {
        Self {
            image: SpectrogramImage::default(),
            shown: None,
            backdrop: BACKDROP,
        }
    }
}

impl VoiceprintState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A host that lays another colour than the dock's behind the tabs (a
    /// card) says so: the wells' rounded corners show it.
    pub fn with_backdrop(mut self, backdrop: Color) -> Self {
        self.backdrop = backdrop;
        self
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
        self.image = SpectrogramImage::default();
        self.shown = None;
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
        return live_spectrogram_panel(src.monitor(), state.backdrop);
    }
    let backdrop = state.backdrop;
    with_spectrogram(src, |spec, metrics| {
        column![
            tab_header("Spectrogram", Some(formant_legend())),
            axis_row(
                spec.max_hz,
                rounded_well(take_spectro_stack(src, state, &spec, metrics), backdrop),
            ),
        ]
        .padding(TAB_PADDING)
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
            }
        };
        // Live, as the other tabs mark it, and with a word until the mic
        // has given a spectrum (an empty plot said nothing).
        let trailing = row![live_dot(), spectrum_legend(&strip.formants)]
            .spacing(14)
            .align_y(Vertical::Center);
        let body = if strip.column.is_empty() {
            well_hint(POWER_LIVE_HINT)
        } else {
            spectrum_strip(strip)
        };
        return padded(
            column![tab_header("Power spectrum", Some(trailing.into())), body],
            TAB_PADDING,
        );
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
        };
        padded(
            column![
                tab_header("Power spectrum", Some(spectrum_legend(&strip.formants))),
                spectrum_strip(strip),
            ],
            TAB_PADDING,
        )
    })
}

/// `VowelTab`: the F1×F2 vowel chart (take playhead or live).
pub fn vowel_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let (f1, f2) = {
            let m = src.monitor().read();
            (m.current_f1(), m.current_f2())
        };
        return vowel_panel(f1, f2, true);
    }
    with_metrics(src, |m, _| {
        let (f1, f2) = vowel_formants(
            src.position(),
            src.analysis_duration_ms(),
            src.analysis_start_ms(),
            &m,
        );
        vowel_panel(f1, f2, false)
    })
}

/// `PitchTab`: pitch — the live tuner, or the take's Logic-style pitch map.
pub fn pitch_tab<'a>(src: &'a dyn AnalysisSource) -> Element<'a, Message> {
    if src.live_monitor() {
        let tuner = live_tuner(&src.monitor().read());
        return padded(
            column![plain_header("Tuner", Some(live_dot())), tuner],
            TAB_PADDING,
        );
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
                plain_header("Timbre", Some(live_dot())),
                bar,
                Space::new().height(12),
                well_hint("Timbre curves are available for recorded takes."),
            ],
            TAB_PADDING,
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
            plain_header("Timbre", Some(timbre_legend())),
            metrics_bar(position, &m, span_ms, start_ms),
            Space::new().height(12),
            plot_well(curves),
        ]
        .padding(TAB_PADDING)
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
            // Titled and marked Live as it will be, the hint in its well (a
            // bare hint, untitled, read as another state).
            _ => padded(
                column![
                    tab_header(
                        LTAS_LIVE_TITLE,
                        Some(
                            row![live_dot(), spectrum_legend(&[])]
                                .spacing(14)
                                .align_y(Vertical::Center)
                                .into()
                        ),
                    ),
                    well_hint(LTAS_LIVE_HINT),
                ],
                TAB_PADDING,
            ),
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
    // Guarded: Dart's round() threw on NaN / Infinity, which swapped the
    // whole tab for an error widget; an unmeasurable stat reads as a dash
    // like the dock's other readouts.
    let stats = row![
        readout(Readout::new(
            "Singer's formant",
            db_or_dash(ltas.singers_formant_db)
        )),
        readout(Readout::new("Brightness α", db_or_dash(ltas.alpha_ratio))),
    ]
    .spacing(READOUT_GAP);
    let strip = SpectrumStrip {
        column: Arc::from(ltas.db.as_slice()),
        bin_hz: ltas.bin_hz,
        max_hz: ltas.max_hz,
        formants: Vec::new(),
        min_ceil_db: None,
    };
    let trailing: Element<'a, Message> = if live {
        row![live_dot(), spectrum_legend(&[])]
            .spacing(14)
            .align_y(Vertical::Center)
            .into()
    } else {
        spectrum_legend(&[])
    };
    column![
        tab_header(
            if live {
                LTAS_LIVE_TITLE
            } else {
                "LTAS · average spectrum"
            },
            Some(trailing),
        ),
        container(stats).padding(ltrb(AXIS_WIDTH, 0.0, 0.0, 10.0)),
        spectrum_strip(strip),
    ]
    .padding(TAB_PADDING)
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
    let header = vibrato_header(v.as_deref());
    let content: Element<'a, Message> = match v {
        Some(v) if v.deviation_cents.len() > 4 => canvas_widget(VibratoPainter { v })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        _ => center(
            text(empty_hint.to_string())
                .size(type_scale::BODY)
                .color(Studio::TEXT_FAINT),
        )
        .into(),
    };
    let trailing: Option<Element<'a, Message>> = live.then(live_dot);
    column![
        plain_header("Vibrato", trailing),
        header,
        Space::new().height(12),
        plot_well(content),
    ]
    .padding(TAB_PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn vibrato_header<'a>(v: Option<&Vibrato>) -> Element<'a, Message> {
    let v = match v {
        Some(v) if v.present => v,
        _ => {
            return readout(Readout::new("Vibrato", "None".to_string()).sub("straight tone"));
        }
    };
    let healthy = v.rate_hz >= 5.0 && v.rate_hz <= 7.0;
    let mut stats = Row::new().spacing(READOUT_GAP).align_y(Vertical::Top);
    stats = stats
        .push(readout(
            Readout::new("Rate", format!("{} Hz", to_string_as_fixed(v.rate_hz, 1)))
                .status(if healthy { TUNER_GREEN } else { TUNER_OFF }),
        ))
        .push(readout(Readout::new(
            "Extent",
            format!("±{}¢", round_to_i64(v.extent_cents)),
        )))
        .push(readout(Readout::new(
            "Regularity",
            format!("{}%", round_to_i64(v.regularity * 100.0)),
        )));
    if !v.jitter.is_nan() {
        stats = stats.push(readout(
            Readout::new("Jitter", format!("{}%", round_to_i64(v.jitter * 100.0))).status(
                if v.jitter <= 0.1 {
                    TUNER_GREEN
                } else {
                    TUNER_OFF
                },
            ),
        ));
    }
    if !v.onset_ms.is_nan() {
        stats = stats.push(readout(Readout::new(
            "Onset",
            format!("{} ms", round_to_i64(v.onset_ms)),
        )));
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
        let swatches = register_legend(
            round_to_i64(reg.chest_pct),
            round_to_i64(reg.mix_pct),
            round_to_i64(reg.head_pct),
        );
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
            plain_header("Register over time", Some(swatches)),
            plot_well(map),
        ]
        .padding(TAB_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
}

/// The chest / mix / head key: a swatch and its share each, in the
/// register colours.
fn register_legend<'a>(chest: i64, mix: i64, head: i64) -> Element<'a, Message> {
    legend(vec![
        (Swatch::Dot(CHEST_COLOR), format!("Chest {chest}%")),
        (
            Swatch::Dot(lerp_color(CHEST_COLOR, HEAD_COLOR, 0.5)),
            format!("Mix {mix}%"),
        ),
        (Swatch::Dot(HEAD_COLOR), format!("Head {head}%")),
    ])
}

/// `SourceToggle(live: ..., onSelect: ...)`: the dock's Take | Live switch,
/// drawn as `DESIGN.md`'s segmented control at 20 px (a strip's height): a
/// `SEGMENTED` well with an inset outline, the selected segment
/// `SEGMENT_SELECTED` in `TEXT_PRIMARY` medium. Live marks itself with a
/// small `RECORD` dot while it is on (the mic is live). A segment's tap is
/// [`Message::SelectLive`], the selected one's too (as Dart's did).
pub fn source_toggle<'a>(live: bool) -> Element<'a, Message> {
    container(
        row![
            toggle_segment("Take", !live, false, Message::SelectLive(false)),
            toggle_segment("Live", live, live, Message::SelectLive(true)),
        ]
        .align_y(Vertical::Center),
    )
    .padding(2)
    .height(SOURCE_TOGGLE_HEIGHT)
    .style(|_: &Theme| {
        boxed(
            Some(Studio::SEGMENTED),
            theme::radius::WELL,
            Some(Studio::CONTROL_ACTIVE),
        )
    })
    .into()
}

/// The Take/Live toggle's height (a segmented control inside a strip).
const SOURCE_TOGGLE_HEIGHT: f32 = 20.0;

fn toggle_segment<'a>(
    label: &'static str,
    selected: bool,
    live_dot: bool,
    on_tap: Message,
) -> Element<'a, Message> {
    let mut face = row![].spacing(5).align_y(Vertical::Center);
    if live_dot {
        face = face.push(dot(Studio::RECORD, 6.0));
    }
    face = face.push(
        studio_text(label)
            .size(theme::type_scale::LABEL)
            .line_height(Pixels(theme::type_scale::LINE))
            .font(if selected {
                font::MEDIUM
            } else {
                font::REGULAR
            }),
    );
    button(container(face).center_y(Length::Fill))
        .padding(Padding::from([0, 9]))
        .height(SOURCE_TOGGLE_HEIGHT - 4.0)
        .style(move |_: &Theme, status| button::Style {
            background: selected.then_some(Background::Color(Studio::SEGMENT_SELECTED)),
            text_color: if selected
                || matches!(status, button::Status::Hovered | button::Status::Pressed)
            {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_SECONDARY
            },
            border: Border {
                radius: Radius::from(theme::radius::SMALL),
                ..Border::default()
            },
            shadow: Shadow::default(),
            snap: true,
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
            .style(fill(Studio::WELL)),
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
/// Live mode) — the Live badge, the input level and the legend over the
/// rolling spectrogram and its formant overlay, in a rounded well whose
/// corners show `backdrop`.
fn live_spectrogram_panel<'a>(
    monitor: &'a MonitorService,
    backdrop: Color,
) -> Element<'a, Message> {
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
    let trailing = row![
        live_dot(),
        canvas_widget(MonitorLevel { level }).width(64).height(4),
        Space::new().width(6),
        formant_legend(),
    ]
    .spacing(10)
    .align_y(Vertical::Center);
    let panel = stack![
        live_spectrogram(monitor),
        canvas_widget(overlay)
            .width(Length::Fill)
            .height(Length::Fill),
    ];
    column![
        tab_header("Spectrogram", Some(trailing.into())),
        // `ClipRRect(borderRadius: BorderRadius.circular(6))` over both.
        axis_row(max_hz, rounded_well(panel, backdrop)),
    ]
    .padding(TAB_PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The spectrogram wells' rounded clip radius (`DESIGN.md`'s wells).
const LIVE_CLIP_RADIUS: f32 = 6.0;

/// `content` as a rounded well: clipped round ([`RoundedCorners`] in
/// `backdrop`, what lies around it) and outlined by an inset 1 px
/// `BORDER_STRONG` line, as `theme::well` draws a well that can clip.
fn rounded_well<'a>(
    content: impl Into<Element<'a, Message>>,
    backdrop: Color,
) -> Element<'a, Message> {
    stack![
        content.into(),
        canvas_widget(RoundedCorners {
            radius: LIVE_CLIP_RADIUS,
            backdrop,
        })
        .width(Length::Fill)
        .height(Length::Fill),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| boxed(None, LIVE_CLIP_RADIUS, Some(Studio::BORDER_STRONG))),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

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
        "Chest"
    } else if score < 0.6 {
        "Mix"
    } else {
        "Head"
    };
    if total == 0 && !has_now {
        // Titled and marked Live as it will be, the hint in its well.
        return padded(
            column![
                plain_header("Register", Some(live_dot())),
                well_hint(REGISTER_LIVE_HINT),
            ],
            TAB_PADDING,
        );
    }
    let pct = |n: usize| round_to_i64(n as f64 * inv);
    let swatches = row![
        live_dot(),
        register_legend(
            pct(m.register_chest()),
            pct(m.register_mix()),
            pct(m.register_head()),
        ),
    ]
    .spacing(14)
    .align_y(Vertical::Center);
    let label_color = if has_now {
        lerp_color(CHEST_COLOR, HEAD_COLOR, clamp_f64(score, 0.0, 1.0) as f32)
    } else {
        Studio::TEXT_FAINT
    };
    let readout = column![
        studio_text(label)
            .size(type_scale::READOUT + 2.0)
            .line_height(Pixels(24.0))
            .font(font::SEMIBOLD)
            .color(label_color),
        Space::new().height(2),
        studio_text(if has_now {
            format!("H1−H2  {} dB", to_string_as_fixed(h, 1))
        } else {
            "Hold a sustained note".to_string()
        })
        .size(type_scale::LABEL)
        .line_height(Pixels(type_scale::LINE))
        .tabular()
        .color(Studio::TEXT_FAINT),
        // The meter's marker reaches 3 px above its bar, so the meter's box
        // starts 3 px higher.
        Space::new().height(16.0 - METER_OVERHANG),
        register_meter(score),
    ]
    .align_x(Horizontal::Center);
    column![
        plain_header("Register", Some(swatches.into())),
        plot_well(center(readout)),
    ]
    .padding(TAB_PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// How far the register meter's marker reaches above and below its bar.
const METER_OVERHANG: f32 = 3.0;

/// The register meter's bar height.
const METER_BAR: f32 = 6.0;

/// `_RegisterMeter`: a horizontal chest↔head gradient bar with a marker at
/// the current `score` (NaN hides the marker), labelled chest / head.
fn register_meter<'a>(score: f64) -> Element<'a, Message> {
    let end = |label: &'static str| {
        studio_text(label)
            .size(type_scale::AXIS)
            .line_height(Pixels(14.0))
            .color(Studio::TEXT_DISABLED)
    };
    column![
        canvas_widget(RegisterMeterPainter { score })
            .width(Length::Fill)
            .height(METER_BAR + 2.0 * METER_OVERHANG),
        Space::new().height(6.0 - METER_OVERHANG),
        row![end("Chest"), Space::new().width(Length::Fill), end("Head")],
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
                .size(type_scale::BODY)
                .color(Studio::TEXT_FAINT),
        )
        .into(),
        Some(note) => {
            let cents = note.cents;
            let color = lerp_color(
                TUNER_GREEN,
                TUNER_OFF,
                clamp_f64(cents.abs() / 35.0, 0.0, 1.0) as f32,
            );
            row![
                container(
                    studio_text(note.name)
                        .size(type_scale::READOUT + 2.0)
                        .line_height(Pixels(24.0))
                        .font(font::SEMIBOLD)
                        .color(Studio::TEXT_PRIMARY)
                )
                .width(64),
                canvas_widget(TunerDialPainter { cents, color })
                    .width(Length::Fill)
                    .height(Length::Fill),
                container(
                    studio_text(format!(
                        "{}{}¢",
                        if cents >= 0.0 { '+' } else { '−' },
                        round_to_i64(cents.abs())
                    ))
                    .size(type_scale::READOUT)
                    .line_height(Pixels(22.0))
                    .font(font::SEMIBOLD)
                    .tabular()
                    .color(color)
                    .align_x(Horizontal::Right)
                )
                .width(64)
                .align_x(Horizontal::Right),
            ]
            .align_y(Vertical::Center)
            .height(Length::Fill)
            .into()
        }
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .max_height(120)
        .padding([10, 16])
        .style(well_style)
        .into()
}

/// `_LiveMetricsBar`: the live ring / tilt / CPP readouts with their
/// session averages. F0 is shown prominently by the tuner, so it's dropped
/// here.
fn live_metrics_bar<'a>(m: &MonitorState) -> Element<'a, Message> {
    row![
        metric_chip(
            "Ring",
            "Singer's-formant ring (3 kHz projection)",
            m.ring(),
            m.ring_avg(),
            TIMBRE_RING,
        ),
        metric_chip(
            "Tilt",
            "Spectral tilt: higher = brighter/pressed",
            m.tilt(),
            m.tilt_avg(),
            TIMBRE_TILT,
        ),
        metric_chip(
            "CPP",
            "Cepstral peak prominence: higher = cleaner",
            m.cpp(),
            m.cpp_avg(),
            TIMBRE_CPP,
        ),
    ]
    .spacing(READOUT_GAP)
    .align_y(Vertical::Top)
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
    // Outside the subject's span every readout reads "—" (NaN), and on an
    // unvoiced frame too, as the curves break there: it read silence's
    // placeholder ("Ring −100.8 dB").
    let at = |series: &[f32]| {
        t.filter(|_| m.frames > 0)
            .map_or(f64::NAN, |t| voiced_value_at(m, series, m.frame_at_time(t)))
    };
    row![
        f0_chip(at(&m.f0)),
        metric_chip(
            "Ring",
            "Singer's-formant ring (3 kHz projection): higher carries more",
            at(&m.ring),
            m.ring_avg,
            TIMBRE_RING,
        ),
        metric_chip(
            "Tilt",
            "Spectral tilt: higher = brighter/pressed, lower = darker/breathy",
            at(&m.tilt),
            m.tilt_avg,
            TIMBRE_TILT,
        ),
        metric_chip(
            "CPP",
            "Cepstral peak prominence: higher = cleaner, lower = breathier",
            at(&m.cpp),
            m.cpp_avg,
            TIMBRE_CPP,
        ),
    ]
    .spacing(READOUT_GAP)
    .align_y(Vertical::Top)
    .into()
}

/// The height of a readout (its label, value and sub-line), for its
/// tooltip's placement.
const READOUT_HEIGHT: f32 = 54.0;

/// A Material tooltip below `target`: its top 6 px below the readout.
fn with_tip<'a>(target: Element<'a, Message>, tip: &'static str) -> Element<'a, Message> {
    material_tooltip(target, tip, READOUT_HEIGHT / 2.0 + 6.0).into()
}

/// `_F0Chip`: the F0 (pitch) readout — frequency plus the equal-tempered
/// note name and cents.
fn f0_chip<'a>(hz: f64) -> Element<'a, Message> {
    let has = !hz.is_nan() && hz > 0.0;
    let mut r = Readout::new(
        "Pitch (F0)",
        if has {
            format!("{} Hz", round_to_i64(hz))
        } else {
            "—".to_string()
        },
    )
    .swatch(Swatch::Dot(F0_COLOR));
    if has {
        r = r.sub(note_label(hz));
    }
    with_tip(readout(r), "Fundamental pitch (F0) at the playhead")
}

/// `_Metric`: one timbre readout — the value at the playhead (or live) and
/// the average, keyed to its curve by `color`.
fn metric_chip<'a>(
    label: &'static str,
    tip: &'static str,
    value: f64,
    avg: f64,
    color: Color,
) -> Element<'a, Message> {
    let fmt = |v: f64| {
        if v.is_nan() {
            "—".to_string()
        } else {
            to_string_as_fixed(v, 1).replacen('-', "−", 1)
        }
    };
    let value = if value.is_nan() {
        "—".to_string()
    } else {
        format!("{} dB", to_string_as_fixed(value, 1))
    };
    with_tip(
        readout(
            Readout::new(label, value)
                .swatch(Swatch::Line(color))
                .sub(format!("avg {}", fmt(avg))),
        ),
        tip,
    )
}

// -------------------------------------------------------------- the chrome

/// The dock tabs' padding.
pub(crate) const TAB_PADDING: Padding = ltrb(16.0, 12.0, 16.0, 16.0);

/// The gap between two readouts in a row.
pub(crate) const READOUT_GAP: f32 = 28.0;

/// `EdgeInsets.fromLTRB`.
const fn ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Padding {
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

/// `_LiveDot`: a small "● Live" badge for live tab headers, in the record
/// colours.
fn live_dot<'a>() -> Element<'a, Message> {
    row![
        dot(Studio::RECORD, 6.0),
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

/// A plot well (`DESIGN.md`): `WELL`, radius 6, an inset `BORDER_STRONG`
/// outline, filling the rest of the tab.
pub(crate) fn plot_well<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(well_style)
        .into()
}

/// A live tab's word before the mic has given it anything, centred in its
/// plot well under the tab's title and Live badge (as Timbre's and the
/// tuner's).
fn well_hint<'a>(hint: &'static str) -> Element<'a, Message> {
    plot_well(center(
        text(hint).size(type_scale::BODY).color(Studio::TEXT_FAINT),
    ))
}

/// The live tabs' words while the mic has given them nothing yet.
pub const POWER_LIVE_HINT: &str = "Sing into the mic to see your power spectrum.";
pub const LTAS_LIVE_HINT: &str = "Sing into the mic to build your live average spectrum.";
pub const REGISTER_LIVE_HINT: &str = "Sing into the mic to map your registers live.";

/// The LTAS tab's title on Live.
const LTAS_LIVE_TITLE: &str = "LTAS · running average";

/// The least height the Vowel, Range and Dynamics tabs lay out in, their
/// padding included: their readouts and a usable chart.
pub(crate) const PLOT_TAB_MIN_HEIGHT: f32 = 224.0;

/// A tab `content` laid out at least [`PLOT_TAB_MIN_HEIGHT`] tall: on a
/// dock shorter than that it scrolls, as the Summary does. At the dock's
/// least height these tabs cut their readouts off or crushed their chart
/// to a line.
pub(crate) fn scrolls_when_short<'a, M: 'a>(
    content: impl Fn() -> Element<'a, M> + 'a,
) -> Element<'a, M> {
    responsive(move |size| {
        if size.height >= PLOT_TAB_MIN_HEIGHT {
            return content();
        }
        scrollable(
            container(content())
                .width(Length::Fill)
                .height(PLOT_TAB_MIN_HEIGHT),
        )
        .fading()
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
    .into()
}

/// [`plot_well`]'s look.
fn well_style(theme: &Theme) -> container::Style {
    theme::well(theme::radius::WELL)(theme)
}

/// What a legend entry's or a readout's key is: a dot (a series of dots),
/// a short line (a curve) or a block (an area).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Swatch {
    Dot(Color),
    Line(Color),
    Block(Color),
}

fn swatch<'a, M: 'a>(s: Swatch) -> Element<'a, M> {
    let (w, h, r, color) = match s {
        Swatch::Dot(c) => (6.0, 6.0, 3.0, c),
        Swatch::Line(c) => (10.0, 2.0, 1.0, c),
        Swatch::Block(c) => (8.0, 8.0, 2.0, c),
    };
    container(Space::new().width(w).height(h))
        .style(move |_: &Theme| boxed(Some(color), r, None))
        .into()
}

/// A legend (`DESIGN.md`: small swatches and 11 px text), 12 px between
/// entries.
pub(crate) fn legend<'a, M: 'a>(entries: Vec<(Swatch, String)>) -> Element<'a, M> {
    Row::with_children(entries.into_iter().map(|(s, label)| {
        row![
            swatch(s),
            studio_text(label)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_SECONDARY),
        ]
        .spacing(6)
        .align_y(Vertical::Center)
        .into()
    }))
    .spacing(12)
    .align_y(Vertical::Center)
    .into()
}

/// A readout (`DESIGN.md`): an 11 px `TEXT_SECONDARY` label (after its
/// swatch, before a verdict dot), the value 18 px semibold tabular with its
/// unit 11 px `TEXT_FAINT`, and an optional 11 px `TEXT_FAINT` sub-line.
pub(crate) struct Readout {
    label: String,
    value: String,
    sub: Option<String>,
    swatch: Option<Swatch>,
    status: Option<Color>,
    whole: bool,
}

impl Readout {
    pub(crate) fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            sub: None,
            swatch: None,
            status: None,
            whole: false,
        }
    }

    /// The value has no unit to split off ("A3 – E5"): all of it at the
    /// readout size.
    pub(crate) fn whole(mut self) -> Self {
        self.whole = true;
        self
    }

    /// A sub-line under the value.
    pub(crate) fn sub(mut self, sub: impl Into<String>) -> Self {
        self.sub = Some(sub.into());
        self
    }

    /// A key before the label (the series it reads).
    pub(crate) fn swatch(mut self, swatch: Swatch) -> Self {
        self.swatch = Some(swatch);
        self
    }

    /// A verdict dot after the label.
    pub(crate) fn status(mut self, color: Color) -> Self {
        self.status = Some(color);
        self
    }
}

/// See [`Readout`]. The value reads as a whole ("5.6 Hz") for the widget
/// tree.
pub(crate) fn readout<'a, M: 'a>(r: Readout) -> Element<'a, M> {
    let mut label = row![].spacing(6).align_y(Vertical::Center);
    if let Some(s) = r.swatch {
        label = label.push(swatch(s));
    }
    label = label.push(
        studio_text(r.label)
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .color(Studio::TEXT_SECONDARY),
    );
    if let Some(color) = r.status {
        label = label.push(swatch(Swatch::Dot(color)));
    }
    let value: Element<'a, M> = if r.whole {
        studio_text(r.value)
            .size(type_scale::READOUT)
            .line_height(Pixels(22.0))
            .font(font::SEMIBOLD)
            .tabular()
            .color(Studio::TEXT_PRIMARY)
            .into()
    } else {
        value_line(&r.value)
    };
    let mut col = column![label, value];
    if let Some(sub) = r.sub {
        col = col.push(
            studio_text(sub)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_FAINT),
        );
    }
    col.into()
}

/// A reading as `DESIGN.md`'s big readout: the number 18 px semibold
/// tabular (a typographic minus), its unit 11 px `TEXT_FAINT` after it.
fn value_line<'a, M: 'a>(value: &str) -> Element<'a, M> {
    let (number, unit) = split_unit(value);
    let shown = match number.strip_prefix('-') {
        Some(rest) => format!("−{rest}"),
        None => number.to_string(),
    };
    let plain = unit.is_empty() && shown == value;
    let mut line = row![studio_text(shown)
        .size(type_scale::READOUT)
        .line_height(Pixels(22.0))
        .font(font::SEMIBOLD)
        .tabular()
        .color(Studio::TEXT_PRIMARY)]
    .spacing(if unit == "%" || unit == "¢" { 1 } else { 3 })
    .align_y(Vertical::Bottom);
    if !unit.is_empty() {
        line = line.push(
            container(
                studio_text(unit.to_string())
                    .size(type_scale::LABEL)
                    .line_height(Pixels(type_scale::LINE))
                    .color(Studio::TEXT_FAINT),
            )
            .padding(Padding {
                bottom: 2.0,
                ..Padding::ZERO
            }),
        );
    }
    if plain {
        line.into()
    } else {
        labelled(line, value.to_string())
    }
}

/// `_TabHeader`: a tab's header row, indented past the frequency axis (see
/// [`header_row`]).
fn tab_header<'a>(label: &str, trailing: Option<Element<'a, Message>>) -> Element<'a, Message> {
    header_row(label, trailing, AXIS_WIDTH)
}

/// The header row of a tab with no frequency axis: flush with its content.
fn plain_header<'a>(label: &str, trailing: Option<Element<'a, Message>>) -> Element<'a, Message> {
    header_row(label, trailing, 0.0)
}

/// A dock tab's 24 px header row, `indent` in from the tab's padding: its
/// title (11 px semibold `TEXT_SECONDARY`, sentence case; it ellipsizes in
/// what `trailing` leaves it), then `trailing` (a legend, the live badge)
/// at its right end; 6 px above what follows.
pub(crate) fn header_row<'a, M: 'a>(
    label: &str,
    trailing: Option<Element<'a, M>>,
    indent: f32,
) -> Element<'a, M> {
    let mut header = row![container(cap_label(label).ellipsis(1)).width(Length::Fill)]
        .spacing(12)
        .align_y(Vertical::Center);
    if let Some(trailing) = trailing {
        header = header.push(trailing);
    }
    container(header)
        .height(24 + 6)
        .align_y(Vertical::Top)
        .padding(ltrb(indent, 4.0, 0.0, 6.0))
        .into()
}

/// `_FormantLegend`: the color key for the F0 and F1/F2/F3 tracks drawn on
/// the spectrogram.
fn formant_legend<'a>() -> Element<'a, Message> {
    legend(vec![
        (Swatch::Dot(F0_COLOR), "F0".to_string()),
        (Swatch::Dot(FORMANT_COLORS[0]), "F1".to_string()),
        (Swatch::Dot(FORMANT_COLORS[1]), "F2".to_string()),
        (Swatch::Dot(FORMANT_COLORS[2]), "F3".to_string()),
    ])
}

/// The power spectrum's key: the formant markers drawn now (of `formants`,
/// Hz: those found), the spectrum, its envelope and the singer's-formant
/// band. A formant not found draws no marker, and listed all the same it
/// read as missing from the chart ("F3", with no F3 line). The markers come
/// first, so the keys always there keep their place at the right as these
/// come and go.
fn spectrum_legend<'a>(formants: &[f64]) -> Element<'a, Message> {
    // The keys of the washes, as they read over a well.
    let on_well = |wash: Color| lerp_color(Studio::WELL, Color { a: 1.0, ..wash }, wash.a * 2.2);
    let mut entries: Vec<(Swatch, String)> = formants
        .iter()
        .take(3)
        .enumerate()
        // The painter's own test for a marker.
        .filter(|(_, hz)| !hz.is_nan() && **hz > 0.0)
        .map(|(i, _)| (Swatch::Line(FORMANT_COLORS[i]), format!("F{}", i + 1)))
        .collect();
    entries.push((
        Swatch::Block(on_well(SPECTRUM_FILL)),
        "Spectrum".to_string(),
    ));
    entries.push((Swatch::Line(SPECTRUM_ENVELOPE), "Envelope".to_string()));
    entries.push((
        Swatch::Block(on_well(SINGERS_FORMANT_BAND)),
        "Singer's formant".to_string(),
    ));
    legend(entries)
}

/// `_TimbreLegend`: the color key for the timbre time curves.
fn timbre_legend<'a>() -> Element<'a, Message> {
    legend(vec![
        (Swatch::Line(TIMBRE_RING), "Ring".to_string()),
        (Swatch::Line(TIMBRE_TILT), "Tilt".to_string()),
        (Swatch::Line(TIMBRE_CPP), "CPP".to_string()),
        (Swatch::Line(TIMBRE_H1H2), "H1−H2".to_string()),
    ])
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

/// `_vowelPanel`: the F1×F2 vowel chart (1.3 wide for 1 tall) as tall as the
/// tab, with the readouts at the playhead beside it, the pair centred
/// across the dock; it scrolls on a short dock ([`scrolls_when_short`]).
fn vowel_panel<'a>(f1: f64, f2: f64, live: bool) -> Element<'a, Message> {
    scrolls_when_short(move || vowel_panel_body(f1, f2, live))
}

fn vowel_panel_body<'a>(f1: f64, f2: f64, live: bool) -> Element<'a, Message> {
    let body = responsive(move |size| {
        let room = Size::new(
            (size.width - VOWEL_READOUTS_WIDTH - VOWEL_GAP).max(0.0),
            size.height,
        );
        let (w, h) = aspect_fit(room, 1.3);
        center(
            row![
                container(
                    canvas_widget(VowelSpacePainter { f1, f2 })
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .padding(10)
                .width(w)
                .height(h)
                .style(well_style),
                Space::new().width(VOWEL_GAP),
                vowel_readouts(f1, f2, h),
            ]
            .height(h),
        )
        .into()
    });
    let trailing: Option<Element<'a, Message>> = live.then(live_dot);
    column![plain_header("Vowel space · F1 × F2", trailing), body]
        .padding(TAB_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The vowel chart's readouts beside it, `height` tall: F1 and F2 at the
/// playhead (or live), and the chart's key at the foot.
fn vowel_readouts<'a>(f1: f64, f2: f64, height: f32) -> Element<'a, Message> {
    let hz = |v: f64| {
        if v.is_finite() && v > 0.0 {
            format!("{} Hz", round_to_i64(v))
        } else {
            "—".to_string()
        }
    };
    column![
        readout(Readout::new("F1", hz(f1)).sub("openness")),
        readout(Readout::new("F2", hz(f2)).sub("frontness")),
        Space::new().height(Length::Fill),
        legend(vec![
            (Swatch::Dot(Studio::TEXT_PRIMARY), "Now".to_string()),
            (Swatch::Dot(Studio::TEXT_DISABLED), "Reference".to_string()),
        ]),
    ]
    .spacing(14)
    .width(VOWEL_READOUTS_WIDTH)
    .height(height)
    .into()
}

/// The vowel readouts' column, and its gap from the chart.
const VOWEL_READOUTS_WIDTH: f32 = 140.0;
const VOWEL_GAP: f32 = 24.0;

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
}

/// `_SpectrumStrip`: a power-spectrum panel — a rotated "dB" in the axis
/// column, the spectrum and its kHz labels in a plot well.
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
    row![
        canvas_widget(RotatedLabel { label: "dB" })
            .width(AXIS_WIDTH)
            .height(Length::Fill),
        plot_well(well),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// --------------------------------------------------------------- painters

/// An axis label (`DESIGN.md`: 10 px regular, tabular) at `position`,
/// aligned as given, in `color`: Inter with tabular figures, on its own
/// line.
fn label_text(
    content: String,
    position: Point,
    color: Color,
    align_x: Alignment,
    align_y: Vertical,
) -> canvas::Text {
    canvas::Text {
        content,
        position,
        color,
        size: Pixels(type_scale::AXIS),
        line_height: text::NATURAL,
        font: tabular_font(font::REGULAR),
        align_x,
        align_y,
        ..canvas::Text::default()
    }
}

/// `_FreqAxisPainter`: the frequency axis — `1k`, `2k`, … in `TEXT_FAINT`,
/// right-aligned 6 px from its edge at each kHz's height.
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
/// (see [`freq_axis_labels`]) in `TEXT_DISABLED`, 6 px in from the well's
/// sides and 4 above its foot.
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
                let inset = match align {
                    Alignment::Left => 6.0,
                    Alignment::Right => -6.0,
                    _ => 0.0,
                };
                frame.fill_text(label_text(
                    format!("{k} kHz"),
                    Point::new(x as f32 + inset, size.height - 4.0),
                    Studio::TEXT_DISABLED,
                    align,
                    Vertical::Bottom,
                ));
            }
        })]
    }
}

/// `RotatedBox(quarterTurns: 3, child: Text(label))`, centred: the label
/// reads bottom to top, in `TEXT_DISABLED`.
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
            Studio::TEXT_DISABLED,
            Alignment::Center,
            Vertical::Center,
        ));
        vec![frame.into_geometry()]
    }
}

/// `_MonitorLevel`: the live input level as a meter bar (`DESIGN.md`'s
/// meter colours at its level: green, then warn, then record) filling its
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
                meter_color(self.level as f32),
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

/// Whether frame `fr` of `m` is voiced (audible, with a pitch): the frames
/// the take's timbre averages are taken over. The analysis measures ring,
/// tilt and CPP on every frame, silence and breath included, where they
/// read as finite extremes (a ring of −100 dB, a CPP at the floor).
fn voiced_frame(m: &VoiceMetrics, fr: usize) -> bool {
    let f0 = value_at(&m.f0, fr);
    m.active.get(fr) == Some(&1) && f0.is_finite() && f0 > 0.0
}

/// Curve `c`'s value at frame `fr` where the frame is voiced, else NaN.
fn voiced_value_at(m: &VoiceMetrics, c: &[f32], fr: usize) -> f64 {
    if voiced_frame(m, fr) {
        value_at(c, fr)
    } else {
        f64::NAN
    }
}

/// The polylines of curve `c` of `m` in a `size` box spanning `duration_ms`:
/// one per run of voiced frames with finite values (an unvoiced frame or a
/// non-finite value breaks the line: the gaps between notes drew as
/// full-height blocks), each point at its frame's time, the curve scaled
/// to its own range over those frames with 7% top/bottom padding so it
/// doesn't touch the edges. Nothing for a flat or empty curve.
pub fn timbre_curve(m: &VoiceMetrics, c: &[f32], duration_ms: i64, size: Size) -> Vec<Vec<Point>> {
    let frames = m.frames;
    if frames == 0 || duration_ms <= 0 {
        return Vec::new();
    }
    let (mut mn, mut mx) = (f64::INFINITY, f64::NEG_INFINITY);
    for fr in 0..frames {
        let v = voiced_value_at(m, c, fr);
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
        let v = voiced_value_at(m, c, fr);
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
                frame.stroke(&path, Stroke::default().with_color(color).with_width(1.2));
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
                // The ±extent guides (`GRID`), then the 0¢ line
                // (`GRID_STRONG`), each on a whole pixel.
                let line = |frame: &mut Frame, at: f64, color: Color| {
                    let at = (at as f32).round() + 0.5;
                    frame.stroke(
                        &Path::line(Point::new(0.0, at), Point::new(w as f32, at)),
                        Stroke::default().with_color(color).with_width(1.0),
                    );
                };
                if extent > 0.0 {
                    line(frame, y(extent), Studio::GRID);
                    line(frame, y(-extent), Studio::GRID);
                }
                line(frame, mid_y, Studio::GRID_STRONG);
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
                    Stroke::default().with_color(DATA_LINE).with_width(1.4),
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

/// `_TunerDialPainter`: the tuner's ±50¢ dial — a 2 px `GRID_STRONG`
/// baseline, the ±12¢ in-tune zone (`IN_TUNE`, soft), ticks every 25¢
/// (`TEXT_DISABLED`, the centre `TEXT_SECONDARY`) and the needle at
/// `cents` in its colour.
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
        let mid_y = (size.height / 2.0).round();
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
        // In-tune zone (±12¢).
        line(
            &mut frame,
            Point::new(x_of(-12.0), mid_y),
            Point::new(x_of(12.0), mid_y),
            theme::alpha(TUNER_GREEN, 0.40),
            4.0,
            true,
        );
        // Ticks at −50/−25/0/+25/+50.
        for c in [-50.0, -25.0, 0.0, 25.0, 50.0] {
            let x = x_of(c).round() + 0.5;
            let centre = c == 0.0;
            let h = if centre { 10.0 } else { 5.0 };
            line(
                &mut frame,
                Point::new(x, mid_y - h),
                Point::new(x, mid_y + h),
                if centre {
                    Studio::TEXT_SECONDARY
                } else {
                    Studio::TEXT_DISABLED
                },
                1.0,
                false,
            );
        }
        // Needle.
        let nx = x_of(self.cents);
        line(
            &mut frame,
            Point::new(nx, mid_y - 14.0),
            Point::new(nx, mid_y + 14.0),
            self.color,
            2.0,
            true,
        );
        frame.fill(&Path::circle(Point::new(nx, mid_y), 4.0), self.color);
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

/// The register meter's bar and marker: the bar [`METER_BAR`] tall, its
/// ends round, a chest→head gradient; the marker 3 px wide, radius 1.5,
/// `TEXT_PRIMARY`, reaching [`METER_OVERHANG`] above and below the bar.
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
                Size::new(size.width, METER_BAR),
                Radius::from(METER_BAR / 2.0),
            ),
            gradient::Linear::new(Point::new(0.0, top), Point::new(size.width, top))
                .add_stop(0.0, CHEST_COLOR)
                .add_stop(1.0, HEAD_COLOR),
        );
        if let Some(left) = register_marker_left(self.score, size.width) {
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(left, 0.0),
                    Size::new(3.0, METER_BAR + 2.0 * METER_OVERHANG),
                    Radius::from(1.5),
                ),
                Studio::TEXT_PRIMARY,
            );
        }
        vec![frame.into_geometry()]
    }
}
