use iced::widget::{button, column, container, mouse_area, Column, Row, Space};
use iced_winit::gesture::Phase;

use super::pan_zoom::testing::{set_event_time, Sent, Trackpad};
use super::scroll_fling::ClampingScrollSimulation;
use super::*;
use crate::ui::core::theme::{fill, Zinc};
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

/// A mouse wheel turned `down` lines (40 px each).
fn lines(down: f32) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y: -down },
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
    // Half way down the content (the wheel on the track is the
    // bar's, 40 px a line), half way down its 52 px travel.
    window.frame(view(1000.0), &[lines(11.25)], t0 + 400 * MS);
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scrolled {
    Outer(f32),
    Inner(f32),
}

#[test]
fn the_wheel_on_the_shown_track_is_the_bars_not_the_contents() {
    // A view whose content is, at the top, a scroll view of its own
    // across its whole width, under the track too.
    let view = || -> Element<'_, Scrolled> {
        fading_scrollbar(
            scrollable(column![
                scrollable(column![Space::new().width(100).height(400)])
                    .width(Length::Fill)
                    .height(150)
                    .on_scroll(|v| Scrolled::Inner(v.absolute_offset().y)),
                Space::new().height(300),
            ])
            .width(100)
            .height(100)
            .on_scroll(|v| Scrolled::Outer(v.absolute_offset().y)),
        )
    };
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(95.0, 70.0));
    window.frame(view(), &[], t0);
    window.frame(view(), &[], t0 + 300 * MS);
    // A wheel's line (40 px) on the shown track: the view scrolls, the
    // content under the track does not.
    let (_, scrolled) = window.frame_messages(view(), &[lines(1.0)], t0 + 316 * MS);
    assert_eq!(scrolled, [Scrolled::Outer(40.0)]);
    // A trackpad's swipe there, and its momentum: nothing.
    let mut pad = Trackpad::new();
    let at = t0 + 332 * MS;
    let swipe = [
        pad.began(at),
        pad.changed(0.0, -30.0, at),
        pad.ended(at),
        pad.momentum(Phase::Began, 0.0, -20.0, at),
    ];
    let (_, scrolled) = window.frame_sent(view(), &swipe, at);
    assert_eq!(scrolled, []);
    // A precise device's points without phases: a scroll signal, the
    // view's.
    let (_, scrolled) = window.frame_sent(view(), &[pad.pixels(0.0, -10.0, at)], t0 + 340 * MS);
    assert_eq!(scrolled, [Scrolled::Outer(50.0)]);
    // Off the track the content has the wheel first.
    window.point_at(Point::new(50.0, 70.0));
    let (_, scrolled) = window.frame_messages(view(), &[lines(1.0)], t0 + 348 * MS);
    assert_eq!(scrolled, [Scrolled::Inner(40.0)]);
}

#[test]
fn a_swipe_begun_on_the_content_stays_the_views_when_the_bar_comes_in() {
    let mut window = window();
    let t0 = Instant::now();
    // On the track's place while the bar is hidden: the view's.
    window.point_at(Point::new(95.0, 50.0));
    window.frame(view(1000.0), &[], t0);
    let mut pad = Trackpad::new();
    set_event_time(Some(t0));
    let (_, scrolled) = window.frame_sent(view(1000.0), &[pad.began(t0)], t0);
    assert_eq!(offset(&scrolled), None);
    let mut at = t0;
    // The bar fades in under the still pointer; the swipe goes on
    // dragging the view with the fingers.
    for i in 1..=30 {
        at += 16 * MS;
        set_event_time(Some(at));
        let (_, scrolled) = window.frame_sent(view(1000.0), &[pad.changed(0.0, -10.0, at)], at);
        assert_eq!(offset(&scrolled), Some(10.0 * i as f32));
    }
    assert!(window.pixel(Point::new(94.0, 50.0)) != BACKGROUND, "shown");
    // The fingers lift at 625 px/s: a fling, which the momentum macOS
    // sends after adds nothing to.
    window.frame_sent(view(1000.0), &[pad.ended(at)], at);
    let (_, scrolled) = window.frame_sent(
        view(1000.0),
        &[pad.momentum(Phase::Began, 0.0, -10.0, at)],
        at + 16 * MS,
    );
    let flung = ClampingScrollSimulation::new(300.0, 625.0).x(0.016) as f32;
    assert!(offset(&scrolled).is_some_and(|y| (y - flung).abs() < 1e-3));
    // A new swipe there, once the fling is over, is the track's.
    assert!(!window.frame(view(1000.0), &[], at + 350 * MS), "over");
    let next = at + 400 * MS;
    set_event_time(Some(next));
    let swipe = [pad.began(next), pad.changed(0.0, -10.0, next)];
    let (_, scrolled) = window.frame_sent(view(1000.0), &swipe, next);
    set_event_time(None);
    assert_eq!(offset(&scrolled), None);
}

