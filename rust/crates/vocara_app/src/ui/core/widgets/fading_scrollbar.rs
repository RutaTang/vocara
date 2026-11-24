//! The scrollbar Flutter's desktop scroll behavior put on every vertical
//! scrollable (Material's `Scrollbar` over `RawScrollbar`, Flutter 3.38 on
//! macOS, with a mouse), drawn and driven here over an iced scrollable
//! whose own scrollbar is hidden (`Scrollbar::hidden()`: nothing drawn,
//! nothing to grab); the iced scrollable keeps the content, the wheel and
//! the layout.
//!
//! The thumb is `ScrollbarPainter`'s as Material set it up: 8 wide
//! (`_kScrollbarThickness`), 2 in from the right edge (`_kScrollbarMargin`),
//! its length the share of the content the view shows of the view's height
//! but at least 48 (`_kScrollbarMinLength`), and its place that share of
//! its travel (the view's height less its length) the view has scrolled
//! (`_setThumbExtent`, `_getScrollToTrack`); radius 8, no track. Its colors
//! come from the style (`theme::scrollbar`).
//!
//! How it moves (`RawScrollbarState`, `_MaterialScrollbarState`):
//!
//! - It is hidden until the view scrolls (the wheel, a drag of the thumb, a
//!   press on the track, the app) or the pointer comes over the bar; it
//!   then fades in over 300 ms and, 600 ms after the scroll ends, the
//!   pointer leaves the bar or the view, or a drag ends, fades out over
//!   300 ms, both on `Curves.fastOutSlowIn` (`fadeDuration`, `timeToFade`).
//!   The pointer holds it only as it moves over it (Flutter's hover events
//!   came with motion only): a scroll that ends under a resting pointer
//!   starts the 600 ms too.
//! - Under the pointer the thumb's color moves to its hovered one over
//!   200 ms, linear (Material's hover animation); dragged, it takes the
//!   dragged one at once.
//! - The pointer is over the bar on its track: the 12 px strip the thumb
//!   runs in (the thumb and its margins), the view's full height. While the
//!   bar is hidden a wider area brings it in: the rectangle around the track
//!   and a 48 px square on the thumb's centre, a strip 18 px wider inward
//!   (`hitTestInteractive(forHover: true)`).
//! - While it shows, the track is the bar's: the content under it sees no
//!   pointer. Pressing the thumb drags it, and the view follows the pointer
//!   in proportion (the content's scroll range over the thumb's travel,
//!   `getTrackToScroll`), from where the thumb was grabbed, clamped to the
//!   content. Pressing the track elsewhere pages: the view scrolls 0.8 of
//!   its height (`ScrollIncrementType.page`) down when the press is below
//!   the thumb's top, up otherwise, over 100 ms on `Curves.easeInOut`
//!   (`handleTrackTapDown`). Hidden, the bar takes no press: the content
//!   under it does.
//! - A view with nothing to scroll shows none.
//!
//! The time comes from the window's frames, so a test injects it. What an
//! event starts (a fade, the hover's color, a page's scroll) starts at the
//! next frame, as a Flutter ticker did; the widget asks for frames only
//! while something moves, or for one when the bar is due to fade.
//!
//! Material's scroll behavior gave a horizontal scrollable no scrollbar, and
//! `ScrollConfiguration(scrollbars: false)` took it off a vertical one:
//! those take `Scrollbar::hidden()` without this widget.
//!
//! Left as it is, iced having no counterpart: the wheel over the shown
//! track scrolls the view, as Flutter's bar passed it to its scrollable,
//! but the iced scrollable offers it to the content under the track first;
//! and a page that would move the view less than half a pixel jumps instead
//! (`nearEqual` against `1 / devicePixelRatio`, taken as a Retina
//! display's 2: iced does not tell a widget the scale).

use std::rc::Rc;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::widget::scrollable::{
    self, AbsoluteOffset, Direction, Scrollable, Scrollbar, Status, Style,
};
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle, Renderer,
    Size, Theme, Vector,
};

use crate::ui::core::motion::{self, Controller, Curve, Lerp};
use crate::ui::core::theme;

/// How long the thumb takes to fade in or out (`_kScrollbarFadeDuration`).
pub const FADE: Duration = Duration::from_millis(300);

/// How long after the scrolling stops the thumb starts to fade
/// (`_kScrollbarTimeToFade`).
pub const TIME_TO_FADE: Duration = Duration::from_millis(600);

/// How long the thumb's color takes to its hovered one
/// (`_hoverAnimationController`).
pub const HOVER: Duration = Duration::from_millis(200);

/// How long a press on the track takes to page (`handleTrackTapDown`'s
/// `moveTo`).
pub const PAGE: Duration = Duration::from_millis(100);

/// How much of the view a press on the track pages
/// (`ScrollIncrementType.page`).
pub const PAGE_SHARE: f32 = 0.8;

/// How thick the thumb is (`_kScrollbarThickness`).
pub const THICKNESS: f32 = 8.0;

/// The room on each side of the thumb across the bar (`_kScrollbarMargin`,
/// the painter's `crossAxisMargin`).
pub const CROSS_AXIS_MARGIN: f32 = 2.0;

/// The room at each end of the track (the painter's `mainAxisMargin`).
const MAIN_AXIS_MARGIN: f32 = 0.0;

/// The thumb's least length (`_kScrollbarMinLength`).
pub const MIN_LENGTH: f32 = 48.0;

/// The thumb's least length while the view is overscrolled: the painter's
/// `minOverscrollLength`, `RawScrollbar.minThumbLength`'s default
/// (`_kMinThumbExtent`), which Material left in place.
const MIN_OVERSCROLL_LENGTH: f32 = 18.0;

/// The square around the thumb's centre that brings a hidden bar in under
/// the pointer (`_kMinInteractiveSize`).
const MIN_INTERACTIVE_SIZE: f32 = 48.0;

/// Less overflow than this is nothing to scroll (`precisionErrorTolerance`,
/// widened to absorb f32 layout noise).
const PRECISION: f32 = 1e-3;

