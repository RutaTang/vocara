//! The app's scroll views: each a Flutter `Scrollable` under the app's
//! `ClampingScrollPhysics` (`app.dart`'s `_NoBounceScrollBehavior`, a
//! `MaterialScrollBehavior`), Flutter 3.38 on macOS, made with
//! [`scrollable`] as iced's `scrollable` is, and the scrollbar Material's
//! desktop behavior put on a vertical one ([`Fading::fading`]). An iced
//! scrollable keeps the content, its layout and its offset; this wrapper
//! moves the offset as Flutter's scroll position moved, and draws and drives
//! the scrollbar.
//!
//! # Scrolling
//!
//! The view scrolls along its axis (vertical, or across with
//! [`ScrollView::horizontal`]) as `Scrollable` and
//! `ScrollPositionWithSingleContext` scrolled it, from the gesture each
//! wheel event was to Flutter's macOS embedder ([`pan_zoom`]); iced's own
//! scrollable never scrolls by a wheel event (its content is a probe that
//! keeps the event from it once the content has seen it):
//!
//! - **A trackpad's swipe** (a pan-zoom gesture) that starts over the view
//!   reaches its drag recognizer (`VerticalDragGestureRecognizer`, or
//!   horizontal; `ScrollBehavior.dragDevices` holds the trackpad), there
//!   while the view can scroll (`setCanDrag`: a range, or an offset off the
//!   start). The start holds the position (`_handleDragDown`, `hold`: a
//!   fling, a page or the thumb's drag stops) and enters the gesture's
//!   arena after the drag recognizers inside the view it reached (a
//!   single-line field's, a nested view's: `pan_zoom::enter`). Alone there,
//!   the view's drag starts at once and each move of the pan along its axis
//!   moves the view 1:1 (`ScrollDragController.update`, `applyUserOffset`),
//!   clamped to its range (a move against an end shows the bar, as the
//!   overscroll notification did); with others, nothing moves until one is
//!   past `kTouchSlop` (18 px, the trackpad's hit slop) along its axis,
//!   the innermost first on a tie, and the move that takes the view's past
//!   starts its drag without moving it (`DragStartBehavior.start`). When
//!   the fingers lift, the drag's velocity
//!   (`MacOSScrollViewFlingVelocityTracker`, a fling past 18 px and
//!   50 px/s, at most 8000 px/s: `scroll_fling::drag_end`) flings the view
//!   on a `ClampingScrollSimulation` from the next frame until it is done
//!   or meets an end (`scroll_fling::Ballistic`); too slow, it stays. A
//!   drag that lost the arena, or one still undecided when the fingers
//!   lift, moves nothing. The momentum macOS sends once they lift reaches
//!   nothing at all: neither the view nor its content sees it.
//! - **A scroll signal** (a mouse wheel's, or a precise device's without
//!   gesture phases) goes to the content first; if nothing there took it
//!   (the innermost scrollable that would scroll wins Flutter's
//!   `pointerSignalResolver`), it scrolls the view by its delta along the
//!   axis, the other one with Shift held (`_pointerSignalEventDelta`'s
//!   `pointerAxisModifiers`, a signal being a mouse's), at once and within
//!   the range (`pointerScroll`: a line 40 px, as the embedder sent it, a
//!   precise device's point a pixel); one that would not move the view
//!   goes on to what lies around it.
//! - **An inertia cancel** (a finger touching the trackpad less than 50 ms
//!   after macOS's momentum last moved, once per momentum: `pan_zoom`)
//!   where the pointer is over the view stops what moves it where its last
//!   frame left it, a fling, a page or a caret's run, its scroll ending
//!   (`Scrollable._receivedPointerSignal`'s `pointerScroll(0)`:
//!   `goBallistic(0)`, idle within the range). It goes to the content
//!   first, and every view it reaches stops, nested ones included (no
//!   `pointerSignalResolver`); a view the pointer is not over flings on.
//!   Over the shown track it is the bar's alone (`jumpTo(pixels)`, the
//!   same stop), which leaves the position to the thumb's drag while one
//!   drives it.
//! - A new swipe that reaches the view, a signal that scrolls it, an
//!   inertia cancel over it, a press of the thumb or the track, a caret
//!   shown or the app scrolling it stop a fling (`beginActivity`); a swipe
//!   that starts elsewhere, a press of the content or a signal that does
//!   not move it do not. A fling after a trackpad's drag leaves the content
//!   its pointer, as Flutter's did.
//!
//! # The scrollbar
//!
//! A vertical view under [`Fading::fading`] takes the scrollbar Flutter's
//! desktop scroll behavior put on every vertical scrollable (Material's
//! `Scrollbar` over `RawScrollbar`, with a mouse), drawn and driven here
//! over the iced scrollable, whose own scrollbar is hidden
//! (`Scrollbar::hidden()`: nothing drawn, nothing to grab).
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
//! - It is hidden until the view scrolls (a swipe, the wheel, a drag of the
//!   thumb, a press on the track, the app) or the pointer comes over the
//!   bar; it then fades in over 300 ms and, 600 ms after the scroll ends
//!   (a fling's when it stops), the pointer leaves the bar or the view, or
//!   a drag ends, fades out over 300 ms, both on `Curves.fastOutSlowIn`
//!   (`fadeDuration`, `timeToFade`). The pointer holds it only as it moves
//!   over it (Flutter's hover events came with motion only): a scroll that
//!   ends under a resting pointer starts the 600 ms too.
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
//! event starts (a fade, the hover's color, a page's scroll, a fling)
//! starts at the next frame, as a Flutter ticker did; the widget asks for
//! frames only while something moves, or for one when the bar is due to
//! fade.
//!
//! Material's scroll behavior gave a horizontal scrollable no scrollbar, and
//! `ScrollConfiguration(scrollbars: false)` took it off a vertical one:
//! those are a [`ScrollView`] as it is ([`no_scrollbar`],
//! [`ScrollView::horizontal`]), scrolling as above with no bar.
//!
//! - The wheel over the shown track is the bar's (`_receivedPointerSignal`,
//!   the content and the view's own listener under the track out of the
//!   hit test): a scroll signal scrolls the view by its vertical delta
//!   (`pointerScroll`: the bar flipped no axis for Shift), within its
//!   range, and the content under the track never sees it; during a drag
//!   of the thumb nothing scrolls. An inertia cancel there stops the view
//!   as one over the content does (`jumpTo(pixels)`), unless the thumb's
//!   drag drives it, and the content under the track never sees it
//!   either. A trackpad's (or a Magic Mouse's) swipe
//!   that starts on the shown track scrolls nothing to its end, nor does
//!   its momentum, and a fling goes on: only the recognizers its start hit
//!   saw it, the view's own drag not among them. One that starts elsewhere
//!   is the view's to its end, should the bar come in under the pointer
//!   meanwhile.
//! - The pointer leaving the view is noticed after each frame too, not
//!   only when it moves (Flutter's mouse tracker re-tested a still pointer
//!   after each frame and sent the bar's region its exit when the view no
//!   longer lay under it, as when the layout moved it away or something
//!   came over it). Content scrolling under a still pointer changes
//!   nothing for the bar: its hover was only ever re-tested on motion.
//! - A text field inside that reveals its caret asks the view to show it
//!   (`text_field::show_on_screen`): the view scrolls the least that
//!   shows the caret's rectangle, over 100 ms on `Curves.fastOutSlowIn`
//!   or at once, as the field asked, a scroll like any other to the bar.
//! - A page that would move the view less than a device pixel jumps
//!   instead (`nearEqual` against `1 / devicePixelRatio`, the window's
//!   scale factor, `ui::core::scale_factor`).
//!
//! Left as it is, iced having no counterpart: a press on the track pages
//! at once, where Flutter's tap waited out the gesture arena (to the
//! release, or 100 ms) when a recognizer above the view wanted the press
//! too. iced hands a widget each press once, in the order of its tree,
//! with no arena: no widget above the view can claim the press back, so
//! there is no later moment for the bar to learn it won.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::operation::Scrollable as ScrollableState;
use iced::advanced::widget::{tree, Id, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Renderer as _, Shell, Widget};
use iced::time::Instant;
use iced::widget::scrollable::{
    self, AbsoluteOffset, Direction, Scrollable, Scrollbar, Status, Style, Viewport,
};
use iced::{
    keyboard, mouse, window, Background, Border, Color, Element, Event, Length, Point, Rectangle,
    Renderer, Size, Theme, Vector,
};

