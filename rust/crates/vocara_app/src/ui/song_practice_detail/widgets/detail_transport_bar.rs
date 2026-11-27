//! Port of `ui/song_practice_detail/widgets/detail_transport_bar.dart`: the
//! practice page's toolbar (`DESIGN.md`, Toolbar), drawn as the approved
//! workspace mockup draws it — the practice's identity on the left (back,
//! the title in 13 px semibold, the take and note counts, the busy
//! spinner), the transport in the middle (a toolbar group of go to start,
//! play/pause and stop | loop and follow playhead; the time display; a
//! recording group of input monitoring | Record), and the timeline zoom on
//! the right (zoom out, a thin slider, zoom in).
//!
//! Plain data in ([`TransportBar`]: the practice and the transport, mixer
//! and recording state the Dart bar read off the view model), [`Message`]s
//! out, each named after the call the Dart widget made, for the screen to
//! route. The gating ([`controls`]), the time display ([`time_display`]),
//! the counts ([`counts_label`]) and the bar's layout ([`bar_layout`]) are
//! pure functions the tests check. Every icon-only control carries its
//! tooltip as its label (what the tests find and click).
//!
//! How the Dart structure maps:
//!
//! - `CustomMultiChildLayout(_BarLayout)`: [`bar_layout`] centres the
//!   transport on the bar and gives the title group and the zoom control the
//!   room either side of it, less a 12 px gap; the title group is a row in
//!   which the title gives way first (`Flexible`), ellipsized.
//! - The counter's `ValueListenableBuilder` and `RepaintBoundary`s: an iced
//!   view is rebuilt from state on every message, and only what changed is
//!   redrawn; the time display reads [`TransportBar::position`] when built.
//! - The counter's tabular figures: `studio_text` with Inter's `tnum`
//!   (`DESIGN.md` allows tabular figures for the time display), so the
//!   transport does not shift as the tenths tick.
//! - `ListenableBuilder(playback.viewport)` around the zoom slider and the
//!   loop and follow buttons: the inputs are read when the bar is built.
//! - The zoom slider's `onTapDown` / `onHorizontalDragUpdate` set the zoom
//!   from the pointer's x: the shared thin slider (`controls::thin_slider`)
//!   sets it on the press and on every move while held, and commits on the
//!   release.
//! - The busy spinner sits after the counts, so the transport never moves
//!   when work starts or ends.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::border::Radius;
use iced::widget::text::Wrapping;
use iced::widget::{button, container, row, Space};
use iced::{
    mouse, Background, Border, Color, Element, Event, Length, Padding, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};
use vocara_model::models::song_practice::SongPractice;

use crate::ui::core::theme::{alpha, boxed, font, height, radius, type_scale, Studio};
use crate::ui::core::widgets::controls::{
    group_button, icon_button, labelled, thin_slider, ToolbarGroup,
};
use crate::ui::core::widgets::line_icon::glyphs;
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::{studio_text, StudioText};
use crate::ui::core::widgets::toolbar;

#[cfg(test)]
mod tests;

/// The bar's height: the page's unified toolbar's (`widgets::toolbar`),
/// its hairline included.
pub const BAR_HEIGHT: f32 = toolbar::HEIGHT;

/// The least clear space [`bar_layout`] keeps between the transport and
/// either side (`_BarLayout._gap`).
pub const SIDE_GAP: f32 = 12.0;

/// How far left of the bar's own centre the transport sits so that it is
/// centred on the window, as the mockup draws it: the toolbar's content
/// starts [`toolbar::LEADING_INSET`] in (past the traffic lights on macOS)
/// but ends [`toolbar::TRAILING_INSET`] in, so the bar's centre is half
/// their difference right of the window's.
pub const CENTER_OFFSET: f32 = (toolbar::LEADING_INSET - toolbar::TRAILING_INSET) / 2.0;

/// The gap between the transport group, the time display and the recording
/// group.
pub const CENTER_GAP: f32 = 8.0;

/// The zoom slider's width and its row's height; its knob.
pub const ZOOM_SLIDER_WIDTH: f32 = 92.0;
const ZOOM_SLIDER_HEIGHT: f32 = 16.0;
pub const ZOOM_KNOB: f32 = 12.0;

