//! The Vocal tract tab's tests: the pose's interpolation and its springs,
//! the 3D projection and the turn a drag makes, the view toggles, the
//! figure's fit at any well size, the callouts' layout, the readouts, the
//! demo and the seam, and the labels a screen reader reads.

use std::f32::consts::FRAC_PI_2;

use iced::Size;

use super::callouts::{self, Side};
use super::camera::{fitted_scale, Projection, P3, SCENE_CENTRE};
use super::demo::{articulate, period, AH, EE, OO};
use super::draw2d::{Fit2d, FRAME_MAX, FRAME_MIN};
use super::geometry::{length, resample};
use super::pose::Cm;
use super::readouts::{self, Status};
use super::spring::{PoseSpring, POSE_SETTLE};
use super::*;
use crate::ui::song_practice_detail::widgets::live_ui::{self, LiveUi};

const MS: Duration = Duration::from_millis(1);

fn close(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

fn ah() -> TractPose {
    articulate(&AH, true, Fit::Good)
}

fn ee() -> TractPose {
    articulate(&EE, true, Fit::Good)
}

/// A state fed the demo from `from` to `to` s at 60 per second, settled.
fn fed(from: f64, to: f64) -> VocalTractState {
    let mut st = VocalTractState::new();
    let t0 = Instant::now();
    let n = ((to - from) * 60.0).round() as u64;
    for i in 0..=n {
        let c = from + i as f64 / 60.0;
        st.feed(Some(TractPose::demo(c)), c, t0 + MS * (i * 16) as u32);
    }
    let mut at = t0 + MS * (n * 16) as u32;
    for _ in 0..20 {
        at += MS * 250;
        st.update(Message::Frame(at), at);
    }
    st
}

// ------------------------------------------------------------ the pose

#[test]
fn a_pose_lerps_every_number_part_way() {
    let (a, b) = (ah(), ee());
    let half = TractPose::lerp(&a, &b, 0.5);
    let k = 30;
    let mid = a.outlines.tongue[k].lerp(b.outlines.tongue[k], 0.5);
    assert!(close(half.outlines.tongue[k].x, mid.x, 1e-4));
    assert!(close(half.outlines.tongue[k].y, mid.y, 1e-4));
    assert!(close(
        half.formants[1],
        (a.formants[1] + b.formants[1]) / 2.0,
        1e-2
    ));
    assert!(close(
        half.jaw_angle,
        (a.jaw_angle + b.jaw_angle) / 2.0,
        1e-6
    ));
    assert_eq!(TractPose::lerp(&a, &b, 0.0), a);
    let end = TractPose::lerp(&a, &b, 1.0).values();
    assert!(end.iter().zip(b.values()).all(|(x, y)| close(*x, y, 1e-3)));
}

#[test]
fn a_pose_of_another_layout_is_resampled_along_its_outlines() {
    let a = ah();
    let mut b = ee();
    b.outlines.tongue = resample(&b.outlines.tongue, 40, true);
    let from = TractPose::lerp(&a, &b, 0.0);
    assert_eq!(from.outlines.tongue.len(), 40);
    // Still a's tongue: every point on its outline, its length kept.
    let on = &a.outlines.tongue;
    for p in &from.outlines.tongue {
        let d = (0..on.len())
            .map(|i| segment_distance(*p, on[i], on[(i + 1) % on.len()]))
            .fold(f32::INFINITY, f32::min);
        assert!(d < 1e-3, "{p:?} is {d} cm off a's tongue");
    }
    let (la, lb) = (
        length(&a.outlines.tongue, true),
        length(&from.outlines.tongue, true),
    );
    assert!(close(la, lb, la * 0.03), "{la} vs {lb}");
}

/// How far `p` is from the segment `a`–`b`.
fn segment_distance(p: Cm, a: Cm, b: Cm) -> f32 {
    let ab = b.minus(a);
    let len2 = ab.x * ab.x + ab.y * ab.y;
    if len2 <= 0.0 {
        return p.dist(a);
    }
    let t = ((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2;
    p.dist(a.lerp(b, t.clamp(0.0, 1.0)))
}

#[test]
fn a_pose_round_trips_through_its_numbers() {
    let a = ah();
    assert_eq!(a.with_values(&a.values()), a);
}

// ------------------------------------------------------------- motion

#[test]
fn the_figure_follows_an_estimate_in_about_seventy_ms_without_overshoot() {
    let mut spring = PoseSpring::default();
    let t0 = Instant::now();
    spring.push(ah(), t0);
    spring.push(ee(), t0);
    let (from, to) = (ah().formants[1], ee().formants[1]);
    let mut last = from;
    for step in 1..=7 {
        spring.advance(t0 + MS * (10 * step));
        let f2 = spring.shown().expect("a pose").formants[1];
        assert!(f2 >= last - 1e-3 && f2 <= to + 1e-3, "no overshoot: {f2}");
        last = f2;
    }
    let covered = (last - from) / (to - from);
    assert!(
        (0.85..=0.95).contains(&covered),
        "{covered} of the way after {POSE_SETTLE:?}"
    );
    assert!(spring.animating());
    for step in 1..=10 {
        spring.advance(t0 + MS * (70 + 100 * step));
    }
    assert_eq!(spring.shown().expect("a pose").values(), ee().values());
    assert!(!spring.animating(), "settled: no more frames");
}

#[test]
fn a_new_estimate_mid_move_keeps_the_motion_going() {
    let mut spring = PoseSpring::default();
    let t0 = Instant::now();
    spring.push(ah(), t0);
    spring.push(ee(), t0);
    spring.advance(t0 + MS * 30);
    let before = spring.shown().expect("a pose").formants[1];
    // The same target again: no jump, no restart from rest.
    spring.push(ee(), t0 + MS * 30);
    assert!(close(
        spring.shown().expect("a pose").formants[1],
        before,
        1e-3
    ));
    spring.advance(t0 + MS * 40);
    assert!(spring.shown().expect("a pose").formants[1] > before);
}

#[test]
fn an_unvoiced_estimate_holds_the_last_shape_and_dims_it() {
    let mut spring = PoseSpring::default();
    let t0 = Instant::now();
    spring.push(ah(), t0);
    assert_eq!(spring.rest(), 0.0);
    spring.push(articulate(&EE, false, Fit::Uncertain), t0 + MS * 10);
    for step in 1..=20 {
        spring.advance(t0 + MS * (10 + 50 * step));
    }
    let shown = spring.shown().expect("a pose");
    assert_eq!(shown.outlines.tongue, ah().outlines.tongue, "held");
    assert!(!shown.voiced);
    assert!(spring.rest() > 0.99, "dimmed: {}", spring.rest());
    assert!(!spring.animating());
}

#[test]
fn before_any_voice_the_figure_follows_the_resting_estimates() {
    // The tab opened before the engine had anything (the placeholder), then
    // a pause in the take: the figure takes the engine's resting shape.
    let mut spring = PoseSpring::default();
    let t0 = Instant::now();
    spring.push(articulate(&AH, false, Fit::Uncertain), t0);
    spring.push(articulate(&OO, false, Fit::Good), t0 + MS * 10);
    for step in 1..=20 {
        spring.advance(t0 + MS * (10 + 50 * step));
    }
    let oo = articulate(&OO, false, Fit::Good);
    assert_eq!(
        spring.shown().expect("a pose").outlines.tongue,
        oo.outlines.tongue
    );
    // Once voiced, a pause holds that shape.
    spring.push(articulate(&EE, true, Fit::Good), t0 + MS * 1100);
    for step in 1..=20 {
        spring.advance(t0 + MS * (1100 + 50 * step));
    }
    spring.push(articulate(&AH, false, Fit::Good), t0 + MS * 2200);
    for step in 1..=20 {
        spring.advance(t0 + MS * (2200 + 50 * step));
    }
    let ee = articulate(&EE, true, Fit::Good);
    assert_eq!(
        spring.shown().expect("a pose").outlines.tongue,
        ee.outlines.tongue
    );
}

#[test]
fn the_pitch_reads_the_sung_note_at_once_not_a_glide_from_silence() {
    let mut spring = PoseSpring::default();
    let t0 = Instant::now();
    spring.push(articulate(&AH, false, Fit::Good), t0);
    spring.push(ah(), t0 + MS * 5);
    spring.advance(t0 + MS * 10);
    let shown = spring.shown().expect("a pose");
    assert!(spring.animating(), "the shape still moves");
    assert_eq!(shown.components.f0_hz, ah().components.f0_hz);
}

#[test]
fn a_parked_playhead_fed_again_and_again_keeps_one_sample() {
    let mut st = VocalTractState::new();
    let t0 = Instant::now();
    for i in 0..1000u32 {
        st.feed(Some(ah()), 4.0, t0 + MS * i);
    }
    assert_eq!(st.history.len(), 1);
    assert_eq!(st.trail().len(), 1);
}

#[test]
fn a_formant_missing_from_a_voiced_frame_keeps_the_last_one_known() {
    let mut st = VocalTractState::new();
    let t0 = Instant::now();
    st.feed(Some(ah()), 1.0, t0);
    let known = ah().formants;
    let mut lost = ah();
    lost.formants = [known[0], 0.0, 0.0];
    st.feed(Some(lost), 1.1, t0 + MS * 100);
    for step in 1..=20 {
        st.update(
            Message::Frame(t0 + MS * (100 + 50 * step)),
            t0 + MS * (100 + 50 * step),
        );
    }
    assert_eq!(st.shown().formants, known);
    assert!(st.trail().iter().all(|&(f1, f2)| f1 > 0.0 && f2 > 0.0));
    let r = st.readouts();
    assert_eq!(r.vowel, Some((known[0], known[1])));
}

/// Live (or take A) sung, then take B selected: a formant B's frame misses
/// is not A's, nor one from before a seek in B.
#[test]
fn a_formant_is_carried_only_within_one_voice_and_not_across_a_seek() {
    let settle = |st: &mut VocalTractState, from: Instant| {
        for step in 1..=20 {
            let at = from + MS * (50 * step);
            st.update(Message::Frame(at), at);
        }
    };
    let mut missing_f2 = ah();
    missing_f2.formants = [640.0, 0.0, 0.0];
    for voice in [Voice::Live, Voice::Take("a".into())] {
        let mut st = VocalTractState::new();
        let t0 = Instant::now();
        st.set_voice(voice);
        st.feed(Some(ee()), 1.0, t0);
        st.set_voice(Voice::Take("b".into()));
        st.feed(Some(missing_f2.clone()), 1.05, t0 + MS * 50);
        settle(&mut st, t0 + MS * 50);
        assert_eq!(st.readouts().vowel, None, "no dot at A's vowel");
        assert_eq!(st.shown().formants[1], 0.0, "a dash, not A's F2");
    }
    // Within one take, a seek: what was found before it is not carried.
    let mut st = VocalTractState::new();
    let t0 = Instant::now();
    st.set_voice(Voice::Take("b".into()));
    st.feed(Some(ee()), 1.0, t0);
    st.feed(Some(missing_f2.clone()), 30.0, t0 + MS * 50);
    settle(&mut st, t0 + MS * 50);
    assert_eq!(st.readouts().vowel, None);
    // And without one it still is.
    st.feed(Some(ee()), 30.02, t0 + MS * 1200);
    st.feed(Some(missing_f2), 30.05, t0 + MS * 1250);
    settle(&mut st, t0 + MS * 1250);
    assert_eq!(st.shown().formants[1], ee().formants[1]);
}

#[test]
fn a_formant_never_found_reads_as_a_dash_and_puts_no_dot_on_the_map() {
    let mut st = VocalTractState::new();
    let mut first = ah();
    first.formants = [640.0, 0.0, 0.0];
    st.feed(Some(first), 1.0, Instant::now());
    let r = st.readouts();
    assert_eq!(r.vowel, None);
    assert!(st.trail().is_empty());
    assert_eq!(
        readouts::formants_line([640.0, 0.0, 0.0]),
        "F1 640 \u{b7} F2 \u{2014} \u{b7} F3 \u{2014} Hz"
    );
}

// --------------------------------------------------------- projection

#[test]
fn the_side_view_draws_the_midsagittal_plane_at_one_scale() {
    let size = Size::new(900.0, 300.0);
    let p = Projection::new(
        Camera {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
        },
        size,
    );
    let s = fitted_scale(size);
    for (x, y) in [(0.0, 0.0), (7.9, 7.1), (-1.2, 9.0)] {
        let (at, depth) = p.project(P3::new(x, y, 0.0));
        assert!(close(at.x, 450.0 + (x - SCENE_CENTRE.x) * s, 1e-3));
        assert!(close(at.y, 150.0 - (y - SCENE_CENTRE.y) * s, 1e-3));
        assert!(close(depth, 0.0, 1e-5));
    }
}

#[test]
fn the_front_view_faces_the_viewer() {
    let p = Projection::new(
        Camera {
            yaw: FRAC_PI_2,
            pitch: 0.0,
            zoom: 1.0,
        },
        Size::new(600.0, 300.0),
    );
    let lips = p.project(P3::new(8.0, 7.0, 0.0));
    let back = p.project(P3::new(-6.0, 7.0, 0.0));
    assert!(lips.1 > back.1, "the face is nearer the viewer");
    assert!(close(lips.0.x, back.0.x, 1e-3), "front and back line up");
    // The subject's right side shows on the viewer's left.
    let right = p.point(P3::new(SCENE_CENTRE.x, 7.0, 5.0));
    assert!(right.x < 300.0);
}

#[test]
fn the_projection_turns_without_stretching() {
    for (yaw, pitch) in [(0.3, 0.2), (0.62, 0.3), (2.0, -0.7)] {
        let p = Projection::new(
            Camera {
                yaw,
                pitch,
                zoom: 1.0,
            },
            Size::new(800.0, 300.0),
        );
        let (a, b) = (P3::new(1.0, 2.0, 3.0), P3::new(-2.0, 5.0, 0.5));
        let (va, vb) = (p.view(a), p.view(b));
        let turned = ((va.x - vb.x).powi(2) + (va.y - vb.y).powi(2) + (va.z - vb.z).powi(2)).sqrt();
        let true_len = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt();
        assert!(close(turned, true_len, 1e-4), "{turned} vs {true_len}");
    }
}

#[test]
fn a_drag_turns_the_head_and_a_scroll_zooms_within_limits() {
    let c = Camera::preset(Preset::Side);
    let turned = c.turned(100.0, 50.0);
    assert!(close(turned.yaw, c.yaw + 1.0, 1e-5));
    assert!(close(turned.pitch, c.pitch + 0.5, 1e-5));
    assert!(close(
        c.turned(0.0, 10_000.0).pitch,
        camera::PITCH_LIMIT,
        1e-6
    ));
    assert!(close(c.zoomed(100.0).zoom, camera::ZOOM_MAX, 1e-6));
    assert!(close(c.zoomed(-100.0).zoom, camera::ZOOM_MIN, 1e-6));
    assert!(c.zoomed(1.0).zoom > 1.0);
}

fn view_of(st: &VocalTractState, live: bool) -> Element<'_, Message> {
    vocal_tract_view(VocalTract {
        state: st,
        live,
        source: if live { Source::Live } else { Source::NoTake },
    })
}

#[test]
fn dragging_the_3d_view_turns_the_head() {
    let mut st = fed(1.0, 2.0);
    st.update(Message::SetView(TractView::ThreeD), Instant::now());
    let yaw = st.camera().yaw;
    let mut ui = LiveUi::new(Size::new(1400.0, 340.0));
    let messages = ui.run(
        view_of(&st, true),
        [
            live_ui::move_to(400.0, 200.0),
            live_ui::press(),
            live_ui::move_to(440.0, 210.0),
            live_ui::move_to(470.0, 210.0),
            live_ui::release(),
            live_ui::move_to(600.0, 210.0),
        ],
    );
    assert_eq!(
        messages,
        [
            Message::Turn { dx: 40.0, dy: 10.0 },
            Message::Turn { dx: 30.0, dy: 0.0 }
        ]
    );
    for m in messages {
        st.update(m, Instant::now());
    }
    assert!(close(st.camera().yaw, yaw + 0.7, 1e-5));
    assert_eq!(st.preset(), None, "turned by hand");
    // A scroll over it zooms.
    let messages = ui.run(view_of(&st, true), [live_ui::scroll(0.0, 80.0)]);
    assert_eq!(messages, [Message::Zoom(2.0)]);
}

#[test]
fn a_preset_turns_the_head_there_smoothly() {
    let mut st = VocalTractState::new();
    assert_eq!(st.preset(), Some(Preset::ThreeQuarter));
    let t0 = Instant::now();
    st.update(Message::Preset(Preset::Front), t0);
    assert_eq!(st.preset(), Some(Preset::Front), "heading there");
    st.update(Message::Frame(t0 + MS * 100), t0 + MS * 100);
    let mid = st.camera().yaw;
    assert!(mid > 0.62 && mid < FRAC_PI_2, "on its way: {mid}");
    assert!(st.animating());
    let mut at = t0 + MS * 100;
    for _ in 0..20 {
        at += MS * 100;
        st.update(Message::Frame(at), at);
    }
    assert!(close(st.camera().yaw, FRAC_PI_2, 1e-3));
    assert_eq!(st.camera().matching(), Some(Preset::Front));
    assert!(!st.animating());
}

// ------------------------------------------------------------ toggles

#[test]
fn the_view_toggle_switches_between_2d_and_3d() {
    let mut st = fed(1.0, 2.0);
    let mut ui = iced_test::simulator(view_of(&st, true));
    assert!(ui.find("Moves as you sing").is_ok(), "the 2D legend");
    assert!(ui.find("Drag to turn \u{b7} scroll to zoom").is_err());
    ui.click("3D").expect("the 3D segment");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [Message::SetView(TractView::ThreeD)]);
    st.update(messages[0].clone(), Instant::now());
    assert_eq!(st.view(), TractView::ThreeD);
    let mut ui = iced_test::simulator(view_of(&st, true));
    for shown in [
        "Drag to turn \u{b7} scroll to zoom",
        "Side",
        "\u{be}",
        "Front",
    ] {
        assert!(ui.find(shown).is_ok(), "{shown}");
    }
    assert!(ui.find("Moves as you sing").is_err());
    ui.click("Side").expect("the Side preset");
    ui.click("2D").expect("the 2D segment");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(
        messages,
        [
            Message::Preset(Preset::Side),
            Message::SetView(TractView::TwoD)
        ]
    );
}

#[test]
fn the_source_toggle_asks_for_live_or_the_take() {
    let st = fed(1.0, 2.0);
    let mut ui = iced_test::simulator(view_of(&st, false));
    assert!(ui.find("No take selected").is_ok());
    ui.click("Live").expect("the Live segment");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [Message::SelectLive(true)]);
    let mut ui = iced_test::simulator(view_of(&st, true));
    assert!(ui.find("Live from your microphone").is_ok());
    ui.click("Take").expect("the Take segment");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(messages, [Message::SelectLive(false)]);
}

