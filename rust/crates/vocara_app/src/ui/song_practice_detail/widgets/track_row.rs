//! Port of `ui/song_practice_detail/widgets/track_row.dart`: a
//! Logic/Cubase-style track row — a fixed-width left control header (color
//! bar, name, mute/solo, volume fader, add-note / add-technique and a ⋯
//! menu) followed by the timeline lane with its clips, their waveforms, the
//! live take while recording, the lane's notes and the playhead. A clip can
//! be dragged horizontally to move it in time; a vocal lane can be
//! long-pressed and dragged onto another to reorder. Right-click anywhere
//! opens a context menu.
//!
//! The row takes plain data (the take, the viewport, what is selected) and
//! reports [`Message`]s; the screen holds what the row cannot: which menu is
//! open (it wraps the row in `track_context_menu::menu_host`, which opens
//! the menu at the press that asked for it), and which lane a reorder drag
//! carries ([`TrackRow::lane_drag`], [`TrackRow::dropping`]).
//!
//! # The gestures
//!
//! Flutter's gesture arena settled which recognizer took a pointer; the
//! row's input handlers reproduce the outcomes a mouse got:
//!
//! - A clip's move-drag starts once the pointer has moved more than a pixel
//!   horizontally (a mouse's hit slop; the movement up to then is
//!   swallowed, `DragStartBehavior.start`); each move reports the clip's new
//!   start ([`Message::ClipMoveLive`]); the release commits
//!   ([`Message::MoveCommit`]). A press that does not drag is a tap (released
//!   within the 18 px tap slop): it selects the clip, opens the note, or
//!   selects the lane. Only the primary button: a two-finger trackpad swipe
//!   reaches iced as a wheel scroll, and pans the timeline, never a clip.
//! - The drag lives in the lane's state, keyed by the clip's id, not in the
//!   clip's widget: Dart had to keep the dragged clip mounted as a 1 px
//!   sliver outside the lane (culling it tore down its recognizer, and the
//!   commit never fired; a keyless cull could transplant the drag onto the
//!   next clip). Here a clip that leaves the viewport mid-drag keeps moving
//!   and commits, whatever is drawn. A drag whose release never came (the
//!   next press arrives first) commits then: a cancelled drag has already
//!   moved the model, so skipping the commit would leave persistence and the
//!   engine silently diverged from what is on screen.
//! - A reorderable lane becomes a reorder drag after a 500 ms press held
//!   within a pixel (`LongPressDraggable` for a mouse): it reports
//!   [`Message::LaneDragStart`], the row fades to 35 % and a label follows
//!   the pointer until the release ([`Message::LaneDragEnd`]). The long
//!   press beats the row's own taps and a clip drag not yet started; a
//!   control that takes its pointer on contact (the fader, the buttons)
//!   beats it. While the screen says another lane is being dragged, a
//!   reorderable row reports the pointer entering and leaving it
//!   ([`Message::LaneDragOver`]: the screen sets [`TrackRow::dropping`],
//!   the 2 px drop line) and a release over it
//!   ([`Message::CrossLaneMove`]). The whole row — an expanded take's
//!   sub-lanes too — is the drop target; only the main row drags and
//!   highlights.
//!
//! iced lists are not lazy: a row scrolled out of the lane list mid-drag is
//! still in the tree, and the release reaches it wherever the pointer is
//! (Dart committed from `dispose`). A row the screen stops showing mid-drag
//! owes its commit to the screen.
//!
//! # Repaints
//!
//! Dart kept the header and the body as subtrees of their own
//! (`StableSubtree` + `RepaintBoundary`) so a fader tick or a pan repainted
//! only what changed. iced rebuilds the view; what is costly is the
//! waveforms, and each clip's canvas keeps a cache keyed by its peaks (by
//! identity: [`clip_peaks`] hands out one shared buffer per clip for as long
//! as its peaks are the same), its slice and its color, so a fader, pan-knob
//! or FX tick on the lane leaves every clip's waveform cached, and a pan
//! redraws them.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, mouse_area, pin, responsive, row,
    stack, text, Space,
};
use iced::{
    mouse, touch, widget, Border, Color, Element, Event, Length, Padding, Point, Rectangle,
    Renderer, Shadow, Size, Theme, Vector,
};
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::util::dart_num::round_to_i64;

use super::track_context_menu::{argb, default_take_color, selected_lane_color};
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, boxed, fill, font, Studio};
use crate::ui::core::widgets::studio_controls::{studio_fader, studio_mini_button};
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::waveform_painter::{
    visible_peak_slice, LiveWaveformPainter, PeakSlice, PlayheadPainter, TimelineGridPainter,
    WaveformPainter,
};
use crate::ui::core::widgets::wrap::Wrap;

#[cfg(test)]
mod tests;

/// Width of the per-track header column (Logic/Cubase style): name,
/// mute/solo, volume and actions live here; the waveform fills the timeline
/// to its right.
pub const TRACK_HEADER_W: f32 = 208.0;

/// A track row's height.
pub const TRACK_ROW_HEIGHT: f32 = 74.0;

/// Horizontal inset applied to the timeline column (ruler + lanes + loop
/// band) so the tracks line up with the section labels and drop-hint boxes.
pub const TIMELINE_H_MARGIN: f32 = 14.0;

/// Live recording peaks arrive one per this many ms (matches the view
/// model).
pub const RECORD_INTERVAL_MS: i64 = 50;

/// `kLongPressTimeout`: how long a still press waits to become a reorder
/// drag.
pub const LONG_PRESS_TIMEOUT: Duration = Duration::from_millis(500);

/// How far a mouse press may drift before its long press is off
/// (`kPrecisePointerHitSlop`).
const LONG_PRESS_SLOP: f32 = 1.0;

/// How far a mouse moves horizontally before a clip's move-drag wins
/// (`kPrecisePointerHitSlop`).
const DRAG_SLOP: f32 = 1.0;

