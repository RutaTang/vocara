//! Port of `ui/song_practice_detail/widgets/analysis_lanes.dart`: the
//! fold-out analysis area under a vocal take — a toolbar toggling which
//! sub-lanes show, then those lanes (pitch, loudness, spectrogram, power,
//! notes, techniques), all sharing the timeline and the header-column width
//! with the parent track.
//!
//! Every lane takes plain data (the take, the viewport, the playhead, the
//! analysis) and reports [`Message`]s named after what the Dart widget
//! called: the analysis cache's `toggleLane` / `healLane`, and the screen's
//! `onAddNote` / `onEditNote` / `onAddTechnique` / `onEditTechnique`
//! callbacks. The screen builds [`analysis_lanes`] from its detail view
//! model (Dart read `vm` directly) and routes the messages back.
//!
//! How the Dart structure maps:
//!
//! - The heal on build: Dart posted `vm.analysis.healLane(id)` after every
//!   build that found the lane lacking data the cache let go. An iced view
//!   cannot act while it is built, so the screen hands the lanes what
//!   `!vm.liveMonitor && laneLacksData(id)` said, with the revision it built
//!   from ([`AnalysisLanes::lacks_data`]); after the frame that shows them
//!   (the redraw, Dart's post-frame callback) the lanes report
//!   [`Message::HealLane`] once per revision — a rebuild with nothing new
//!   asks nothing, as Dart's did not rebuild then.
//! - The spectrogram lane's take image: Dart's `SpectrogramImage` widget
//!   rendered it in its own state; here the screen keeps a [`LaneImage`] per
//!   take whose spectrogram lane is on screen, [`LaneImage::sync`]s it after
//!   each update and lands its renders ([`Message::ImageRendered`]).
//! - `ValueListenable<Duration> position`: an iced view is rebuilt on every
//!   message, so each lane takes the playhead (`position_ms`) as it is when
//!   built. The still layers (the loudness envelope) keep a cache keyed by
//!   what they show — the metrics by `Arc` identity, the spans by value —
//!   never by the playhead, which is its own canvas on top: a tick redraws
//!   the playheads and the power spectrum (which follows it), nothing else.
//! - `Positioned` children of a clipping `Stack`: iced's `pin` squeezes a
//!   child into the room left of its position, so a take box or a marker
//!   reaching past the lane's edge would be squashed (and its time axis with
//!   it). The lanes place them with [`Placed`], which lays the child out at
//!   its own size wherever it lands, as Flutter did, and clip to the lane.
//! - The marker lanes' range drag lives in the lane's canvas state, in
//!   TIMES, as Dart held it in its `State`.
//!
//! Where iced differs: text has no letter spacing (set solid) and no
//! ellipsis (a marker's label is clipped at its box).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, canvas as canvas_widget, column, container, responsive, row, scrollable, stack, text,
    Space,
};
use iced::{
    mouse, window, Border, Color, Element, Event, Length, Padding, Point, Rectangle, Renderer,
    Shadow, Size, Task, Theme, Vector,
};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::spectrogram::{AnalysisFraming, Spectrogram};
use vocara_model::models::voice_metrics::VoiceMetrics;
use vocara_model::util::dart_num::{clamp_f64, round_to_i64};

use super::pitch_lane::{lane_visible_frames, pitch_lane, LANE_MARGIN_PX, PITCH_LANE_HEIGHT};
use super::track_row::TRACK_HEADER_W;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, boxed, fill, font, Studio};
use crate::ui::core::widgets::painter::{KeyedCache, Same};
use crate::ui::core::widgets::spectrogram_painter::{
    FrequencyGridPainter, PowerSpectrumPainter, RenderRequest, Rendered, SpectrogramImage,
};
use crate::ui::core::widgets::studio_hint::lane_hint;
use crate::ui::core::widgets::waveform_painter::PlayheadPainter;
use crate::ui::core::widgets::wrap::Wrap;
pub use crate::ui::song_practice_detail::view_model::analysis_cache::AnalysisLane;

#[cfg(test)]
mod tests;

/// Height of a notes / techniques marker lane.
pub const MARKER_LANE_HEIGHT: f32 = 56.0;

/// Height of the lane toggle bar.
pub const LANE_TOOLBAR_HEIGHT: f32 = 30.0;

/// The loudness lane's fixed window under the take's peak (dB).
pub const LOUDNESS_RANGE_DB: f64 = 48.0;

/// `kPrecisePointerHitSlop`: how far a mouse travels horizontally before a
/// horizontal drag recognizer accepts it.
const PRECISE_HIT_SLOP: f32 = 1.0;

/// `kTouchSlop`: how far a tap may travel before it is no tap.
const TAP_SLOP: f32 = 18.0;

/// `_laneLabels`: the toggle chip's label for each lane.
pub fn lane_label(lane: AnalysisLane) -> &'static str {
    match lane {
        AnalysisLane::Pitch => "Pitch",
        AnalysisLane::Loudness => "Loudness",
        AnalysisLane::Spectrogram => "Spectrogram",
        AnalysisLane::Power => "Power",
        AnalysisLane::Notes => "Notes",
        AnalysisLane::Technique => "Techniques",
    }
}

/// What the lanes ask of the screen, each named after the call the Dart
/// widget made.
#[derive(Debug, Clone)]
pub enum Message {
    /// A toolbar chip: `vm.analysis.toggleLane(trackId, lane)`.
    ToggleLane {
        track_id: String,
        lane: AnalysisLane,
    },
    /// The lanes are on screen lacking data the cache let go:
    /// `vm.analysis.healLane(trackId)` (see [`AnalysisLanes::lacks_data`]).
    HealLane(String),
    /// A tap (`end_ms: None`, a point) or a drag (the range, in time order)
    /// on the notes lane: `onAddNote(startMs, endMs)`.
    AddNote { start_ms: i64, end_ms: Option<i64> },
    /// A note's chip (its id) was tapped: `onEditNote(note)`.
    EditNote(String),
    /// A tap or a drag on the technique lane: `onAddTechnique(startMs,
    /// endMs)`.
    AddTechnique { start_ms: i64, end_ms: Option<i64> },
    /// A technique marker's chip (its id) was tapped:
    /// `onEditTechnique(marker)`.
    EditTechnique(String),
    /// The spectrogram lane's take image finished rendering; hand it to the
    /// take's [`LaneImage::rendered`].
    ImageRendered(Rendered),
}