use super::pan_zoom::{self, Scroll, TOUCH_SLOP};
use super::scroll_fling::{self, Ballistic, MacOsVelocityTracker};
use crate::ui::core::motion::{self, Controller, Curve, Lerp};
use crate::ui::core::scale_factor::{scale_factor, set_scale_factor};
use crate::ui::core::theme;
use crate::ui::core::widgets::text_field::show_on_screen;

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

/// A page shorter than this jumps (`toleranceFor(...).distance`, a
/// device pixel, see the module docs).
fn near() -> f32 {
    1.0 / scale_factor()
}

/// How far a scroll signal's `delta` over the shown track scrolls the view
/// down (the bar's `_pointerSignalEventDelta`: the vertical scroll delta,
/// no axis flipped for Shift).
fn wheel_on_track(delta: mouse::ScrollDelta, modifiers: keyboard::Modifiers) -> f32 {
    pan_zoom::scroll_delta(delta, modifiers.shift()).y
}

/// The axis a scroll view scrolls along (`Scrollable.axis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Axis {
    #[default]
    Vertical,
    Horizontal,
}

impl Axis {
    /// `vector`'s part along the axis.
    fn of(self, vector: Vector) -> f32 {
        match self {
            Axis::Vertical => vector.y,
            Axis::Horizontal => vector.x,
        }
    }

    /// `size`'s extent along the axis.
    fn extent(self, size: Size) -> f32 {
        match self {
            Axis::Vertical => size.height,
            Axis::Horizontal => size.width,
        }
    }