// ------------------------------------------------------------- layout

#[test]
fn the_figure_keeps_its_aspect_at_any_well_size() {
    for size in [
        Size::new(988.0, 291.0),
        Size::new(828.0, 244.0),
        Size::new(600.0, 420.0),
        Size::new(1800.0, 160.0),
        Size::new(320.0, 320.0),
    ] {
        let fit = Fit2d::new(size);
        // A centimetre is as wide as it is tall.
        let o = fit.px(Cm::new(0.0, 0.0));
        let x = fit.px(Cm::new(1.0, 0.0));
        let y = fit.px(Cm::new(0.0, 1.0));
        assert!(close(x.x - o.x, o.y - y.y, 1e-4), "{size:?}");
        // The whole frame inside the well, touching two opposite sides,
        // centred.
        let (lo, hi) = (
            fit.px(Cm::new(FRAME_MIN.x, FRAME_MAX.y)),
            fit.px(Cm::new(FRAME_MAX.x, FRAME_MIN.y)),
        );
        assert!(
            lo.x >= -0.01
                && lo.y >= -0.01
                && hi.x <= size.width + 0.01
                && hi.y <= size.height + 0.01
        );
        let fills_width = close(hi.x - lo.x, size.width, 0.01);
        let fills_height = close(hi.y - lo.y, size.height, 0.01);
        assert!(fills_width || fills_height, "{size:?}");
        assert!(close(lo.x + hi.x, size.width, 0.01) && close(lo.y + hi.y, size.height, 0.01));
        // The 3D view: one scale too, the scene inside.
        let s = fitted_scale(size);
        assert!(2.0 * camera::SCENE_HALF_WIDTH * s <= size.width + 0.01);
        assert!(2.0 * camera::SCENE_HALF_HEIGHT * s <= size.height + 0.01);
    }
}

