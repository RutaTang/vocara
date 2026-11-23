//! Port of `ui/song_practice_detail/widgets/detail_transport_bar.dart`: the
//! top toolbar of the detail view — practice identity on the left (back,
//! title, take and note counts), the always-visible transport in the middle
//! (play/pause, stop, loop, follow-cursor, the position counter, input
//! monitoring, REC and the busy spinner), and the timeline zoom on the
//! right.
//!
//! Plain data in ([`TransportBar`]: the practice and the transport, mixer
//! and recording state the Dart bar read off the view model), [`Message`]s
//! out, each named after the call the Dart widget made, for the screen to
//! route. The gating ([`controls`]), the counter ([`lcd_text`]), the counts
//! ([`counts_label`]), the bar's layout ([`bar_layout`]) and the zoom
//! slider's mapping are pure functions the tests check.
//!
//! How the Dart structure maps:
//!
//! - `CustomMultiChildLayout(_BarLayout)`: [`bar_layout`] centres the
//!   transport on the bar and gives the title group and the zoom control the
//!   room either side of it, less a 12 px gap; the title group is a row in
//!   which the title gives way first (`Flexible`), clipped where Flutter
//!   ellipsized it (iced has no ellipsis).
//! - The counter's `ValueListenableBuilder` and `RepaintBoundary`s: an iced
//!   view is rebuilt from state on every message, and only what changed is
//!   redrawn; the counter reads [`TransportBar::position`] when built.
//! - The counter's tabular figures and 1 px letter spacing (iced text has
//!   neither): the counter lays its characters out itself, each digit in a
//!   cell as wide as the widest digit and every character followed by 1 px,
//!   so the transport does not shift as the seconds tick.
//! - `ListenableBuilder(playback.viewport)` around the zoom slider and the
//!   loop and follow buttons: the inputs are read when the bar is built.
//! - The zoom slider's `onTapDown` / `onHorizontalDragUpdate` set the zoom
//!   from the pointer's x; here the press sets it at once (Flutter waited
//!   for the tap to win or `kPressTimeout`, a drag's first update landing
//!   within a pixel or two of it), every move while held sets it, and the
//!   release commits.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::{self as core_text, Paragraph as _, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::widget::text::Wrapping;
use iced::widget::{button, canvas as canvas_widget, column, container, row, text, Space};
use iced::{
    mouse, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme,
    Vector,
};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::format::{format_clock, format_clock_ms};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{alpha, bare_button, boxed, fill, font, Studio};
use crate::ui::core::widgets::studio_controls::studio_round_button;
use crate::ui::core::widgets::studio_hint::spinner;

#[cfg(test)]
mod tests;

/// The bar's height.
pub const BAR_HEIGHT: f32 = 54.0;

/// The bar's side padding.
const BAR_PADDING: f32 = 14.0;

/// The least clear space [`bar_layout`] keeps between the transport and
/// either side (`_BarLayout._gap`).
pub const SIDE_GAP: f32 = 12.0;

/// The transport buttons' diameter.
const ROUND_BUTTON: f32 = 34.0;

/// The zoom slider's width (`_ZoomSlider._width`) and height.
pub const ZOOM_SLIDER_WIDTH: f32 = 96.0;
const ZOOM_SLIDER_HEIGHT: f32 = 22.0;

/// The zoom icons' steps: out lengthens the visible span by a quarter, in
/// shortens it by a fifth.
pub const ZOOM_OUT_FACTOR: f64 = 1.25;
pub const ZOOM_IN_FACTOR: f64 = 0.8;

/// The counter's line box: Inter's ascent + descent (≈ 1.21 em), the height
/// Flutter's `Text` gave it.
const LCD_LINE_HEIGHT: f32 = 1.21;