    /// A scroll signal's scroll along the axis for its scroll delta
    /// `delta` (`_pointerSignalEventDelta`): the other axis's with Shift
    /// held (`pointerAxisModifiers`, a signal being a mouse's).
    fn signal(self, delta: Vector, shift: bool) -> f32 {
        match (self, shift) {
            (Axis::Vertical, false) | (Axis::Horizontal, true) => delta.y,
            (Axis::Vertical, true) | (Axis::Horizontal, false) => delta.x,
        }
    }

    /// The iced scrollable's direction, its own scrollbar hidden.
    fn direction(self) -> Direction {
        match self {
            Axis::Vertical => Direction::Vertical(Scrollbar::hidden()),
            Axis::Horizontal => Direction::Horizontal(Scrollbar::hidden()),
        }
    }
}

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

/// An app scroll view, built as iced's `scrollable` is (see the module
/// docs): with no scrollbar as it is, vertical unless made
/// [`ScrollView::horizontal`]; [`Fading::fading`] puts the bar on it.
pub struct ScrollView<'a, M> {
    scrollable: Scrollable<'a, M>,
    axis: Axis,
}

/// `content` in an app scroll view (see the module docs).
pub fn scrollable<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> ScrollView<'a, M> {
    ScrollView {
        scrollable: iced::widget::scrollable(Element::new(Probe {
            content: content.into(),
        })),
        axis: Axis::Vertical,
    }
}

impl<'a, M: 'a> ScrollView<'a, M> {
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.width(width);
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.scrollable = self.scrollable.height(height);
        self
    }

    /// The iced scrollable's id, for the app's scroll operations.
    pub fn id(mut self, id: impl Into<Id>) -> Self {
        self.scrollable = self.scrollable.id(id);
        self
    }

    /// Publishes the viewport as the offset changes.
    pub fn on_scroll(mut self, f: impl Fn(Viewport) -> M + 'a) -> Self {
        self.scrollable = self.scrollable.on_scroll(f);
        self
    }

    /// Scrolls across (`scrollDirection: Axis.horizontal`): Material's
    /// scroll behavior gave such a view no scrollbar.
    pub fn horizontal(mut self) -> Self {
        self.axis = Axis::Horizontal;
        self
    }
}

impl<'a, M: 'a> From<ScrollView<'a, M>> for Element<'a, M> {
    fn from(view: ScrollView<'a, M>) -> Self {
        Element::new(FadingScrollbar {
            scrollable: view.scrollable.direction(view.axis.direction()).into(),
            style: None,
            axis: view.axis,
        })
    }
}

/// `view` scrolling vertically with no scrollbar
/// (`ScrollConfiguration(scrollbars: false)`): nothing is drawn and
/// nothing takes the pointer (iced's own scrollbar, even clear, would take
/// the strip at its edge from the content).
pub fn no_scrollbar<M>(view: ScrollView<'_, M>) -> ScrollView<'_, M> {
    debug_assert_eq!(view.axis, Axis::Vertical, "a vertical view");
    view
}

/// `view` under the app's scrollbar (`theme::scrollbar`), as the module
/// docs say. It scrolls vertically.
pub fn fading_scrollbar<'a, M: 'a>(view: ScrollView<'a, M>) -> Element<'a, M> {
    fading_scrollbar_with(view, theme::scrollbar)
}

/// `view` under the scrollbar whose colors and radius `style` gives (its
/// vertical rail's scroller, idle, hovered and dragged), as the module
/// docs say.
pub fn fading_scrollbar_with<'a, M: 'a>(
    view: ScrollView<'a, M>,
    style: impl Fn(&Theme, Status) -> Style + 'a,
) -> Element<'a, M> {
    with_bar(view.scrollable, style)
}

/// `scrollable`, probed, scrolling vertically under the bar `style` gives.
fn with_bar<'a, M: 'a>(
    scrollable: Scrollable<'a, M>,
    style: impl Fn(&Theme, Status) -> Style + 'a,
) -> Element<'a, M> {
    let style: SharedStyle<'a> = Rc::new(style);
    let looks = Rc::clone(&style);
    // The iced scrollable draws its container; its own scrollbar is hidden.
    let scrollable = scrollable
        .direction(Axis::Vertical.direction())
        .style(move |theme, status| looks(theme, status));
    Element::new(FadingScrollbar {
        scrollable: scrollable.into(),
        style: Some(style),
        axis: Axis::Vertical,
    })
}

/// A scroll view that will take the fading scrollbar, still taking its
/// size: what [`Fading::fading`] makes of one, to finish as the view was.
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
        with_bar(fading.scrollable, fading.style)
    }
}

/// `.fading()` on a [`ScrollView`] that Material's desktop scroll behavior
/// gave a scrollbar.
pub trait Fading<'a, M> {
    /// Under the app's scrollbar (see the module docs), scrolling
    /// vertically.
    fn fading(self) -> FadingScrollable<'a, M>;

    /// Under the scrollbar `style` gives, scrolling vertically.
    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M>;
}

impl<'a, M: 'a> Fading<'a, M> for ScrollView<'a, M> {
    fn fading(self) -> FadingScrollable<'a, M> {
        self.fading_with(theme::scrollbar)
    }

    fn fading_with(self, style: impl Fn(&Theme, Status) -> Style + 'a) -> FadingScrollable<'a, M> {
        debug_assert_eq!(self.axis, Axis::Vertical, "the bar is a vertical view's");
        FadingScrollable {
            scrollable: self.scrollable,
            style: Box::new(style),
        }
    }
}

