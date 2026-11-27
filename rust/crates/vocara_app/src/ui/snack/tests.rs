use iced::advanced::subscription::into_recipes;
use iced::{mouse, Point, Size};

use super::*;
use crate::ui::core::widgets::animated::testing::{near, Frames, MS};

const DURATION: Duration = Duration::from_millis(4000);

fn look(text: &str) -> Element<'_, Infallible> {
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

/// A snack bar's `Material` with its text on it: black on white.
fn labelled_material(text: &str) -> Element<'_, Infallible> {
    iced::widget::container(
        crate::ui::core::text(text)
            .size(24)
            .color(iced::Color::BLACK),
    )
    .width(Length::Fill)
    .padding(8)
    .style(|_| crate::ui::core::theme::boxed(Some(iced::Color::WHITE), 0.0, None))
    .into()
}

#[test]
fn a_floating_snack_bar_fades_as_one_layer() {
    use crate::ui::core::widgets::animated::testing::{over, BACKGROUND};
    let size = Size::new(200.0, 120.0);
    let pixels = |window: &Frames| -> Vec<iced::Color> {
        (0..size.height as u32)
            .flat_map(|y| (0..size.width as u32).map(move |x| (x, y)))
            .map(|(x, y)| window.pixel(Point::new(x as f32, y as f32)))
            .collect()
    };
    let t0 = Instant::now();
    let mut q = SnackQueue::new();
    q.show(
        SnackBar::new(
            "Hg",
            DURATION,
            false,
            SnackStyle::Floating,
            labelled_material,
        ),
        t0 - TRANSITION,
    );
    q.frame(t0);
    let mut window = Frames::new(size);
    window.frame(q.view().expect("drawn"), &[], t0);
    let full = pixels(&window);
    // Leaving, halfway through its fade at its full height: each pixel, its
    // text's on its fill too, the bar's at the fade's opacity.
    let t1 = q.timer().expect("its timer").deadline;
    q.expired(q.current_seq().expect("a seq"), t1);
    let half = t1 + 125 * MS;
    q.frame(half);
    let (opacity, _) = progress(SnackStyle::Floating, leaving(&q), half);
    assert!(opacity > 0.1 && opacity < 0.9, "{opacity}");
    window.frame(q.view().expect("drawn"), &[], half);
    let mut text = 0;
    for (full, faded) in full.iter().zip(pixels(&window)) {
        let layer = over(
            iced::Color {
                a: opacity,
                ..*full
            },
            BACKGROUND,
        );
        assert!(near(faded, layer), "{faded:?}, not {layer:?}");
        text += usize::from(full.r < 0.5 && full.r > 0.0);
    }
    assert!(text > 10, "its text on the fill ({text} pixels)");
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
fn white_bar(_: &str) -> Element<'_, Infallible> {
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

#[test]
fn a_fixed_snack_bar_is_the_rounded_toast_card() {
    use crate::ui::core::theme::Studio;
    use crate::ui::core::widgets::animated::testing::BACKGROUND;
    // The toast card: radius 8, an inset 1 px outline, the raised fill.
    let mut window = Frames::new(Size::new(200.0, 60.0));
    window.frame(fixed_bar("Saved"), &[], Instant::now());
    let bar = theme::SNACK_BAR_DEFAULT_BACKGROUND;
    let at = |x: f32, y: f32| window.pixel(Point::new(x, y));
    // The corners' 8 px arcs leave their outermost pixels unpainted...
    assert!(near(at(0.0, 0.0), BACKGROUND), "{:?}", at(0.0, 0.0));
    assert!(near(at(199.0, 0.0), BACKGROUND), "{:?}", at(199.0, 0.0));
    // ... the straight edges are its outline, inside the bar ...
    assert!(
        near(at(0.0, 20.0), Studio::BORDER_STRONG),
        "{:?}",
        at(0.0, 20.0)
    );
    assert!(
        near(at(100.0, 0.0), Studio::BORDER_STRONG),
        "{:?}",
        at(100.0, 0.0)
    );
    // ... and inside it the fill.
    assert!(near(at(1.0, 20.0), bar), "{:?}", at(1.0, 20.0));
    assert!(near(at(8.0, 8.0), bar), "{:?}", at(8.0, 8.0));
}

// ------------------------------------------------------------- swiping

#[test]
fn a_snack_bar_swiped_away_goes_at_once_and_the_next_enters() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Fixed), t0);
    q.show(bar("b", false, SnackStyle::Fixed), t0);
    let a = q.current_seq().expect("a's seq");
    let t1 = t0 + TRANSITION + 1000 * MS;
    q.swiped(a, t1);
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("b"));
    assert!(q.leaving.is_none(), "no exit of its own: it has slid out");
    // "b" enters now, and has its time after its entrance.
    assert_eq!(showing(&q).value(t1), 0.0);
    assert!(showing(&q).value(t1 + 125 * MS) > 0.0);
    let timer = q.timer().expect("b's timer");
    assert!(about(timer.deadline, t1 + TRANSITION + DURATION));
    // A stale report takes down nothing.
    q.swiped(a, t1);
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("b"));
}