/// `kTouchSlop`: how far a tap may travel before it is no tap.
const TAP_SLOP: f32 = 18.0;

// ------------------------------------------------------------- geometry

/// Absolute timeline ms → x in a lane `width` wide showing `dur` ms from
/// `view_start`.
fn x_of(ms: i64, dur: i64, view_start: i64, width: f64) -> f64 {
    if dur <= 0 {
        0.0
    } else {
        (ms - view_start) as f64 / dur as f64 * width
    }
}

/// The visible box (clamped to the window) of a span on a lane, and the
/// slice of its peaks that shows, so a clip far wider than the view stays
/// bounded and stays aligned.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleSpan {
    pub left: f64,
    pub width: f64,
    /// The peaks in view and their viewport mapping
    /// ([`visible_peak_slice`]).
    pub slice: PeakSlice,
}

/// `visible()`: the box a span (`span_start_ms` for `span_dur_ms`, with
/// `n_peaks` peaks) takes in a lane `width` wide showing `dur` ms from
/// `view_start`, or `None` when none of it is in view.
pub fn visible_span(
    dur: i64,
    view_start: i64,
    width: f64,
    span_start_ms: i64,
    span_dur_ms: i64,
    n_peaks: usize,
) -> Option<VisibleSpan> {
    // A lane laid out narrower than a pixel shows no clip (and its box clamp
    // below, 1 px up to the lane's width, would throw).
    if dur <= 0 || span_dur_ms <= 0 || width < 1.0 {
        return None;
    }
    let span_end = span_start_ms.wrapping_add(span_dur_ms);
    let view_end = view_start.wrapping_add(dur);
    let vs = span_start_ms.max(view_start);
    let ve = span_end.min(view_end);
    if ve <= vs {
        return None;
    }
    // The cut is at WHOLE peak indices while the box starts at the
    // un-quantized viewport edge, so the slice's first peak belongs up to
    // one peak interval BEFORE the box. The shared slice helper hands the
    // painter the discarded fraction (and the box width in peak units) so
    // every peak draws at its true timeline x — anchored to the ruler, not
    // the box.
    let slice = visible_peak_slice(n_peaks, span_start_ms, span_dur_ms, vs, ve);
    let left = x_of(vs, dur, view_start, width);
    Some(VisibleSpan {
        left,
        width: (x_of(ve, dur, view_start, width) - left).clamp(1.0, width),
        slice,
    })
}

/// `clipBox()`: a clip's box on the lane — [`visible_span`], or, for the
/// clip whose move-drag is in flight when it has left the view, a 1 px
/// sliver parked just outside the lane's edge, where the lane clips it
/// invisible. (Dart kept the dragged clip mounted so its recognizer lived;
/// here the drag lives in the lane, and the sliver draws nothing.)
pub fn clip_box(
    clip: &AudioClip,
    dragging: bool,
    dur: i64,
    view_start: i64,
    width: f64,
) -> Option<VisibleSpan> {
    let v = visible_span(
        dur,
        view_start,
        width,
        clip.start_ms,
        clip.duration_ms,
        clip.waveform_peaks.len(),
    );
    if v.is_some() || !dragging {
        return v;
    }
    Some(VisibleSpan {
        left: if clip.end_ms() <= view_start {
            -1.0
        } else {
            width
        },
        width: 1.0,
        slice: PeakSlice {
            range: 0..0,
            index_offset: 0.0,
            index_span: 1.0,
        },
    })
}

/// The live take's fixed pixels-per-sample: the timeline's px/ms × the
/// record interval, so the painter anchors columns to it and the waveform
/// grows without the drawn part shifting.
pub fn recording_pixels_per_sample(dur: i64, width: f64) -> f64 {
    if dur <= 0 {
        1.0
    } else {
        RECORD_INTERVAL_MS as f64 * width / dur as f64
    }
}

/// The forming take's box: `peaks` recorded from `start_ms`, one per
/// [`RECORD_INTERVAL_MS`].
pub fn recording_span(
    dur: i64,
    view_start: i64,
    width: f64,
    start_ms: i64,
    n_peaks: usize,
) -> Option<VisibleSpan> {
    visible_span(
        dur,
        view_start,
        width,
        start_ms,
        n_peaks as i64 * RECORD_INTERVAL_MS,
        n_peaks,
    )
}

/// Where a clip a move-drag picked up at `base_start_ms` lands after `dx`
/// px on a lane `lane_width` wide showing `dur` ms; `None` (no move) on a
/// lane with no timeline or no width.
pub fn dragged_start_ms(base_start_ms: i64, dx: f64, lane_width: f64, dur: i64) -> Option<i64> {
    if dur <= 0 || lane_width <= 0.0 {
        return None;
    }
    Some(base_start_ms + round_to_i64(dx / lane_width * dur as f64))
}

/// Whether a clip shows as selected: the lane is selected and either no
/// clip is (every clip on the lane highlights) or this one is.
pub fn clip_selected(lane_selected: bool, selected_clip_id: Option<&str>, clip_id: &str) -> bool {
    lane_selected && selected_clip_id.is_none_or(|id| id == clip_id)
}

/// A lane's base color: a custom tint if set; otherwise the backing shares
/// the master's signature accent, and a vocal take gets a stable per-take
/// palette color (so takes read as distinct, not a flat gray). The color
/// never changes with selection (Logic style) — selection is shown by the
/// header background and the selected clip's outer border only.
pub fn lane_accent(track: &AudioTrack) -> Color {
    if let Some(value) = track.color_value {
        argb(value)
    } else if track.is_backing() {
        Studio::ACCENT
    } else {
        default_take_color(&track.id)
    }
}

/// The note marker's box (16 × 16, centred on the note's x, 1 px off the
/// lane's bottom) in a lane `size`.
fn note_box(x: f64, size: Size) -> Rectangle {
    Rectangle::new(
        Point::new(x as f32 - 8.0, size.height - 1.0 - 16.0),
        Size::new(16.0, 16.0),
    )
}