// ------------------------------------------------------------- geometry

/// Absolute timeline ms → x in a lane `width` wide showing `dur` ms from
/// `view_start` (`xOf`; 0 with no timeline).
pub fn lane_x_of(ms: i64, dur: i64, view_start: i64, width: f64) -> f64 {
    if dur <= 0 {
        0.0
    } else {
        (ms - view_start) as f64 / dur as f64 * width
    }
}

/// x in a lane → absolute timeline ms (`msOf`; 0 with no timeline).
pub fn lane_ms_of(x: f64, dur: i64, view_start: i64, width: f64) -> i64 {
    if dur <= 0 {
        0
    } else {
        round_to_i64(view_start as f64 + x / width * dur as f64)
    }
}

/// `xOf(take start)` and the take box's width (at least a pixel): where the
/// spectrogram lane places the take, whose image maps the take's own time
/// over that box.
pub fn take_box(
    track_start_ms: i64,
    track_duration_ms: i64,
    dur: i64,
    view_start: i64,
    width: f64,
) -> (f64, f64) {
    let start_x = lane_x_of(track_start_ms, dur, view_start, width);
    let end_x = lane_x_of(
        track_start_ms.wrapping_add(track_duration_ms),
        dur,
        view_start,
        width,
    );
    let w = end_x - start_x;
    (start_x, if w < 1.0 { 1.0 } else { w })
}

/// The held drag's range in time order (`_dragRange`). A right-to-left
/// drag must reach `onAdd` (and the editors, whose header tells a range from
/// a point by end > start) as the same range a left-to-right one would.
pub fn ordered_range(start_ms: i64, cur_ms: i64) -> (i64, i64) {
    if start_ms < cur_ms {
        (start_ms, cur_ms)
    } else {
        (cur_ms, start_ms)
    }
}

/// The live range-selection highlight's left and width for a held range,
/// drawn from the times so a page flip carries it with the content (clamped
/// to the box once part of it scrolls out).
pub fn drag_highlight(range: (i64, i64), dur: i64, view_start: i64, width: f64) -> (f64, f64) {
    let left = clamp_f64(lane_x_of(range.0, dur, view_start, width), 0.0, width);
    let right = clamp_f64(lane_x_of(range.1, dur, view_start, width), 0.0, width);
    // Dart's clamp(1.0, w) throws on a lane narrower than a pixel; it never
    // gets a drag there.
    let w = if width < 1.0 {
        width.max(0.0)
    } else {
        clamp_f64(right - left, 1.0, width)
    };
    (left, w)
}

/// Where a marker sits on its lane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkerBox {
    /// A range bar from `left` (clamped into the viewport, so the icon and
    /// label anchored at its left stay visible when the range begins
    /// off-screen), at least 22 px wide.
    Range { left: f64, width: f64 },
    /// A content-sized point pill from `left`.
    Point { left: f64 },
}

/// `_marker`: the box of a marker from `start_ms` (to `end_ms` for a
/// range) on a lane `width` wide, or `None` when it lies wholly outside the
/// view (beyond a 20 px margin).
pub fn marker_box(
    start_ms: i64,
    end_ms: Option<i64>,
    dur: i64,
    view_start: i64,
    width: f64,
) -> Option<MarkerBox> {
    match end_ms {
        Some(end) if end > start_ms => {
            let xs = lane_x_of(start_ms, dur, view_start, width);
            let xe = lane_x_of(end, dur, view_start, width);
            if xe < -20.0 || xs > width + 20.0 {
                return None;
            }
            let left = clamp_f64(xs, 0.0, width.max(0.0));
            Some(MarkerBox::Range {
                left,
                width: (xe - left).max(22.0),
            })
        }
        _ => {
            let x = lane_x_of(start_ms, dur, view_start, width);
            if x < -20.0 || x > width + 20.0 {
                return None;
            }
            Some(MarkerBox::Point { left: x })
        }
    }
}

// ------------------------------------------------------- the take image

/// The spectrogram lane's take image, which the screen keeps per take while
/// its spectrogram lane is on screen (Dart's `SpectrogramImage` held it in
/// its own state).
#[derive(Default)]
pub struct LaneImage {
    image: SpectrogramImage,
    /// The spectrogram the image is (or is being rendered) for.
    shown: Option<Arc<Spectrogram>>,
}

impl LaneImage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Brings the image up to `spectrogram`, the one the lane shows (`None`
    /// while it loads, without one, or once the lane is folded in: the image
    /// goes, as Dart's went with its widget). Call it after every update
    /// while the take's lanes are on screen, and run the render it returns;
    /// [`Message::ImageRendered`] brings it back.
    pub fn sync(&mut self, spectrogram: Option<&Arc<Spectrogram>>) -> Task<Message> {
        match self.sync_request(spectrogram) {
            Some(request) => request.into_task(Message::ImageRendered),
            None => Task::none(),
        }
    }

    /// [`Self::sync`], handing back the render to run instead of a task.
    pub fn sync_request(
        &mut self,
        spectrogram: Option<&Arc<Spectrogram>>,
    ) -> Option<RenderRequest> {
        let Some(spec) = spectrogram else {
            *self = Self::default();
            return None;
        };
        self.shown = Some(Arc::clone(spec));
        self.image.show(spec)
    }

    /// Lands a render ([`Message::ImageRendered`]); a stale one is dropped.
    pub fn rendered(&mut self, done: Rendered) {
        self.image.rendered(done);
    }

    /// Whether the image (rendered or rendering) is `spec`'s.
    pub fn shows(&self, spec: &Arc<Spectrogram>) -> bool {
        self.shown.as_ref().is_some_and(|s| Arc::ptr_eq(s, spec))
    }
}