#[test]
fn one_swiped_while_it_leaves_lets_the_next_enter_now() {
    let mut q = SnackQueue::new();
    let t0 = Instant::now();
    q.show(bar("a", false, SnackStyle::Floating), t0);
    q.show(bar("b", false, SnackStyle::Floating), t0);
    let a = q.timer().expect("a's timer");
    let t1 = a.deadline;
    q.expired(a.seq, t1);
    // "a" leaves; "b" waits for it until t1 + TRANSITION.
    assert!(q.leaving.is_some());
    let t2 = t1 + 100 * MS;
    q.swiped(a.seq, t2);
    assert!(q.leaving.is_none());
    assert_eq!(showing(&q).value(t2), 0.0);
    assert!(showing(&q).value(t2 + 125 * MS) > 0.0, "entering from t2");
    let timer = q.timer().expect("b's timer");
    assert!(about(timer.deadline, t2 + TRANSITION + DURATION));
}

/// What the test window draws as a snack bar's `Material`: a 40 tall white
/// bar.
fn white_material(_: &str) -> Element<'_, Infallible> {
    iced::widget::container(iced::widget::Space::new().width(Length::Fill).height(40))
        .style(|_| crate::ui::core::theme::boxed(Some(iced::Color::WHITE), 0.0, None))
        .into()
}

/// A queue showing one snack bar of `style`, all in by `t0`.
fn shown(style: SnackStyle, t0: Instant) -> SnackQueue {
    let mut q = SnackQueue::new();
    q.show(
        SnackBar::new("a", DURATION, false, style, white_material),
        t0 - TRANSITION,
    );
    q.frame(t0);
    q
}

fn left_press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn left_release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

/// Pointer events are handled at `at` from here on.
fn events_at(at: Instant) {
    crate::ui::core::widgets::dismissible::testing::set_event_time(Some(at));
}

#[test]
fn a_floating_snack_bar_follows_a_drag_down_on_its_material_and_slides_back() {
    // A 200 × 100 window: the bar's box is its 40 px Material in a 16 px
    // margin, 72 tall at the bottom (y 28 to 100), the Material at y 44 to
    // 84, x 16 to 184.
    let mut window = Frames::new(Size::new(200.0, 100.0));
    let t0 = Instant::now();
    let mut q = shown(SnackStyle::Floating, t0);
    let white = |w: &Frames, y: f32| near(w.pixel(Point::new(100.0, y)), iced::Color::WHITE);
    events_at(t0);
    window.frame(q.view().expect("drawn"), &[], t0);
    assert!(white(&window, 46.0) && !white(&window, 42.0), "at rest");
    // Over the Material, the bar takes the pointer (nothing beneath shows
    // its cursor); over the margin, it does not.
    window.point_at(Point::new(100.0, 60.0));
    window.frame(q.view().expect("drawn"), &[], t0);
    assert_eq!(window.interaction(), mouse::Interaction::Idle);

    // Down 20 px, slowly: it follows, clipped at the page's bottom.
    let t1 = t0 + 100 * MS;
    events_at(t1);
    window.frame(q.view().expect("drawn"), &[left_press()], t1);
    for (i, y) in [65.0, 70.0, 75.0, 80.0].into_iter().enumerate() {
        let t = t1 + 50 * MS * (i as u32 + 1);
        events_at(t);
        window.point_at(Point::new(100.0, y));
        q.frame(t);
        window.frame(q.view().expect("drawn"), &[], t);
    }
    assert!(!white(&window, 60.0) && white(&window, 66.0), "20 px down");
    assert!(white(&window, 99.0), "its bottom under the page's edge");

    // Let go 20 / 72 of the way down: it slides back over that share of
    // 200 ms (56 ms), and nothing is reported.
    let up = t1 + 300 * MS;
    events_at(up);
    let (more, messages) = window.frame_messages(q.view().expect("drawn"), &[left_release()], up);
    assert!(more, "it asks for frames while it slides");
    assert!(messages.is_empty());
    let back = up + 60 * MS;
    q.frame(back);
    let (more, messages) = window.frame_messages(q.view().expect("drawn"), &[], back);
    assert!(!more && messages.is_empty());
    assert!(
        white(&window, 46.0) && !white(&window, 42.0),
        "back at rest"
    );
    assert_eq!(q.current().map(|b| b.text.as_str()), Some("a"));

    // A drag on the margin is no drag.
    window.point_at(Point::new(100.0, 36.0));
    let t2 = back + 100 * MS;
    events_at(t2);
    window.frame(q.view().expect("drawn"), &[left_press()], t2);
    assert_eq!(window.interaction(), mouse::Interaction::None);
    window.point_at(Point::new(100.0, 90.0));
    events_at(t2 + 50 * MS);
    window.frame(q.view().expect("drawn"), &[], t2 + 50 * MS);
    assert!(white(&window, 46.0), "it stays put");
    events_at(t2 + 100 * MS);
    window.frame(q.view().expect("drawn"), &[left_release()], t2 + 100 * MS);
    crate::ui::core::widgets::dismissible::testing::set_event_time(None);
}