/// Where a note marker takes a tap: its 12 px glyph, centred in the box
/// (the marker's `GestureDetector` deferred to its child, the icon).
fn note_hit_box(x: f64, size: Size) -> Rectangle {
    let b = note_box(x, size);
    Rectangle::new(Point::new(b.x + 2.0, b.y + 2.0), Size::new(12.0, 12.0))
}

/// The widget id of a clip's block on its lane (`ValueKey(clip.id)`).
pub fn clip_widget_id(clip_id: &str) -> widget::Id {
    widget::Id::from(format!("clip:{clip_id}"))
}

/// The widget id of the box a clip's waveform is painted in.
pub fn waveform_widget_id(clip_id: &str) -> widget::Id {
    widget::Id::from(format!("waveform:{clip_id}"))
}

// ------------------------------------------------------------ the peaks

/// How many clips' peaks [`clip_peaks`] remembers before it starts over.
const CLIP_PEAKS_CAP: usize = 1024;

static CLIP_PEAKS: LazyLock<Mutex<HashMap<String, Arc<[f64]>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `clip`'s peaks as a shared buffer: the same one for as long as the clip
/// (by id) has the same peaks, so its waveform's cache — keyed by the
/// buffer's identity, as Dart compared the lists — survives every rebuild
/// that leaves them alone (a fader tick copies the take and its clips), and
/// a clip whose peaks changed (a re-analysis) gets a new one.
pub fn clip_peaks(clip: &AudioClip) -> Arc<[f64]> {
    let same = |a: &[f64], b: &[f64]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    };
    let mut memo = CLIP_PEAKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(peaks) = memo.get(&clip.id) {
        if same(peaks, &clip.waveform_peaks) {
            return Arc::clone(peaks);
        }
    }
    if memo.len() >= CLIP_PEAKS_CAP {
        memo.clear();
    }
    let peaks: Arc<[f64]> = clip.waveform_peaks.as_slice().into();
    memo.insert(clip.id.clone(), Arc::clone(&peaks));
    peaks
}

// ------------------------------------------------------------- messages

/// What a track row reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A tap on the header or on the lane's empty stretch: select the lane.
    Select,
    /// A clip (its id) was tapped, to select it.
    SelectClip(String),
    /// Every tick of a clip's move-drag: the clip and its new absolute start
    /// (ms).
    ClipMoveLive {
        clip_id: String,
        start_ms: i64,
    },
    /// The move-drag ended: persist the new position.
    MoveCommit,
    /// A right-click on a clip (its id): the clip menu, at the press.
    ClipContextMenu(String),
    /// A right-click on the lane, or the ⋯ button: the lane menu, at the
    /// press.
    ContextMenu,
    /// A note marker (the note's id) was tapped, to edit it.
    EditNote(String),
    AddNote,
    AddTechnique,
    ToggleMute,
    ToggleSolo,
    /// Every volume-fader tick, with the new 0..1 value.
    Volume(f64),
    /// The volume drag ended: persist it.
    VolumeCommit,
    /// The disclosure chevron: fold the inline analysis lanes in or out.
    ToggleExpand,
    /// A long press made this lane a reorder drag: the screen marks it as
    /// the lane being dragged ([`TrackRow::lane_drag`] on every row).
    LaneDragStart,
    /// The reorder drag ended (released anywhere).
    LaneDragEnd,
    /// While another lane is dragged: whether it is over this one now
    /// ([`TrackRow::dropping`]).
    LaneDragOver(bool),
    /// Another lane (its track id) was dropped onto this one.
    CrossLaneMove(String),
}

// ---------------------------------------------------------------- the row

/// `TrackRow(...)`: build it with [`track_row`], configure it, and convert
/// it into an [`Element`].
pub struct TrackRow<'a, M> {
    track: &'a AudioTrack,
    label: String,
    timeline_duration_ms: i64,
    view_start_ms: i64,
    position_ms: i64,
    selected: bool,
    armed: bool,
    reorderable: bool,
    selected_clip_id: Option<String>,
    recording: Option<(&'a [f64], i64)>,
    can_expand: bool,
    expanded: bool,
    expandable: bool,
    sub_lanes: Option<Element<'a, M>>,
    lane_drag: Option<String>,
    dropping: bool,
    on_message: Rc<dyn Fn(Message) -> M + 'a>,
}

/// The row for `track`, headed `label`, on a lane showing
/// `timeline_duration_ms` of the timeline (the visible span; from 0 until
/// [`TrackRow::view_start_ms`]) with the playhead at `position_ms`; its
/// messages reach the screen through `on_message`. Unselected, not armed,
/// not reorderable, not expandable.
pub fn track_row<'a, M>(
    track: &'a AudioTrack,
    label: impl Into<String>,
    timeline_duration_ms: i64,
    position_ms: i64,
    on_message: impl Fn(Message) -> M + 'a,
) -> TrackRow<'a, M> {
    TrackRow {
        track,
        label: label.into(),
        timeline_duration_ms,
        view_start_ms: 0,
        position_ms,
        selected: false,
        armed: false,
        reorderable: false,
        selected_clip_id: None,
        recording: None,
        can_expand: false,
        expanded: false,
        expandable: false,
        sub_lanes: None,
        lane_drag: None,
        dropping: false,
        on_message: Rc::new(on_message),
    }
}

impl<'a, M> TrackRow<'a, M> {
    /// The absolute time (ms) at the lane's left edge: with the visible
    /// span, the timeline viewport.
    pub fn view_start_ms(mut self, ms: i64) -> Self {
        self.view_start_ms = ms;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Whether this take is the record target (REC punches into it) —
    /// shown as a small red arm dot in the lane header.
    pub fn armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }

    /// Whether this lane can be long-press dragged onto another to reorder
    /// (and accepts another dropped onto it).
    pub fn reorderable(mut self, reorderable: bool) -> Self {
        self.reorderable = reorderable;
        self
    }

