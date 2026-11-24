use super::*;

const MS: Duration = Duration::from_millis(1);
const HEIGHT: f32 = 100.0;

fn at(y: f32) -> Point {
    Point::new(10.0, y)
}

fn close(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() < by
}

/// A drag from y = 0 down to `to`, in `steps` moves `gap` apart, starting
/// at `t0`; the time of its last move.
fn drag(d: &mut Dismiss, t0: Instant, to: f32, steps: u32, gap: Duration) -> Instant {
    d.press(at(0.0), t0, HEIGHT);
    let mut t = t0;
    for i in 1..=steps {
        t = t0 + gap * i;
        d.moved(at(to * i as f32 / steps as f32), t, HEIGHT);
    }
    t
}

// ------------------------------------------------------------ velocity

#[test]
fn the_least_squares_slope_is_flutters_degree_2_fit() {
    // y = 2 + 3x + 0.5x²: the fit is exact, its slope at 0 is 3.
    let x = [-30.0, -20.0, -10.0, 0.0];
    let y: Vec<f64> = x.iter().map(|x| 2.0 + 3.0 * x + 0.5 * x * x).collect();
    let slope = least_squares_slope(&x, &y).expect("a fit");
    assert!(close(slope, 3.0, 1e-9), "{slope}");
    // Two samples at one time: degenerate.
    assert_eq!(
        least_squares_slope(&[0.0, 0.0, 0.0], &[1.0, 2.0, 3.0]),
        None
    );
}

#[test]
fn the_velocity_is_the_recent_samples_slope() {
    let mut v = VelocityTracker::default();
    let t0 = Instant::now();
    // 8 px every 8 ms: 1000 px/s.
    for i in 0..6u32 {
        v.add(t0 + 8 * MS * i, 8.0 * i as f32);
    }
    let last = t0 + 40 * MS;
    let e = v.estimate(last).expect("an estimate");
    assert!(close(e.velocity_y, 1000.0, 1e-6), "{e:?}");
    assert!(close(e.offset_y, 40.0, 1e-9));
    // Still for more than 40 ms by the time it is asked: no velocity.
    let e = v.estimate(last + 41 * MS).expect("an estimate");
    assert_eq!(e.velocity_y, 0.0);
    // Within 40 ms (whole milliseconds), still the motion's.
    let e = v.estimate(last + 40 * MS + MS / 2).expect("an estimate");
    assert!(close(e.velocity_y, 1000.0, 1e-6));
}

#[test]
fn the_velocity_reads_only_the_last_100_ms_and_no_older_pause() {
    let mut v = VelocityTracker::default();
    let t0 = Instant::now();
    // A fast stretch long ago, a pause of more than 40 ms, then a slow one:
    // only the slow stretch counts.
    v.add(t0, 0.0);
    v.add(t0 + 5 * MS, 50.0);
    v.add(t0 + 10 * MS, 100.0);
    for i in 0..4u32 {
        v.add(t0 + 60 * MS + 10 * MS * i, 100.0 + 2.0 * i as f32);
    }
    let e = v.estimate(t0 + 90 * MS).expect("an estimate");
    assert!(close(e.velocity_y, 200.0, 1e-6), "{e:?}");
    // Fewer than three samples: no velocity, but the offset.
    let mut v = VelocityTracker::default();
    v.add(t0, 0.0);
    v.add(t0 + 5 * MS, 10.0);
    let e = v.estimate(t0 + 5 * MS).expect("an estimate");
    assert_eq!((e.velocity_y, e.offset_y), (0.0, 10.0));
}

#[test]
fn a_mouse_drag_end_is_a_fling_past_50_px_s_and_one_pixel_clamped_to_8000() {
    let e = |velocity_y, offset_y| {
        Some(VelocityEstimate {
            velocity_y,
            offset_y,
        })
    };
    assert_eq!(fling_velocity(e(50.0, 10.0)), 0.0);
    assert_eq!(fling_velocity(e(51.0, 1.0)), 0.0, "the hit slop");
    assert_eq!(fling_velocity(e(51.0, 1.5)), 51.0);
    assert_eq!(fling_velocity(e(-9000.0, -30.0)), -8000.0);
    assert_eq!(fling_velocity(None), 0.0);
}

// ------------------------------------------------------------- dismiss

#[test]
fn it_follows_a_drag_down_and_never_up() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    d.press(at(50.0), t0, HEIGHT);
    d.moved(at(40.0), t0 + 10 * MS, HEIGHT);
    assert_eq!(d.offset(t0), 0.0, "not above where it started");
    d.moved(at(75.0), t0 + 20 * MS, HEIGHT);
    assert_eq!(d.offset(t0), 0.35, "down 35 px of 100");
    // Back up past the start: the move is dropped, the extent holds.
    d.moved(at(10.0), t0 + 30 * MS, HEIGHT);
    assert_eq!(d.offset(t0), 0.35);
}