#[test]
fn the_callouts_never_overlap_and_keep_their_order() {
    let order = |side| -> Vec<&'static str> {
        callouts::anchors(&ah())
            .into_iter()
            .filter(|a| a.side == side)
            .map(|a| a.text)
            .collect()
    };
    assert_eq!(
        order(Side::Right),
        [
            "Nasal cavity",
            "Hard palate",
            "Lips",
            "Teeth",
            "Tongue",
            "Jaw",
            "Larynx"
        ]
    );
    assert_eq!(
        order(Side::Left),
        ["Velum", "Pharynx", "Spine", "Epiglottis", "Vocal folds"]
    );
    let steps = (period() / 0.25) as usize;
    for size in [
        Size::new(988.0, 291.0),
        Size::new(828.0, 244.0),
        Size::new(700.0, 180.0),
        Size::new(1400.0, 560.0),
    ] {
        let fit = Fit2d::new(size);
        for i in 0..steps {
            let pose = TractPose::demo(i as f64 * 0.25);
            let placed = callouts::place(&callouts::anchors(&pose), &fit, size);
            assert_eq!(placed.len(), 12);
            for side in [Side::Left, Side::Right] {
                let column: Vec<&callouts::Placed> =
                    placed.iter().filter(|c| c.side == side).collect();
                assert_eq!(
                    column.iter().map(|c| c.text).collect::<Vec<_>>(),
                    order(side),
                    "the order holds"
                );
                for pair in column.windows(2) {
                    let (a, b) = (pair[0].span(), pair[1].span());
                    assert!(
                        a.1 <= b.0,
                        "{} and {} overlap at {size:?}",
                        pair[0].text,
                        pair[1].text
                    );
                }
                for c in &column {
                    let (top, bottom) = c.span();
                    assert!(
                        top >= 0.0 && bottom <= size.height,
                        "{} in the well",
                        c.text
                    );
                }
            }
            // The columns stand apart, either side of the head.
            let left = placed
                .iter()
                .filter(|c| c.side == Side::Left)
                .map(|c| c.label.x)
                .fold(f32::MIN, f32::max);
            let right = placed
                .iter()
                .filter(|c| c.side == Side::Right)
                .map(|c| c.label.x)
                .fold(f32::MAX, f32::min);
            let head = fit.px(Cm::new(-9.7, 0.0)).x..fit.px(Cm::new(10.9, 0.0)).x;
            assert!(left < head.start && right > head.end);
        }
    }
}