/// A page shorter than this jumps (`toleranceFor(...).distance`,
/// `1 / devicePixelRatio`, see the module docs).
const NEAR: f32 = 0.5;

/// The style's statuses the thumb's colors are read at.
const IDLE: Status = Status::Active {
    is_horizontal_scrollbar_disabled: false,
    is_vertical_scrollbar_disabled: false,
};
const HOVERED: Status = Status::Hovered {
    is_horizontal_scrollbar_hovered: false,
    is_vertical_scrollbar_hovered: true,
    is_horizontal_scrollbar_disabled: false,
    is_vertical_scrollbar_disabled: false,
};
const DRAGGED: Status = Status::Dragged {
    is_horizontal_scrollbar_dragged: false,
    is_vertical_scrollbar_dragged: true,
    is_horizontal_scrollbar_disabled: false,
    is_vertical_scrollbar_disabled: false,
};

/// The scrollbar's style, which the wrapper and its scrollable share.
type SharedStyle<'a> = Rc<dyn Fn(&Theme, Status) -> Style + 'a>;

/// `scrollable` under the app's scrollbar (`theme::scrollbar`), as the
/// module docs say. It scrolls vertically: its direction is set here.
pub fn fading_scrollbar<'a, M: 'a>(scrollable: Scrollable<'a, M>) -> Element<'a, M> {
    fading_scrollbar_with(scrollable, theme::scrollbar)
}

/// `scrollable` under the scrollbar whose colors and radius `style` gives
/// (its vertical rail's scroller, idle, hovered and dragged), as the module
/// docs say.
pub fn fading_scrollbar_with<'a, M: 'a>(
    scrollable: Scrollable<'a, M>,
    style: impl Fn(&Theme, Status) -> Style + 'a,
) -> Element<'a, M> {
    let style: SharedStyle<'a> = Rc::new(style);
    let looks = Rc::clone(&style);
    // The iced scrollable draws its container; its own scrollbar is hidden.
    let scrollable = vertical(scrollable).style(move |theme, status| looks(theme, status));
    Element::new(FadingScrollbar {
        scrollable: scrollable.into(),
        style,
    })
}

/// `scrollable` scrolling vertically, its own scrollbar hidden.
fn vertical<M>(scrollable: Scrollable<'_, M>) -> Scrollable<'_, M> {
    scrollable.direction(Direction::Vertical(Scrollbar::hidden()))
}

/// `scrollable` scrolling vertically with no scrollbar
/// (`ScrollConfiguration(scrollbars: false)`): the wheel scrolls it;
/// nothing is drawn and nothing takes the pointer (iced's own scrollbar,
/// even clear, would take the strip at its edge from the content).
pub fn no_scrollbar<M>(scrollable: Scrollable<'_, M>) -> Scrollable<'_, M> {
    vertical(scrollable)
}

/// A scrollable that will take the fading scrollbar, still taking its
/// size: what [`Fading::fading`] makes of one, to finish as the scrollable
/// was.
pub struct FadingScrollable<'a, M> {
    scrollable: Scrollable<'a, M>,
    style: scrollable::StyleFn<'a, Theme>,
}

impl<M> FadingScrollable<'_, M> {
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.height(height);
        self
    }
}

impl<'a, M: 'a> From<FadingScrollable<'a, M>> for Element<'a, M> {
    fn from(fading: FadingScrollable<'a, M>) -> Self {
        fading_scrollbar_with(fading.scrollable, fading.style)
    }
}

/// `.fading()` where an iced scrollable took `.style(theme::scrollbar)`.
pub trait Fading<'a, M> {
    /// Under the app's scrollbar (see the module docs), scrolling
    /// vertically.
    fn fading(self) -> FadingScrollable<'a, M>;

    /// Under the scrollbar `style` gives, scrolling vertically.
    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M>;
}

impl<'a, M: 'a> Fading<'a, M> for Scrollable<'a, M> {
    fn fading(self) -> FadingScrollable<'a, M> {
        self.fading_with(theme::scrollbar)
    }

    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M> {
        FadingScrollable {
            scrollable: vertical(self),
            style: Box::new(style),
        }
    }
}

struct FadingScrollbar<'a, M> {
    scrollable: Element<'a, M>,
    style: SharedStyle<'a>,
}

impl<M> FadingScrollbar<'_, M> {
    /// The thumb's color at `opacity` (`_thumbColor` resolved, then
    /// `_paintThumb`'s `color.withOpacity(color.opacity * fade)`, its alpha
    /// in 8 bits each time).
    fn thumb_color(&self, theme: &Theme, dragged: bool, hover: f32, opacity: f32) -> Color {
        let color = |status| match (self.style)(theme, status)
            .vertical_rail
            .scroller
            .background
        {
            Background::Color(color) => color,
            Background::Gradient(_) => Color::TRANSPARENT,
        };
        let color = if dragged {
            color(DRAGGED)
        } else {
            color(IDLE).lerp(color(HOVERED), hover)
        };
        let alpha = (color.a * 255.0).round();
        Color {
            a: (alpha * opacity).round() / 255.0,
            ..color
        }
    }
}

/// What `ScrollbarPainter` lays out for a frame, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bar {
    /// Where the thumb runs, with its margins (`_trackRect`).
    track: Rectangle,
    thumb: Rectangle,
    /// The content's scroll range (`maxScrollExtent - minScrollExtent`).
    range: f32,
    /// How far the thumb can move (`_traversableTrackExtent -
    /// _thumbExtent`).
    travel: f32,
}

