//! Port of `ui/song_practice_detail/widgets/ruler_bar.dart`: the interactive
//! time ruler. It is split like a track row: an empty corner over the
//! track-header column, then the timeline ticks. Dragging the ruler body
//! moves the playhead (scrub); the loop region (defaulting to the full
//! timeline) is resized only by dragging its left/right edge handles, so
//! scrubbing isn't hijacked.
//!
//! Plain data in ([`RulerBar`]: the visible span, the view's left edge, the
//! playhead and the loop region, as the detail view model and its playback
//! controller hold them), [`Message`]s out, each named after the call the
//! Dart widget made: `vm.seek`, `playback.setLoopStart`,
//! `playback.setLoopEnd`. The geometry is in pure functions the tests check.
//!
//! How the Dart structure maps:
//!
//! - The body's `GestureDetector(onTapUp, onHorizontalDragUpdate)` and the
//!   two `_LoopHandle`s are one canvas ([`RulerDrag`] is their recognizer
//!   state), hit-tested as the `Stack` was: the end handle over the start
//!   handle over the body, inside the ruler. A press becomes a horizontal
//!   drag once it has travelled more than [`HIT_SLOP`] across (a mouse's
//!   `kPrecisePointerHitSlop`); the move that crossed it is swallowed
//!   (`DragStartBehavior.start`). A press released before that, within
//!   [`TAP_SLOP`] of where it went down, is a tap.
//! - `supportedDevices: kPointerDragDevices` (no trackpad): iced delivers a
//!   two-finger trackpad swipe as `WheelScrolled`, never as a button drag,
//!   and the ruler takes no wheel input, so a swipe over a handle or the
//!   body leaves them alone by construction.
//! - The `RepaintBoundary` around the ticks: the [`TimeRulerPainter`]
//!   canvas caches its drawing by its inputs, so nothing but a change of
//!   the view redraws it; the playhead is its own canvas on top
//!   ([`PlayheadPainter`], Dart's `PlayheadLayer`), taking no input.
//! - The `Stack` clipped its handles to the ruler (`Clip.hardEdge`): a
//!   handle's grip is drawn clipped to the ticks' width here too.

use std::time::Duration;

use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{canvas as canvas_widget, container, row, stack, Space};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};
use vocara_model::util::dart_num::{clamp_f64, round_to_i64};

use crate::ui::core::theme::{alpha, fill, Studio};
use crate::ui::core::widgets::waveform_painter::{PlayheadPainter, TimeRulerPainter};

#[cfg(test)]
mod tests;

/// The ruler's height (`rulerHeight`).
pub const RULER_HEIGHT: f32 = 28.0;

/// How wide a loop handle is to grab (`_LoopHandle.width`).
pub const LOOP_HANDLE_WIDTH: f32 = 16.0;

/// The width of the track-header column the ruler's corner sits over: the
/// track row's `kTrackHeaderW` (`track_row::TRACK_HEADER_W` where that port
/// is in; the value is the same).
pub const TRACK_HEADER_W: f32 = 208.0;

/// The timeline's side inset, matching the lanes and drop boxes below so
/// ticks stay aligned with the waveforms: the track row's
/// `kTimelineHMargin` (`track_row::TIMELINE_H_MARGIN`).
pub const TIMELINE_H_MARGIN: f32 = 14.0;

/// How far a mouse press travels across before it is a horizontal drag
/// (`kPrecisePointerHitSlop`, the drag recognizer's slop for a mouse).
pub const HIT_SLOP: f64 = 1.0;

/// How far a press may stray from where it went down and still be a tap
/// (`kTouchSlop`, the tap recognizer's slop).
pub const TAP_SLOP: f32 = 18.0;

/// What the ruler asks of the detail view model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A tap anywhere on the ruler (a loop handle included), or each move of
    /// a drag on its body: move the playhead here (`vm.seek`).
    Seek(Duration),
    /// A drag on the loop's start handle (`playback.setLoopStart`), in ms.
    SetLoopStart(i64),
    /// A drag on the loop's end handle (`playback.setLoopEnd`), in ms.
    SetLoopEnd(i64),
}