// ------------------------------------------------------------ the lanes

/// `AnalysisLanes(...)`: the toolbar and the lanes shown for a take. Build
/// it with [`analysis_lanes`], configure it and convert it into an
/// [`Element`].
pub struct AnalysisLanes<'a> {
    track: &'a AudioTrack,
    lanes: BTreeSet<AnalysisLane>,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
    metrics_loading: bool,
    metrics: Option<Arc<VoiceMetrics>>,
    spectrogram_loading: bool,
    spectrogram: Option<Arc<Spectrogram>>,
    image: Option<&'a LaneImage>,
    lacks_data: Option<u64>,
}

/// The lanes of `track` (`vm.analysis.lanesFor(id)`: `lanes`) on a lane
/// showing `timeline_duration_ms` of the timeline, with the playhead at
/// `position_ms` (absolute). From 0, no analysis, nothing lacking until
/// configured.
pub fn analysis_lanes(
    track: &AudioTrack,
    lanes: BTreeSet<AnalysisLane>,
    timeline_duration_ms: i64,
    position_ms: i64,
) -> AnalysisLanes<'_> {
    AnalysisLanes {
        track,
        lanes,
        timeline_duration_ms,
        view_start_ms: 0,
        position_ms,
        metrics_loading: false,
        metrics: None,
        spectrogram_loading: false,
        spectrogram: None,
        image: None,
        lacks_data: None,
    }
}

impl<'a> AnalysisLanes<'a> {
    /// `vm.playback.viewStartMs`.
    pub fn view_start_ms(mut self, ms: i64) -> Self {
        self.view_start_ms = ms;
        self
    }

    /// `metricsLoadingFor(id)` and `metricsFor(id)` (the pitch, loudness and
    /// power lanes).
    pub fn metrics(mut self, loading: bool, metrics: Option<Arc<VoiceMetrics>>) -> Self {
        self.metrics_loading = loading;
        self.metrics = metrics;
        self
    }

    /// `spectrogramLoadingFor(id)` and `spectrogramFor(id)` (the spectrogram
    /// and power lanes), with the take's image (see [`LaneImage`]).
    pub fn spectrogram(
        mut self,
        loading: bool,
        spectrogram: Option<Arc<Spectrogram>>,
        image: &'a LaneImage,
    ) -> Self {
        self.spectrogram_loading = loading;
        self.spectrogram = spectrogram;
        self.image = Some(image);
        self
    }

    /// `!vm.liveMonitor && vm.analysis.laneLacksData(id)`, and the revision
    /// of the screen the lanes are built from (bumped on every change it
    /// notifies): while it holds, the lanes report [`Message::HealLane`]
    /// after the frame that shows them, once per revision.
    pub fn lacks_data(mut self, lacks: bool, revision: u64) -> Self {
        self.lacks_data = lacks.then_some(revision);
        self
    }
}

impl<'a> From<AnalysisLanes<'a>> for Element<'a, Message> {
    fn from(l: AnalysisLanes<'a>) -> Self {
        let track = l.track;
        let id = track.id.clone();
        let dur = l.timeline_duration_ms;
        let view_start = l.view_start_ms;
        let position = l.position_ms;
        let mut col = column![lane_toolbar(&l.lanes, &id)].width(Length::Fill);
        if l.lanes.contains(&AnalysisLane::Pitch) {
            col = col.push(
                pitch_lane(
                    track,
                    dur,
                    position,
                    TRACK_HEADER_W,
                    l.metrics_loading,
                    l.metrics.clone(),
                )
                .view_start_ms(view_start),
            );
        }
        if l.lanes.contains(&AnalysisLane::Loudness) {
            col = col.push(loudness_lane(
                track,
                dur,
                view_start,
                position,
                l.metrics_loading,
                l.metrics.clone(),
            ));
        }
        if l.lanes.contains(&AnalysisLane::Spectrogram) {
            col = col.push(spectrogram_lane(
                track,
                dur,
                view_start,
                position,
                l.spectrogram_loading,
                l.spectrogram.clone(),
                l.image,
            ));
        }
        if l.lanes.contains(&AnalysisLane::Power) {
            col = col.push(power_lane(
                track,
                position,
                l.spectrogram_loading,
                l.spectrogram.as_deref(),
                l.metrics.as_deref(),
            ));
        }
        if l.lanes.contains(&AnalysisLane::Notes) {
            col = col.push(notes_lane(track, dur, view_start, position));
        }
        if l.lanes.contains(&AnalysisLane::Technique) {
            col = col.push(technique_lane(track, dur, view_start, position));
        }
        heal_after_frame(col.into(), id, l.lacks_data)
    }
}

/// Reports [`Message::HealLane`] after a frame shows the lanes lacking data,
/// once per revision (Dart's `addPostFrameCallback` on each build).
fn heal_after_frame<'a>(
    content: Element<'a, Message>,
    track_id: String,
    lacks: Option<u64>,
) -> Element<'a, Message> {
    Wrap::<Message, Option<u64>>::new(
        content,
        Box::new(move |asked, event, _, _, shell| {
            let Some(revision) = lacks else {
                return;
            };
            if matches!(event, Event::Window(window::Event::RedrawRequested(_)))
                && *asked != Some(revision)
            {
                *asked = Some(revision);
                shell.publish(Message::HealLane(track_id.clone()));
            }
        }),
    )
    .into()
}

// ---------------------------------------------------------- the toolbar