impl Bar {
    /// The bar at the right of `bounds` over `content` px of content
    /// scrolled `pixels` down (`ScrollbarPainter.paint`, no padding); none
    /// when there is nothing to scroll or no room.
    fn new(bounds: Rectangle, content: f32, pixels: f32) -> Option<Self> {
        let viewport = bounds.height;
        let range = content - viewport;
        let track_extent = viewport;
        let traversable = track_extent - 2.0 * MAIN_AXIS_MARGIN;
        if range <= PRECISION || traversable <= 0.0 {
            return None;
        }
        // `_setThumbExtent`.
        let inside =
            viewport - (-pixels).clamp(0.0, viewport) - (pixels - range).clamp(0.0, viewport);
        let visible = (inside / content).clamp(0.0, 1.0);
        let extent = traversable
            .min(MIN_OVERSCROLL_LENGTH)
            .max(traversable * visible);
        let overscrolled = 1.0 - inside / viewport;
        let least = MIN_LENGTH.min(traversable);
        let least = if pixels > 0.0 && range - pixels > 0.0 {
            least
        } else {
            least * (1.0 - overscrolled.clamp(0.0, 0.2) / 0.2)
        };
        let extent = extent.clamp(least, traversable);
        // `_getScrollToTrack`, then the leading margin.
        let past = (pixels / range).clamp(0.0, 1.0);
        let offset = past * (traversable - extent) + MAIN_AXIS_MARGIN;
        let x = bounds.x + bounds.width - THICKNESS - CROSS_AXIS_MARGIN;
        Some(Self {
            track: Rectangle {
                x: x - CROSS_AXIS_MARGIN,
                y: bounds.y,
                width: THICKNESS + 2.0 * CROSS_AXIS_MARGIN,
                height: track_extent,
            },
            thumb: Rectangle {
                x,
                y: bounds.y + offset,
                width: THICKNESS,
                height: extent,
            },
            range,
            travel: traversable - extent,
        })
    }

    /// The pointer at `at` is over the bar for the hover
    /// (`hitTestInteractive(forHover: true)`, a mouse): on the track while
    /// it shows; while it is hidden, also near the thumb.
    fn hovered(&self, at: Point, shown: bool) -> bool {
        if shown {
            return self.track.contains(at);
        }
        let centre = self.thumb.center();
        let reach = MIN_INTERACTIVE_SIZE / 2.0;
        let left = self.track.x.min(centre.x - reach);
        let top = self.track.y.min(centre.y - reach);
        let right = (self.track.x + self.track.width).max(centre.x + reach);
        let bottom = (self.track.y + self.track.height).max(centre.y + reach);
        Rectangle {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
        .contains(at)
    }

    /// The bar takes the pointer at `at` from the content (`hitTest`,
    /// `hitTestInteractive`): on the track while it shows.
    fn takes(&self, at: Point, shown: bool) -> bool {
        shown && self.track.contains(at)
    }

    /// The pointer at `at` is on the thumb (`hitTestOnlyThumbInteractive`).
    fn on_thumb(&self, at: Point, shown: bool) -> bool {
        shown && self.thumb.contains(at)
    }

    /// How far down the thumb is at `pixels` (`getThumbScrollOffset`).
    fn thumb_offset(&self, pixels: f32) -> f32 {
        (pixels / self.range).clamp(0.0, 1.0) * self.travel
    }

    /// The scroll a move of the thumb by `along` makes
    /// (`getTrackToScroll`); none when the thumb fills its track.
    fn track_to_scroll(&self, along: f32) -> Option<f32> {
        (self.travel > 0.0).then(|| self.range * along / self.travel)
    }
}

/// A held drag of the thumb.
#[derive(Debug, Clone, Copy)]
struct Drag {
    /// Where it was pressed, down (`_startDragScrollbarAxisOffset`).
    start: f32,
    /// Where it last moved the view (`_lastDragUpdateOffset`).
    last: Point,
    /// Where the thumb was then (`_startDragThumbOffset`).
    thumb: f32,
    /// It still drives the scroll (`_thumbDrag`): a scroll by anything else
    /// ends that, though the button stays down.
    live: bool,
}

/// A press on the track's scroll (`DrivenScrollActivity`).
#[derive(Debug, Clone, Copy)]
struct Page {
    from: f32,
    to: f32,
    /// Its first frame, once it had one.
    start: Option<Instant>,
}

/// What the timer is told, the last word counting.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Timer {
    Cancel,
    Start,
}

/// What the events since the last frame asked of the animations, done at
/// the next frame.
#[derive(Debug, Default, Clone, Copy)]
struct Pending {
    show: bool,
    timer: Option<Timer>,
    hover: Option<bool>,
}

/// The bar's state (`RawScrollbarState`'s and Material's).
struct State {
    fade: Controller,
    /// When the thumb is due to fade, while that is pending
    /// (`_fadeoutTimer`).
    timer: Option<Instant>,
    /// Material's hover animation.
    hover: Controller,
    /// The pointer is over the bar (`_hoverIsActive`).
    hovering: bool,
    /// The pointer is over the view (the bar's `MouseRegion`).
    inside: bool,
    /// The mouse buttons held down, as bits: the pointer hovers only with
    /// none (`PointerHoverEvent`).
    buttons: u8,
    /// The pointer went down on the shown track: the content sees nothing
    /// of it until its last button is up, as the bar's hit test had kept
    /// the content out of the press.
    held: bool,
    drag: Option<Drag>,
    page: Option<Page>,
    pending: Pending,
    /// Something changed that a frame must show.
    dirty: bool,
    /// The scroll offset down (`pixels`), once seen: what this widget set,
    /// to the fraction, or what the scrollable shows.
    pixels: Option<f32>,
    now: Option<Instant>,
}

/// `controller` is heading in or in (`isForwardOrCompleted`).
fn heading_in(controller: &Controller, now: Option<Instant>) -> bool {
    now.is_some_and(|now| {
        matches!(
            controller.status(now),
            motion::Status::Forward | motion::Status::Completed
        )
    })
}

fn button_bit(button: mouse::Button) -> u8 {
    match button {
        mouse::Button::Left => 1,
        mouse::Button::Right => 2,
        mouse::Button::Middle => 4,
        mouse::Button::Back => 8,
        mouse::Button::Forward => 16,
        mouse::Button::Other(_) => 32,
    }
}

impl State {
    fn new() -> Self {
        Self {
            fade: Controller::new(FADE, FADE),
            timer: None,
            hover: Controller::new(HOVER, HOVER),
            hovering: false,
            inside: false,
            buttons: 0,
            held: false,
            drag: None,
            page: None,
            pending: Pending::default(),
            dirty: false,
            pixels: None,
            now: None,
        }
    }