/// The zoom icons' steps: out lengthens the visible span by a quarter, in
/// shortens it by a fifth.
pub const ZOOM_OUT_FACTOR: f64 = 1.25;
pub const ZOOM_IN_FACTOR: f64 = 0.8;

/// The busy spinner's label (what a test finds).
pub const BUSY_LABEL: &str = "Working…";

/// The Record button's text.
pub const RECORD_LABEL: &str = "Record";

/// The time display's "/" between the playhead and the length.
const TIME_SLASH: Color = Color::from_rgb8(0x5C, 0x5C, 0x68);

/// What the transport bar asks of the detail view model (or the screen).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The back chevron (`onBack`: the screen finalizes an in-flight take,
    /// then pops or goes Home).
    Back,
    /// Go to start: the playhead to the timeline's start (`vm.seek(0)`).
    GoToStart,
    /// Play/pause (`playback.togglePlay`).
    TogglePlay,
    /// Stop with no take in flight (`playback.stop`).
    Stop,
    /// Stop or Record while a take is recording or still arming: end (or
    /// cancel) the take (`vm.stopRecording`).
    StopRecording,
    /// The loop button (`playback.toggleLoop`).
    ToggleLoop,
    /// The follow-playhead button (`playback.toggleFollowCursor`).
    ToggleFollowCursor,
    /// Input monitoring: arm/disarm the mic
    /// (`recording.toggleInputMonitor`).
    ToggleInputMonitor,
    /// Record with no take in flight (`recording.startRecording`).
    StartRecording,
    /// A zoom icon: scale the visible span by this factor
    /// (`playback.zoomTimeline`: [`ZOOM_OUT_FACTOR`] or [`ZOOM_IN_FACTOR`]).
    ZoomTimeline(f64),
    /// The zoom slider moved to this position in [0, 1]
    /// (`playback.setTimelineZoomLive`).
    SetTimelineZoomLive(f64),
    /// The zoom slider was let go (`playback.commitTimelineZoom`).
    CommitTimelineZoom,
}

/// What the bar shows: the practice (`DetailTransportBar.practice`) and the
/// view-model state the Dart bar read.
#[derive(Debug, Clone, Copy)]
pub struct TransportBar<'a> {
    pub practice: &'a SongPractice,
    /// `vm.mixer.visibleNoteCount` — the mixer-side count, not the
    /// practice's: only ONE backing is on screen at a time (the
    /// Accompaniment ⇄ Original toggle), and the model can't know which —
    /// counting both could show more notes than the user can find.
    pub visible_note_count: usize,
    /// `vm.playback.isPlaying`.
    pub is_playing: bool,
    /// `vm.isRecording`.
    pub is_recording: bool,
    /// `vm.recording.isStartingTake`: Record's arm window.
    pub is_starting_take: bool,
    /// `vm.playback.loopEnabled`.
    pub loop_enabled: bool,
    /// `vm.playback.hasLoop`.
    pub has_loop: bool,
    /// `vm.playback.followCursor`.
    pub follow_cursor: bool,
    /// `vm.playback.timelineZoom`, in [0, 1].
    pub timeline_zoom: f64,
    /// The playhead (`vm.position`).
    pub position: Duration,
    /// `vm.recording.inputMonitorEnabled`.
    pub input_monitor_enabled: bool,
    /// `vm.busy`: shows the spinner.
    pub busy: bool,
}

/// What each transport control does now: `None` is a dead control.
#[derive(Debug, Clone, PartialEq)]
pub struct Controls {
    pub go_to_start: Option<Message>,
    pub play: Option<Message>,
    pub stop: Option<Message>,
    pub toggle_loop: Option<Message>,
    pub rec: Message,
    /// Whether a take is in flight (recording or still arming): Record
    /// shows its stop square.
    pub taking: bool,
}