/// `_LaneToolbar`: a chip per [`AnalysisLane`], lit when that lane is shown;
/// a tap toggles it ([`Message::ToggleLane`]).
pub fn lane_toolbar<'a>(active: &BTreeSet<AnalysisLane>, track_id: &str) -> Element<'a, Message> {
    let mut chips = row![];
    for l in AnalysisLane::ALL {
        chips = chips.push(
            container(lane_chip(
                lane_label(l),
                active.contains(&l),
                Message::ToggleLane {
                    track_id: track_id.to_string(),
                    lane: l,
                },
            ))
            .padding(Padding {
                right: 6.0,
                ..Padding::ZERO
            }),
        );
    }
    let label = container(
        text("LANES")
            .size(9)
            .font(font::BOLD)
            .color(Studio::TEXT_FAINT),
    )
    .padding(Padding {
        left: 30.0,
        ..Padding::ZERO
    })
    .width(TRACK_HEADER_W)
    .height(Length::Fill)
    .align_y(Vertical::Center);
    // Scroll rather than overflow if the center area is dragged narrow.
    let strip = scrollable(
        container(chips)
            .padding([0, 8])
            .height(Length::Fill)
            .align_y(Vertical::Center),
    )
    .direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(0).scroller_width(0),
    ))
    .width(Length::Fill)
    .height(Length::Fill);
    column![
        container(row![label, strip].height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(Studio::SURFACE)),
        rule(true),
    ]
    .width(Length::Fill)
    .height(LANE_TOOLBAR_HEIGHT)
    .into()
}

/// A toolbar chip: coral-washed and outlined when on.
fn lane_chip<'a>(label: &str, on: bool, on_tap: Message) -> Element<'a, Message> {
    let face = container(
        text(label.to_string())
            .size(11)
            .font(font::SEMIBOLD)
            .color(if on {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::TEXT_SECONDARY
            })
            .wrapping(Wrapping::None),
    )
    // 4 × 10, inside the 1 px border (a decoration's border pads its child).
    .padding([5, 11])
    .style(move |_| {
        boxed(
            Some(if on {
                alpha(Studio::ACCENT, 0.18)
            } else {
                Studio::SURFACE_RAISED
            }),
            5.0,
            Some(if on {
                alpha(Studio::ACCENT, 0.6)
            } else {
                Studio::BORDER
            }),
        )
    });
    button(face)
        .padding(0)
        .style(crate::ui::core::theme::bare_button)
        .on_press(on_tap)
        .into()
}

// ------------------------------------------------------------ the shell

/// A 1 px line in `Studio.border`: one side of a `Border(...)` box.
fn rule<'a>(horizontal: bool) -> Element<'a, Message> {
    let line = container(Space::new()).style(fill(Studio::BORDER));
    if horizontal {
        line.width(Length::Fill).height(1).into()
    } else {
        line.width(1).height(Length::Fill).into()
    }
}

/// `_laneShell`: an indented header column (`label`) and the lane `body`,
/// sized and bordered like the other lanes. The bottom border takes its
/// pixel from `height`, as a decoration's border pads its child.
fn lane_shell<'a>(label: &str, height: f32, body: Element<'a, Message>) -> Element<'a, Message> {
    let header = container(row![
        container(
            text(label.to_string())
                .size(10)
                .font(font::BOLD)
                .color(Studio::TEXT_FAINT)
                .wrapping(Wrapping::None),
        )
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
    .width(TRACK_HEADER_W)
    .height(Length::Fill)
    .style(fill(Studio::PANEL));
    column![
        row![
            header,
            container(body)
                .width(Length::Fill)
                .height(Length::Fill)
                .clip(true)
        ]
        .height(Length::Fill),
        rule(true),
    ]
    .width(Length::Fill)
    .height(height)
    .into()
}

/// `ColoredBox(Studio.inset)` filling its slot.
fn inset_fill<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(fill(Studio::INSET))
        .into()
}

/// `PlayheadLayer(position, timelineDurationMs, startMs: viewStartMs, color:
/// Studio.playhead)`: its own canvas, taking no input.
fn playhead_layer<'a>(position_ms: i64, dur: i64, view_start: i64) -> Element<'a, Message> {
    canvas_widget(PlayheadPainter {
        position_ms,
        timeline_duration_ms: dur,
        color: Studio::PLAYHEAD,
        show_head: false,
        start_ms: view_start,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

// ----------------------------------------------------- spectrogram lane

/// The hint of a spectrogram lane whose take has no spectrogram.
pub const NO_SPECTROGRAM_HINT: &str = "Spectrogram is available for recorded WAV takes.";

/// The hint of a power lane whose take has no spectrogram.
pub const NO_POWER_HINT: &str = "Power spectrum is available for recorded WAV takes.";

/// The hint of a loudness lane whose take has no metrics.
pub const NO_LOUDNESS_HINT: &str = "Loudness is available for recorded WAV takes.";

/// A lane's hint while its analysis computes.
pub const ANALYZING_HINT: &str = "Analyzing…";

/// `SpectrogramLane`: a take's spectrogram, time-aligned under its waveform.
/// `image` is the take's [`LaneImage`] (nothing is drawn until it renders,
/// nor while it is another spectrogram's).
pub fn spectrogram_lane<'a>(
    track: &AudioTrack,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
    loading: bool,
    spectrogram: Option<Arc<Spectrogram>>,
    image: Option<&'a LaneImage>,
) -> Element<'a, Message> {
    let body = if loading {
        lane_hint(ANALYZING_HINT)
    } else if let Some(spec) = spectrogram {
        let dur = timeline_duration_ms;
        let (track_start, track_dur) = (track.start_ms(), track.duration_ms());
        // The image of another spectrogram must not stand in for this one.
        let image = image.filter(|i| i.shows(&spec));
        responsive(move |size| {
            let (left, width) = take_box(
                track_start,
                track_dur,
                dur,
                view_start_ms,
                f64::from(size.width),
            );
            let take_image = match image {
                Some(i) => i.image.view(track_dur),
                None => Space::new().width(Length::Fill).height(Length::Fill).into(),
            };
            // This box spans the take, so the image places its columns on
            // the take's own time axis inside it.
            let take = container(stack![
                take_image,
                canvas_widget(FrequencyGridPainter::new(spec.max_hz))
                    .width(Length::Fill)
                    .height(Length::Fill),
            ])
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true);
            Element::from(
                container(stack![
                    inset_fill(),
                    Placed::new(take, left as f32, 0.0, 0.0).width(width as f32),
                    playhead_layer(position_ms, dur, view_start_ms),
                ])
                .width(Length::Fill)
                .height(Length::Fill)
                .clip(true),
            )
        })
        .into()
    } else {
        lane_hint(NO_SPECTROGRAM_HINT)
    };
    lane_shell("SPECTROGRAM", PITCH_LANE_HEIGHT, body)
}