/// What the transport bar asks of the detail view model (or the screen).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The back chevron (`onBack`: the screen finalizes an in-flight take,
    /// then pops or goes Home).
    Back,
    /// Play/pause (`playback.togglePlay`).
    TogglePlay,
    /// Stop with no take in flight (`playback.stop`).
    Stop,
    /// Stop or REC while a take is recording or still arming: end (or
    /// cancel) the take (`vm.stopRecording`).
    StopRecording,
    /// The loop button (`playback.toggleLoop`).
    ToggleLoop,
    /// The follow-cursor button (`playback.toggleFollowCursor`).
    ToggleFollowCursor,
    /// The headphones: arm/disarm input monitoring
    /// (`recording.toggleInputMonitor`).
    ToggleInputMonitor,
    /// REC with no take in flight (`recording.startRecording`).
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
    /// `vm.recording.isStartingTake`: REC's arm window.
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
    pub play: Option<Message>,
    pub stop: Option<Message>,
    pub toggle_loop: Option<Message>,
    pub rec: Message,
    /// Whether a take is in flight (recording or still arming): the REC pill
    /// shows its stop square.
    pub taking: bool,
}

/// The transport's gating. Every gate protects a state the engine cannot
/// recover from on its own: play and stop are dead with nothing loaded,
/// play is dead while a take is in flight (starting playback into a live
/// take bleeds the backing into it), and the loop toggle is dead until a
/// loop region exists.
pub fn controls(bar: &TransportBar<'_>) -> Controls {
    let p = bar.practice;
    // The reference (Original) is playable too — when the backing toggle is
    // on "Original" it's the active backing — so it must count toward
    // playability.
    let has_tracks =
        p.has_accompaniment() || p.has_reference() || !p.active_vocal_tracks().is_empty();
    // A take in REC's arm window is a take too: Stop and the pill cancel it.
    let taking = bar.is_recording || bar.is_starting_take;
    // Dead there too: the backing it started jumped back to the take's start
    // the moment the mic armed.
    let can_play = has_tracks && !taking;
    Controls {
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

/// The position counter: "mm:ss / mm:ss", the playhead over the timeline's
/// length.
pub fn lcd_text(position: Duration, total_duration_ms: i64) -> String {
    format!(
        "{} / {}",
        format_clock(position),
        format_clock_ms(total_duration_ms)
    )
}

/// The zoom a press or drag at `dx` across the slider sets.
pub fn zoom_value_at(dx: f32) -> f64 {
    (f64::from(dx) / f64::from(ZOOM_SLIDER_WIDTH)).clamp(0.0, 1.0)
}

/// Where the slider's knob sits for `value` on a slider `width` wide (its
/// track runs 3 px in from either end).
pub fn zoom_knob_x(value: f64, width: f32) -> f32 {
    let (left, right) = (3.0, width - 3.0);
    left + value.clamp(0.0, 1.0) as f32 * (right - left)
}

/// `_BarLayout.performLayout`: where the left group, the transport and the
/// zoom control go on a bar of `size`, given their sizes (`left` and `right`
/// as laid out within [`side_room`]). Each is centred vertically; the
/// transport is centred across the bar, the left group flush left and the
/// zoom flush right.
pub fn bar_layout(size: Size, left: Size, center: Size, right: Size) -> [Point; 3] {
    [
        Point::new(0.0, (size.height - left.height) / 2.0),
        Point::new(
            (size.width - center.width) / 2.0,
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
    ((width - center_width) / 2.0 - SIDE_GAP).max(0.0)
}

// ---------------------------------------------------------------------- view

/// The transport bar: [`BAR_HEIGHT`] tall, `Studio.surface`, a bottom
/// border.
pub fn detail_transport_bar<'a>(bar: TransportBar<'a>) -> Element<'a, Message> {
    let c = controls(&bar);
    let practice = bar.practice;
    let takes = practice.active_vocal_tracks().len();

    // Left: back + title + counts. The title gives way first.
    let left = flexible_row(
        vec![
            back_button(),
            Space::new().width(6).into(),
            container(
                text(practice.title.clone())
                    .size(15)
                    .font(font::BOLD)
                    .color(Studio::TEXT_PRIMARY)
                    .wrapping(Wrapping::None),
            )
            .clip(true)
            .into(),
            Space::new().width(10).into(),
            text(counts_label(takes, bar.visible_note_count))
                .size(12)
                .color(Studio::TEXT_FAINT)
                .wrapping(Wrapping::None)
                .into(),
        ],
        2,
    );

    // Right: timeline zoom — a horizontal slider flanked by zoom icons, like
    // Logic's zoom control.
    let right = row![
        zoom_icon(Icon::ZoomOut, ZOOM_OUT_FACTOR),
        Space::new().width(8),
        zoom_slider(bar.timeline_zoom),
        Space::new().width(8),
        zoom_icon(Icon::ZoomIn, ZOOM_IN_FACTOR),
    ]
    .align_y(Vertical::Center);

    // Center: the transport, independent of the title width.
    let mut center = row![
        studio_round_button(
            if bar.is_playing {
                Icon::Pause
            } else {
                Icon::Play
            },
            c.play.clone()
        )
        .filled(true)
        .size(ROUND_BUTTON),
        Space::new().width(8),
        studio_round_button(Icon::Square, c.stop.clone())
            .color(Studio::TEXT_SECONDARY)
            .size(ROUND_BUTTON),
        Space::new().width(8),
        studio_round_button(Icon::Repeat, c.toggle_loop.clone())
            .color(if bar.loop_enabled {
                Studio::ACCENT
            } else {
                Studio::TEXT_SECONDARY
            })
            .size(ROUND_BUTTON),
        Space::new().width(8),
        studio_round_button(Icon::LocateFixed, Some(Message::ToggleFollowCursor))
            .color(if bar.follow_cursor {
                Studio::ACCENT
            } else {
                Studio::TEXT_SECONDARY
            })
            .size(ROUND_BUTTON),
        Space::new().width(16),
        lcd(lcd_text(bar.position, practice.timeline_duration_ms)),
        Space::new().width(12),
        // Input monitoring: arm the mic (meter + level live in the mixer's
        // MIC strip).
        studio_round_button(Icon::Headphones, Some(Message::ToggleInputMonitor))
            .color(if bar.input_monitor_enabled {
                Studio::ACCENT
            } else {
                Studio::TEXT_SECONDARY
            })
            .size(ROUND_BUTTON),
        Space::new().width(12),
        rec_pill(c.taking, c.rec.clone()),
    ]
    .align_y(Vertical::Center);
    if bar.busy {
        center =
            center
                .push(Space::new().width(14))
                .push(spinner(13.0, 2.0, Studio::TEXT_SECONDARY));
    }

    let slots = Arranged::new(
        vec![left, right.into(), center.into()],
        Size::new(Length::Fill, Length::Fill),
        Box::new(arrange_bar),
    );
    container(column![
        container(slots)
            .width(Length::Fill)
            .height(BAR_HEIGHT - 1.0)
            .padding([0.0, BAR_PADDING]),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(fill(Studio::BORDER)),
    ])
    .width(Length::Fill)
    .height(BAR_HEIGHT)
    .style(fill(Studio::SURFACE))
    .into()
}

/// `_BackButton`: a 30 px square hit area with a 20 px chevron.
fn back_button<'a>() -> Element<'a, Message> {
    button(
        container(icons::icon(Icon::ChevronLeft, 20.0, Studio::TEXT_SECONDARY))
            .center_x(30)
            .center_y(30),
    )
    .padding(0)
    .style(bare_button)
    .on_press(Message::Back)
    .into()
}

/// `_ZoomIcon`: a 15 px zoom glyph with a 2 px pad; a tap nudges the zoom.
fn zoom_icon<'a>(icon: Icon, factor: f64) -> Element<'a, Message> {
    button(container(icons::icon(icon, 15.0, Studio::TEXT_SECONDARY)).padding(2))
        .padding(0)
        .style(bare_button)
        .on_press(Message::ZoomTimeline(factor))
        .into()
}

/// `_RecPill`: 32 px tall, radius 16 — a red dot (a rounded square while a
/// take is in flight) and "REC".
fn rec_pill<'a>(is_recording: bool, on_tap: Message) -> Element<'a, Message> {
    let dot = container(Space::new().width(9).height(9)).style(move |_| {
        boxed(
            Some(Studio::RECORD),
            if is_recording { 2.0 } else { 4.5 },
            None,
        )
    });
    let pill = container(
        row![
            dot,
            Space::new().width(7),
            text("REC").size(12).font(font::BOLD).color(Studio::RECORD),
        ]
        .align_y(Vertical::Center),
    )
    .height(32)
    // A decoration's border pads its child too: 12 + 1.
    .padding([0, 13])
    .align_y(Vertical::Center)
    .style(move |_| {
        boxed(
            Some(if is_recording {
                alpha(Studio::RECORD, 0.16)
            } else {
                Studio::SURFACE_RAISED
            }),
            16.0,
            Some(if is_recording {
                Studio::RECORD
            } else {
                Studio::BORDER_STRONG
            }),
        )
    });
    button(pill)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

/// `_Lcd`: the counter in an inset well (12 / 6 padding, radius 6), 14 px
/// w600 `Studio.lcd`, tabular figures, 1 px letter spacing.
fn lcd<'a>(content: String) -> Element<'a, Message> {
    container(lcd_counter(content))
        // A decoration's border pads its child too (Flutter's `Container`
        // adds `decoration.padding`): 1 px more each side.
        .padding([7, 13])
        .style(|_| boxed(Some(Studio::INSET), 6.0, Some(Studio::BORDER)))
        .into()
}

// --------------------------------------------------------------- zoom slider

/// `_ZoomSlider`: a horizontal timeline-zoom slider: dragging the knob left
/// zooms out (longer timeline), right zooms in (shorter). Continuous, like
/// Logic's zoom slider.
fn zoom_slider<'a>(value: f64) -> Element<'a, Message> {
    canvas_widget(ZoomSlider { value })
        .width(ZOOM_SLIDER_WIDTH)
        .height(ZOOM_SLIDER_HEIGHT)
        .into()
}

struct ZoomSlider {
    value: f64,
}

/// Whether the slider is held (it owes a commit on release).
#[derive(Debug, Default)]
pub struct ZoomSliderState {
    held: bool,
}

impl canvas::Program<Message> for ZoomSlider {
    type State = ZoomSliderState;

    fn update(
        &self,
        state: &mut ZoomSliderState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                state.held = true;
                Some(
                    canvas::Action::publish(Message::SetTimelineZoomLive(zoom_value_at(p.x)))
                        .and_capture(),
                )
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if state.held => {
                Some(canvas::Action::publish(Message::SetTimelineZoomLive(
                    zoom_value_at(position.x - bounds.x),
                )))
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.held =>
            {
                state.held = false;
                Some(canvas::Action::publish(Message::CommitTimelineZoom))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &ZoomSliderState,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let cy = bounds.height / 2.0;
        let (left, right) = (3.0, bounds.width - 3.0);
        let line = |color| {
            Stroke::default()
                .with_color(color)
                .with_width(3.0)
                .with_line_cap(LineCap::Round)
        };
        frame.stroke(
            &Path::line(Point::new(left, cy), Point::new(right, cy)),
            line(Studio::BORDER_STRONG),
        );
        let tx = zoom_knob_x(self.value, bounds.width);
        frame.stroke(
            &Path::line(Point::new(left, cy), Point::new(tx, cy)),
            line(Studio::TEXT_SECONDARY),
        );
        frame.fill(&Path::circle(Point::new(tx, cy), 5.0), Studio::TEXT_PRIMARY);
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &ZoomSliderState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.held || cursor.is_over(bounds) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
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

// ------------------------------------------------------------ the counter

/// One line of text with tabular figures and letter spacing (see the module
/// docs): every digit takes the widest digit's advance, centred in it, and
/// every character is followed by `letter_spacing`.
struct TabularText {
    content: String,
    size: f32,
    font: Font,
    color: Color,
    letter_spacing: f32,
}

/// The laid-out characters (each one's x and own advance) and the line's
/// size.
#[derive(Debug, Default, Clone)]
struct TabularLayout {
    glyphs: Vec<(char, f32, f32)>,
    width: f32,
    height: f32,
}

impl TabularText {
    fn text_at<'s>(&self, content: &'s str) -> core_text::Text<&'s str, Font> {
        core_text::Text {
            content,
            bounds: Size::INFINITE,
            size: Pixels(self.size),
            line_height: core_text::LineHeight::Relative(LCD_LINE_HEIGHT),
            font: self.font,
            align_x: core_text::Alignment::Left,
            align_y: Vertical::Top,
            shaping: core_text::Shaping::Advanced,
            wrapping: core_text::Wrapping::None,
        }
    }

    fn width_of(&self, content: &str) -> f32 {
        <Renderer as core_text::Renderer>::Paragraph::with_text(self.text_at(content))
            .min_bounds()
            .width
    }

    /// A character's advance: measured between two digits, so a space is
    /// not dropped as trailing whitespace.
    fn advance(&self, c: char) -> f32 {
        self.width_of(&format!("0{c}0")) - self.width_of("00")
    }

    fn arrange(&self) -> TabularLayout {
        let cell = ('0'..='9').map(|d| self.advance(d)).fold(0.0, f32::max);
        let mut x = 0.0;
        let mut glyphs = Vec::new();
        for c in self.content.chars() {
            let own = self.advance(c);
            let slot = if c.is_ascii_digit() { cell } else { own };
            glyphs.push((c, x + (slot - own) / 2.0, own));
            x += slot + self.letter_spacing;
        }
        TabularLayout {
            glyphs,
            width: x,
            height: self.size * LCD_LINE_HEIGHT,
        }
    }
}

/// The counter's width for `content` (the tests'): every digit as wide as
/// the widest, so the readout never changes width as the clock runs.
#[cfg(test)]
pub(crate) fn counter_width(content: &str) -> f32 {
    lcd_counter(content.to_string()).arrange().width
}

/// The counter's text: 14 px w600 `Studio.lcd`, 1 px letter spacing.
fn lcd_counter(content: String) -> TabularText {
    TabularText {
        content,
        size: 14.0,
        font: font::SEMIBOLD,
        color: Studio::LCD,
        letter_spacing: 1.0,
    }
}

impl<M> Widget<M, Theme, Renderer> for TabularText {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<TabularLayout>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(TabularLayout::default())
    }

    fn layout(&mut self, tree: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        let arranged = self.arrange();
        let size = Size::new(arranged.width, arranged.height);
        *tree.state.downcast_mut::<TabularLayout>() = arranged;
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, size))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let arranged = tree.state.downcast_ref::<TabularLayout>();
        let bounds = layout.bounds();
        for &(c, x, own) in &arranged.glyphs {
            let t = self.text_at("");
            renderer.fill_text(
                core_text::Text {
                    content: c.to_string(),
                    bounds: Size::new(own + 1.0, arranged.height),
                    size: t.size,
                    line_height: t.line_height,
                    font: t.font,
                    align_x: t.align_x,
                    align_y: t.align_y,
                    shaping: t.shaping,
                    wrapping: t.wrapping,
                },
                Point::new(bounds.x + x, bounds.y),
                self.color,
                *viewport,
            );
        }
    }

    fn operate(
        &mut self,
        _: &mut Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.content);
    }
}

impl<'a, M: 'a> From<TabularText> for Element<'a, M> {
    fn from(t: TabularText) -> Self {
        Element::new(t)
    }
}