/// The transport's gating. Every gate protects a state the engine cannot
/// recover from on its own: play, stop and go to start are dead with
/// nothing loaded, play and go to start are dead while a take is in flight
/// (starting playback into a live take bleeds the backing into it; jumping
/// the backing mid-take leaves everything sung after the jump against the
/// wrong part of the song), and the loop toggle is dead until a loop region
/// exists.
pub fn controls(bar: &TransportBar<'_>) -> Controls {
    let p = bar.practice;
    // The reference (Original) is playable too — when the backing toggle is
    // on "Original" it's the active backing — so it must count toward
    // playability.
    let has_tracks =
        p.has_accompaniment() || p.has_reference() || !p.active_vocal_tracks().is_empty();
    // A take in Record's arm window is a take too: Stop and Record cancel
    // it.
    let taking = bar.is_recording || bar.is_starting_take;
    // Dead there too: the backing it started jumped back to the take's start
    // the moment the mic armed.
    let can_play = has_tracks && !taking;
    Controls {
        go_to_start: can_play.then_some(Message::GoToStart),
        play: can_play.then_some(Message::TogglePlay),
        // Mid-take, Stop ends the take (what Stop means in any DAW); the
        // transport stop itself is a no-op while recording.
        stop: if taking {
            Some(Message::StopRecording)
        } else {
            has_tracks.then_some(Message::Stop)
        },
        toggle_loop: bar.has_loop.then_some(Message::ToggleLoop),
        rec: if taking {
            Message::StopRecording
        } else {
            Message::StartRecording
        },
        taking,
    }
}

/// "1 take · 3 notes", "2 takes · 0 notes".
pub fn counts_label(takes: usize, notes: usize) -> String {
    let takes = if takes == 1 {
        "1 take".to_string()
    } else {
        format!("{takes} takes")
    };
    format!("{takes} · {notes} notes")
}

/// A time on the display: "mm:ss.t", the minutes growing past 99, the
/// tenth truncated (the display never shows a tenth not yet reached); a
/// negative time reads as zero.
pub fn time_text(ms: i64) -> String {
    let tenths = ms.max(0) / 100;
    let (minutes, seconds, tenth) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
    format!("{minutes:02}:{seconds:02}.{tenth}")
}

/// The time display's two readings: the playhead and the timeline's
/// length.
pub fn time_display(position: Duration, total_duration_ms: i64) -> (String, String) {
    (
        time_text(i64::try_from(position.as_millis()).unwrap_or(i64::MAX)),
        time_text(total_duration_ms),
    )
}

/// `_BarLayout.performLayout`: where the left group, the transport and the
/// zoom control go on a bar of `size`, given their sizes (`left` and `right`
/// as laid out within [`side_room`]). Each is centred vertically; the
/// transport is centred on the window ([`CENTER_OFFSET`] left of the bar's
/// centre, never past the bar's left edge), the left group flush left and
/// the zoom flush right.
pub fn bar_layout(size: Size, left: Size, center: Size, right: Size) -> [Point; 3] {
    [
        Point::new(0.0, (size.height - left.height) / 2.0),
        Point::new(
            ((size.width - center.width) / 2.0 - CENTER_OFFSET).max(0.0),
            (size.height - center.height) / 2.0,
        ),
        Point::new(size.width - right.width, (size.height - right.height) / 2.0),
    ]
}

/// The room either side of a transport `center_width` wide on a bar
/// `width` wide: laid over one another in a Stack, a title longer than that
/// room ran under the play and stop buttons (and a very long one overflowed
/// the bar); bounded here, it gives way instead.
pub fn side_room(width: f32, center_width: f32) -> f32 {
    // The left side is the narrower one, the transport sitting
    // CENTER_OFFSET left of the bar's centre; both sides take its room.
    ((width - center_width) / 2.0 - CENTER_OFFSET - SIDE_GAP).max(0.0)
}

// ---------------------------------------------------------------------- view