// -------------------------------------------------------------- the probe

/// Where a wheel event's handover between a scroll view and its probe
/// stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probing {
    Idle,
    /// The view handed its scrollable the event.
    Armed,
    /// The probe handed it to the content, which took it or not.
    Answered(bool),
}

thread_local! {
    static PROBE: Cell<Probing> = const { Cell::new(Probing::Idle) };
}

/// A scroll view's content inside its iced scrollable: lays out, draws and
/// operates as the content does (its tree is the content's), and hands it
/// every event; a wheel event the view hands over then goes no further,
/// so iced's scrollable never scrolls by it (the view scrolls itself, see
/// the module docs), and the view learns whether the content took it.
struct Probe<'a, M> {
    content: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for Probe<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
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
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
        // A view inside the content hands its own over.
        let armed = matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. }))
            && PROBE.with(|probe| probe.replace(Probing::Idle)) == Probing::Armed;
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        if armed {
            PROBE.with(|probe| probe.set(Probing::Answered(shell.is_event_captured())));
            shell.capture_event();
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
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
            .operate(tree, layout, renderer, operation);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

/// Hands `event`, a wheel event, to the content of `scrollable` through
/// its probe, in a shell of its own: what the content did reaches `shell`
/// but for the probe's capture. Whether the content took the event (or
/// something before it had).
#[allow(clippy::too_many_arguments)]
fn update_probed<M>(
    scrollable: &mut Element<'_, M>,
    tree: &mut Tree,
    event: &Event,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
    renderer: &Renderer,
    clipboard: &mut dyn Clipboard,
    shell: &mut Shell<'_, M>,
    viewport: &Rectangle,
) -> bool {
    let mut messages = Vec::new();
    let mut inner = Shell::new(&mut messages);
    if shell.is_event_captured() {
        inner.capture_event();
    }
    let before = PROBE.with(|probe| probe.replace(Probing::Armed));
    scrollable.as_widget_mut().update(
        tree, event, layout, cursor, renderer, clipboard, &mut inner, viewport,
    );
    let answer = PROBE.with(|probe| probe.replace(before));
    shell.request_redraw_at(inner.redraw_request());
    shell.request_input_method(inner.input_method());
    if inner.is_layout_invalid() {
        shell.invalidate_layout();
    }
    if inner.are_widgets_invalid() {
        shell.invalidate_widgets();
    }
    let captured = inner.is_event_captured();
    drop(inner);
    for message in messages {
        shell.publish(message);
    }
    match answer {
        Probing::Answered(taken) => taken,
        // The probe was not reached (iced's scrollable kept the event from
        // its content): what the scrollable did with it stands.
        Probing::Idle | Probing::Armed => captured,
    }
}

// ---------------------------------------------------------- the wrapper

struct FadingScrollbar<'a, M> {
    scrollable: Element<'a, M>,
    /// The bar's style; none for a view with no bar.
    style: Option<SharedStyle<'a>>,
    axis: Axis,
}