#[test]
fn a_held_thumb_keeps_the_wheel_on_the_track_from_scrolling() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(94.0, 10.0));
    window.frame(view(1000.0), &[], t0);
    window.frame(view(1000.0), &[], t0 + 300 * MS);
    window.frame(view(1000.0), &[press()], t0 + 316 * MS);
    let (_, scrolled) = window.frame_messages(view(1000.0), &[lines(1.0)], t0 + 332 * MS);
    assert_eq!(offset(&scrolled), None);
}

#[test]
fn a_page_under_a_device_pixel_jumps() {
    // 0.8 px to scroll: a press below the thumb pages 0.8.
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(94.0, 99.8));
    set_scale_factor(1.0);
    window.frame(view(100.8), &[], t0);
    window.frame(view(100.8), &[], t0 + 300 * MS);
    let (_, scrolled) = window.frame_messages(view(100.8), &[press(), release()], t0 + 316 * MS);
    assert!(
        offset(&scrolled).is_some_and(|y| (y - 0.8).abs() < 1e-4),
        "a pixel's tolerance: at once"
    );

    // On a Retina display half a pixel is the tolerance: it runs.
    let mut window = Frames::new(Size::new(120.0, 120.0));
    set_scale_factor(2.0);
    window.point_at(Point::new(94.0, 99.8));
    window.frame(view(100.8), &[], t0);
    window.frame(view(100.8), &[], t0 + 300 * MS);
    let (more, scrolled) = window.frame_messages(view(100.8), &[press(), release()], t0 + 316 * MS);
    set_scale_factor(1.0);
    assert_eq!(offset(&scrolled), None, "it starts at the next frame");
    assert!(more);
}

#[test]
fn a_view_that_leaves_a_still_pointer_lets_its_bar_fade() {
    // The view `top` down a taller window (a column leaves out a
    // child of no size: the view is never its first).
    let view = |top: f32| -> Element<'_, f32> {
        column![
            Space::new().height(top),
            fading_scrollbar(
                scrollable(column![Space::new().width(100).height(1000.0)])
                    .width(100)
                    .height(100)
            ),
        ]
        .into()
    };
    let mut window = Frames::new(Size::new(120.0, 400.0));
    let t0 = Instant::now();
    // The pointer on the track holds the bar in.
    window.point_at(Point::new(94.0, 30.0));
    window.frame(view(1.0), &[], t0);
    window.frame(view(1.0), &[], t0 + 300 * MS);
    window.frame(view(1.0), &[], t0 + 2000 * MS);
    let thumb_at = |top: f32| Point::new(94.0, top + 24.0);
    assert!(near(window.pixel(thumb_at(1.0)), thumb(0.65)), "held");
    // The view moves down from under the pointer, which stays still:
    // after the frame the bar learns it left, and fades 600 ms on.
    let t1 = t0 + 2100 * MS;
    assert!(window.frame_still(view(201.0), &[], t1), "a frame to come");
    window.frame_still(view(201.0), &[], t1 + 16 * MS);
    assert!(!near(window.pixel(thumb_at(201.0)), BACKGROUND), "still in");
    window.frame_still(view(201.0), &[], t1 + 616 * MS);
    window.frame_still(view(201.0), &[], t1 + 1000 * MS);
    assert!(near(window.pixel(thumb_at(201.0)), BACKGROUND), "faded");
}