    /// The selected clip on this lane (highlighted), if any.
    pub fn selected_clip_id(mut self, id: Option<&str>) -> Self {
        self.selected_clip_id = id.map(str::to_string);
        self
    }

    /// While this lane is the armed recording target: its live waveform
    /// peaks (one per [`RECORD_INTERVAL_MS`]) from `start_ms` — drawn as a
    /// forming clip.
    pub fn recording(mut self, peaks: &'a [f64], start_ms: i64) -> Self {
        self.recording = Some((peaks, start_ms));
        self
    }

    /// The inline analysis lanes (Logic-style fold-out): whether the take
    /// can fold them out (the chevron), whether they are out, and whether
    /// the chevron acts (`onToggleExpand` set).
    pub fn expand(mut self, can_expand: bool, expanded: bool, expandable: bool) -> Self {
        self.can_expand = can_expand;
        self.expanded = expanded;
        self.expandable = expandable;
        self
    }

    /// The built toolbar + lanes rendered below the row when expanded.
    pub fn sub_lanes(mut self, sub_lanes: impl Into<Element<'a, M>>) -> Self {
        self.sub_lanes = Some(sub_lanes.into());
        self
    }

    /// The lane (its track id) a reorder drag carries, if any — the same on
    /// every row.
    pub fn lane_drag(mut self, track_id: Option<&str>) -> Self {
        self.lane_drag = track_id.map(str::to_string);
        self
    }

    /// Whether a dragged lane hovers over this one (the 2 px drop line on
    /// top).
    pub fn dropping(mut self, dropping: bool) -> Self {
        self.dropping = dropping;
        self
    }
}

impl<'a, M: 'a> From<TrackRow<'a, M>> for Element<'a, M> {
    fn from(r: TrackRow<'a, M>) -> Self {
        let main = main_row(&r);
        let main = if r.reorderable {
            Element::new(LaneDrag {
                content: main,
                ghost: drag_ghost(&r.label),
            })
        } else {
            main
        };
        let f = Rc::clone(&r.on_message);
        // The main row plus any expanded sub-lanes. The whole column is the
        // drop target — so an expanded take's tall body still accepts a
        // dropped lane — while only the main row drags/highlights.
        let mut content = column![main.map(move |m| f(m))].width(Length::Fill);
        if r.expanded {
            if let Some(sub_lanes) = r.sub_lanes {
                content = content.push(sub_lanes);
            }
        }
        if !r.reorderable {
            return content.into();
        }
        // A lane never accepts itself.
        let candidate = r.lane_drag.filter(|id| *id != r.track.id);
        drop_target(content.into(), candidate, r.on_message)
    }
}

/// The 74 px main row: the header and the lane, a 1 px `Studio.border`
/// line under them, the drop line over them while a dragged lane hovers,
/// and a `Studio.hover` wash (55 %) under them when selected.
fn main_row<'a, M>(r: &TrackRow<'a, M>) -> Element<'a, Message> {
    let accent = lane_accent(r.track);
    let selected = r.selected;
    container(column![
        // Always there (0 px when not dropping), so the row below it keeps
        // its widget state as the line comes and goes.
        container(Space::new())
            .width(Length::Fill)
            .height(if r.dropping { 2 } else { 0 })
            .style(fill(Studio::ACCENT_BRIGHT)),
        row![header(r, accent), body(r, accent)].height(Length::Fill),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
    ])
    .width(Length::Fill)
    .height(TRACK_ROW_HEIGHT)
    .style(move |_| {
        if selected {
            boxed(Some(alpha(Studio::HOVER, 0.55)), 0.0, None)
        } else {
            container::Style::default()
        }
    })
    .into()
}

// -------------------------------------------------------------- the header

/// `_TrackHeader`: the fixed-width left control column for a track
/// (Logic/Cubase mixer-strip style): a 3 px color bar, the name + ⋯ menu,
/// and a row of mute/solo, volume and add-note controls. A tap on it
/// selects the lane; a right-click opens the lane menu. The label is 11.5
/// px w700 with 0.3 letter spacing, ellipsized.
fn header<'a, M>(r: &TrackRow<'a, M>, accent: Color) -> Element<'a, Message> {
    let track = r.track;
    let mut top = row![].align_y(Vertical::Center);
    if r.can_expand {
        top = top.push(disclose_button(r.expanded, r.expandable));
    }
    if r.armed {
        top = top
            .push(
                container(Space::new().width(7).height(7))
                    .style(|_| boxed(Some(Studio::RECORD), 3.5, None)),
            )
            .push(Space::new().width(6));
    }
    top = top
        .push(
            studio_text(r.label.to_uppercase())
                .size(11.5)
                .font(font::BOLD)
                .letter_spacing(0.3)
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .ellipsis(1),
        )
        .push(track_menu_button(Message::ContextMenu));
    let mut controls = row![
        ms_toggle("M", track.muted, Studio::RECORD, Message::ToggleMute),
        Space::new().width(4),
        ms_toggle("S", track.soloed, Studio::ACCENT, Message::ToggleSolo),
        Space::new().width(10),
        container(studio_fader(
            track.volume,
            accent,
            Message::Volume,
            Message::VolumeCommit
        ))
        .width(Length::Fill),
    ]
    .align_y(Vertical::Center);
    // Note/technique annotations apply to vocal takes only — the backing
    // track gets no add buttons.
    if !track.is_backing() {
        controls = controls
            .push(Space::new().width(8))
            .push(studio_mini_button(
                Icon::StickyNote,
                Message::AddNote,
                Studio::TEXT_SECONDARY,
            ))
            .push(Space::new().width(6))
            .push(studio_mini_button(
                Icon::Waypoints,
                Message::AddTechnique,
                Studio::TEXT_SECONDARY,
            ));
    }
    let content = container(column![top, Space::new().height(Length::Fill), controls])
        .padding(Padding {
            top: 8.0,
            right: 8.0,
            bottom: 8.0,
            left: 9.0,
        })
        .width(Length::Fill)
        .height(Length::Fill);
    let area = mouse_area(Wrap::<Message, TapState>::new(
        content,
        Box::new(|state, event, bounds, cursor, shell| match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                if cursor.is_over(bounds) && !shell.is_event_captured() {
                    shell.publish(Message::ContextMenu);
                }
            }
            _ => {
                if state.update(event, bounds, cursor, shell.is_event_captured()) {
                    shell.publish(Message::Select);
                }
            }
        }),
    ))
    .interaction(mouse::Interaction::Pointer);
    let selected = r.selected;
    container(row![
        container(Space::new())
            .width(3)
            .height(Length::Fill)
            .style(fill(accent)),
        area,
        container(Space::new())
            .width(1)
            .height(Length::Fill)
            .style(fill(Studio::BORDER)),
    ])
    .width(TRACK_HEADER_W)
    .height(Length::Fill)
    .style(fill(if selected {
        Studio::SURFACE_RAISED
    } else {
        Studio::PANEL
    }))
    .into()
}