#[test]
fn the_panel_fits_the_dock() {
    let full = readouts::metrics(360.0);
    assert_eq!((full.pad_top, full.row), (readouts::PAD_TOP, readouts::ROW));
    assert!(full.bottom >= readouts::BOTTOM && !full.scrolls);
    // The mockup's dock: the card still lines up with the well.
    let mockup = readouts::metrics(321.0);
    assert_eq!(mockup.pad_top, readouts::PAD_TOP);
    assert!(mockup.row >= readouts::ROW_TIGHT && !mockup.scrolls);
    // The default dock: tighter, still whole.
    let default = readouts::metrics(274.0);
    assert_eq!(default.pad_top, 0.0);
    assert!(default.row >= readouts::ROW_MIN && !default.scrolls);
    // Shorter: it scrolls.
    assert!(readouts::metrics(160.0).scrolls);
    for h in [160.0, 220.0, 274.0, 300.0, 321.0, 350.0, 500.0] {
        let m = readouts::metrics(h);
        let used = m.pad_top + 8.0 + 7.0 * m.row + 10.0 + m.bottom;
        assert!(m.scrolls || used <= h + 0.5, "{h}: {used}");
        assert!(m.row >= readouts::ROW_MIN && m.row <= readouts::ROW);
    }
}

// ------------------------------------------------------------ readouts