#[test]
fn a_text_field_inside_has_its_caret_shown_with_its_scroll_padding() {
    use crate::ui::core::widgets::focus_traversal::focus_scope;
    use crate::ui::core::widgets::text_field::text_input;
    use iced::keyboard::{self, key::Named};

    // Two 14 px fields 300 apart in a 100 px view, room below them.
    let field = || {
        text_input("", "text")
            .on_input(|_| -1.0)
            .size(14)
            .line_height(1.0)
            .padding(0)
    };
    let view = || -> Element<'_, f32> {
        focus_scope(fading_scrollbar(
            scrollable(column![
                field(),
                Space::new().height(300),
                field(),
                Space::new().height(100)
            ])
            .width(100)
            .height(100),
        ))
    };
    let tab = || iced_test::simulator::press_key(keyboard::Key::Named(Named::Tab), None);
    let mut window = window();
    let offset = |window: &mut Frames| {
        let mut read = Read::default();
        window.operate(view(), &mut read);
        read.offset.expect("a scrollable")
    };
    let t0 = Instant::now();
    window.frame(view(), &[tab()], t0);
    window.frame(view(), &[], t0 + 16 * MS);
    window.frame(view(), &[], t0 + 200 * MS);
    assert_eq!(offset(&mut window), 0.0, "the first is in view");

    // Tab to the second: at once its text box ends at the view's
    // bottom (`ensureVisible`); after its frame, the view runs on over
    // 100 ms until its selection's line (Tab selected all its text) and
    // 24 px of scroll padding below it show.
    let t1 = t0 + 300 * MS;
    window.frame(view(), &[tab()], t1);
    assert_eq!(offset(&mut window), 314.0 + 14.0 - 100.0);
    window.frame(view(), &[], t1 + 16 * MS);
    assert_eq!(offset(&mut window), 228.0, "the run's first frame");
    window.frame(view(), &[], t1 + 66 * MS);
    // (The scroll view draws on whole pixels.)
    let half = 228.0 + 24.0 * Curve::FastOutSlowIn.transform(0.5);
    assert_eq!(offset(&mut window), half.round());
    window.frame(view(), &[], t1 + 116 * MS);
    assert_eq!(offset(&mut window), 314.0 + 14.0 + 24.0 - 100.0);

    // Round to the first, above the view: `ensureVisible` going forward
    // leaves it; its caret's reveal brings the view back to the top.
    let t2 = t1 + 300 * MS;
    window.frame(view(), &[tab()], t2);
    assert_eq!(offset(&mut window), 252.0);
    window.frame(view(), &[], t2 + 16 * MS);
    window.frame(view(), &[], t2 + 116 * MS);
    assert_eq!(offset(&mut window), 0.0);
}

// ------------------------------------------------- swipes, flings, wheels

const RED: Color = Color::from_rgb(1.0, 0.0, 0.0);
const BLUE: Color = Color::from_rgb(0.0, 0.0, 1.0);

/// `count` 50 px stripes along `axis`, 100 px across, red first.
fn stripes<'a>(count: usize, axis: Axis) -> Vec<Element<'a, f32>> {
    (0..count)
        .map(|i| {
            let (width, height) = match axis {
                Axis::Vertical => (100.0, 50.0),
                Axis::Horizontal => (50.0, 100.0),
            };
            container(Space::new().width(width).height(height))
                .style(fill(if i % 2 == 0 { RED } else { BLUE }))
                .into()
        })
        .collect()
}

/// A 100 × 100 view with no bar of twenty stripes down (900 px to
/// scroll), publishing its offset.
fn list<'a>() -> Element<'a, f32> {
    scrollable(Column::with_children(stripes(20, Axis::Vertical)))
        .width(100)
        .height(100)
        .on_scroll(|viewport| viewport.absolute_offset().y)
        .into()
}

/// [`list`] across.
fn strip<'a>() -> Element<'a, f32> {
    scrollable(Row::with_children(stripes(20, Axis::Horizontal)))
        .horizontal()
        .width(100)
        .height(100)
        .on_scroll(|viewport| viewport.absolute_offset().x)
        .into()
}

/// Sends `sent`, handled at `at`, and draws the frame at `at`: the offset
/// the view published, if it did.
fn send(window: &mut Frames, view: Element<'_, f32>, sent: &[Sent], at: Instant) -> Option<f32> {
    set_event_time(Some(at));
    let (_, scrolled) = window.frame_sent(view, sent, at);
    set_event_time(None);
    offset(&scrolled)
}