// ---------------------------------------------------------- power lane

/// The column and formants the power lane draws for the playhead at
/// `position_ms` over a take placed at `track_start_ms` for
/// `track_duration_ms` (`_PlayheadPowerPainter.paint`). Outside the take
/// nothing is playing: an empty column (an empty spectrum) and no formants,
/// rather than the take's first/last frame (a stale edge).
pub fn playhead_power(
    position_ms: i64,
    track_start_ms: i64,
    track_duration_ms: i64,
    spectrogram: &Spectrogram,
    metrics: Option<&VoiceMetrics>,
) -> (Arc<[f32]>, Vec<f64>) {
    let ms = position_ms.wrapping_sub(track_start_ms);
    let in_span = ms >= 0 && ms <= track_duration_ms;
    let seconds = ms as f64 / 1000.0;
    let mut formants = Vec::new();
    if let Some(m) = metrics.filter(|m| in_span && m.frames > 0) {
        let fr = m.frame_at_time(seconds);
        let at = |c: &[f32]| c.get(fr).map_or(f64::NAN, |&v| f64::from(v));
        formants = vec![at(&m.f1), at(&m.f2), at(&m.f3)];
    }
    let column: Arc<[f32]> = if in_span {
        Arc::from(spectrogram.column_at_time(seconds))
    } else {
        Arc::from(Vec::new())
    };
    (column, formants)
}

/// `PowerLane`: the instantaneous power spectrum at the playhead (frequency
/// X-axis — not time-aligned, like the dock's), with the take's formants
/// marked. It follows the playhead: each view draws the column under it (the
/// rest of the lanes keep their caches; see the module docs).
pub fn power_lane<'a>(
    track: &AudioTrack,
    position_ms: i64,
    loading: bool,
    spectrogram: Option<&Spectrogram>,
    metrics: Option<&VoiceMetrics>,
) -> Element<'a, Message> {
    let body = if loading {
        lane_hint(ANALYZING_HINT)
    } else if let Some(spec) = spectrogram {
        let (column, formants) = playhead_power(
            position_ms,
            track.start_ms(),
            track.duration_ms(),
            spec,
            metrics,
        );
        let mut painter = PowerSpectrumPainter::new(column, spec.bin_hz(), spec.max_hz);
        painter.formants = formants;
        container(
            canvas_widget(painter)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(fill(Studio::INSET))
        .into()
    } else {
        lane_hint(NO_POWER_HINT)
    };
    lane_shell("POWER SPECTRUM", PITCH_LANE_HEIGHT, body)
}

// -------------------------------------------------------- loudness lane

/// `LoudnessLane`: the take's loudness (RMS dBFS) envelope — the dynamics /
/// phrasing curve, time-aligned under the waveform.
pub fn loudness_lane<'a>(
    track: &AudioTrack,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
    loading: bool,
    metrics: Option<Arc<VoiceMetrics>>,
) -> Element<'a, Message> {
    let body = match metrics {
        _ if loading => lane_hint(ANALYZING_HINT),
        Some(m) if m.frames > 0 => stack![
            inset_fill(),
            LoudnessLanePainter {
                metrics: m,
                timeline_duration_ms,
                start_ms: track.start_ms(),
                view_start_ms,
            }
            .view(),
            playhead_layer(position_ms, timeline_duration_ms, view_start_ms),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
        _ => lane_hint(NO_LOUDNESS_HINT),
    };
    lane_shell("LOUDNESS", PITCH_LANE_HEIGHT, body)
}

// Per-take memo of the loudest finite frame (Dart's `Expando`): found once
// per take rather than on every paint, which a pan, a zoom or a follow page
// flip asks of each loudness view (some 56k frames for a 10-minute take).
// Metrics are never mutated once computed (see the memos in pitch_lane), so
// identity is the key, held weakly so the entry goes with its take.
static PEAK_MEMO: Mutex<Vec<(Weak<VoiceMetrics>, Option<f64>)>> = Mutex::new(Vec::new());

/// The loudest finite frame of `m`'s loudness, or `None`: computed once per
/// take.
pub fn loudness_peak_of(m: &Arc<VoiceMetrics>) -> Option<f64> {
    let mut table = PEAK_MEMO.lock().unwrap_or_else(|e| e.into_inner());
    table.retain(|(k, _)| k.strong_count() > 0);
    if let Some((_, peak)) = table
        .iter()
        .find(|(k, _)| std::ptr::addr_eq(k.as_ptr(), Arc::as_ptr(m)))
    {
        return *peak;
    }
    let peak = loudness_peak(m);
    table.push((Arc::downgrade(m), peak));
    peak
}

/// The loudest finite frame (uncached).
fn loudness_peak(m: &VoiceMetrics) -> Option<f64> {
    let mut peak: Option<f64> = None;
    for &v in &m.loudness {
        let v = f64::from(v);
        if v.is_finite() && peak.is_none_or(|p| v > p) {
            peak = Some(v);
        }
    }
    peak
}

/// What [`LoudnessLanePainter`] draws: the filled areas (each a closed
/// polygon, down to the lane's bottom at either end) and the envelope's
/// top-line runs. One of each per run of finite frames: a multi-clip take's
/// aggregate metrics carry NaN frames in the gaps between clips, which must
/// break the envelope instead of feeding NaN points into it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoudnessShape {
    pub areas: Vec<Vec<(f64, f64)>>,
    pub lines: Vec<Vec<(f64, f64)>>,
}

/// `LoudnessLanePainter`: the loudness envelope as a filled area and a top
/// line, auto-ranged to the take's peak over a fixed
/// [`LOUDNESS_RANGE_DB`] window. `start_ms` places the take's frames on the
/// timeline; `view_start_ms` is the absolute time at the viewport's left.
#[derive(Debug, Clone)]
pub struct LoudnessLanePainter {
    pub metrics: Arc<VoiceMetrics>,
    pub timeline_duration_ms: i64,
    pub start_ms: i64,
    pub view_start_ms: i64,
}

