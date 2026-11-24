use iced::advanced::subscription::into_recipes;
use iced::{Point, Size};

use super::*;
use crate::ui::core::widgets::animated::testing::{near, Frames, MS};

const DURATION: Duration = Duration::from_millis(4000);

fn look(text: &str, _: f32) -> Element<'_, Infallible> {
    crate::ui::core::text(text).into()
}

fn bar(text: &str, clears: bool, style: SnackStyle) -> SnackBar {
    SnackBar::new(text, DURATION, clears, style, look)
}

/// The controller of the one showing.
fn showing(q: &SnackQueue) -> &Controller {
    &q.queue
        .front()
        .and_then(|e| e.motion.as_ref())
        .expect("one showing")
        .controller
}

/// The controller of the one leaving.
fn leaving(q: &SnackQueue) -> &Controller {
    &q.leaving
        .as_ref()
        .and_then(|e| e.motion.as_ref())
        .expect("one leaving")
        .controller
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 2e-3
}

/// Two instants within a microsecond (durations scale in `f32`).
fn about(a: Instant, b: Instant) -> bool {
    a.max(b).duration_since(a.min(b)) < Duration::from_micros(1)
}

#[test]
fn its_time_counts_from_the_end_of_its_entrance() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("a"), "at once");
    let timer = q.timer().expect("its timer");
    assert_eq!(timer.deadline, t0 + TRANSITION + DURATION);
}

#[test]
fn a_floating_snack_bar_fades_in_while_it_grows_then_fades_out() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    let at = |q: &SnackQueue, t| progress(SnackStyle::Floating, showing(q), t);
    assert_eq!(at(&q, t0), (0.0, 0.0), "start");
    // Halfway: the fade (40 % to 60 %, easing in) has just begun; the
    // height is halfway along its quartic ease.
    let (opacity, height) = at(&q, t0 + 125 * MS);
    assert!(
        close(opacity, Curve::SnackBarM3FadeIn.transform(0.5)),
        "{opacity}"
    );
    assert!(
        close(height, Curve::EaseInOutQuart.transform(0.5)),
        "{height}"
    );
    assert_eq!(at(&q, t0 + TRANSITION), (1.0, 1.0), "end");

    // Its time up, it fades out at its full height, and the state moves on
    // at once.
    let t1 = t0 + TRANSITION + DURATION;
    q.expired(q.current_seq().expect("a seq"), t1);
    assert!(q.current().is_none());
    let out = |t| progress(SnackStyle::Floating, leaving(&q), t);
    assert_eq!(out(t1), (1.0, 1.0), "start");
    assert_eq!(out(t1 + 100 * MS).0, 1.0, "still opaque at 40 %");
    let (opacity, height) = out(t1 + 125 * MS);
    assert!(close(opacity, Curve::SnackBarM3FadeIn.transform(0.5)));
    assert_eq!(height, 1.0, "its height holds");
    assert_eq!(out(t1 + 150 * MS).0, 0.0, "gone at 60 %");
    q.frame(t1 + TRANSITION);
    assert!(q.leaving.is_none(), "dropped once it has left");
}

#[test]
fn a_fixed_snack_bar_slides_up_and_back_down() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Fixed), t0);
    let at = |q: &SnackQueue, t| progress(SnackStyle::Fixed, showing(q), t);
    assert_eq!(at(&q, t0), (1.0, 0.0), "start");
    let (_, height) = at(&q, t0 + 125 * MS);
    assert!(close(height, Curve::FastOutSlowIn.transform(0.5)), "middle");
    assert_eq!(at(&q, t0 + TRANSITION), (1.0, 1.0), "end");
    let t1 = t0 + 10 * TRANSITION;
    q.expired(q.current_seq().expect("a seq"), t1);
    let (_, height) = progress(SnackStyle::Fixed, leaving(&q), t1 + 125 * MS);
    assert!(
        close(height, Curve::FastOutSlowIn.transform(0.5)),
        "back down"
    );
}

#[test]
fn the_next_enters_once_the_one_before_has_left() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Fixed), t0);
    q.show(bar("b", false, SnackStyle::Fixed), t0);
    assert_eq!(q.texts().collect::<Vec<_>>(), ["a", "b"], "queued");
    let first = q.timer().expect("a's timer");
    let t1 = first.deadline;
    q.expired(first.seq, t1);
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("b"));
    // "b" waits out "a"'s exit, then enters, then has its time.
    assert_eq!(showing(&q).value(t1 + TRANSITION), 0.0);
    assert!(showing(&q).value(t1 + TRANSITION + 125 * MS) > 0.0);
    let second = q.timer().expect("b's timer");
    assert!(about(
        second.deadline,
        t1 + TRANSITION + TRANSITION + DURATION
    ));
    // A stale timer takes down nothing.
    q.expired(first.seq, t1 + TRANSITION);
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("b"));
}