#[test]
fn let_go_short_of_40_percent_it_slides_back_over_its_share_of_200_ms() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    // 30 px, slowly (well under a fling).
    let last = drag(&mut d, t0, 30.0, 3, 30 * MS);
    let up = last + 30 * MS;
    assert!(!d.release(up));
    assert!(d.is_animating(up));
    assert!(close(d.offset(up + 30 * MS) as f64, 0.15, 1e-6), "halfway");
    // 30 % of 200 ms.
    assert!(d.is_animating(up + 60 * MS));
    assert!(!d.is_animating(up + 61 * MS));
    assert_eq!(d.offset(up + 61 * MS), 0.0);
    assert!(!d.frame(up + 100 * MS), "never dismissed");
}

#[test]
fn let_go_past_40_percent_it_slides_out_and_reports_once() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    let last = drag(&mut d, t0, 60.0, 6, 30 * MS);
    let up = last + 30 * MS;
    assert!(!d.release(up));
    assert!(!d.frame(up + 79 * MS), "40 % of 200 ms to go");
    assert!(close(d.offset(up + 40 * MS) as f64, 0.8, 1e-6));
    assert!(d.frame(up + 81 * MS));
    assert!(!d.frame(up + 100 * MS), "once");
    assert_eq!(d.offset(up + 100 * MS), 1.0);
}

#[test]
fn a_downward_fling_flies_it_out_on_the_spring() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    // 20 px at 1000 px/s, let go at once.
    let last = drag(&mut d, t0, 20.0, 4, 5 * MS);
    assert!(!d.release(last));
    // Flutter's overdamped fling spring from 0.2 at 1000 / 300.
    assert!(close(d.offset(last + 50 * MS) as f64, 0.503_617, 1e-4));
    assert!(close(d.offset(last + 100 * MS) as f64, 0.765_476, 1e-4));
    assert!(!d.frame(last + 276 * MS));
    assert!(d.frame(last + 278 * MS), "done within 0.01 of 1.01");
}

#[test]
fn an_upward_fling_flies_it_back() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    d.press(at(0.0), t0, HEIGHT);
    d.moved(at(50.0), t0 + 100 * MS, HEIGHT);
    // Then up fast: the extent holds at 50 (a drag never goes above its
    // start, but these moves stay below it).
    for i in 1..=4u32 {
        d.moved(
            at(50.0 - 9.0 * i as f32),
            t0 + 100 * MS + 4 * MS * i,
            HEIGHT,
        );
    }
    let up = t0 + 116 * MS;
    assert!(close(d.offset(up) as f64, 0.14, 1e-6));
    assert!(!d.release(up));
    assert!(d.is_animating(up));
    let later = up + 300 * MS;
    assert_eq!(d.offset(later), 0.0);
    assert!(!d.is_animating(later));
    assert!(!d.frame(later));
}

#[test]
fn dragged_all_the_way_out_it_is_dismissed_on_release() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    let last = drag(&mut d, t0, 120.0, 4, 30 * MS);
    assert_eq!(d.offset(last), 1.0);
    assert!(!d.frame(last), "not while the drag holds it");
    assert!(d.release(last + 50 * MS));
    assert!(!d.frame(last + 60 * MS), "reported once");
}

#[test]
fn a_press_catches_it_mid_slide() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    let last = drag(&mut d, t0, 60.0, 6, 30 * MS);
    let up = last + 30 * MS;
    d.release(up);
    // 20 ms into the slide out it is at 0.7: caught there, and dragged on.
    let caught = up + 20 * MS;
    d.press(at(0.0), caught, HEIGHT);
    assert!(!d.is_animating(caught));
    assert!(close(d.offset(caught) as f64, 0.7, 1e-6));
    d.moved(at(-20.0), caught + 30 * MS, HEIGHT);
    assert!(close(d.offset(caught) as f64, 0.5, 1e-6), "dragged back up");
    // Let go slowly below 40 %... at 50 %: out again.
    d.release(caught + 100 * MS);
    assert!(d.frame(caught + 100 * MS + 101 * MS));
}

#[test]
fn a_click_does_nothing() {
    let mut d = Dismiss::default();
    let t0 = Instant::now();
    d.press(at(0.0), t0, HEIGHT);
    assert!(!d.release(t0 + 80 * MS));
    assert!(!d.is_animating(t0 + 80 * MS));
    assert_eq!(d.offset(t0 + 80 * MS), 0.0);
    assert!(!d.frame(t0 + 200 * MS));
}

#[test]
fn rounded_hits_leave_the_corners_out() {
    let rect = Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 40.0));
    assert!(in_rounded(rect, 8.0, Point::new(50.0, 0.5)));
    assert!(in_rounded(rect, 8.0, Point::new(8.0, 8.0)));
    assert!(!in_rounded(rect, 8.0, Point::new(1.0, 1.0)));
    assert!(!in_rounded(rect, 8.0, Point::new(99.0, 39.0)));
    assert!(!in_rounded(rect, 8.0, Point::new(101.0, 20.0)));
}