impl LoudnessLanePainter {
    /// The shape in a `size` box; `None` when it draws nothing (no
    /// timeline, no frames, nothing finite in view or at all).
    pub fn shape(&self, size: Size) -> Option<LoudnessShape> {
        let dur = self.timeline_duration_ms;
        let m = self.metrics.as_ref();
        if dur <= 0 || m.frames == 0 {
            return None;
        }
        let peak = loudness_peak_of(&self.metrics)?;
        let floor = peak - LOUDNESS_RANGE_DB;
        let (w, h) = (f64::from(size.width), f64::from(size.height));
        let x_of = |ms: f64| (ms - self.view_start_ms as f64) / dur as f64 * w;
        let y_of = |db: f64| (1.0 - clamp_f64((db - floor) / LOUDNESS_RANGE_DB, 0.0, 1.0)) * h;

        let mut shape = LoudnessShape::default();
        let mut area: Vec<(f64, f64)> = Vec::new();
        let mut line: Vec<(f64, f64)> = Vec::new();
        let mut last_x = 0.0;
        let close = |area: &mut Vec<(f64, f64)>,
                     line: &mut Vec<(f64, f64)>,
                     last_x: f64,
                     shape: &mut LoudnessShape| {
            if area.is_empty() {
                return;
            }
            area.push((last_x, h));
            shape.areas.push(std::mem::take(area));
            shape.lines.push(std::mem::take(line));
        };
        // Only the frames that can reach the (clipped) box: zoomed in on a
        // long take, the path would otherwise run through every frame of it.
        let window =
            lane_visible_frames(m, self.start_ms, self.view_start_ms, dur, w, LANE_MARGIN_PX);
        let mut fr = window.first;
        while fr <= window.last {
            let db = m
                .loudness
                .get(fr as usize)
                .map_or(f64::NAN, |&v| f64::from(v));
            if !db.is_finite() {
                close(&mut area, &mut line, last_x, &mut shape);
                fr += 1;
                continue;
            }
            let x = x_of(self.start_ms as f64 + m.time_of_frame(fr) * 1000.0);
            let y = y_of(db);
            if area.is_empty() {
                area.push((x, h));
            }
            area.push((x, y));
            line.push((x, y));
            last_x = x;
            fr += 1;
        }
        close(&mut area, &mut line, last_x, &mut shape);
        if shape.lines.is_empty() {
            return None;
        }
        Some(shape)
    }

    /// The painter as a canvas filling the parent, its drawing cached until
    /// what it shows changes (`shouldRepaint`).
    pub fn view<'a, M: 'a>(self) -> Element<'a, M> {
        canvas_widget(self)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Draws the shape into `frame` (which clips to its box: zoomed in, most
    /// of the envelope maps outside it, and would paint over the
    /// track-header column; see the pitch lane).
    fn draw_into(&self, frame: &mut Frame, size: Size) {
        let Some(shape) = self.shape(size) else {
            return;
        };
        let polyline = |runs: &[Vec<(f64, f64)>], close: bool| {
            Path::new(|b| {
                for run in runs {
                    let mut points = run.iter();
                    if let Some(&(x, y)) = points.next() {
                        b.move_to(Point::new(x as f32, y as f32));
                    }
                    for &(x, y) in points {
                        b.line_to(Point::new(x as f32, y as f32));
                    }
                    if close {
                        b.close();
                    }
                }
            })
        };
        frame.fill(&polyline(&shape.areas, true), alpha(Studio::ACCENT, 0.18));
        frame.stroke(
            &polyline(&shape.lines, false),
            Stroke::default()
                .with_color(alpha(Studio::ACCENT, 0.85))
                .with_width(1.5),
        );
    }
}

#[cfg(test)]
thread_local! {
    /// How many times a loudness envelope recorded its geometry, on this
    /// thread.
    pub(crate) static LOUDNESS_RECORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a loudness layer was drawn for (`shouldRepaint`).
type LoudnessKey = (Same<VoiceMetrics>, [i64; 3]);

impl<M> canvas::Program<M> for LoudnessLanePainter {
    type State = KeyedCache<LoudnessKey>;

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
            [self.timeline_duration_ms, self.start_ms, self.view_start_ms],
        );
        let size = bounds.size();
        vec![state.draw(renderer, size, key, |frame| {
            #[cfg(test)]
            LOUDNESS_RECORDS.with(|n| n.set(n.get() + 1));
            self.draw_into(frame, size);
        })]
    }
}

// --------------------------------------------------------- marker lanes

/// Visual styling for a marker lane (`_MarkerStyle`: notes are neutral;
/// techniques are coral).
#[derive(Debug, Clone, Copy)]
pub struct MarkerStyle {
    pub icon: Icon,
    pub icon_color: Color,
    pub fill: Color,
    pub border: Color,
    pub label_color: Color,
    pub label_font: iced::Font,
}

impl MarkerStyle {
    /// `_MarkerStyle.note`.
    pub const NOTE: MarkerStyle = MarkerStyle {
        icon: Icon::StickyNote,
        icon_color: Studio::ACCENT,
        fill: Studio::SURFACE_RAISED,
        border: Studio::BORDER,
        label_color: Studio::TEXT_SECONDARY,
        label_font: font::REGULAR,
    };

    /// `_MarkerStyle.technique`.
    pub const TECHNIQUE: MarkerStyle = MarkerStyle {
        icon: Icon::Waypoints,
        icon_color: Studio::ACCENT,
        fill: alpha(Studio::ACCENT, 0.14),
        border: alpha(Studio::ACCENT, 0.4),
        label_color: Studio::ACCENT_BRIGHT,
        label_font: font::SEMIBOLD,
    };
}

/// One marker to render on a marker lane (`_LaneItem`) — a point (when
/// `end_ms` is `None`) or a range. Decouples the lane from the note and
/// technique models.
#[derive(Debug, Clone)]
pub struct LaneItem {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub label: String,
    pub on_tap: Message,
}