/// The thumb's color in `style` at `opacity` (`_thumbColor` resolved, then
/// `_paintThumb`'s `color.withOpacity(color.opacity * fade)`, its alpha in
/// 8 bits each time).
fn thumb_color(
    style: &SharedStyle<'_>,
    theme: &Theme,
    dragged: bool,
    hover: f32,
    opacity: f32,
) -> Color {
    let color = |status| match style(theme, status).vertical_rail.scroller.background {
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

/// A scroll over 100 ms (`DrivenScrollActivity`): a press on the track's
/// page, or a text field's caret shown.
#[derive(Debug, Clone, Copy)]
struct Page {
    from: f32,
    to: f32,
    /// Its first frame, once it had one.
    start: Option<Instant>,
    curve: Curve,
}

/// A pan-zoom gesture that started over the view: whether the shown track
/// took its start (nothing scrolls, and the content sees none of it), and
/// the view's drag recognizer on it, if the start reached it.
#[derive(Debug, Clone)]
struct Pan {
    gesture: u64,
    on_track: bool,
    drag: Option<PanDrag>,
}

/// Where the view's drag recognizer is on a gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// In the arena, holding the position (`_DragState.possible`).
    Possible,
    /// Won: its drag moves the view (`DragScrollActivity`).
    Dragging,
    /// Lost, ended, or its drag disposed by another activity.
    Done,
}

/// The view's drag recognizer on a pan-zoom gesture
/// (`VerticalDragGestureRecognizer`, `Scrollable._handleDrag*`).
#[derive(Debug, Clone)]
struct PanDrag {
    entry: pan_zoom::Entry,
    /// The pan along the axis since the start: what the velocity tracker
    /// samples, and how far it has moved toward its slop
    /// (`_globalDistanceMoved`).
    pan: f32,
    tracker: MacOsVelocityTracker,
    stage: Stage,
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

/// The view's state: its scroll position's (`ScrollPosition`'s activity)
/// and its bar's (`RawScrollbarState`'s and Material's).
struct State {
    /// The view has a bar.
    has_bar: bool,
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
    /// A fling under way (`BallisticScrollActivity`).
    fling: Option<Ballistic>,
    pending: Pending,
    /// Something changed that a frame must show.
    dirty: bool,
    /// The scroll offset along the axis (`pixels`), once seen: what this
    /// widget set, to the fraction, or what the scrollable shows.
    pixels: Option<f32>,
    now: Option<Instant>,
    /// The keyboard's modifiers (Shift, for the wheel).
    modifiers: keyboard::Modifiers,
    /// The text fields' request count it last looked at
    /// (`text_field::show_on_screen`).
    asked: u64,
    /// The pan-zoom gesture last started over the view.
    pan: Option<Pan>,
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
    fn new(has_bar: bool) -> Self {
        Self {
            has_bar,
            fade: Controller::new(FADE, FADE),
            timer: None,
            hover: Controller::new(HOVER, HOVER),
            hovering: false,
            inside: false,
            buttons: 0,
            held: false,
            drag: None,
            page: None,
            fling: None,
            pending: Pending::default(),
            dirty: false,
            pixels: None,
            now: None,
            modifiers: keyboard::Modifiers::default(),
            asked: show_on_screen::asked(),
            pan: None,
        }
    }

    /// The thumb's opacity as last drawn.
    fn opacity(&self) -> f32 {
        self.now
            .map_or(0.0, |now| self.fade.curved(Curve::FastOutSlowIn, None, now))
    }

    fn bar(&self, bounds: Rectangle, content: f32) -> Option<Bar> {
        if !self.has_bar {
            return None;
        }
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
        if self.has_bar && !heading_in(&self.fade, self.now) && !self.pending.show {
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

    /// The view scrolled (`ScrollUpdateNotification`, or an
    /// `OverscrollNotification` for a move against an end) and, if `ended`,
    /// the scroll ended (`ScrollEndNotification`).
    fn scrolled(&mut self, ended: bool) {
        self.show();
        self.cancel_timer();
        if ended {
            self.scroll_ended();
        }
    }

    /// The scroll ended (`ScrollEndNotification`): the bar fades once the
    /// time is up, unless the thumb's drag holds it.
    fn scroll_ended(&mut self) {
        if self.has_bar && !self.drag.is_some_and(|drag| drag.live) {
            self.start_timer();
        }
    }

    /// This widget scrolled the view to `to`, the scroll ending if `ended`.
    fn moved(&mut self, to: f32, ended: bool) {
        self.pixels = Some(to);
        self.dirty = true;
        self.scrolled(ended);
    }

    /// Something new drives the position (`beginActivity`): what drove it
    /// stops (a page's or a caret's run, a fling, the trackpad's drag, the
    /// thumb's drag, which goes on holding the button). Whether a run that
    /// was scrolling stopped (its scroll then ends, unless what comes
    /// scrolls on).
    fn stop_runs(&mut self) -> bool {
        let paged = self.page.take().is_some();
        let flung = self.fling.take().is_some();
        let dragged = self
            .pan
            .as_mut()
            .and_then(|pan| pan.drag.as_mut())
            .is_some_and(|drag| {
                let dragging = drag.stage == Stage::Dragging;
                if dragging {
                    drag.stage = Stage::Done;
                }
                dragging
            });
        if let Some(drag) = &mut self.drag {
            drag.live = false;
        }
        paged || flung || dragged
    }

    /// Takes in the offset the scrollable shows, `seen`, with `range` to
    /// scroll: a change this widget did not make (the app's) is a scroll
    /// that ended, and it ends what drove the position (`jumpTo`'s
    /// `goIdle`).
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
        self.stop_runs();
        self.scrolled(true);
    }

    /// `pointerScroll(delta)` of a scroll signal the view takes: at once
    /// by `delta` within `0..=range`, what drove the position stopped
    /// (`goIdle`), the scroll ending; none when it would not move. Where
    /// it scrolls to.
    fn pointer_scroll(&mut self, delta: f32, range: f32) -> Option<f32> {
        let pixels = self.pixels.unwrap_or(0.0);
        let to = (pixels + delta).clamp(0.0, range.max(0.0));
        if delta == 0.0 || to == pixels {
            return None;
        }
        self.stop_runs();
        self.moved(to, true);
        Some(to)
    }

    /// `goIdle`, as an inertia cancel's `pointerScroll(0)` (`goBallistic(0)`
    /// within the range) and the bar's `jumpTo(pixels)` went: what drove
    /// the position stops where its last frame left it, a run that was
    /// scrolling ending its scroll.
    fn go_idle(&mut self) {
        if self.stop_runs() {
            self.scroll_ended();
        }
    }

    /// The view's drag recognizer on `gesture`, if it holds it.
    fn pan_drag(&mut self, gesture: u64) -> Option<&mut PanDrag> {
        self.pan
            .as_mut()
            .filter(|pan| pan.gesture == gesture)?
            .drag
            .as_mut()
    }

    /// A pan-zoom start that reached the view's drag recognizer, the pan
    /// being `gesture` started at `time` (`addAllowedPointerPanZoom`,
    /// `_handleDragDown`): the position held, a run moving it ending its
    /// scroll; the recognizer enters the arena.
    fn pan_start(&mut self, gesture: u64, time: Instant) {
        if self.stop_runs() {
            self.scroll_ended();
        }
        let mut tracker = MacOsVelocityTracker::default();
        tracker.add(time, 0.0, pan_zoom::event_time());
        if let Some(pan) = self.pan.as_mut().filter(|pan| pan.gesture == gesture) {
            pan.drag = Some(PanDrag {
                entry: pan_zoom::enter(gesture, time),
                pan: 0.0,
                tracker,
                stage: Stage::Possible,
            });
        }
    }

    /// A move of `gesture` by `along` on the axis at `time`, if the view's
    /// drag recognizer holds it (`handleEvent` for a
    /// `PointerPanZoomUpdateEvent`): the move the view's drag makes, if
    /// any (`Some(None)`: held, nothing moves).
    fn pan_update(&mut self, gesture: u64, along: f32, time: Instant) -> Option<Option<f32>> {
        let drag = self
            .pan_drag(gesture)
            .filter(|drag| drag.stage != Stage::Done)?;
        drag.pan += along;
        drag.tracker.add(time, drag.pan, pan_zoom::event_time());
        let (stage, moves) = match drag.stage {
            Stage::Possible => match drag.entry.standing() {
                // Alone, it won as the start closed the arena: its drag
                // started then, and this is its first move.
                Some(true) => (Stage::Dragging, true),
                // Another won: `rejectGesture`, its hold let go.
                Some(false) => (Stage::Done, false),
                // Past its slop it accepts; the drag starts from here.
                None if drag.pan.abs() > TOUCH_SLOP as f32 => {
                    if drag.entry.accept() {
                        (Stage::Dragging, false)
                    } else {
                        (Stage::Done, false)
                    }
                }
                None => (Stage::Possible, false),
            },
            stage => (stage, stage == Stage::Dragging),
        };
        let starts = drag.stage == Stage::Possible && stage == Stage::Dragging;
        if starts {
            // `position.drag`: a `DragScrollActivity` takes over.
            self.stop_runs();
        }
        if let Some(drag) = self.pan_drag(gesture) {
            drag.stage = stage;
        }
        Some((moves && along != 0.0).then_some(along))
    }

    /// The end of `gesture`, if the view's drag recognizer holds it
    /// (`didStopTrackingLastPointer`): a drag under way ends in a fling
    /// (`_checkEnd`, `ScrollDragController.end`, `goBallistic`), in
    /// `0..=range`; an undecided one is let go. Whether it held it.
    fn pan_end(&mut self, gesture: u64, range: f32) -> bool {
        let Some(drag) = self
            .pan_drag(gesture)
            .filter(|drag| drag.stage != Stage::Done)
        else {
            return false;
        };
        let was = drag.stage;
        // Alone, it won at the start though nothing moved since.
        let dragging = was == Stage::Dragging || drag.entry.standing() == Some(true);
        let estimate = drag.tracker.estimate(pan_zoom::event_time());
        drag.stage = Stage::Done;
        if !dragging {
            return true;
        }
        if was == Stage::Possible {
            self.stop_runs();
        }
        let velocity = scroll_fling::drag_end(estimate);
        let pixels = self.pixels.unwrap_or(0.0);
        self.fling = Ballistic::new(f64::from(pixels), f64::from(range.max(0.0)), velocity);
        if self.fling.is_some() {
            self.dirty = true;
        } else {
            // `goIdle`.
            self.scroll_ended();
        }
        true
    }

    /// The drag's move by `delta` along the axis (`applyUserOffset`):
    /// against it, clamped to `0..=range` (a move against an end
    /// overscrolls, which shows the bar). Where the view scrolls to.
    fn apply_user_offset(&mut self, delta: f32, range: f32) -> Option<f32> {
        let pixels = self.pixels.unwrap_or(0.0);
        let to = (pixels - delta).clamp(0.0, range.max(0.0));
        if to == pixels {
            self.scrolled(false);
            return None;
        }
        self.moved(to, false);
        Some(to)
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
        let y = page.from + (page.to - page.from) * page.curve.transform(t);
        Some((y, t >= 1.0))
    }

    /// Where the fling is at `now` in `0..=range`, and whether it goes on.
    fn fling_at(&mut self, now: Instant, range: f32) -> Option<(f32, bool)> {
        let fling = self.fling.as_mut()?;
        let (to, more) = fling.frame(now, f64::from(range.max(0.0)));
        Some((to as f32, more))
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
        let moving = self.page.is_some() || self.fling.is_some();
        if self.fade.is_animating(now) || self.hover.is_animating(now) || moving {
            shell.request_redraw();
        } else if let Some(due) = self.timer {
            shell.request_redraw_at(due);
        }
    }
}

/// Reads the scrollable's offset along an axis as it shows it, stopping at
/// it.
#[derive(Default)]
struct Read {
    axis: Axis,
    offset: Option<f32>,
}

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
        self.offset.get_or_insert(self.axis.of(translation));
    }
}

/// Scrolls the scrollable to an offset along an axis, stopping at it.
struct ScrollTo {
    axis: Axis,
    to: Option<f32>,
}

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
        if let Some(to) = self.to.take() {
            state.scroll_to(match self.axis {
                Axis::Vertical => AbsoluteOffset {
                    x: None,
                    y: Some(to),
                },
                Axis::Horizontal => AbsoluteOffset {
                    x: Some(to),
                    y: None,
                },
            });
        }
    }
}