    /// The thumb's opacity as last drawn.
    fn opacity(&self) -> f32 {
        self.now
            .map_or(0.0, |now| self.fade.curved(Curve::FastOutSlowIn, None, now))
    }

    fn bar(&self, bounds: Rectangle, content: f32) -> Option<Bar> {
        Bar::new(bounds, content, self.pixels.unwrap_or(0.0))
    }

    /// What the content sees of `cursor`: nothing while the bar has it, a
    /// press the bar took or, with no button down, the shown track under it
    /// (a press the content took keeps the pointer, over the track too).
    fn content_cursor(&self, bar: Option<Bar>, cursor: mouse::Cursor) -> mouse::Cursor {
        let shown = self.opacity() > 0.0;
        let taken = self.held
            || self.buttons == 0
                && cursor
                    .position()
                    .zip(bar)
                    .is_some_and(|(at, bar)| bar.takes(at, shown));
        if taken {
            cursor.levitate()
        } else {
            cursor
        }
    }

    /// Fades the thumb in (`_fadeoutAnimationController.forward()`).
    fn show(&mut self) {
        if !heading_in(&self.fade, self.now) && !self.pending.show {
            self.pending.show = true;
            self.dirty = true;
        }
    }

    /// `_fadeoutTimer?.cancel()`.
    fn cancel_timer(&mut self) {
        if self.timer.is_some() || self.pending.timer.is_some() {
            self.pending.timer = Some(Timer::Cancel);
            self.dirty = true;
        }
    }

    /// `_maybeStartFadeoutTimer()`. On a thumb that is not in or coming in
    /// the timer could only fade what is gone: none starts, any is off.
    fn start_timer(&mut self) {
        if heading_in(&self.fade, self.now) || self.pending.show {
            self.pending.timer = Some(Timer::Start);
            self.dirty = true;
        } else {
            self.cancel_timer();
        }
    }

    /// Material's hover animation, forward or back.
    fn hover_to(&mut self, on: bool) {
        let heading = self
            .pending
            .hover
            .unwrap_or_else(|| heading_in(&self.hover, self.now));
        if heading != on {
            self.pending.hover = Some(on);
            self.dirty = true;
        }
    }

    /// The view scrolled (`ScrollUpdateNotification`) and, if `ended`, the
    /// scroll ended (`ScrollEndNotification`).
    fn scrolled(&mut self, ended: bool) {
        self.show();
        self.cancel_timer();
        if ended && !self.drag.is_some_and(|drag| drag.live) {
            self.start_timer();
        }
    }

    /// This widget scrolled the view to `to`, the scroll ending if `ended`.
    fn moved(&mut self, to: f32, ended: bool) {
        self.pixels = Some(to);
        self.dirty = true;
        self.scrolled(ended);
    }

    /// Takes in the offset the scrollable shows, `seen`, with `range` to
    /// scroll: a change this widget did not make (the wheel, the app) is a
    /// scroll that ended, and it ends a page and a drag's hold on the view
    /// (`goIdle`).
    fn observe(&mut self, seen: Option<f32>, range: f32) {
        let Some(seen) = seen else { return };
        let Some(pixels) = self.pixels else {
            self.pixels = Some(seen);
            return;
        };
        let pixels = pixels.clamp(0.0, range.max(0.0));
        if seen == pixels.round() {
            self.pixels = Some(pixels);
            return;
        }
        self.pixels = Some(seen);
        self.page = None;
        if let Some(drag) = &mut self.drag {
            drag.live = false;
        }
        self.scrolled(true);
    }

    /// The pointer moved with no button down, to `at` over the view or off
    /// it (`handleHover`, `handleHoverExit`).
    fn hover(&mut self, at: Option<Point>, bar: Option<Bar>) {
        let shown = self.opacity() > 0.0;
        match at {
            Some(at) => {
                self.inside = true;
                if bar.is_some_and(|bar| bar.hovered(at, shown)) {
                    self.hovering = true;
                    self.show();
                    self.cancel_timer();
                    self.hover_to(true);
                } else if self.hovering {
                    self.hovering = false;
                    self.start_timer();
                    self.hover_to(false);
                }
            }
            None if self.inside => {
                self.inside = false;
                self.hovering = false;
                self.start_timer();
                self.hover_to(false);
            }
            None => {}
        }
    }

    /// The frame at `at`; its time.
    fn advance(&mut self, at: Instant) -> Instant {
        let now = self.now.map_or(at, |was| was.max(at));
        self.now = Some(now);
        now
    }

    /// Where the page's scroll is at `now`, and whether it is over.
    fn page_at(&mut self, now: Instant) -> Option<(f32, bool)> {
        let page = self.page.as_mut()?;
        let start = *page.start.get_or_insert(now);
        let t = (now.duration_since(start).as_secs_f32() / PAGE.as_secs_f32()).min(1.0);
        let y = page.from + (page.to - page.from) * Curve::EaseInOut.transform(t);
        Some((y, t >= 1.0))
    }

    /// Does what the events asked, fades the thumb if it is due or there is
    /// nothing to scroll, and asks for the frames that takes.
    fn settle<M>(&mut self, now: Instant, scrolls: bool, shell: &mut Shell<'_, M>) {
        let pending = std::mem::take(&mut self.pending);
        // This frame shows it all.
        self.dirty = false;
        if pending.show {
            self.fade.forward(now);
        }
        match pending.timer {
            Some(Timer::Cancel) => self.timer = None,
            Some(Timer::Start) => self.timer = Some(now + TIME_TO_FADE),
            None => {}
        }
        match pending.hover {
            Some(true) => self.hover.forward(now),
            Some(false) => self.hover.reverse(now),
            None => {}
        }
        if !scrolls {
            // Nothing to scroll: no bar.
            self.timer = None;
            if heading_in(&self.fade, Some(now)) {
                self.fade.reverse(now);
            }
        }
        if let Some(due) = self.timer.filter(|due| *due <= now) {
            self.timer = None;
            self.fade.reverse(due);
        }
        if self.fade.is_animating(now) || self.hover.is_animating(now) || self.page.is_some() {
            shell.request_redraw();
        } else if let Some(due) = self.timer {
            shell.request_redraw_at(due);
        }
    }
}