/// A swipe up over `view` from `t0`, four moves of 10 px 10 ms apart
/// (1000 px/s): the time of its last move.
fn swipe_up(
    window: &mut Frames,
    pad: &mut Trackpad,
    view: fn() -> Element<'static, f32>,
    t0: Instant,
) -> Instant {
    send(window, view(), &[pad.began(t0)], t0);
    let mut at = t0;
    for _ in 0..4 {
        at += 10 * MS;
        send(window, view(), &[pad.changed(0.0, -10.0, at)], at);
    }
    at
}

#[test]
fn a_two_finger_swipe_drags_the_view_with_the_fingers() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 50.0));
    window.frame(list(), &[], t0);
    let mut pad = Trackpad::new();
    // The start holds the view; alone in the gesture's arena, its drag
    // starts there, and each move of the fingers moves the content with
    // them from the first.
    assert_eq!(send(&mut window, list(), &[pad.began(t0)], t0), None);
    let at = t0 + 8 * MS;
    let moved = send(&mut window, list(), &[pad.changed(0.0, -12.0, at)], at);
    assert_eq!(moved, Some(12.0));
    // The first stripe's red now ends at 38, the blue then.
    assert!(near(window.pixel(Point::new(40.0, 30.0)), RED));
    assert!(near(window.pixel(Point::new(40.0, 45.0)), BLUE));
    let at = t0 + 16 * MS;
    let moved = send(&mut window, list(), &[pad.changed(0.0, -7.5, at)], at);
    assert_eq!(moved, Some(19.5));
    // Across: nothing, the view's drag taking the pan along its axis.
    let at = t0 + 24 * MS;
    let moved = send(&mut window, list(), &[pad.changed(-30.0, 0.0, at)], at);
    assert_eq!(moved, None);
    // Back down with them.
    let at = t0 + 32 * MS;
    let moved = send(&mut window, list(), &[pad.changed(5.0, 4.0, at)], at);
    assert_eq!(moved, Some(15.5));
    // Drawn on the whole pixel: the red to 34, the blue to 84, the red.
    assert!(near(window.pixel(Point::new(40.0, 25.0)), RED));
    assert!(near(window.pixel(Point::new(40.0, 50.0)), BLUE));
    assert!(near(window.pixel(Point::new(40.0, 92.0)), RED));
}

#[test]
fn lifting_the_fingers_flings_the_view_on_the_clamping_simulation_and_the_momentum_moves_nothing() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 50.0));
    window.frame(list(), &[], t0);
    let mut pad = Trackpad::new();
    let last = swipe_up(&mut window, &mut pad, list, t0);
    // The fingers lift 5 ms after the last move: from 40 px at 1000 px/s
    // the view flies on from its next frame, here the lift's own, which
    // leaves it where it is.
    let up = last + 5 * MS;
    let ended = send(&mut window, list(), &[pad.ended(up)], up);
    assert_eq!(ended, None, "its first frame");
    let simulation = ClampingScrollSimulation::new(40.0, 1000.0);
    let mut k = 0;
    loop {
        k += 1;
        let since = 16 * k * MS;
        let at = up + since;
        let t = since.as_secs_f64();
        // macOS's momentum comes along every other frame.
        let sent = if k % 2 == 0 {
            vec![pad.momentum(Phase::Changed, 0.0, -30.0, at)]
        } else {
            vec![]
        };
        set_event_time(Some(at));
        let (more, scrolled) = window.frame_sent(list(), &sent, at);
        set_event_time(None);
        let expected = simulation.x(t) as f32;
        let y = offset(&scrolled).expect("it flies on");
        assert!((y - expected).abs() < 1e-3, "{t}: {y} for {expected}");
        if simulation.is_done(t) {
            assert!(!more, "at rest");
            break;
        }
        assert!(more, "{t}: flying");
    }
    // Some 194 px on, where it rests.
    let rest = simulation.x(10.0) as f32;
    assert!((rest - 234.3).abs() < 0.1, "{rest}");
    // The red of the fifth stripe ends at 250 - 234.3, the blue then.
    assert!(near(window.pixel(Point::new(40.0, 10.0)), RED));
    assert!(near(window.pixel(Point::new(40.0, 40.0)), BLUE));
    let (more, scrolled) = window.frame_messages(list(), &[], up + 2000 * MS);
    assert!(!more && scrolled.is_empty());
}