impl LaneItem {
    pub fn is_range(&self) -> bool {
        self.end_ms.is_some_and(|e| e > self.start_ms)
    }
}

/// `NotesLane`: a dedicated marker track for the take's notes — point
/// pills / range bars at their timeline positions (tap one to edit,
/// [`Message::EditNote`]). Tap empty space to add a point at that spot; drag
/// across the lane to add a note spanning that range
/// ([`Message::AddNote`]).
pub fn notes_lane<'a>(
    track: &AudioTrack,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
) -> Element<'a, Message> {
    let items = track
        .notes
        .iter()
        .map(|n| LaneItem {
            start_ms: n.position_ms,
            end_ms: n.end_ms,
            label: n.content.clone(),
            on_tap: Message::EditNote(n.id.clone()),
        })
        .collect();
    marker_lane(
        "NOTES",
        MarkerStyle::NOTE,
        items,
        timeline_duration_ms,
        view_start_ms,
        position_ms,
        Lane::Notes,
    )
}

/// `TechniqueLane`: a sub-lane mirroring [`notes_lane`] whose markers
/// reference techniques from the library. Tap empty space to add one (after
/// picking) at that spot; drag to add one spanning a range
/// ([`Message::AddTechnique`]); tap a marker to edit or remove it
/// ([`Message::EditTechnique`]).
pub fn technique_lane<'a>(
    track: &AudioTrack,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
) -> Element<'a, Message> {
    let items = track
        .technique_markers
        .iter()
        .map(|m| LaneItem {
            start_ms: m.position_ms,
            end_ms: m.end_ms,
            label: m.technique_name.clone(),
            on_tap: Message::EditTechnique(m.id.clone()),
        })
        .collect();
    marker_lane(
        "TECHNIQUES",
        MarkerStyle::TECHNIQUE,
        items,
        timeline_duration_ms,
        view_start_ms,
        position_ms,
        Lane::Technique,
    )
}

/// Which lane a marker lane is: what its taps and drags add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Notes,
    Technique,
}

impl Lane {
    /// `onAdd(startMs, endMs)`.
    fn add(self, start_ms: i64, end_ms: Option<i64>) -> Message {
        match self {
            Lane::Notes => Message::AddNote { start_ms, end_ms },
            Lane::Technique => Message::AddTechnique { start_ms, end_ms },
        }
    }
}

/// `_MarkerLane`: renders `items` as point pills / range bars over the
/// timeline, with a playhead overlay. Tapping the empty background adds a
/// point at the tapped spot; dragging horizontally adds a marker spanning
/// the drag (with a live selection highlight).
fn marker_lane<'a>(
    label: &str,
    style: MarkerStyle,
    items: Vec<LaneItem>,
    dur: i64,
    view_start: i64,
    position_ms: i64,
    lane: Lane,
) -> Element<'a, Message> {
    let body = responsive(move |size| {
        let w = f64::from(size.width);
        let mut layers: Vec<Element<'a, Message>> = vec![
            inset_fill(),
            canvas_widget(MarkerGestures {
                dur,
                view_start,
                lane,
                color: style.icon_color,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        ];
        for it in &items {
            let Some(b) = marker_box(it.start_ms, it.end_ms, dur, view_start, w) else {
                continue;
            };
            let placed = match b {
                MarkerBox::Range { left, width } => Placed::new(
                    marker_chip(style, &it.label, it.on_tap.clone(), true),
                    left as f32,
                    6.0,
                    6.0,
                )
                .width(width as f32),
                MarkerBox::Point { left } => Placed::new(
                    marker_chip(style, &it.label, it.on_tap.clone(), false),
                    left as f32,
                    6.0,
                    6.0,
                ),
            };
            layers.push(placed.into());
        }
        layers.push(playhead_layer(position_ms, dur, view_start));
        Element::from(
            container(iced::widget::Stack::with_children(layers))
                .width(Length::Fill)
                .height(Length::Fill)
                .clip(true),
        )
    });
    lane_shell(label, MARKER_LANE_HEIGHT, body.into())
}

/// `_MarkerChip`: a content-sized point pill, or (when `fill`) a bar that
/// stretches to its placed width to show a range span; a tap reports
/// `on_tap`.
fn marker_chip<'a>(
    style: MarkerStyle,
    label: &str,
    on_tap: Message,
    fill: bool,
) -> Element<'a, Message> {
    let label_text = text(label.to_string())
        .size(11)
        .font(style.label_font)
        .color(style.label_color)
        .wrapping(Wrapping::None);
    // A point pill is bounded so a long label is cut; a range bar gives the
    // label whatever width the span leaves it.
    let label_box: Element<'a, Message> = if fill {
        container(label_text).width(Length::Fill).clip(true).into()
    } else {
        container(label_text).max_width(150).clip(true).into()
    };
    let content = row![
        icons::icon(style.icon, 11.0, style.icon_color),
        Space::new().width(5),
        label_box,
    ]
    .align_y(Vertical::Center);
    let face = container(content)
        // 4 × 7 (a bar) or 8 (a pill), inside the 1 px border.
        .padding([5.0, if fill { 8.0 } else { 9.0 }])
        .height(Length::Fill)
        .width(if fill { Length::Fill } else { Length::Shrink })
        .align_y(Vertical::Center)
        .clip(true)
        .style(move |_| boxed(Some(style.fill), 4.0, Some(style.border)));
    button(face)
        .padding(0)
        .width(if fill { Length::Fill } else { Length::Shrink })
        .height(Length::Fill)
        .style(|_: &Theme, _| button::Style {
            background: None,
            text_color: Studio::TEXT_PRIMARY,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        })
        .on_press(on_tap)
        .into()
}

/// The background gesture of a marker lane (`GestureDetector(onTapUp,
/// onHorizontalDrag…)`), drawing the live range highlight. The drag is held
/// in TIMES (ms) — taken at the view start of the moment each was touched.
/// Not pixels converted on release: follow-cursor can page the view while
/// the drag is held, and the release-time view start would then commit a
/// range the user never dragged over (and draw the highlight there too).
struct MarkerGestures {
    dur: i64,
    view_start: i64,
    lane: Lane,
    color: Color,
}