#[test]
fn a_marker_at_a_track_end_shows_whole_and_leaves_its_label_clear() {
    // At an end the knob was cut in half by the track's edge and covered
    // the end's label ("Back", "Closed"); then, in the tighter rows, the
    // knob and its glow still lay over the label's lower half.
    let w = 160.0;
    for row in [readouts::ROW, readouts::ROW_TIGHT, readouts::ROW_MIN] {
        let h = row - 1.0;
        for position in [0.0, 1.0] {
            let (knob, label_y, glow) = readouts::track_marks(w, h, position);
            assert!(knob.x - readouts::KNOB_RADIUS >= 0.0, "{knob:?}");
            assert!(knob.x + readouts::KNOB_RADIUS <= w, "{knob:?}");
            // The label's line, its descenders included, above the glow.
            assert!(
                label_y + readouts::END_LINE / 2.0 <= knob.y - glow,
                "row {row}: the label ({label_y}) runs into the glow ({glow}) at {knob:?}"
            );
            assert!(label_y - readouts::END_LINE / 2.0 >= 0.0, "{label_y}");
            // The knob and its glow inside the track.
            assert!(glow >= readouts::KNOB_RADIUS);
            assert!(knob.y + glow <= h + 1e-3, "row {row}: {knob:?} {glow}");
        }
    }
    // The mockup's glow where the row has room for it.
    let (_, _, glow) = readouts::track_marks(w, 40.0, 0.5);
    assert_eq!(glow, readouts::GLOW_RADIUS);
    // The ends still read as the ends.
    let h = readouts::ROW - 1.0;
    let (lo, _, _) = readouts::track_marks(w, h, 0.0);
    let (hi, _, _) = readouts::track_marks(w, h, 1.0);
    assert_eq!(
        (lo.x, hi.x),
        (readouts::KNOB_RADIUS, w - readouts::KNOB_RADIUS)
    );
}