#[test]
fn a_new_swipe_on_the_view_or_a_wheel_that_scrolls_it_stops_a_fling() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 50.0));
    window.frame(list(), &[], t0);
    let mut pad = Trackpad::new();
    let up = swipe_up(&mut window, &mut pad, list, t0) + 5 * MS;
    send(&mut window, list(), &[pad.ended(up)], up);
    let simulation = ClampingScrollSimulation::new(40.0, 1000.0);
    for k in 1..=3 {
        window.frame(list(), &[], up + 16 * k * MS);
    }
    let held = simulation.x(0.048) as f32;
    // Two fingers land: the view stays where the last frame left it.
    let at = up + 56 * MS;
    assert_eq!(send(&mut window, list(), &[pad.began(at)], at), None);
    let (more, scrolled) = window.frame_messages(list(), &[], at + 16 * MS);
    assert!(!more && scrolled.is_empty(), "stopped");
    // The new swipe drags it from there; slow and short, it flings not.
    let at = at + 30 * MS;
    let moved = send(&mut window, list(), &[pad.changed(0.0, -5.0, at)], at);
    assert!(moved.is_some_and(|y| (y - (held + 5.0)).abs() < 1e-3));
    send(&mut window, list(), &[pad.ended(at + 5 * MS)], at + 5 * MS);
    let (more, scrolled) = window.frame_messages(list(), &[], at + 21 * MS);
    assert!(!more && scrolled.is_empty(), "at rest");

    // Flung again, a wheel's signal moves it by its delta at once from
    // where the last frame left it, and the fling is over.
    let t1 = at + 500 * MS;
    let from = held + 5.0;
    let up = swipe_up(&mut window, &mut pad, list, t1) + 5 * MS;
    send(&mut window, list(), &[pad.ended(up)], up);
    window.frame(list(), &[], up + 16 * MS);
    let simulation = ClampingScrollSimulation::new(f64::from(from + 40.0), 1000.0);
    let there = simulation.x(0.016) as f32;
    let at = up + 20 * MS;
    let moved = send(&mut window, list(), &[pad.pixels(0.0, -10.0, at)], at);
    assert!(moved.is_some_and(|y| (y - (there + 10.0)).abs() < 1e-3));
    let (more, scrolled) = window.frame_messages(list(), &[], at + 16 * MS);
    assert!(!more && scrolled.is_empty(), "stopped");
}

#[test]
fn a_wheel_that_would_not_move_the_view_leaves_a_fling_alone() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 50.0));
    window.frame(list(), &[], t0);
    let mut pad = Trackpad::new();
    let up = swipe_up(&mut window, &mut pad, list, t0) + 5 * MS;
    send(&mut window, list(), &[pad.ended(up)], up);
    let simulation = ClampingScrollSimulation::new(40.0, 1000.0);
    // A wheel turned across: no scroll down, and the fling flies on.
    let at = up + 16 * MS;
    let moved = send(&mut window, list(), &[pad.pixels(-10.0, 0.0, at)], at);
    let expected = simulation.x(0.016) as f32;
    assert!(moved.is_some_and(|y| (y - expected).abs() < 1e-3));
}