/// A tap (`GestureDetector.onTap`) in flight: the primary press, while it
/// has stayed within the tap slop.
#[derive(Debug, Default)]
struct TapState {
    down: Option<Point>,
}

impl TapState {
    /// Follows the primary button; true when a tap completed. A press a
    /// child took (a button, the fader) is not this tap; a release with no
    /// cursor (the reorder drag let the pointer go) completes nothing.
    fn update(
        &mut self,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        captured: bool,
    ) -> bool {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                self.down = cursor.position_over(bounds).filter(|_| !captured);
                false
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(down), Some(p)) = (self.down, cursor.position()) {
                    if down.distance(p) > TAP_SLOP {
                        self.down = None;
                    }
                }
                false
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                self.down.take().is_some() && cursor.position().is_some()
            }
            _ => false,
        }
    }
}

/// `_DiscloseButton`: the chevron that folds the inline analysis lanes in
/// or out — down and coral when out. Inert when the row has no toggle
/// (a tap then selects the lane, as Dart's null `onTap` let it).
fn disclose_button<'a>(expanded: bool, expandable: bool) -> Element<'a, Message> {
    let glyph = container(icons::icon(
        if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        14.0,
        if expanded {
            Studio::ACCENT
        } else {
            Studio::TEXT_SECONDARY
        },
    ))
    .padding(Padding {
        right: 4.0,
        ..Padding::ZERO
    });
    button(glyph)
        .padding(0)
        .style(bare_button)
        .on_press_maybe(expandable.then_some(Message::ToggleExpand))
        .into()
}

/// `TrackMenuButton`: the ⋯ overflow button — opens the track context menu
/// at its own position, on the press (`onTapDown`). Wrap whatever holds it
/// in the menu host: the host keeps that press for the menu.
pub fn track_menu_button<'a, M: Clone + 'a>(on_tap: M) -> Element<'a, M> {
    let glyph =
        container(icons::icon(Icon::Ellipsis, 16.0, Studio::TEXT_SECONDARY)).padding(Padding {
            left: 4.0,
            ..Padding::ZERO
        });
    Wrap::<M, ()>::new(
        glyph,
        Box::new(move |_, event, bounds, cursor, shell| {
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                if cursor.is_over(bounds) && !shell.is_event_captured() {
                    shell.publish(on_tap.clone());
                    shell.capture_event();
                }
            }
        }),
    )
    .interaction(Box::new(|_, bounds, cursor| {
        cursor
            .is_over(bounds)
            .then_some(mouse::Interaction::Pointer)
    }))
    .into()
}

/// `MsToggle`: a 20 × 18 mute / solo key, radius 3, its 10 px w700 letter
/// in `active_color` (on a 20 % wash, 60 % outline) when on.
pub fn ms_toggle<'a, M: Clone + 'a>(
    label: &'a str,
    active: bool,
    active_color: Color,
    on_tap: M,
) -> Element<'a, M> {
    let face = container(text(label).size(10).font(font::BOLD).color(if active {
        active_color
    } else {
        Studio::TEXT_SECONDARY
    }))
    .center_x(20)
    .center_y(18)
    .style(move |_| {
        boxed(
            Some(if active {
                alpha(active_color, 0.2)
            } else {
                Studio::SURFACE_RAISED
            }),
            3.0,
            Some(if active {
                alpha(active_color, 0.6)
            } else {
                Studio::BORDER
            }),
        )
    });
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

// ---------------------------------------------------------------- the lane

/// What the lane's input needs of the take: its clips' spans and its
/// notes' positions, on the viewport.
struct LaneGeometry {
    dur: i64,
    view_start: i64,
    clips: Vec<(String, i64, i64)>,
    notes: Vec<(String, i64)>,
}

/// What a press on the lane landed on.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Clip(String),
    Note(String),
    Lane,
}

impl LaneGeometry {
    fn of(track: &AudioTrack, dur: i64, view_start: i64) -> Self {
        Self {
            dur,
            view_start,
            clips: track
                .clips
                .iter()
                .map(|c| (c.id.clone(), c.start_ms, c.duration_ms))
                .collect(),
            notes: track
                .notes
                .iter()
                .map(|n| (n.id.clone(), n.position_ms))
                .collect(),
        }
    }

    /// What is under `at` (lane coordinates) in a lane `size`: a note marker
    /// over a clip, a later clip over an earlier one (the stack's order).
    fn hit(&self, at: Point, size: Size) -> Hit {
        let width = f64::from(size.width);
        for (id, position) in self.notes.iter().rev() {
            if note_hit_box(x_of(*position, self.dur, self.view_start, width), size).contains(at) {
                return Hit::Note(id.clone());
            }
        }
        for (id, start, duration) in self.clips.iter().rev() {
            if let Some(v) = visible_span(self.dur, self.view_start, width, *start, *duration, 0) {
                let block = Rectangle::new(
                    Point::new(v.left as f32, 3.0),
                    Size::new(v.width as f32, (size.height - 6.0).max(0.0)),
                );
                if block.contains(at) {
                    return Hit::Clip(id.clone());
                }
            }
        }
        Hit::Lane
    }