#[test]
fn a_fixed_snack_bar_dragged_past_40_percent_slides_out_and_is_swiped_away() {
    // The fixed 40 px bar at the window's bottom (y 60 to 100), taking the
    // drag anywhere on it.
    let mut window = Frames::new(Size::new(200.0, 100.0));
    let t0 = Instant::now();
    let mut q = shown(SnackStyle::Fixed, t0);
    let seq = q.current_seq().expect("its seq");
    let white = |w: &Frames, y: f32| near(w.pixel(Point::new(5.0, y)), iced::Color::WHITE);
    window.point_at(Point::new(5.0, 62.0));
    events_at(t0);
    window.frame(q.view().expect("drawn"), &[left_press()], t0);
    // 20 px of 40: past the threshold.
    for (i, y) in [67.0, 72.0, 77.0, 82.0].into_iter().enumerate() {
        let t = t0 + 50 * MS * (i as u32 + 1);
        events_at(t);
        window.point_at(Point::new(5.0, y));
        window.frame(q.view().expect("drawn"), &[], t);
    }
    assert!(!white(&window, 78.0) && white(&window, 82.0), "half down");
    let up = t0 + 250 * MS;
    events_at(up);
    let (_, messages) = window.frame_messages(q.view().expect("drawn"), &[left_release()], up);
    assert!(messages.is_empty(), "not yet");
    // Half of 200 ms to go.
    let (more, messages) = window.frame_messages(q.view().expect("drawn"), &[], up + 99 * MS);
    assert!(more && messages.is_empty());
    let (more, messages) = window.frame_messages(q.view().expect("drawn"), &[], up + 101 * MS);
    assert!(!more);
    assert_eq!(messages, vec![SnackEvent::Swiped(seq)]);
    assert!(!white(&window, 99.0), "out of sight");
    q.swiped(seq, up + 101 * MS);
    assert!(q.current().is_none() && q.view().is_none());
    crate::ui::core::widgets::dismissible::testing::set_event_time(None);
}

#[test]
fn a_new_snack_bar_in_a_swiped_ones_place_starts_at_rest() {
    let mut window = Frames::new(Size::new(200.0, 100.0));
    let t0 = Instant::now();
    let mut q = shown(SnackStyle::Fixed, t0);
    q.show(
        SnackBar::new("b", DURATION, false, SnackStyle::Fixed, white_material),
        t0,
    );
    let seq = q.current_seq().expect("a's seq");
    // Dragged all the way out and let go: swiped at once.
    window.point_at(Point::new(5.0, 62.0));
    events_at(t0);
    window.frame(q.view().expect("drawn"), &[left_press()], t0);
    events_at(t0 + 50 * MS);
    window.point_at(Point::new(5.0, 110.0));
    window.frame(q.view().expect("drawn"), &[], t0 + 50 * MS);
    events_at(t0 + 100 * MS);
    let (_, messages) =
        window.frame_messages(q.view().expect("drawn"), &[left_release()], t0 + 100 * MS);
    assert_eq!(messages, vec![SnackEvent::Swiped(seq)]);
    let t1 = t0 + 100 * MS;
    q.swiped(seq, t1);
    // "b" enters where "a" was drawn, from its own rest, not "a"'s slide.
    let t2 = t1 + TRANSITION;
    q.frame(t2);
    window.point_at(Point::new(-10.0, -10.0));
    window.frame(q.view().expect("drawn"), &[], t2);
    assert!(near(
        window.pixel(Point::new(5.0, 62.0)),
        iced::Color::WHITE
    ));
    crate::ui::core::widgets::dismissible::testing::set_event_time(None);
}