#[test]
fn a_drag_and_a_fling_stop_at_the_ends_and_a_drag_against_one_shows_the_bar() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 50.0));
    window.frame(view(300.0), &[], t0);
    let mut pad = Trackpad::new();
    // At the top, the fingers pull down: nothing moves, but the bar comes
    // in (the overscroll's notification).
    send(&mut window, view(300.0), &[pad.began(t0)], t0);
    let at = t0 + 10 * MS;
    let moved = send(&mut window, view(300.0), &[pad.changed(0.0, 10.0, at)], at);
    assert_eq!(moved, None);
    window.frame(view(300.0), &[], at + 300 * MS);
    assert!(
        near(window.pixel(Point::new(94.0, 20.0)), thumb(0.30)),
        "shown"
    );
    // Up past the end: it stops there.
    let at = at + 316 * MS;
    let moved = send(
        &mut window,
        view(300.0),
        &[pad.changed(0.0, -150.0, at)],
        at,
    );
    assert_eq!(moved, Some(150.0));
    let at = at + 10 * MS;
    let moved = send(
        &mut window,
        view(300.0),
        &[pad.changed(0.0, -100.0, at)],
        at,
    );
    assert_eq!(moved, Some(200.0));
    // Lifted fast toward the end it is at: no fling.
    let up = at + 5 * MS;
    assert_eq!(send(&mut window, view(300.0), &[pad.ended(up)], up), None);
    let (_, scrolled) = window.frame_messages(view(300.0), &[], up + 16 * MS);
    assert!(scrolled.is_empty());
    // Flung back toward the start at 3000 px/s from 80, it flies to the
    // top and stops there.
    let t1 = up + 100 * MS;
    send(&mut window, view(300.0), &[pad.began(t1)], t1);
    let mut at = t1;
    for _ in 0..4 {
        at += 10 * MS;
        send(&mut window, view(300.0), &[pad.changed(0.0, 30.0, at)], at);
    }
    let up = at + 5 * MS;
    send(&mut window, view(300.0), &[pad.ended(up)], up);
    let simulation = ClampingScrollSimulation::new(80.0, -3000.0);
    let mut k = 0;
    let stop = loop {
        k += 1;
        let since = 16 * k * MS;
        let (_, scrolled) = window.frame_messages(view(300.0), &[], up + since);
        let y = offset(&scrolled).expect("it flies");
        let expected = simulation.x(since.as_secs_f64()) as f32;
        if expected <= 0.0 {
            assert_eq!(y, 0.0, "clamped at the top");
            break up + since;
        }
        assert!((y - expected).abs() < 1e-3, "{y} for {expected}");
    };
    let (_, scrolled) = window.frame_messages(view(300.0), &[], stop + 16 * MS);
    assert!(scrolled.is_empty(), "stopped at the end");
}

#[test]
fn a_wheel_scrolls_the_innermost_view_it_would_move_40_px_a_line() {
    let view = || nested(400.0);
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 30.0));
    window.frame(view(), &[], t0);
    let (_, scrolled) = window.frame_messages(view(), &[lines(1.0)], t0 + 16 * MS);
    assert_eq!(scrolled, [Scrolled::Inner(40.0)]);
    let (_, scrolled) = window.frame_messages(view(), &[wheel(500.0)], t0 + 32 * MS);
    assert_eq!(scrolled, [Scrolled::Inner(340.0)], "to its end");
    // At its end the inner one would not move: the outer's.
    let (_, scrolled) = window.frame_messages(view(), &[wheel(25.5)], t0 + 48 * MS);
    assert_eq!(scrolled, [Scrolled::Outer(25.5)]);
    // Back up: the inner's again.
    window.point_at(Point::new(40.0, 20.0));
    let (_, scrolled) = window.frame_messages(view(), &[lines(-1.0)], t0 + 64 * MS);
    assert_eq!(scrolled, [Scrolled::Inner(300.0)]);
    // Under Shift a vertical view takes the wheel's other axis: none.
    let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(
        keyboard::Modifiers::SHIFT,
    ));
    let turned = Event::Mouse(mouse::Event::WheelScrolled {
        // As the platform hands it over: macOS turns it across.
        delta: if cfg!(target_os = "macos") {
            mouse::ScrollDelta::Pixels { x: -10.0, y: 0.0 }
        } else {
            mouse::ScrollDelta::Pixels { x: 0.0, y: -10.0 }
        },
    });
    let (_, scrolled) = window.frame_messages(view(), &[shift, turned], t0 + 80 * MS);
    assert_eq!(scrolled, []);
}

/// An outer view whose content is, at its top, a 60 px view of `inner` px,
/// over 300 px of its own.
fn nested<'a>(inner: f32) -> Element<'a, Scrolled> {
    fading_scrollbar(
        scrollable(column![
            scrollable(column![Space::new().width(100).height(inner)])
                .width(Length::Fill)
                .height(60)
                .on_scroll(|v| Scrolled::Inner(v.absolute_offset().y)),
            Space::new().height(300),
        ])
        .width(100)
        .height(100)
        .on_scroll(|v| Scrolled::Outer(v.absolute_offset().y)),
    )
}