/// The transport bar: the page's unified toolbar (`widgets::toolbar`:
/// [`BAR_HEIGHT`] tall, `Studio::TOOLBAR`, a bottom hairline, its content
/// past the traffic lights).
pub fn detail_transport_bar<'a>(bar: TransportBar<'a>) -> Element<'a, Message> {
    let c = controls(&bar);
    let practice = bar.practice;
    let takes = practice.active_vocal_tracks().len();

    // Left: back, the title, the counts (and the spinner). The title gives
    // way first.
    let mut left = vec![
        // The mockup's 6 px past the traffic lights' 14 px gap.
        Space::new().width(8).into(),
        toolbar::back_button(Message::Back).into(),
        Space::new().width(14).into(),
        toolbar::title(practice.title.clone()),
        Space::new().width(8).into(),
        studio_text(counts_label(takes, bar.visible_note_count))
            .size(type_scale::BODY)
            .line_height(iced::Pixels(type_scale::LINE))
            .tabular()
            .color(Studio::TEXT_FAINT)
            .wrapping(Wrapping::None)
            .into(),
    ];
    if bar.busy {
        left.push(Space::new().width(10).into());
        left.push(labelled(
            spinner(12.0, 1.5, Studio::TEXT_SECONDARY),
            BUSY_LABEL,
        ));
    }
    let left = flexible_row(left, 3);

    // Right: the timeline zoom, a thin slider between zoom out and zoom in.
    let right = row![
        icon_button(glyphs::MINUS, Message::ZoomTimeline(ZOOM_OUT_FACTOR))
            .panel()
            .icon_size(13.0)
            .tooltip("Zoom out"),
        container(
            thin_slider(
                bar.timeline_zoom as f32,
                |v| Message::SetTimelineZoomLive(f64::from(v)),
                Message::CommitTimelineZoom,
            )
            .knob(ZOOM_KNOB)
            .height(ZOOM_SLIDER_HEIGHT),
        )
        .width(ZOOM_SLIDER_WIDTH),
        icon_button(glyphs::PLUS, Message::ZoomTimeline(ZOOM_IN_FACTOR))
            .panel()
            .icon_size(13.0)
            .tooltip("Zoom in"),
    ]
    .spacing(6)
    .align_y(Vertical::Center);

    // Center: the transport, independent of the title's width.
    let transport = ToolbarGroup::new()
        .push(group_button(
            glyphs::GO_TO_START,
            "Go to start",
            c.go_to_start.clone(),
            false,
        ))
        .push(play_button(bar.is_playing, c.play.clone()))
        .push(group_button(glyphs::STOP, "Stop", c.stop.clone(), false))
        .divider()
        .push(group_button(
            glyphs::LOOP,
            "Loop",
            c.toggle_loop.clone(),
            bar.loop_enabled,
        ))
        .push(group_button(
            glyphs::FOLLOW,
            "Follow playhead",
            Some(Message::ToggleFollowCursor),
            bar.follow_cursor,
        ));
    let (now, total) = time_display(bar.position, practice.timeline_duration_ms);
    // Input monitoring arms the mic (its meter and levels live in the
    // mixer's input strip).
    let recording = ToolbarGroup::new()
        .push(group_button(
            glyphs::MONITOR,
            "Input monitoring",
            Some(Message::ToggleInputMonitor),
            bar.input_monitor_enabled,
        ))
        .divider()
        .push(record_button(c.taking, bar.is_recording, c.rec.clone()));
    let center = row![transport, time_readout(now, total), recording]
        .spacing(CENTER_GAP)
        .align_y(Vertical::Center);

    let slots = Arranged::new(
        vec![left, right.into(), center.into()],
        Size::new(Length::Fill, Length::Fill),
        Box::new(arrange_bar),
    );
    toolbar::page_toolbar(slots)
}

/// Play or pause, its glyph and tooltip following the transport; drawn in
/// `TEXT_PRIMARY` while it can play (the group's main control, as the
/// mockup draws it).
fn play_button<'a>(playing: bool, on_press: Option<Message>) -> Element<'a, Message> {
    let (glyph, tip) = if playing {
        (glyphs::PAUSE, "Pause")
    } else {
        (glyphs::PLAY, "Play")
    };
    let enabled = on_press.is_some();
    group_button(glyph, tip, on_press, enabled)
}