/// What the ruler shows: `RulerBar`'s constructor parameters and the
/// playback controller's state it read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RulerBar {
    /// The visible span mapped across the ticks (`timelineDurationMs`: the
    /// screen passes `playback.viewSpanMs`).
    pub timeline_duration_ms: i64,
    /// The absolute time at the ticks' left edge (`playback.viewStartMs`).
    pub view_start_ms: i64,
    /// The playhead (`vm.position`).
    pub position: Duration,
    /// `playback.loopStartMs`.
    pub loop_start_ms: i64,
    /// `playback.loopEndMs`.
    pub loop_end_ms: i64,
    /// `playback.loopEnabled`: the band's tint is stronger when it loops.
    pub loop_enabled: bool,
}

/// A loop edge: which handle a drag moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

// ------------------------------------------------------------------ geometry

/// The x of `ms` on ticks `w` wide showing `dur` ms from `view_start_ms`
/// (`xOf`). Unclamped: a loop edge off the view lies off the ruler.
pub fn x_of(ms: i64, view_start_ms: i64, dur: i64, w: f64) -> f64 {
    (ms - view_start_ms) as f64 / dur as f64 * w
}

/// The time at `dx` on the ticks (`msAt`): the view's left edge plus the
/// fraction of the span, clamped to the view, rounded half away from zero.
pub fn ms_at(dx: f64, view_start_ms: i64, dur: i64, w: f64) -> i64 {
    round_to_i64(view_start_ms as f64 + clamp_f64(dx / w, 0.0, 1.0) * dur as f64)
}

/// The loop band's left and right edges: the fill is clamped to the
/// viewport so a loop overflowing the left edge still tints the whole
/// visible region (and never overshoots the right); the edge handles stay
/// UNCLAMPED so they track the true loop edges. (`w > 0` wherever this is
/// called, so the clamp's bounds are in order.)
pub fn loop_fill(start_x: f64, end_x: f64, w: f64) -> (f64, f64) {
    (clamp_f64(start_x, 0.0, w), clamp_f64(end_x, 0.0, w))
}

/// The band's tint: the accent at 20 % while it loops, 7 % otherwise.
pub fn loop_fill_color(loop_enabled: bool) -> Color {
    alpha(Studio::ACCENT, if loop_enabled { 0.20 } else { 0.07 })
}

/// Whether the end handle also takes a drag that starts leftward, moving the
/// START: it is drawn over the start's grip when the region is narrower
/// than half a handle, and a drag leftward there must move the start, or
/// the region could only be widened to the right. Wider, the start's grip is
/// still there to take, and a leftward drag on the end's own grip shrinks
/// the region.
pub fn end_takes_leftward(start_x: f64, end_x: f64) -> bool {
    (end_x - start_x).abs() < f64::from(LOOP_HANDLE_WIDTH) / 2.0
}

/// The handle under `x` on ticks `w` wide, if any: the end's over the
/// start's, each [`LOOP_HANDLE_WIDTH`] wide about its edge, and only inside
/// the ruler (a handle reaching past it is clipped there).
pub fn handle_at(x: f64, w: f64, start_x: f64, end_x: f64) -> Option<Edge> {
    if !(0.0..w).contains(&x) {
        return None;
    }
    let half = f64::from(LOOP_HANDLE_WIDTH) / 2.0;
    let on = |center: f64| (center - half..center + half).contains(&x);
    if on(end_x) {
        Some(Edge::End)
    } else if on(start_x) {
        Some(Edge::Start)
    } else {
        None
    }
}

/// Where a handle drag puts its edge: where the edge was when the drag
/// began, plus the whole distance the pointer has moved since. Adding each
/// tick's own rounded delta lost the fraction every time, so at full zoom
/// (about 1.3 ms per px) a slow drag left the handle trailing the pointer,
/// or not moving.
pub fn dragged_edge_ms(base_ms: i64, dx: f64, ms_per_px: f64) -> i64 {
    base_ms + round_to_i64(dx * ms_per_px)
}