    fn clip_start(&self, id: &str) -> Option<i64> {
        self.clips
            .iter()
            .find(|(clip, _, _)| clip == id)
            .map(|(_, start, _)| *start)
    }
}

/// The lane's pointer state: a primary press in flight, and a clip
/// move-drag.
#[derive(Debug, Default)]
struct LaneState {
    press: Option<LanePress>,
    drag: Option<ClipDrag>,
}

#[derive(Debug)]
struct LanePress {
    /// Where it landed, in lane coordinates.
    at: Point,
    target: Hit,
    /// Still a tap: within the tap slop.
    tap: bool,
}

/// A clip's move-drag (`_ClipRegionState`): the clip, its start when the
/// drag began, the accumulated horizontal travel, and the pointer's last x.
#[derive(Debug)]
struct ClipDrag {
    clip_id: String,
    base_start_ms: i64,
    dx: f64,
    last_x: f32,
}

impl LaneState {
    /// Follows the pointer over the lane; what it reports goes to `shell`.
    fn update(
        &mut self,
        g: &LaneGeometry,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) {
        let local = |p: Point| Point::new(p.x - bounds.x, p.y - bounds.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // A drag whose release was lost commits once, as Dart's
                // cancel did: its live moves already moved the model.
                if self.drag.take().is_some() {
                    shell.publish(Message::MoveCommit);
                }
                self.press = cursor.position_over(bounds).map(|p| {
                    let at = local(p);
                    LanePress {
                        at,
                        target: g.hit(at, bounds.size()),
                        tap: true,
                    }
                });
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                let Some(p) = cursor.position_over(bounds) else {
                    return;
                };
                match g.hit(local(p), bounds.size()) {
                    Hit::Clip(id) => shell.publish(Message::ClipContextMenu(id)),
                    Hit::Lane => shell.publish(Message::ContextMenu),
                    // A note marker takes taps only.
                    Hit::Note(_) => {}
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(p) = cursor.position().map(local) else {
                    return;
                };
                if let Some(drag) = &mut self.drag {
                    drag.dx += f64::from(p.x - drag.last_x);
                    drag.last_x = p.x;
                    if let Some(start_ms) = dragged_start_ms(
                        drag.base_start_ms,
                        drag.dx,
                        f64::from(bounds.width),
                        g.dur,
                    ) {
                        shell.publish(Message::ClipMoveLive {
                            clip_id: drag.clip_id.clone(),
                            start_ms,
                        });
                    }
                    shell.capture_event();
                    return;
                }
                let Some(press) = &mut self.press else {
                    return;
                };
                if let Hit::Clip(id) = &press.target {
                    if (p.x - press.at.x).abs() > DRAG_SLOP {
                        // The drag wins: no tap. The movement that won it is
                        // swallowed (`DragStartBehavior.start`).
                        if let Some(base_start_ms) = g.clip_start(id) {
                            self.drag = Some(ClipDrag {
                                clip_id: id.clone(),
                                base_start_ms,
                                dx: 0.0,
                                last_x: p.x,
                            });
                        }
                        self.press = None;
                        shell.capture_event();
                        return;
                    }
                }
                if press.at.distance(p) > TAP_SLOP {
                    press.tap = false;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if self.drag.take().is_some() {
                    shell.publish(Message::MoveCommit);
                    return;
                }
                let Some(press) = self.press.take() else {
                    return;
                };
                // No cursor: the reorder drag let the pointer go.
                if !press.tap || cursor.position().is_none() {
                    return;
                }
                shell.publish(match press.target {
                    Hit::Clip(id) => Message::SelectClip(id),
                    Hit::Note(id) => Message::EditNote(id),
                    Hit::Lane => Message::Select,
                });
            }
            _ => {}
        }
    }
}

/// `_Body`: the timeline lane — the empty-lane background and grid, each
/// clip as a block, the forming take while recording, the note markers and
/// the playhead — and its input (see the module docs).
fn body<'a, M>(r: &TrackRow<'a, M>, accent: Color) -> Element<'a, Message> {
    let track = r.track;
    let dur = r.timeline_duration_ms; // the visible span
    let view_start = r.view_start_ms;
    let position_ms = r.position_ms;
    let lane_selected = r.selected;
    let selected_clip_id = r.selected_clip_id.clone();
    let recording = r.recording;
    let lanes = responsive(move |size| {
        let width = f64::from(size.width);
        let block_height = (size.height - 6.0).max(0.0);
        let mut layers: Vec<Element<'a, Message>> = vec![
            // Empty-lane background + grid (bottom of the stack).
            container(
                canvas_widget(TimelineGridPainter {
                    timeline_duration_ms: dur,
                    color: Studio::GRID,
                    start_ms: view_start,
                })
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(fill(Studio::INSET))
            .into(),
        ];
        // Each clip — its own block, by id.
        for clip in &track.clips {
            let Some(v) = clip_box(clip, false, dur, view_start, width) else {
                continue;
            };
            let region = clip_region(
                clip,
                &v,
                accent,
                clip_selected(lane_selected, selected_clip_id.as_deref(), &clip.id),
                block_height,
            );
            layers.push(pin(region).x(v.left as f32).y(3.0).into());
        }
        // The forming clip while this lane is the armed recording target.
        if let Some((peaks, start_ms)) = recording {
            if let Some(v) = recording_span(dur, view_start, width, start_ms, peaks.len()) {
                let painter = LiveWaveformPainter {
                    peaks: peaks.get(v.slice.range.clone()).unwrap_or(&[]).into(),
                    color: Studio::RECORD,
                    pixels_per_sample: recording_pixels_per_sample(dur, width),
                    // Keeps the absolute sample→column mapping glued to the
                    // timeline when the window edge lands mid-peak.
                    sample_offset: v.slice.index_offset,
                };
                layers.push(
                    pin(forming_clip(painter, v.width as f32, block_height))
                        .x(v.left as f32)
                        .y(3.0)
                        .into(),
                );
            }
        }
        for note in &track.notes {
            let at = note_box(x_of(note.position_ms, dur, view_start, width), size);
            layers.push(pin(note_marker(accent)).x(at.x).y(at.y).into());
        }
        layers.push(
            canvas_widget(PlayheadPainter {
                position_ms,
                timeline_duration_ms: dur,
                color: Studio::PLAYHEAD,
                show_head: false,
                start_ms: view_start,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        );
        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    });
    let geometry = Rc::new(LaneGeometry::of(track, dur, view_start));
    let cursor_geometry = Rc::clone(&geometry);
    Wrap::<Message, LaneState>::new(
        container(lanes)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true),
        Box::new(move |state, event, bounds, cursor, shell| {
            state.update(&geometry, event, bounds, cursor, shell);
        }),
    )
    .interaction(Box::new(move |_, bounds, cursor| {
        let p = cursor.position_over(bounds)?;
        let at = Point::new(p.x - bounds.x, p.y - bounds.y);
        match cursor_geometry.hit(at, bounds.size()) {
            Hit::Clip(_) => Some(mouse::Interaction::Grab),
            Hit::Note(_) => Some(mouse::Interaction::Pointer),
            Hit::Lane => None,
        }
    }))
    .into()
}