/// The time display (`DESIGN.md`, Type): a 30 px `INSET` well, radius 7,
/// an inset `BORDER` outline, 12 px in; the playhead in `TEXT_PRIMARY`, a
/// faint "/", the length in `TEXT_FAINT`; 14 px medium, tabular figures.
fn time_readout<'a>(now: String, total: String) -> Element<'a, Message> {
    let part = |content: String, color: Color| -> StudioText<'a> {
        studio_text(content)
            .size(type_scale::TIME)
            .font(font::MEDIUM)
            .line_height(iced::Pixels(18.0))
            .letter_spacing(0.2)
            .tabular()
            .color(color)
            .wrapping(Wrapping::None)
    };
    container(
        row![
            part(now, Studio::TEXT_PRIMARY),
            part("/".to_string(), TIME_SLASH),
            part(total, Studio::TEXT_FAINT),
        ]
        .spacing(6)
        .align_y(Vertical::Center),
    )
    .padding(Padding::from([0, 12]))
    .center_y(height::TOOLBAR_GROUP)
    .style(|_| boxed(Some(Studio::INSET), radius::GROUP, Some(Studio::BORDER)))
    .into()
}

/// The Record button, the recording group's right half: a red dot (10 px,
/// a soft 3 px halo) and "Record". While a take is in flight the dot turns
/// into a rounded stop square, the half fills with `RECORD` (12 % while
/// the take arms, 20 % once it records) and the text turns `RECORD_TEXT`:
/// a press then ends the take.
fn record_button<'a>(taking: bool, recording: bool, on_press: Message) -> Element<'a, Message> {
    let dot = container(Space::new().width(10).height(10))
        .style(move |_| boxed(Some(Studio::RECORD), if taking { 2.0 } else { 5.0 }, None));
    // `box-shadow: 0 0 0 3px rgba(229,87,75,.18)`: a 3 px ring round the
    // dot (the stop square has none).
    let dot = container(dot)
        .center_x(16)
        .center_y(16)
        .style(move |_| boxed((!taking).then_some(alpha(Studio::RECORD, 0.18)), 8.0, None));
    let face = row![
        dot,
        studio_text(RECORD_LABEL)
            .size(type_scale::BODY)
            .font(font::MEDIUM)
            .line_height(iced::Pixels(type_scale::LINE)),
    ]
    .spacing(4)
    .align_y(Vertical::Center);
    let fill = if recording {
        0.20
    } else if taking {
        0.12
    } else {
        0.0
    };
    // Inside the group's 1 px outline: 28 tall, rounded on the right only.
    let inner = height::TOOLBAR_GROUP - 2.0;
    let target = button(container(face).center_y(inner))
        .padding(Padding {
            top: 0.0,
            right: 11.0,
            bottom: 0.0,
            left: 7.0,
        })
        .height(inner)
        .on_press(on_press)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let background = if fill > 0.0 {
                Some(alpha(
                    Studio::RECORD,
                    fill + if hovered { 0.06 } else { 0.0 },
                ))
            } else {
                hovered.then_some(Studio::HOVER)
            };
            button::Style {
                background: background.map(Background::Color),
                text_color: if taking {
                    Studio::RECORD_TEXT
                } else {
                    Studio::TEXT_PRIMARY
                },
                border: Border {
                    radius: Radius {
                        top_left: 0.0,
                        top_right: radius::GROUP - 1.0,
                        bottom_right: radius::GROUP - 1.0,
                        bottom_left: 0.0,
                    },
                    ..Border::default()
                },
                shadow: Shadow::default(),
                snap: true,
            }
        });
    let tip = if taking { "Stop recording" } else { "Record" };
    container(material_tooltip(target, tip, 6.0))
        .padding(Padding {
            top: 1.0,
            right: 1.0,
            bottom: 1.0,
            left: 0.0,
        })
        .into()
}

// ------------------------------------------------------------------ layouts