#[test]
fn a_two_finger_swipe_down_on_it_swipes_it_away_and_its_momentum_moves_nothing() {
    use crate::ui::core::widgets::pan_zoom::testing::Trackpad;
    use iced_winit::gesture::Phase;

    // The fixed 40 px bar at the window's bottom (y 60 to 100), the
    // pointer resting on it.
    let mut window = Frames::new(Size::new(200.0, 100.0));
    let t0 = Instant::now();
    let q = shown(SnackStyle::Fixed, t0);
    let seq = q.current_seq().expect("its seq");
    let white = |w: &Frames, y: f32| near(w.pixel(Point::new(5.0, y)), iced::Color::WHITE);
    window.point_at(Point::new(5.0, 70.0));
    let mut pad = Trackpad::new();
    events_at(t0);
    let (_, messages) = window.frame_sent(q.view().expect("drawn"), &[pad.began(t0)], t0);
    assert!(messages.is_empty());
    // Down 6 px each 10 ms, 24 px in all: past 40 % of its height, the
    // content following the fingers.
    let mut t = t0;
    for _ in 0..4 {
        t += 10 * MS;
        events_at(t);
        window.frame_sent(q.view().expect("drawn"), &[pad.changed(0.0, 6.0, t)], t);
    }
    assert!(!white(&window, 82.0) && white(&window, 86.0), "24 px down");
    // The fingers lift: 600 px/s over 24 px is no fling (under 700), but
    // past 40 % it slides out over the rest of 200 ms (80 ms).
    let up = t + 5 * MS;
    events_at(up);
    let (more, messages) = window.frame_sent(q.view().expect("drawn"), &[pad.ended(up)], up);
    assert!(more && messages.is_empty());
    // The momentum macOS sends after it reaches nothing: it slides on.
    let m = up + 10 * MS;
    events_at(m);
    let (_, messages) = window.frame_sent(
        q.view().expect("drawn"),
        &[pad.momentum(Phase::Began, 0.0, -30.0, m)],
        m,
    );
    assert!(messages.is_empty());
    assert!(!white(&window, 84.0), "further out, not back");
    let (more, messages) = window.frame_messages(q.view().expect("drawn"), &[], up + 81 * MS);
    assert!(!more);
    assert_eq!(messages, vec![SnackEvent::Swiped(seq)]);
    crate::ui::core::widgets::dismissible::testing::set_event_time(None);
}

#[test]
fn a_two_finger_swipe_that_started_off_it_leaves_it_at_rest() {
    use crate::ui::core::widgets::pan_zoom::testing::Trackpad;

    let mut window = Frames::new(Size::new(200.0, 100.0));
    let t0 = Instant::now();
    let q = shown(SnackStyle::Fixed, t0);
    let white = |w: &Frames, y: f32| near(w.pixel(Point::new(5.0, y)), iced::Color::WHITE);
    let mut pad = Trackpad::new();
    // It starts over the page above the bar; the pointer is then on it.
    window.point_at(Point::new(5.0, 20.0));
    events_at(t0);
    window.frame_sent(q.view().expect("drawn"), &[pad.began(t0)], t0);
    window.point_at(Point::new(5.0, 70.0));
    let t1 = t0 + 10 * MS;
    events_at(t1);
    window.frame_sent(q.view().expect("drawn"), &[pad.changed(0.0, 30.0, t1)], t1);
    window.frame_sent(q.view().expect("drawn"), &[pad.ended(t1)], t1);
    assert!(white(&window, 62.0), "at rest");
    crate::ui::core::widgets::dismissible::testing::set_event_time(None);
}