#[test]
fn a_swipe_in_a_nested_view_is_the_inner_ones_on_a_tie_and_the_outers_elsewhere() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 30.0));
    window.frame(nested(400.0), &[], t0);
    let mut pad = Trackpad::new();
    // Both drags in the arena: nothing moves until one is past 18 px;
    // both are on the same move, and the inner view's, handed it first,
    // wins.
    set_event_time(Some(t0));
    let (_, scrolled) = window.frame_sent(nested(400.0), &[pad.began(t0)], t0);
    assert_eq!(scrolled, []);
    let (_, scrolled) = window.frame_sent(nested(400.0), &[pad.changed(0.0, -12.0, t0)], t0);
    assert_eq!(scrolled, [], "within the slop");
    let (_, scrolled) = window.frame_sent(nested(400.0), &[pad.changed(0.0, -12.0, t0)], t0);
    assert_eq!(scrolled, [], "won: the drag starts here");
    let (_, scrolled) = window.frame_sent(nested(400.0), &[pad.changed(0.0, -5.0, t0)], t0);
    assert_eq!(scrolled, [Scrolled::Inner(5.0)]);
    window.frame_sent(nested(400.0), &[pad.ended(t0)], t0);
    // Below the inner view, the outer's alone: it moves at once.
    window.point_at(Point::new(40.0, 80.0));
    let t1 = t0 + 100 * MS;
    set_event_time(Some(t1));
    window.frame_sent(nested(400.0), &[pad.began(t1)], t1);
    let (_, scrolled) = window.frame_sent(nested(400.0), &[pad.changed(0.0, -3.0, t1)], t1);
    set_event_time(None);
    assert_eq!(scrolled, [Scrolled::Outer(3.0)]);
}

#[test]
fn a_view_with_nothing_to_scroll_takes_no_swipe() {
    // The inner view's content fits: its drag is not there
    // (`setCanDrag(false)`), and the outer's is alone.
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 30.0));
    window.frame(nested(50.0), &[], t0);
    let mut pad = Trackpad::new();
    set_event_time(Some(t0));
    window.frame_sent(nested(50.0), &[pad.began(t0)], t0);
    let (_, scrolled) = window.frame_sent(nested(50.0), &[pad.changed(0.0, -4.0, t0)], t0);
    set_event_time(None);
    assert_eq!(scrolled, [Scrolled::Outer(4.0)]);
}

#[test]
fn a_swipe_on_a_field_whose_text_fits_is_the_views_from_its_first_move() {
    use crate::ui::core::widgets::text_field::text_input;

    // The field's own drag is there only while its text can scroll
    // (`setCanDrag`): here the view's is alone in the arena.
    let view = || -> Element<'_, f32> {
        fading_scrollbar(
            scrollable(column![
                text_input("", "short")
                    .on_input(|_| -1.0)
                    .size(14)
                    .line_height(1.0)
                    .padding(0),
                Space::new().height(300),
            ])
            .width(100)
            .height(100)
            .on_scroll(|viewport| viewport.absolute_offset().y),
        )
    };
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 7.0));
    window.frame(view(), &[], t0);
    let mut pad = Trackpad::new();
    send(&mut window, view(), &[pad.began(t0)], t0);
    let at = t0 + 10 * MS;
    let moved = send(&mut window, view(), &[pad.changed(-6.0, -4.0, at)], at);
    assert_eq!(moved, Some(4.0));
}