#[test]
fn the_voice_reading_stays_on_one_line_inside_its_card() {
    // "A4 · breathy" wrapped to two lines and spilled below the card.
    let mut settings = crate::app::settings();
    settings.fonts = crate::ui::core::theme::font::FACES
        .iter()
        .copied()
        .map(std::borrow::Cow::Borrowed)
        .collect();
    let st = fed(1.0, 2.2);
    let mut r = st.readouts();
    r.rows[6].value = "A\u{266f}4 \u{b7} pressed".to_string();
    let mut ui = iced_test::Simulator::with_size(
        settings,
        Size::new(readouts::WIDTH, 360.0),
        readouts::panel(&r, 360.0),
    );
    let value = ui
        .find("A\u{266f}4 \u{b7} pressed")
        .expect("the reading")
        .bounds();
    // One 16 px line (two read 32).
    assert!(value.height < 24.0, "{value:?}");
    assert!(
        value.x + value.width <= readouts::WIDTH - 14.0 + 0.5,
        "{value:?}"
    );
}

#[test]
fn the_readouts_read_the_mockups_ah() {
    let values = readouts::values(&ah().components);
    assert_eq!(
        values,
        [
            "14 mm",
            "Low",
            "Central",
            "Relaxed",
            "+3 mm",
            "Closed",
            "G4 \u{b7} clear"
        ]
    );
    let p = readouts::positions(&ah().components);
    assert!(close(p[0], 14.0 / 22.0, 1e-4));
    assert!(close(p[5], 0.92, 1e-4), "the velum near Closed");
    assert_eq!(
        readouts::formants_line([640.0, 1190.0, 2620.0]),
        "F1 640 \u{b7} F2 1190 \u{b7} F3 2620 Hz"
    );
    let oo = readouts::values(&articulate(&OO, true, Fit::Good).components);
    assert_eq!(oo[3], "Rounded");
    assert_eq!(oo[4], "\u{2212}4 mm");
}

#[test]
fn the_vowel_map_names_the_nearest_vowel() {
    assert_eq!(readouts::nearest_vowel(640.0, 1190.0), "ah");
    assert_eq!(readouts::nearest_vowel(290.0, 2250.0), "ee");
    assert_eq!(readouts::nearest_vowel(310.0, 870.0), "oo");
    // The corners of the quadrilateral.
    let i = readouts::map_point(300.0, 2300.0);
    let u = readouts::map_point(300.0, 850.0);
    assert!(close(i.x, 22.0, 0.5) && close(i.y, 16.0, 0.5));
    assert!(close(u.x, 128.0, 0.5) && close(u.y, 16.0, 0.5));
}

#[test]
fn the_bands_cover_the_last_two_seconds() {
    let st = fed(0.5, 2.3);
    let bands = st.bands();
    let p = readouts::positions(&st.shown().components);
    for (band, at) in bands.iter().zip(p) {
        let (lo, hi) = band.expect("a band");
        assert!(lo <= at + 1e-3 && at <= hi + 1e-3, "{lo}..{hi} holds {at}");
    }
    let (lo, hi) = bands[0].expect("the jaw's");
    assert!(hi - lo > 0.02, "the jaw moved a little");
    assert!(!st.trail().is_empty());
    // A seek back forgets what came after it.
    let mut st = st;
    st.feed(Some(TractPose::demo(0.6)), 0.6, Instant::now());
    assert!(st.history.iter().all(|s| s.clock <= 0.6));
    assert!(st.history.len() < 10);
}