/// A `color` box with the region's border overlaid (not inset): `width`
/// wide, radius 4.
fn border_overlay<'a>(color: Color, width: f32) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            border: Border {
                color,
                width,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The region's 4 px color cap, rounded where the region is.
fn region_cap<'a>(color: Color) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(4)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: iced::border::Radius {
                    top_left: 4.0,
                    top_right: 4.0,
                    bottom_right: 0.0,
                    bottom_left: 0.0,
                },
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// `_Region`: a Logic Pro-style audio region — a rounded, tinted block with
/// a colored cap strip and the waveform drawn inside (or "Analyzing…" until
/// its peaks are in), so the clip's extent is unmistakable.
fn clip_region<'a>(
    clip: &AudioClip,
    v: &VisibleSpan,
    color: Color,
    selected: bool,
    height: f32,
) -> Element<'a, Message> {
    let inner: Element<'a, Message> = if clip.has_waveform() {
        WaveformPainter::sliced(clip_peaks(clip), v.slice.clone(), color)
            .into_canvas()
            .into()
    } else {
        center(text("Analyzing…").size(10).color(Studio::TEXT_FAINT)).into()
    };
    let block = container(column![
        region_cap(alpha(color, 0.9)),
        container(inner)
            .id(waveform_widget_id(&clip.id))
            .padding([3, 0])
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| boxed(Some(alpha(color, 0.14)), 4.0, None))
    .clip(true);
    // Fill stays constant; only the outer border brightens when selected
    // (Logic style) — a lift of the lane's own hue, not pure white — so the
    // waveform inside reads the same either way. Overlaid, not inset (as
    // the forming clip): the painter maps the clip's full width, and an
    // inset drew its edges up to 4 px off the ruler and playhead, shifted
    // the waveform a pixel on selection, and squeezed a take as recording
    // stopped.
    let border = if selected {
        border_overlay(selected_lane_color(color), 2.0)
    } else {
        border_overlay(alpha(color, 0.55), 1.0)
    };
    container(stack![block, border])
        .id(clip_widget_id(&clip.id))
        .width(v.width as f32)
        .height(height)
        .into()
}

/// `_FormingClip`: the clip forming while recording. Draws its waveform
/// full-width (border as an overlay, not an inset) at a fixed
/// pixels-per-sample, so the painter width is exactly the clip width and the
/// already-drawn waveform stays pixel-stable as the take grows — the
/// wobble-free way Logic / Pro Tools / Cubase show a live take, rather than
/// a waveform that restretches every frame.
fn forming_clip<'a>(painter: LiveWaveformPainter, width: f32, height: f32) -> Element<'a, Message> {
    let color = Studio::RECORD;
    let block = container(column![
        region_cap(alpha(color, 0.9)),
        // Vertical inset only: horizontal padding would shrink the painter
        // and break the fixed pixels-per-sample mapping.
        container(
            canvas_widget(painter)
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .padding([3, 0])
        .width(Length::Fill)
        .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| boxed(Some(alpha(color, 0.14)), 4.0, None))
    .clip(true);
    container(stack![block, border_overlay(alpha(color, 0.55), 1.0)])
        .width(width)
        .height(height)
        .into()
}

/// `_NoteMarker`: a 12 px sticky-note glyph in the lane's color, centred in
/// a 16 px box (the lane's input opens the note on a tap).
fn note_marker<'a>(color: Color) -> Element<'a, Message> {
    container(icons::icon(Icon::StickyNote, 12.0, color))
        .center_x(16)
        .center_y(16)
        .into()
}

// ------------------------------------------------------- the reorder drag

/// `_DragGhost`: the floating label shown under the pointer while
/// long-press dragging a lane — a grip glyph and the label (11 px w700, 0.8
/// letter spacing) on a raised, coral-ringed chip with a soft shadow.
fn drag_ghost<'a>(label: &str) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::GripVertical, 14.0, Studio::TEXT_SECONDARY),
            Space::new().width(8),
            studio_text(label.to_uppercase())
                .size(11)
                .font(font::BOLD)
                .letter_spacing(0.8)
                .color(Studio::TEXT_PRIMARY)
                .wrapping(Wrapping::None),
        ]
        .align_y(Vertical::Center),
    )
    .padding([8, 12])
    .style(|_| container::Style {
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0x66 as f32 / 255.0),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..boxed(
            Some(Studio::SURFACE_RAISED),
            6.0,
            Some(Studio::ACCENT_BRIGHT),
        )
    })
    .into()
}

/// The main row of a reorderable lane, as `LongPressDraggable` made it (see
/// the module docs).
struct LaneDrag<'a> {
    content: Element<'a, Message>,
    ghost: Element<'a, Message>,
}