/// The content's extent along `axis` under the scrollable laid out at
/// `layout`.
fn content_extent(layout: Layout<'_>, axis: Axis) -> f32 {
    layout
        .children()
        .next()
        .map_or(0.0, |content| axis.extent(content.bounds().size()))
}

/// The offset along `axis` `scrollable` shows.
fn read<M>(
    scrollable: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    axis: Axis,
) -> Option<f32> {
    let mut read = Read { axis, offset: None };
    scrollable
        .as_widget_mut()
        .operate(tree, layout, renderer, &mut read);
    read.offset
}

/// Scrolls `scrollable` to `to` along `axis`.
fn scroll_to<M>(
    scrollable: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    renderer: &Renderer,
    axis: Axis,
    to: f32,
) {
    scrollable.as_widget_mut().operate(
        tree,
        layout,
        renderer,
        &mut ScrollTo { axis, to: Some(to) },
    );
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
        tree::State::new(State::new(self.style.is_some()))
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.scrollable)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.state.downcast_mut::<State>().has_bar = self.style.is_some();
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
        let axis = self.axis;
        let bounds = layout.bounds();
        let content = content_extent(layout, axis);
        let range = content - axis.extent(bounds.size());
        let Tree {
            state, children, ..
        } = tree;
        let state = state.downcast_mut::<State>();
        let child = &mut children[0];
        let scrollable = &mut self.scrollable;
        let scroll = |scrollable: &mut Element<'_, M>, child: &mut Tree, to: f32| {
            scroll_to(scrollable, child, layout, renderer, axis, to);
        };
        // A scroll since the last event (an operation of the app's).
        state.observe(read(scrollable, child, layout, renderer, axis), range);

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
            // The fling's frame (`BallisticScrollActivity`): done, or
            // stopped at an end, it goes idle and its scroll ends.
            if let Some((to, more)) = state.fling_at(now, range) {
                if state.pixels != Some(to) {
                    scroll(scrollable, child, to);
                    state.moved(to, false);
                }
                if !more {
                    state.fling = None;
                    state.scroll_ended();
                }
            }
            let scrolls = state.bar(bounds, content).is_some();
            state.settle(now, scrolls, shell);
            // A still pointer the view no longer lies under has left it
            // (the mouse tracker's re-test after the frame).
            if state.buttons == 0 && state.inside && cursor.position_over(bounds).is_none() {
                let bar = state.bar(bounds, content);
                state.hover(None, bar);
            }
        }

        let pixels = state.pixels.unwrap_or(0.0);
        let bar = state.bar(bounds, content);
        let shown = state.opacity() > 0.0;
        let mut content_cursor = state.content_cursor(bar, cursor);
        let mut pass = true;
        // A wheel event the view looks at once its content has.
        let mut wheel = None;
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
                        // `handleThumbPress` (its hold ends a page or a
                        // fling) and `handleThumbPressStart`.
                        state.stop_runs();
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
                        if (to - pixels).abs() < near() {
                            let running = state.stop_runs();
                            if to != pixels {
                                scroll(scrollable, child, to);
                                state.moved(to, true);
                            } else if running {
                                state.start_timer();
                            }
                        } else {
                            state.stop_runs();
                            state.page = Some(Page {
                                from: pixels,
                                to,
                                start: None,
                                curve: Curve::EaseInOut,
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
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let track = cursor
                    .position()
                    .zip(bar)
                    .filter(|(at, bar)| bar.takes(*at, shown))
                    .map(|(_, bar)| bar);
                let scroll_event = pan_zoom::current();
                if let Scroll::Start { gesture, .. } = scroll_event {
                    // Hit tested at its start: the shown track's, or the
                    // view's and its content's.
                    state.pan = Some(Pan {
                        gesture,
                        on_track: track.is_some(),
                        drag: None,
                    });
                }
                // The gesture as its start found the view: whether on the
                // shown track, and whether the view's drag took it.
                let started = state
                    .pan
                    .as_ref()
                    .filter(|pan| Some(pan.gesture) == scroll_event.gesture())
                    .map(|pan| (pan.on_track, pan.drag.is_some()));
                let on_track = match scroll_event {
                    Scroll::Signal => track.is_some(),
                    _ => started.map_or(track.is_some(), |(on_track, _)| on_track),
                };
                let held = started.is_some_and(|(on_track, dragged)| on_track || dragged);
                match (scroll_event, track) {
                    (Scroll::Signal, Some(bar)) => {
                        // The shown track's: the content under it never
                        // sees the wheel. Held by a drag, the bar's own
                        // wheel waits.
                        pass = false;
                        let dy = wheel_on_track(*delta, state.modifiers);
                        let to = (pixels + dy).clamp(0.0, bar.range);
                        if state.drag.is_none() && dy != 0.0 && to != pixels {
                            // `pointerScroll`: a scroll that ends at once,
                            // anything moving the view stopped.
                            state.stop_runs();
                            scroll(scrollable, child, to);
                            state.moved(to, true);
                            shell.capture_event();
                        }
                    }
                    (Scroll::InertiaCancel, Some(_)) => {
                        // The shown track's: the bar jumps the position to
                        // where it is (`jumpTo(pixels)`), unless the
                        // thumb's drag has it; the content under the track
                        // never sees the cancel.
                        pass = false;
                        if !state.drag.is_some_and(|drag| drag.live) {
                            state.go_idle();
                        }
                    }
                    (Scroll::Nothing { .. }, _) => {
                        // Momentum: it reaches nothing at all.
                        pass = false;
                        if held || cursor.position_over(bounds).is_some() {
                            shell.capture_event();
                        }
                    }
                    // The track's gesture: nothing scrolls, and the
                    // content never sees it.
                    _ if on_track => pass = false,
                    // Elsewhere the content has it first, then the view.
                    _ => {
                        content_cursor = cursor;
                        wheel = Some((scroll_event, *delta));
                    }
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.modifiers = *modifiers;
            }
            Event::Window(window::Event::Rescaled(scale_factor)) => {
                set_scale_factor(*scale_factor);
            }
            _ => {}
        }

        if pass {
            let taken = if wheel.is_some() {
                update_probed(
                    scrollable,
                    child,
                    event,
                    layout,
                    content_cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                )
            } else {
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
                false
            };
            // What the event scrolled.
            state.observe(read(scrollable, child, layout, renderer, axis), range);
            if let Some((scroll_event, delta)) = wheel {
                let over = cursor.position_over(bounds).is_some();
                let pixels = state.pixels.unwrap_or(0.0);
                // `setCanDrag`, `shouldAcceptUserOffset`.
                let can_drag = range > PRECISION || pixels != 0.0;
                let mut held = false;
                match scroll_event {
                    Scroll::Signal if !taken && over => {
                        let delta = pan_zoom::scroll_delta(delta, state.modifiers.shift());
                        let along = axis.signal(delta, state.modifiers.shift());
                        if let Some(to) = state.pointer_scroll(along, range) {
                            scroll(scrollable, child, to);
                            held = true;
                        }
                    }
                    Scroll::Start { gesture, time } if over && can_drag => {
                        state.pan_start(gesture, time);
                        held = true;
                    }
                    Scroll::Update {
                        gesture,
                        delta,
                        time,
                    } => {
                        if let Some(moves) = state.pan_update(gesture, axis.of(delta), time) {
                            held = true;
                            if let Some(to) =
                                moves.and_then(|along| state.apply_user_offset(along, range))
                            {
                                scroll(scrollable, child, to);
                            }
                        }
                    }
                    Scroll::End { gesture } => held = state.pan_end(gesture, range),
                    // `pointerScroll(0)`, whatever the content did with it:
                    // every scrollable the hit test reached stops, no
                    // resolver choosing one.
                    Scroll::InertiaCancel if over => state.go_idle(),
                    _ => {}
                }
                if taken || held {
                    shell.capture_event();
                }
            }
            // A text field inside asked to have its caret shown.
            if axis == Axis::Vertical && state.asked != show_on_screen::asked() {
                state.asked = show_on_screen::asked();
                let mut take = show_on_screen::Take::default();
                scrollable
                    .as_widget_mut()
                    .operate(child, layout, renderer, &mut take);
                let content_top = layout
                    .children()
                    .next()
                    .map_or(bounds.y, |content| content.bounds().y);
                for request in take.0 {
                    let pixels = state.pixels.unwrap_or(0.0);
                    let Some(to) = show_on_screen::target(
                        bounds.height,
                        content_top,
                        pixels,
                        range,
                        request.rect,
                    ) else {
                        continue;
                    };
                    if request.animate && (to - pixels).abs() >= near() {
                        // `moveTo` over the caret's 100 ms.
                        state.stop_runs();
                        state.page = Some(Page {
                            from: pixels,
                            to,
                            start: None,
                            curve: Curve::FastOutSlowIn,
                        });
                        state.dirty = true;
                    } else if to != pixels {
                        state.stop_runs();
                        scroll(scrollable, child, to);
                        state.moved(to, true);
                    }
                }
            }
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
        let bar = state.bar(bounds, content_extent(layout, self.axis));
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
        let (Some(bar), Some(bar_style), Some(clip)) =
            (bar, &self.style, bounds.intersection(viewport))
        else {
            return;
        };
        if opacity == 0.0 {
            return;
        }
        let hover = state.now.map_or(0.0, |now| state.hover.value(now));
        let color = thumb_color(bar_style, theme, state.drag.is_some(), hover, opacity);
        let radius = bar_style(theme, IDLE).vertical_rail.scroller.border.radius;
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
        let bar = state.bar(layout.bounds(), content_extent(layout, self.axis));
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
mod tests;