#[test]
fn a_swipe_on_a_multi_line_field_is_its_own_only_while_its_text_overflows() {
    use crate::ui::core::widgets::text_field::editor::{text_editor, Content};

    /// A 40 px field at the top of the view.
    fn view(content: &Content) -> Element<'_, f32> {
        fading_scrollbar(
            scrollable(column![
                text_editor(content)
                    .read_only(true)
                    .size(14)
                    .line_height(1.0)
                    .padding(0)
                    .height(40),
                Space::new().height(300),
            ])
            .width(100)
            .height(100)
            .on_scroll(|viewport| viewport.absolute_offset().y),
        )
    }
    let long = Content::with_text(&"line\n".repeat(20));
    let short = Content::with_text("one line");
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(40.0, 20.0));
    window.frame(view(&long), &[], t0);
    let mut pad = Trackpad::new();
    // Its text overflows: its drag and the view's race, and the field's,
    // handed the moves first, wins the tie; the view stays.
    send(&mut window, view(&long), &[pad.began(t0)], t0);
    for _ in 0..4 {
        let moved = send(&mut window, view(&long), &[pad.changed(0.0, -12.0, t0)], t0);
        assert_eq!(moved, None);
    }
    send(&mut window, view(&long), &[pad.ended(t0)], t0);
    // Its text fits: the view's drag is alone, from the first move.
    window.frame(view(&short), &[], t0 + 100 * MS);
    let t1 = t0 + 200 * MS;
    send(&mut window, view(&short), &[pad.began(t1)], t1);
    let moved = send(&mut window, view(&short), &[pad.changed(0.0, -4.0, t1)], t1);
    assert_eq!(moved, Some(4.0));
}

#[test]
fn a_horizontal_view_drags_flings_and_takes_the_wheel_across() {
    let mut window = window();
    let t0 = Instant::now();
    window.point_at(Point::new(50.0, 40.0));
    window.frame(strip(), &[], t0);
    let mut pad = Trackpad::new();
    send(&mut window, strip(), &[pad.began(t0)], t0);
    // Left with the fingers, from the first move; down, nothing.
    let at = t0 + 10 * MS;
    let moved = send(&mut window, strip(), &[pad.changed(-14.0, 0.0, at)], at);
    assert_eq!(moved, Some(14.0));
    assert!(near(window.pixel(Point::new(30.0, 40.0)), RED));
    assert!(near(window.pixel(Point::new(40.0, 40.0)), BLUE));
    let at = at + 10 * MS;
    let moved = send(&mut window, strip(), &[pad.changed(0.0, -30.0, at)], at);
    assert_eq!(moved, None);
    // A fast swipe across, lifted: it flies on, at 2000 px/s from 74.
    let mut at = at;
    for _ in 0..3 {
        at += 10 * MS;
        send(&mut window, strip(), &[pad.changed(-20.0, 0.0, at)], at);
    }
    let up = at + 5 * MS;
    send(&mut window, strip(), &[pad.ended(up)], up);
    let (_, scrolled) = window.frame_messages(strip(), &[], up + 16 * MS);
    let expected = ClampingScrollSimulation::new(74.0, 2000.0).x(0.016) as f32;
    assert!(offset(&scrolled).is_some_and(|x| (x - expected).abs() < 1e-3));
    for k in 2..=60 {
        window.frame(strip(), &[], up + 16 * k * MS);
    }
    // At rest, its momentum moves nothing, nor does a wheel turned down...
    let at = up + 1000 * MS;
    let sent = [
        pad.momentum(Phase::Changed, -40.0, 0.0, at),
        pad.lines(0.0, -1.0, at),
    ];
    assert_eq!(send(&mut window, strip(), &sent, at), None);
    // ...but one turned across does, 40 px a line, and one turned down
    // under Shift.
    let before = {
        let mut read = Read {
            axis: Axis::Horizontal,
            offset: None,
        };
        window.operate(strip(), &mut read);
        read.offset.expect("a scrollable")
    };
    let at = at + 100 * MS;
    let moved = send(&mut window, strip(), &[pad.lines(-1.0, 0.0, at)], at);
    assert_eq!(moved.map(f32::round), Some(before + 40.0));
    let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(
        keyboard::Modifiers::SHIFT,
    ));
    let turned = Event::Mouse(mouse::Event::WheelScrolled {
        delta: if cfg!(target_os = "macos") {
            mouse::ScrollDelta::Pixels { x: -10.0, y: 0.0 }
        } else {
            mouse::ScrollDelta::Pixels { x: 0.0, y: -10.0 }
        },
    });
    let (_, scrolled) = window.frame_messages(strip(), &[shift, turned], at + 16 * MS);
    assert_eq!(offset(&scrolled).map(f32::round), Some(before + 50.0));
}