/// A long press in flight, or the reorder drag it became.
#[derive(Debug, Default)]
struct LaneDragState {
    press: Option<(Point, Instant)>,
    /// The long press won: the lane is being dragged.
    dragging: bool,
    /// Where the pointer took the lane, from the row's top-left: where the
    /// ghost appears.
    origin: Vector,
}

impl LaneDragState {
    /// Accepts the long press once it has been held long enough.
    fn check(&mut self, bounds: Rectangle, shell: &mut Shell<'_, Message>) {
        let Some((at, time)) = self.press else {
            return;
        };
        if time.elapsed() < LONG_PRESS_TIMEOUT {
            shell.request_redraw_at(time + LONG_PRESS_TIMEOUT);
            return;
        }
        self.press = None;
        self.dragging = true;
        self.origin = at - bounds.position();
        shell.publish(Message::LaneDragStart);
        shell.invalidate_layout();
        shell.request_redraw();
    }
}

/// The veil over a dragged lane: the screen's background at 65 %, so the
/// row reads at 35 % (`childWhenDragging: Opacity(opacity: 0.35)`) over it.
const DRAGGED_VEIL: Color = alpha(Studio::BACKGROUND, 0.65);

impl Widget<Message, Theme, Renderer> for LaneDrag<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<LaneDragState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(LaneDragState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.ghost)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children.resize_with(2, Tree::empty);
        tree.children[0].diff(&self.content);
        tree.children[1].diff(&self.ghost);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if tree.state.downcast_ref::<LaneDragState>().dragging {
            use iced::advanced::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    ..renderer::Quad::default()
                },
                DRAGGED_VEIL,
            );
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        let bounds = layout.bounds();
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<LaneDragState>();
        if state.dragging {
            match event {
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                | Event::Touch(
                    touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. },
                ) => {
                    // The long press won the pointer: the row's own taps and
                    // drags let it go without acting.
                    self.content.as_widget_mut().update(
                        &mut children[0],
                        event,
                        layout,
                        mouse::Cursor::Unavailable,
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                    state.dragging = false;
                    shell.publish(Message::LaneDragEnd);
                    shell.invalidate_layout();
                    shell.request_redraw();
                }
                // The ghost follows the pointer.
                Event::Mouse(mouse::Event::CursorMoved { .. }) => shell.request_redraw(),
                _ => {}
            }
            return;
        }
        self.content.as_widget_mut().update(
            &mut children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let captured = shell.is_event_captured();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // A control that takes its pointer on contact (the fader, a
                // button) keeps it.
                state.press = cursor
                    .position_over(bounds)
                    .filter(|_| !captured)
                    .map(|p| (p, Instant::now()));
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some((at, _)) = state.press {
                    // Moved off the spot, or a clip drag took the pointer.
                    let drifted = cursor
                        .position()
                        .is_none_or(|p| p.distance(at) > LONG_PRESS_SLOP);
                    if captured || drifted {
                        state.press = None;
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.press = None;
            }
            _ => {}
        }
        // Any event may be the one that finds the press held long enough —
        // the redraw it asked for at the deadline, if nothing else.
        state.check(bounds, shell);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<LaneDragState>().dragging {
            return mouse::Interaction::None;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
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
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_ref::<LaneDragState>();
        let [content_tree, ghost_tree] = children.as_mut_slice() else {
            return None;
        };
        if state.dragging {
            // The feedback's top-left at the pointer
            // (`pointerDragAnchorStrategy`).
            let at = layout.bounds().position() + translation + state.origin;
            return Some(overlay::Element::new(Box::new(Ghost {
                ghost: &mut self.ghost,
                tree: ghost_tree,
                at,
            })));
        }
        self.content
            .as_widget_mut()
            .overlay(content_tree, layout, renderer, viewport, translation)
    }
}

/// The ghost over the window, under the pointer. It takes no input
/// (`IgnorePointer`): the lanes beneath see the pointer, to take the drop.
struct Ghost<'a, 'b> {
    ghost: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    /// Where the lane was taken (the press): laid out there, drawn at the
    /// pointer.
    at: Point,
}

impl overlay::Overlay<Message, Theme, Renderer> for Ghost<'_, '_> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        self.ghost
            .as_widget_mut()
            .layout(
                self.tree,
                renderer,
                &layout::Limits::new(Size::ZERO, bounds),
            )
            .move_to(self.at)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        use iced::advanced::Renderer as _;
        // Drawn where the pointer is now, without a relayout per move.
        let offset = cursor
            .position()
            .map_or(Vector::ZERO, |p| p - layout.bounds().position());
        renderer.with_translation(offset, |renderer| {
            self.ghost.as_widget().draw(
                self.tree,
                renderer,
                theme,
                style,
                layout,
                mouse::Cursor::Unavailable,
                &layout.bounds(),
            );
        });
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.ghost
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::None
    }
}

/// Whether a dragged lane is over this row (as last reported).
#[derive(Debug, Default)]
struct DropState {
    over: bool,
}

/// `DragTarget`: while `candidate` (another lane) is dragged, reports the
/// pointer entering and leaving `content` and a release over it.
fn drop_target<'a, M: 'a>(
    content: Element<'a, M>,
    candidate: Option<String>,
    on_message: Rc<dyn Fn(Message) -> M + 'a>,
) -> Element<'a, M> {
    Wrap::<M, DropState>::new(
        content,
        Box::new(move |state, event, bounds, cursor, shell| {
            let Some(dragged) = &candidate else {
                state.over = false;
                return;
            };
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    let over = cursor.is_over(bounds);
                    if over != state.over {
                        state.over = over;
                        shell.publish(on_message(Message::LaneDragOver(over)));
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if cursor.is_over(bounds) {
                        shell.publish(on_message(Message::CrossLaneMove(dragged.clone())));
                    }
                    state.over = false;
                }
                _ => {}
            }
        }),
    )
    .into()
}