/// How an [`Arranged`] lays its children out.
type Arrange<'a, M> = Box<
    dyn Fn(&mut [Element<'a, M>], &mut [Tree], &Renderer, &layout::Limits) -> layout::Node + 'a,
>;

/// Children laid out by a function: the counterpart of the Dart bar's
/// `CustomMultiChildLayout` and its `Row(mainAxisSize: min)` with a
/// `Flexible` title. Draws, operates and routes events as a `Row` does.
struct Arranged<'a, M> {
    children: Vec<Element<'a, M>>,
    size: Size<Length>,
    arrange: Arrange<'a, M>,
}

impl<'a, M> Arranged<'a, M> {
    fn new(children: Vec<Element<'a, M>>, size: Size<Length>, arrange: Arrange<'a, M>) -> Self {
        Self {
            children,
            size,
            arrange,
        }
    }
}

/// `Row(mainAxisSize: MainAxisSize.min, children: ...)` in which the child
/// at `flex` is wrapped in a (loose) `Flexible`: it takes what the others
/// leave, never more than its own width; the row shrinks to its children.
/// (iced's `Row` lays its children out in order, so a long first child
/// would leave nothing for the ones after it.) The inspector's chips use it
/// too.
pub(crate) fn flexible_row<'a, M: 'a>(
    children: Vec<Element<'a, M>>,
    flex: usize,
) -> Element<'a, M> {
    Arranged::new(
        children,
        Size::new(Length::Shrink, Length::Shrink),
        Box::new(move |children, trees, renderer, limits| {
            loose_flex_row(children, trees, renderer, limits, flex)
        }),
    )
    .into()
}

/// `_BarLayout`, over `[left, right, center]` (Dart's paint order).
fn arrange_bar<M>(
    children: &mut [Element<'_, M>],
    trees: &mut [Tree],
    renderer: &Renderer,
    limits: &layout::Limits,
) -> layout::Node {
    let size = limits.max();
    let loose = |max: Size| layout::Limits::new(Size::ZERO, max);
    let center = children[2]
        .as_widget_mut()
        .layout(&mut trees[2], renderer, &loose(size));
    let side = Size::new(side_room(size.width, center.size().width), size.height);
    let left = children[0]
        .as_widget_mut()
        .layout(&mut trees[0], renderer, &loose(side));
    let right = children[1]
        .as_widget_mut()
        .layout(&mut trees[1], renderer, &loose(side));
    let [l, c, r] = bar_layout(size, left.size(), center.size(), right.size());
    layout::Node::with_children(
        size,
        vec![left.move_to(l), right.move_to(r), center.move_to(c)],
    )
}

/// `Row(mainAxisSize: min)` whose child `flex` is `Flexible` (loose): the
/// other children are laid out first at their own widths, the flexible one
/// gets what is left, then all sit side by side, centred vertically.
fn loose_flex_row<M>(
    children: &mut [Element<'_, M>],
    trees: &mut [Tree],
    renderer: &Renderer,
    limits: &layout::Limits,
    flex: usize,
) -> layout::Node {
    let max = limits.max();
    let mut nodes: Vec<Option<layout::Node>> = (0..children.len()).map(|_| None).collect();
    let mut used = 0.0;
    for (i, (child, tree)) in children.iter_mut().zip(trees.iter_mut()).enumerate() {
        if i == flex {
            continue;
        }
        let node =
            child
                .as_widget_mut()
                .layout(tree, renderer, &layout::Limits::new(Size::ZERO, max));
        used += node.size().width;
        nodes[i] = Some(node);
    }
    if let (Some(child), Some(tree)) = (children.get_mut(flex), trees.get_mut(flex)) {
        let room = Size::new((max.width - used).max(0.0), max.height);
        nodes[flex] = Some(child.as_widget_mut().layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, room),
        ));
    }
    let nodes: Vec<layout::Node> = nodes.into_iter().flatten().collect();
    let height = nodes.iter().map(|n| n.size().height).fold(0.0, f32::max);
    let mut x = 0.0;
    let placed = nodes
        .into_iter()
        .map(|n| {
            let s = n.size();
            let placed = n.move_to(Point::new(x, (height - s.height) / 2.0));
            x += s.width;
            placed
        })
        .collect();
    layout::Node::with_children(Size::new(x.min(max.width), height), placed)
}

impl<M> Widget<M, Theme, Renderer> for Arranged<'_, M> {
    fn size(&self) -> Size<Length> {
        self.size
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.size.width).height(self.size.height);
        (self.arrange)(&mut self.children, &mut tree.children, renderer, &limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<Arranged<'a, M>> for Element<'a, M> {
    fn from(arranged: Arranged<'a, M>) -> Self {
        Element::new(arranged)
    }
}