#[test]
fn the_panel_rests_calmly_without_a_voice_or_a_take() {
    // The demo's "oo", then its rest.
    let st = fed(7.6, 8.6);
    assert!(st.resting());
    assert_eq!(st.readouts().status, Status::Resting);
    let mut st = VocalTractState::new();
    st.feed(None, 1.0, Instant::now());
    let r = st.readouts();
    assert_eq!(r.status, Status::NoTake);
    assert_eq!(r.vowel, None);
    let mut ui = iced_test::simulator(view_of(&st, false));
    assert!(ui
        .find("Select a recorded take, or switch to Live.")
        .is_ok());
    let st = fed(1.0, 2.0);
    assert_eq!(
        st.readouts().status,
        Status::Voiced {
            fit: Fit::Good,
            formants: st.shown().formants
        }
    );
}

// ---------------------------------------------------- the demo, the seam

#[test]
fn the_demo_sings_ah_ee_oo_then_rests() {
    let at = |t: f64| TractPose::demo(t);
    let (a, e, o, r) = (at(2.0), at(4.9), at(7.8), at(8.6));
    assert!(a.voiced && e.voiced && o.voiced && !r.voiced);
    assert!(a.components.tongue_height < 0.4 && a.components.jaw_mm > 12.0);
    assert!(e.components.tongue_height > 0.7 && e.components.tongue_front > 0.7);
    assert!(o.components.tongue_front < 0.35 && o.components.lip_round > 0.8);
    assert_eq!(readouts::nearest_vowel(a.formants[0], a.formants[1]), "ah");
    assert_eq!(readouts::nearest_vowel(e.formants[0], e.formants[1]), "ee");
    assert_eq!(readouts::nearest_vowel(o.formants[0], o.formants[1]), "oo");
    // Round again.
    let again = at(2.0 + f64::from(period()));
    assert_eq!(
        readouts::nearest_vowel(again.formants[0], again.formants[1]),
        "ah"
    );
    assert!(close(again.components.jaw_mm, a.components.jaw_mm, 3.0));
}

#[test]
fn every_demo_pose_is_to_scale_and_of_one_layout() {
    let layout = ah().layout();
    for i in 0..40 {
        let pose = TractPose::demo(f64::from(i) * 0.24);
        assert_eq!(
            pose.layout(),
            layout,
            "one layout: the springs move point by point"
        );
        // An adult male's tract: about 17 cm from the glottis to the lips.
        let centre: Vec<Cm> = pose.rings.iter().map(|r| r.centre).collect();
        let len = length(&centre, false);
        assert!((14.0..20.0).contains(&len), "{len} cm");
        assert!(pose.rings.iter().all(|r| r.radius > 0.0 && r.radius < 2.5));
    }
}

#[test]
fn with_no_take_the_figure_rests_and_the_panel_reads_nothing() {
    // The backing selected after a take: the header said "No take
    // selected", yet the panel read the take's jaw, tongue and lips, and
    // the figure kept its shape.
    let t0 = Instant::now();
    let mut st = VocalTractState::new();
    st.feed(Some(ee()), 1.0, t0);
    st.feed(None, 1.1, t0 + MS * 20);
    for step in 1..=20 {
        st.update(Message::Frame(t0 + MS * (20 + 50 * step)), t0);
    }
    let resting = articulate(&AH, false, Fit::Uncertain);
    assert_eq!(st.shown().outlines.tongue, resting.outlines.tongue);
    let r = st.readouts();
    assert_eq!(r.status, Status::NoTake);
    assert!(
        r.rows.iter().all(|row| row.value == "\u{2014}"),
        "{:?}",
        r.rows
    );
    assert_eq!(r.vowel, None);
    assert_eq!(r.rest, 1.0);
}

#[test]
fn live_with_nothing_sung_yet_rests_neutral_and_reads_nothing() {
    // Take 1's shape, then Live with no voice yet: the panel said "the last
    // shape is held" and read Take 1's jaw, tongue and lips as Live's.
    let t0 = Instant::now();
    let settle = |st: &mut VocalTractState, from: Instant| {
        for step in 1..=20 {
            let at = from + MS * (50 * step);
            st.update(Message::Frame(at), at);
        }
    };
    let mut st = VocalTractState::new();
    st.set_voice(Voice::Take("t1".into()));
    st.feed(Some(ee()), 1.0, t0);
    settle(&mut st, t0);
    st.set_voice(Voice::Live);
    let mut silence = ee();
    silence.voiced = false;
    st.feed(Some(silence.clone()), 0.0, t0 + MS * 1100);
    settle(&mut st, t0 + MS * 1100);
    let resting = articulate(&AH, false, Fit::Uncertain);
    assert_eq!(st.shown().outlines.tongue, resting.outlines.tongue);
    let r = st.readouts();
    assert_eq!(r.status, Status::NotSung { live: true });
    assert!(
        r.rows.iter().all(|row| row.value == "\u{2014}"),
        "{:?}",
        r.rows
    );
    assert!(r.rows.iter().all(|row| row.band.is_none()));
    assert_eq!(r.vowel, None);
    assert_eq!(r.rest, 1.0);
    {
        let mut ui = iced_test::simulator(view_of(&st, true));
        assert!(ui
            .find("No voice yet: sing into the mic to see your shape.")
            .is_ok());
    }
    // Once sung, its own shape, held when the voice stops.
    st.feed(Some(ah()), 0.5, t0 + MS * 2200);
    assert!(matches!(st.readouts().status, Status::Voiced { .. }));
    st.feed(Some(silence), 0.6, t0 + MS * 2300);
    assert_eq!(st.readouts().status, Status::Resting);
    assert_ne!(st.readouts().rows[0].value, "\u{2014}");
}