/// A whole-millisecond `Duration` (`Duration(milliseconds: ms)`); a time on
/// the ruler is never negative.
fn millis(ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

// ---------------------------------------------------------------------- view

/// The ruler: [`RULER_HEIGHT`] tall, `Studio.surface`, inset by
/// [`TIMELINE_H_MARGIN`]; the corner over the track headers, then the ticks
/// with the loop band, its handles and the playhead.
pub fn ruler_bar<'a>(ruler: RulerBar) -> Element<'a, Message> {
    let dur = ruler.timeline_duration_ms;
    // Corner over the track-header column — keeps ticks aligned with each
    // lane's waveform. Its right border is inside its width.
    let corner = row![
        Space::new()
            .width(TRACK_HEADER_W - 1.0)
            .height(Length::Fill),
        container(Space::new())
            .width(1)
            .height(Length::Fill)
            .style(fill(Studio::BORDER)),
    ];
    let ticks = canvas_widget(TimeRulerPainter {
        timeline_duration_ms: dur,
        tick_color: Studio::GRID_STRONG,
        text_color: Studio::TEXT_SECONDARY,
        start_ms: ruler.view_start_ms,
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let lane = stack![
        canvas_widget(LoopBand { ruler })
            .width(Length::Fill)
            .height(Length::Fill),
        ticks,
        canvas_widget(Handles { ruler })
            .width(Length::Fill)
            .height(Length::Fill),
        canvas_widget(PlayheadPainter {
            position_ms: i64::try_from(ruler.position.as_millis()).unwrap_or(i64::MAX),
            timeline_duration_ms: dur,
            color: Studio::PLAYHEAD,
            show_head: true,
            start_ms: ruler.view_start_ms,
        })
        .width(Length::Fill)
        .height(Length::Fill),
    ];
    container(row![corner, container(lane).width(Length::Fill).clip(true)])
        .width(Length::Fill)
        .height(RULER_HEIGHT)
        .padding([0.0, TIMELINE_H_MARGIN])
        .style(fill(Studio::SURFACE))
        .into()
}

/// Whether the ruler is live: a span and a width to map it across. With
/// neither, Dart drew the ticks alone — no band, handles or input.
fn live(dur: i64, w: f64) -> bool {
    dur > 0 && w > 0.0
}

/// The loop band, under the ticks (`IgnorePointer(ColoredBox)`).
struct LoopBand {
    ruler: RulerBar,
}

impl<M> canvas::Program<M> for LoopBand {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let r = self.ruler;
        let w = f64::from(bounds.width);
        let dur = r.timeline_duration_ms;
        let mut frame = Frame::new(renderer, bounds.size());
        if live(dur, w) {
            let (left, right) = loop_fill(
                x_of(r.loop_start_ms, r.view_start_ms, dur, w),
                x_of(r.loop_end_ms, r.view_start_ms, dur, w),
                w,
            );
            // A band whose end is left of its start has a negative width in
            // Dart (nothing painted).
            if right > left {
                frame.fill_rectangle(
                    Point::new(left as f32, 0.0),
                    Size::new((right - left) as f32, bounds.height),
                    loop_fill_color(r.loop_enabled),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The edge handles and every pointer input on the ruler (see the module
/// docs).
struct Handles {
    ruler: RulerBar,
}

/// The ruler's recognizer state: the press in flight, if any.
#[derive(Debug, Default)]
pub struct RulerDrag {
    press: Option<Press>,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    /// What the press landed on: a handle, or the body (`None`).
    handle: Option<Edge>,
    /// Where it went down, on the ticks.
    down: Point,
    /// Where the pointer last was, in window coordinates (a tap seeks where
    /// it is released: `onTapUp`'s position).
    last: Point,
    /// The horizontal travel before the drag was accepted.
    pending: f64,
    /// Whether the pointer strayed past the tap slop.
    strayed: bool,
    /// Whether it became a horizontal drag.
    dragging: bool,
    /// A handle drag: how far the pointer has moved since the drag began
    /// (`_dx`)...
    dx: f64,
    /// ...and the edge it moves with where that edge was then (`_edge`,
    /// `_baseMs`), chosen by the drag's first movement.
    edge: Option<(Edge, i64)>,
}

impl Handles {
    fn xs(&self, w: f64) -> (f64, f64) {
        let r = self.ruler;
        let dur = r.timeline_duration_ms;
        (
            x_of(r.loop_start_ms, r.view_start_ms, dur, w),
            x_of(r.loop_end_ms, r.view_start_ms, dur, w),
        )
    }

    fn seek_at(&self, dx: f64, w: f64) -> Message {
        let r = self.ruler;
        Message::Seek(millis(ms_at(
            dx,
            r.view_start_ms,
            r.timeline_duration_ms,
            w,
        )))
    }

    /// The edge a handle drag moves, and where it was: chosen by the drag's
    /// first movement — the start, when the end handle lies over the
    /// start's grip and the drag heads left.
    fn choose_edge(&self, handle: Edge, dx: f64, w: f64) -> (Edge, i64) {
        let r = self.ruler;
        let (start_x, end_x) = self.xs(w);
        match handle {
            Edge::End if dx < 0.0 && end_takes_leftward(start_x, end_x) => {
                (Edge::Start, r.loop_start_ms)
            }
            Edge::End => (Edge::End, r.loop_end_ms),
            Edge::Start => (Edge::Start, r.loop_start_ms),
        }
    }
}

impl canvas::Program<Message> for Handles {
    type State = RulerDrag;

    fn update(
        &self,
        state: &mut RulerDrag,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let w = f64::from(bounds.width);
        let dur = self.ruler.timeline_duration_ms;
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if !live(dur, w) {
                    return None;
                }
                let p = cursor.position_in(bounds)?;
                let (start_x, end_x) = self.xs(w);
                state.press = Some(Press {
                    handle: handle_at(f64::from(p.x), w, start_x, end_x),
                    down: p,
                    last: Point::new(p.x + bounds.x, p.y + bounds.y),
                    pending: 0.0,
                    strayed: false,
                    dragging: false,
                    dx: 0.0,
                    edge: None,
                });
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let press = state.press.as_mut()?;
                let local = Point::new(position.x - bounds.x, position.y - bounds.y);
                let delta = f64::from(position.x - press.last.x);
                press.last = *position;
                if press.down.distance(local) > TAP_SLOP {
                    press.strayed = true;
                }
                if !press.dragging {
                    press.pending += delta;
                    if press.pending.abs() > HIT_SLOP {
                        // The drag wins: it starts here, and the movement up
                        // to now is swallowed (`DragStartBehavior.start`); a
                        // handle's drag starts afresh (`onHorizontalDragStart`).
                        press.dragging = true;
                        press.dx = 0.0;
                        press.edge = None;
                    }
                    return None;
                }
                if !live(dur, w) {
                    return None;
                }
                let Some(handle) = press.handle else {
                    return Some(canvas::Action::publish(self.seek_at(f64::from(local.x), w)));
                };
                press.dx += delta;
                let (edge, base) = match press.edge {
                    Some(chosen) => chosen,
                    None => {
                        if press.dx == 0.0 {
                            return None;
                        }
                        let chosen = self.choose_edge(handle, press.dx, w);
                        press.edge = Some(chosen);
                        chosen
                    }
                };
                let ms = dragged_edge_ms(base, press.dx, dur as f64 / w);
                Some(canvas::Action::publish(match edge {
                    Edge::Start => Message::SetLoopStart(ms),
                    Edge::End => Message::SetLoopEnd(ms),
                }))
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let press = state.press.take()?;
                if press.dragging || press.strayed || !live(dur, w) {
                    return None;
                }
                // A tap: on a handle it still seeks, as on the rest of the
                // ruler (the default loop starts at 0, and its handle covered
                // the click back to 0:00).
                let x = press.last.x - bounds.x;
                Some(canvas::Action::publish(self.seek_at(f64::from(x), w)))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _: &RulerDrag,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let w = f64::from(bounds.width);
        let mut frame = Frame::new(renderer, bounds.size());
        if live(self.ruler.timeline_duration_ms, w) {
            let (start_x, end_x) = self.xs(w);
            for center in [start_x, end_x] {
                // A 3 px grip centred in the handle, clipped to the ruler.
                let left = (center as f32 - 1.5).max(0.0);
                let right = (center as f32 + 1.5).min(bounds.width);
                if right > left {
                    frame.fill(
                        &Path::rounded_rectangle(
                            Point::new(left, 0.0),
                            Size::new(right - left, bounds.height),
                            2.0.into(),
                        ),
                        Studio::ACCENT,
                    );
                }
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &RulerDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let w = f64::from(bounds.width);
        let dragging_handle = state
            .press
            .is_some_and(|p| p.dragging && p.handle.is_some());
        let over_handle = live(self.ruler.timeline_duration_ms, w)
            && cursor.position_in(bounds).is_some_and(|p| {
                let (start_x, end_x) = self.xs(w);
                handle_at(f64::from(p.x), w, start_x, end_x).is_some()
            });
        if dragging_handle || over_handle {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}