/// Reads the scrollable's offset down as it shows it, stopping at it.
#[derive(Default)]
struct Read(Option<f32>);

impl Operation for Read {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollableState,
    ) {
        self.0.get_or_insert(translation.y);
    }
}

/// Scrolls the scrollable to an offset down, stopping at it.
struct ScrollTo(Option<f32>);

impl Operation for ScrollTo {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn ScrollableState,
    ) {
        if let Some(y) = self.0.take() {
            state.scroll_to(AbsoluteOffset {
                x: None,
                y: Some(y),
            });
        }
    }
}

/// The content's height under the scrollable laid out at `layout`.
fn content_height(layout: Layout<'_>) -> f32 {
    layout
        .children()
        .next()
        .map_or(0.0, |content| content.bounds().height)
}

/// The offset down `scrollable` shows.
fn read<M>(
    scrollable: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
) -> Option<f32> {
    let mut read = Read::default();
    scrollable
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut read);
    read.0
}

/// Scrolls `scrollable` to `y` down.
fn scroll_to<M>(
    scrollable: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    y: f32,
) {
    scrollable
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut ScrollTo(Some(y)));
}

impl<M> Widget<M, Theme, Renderer> for FadingScrollbar<'_, M> {
    fn size(&self) -> Size<Length> {
        self.scrollable.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.scrollable.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.scrollable));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.scrollable
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        let bounds = layout.bounds();
        let content = content_height(layout);
        let range = content - bounds.height;
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();
        let child = &mut children[0];
        let scrollable = &mut self.scrollable;
        let scroll = |scrollable: &mut Element<'_, M>, child: &mut Tree, y: f32| {
            scroll_to(scrollable, child, layout, renderer, y);
        };
        // A scroll since the last event (an operation of the app's).
        state.observe(read(scrollable, child, layout, renderer), range);

        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            let now = state.advance(*at);
            if let Some((y, over)) = state.page_at(now) {
                if state.pixels != Some(y) {
                    scroll(scrollable, child, y);
                    state.moved(y, false);
                }
                if over {
                    state.page = None;
                    state.start_timer();
                }
            }
            let scrolls = state.bar(bounds, content).is_some();
            state.settle(now, scrolls, shell);
        }

        let pixels = state.pixels.unwrap_or(0.0);
        let bar = state.bar(bounds, content);
        let shown = state.opacity() > 0.0;
        let mut content_cursor = state.content_cursor(bar, cursor);
        let mut pass = true;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                // The pointer goes down with its first button, and is hit
                // tested then: what it lands on has it until the last is up.
                let down = state.buttons == 0;
                state.buttons |= button_bit(*button);
                let taken = cursor
                    .position_over(bounds)
                    .zip(bar)
                    .filter(|(at, bar)| down && bar.takes(*at, shown));
                state.held |= taken.is_some();
                // The thumb's drag and the track's tap take the primary
                // button.
                if let Some((at, bar)) = taken.filter(|_| *button == mouse::Button::Left) {
                    pass = false;
                    shell.capture_event();
                    if bar.on_thumb(at, shown) {
                        // `handleThumbPress` (its hold ends a page) and
                        // `handleThumbPressStart`.
                        state.page = None;
                        state.cancel_timer();
                        state.show();
                        state.drag = Some(Drag {
                            start: at.y,
                            last: at,
                            thumb: bar.thumb_offset(pixels),
                            live: true,
                        });
                        state.dirty = true;
                    } else {
                        // `handleTrackTapDown`.
                        let step = PAGE_SHARE * bounds.height;
                        let step = if at.y > bar.thumb.y { step } else { -step };
                        let to = (pixels + step).clamp(0.0, bar.range);
                        if (to - pixels).abs() < NEAR {
                            let paging = state.page.take().is_some();
                            if to != pixels {
                                scroll(scrollable, child, to);
                                state.moved(to, true);
                            } else if paging {
                                state.start_timer();
                            }
                        } else {
                            state.page = Some(Page {
                                from: pixels,
                                to,
                                start: None,
                            });
                            state.dirty = true;
                        }
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                state.buttons &= !button_bit(*button);
                if *button == mouse::Button::Left && state.drag.take().is_some() {
                    // `handleThumbPressEnd`.
                    state.start_timer();
                    state.dirty = true;
                }
                if state.buttons == 0 {
                    state.held = false;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(drag) = state.drag {
                    let moved = cursor
                        .land()
                        .position()
                        .filter(|at| drag.live && drag.last != *at)
                        .zip(bar);
                    if let Some((at, bar)) = moved {
                        // `handleThumbPressUpdate`, `_getPrimaryDelta`: the
                        // thumb follows the pointer from where it was
                        // grabbed; should the content have changed so that
                        // takes the view against the drag, the view moves
                        // by the last step alone.
                        let from_start = at.y - drag.start;
                        let from_last = at.y - drag.last.y;
                        let to = bar.track_to_scroll(drag.thumb + from_start).map(|to| {
                            if (from_start > 0.0 && to < pixels)
                                || (from_start < 0.0 && to > pixels)
                            {
                                pixels + bar.track_to_scroll(from_last).unwrap_or(0.0)
                            } else {
                                to
                            }
                        });
                        if let Some(to) = to.filter(|to| *to != pixels) {
                            let to = to.clamp(0.0, bar.range);
                            state.drag = Some(Drag { last: at, ..drag });
                            if to != pixels {
                                scroll(scrollable, child, to);
                                state.moved(to, false);
                            }
                        }
                    }
                    shell.capture_event();
                } else if state.buttons == 0 {
                    state.hover(cursor.position_over(bounds), bar);
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if state.buttons == 0 => {
                state.hover(None, bar);
            }
            Event::Mouse(mouse::Event::WheelScrolled { .. }) => {
                let on_track = cursor
                    .position()
                    .zip(bar)
                    .is_some_and(|(at, bar)| bar.takes(at, shown));
                if state.drag.is_some() && on_track {
                    // Held by the drag: the bar's own wheel waits.
                    pass = false;
                } else {
                    // The bar hands the wheel to its scrollable.
                    content_cursor = cursor;
                }
            }
            _ => {}
        }

        if pass {
            scrollable.as_widget_mut().update(
                child,
                event,
                layout,
                content_cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            // What the event scrolled.
            state.observe(read(scrollable, child, layout, renderer), range);
        }
        if std::mem::take(&mut state.dirty) {
            shell.request_redraw();
        }
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
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let bar = state.bar(bounds, content_height(layout));
        self.scrollable.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            state.content_cursor(bar, cursor),
            viewport,
        );
        let opacity = state.opacity();
        let (Some(bar), Some(clip)) = (bar, bounds.intersection(viewport)) else {
            return;
        };
        if opacity == 0.0 {
            return;
        }
        let hover = state.now.map_or(0.0, |now| state.hover.value(now));
        let color = self.thumb_color(theme, state.drag.is_some(), hover, opacity);
        let radius = (self.style)(theme, IDLE)
            .vertical_rail
            .scroller
            .border
            .radius;
        // Over the content, which the scrollable drew in a layer of its own.
        renderer.with_layer(clip, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: bar.thumb,
                    border: Border {
                        radius,
                        ..Border::default()
                    },
                    snap: false,
                    ..renderer::Quad::default()
                },
                color,
            );
        });
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.scrollable
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        let bar = state.bar(layout.bounds(), content_height(layout));
        self.scrollable.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            state.content_cursor(bar, cursor),
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
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.scrollable.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::{button, column, mouse_area, scrollable, Space};

    use super::*;
    use crate::ui::core::theme::Zinc;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    /// A 100 × 100 view of a `content` px column at the window's top left,
    /// publishing its offset down as it scrolls.
    fn view<'a>(content: f32) -> Element<'a, f32> {
        fading_scrollbar(
            scrollable(column![Space::new().width(100).height(content)])
                .width(100)
                .height(100)
                .on_scroll(|viewport| viewport.absolute_offset().y),
        )
    }

    fn window() -> Frames {
        Frames::new(Size::new(120.0, 120.0))
    }

    /// The thumb drawn at `alpha` of the zinc foreground.
    fn thumb(alpha: f32) -> Color {
        over(
            Color {
                a: Zinc::FOREGROUND.a * alpha,
                ..Zinc::FOREGROUND
            },
            BACKGROUND,
        )
    }

    fn wheel(down: f32) -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -down },
        })
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    /// The offset the view last published, if it did.
    fn offset(messages: &[f32]) -> Option<f32> {
        messages.last().copied()
    }

    fn view_box() -> Rectangle {
        Rectangle::new(Point::ORIGIN, Size::new(100.0, 100.0))
    }

    #[test]
    fn the_thumb_is_8_wide_2_in_from_the_edge_and_its_share_of_the_track_but_48_at_least() {
        // A third of 300 px shows: its share of the 100 px track would be
        // 33, under the least.
        let bar = Bar::new(view_box(), 300.0, 0.0).expect("a bar");
        assert_eq!(
            bar.track,
            Rectangle::new(Point::new(88.0, 0.0), Size::new(12.0, 100.0))
        );
        assert_eq!(
            bar.thumb,
            Rectangle::new(Point::new(90.0, 0.0), Size::new(8.0, 48.0))
        );
        // Two thirds of 150 px: 66.7 long, its travel the remaining 33.3.
        let bar = Bar::new(view_box(), 150.0, 25.0).expect("a bar");
        assert!((bar.thumb.height - 200.0 / 3.0).abs() < 1e-3);
        assert!((bar.thumb.y - 0.5 * 100.0 / 3.0).abs() < 1e-3);
        // The offset is the share scrolled of the travel: 450 of 900 over
        // a 52 px travel.
        let bar = Bar::new(view_box(), 1000.0, 450.0).expect("a bar");
        assert_eq!(
            bar.thumb,
            Rectangle::new(Point::new(90.0, 26.0), Size::new(8.0, 48.0))
        );
        let end = Bar::new(view_box(), 1000.0, 900.0).expect("a bar");
        assert_eq!(end.thumb.y, 52.0);
        // In a view under 48 tall the thumb fills its track, and a drag of
        // it moves nothing.
        let short = Rectangle::new(Point::ORIGIN, Size::new(100.0, 40.0));
        let bar = Bar::new(short, 400.0, 0.0).expect("a bar");
        assert_eq!(bar.thumb.height, 40.0);
        assert_eq!(bar.track_to_scroll(10.0), None);
        // Nothing to scroll: no bar.
        assert_eq!(Bar::new(view_box(), 100.0, 0.0), None);
    }

    #[test]
    fn the_thumb_is_drawn_where_the_painter_put_it() {
        let mut window = window();
        let t0 = Instant::now();
        // The pointer on the track brings the bar in.
        window.point_at(Point::new(94.0, 80.0));
        window.frame(view(1000.0), &[], t0);
        window.frame(view(1000.0), &[], t0 + 300 * MS);
        let on = thumb(0.65);
        assert!(near(window.pixel(Point::new(90.0, 24.0)), on), "left edge");
        assert!(near(window.pixel(Point::new(97.0, 24.0)), on), "right edge");
        assert!(
            near(window.pixel(Point::new(89.0, 24.0)), BACKGROUND),
            "margin"
        );
        assert!(
            near(window.pixel(Point::new(98.0, 24.0)), BACKGROUND),
            "margin"
        );
        assert!(near(window.pixel(Point::new(94.0, 46.0)), on), "48 long");
        assert!(
            near(window.pixel(Point::new(94.0, 48.0)), BACKGROUND),
            "no more"
        );
        // Half way down the content, half way down its 52 px travel.
        window.frame(view(1000.0), &[wheel(450.0)], t0 + 400 * MS);
        assert!(near(window.pixel(Point::new(94.0, 25.0)), BACKGROUND));
        assert!(near(window.pixel(Point::new(94.0, 27.0)), on));
        assert!(near(window.pixel(Point::new(94.0, 72.0)), on));
        assert!(near(window.pixel(Point::new(94.0, 74.0)), BACKGROUND));
    }

    #[test]
    fn the_thumb_shows_on_a_scroll_and_fades_600_ms_after_it() {
        let mut window = window();
        let t0 = Instant::now();
        window.point_at(Point::new(40.0, 50.0));
        assert!(!window.frame(view(300.0), &[], t0), "idle");
        assert!(
            near(window.pixel(Point::new(94.0, 10.0)), BACKGROUND),
            "hidden"
        );
        // Scrolled 30 of 200, the 48 px thumb is 7.8 down its 52 px travel.
        let on_thumb = Point::new(94.0, 30.0);
        assert!(
            window.frame(view(300.0), &[wheel(30.0)], t0),
            "a scroll: frames"
        );
        assert!(near(window.pixel(on_thumb), BACKGROUND), "start");
        window.frame(view(300.0), &[], t0 + 150 * MS);
        let middle = Curve::FastOutSlowIn.transform(0.5);
        assert!(near(window.pixel(on_thumb), thumb(0.30 * middle)), "middle");
        assert!(!window.frame(view(300.0), &[], t0 + 300 * MS), "in: waits");
        assert!(near(window.pixel(on_thumb), thumb(0.30)), "end");
        // 600 ms after the scroll it fades out.
        window.frame(view(300.0), &[], t0 + 599 * MS);
        assert!(near(window.pixel(on_thumb), thumb(0.30)), "still");
        assert!(window.frame(view(300.0), &[], t0 + 600 * MS), "fading");
        window.frame(view(300.0), &[], t0 + 750 * MS);
        assert!(near(window.pixel(on_thumb), thumb(0.30 * middle)), "going");
        assert!(!window.frame(view(300.0), &[], t0 + 900 * MS), "rests");
        assert!(near(window.pixel(on_thumb), BACKGROUND), "gone");
    }

    #[test]
    fn the_pointer_on_the_bar_holds_it_and_warms_its_color_over_200_ms() {
        let mut window = window();
        let t0 = Instant::now();
        window.point_at(Point::new(95.0, 90.0));
        window.frame(view(300.0), &[], t0);
        window.frame(view(300.0), &[], t0 + 100 * MS);
        // The thumb sits at the top (0..48).
        let on = Point::new(95.0, 16.0);
        let mid = Curve::FastOutSlowIn.transform(1.0 / 3.0);
        let warming = 0.30_f32.lerp(0.65, 0.5);
        assert!(
            near(window.pixel(on), thumb(warming * mid)),
            "both on the way"
        );
        window.frame(view(300.0), &[], t0 + 300 * MS);
        assert!(near(window.pixel(on), thumb(0.65)), "in, hovered");
        // Held while the pointer moves on it.
        assert!(!window.frame(view(300.0), &[], t0 + 2000 * MS));
        assert!(near(window.pixel(on), thumb(0.65)), "held");
        // Off the bar: back to idle over 200 ms, and gone 600 ms on.
        window.point_at(Point::new(40.0, 50.0));
        let t1 = t0 + 2100 * MS;
        window.frame(view(300.0), &[], t1);
        window.frame(view(300.0), &[], t1 + 200 * MS);
        assert!(near(window.pixel(on), thumb(0.30)), "cooled");
        window.frame(view(300.0), &[], t1 + 600 * MS);
        assert!(!window.frame(view(300.0), &[], t1 + 900 * MS));
        assert!(near(window.pixel(on), BACKGROUND), "gone");
    }

    #[test]
    fn a_hidden_bar_comes_in_for_the_pointer_near_it_then_wants_it_on_its_track() {
        let mut window = window();
        let t0 = Instant::now();
        // 25 px in from the edge, beside the 12 px track: near enough
        // while the bar is hidden (the track widened to take a 48 px square
        // round the thumb's centre), too far once it shows.
        window.point_at(Point::new(60.0, 90.0));
        assert!(!window.frame(view(300.0), &[], t0), "too far");
        window.point_at(Point::new(75.0, 90.0));
        assert!(window.frame(view(300.0), &[], t0 + 16 * MS), "near: in");
        window.frame(view(300.0), &[], t0 + 32 * MS);
        // The next move finds it showing and the pointer off its track: it
        // fades 600 ms on.
        window.frame(view(300.0), &[], t0 + 48 * MS);
        window.frame(view(300.0), &[], t0 + 316 * MS);
        let on = Point::new(94.0, 16.0);
        assert!(near(window.pixel(on), thumb(0.30)), "in, not hovered");
        window.frame(view(300.0), &[], t0 + 647 * MS);
        assert!(near(window.pixel(on), thumb(0.30)), "still");
        assert!(window.frame(view(300.0), &[], t0 + 648 * MS), "fading");
        assert!(!window.frame(view(300.0), &[], t0 + 948 * MS));
        assert!(near(window.pixel(on), BACKGROUND), "gone");
    }

    #[test]
    fn a_view_with_nothing_to_scroll_shows_no_bar() {
        let mut window = window();
        let t0 = Instant::now();
        window.point_at(Point::new(95.0, 20.0));
        assert!(!window.frame(view(50.0), &[], t0));
        assert!(!window.frame(view(50.0), &[wheel(30.0)], t0 + 10 * MS));
        assert!(near(window.pixel(Point::new(94.0, 20.0)), BACKGROUND));
    }

    #[test]
    fn dragging_the_thumb_scrolls_the_view_in_proportion() {
        let mut window = window();
        let t0 = Instant::now();
        // 200 px to scroll over the 48 px thumb's 52 px travel.
        window.point_at(Point::new(94.0, 10.0));
        window.frame(view(300.0), &[], t0);
        window.frame(view(300.0), &[], t0 + 300 * MS);
        // Pressed, the thumb takes its dragged color at once.
        let (_, scrolled) = window.frame_messages(view(300.0), &[press()], t0 + 316 * MS);
        assert_eq!(offset(&scrolled), None, "a press moves nothing");
        let at = Point::new(94.0, 30.0);
        assert!(near(window.pixel(at), thumb(0.75)), "dragged");
        // 26 px down the track is half its travel: half the content.
        window.point_at(Point::new(94.0, 36.0));
        let (_, scrolled) = window.frame_messages(view(300.0), &[], t0 + 332 * MS);
        assert_eq!(offset(&scrolled), Some(100.0));
        let at = Point::new(94.0, 60.0);
        assert!(near(window.pixel(at), thumb(0.75)), "it follows");
        // Across, nothing.
        window.point_at(Point::new(80.0, 36.0));
        let (_, scrolled) = window.frame_messages(view(300.0), &[], t0 + 348 * MS);
        assert_eq!(offset(&scrolled), None);
        // Past either end of the view, the content's end.
        window.point_at(Point::new(94.0, 200.0));
        let (_, scrolled) = window.frame_messages(view(300.0), &[], t0 + 364 * MS);
        assert_eq!(offset(&scrolled), Some(200.0));
        window.point_at(Point::new(94.0, -50.0));
        let (_, scrolled) = window.frame_messages(view(300.0), &[], t0 + 380 * MS);
        assert_eq!(offset(&scrolled), Some(0.0));
        // Let go over the thumb: hovered.
        window.point_at(Point::new(94.0, 10.0));
        window.frame(view(300.0), &[release()], t0 + 396 * MS);
        let at = Point::new(94.0, 30.0);
        assert!(near(window.pixel(at), thumb(0.65)), "hovered");
    }

    #[test]
    fn a_press_on_the_track_pages_0_8_of_the_view_over_100_ms() {
        let mut window = window();
        let t0 = Instant::now();
        // 900 px to scroll; the thumb sits at 0..48.
        window.point_at(Point::new(94.0, 90.0));
        window.frame(view(1000.0), &[], t0);
        window.frame(view(1000.0), &[], t0 + 300 * MS);
        // Below the thumb: down 80, from the next frame on.
        let t1 = t0 + 400 * MS;
        let (more, scrolled) = window.frame_messages(view(1000.0), &[press()], t1);
        assert!(more, "paging");
        assert_eq!(offset(&scrolled), None, "it starts from here");
        window.frame(view(1000.0), &[release()], t1 + 10 * MS);
        let (_, scrolled) = window.frame_messages(view(1000.0), &[], t1 + 50 * MS);
        let half = 80.0 * Curve::EaseInOut.transform(0.5);
        assert!(offset(&scrolled).is_some_and(|y| (y - half).abs() < 1e-3));
        let (_, scrolled) = window.frame_messages(view(1000.0), &[], t1 + 75 * MS);
        let most = 80.0 * Curve::EaseInOut.transform(0.75);
        assert!(offset(&scrolled).is_some_and(|y| (y - most).abs() < 1e-3));
        let (more, scrolled) = window.frame_messages(view(1000.0), &[], t1 + 100 * MS);
        assert_eq!(offset(&scrolled), Some(80.0));
        assert!(!more, "paged");
        // Above the thumb (now 4.6 down): back up, no further than the top.
        window.point_at(Point::new(94.0, 2.0));
        let t2 = t1 + 200 * MS;
        window.frame(view(1000.0), &[press(), release()], t2);
        let (_, scrolled) = window.frame_messages(view(1000.0), &[], t2 + 100 * MS);
        assert_eq!(offset(&scrolled), Some(0.0));
    }

    #[test]
    fn the_shown_bar_takes_the_pointer_from_the_content_and_the_hidden_one_does_not() {
        let view = || -> Element<'_, ()> {
            fading_scrollbar(
                scrollable(column![
                    button(Space::new().height(40))
                        .width(Length::Fill)
                        .on_press(()),
                    Space::new().height(260),
                ])
                .width(100)
                .height(100),
            )
        };
        let mut window = window();
        let t0 = Instant::now();
        // On the hidden thumb: the button's (and the bar comes in).
        window.point_at(Point::new(95.0, 20.0));
        let (_, clicked) = window.frame_messages(view(), &[press(), release()], t0);
        assert_eq!(clicked.len(), 1, "the button");
        // Shown, the thumb's.
        window.frame(view(), &[], t0 + 300 * MS);
        let (_, clicked) = window.frame_messages(view(), &[press(), release()], t0 + 316 * MS);
        assert!(clicked.is_empty(), "the bar");
        // Off the track, the button's.
        window.point_at(Point::new(50.0, 20.0));
        let (_, clicked) = window.frame_messages(view(), &[press(), release()], t0 + 332 * MS);
        assert_eq!(clicked.len(), 1, "the button again");
    }

    #[test]
    fn the_shown_track_takes_the_hover_but_not_a_drag_the_content_began() {
        let view = || -> Element<'_, Point> {
            fading_scrollbar(
                scrollable(mouse_area(Space::new().width(100).height(300)).on_move(|at| at))
                    .width(100)
                    .height(100),
            )
        };
        let mut window = window();
        let t0 = Instant::now();
        let on_track = Point::new(95.0, 50.0);
        window.point_at(on_track);
        let (_, moves) = window.frame_messages(view(), &[], t0);
        assert_eq!(moves, [on_track], "hidden, the content's");
        window.frame(view(), &[], t0 + 300 * MS);
        window.point_at(Point::new(95.0, 60.0));
        let (_, moves) = window.frame_messages(view(), &[], t0 + 316 * MS);
        assert!(moves.is_empty(), "shown, the bar's");
        // Pressed on the content, the pointer stays the content's over the
        // track.
        window.point_at(Point::new(50.0, 50.0));
        window.frame(view(), &[press()], t0 + 332 * MS);
        window.point_at(on_track);
        let (_, moves) = window.frame_messages(view(), &[], t0 + 348 * MS);
        assert_eq!(moves, [on_track], "the content's drag");
    }
}