#[test]
fn the_panel_says_when_the_estimate_is_being_worked_out() {
    let now = Instant::now();
    let mut st = VocalTractState::new();
    st.feed_preparing(false, 1.0, now);
    assert!(st.resting());
    assert_eq!(st.readouts().status, Status::Preparing { live: false });
    {
        let mut ui = iced_test::simulator(view_of(&st, false));
        assert!(ui.find("Reading this take").is_ok());
    }
    st.feed_preparing(true, 1.0, now);
    assert_eq!(st.readouts().status, Status::Preparing { live: true });
    {
        let mut ui = iced_test::simulator(view_of(&st, true));
        assert!(ui.find("Getting ready").is_ok());
    }
    // The first estimate ends it; nothing to read is no take, not preparing.
    st.feed(Some(TractPose::demo(1.0)), 1.1, now);
    assert!(matches!(st.readouts().status, Status::Voiced { .. }));
    st.feed(None, 1.2, now);
    assert_eq!(st.readouts().status, Status::NoTake);
}

#[test]
fn the_clock_counts_from_live_or_reads_the_playhead() {
    let mut st = VocalTractState::new();
    let t0 = Instant::now();
    assert_eq!(st.clock(true, Duration::from_secs(9), t0), 0.0);
    let c = st.clock(true, Duration::from_secs(9), t0 + MS * 1500);
    assert!(close(c as f32, 1.5, 1e-6));
    assert_eq!(st.clock(false, Duration::from_millis(20_900), t0), 20.9);
    // Live again starts from zero.
    assert_eq!(st.clock(true, Duration::ZERO, t0 + MS * 5000), 0.0);
}

// ------------------------------------------------------ accessibility

#[test]
fn the_figure_is_described_for_a_screen_reader() {
    let a = describe(&ah(), TractView::TwoD, None, false);
    assert_eq!(
        a,
        "A side cutaway of the head, to scale, while singing an open vowel: the jaw open, \
         the tongue low and slightly back, the lips apart, the velum raised and the larynx \
         slightly high"
    );
    let e = describe(&ee(), TractView::ThreeD, Some(Preset::ThreeQuarter), true);
    assert!(e.starts_with("A 3D wireframe head, to scale, turned to three quarters"));
    assert!(e.contains("resting on the last shape sung, a close front vowel"));
    assert!(e.contains("the lips spread"));
    // The view carries it, and names its controls.
    let st = fed(1.0, 2.2);
    let label = describe(&st.shown(), TractView::TwoD, st.camera().matching(), false);
    let jaw = format!("Jaw: {}", readouts::values(&st.shown().components)[0]);
    let mut ui = iced_test::simulator(view_of(&st, true));
    for name in [label.as_str(), "View", "Source", "Components", jaw.as_str()] {
        assert!(ui.find(name).is_ok(), "{name}");
    }
    assert!(ui.find("Vowel map: your vowel sits near ah").is_ok());
}

#[test]
fn the_figure_draws_at_every_dock_size() {
    // The canvases draw at the sizes the dock gives, down to its least
    // (the same geometry goes to either renderer: no custom pipeline).
    for view in [TractView::TwoD, TractView::ThreeD] {
        let mut st = fed(1.0, 2.2);
        st.update(Message::SetView(view), Instant::now());
        for size in [
            Size::new(1400.0, 340.0),
            Size::new(900.0, 160.0),
            Size::new(300.0, 120.0),
        ] {
            let mut ui = LiveUi::new(size);
            let frame = ui.snapshot(view_of(&st, true));
            if view == TractView::TwoD && size.width > 1000.0 {
                // The tongue is coral where the fit puts it: the well starts
                // past the padding and the header.
                let well = Size::new(
                    size.width - 16.0 - 16.0 - 16.0 - readouts::WIDTH,
                    size.height - 12.0 - 30.0 - 14.0,
                );
                let fit = Fit2d::new(well);
                let at = fit.px(super::geometry::centroid(&st.shown().outlines.tongue));
                let [r, g, b, _] = frame.pixel((16.0 + at.x) as usize, (42.0 + at.y) as usize);
                assert!(r > g + 40 && r > b + 40, "coral: {r} {g} {b}");
            }
        }
    }
}