/// A press on the lane's background in progress.
#[derive(Debug, Default)]
pub(crate) struct MarkerDrag {
    /// Where the press landed (lane-local).
    down: Option<Point>,
    /// Moved past the tap slop: no tap on release.
    tap_lost: bool,
    /// The times under the drag's start and its current pointer, once the
    /// press became a horizontal drag.
    range: Option<(i64, i64)>,
}

impl MarkerGestures {
    fn ms_at(&self, x: f32, width: f32) -> i64 {
        let w = f64::from(width);
        lane_ms_of(
            clamp_f64(f64::from(x), 0.0, w.max(0.0)),
            self.dur,
            self.view_start,
            w,
        )
    }
}

impl canvas::Program<Message> for MarkerGestures {
    type State = MarkerDrag;

    fn update(
        &self,
        state: &mut MarkerDrag,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            // Only the primary button and a mouse: a two-finger trackpad
            // swipe reaches iced as a wheel scroll, and pans the timeline —
            // taken as a range drag it opened the editor for a range nobody
            // drew.
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let at = cursor.position_in(bounds)?;
                // A press whose release never came is let go (Dart's
                // onHorizontalDragCancel), never committed.
                *state = MarkerDrag {
                    down: Some(at),
                    ..MarkerDrag::default()
                };
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let down = state.down?;
                // The pointer is the drag's wherever it goes (over a marker
                // too, which takes no drag).
                let at = cursor.land().position_from(bounds.position())?;
                if let Some((start, _)) = state.range {
                    state.range = Some((start, self.ms_at(at.x, bounds.width)));
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if at.distance(down) > TAP_SLOP {
                    state.tap_lost = true;
                }
                if (at.x - down.x).abs() > PRECISE_HIT_SLOP {
                    // DragStartBehavior.start: the drag starts where it was
                    // recognized.
                    let ms = self.ms_at(at.x, bounds.width);
                    state.range = Some((ms, ms));
                    state.tap_lost = true;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                None
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let down = state.down.take()?;
                let range = state.range.take();
                let tap_lost = std::mem::take(&mut state.tap_lost);
                if let Some((start, cur)) = range {
                    let (s, e) = ordered_range(start, cur);
                    return Some(canvas::Action::publish(self.lane.add(s, Some(e))).and_capture());
                }
                if tap_lost {
                    return Some(canvas::Action::capture());
                }
                let up = cursor
                    .land()
                    .position_from(bounds.position())
                    .unwrap_or(down);
                Some(
                    canvas::Action::publish(self.lane.add(self.ms_at(up.x, bounds.width), None))
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &MarkerDrag,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let Some((start, cur)) = state.range else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let (left, width) = drag_highlight(
            ordered_range(start, cur),
            self.dur,
            self.view_start,
            f64::from(bounds.width),
        );
        let (left, width) = (left as f32, width as f32);
        frame.fill_rectangle(
            Point::new(left, 0.0),
            Size::new(width, bounds.height),
            alpha(self.color, 0.16),
        );
        // `Border.all`: a 1 px line along the inside of the box.
        frame.stroke_rectangle(
            Point::new(left + 0.5, 0.5),
            Size::new((width - 1.0).max(0.0), (bounds.height - 1.0).max(0.0)),
            Stroke::default()
                .with_color(alpha(self.color, 0.5))
                .with_width(1.0),
        );
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------------ placement

/// `Positioned(left, top, bottom, width?)` in a clipping `Stack`: lays its
/// child out `top`..`bottom` inset vertically, `width` wide (or at its own
/// width), at `left` — wherever that lands, past either edge included — and
/// fills its parent itself. The pointer reaches the child only inside the
/// parent's box (Flutter hit-tested a stack's children only within it).
pub(crate) struct Placed<'a> {
    content: Element<'a, Message>,
    left: f32,
    top: f32,
    bottom: f32,
    width: Option<f32>,
}

impl<'a> Placed<'a> {
    pub(crate) fn new(
        content: impl Into<Element<'a, Message>>,
        left: f32,
        top: f32,
        bottom: f32,
    ) -> Self {
        Self {
            content: content.into(),
            left,
            top,
            bottom,
            width: None,
        }
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The cursor the child sees: none outside the parent's box.
    fn cursor(cursor: mouse::Cursor, bounds: Rectangle) -> mouse::Cursor {
        if cursor.is_over(bounds) {
            cursor
        } else {
            cursor.levitate()
        }
    }
}

impl Widget<Message, Theme, Renderer> for Placed<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let height = (size.height - self.top - self.bottom).max(0.0);
        let child_limits = match self.width {
            Some(w) => {
                let w = w.max(0.0);
                layout::Limits::new(Size::new(w, height), Size::new(w, height))
            }
            None => layout::Limits::new(Size::new(0.0, height), Size::new(f32::INFINITY, height)),
        };
        let node = self
            .content
            .as_widget_mut()
            .layout(tree, renderer, &child_limits)
            .move_to(Point::new(self.left, self.top));
        layout::Node::with_children(size, vec![node])
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else {
            return;
        };
        self.content.as_widget().draw(
            tree,
            renderer,
            theme,
            style,
            child,
            Self::cursor(cursor, layout.bounds()),
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(tree, child, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Some(child) = layout.children().next() else {
            return;
        };
        self.content.as_widget_mut().update(
            tree,
            event,
            child,
            Self::cursor(cursor, layout.bounds()),
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let Some(child) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        self.content.as_widget().mouse_interaction(
            tree,
            child,
            Self::cursor(cursor, layout.bounds()),
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let child = layout.children().next()?;
        self.content
            .as_widget_mut()
            .overlay(tree, child, renderer, viewport, translation)
    }
}

impl<'a> From<Placed<'a>> for Element<'a, Message> {
    fn from(placed: Placed<'a>) -> Self {
        Element::new(placed)
    }
}