#[test]
fn a_clearing_snack_bar_sends_the_one_up_away_from_where_it_got() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    q.show(bar("b", false, SnackStyle::Floating), t0);
    // Cleared 100 ms into "a"'s entrance (40 % in): "a" leaves over 40 % of
    // the exit, "b" is dropped, and "c" enters once "a" has left.
    let t1 = t0 + 100 * MS;
    q.show(bar("c", true, SnackStyle::Floating), t1);
    assert_eq!(q.texts().collect::<Vec<_>>(), ["c"]);
    assert!(close(leaving(&q).value(t1), 0.4));
    assert!(about(leaving(&q).ends_at(t1), t1 + 100 * MS));
    let timer = q.timer().expect("c's timer");
    assert!(about(timer.deadline, t1 + 100 * MS + TRANSITION + DURATION));
    // Interrupted entering, "a" keeps its entrance curve on the way out
    // (`CurvedAnimation`'s direction holds until it is dismissed).
    let at = t1 + 50 * MS;
    let (_, height) = progress(SnackStyle::Floating, leaving(&q), at);
    assert!(close(
        height,
        Curve::EaseInOutQuart.transform(leaving(&q).value(at))
    ));
}

#[test]
fn one_not_yet_in_sight_is_cleared_at_once() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    q.show(bar("b", true, SnackStyle::Floating), t0);
    assert!(q.leaving.is_none(), "nothing to see leave");
    assert_eq!(q.timer().expect("b's").deadline, t0 + TRANSITION + DURATION);
}

#[test]
fn a_snack_bar_handed_over_carries_on_from_where_it_got() {
    let mut page = SnackQueue::new();
    let t0 = Instant::now();
    page.show(bar("a", false, SnackStyle::Floating), t0);
    page.show(bar("b", false, SnackStyle::Floating), t0);
    // Handed over 100 ms into its entrance.
    let t1 = t0 + 100 * MS;
    let bars = page.take(t1);
    assert!(page.is_empty() && page.view().is_none());
    assert_eq!(bars[0].entrance, 150 * MS);
    assert_eq!(bars[0].left, DURATION);
    assert_eq!((bars[1].entrance, bars[1].left), (TRANSITION, DURATION));
    let mut app = SnackQueue::new();
    app.adopt(bars, t1);
    assert!(close(showing(&app).value(t1), 0.4));
    assert!(about(
        app.timer().expect("a's").deadline,
        t0 + TRANSITION + DURATION
    ));
    // Later, part way through its time.
    let mut page = SnackQueue::new();
    page.show(bar("a", false, SnackStyle::Floating), t0);
    let t2 = t0 + TRANSITION + 1000 * MS;
    let bars = page.take(t2);
    assert_eq!(
        (bars[0].entrance, bars[0].left),
        (Duration::ZERO, 3000 * MS)
    );
    let mut app = SnackQueue::new();
    app.adopt(bars, t2);
    assert_eq!(showing(&app).value(t2), 1.0, "in full view");
    assert_eq!(app.timer().expect("a's").deadline, t2 + 3000 * MS);
    // Its time up: not handed over.
    let mut page = SnackQueue::new();
    page.show(bar("a", false, SnackStyle::Floating), t0);
    assert!(page.take(t0 + TRANSITION + DURATION).is_empty());
}

#[test]
fn the_frames_run_only_while_one_enters_or_leaves() {
    let mut q = SnackQueue::new();
    assert!(!q.is_animating(Instant::now()));
    assert!(into_recipes(q.subscription()).is_empty(), "nothing at all");
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    assert!(q.is_animating(t0 + 100 * MS));
    assert_eq!(
        into_recipes(q.subscription()).len(),
        2,
        "its timer and frames"
    );
    assert!(!q.is_animating(t0 + TRANSITION), "in, still");
    // Shown long ago: its timer alone.
    let mut q = SnackQueue::new();
    q.show(bar("a", false, SnackStyle::Floating), t0 - 1000 * MS);
    assert_eq!(into_recipes(q.subscription()).len(), 1);
}

/// What the fixed bar draws: a 40 tall white bar at the window's bottom.
fn white_bar(_: &str, _: f32) -> Element<'_, Infallible> {
    iced::widget::container(iced::widget::Space::new().width(Length::Fill).height(40))
        .style(|_| crate::ui::core::theme::boxed(Some(iced::Color::WHITE), 0.0, None))
        .into()
}

#[test]
fn a_fixed_snack_bar_is_drawn_revealed_from_the_bottom_edge() {
    let mut q = SnackQueue::new();
    let mut window = Frames::new(Size::new(100.0, 100.0));
    let t0 = Instant::now();
    q.show(
        SnackBar::new("a", DURATION, false, SnackStyle::Fixed, white_bar),
        t0,
    );
    let white = |w: &Frames, y: f32| near(w.pixel(Point::new(50.0, y)), iced::Color::WHITE);
    q.frame(t0);
    window.frame(q.view().expect("drawn"), &[], t0);
    assert!(!white(&window, 99.0), "start: none of it");
    q.frame(t0 + 125 * MS);
    window.frame(q.view().expect("drawn"), &[], t0);
    // Up 78 % of its 40 px (fastOutSlowIn at half time): its top edge at 69.
    assert!(white(&window, 72.0) && !white(&window, 66.0), "middle");
    q.frame(t0 + TRANSITION);
    window.frame(q.view().expect("drawn"), &[], t0);
    assert!(
        white(&window, 61.0) && !white(&window, 59.0),
        "end: all of it"
    );
}
