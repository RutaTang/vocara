// media_kit's desktop controls over a 480 × 270 video, frame by frame: the
// layout, showing and hiding, the seek bar, the buttons, the volume button
// and slider, the double press, buffering, and the keyboard shortcuts.

use super::*;
use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};
use iced::keyboard::key::{Named, Physical};
use iced::keyboard::{Key, Location, Modifiers};
use iced::widget::{column, container, stack, Space};

const W: f32 = 480.0;
const H: f32 = 270.0;

/// The window: the controls at the top, a strip under them to leave to.
const WINDOW: Size = Size::new(W, H + 40.0);
const OUTSIDE: Point = Point::new(240.0, H + 20.0);
/// Over the video, clear of the bars.
const MIDDLE: Point = Point::new(240.0, 100.0);

// Where things are in a 480 × 270 video (see `the_bars_lay_out_as_media_kit_did`).
const SEEK_Y: f32 = 212.0;
const PLAY: Point = Point::new(38.0, 242.0);
const VOLUME: Point = Point::new(84.0, 242.0);
const FULLSCREEN: Point = Point::new(442.0, 242.0);
/// Inside the fullscreen icon's top-left corner: white while shown.
const SHOWN_PROBE: Point = Point::new(435.0, 235.0);

const RED: Color = SEEK_BAR_POSITION_COLOR;

fn s(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// Halfway through a 10 s clip, playing, at volume 80.
fn halfway() -> Playback {
    Playback {
        playing: true,
        position: s(5),
        duration: s(10),
        volume: 80.0,
        ..Playback::default()
    }
}

fn view<'a>(playback: Playback) -> Element<'a, Command> {
    column![
        container(video_controls(playback, |c| c))
            .width(W)
            .height(H),
        Space::new().height(40),
    ]
    .into()
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

fn wheel(delta: mouse::ScrollDelta) -> Event {
    Event::Mouse(mouse::Event::WheelScrolled { delta })
}

/// A seek bar point at `fraction` of its width.
fn on_seek_bar(fraction: f32) -> Point {
    Point::new(BAR_MARGIN + (W - 2.0 * BAR_MARGIN) * fraction, SEEK_Y)
}

fn is_white(c: Color) -> bool {
    near(c, Color::WHITE)
}

/// A window with the pointer over the video since `t0`, the controls fully
/// in by the frame it returns the time of.
fn shown(playback: Playback, t0: Instant) -> (Frames, Instant) {
    let mut window = Frames::new(WINDOW);
    window.frame(view(playback), &[], t0);
    window.point_at(MIDDLE);
    window.frame(view(playback), &[], t0 + 10 * MS);
    let t = t0 + 200 * MS;
    window.frame(view(playback), &[], t);
    assert!(is_white(window.pixel(SHOWN_PROBE)), "shown");
    (window, t)
}

// ------------------------------------------------------------------ layout

#[test]
fn the_bars_lay_out_as_media_kit_did() {
    let g = geometry(
        Rectangle::new(Point::ORIGIN, Size::new(W, H)),
        VOLUME_STRIP_LEAD,
    );
    // The seek bar: 16 in from each side, 36 tall, 16 below its slot
    // (which ends where the 56 px bottom bar begins).
    assert_eq!(
        g.seek,
        Rectangle::new(Point::new(16.0, 194.0), Size::new(448.0, 36.0))
    );
    assert_eq!(g.seek_y, SEEK_Y);
    // The bottom bar's row, centred on its 56 px: play (44), 4 px, the
    // volume button (40), the strip, the text; fullscreen at the far end.
    assert_eq!(
        g.play,
        Rectangle::new(Point::new(16.0, 220.0), Size::new(44.0, 44.0))
    );
    assert_eq!(
        g.volume,
        Rectangle::new(Point::new(64.0, 222.0), Size::new(40.0, 40.0))
    );
    assert_eq!(
        g.strip,
        Rectangle::new(Point::new(104.0, 214.0), Size::new(12.0, 56.0))
    );
    assert_eq!(
        g.slider,
        Rectangle::new(Point::new(116.0, 214.0), Size::new(52.0, 56.0))
    );
    assert_eq!(g.text, Point::new(116.0, 236.0));
    assert_eq!(
        g.fullscreen,
        Rectangle::new(Point::new(420.0, 220.0), Size::new(44.0, 44.0))
    );
    assert_eq!(g.spinner, Point::new(240.0, 135.0));
    // The strip open: 12 + 52 + 18, and the text moves along.
    let open = geometry(
        Rectangle::new(Point::ORIGIN, Size::new(W, H)),
        VOLUME_STRIP_OPEN,
    );
    assert_eq!(VOLUME_STRIP_OPEN, 82.0);
    assert_eq!(open.text, Point::new(186.0, 236.0));
    // Hit order: the bottom bar's buttons over the seek bar's area.
    let at = |x: f32, y: f32| target(&g, Point::new(x, y), true);
    assert_eq!(at(30.0, 225.0), Some(Target::Button(Button::Play)));
    assert_eq!(
        at(62.0, 225.0),
        Some(Target::Seek),
        "in the gap before the volume button"
    );
    assert_eq!(at(70.0, 225.0), Some(Target::Button(Button::Volume)));
    assert_eq!(at(70.0, 218.0), Some(Target::Seek), "above it");
    assert_eq!(at(110.0, 220.0), Some(Target::Strip));
    assert_eq!(at(300.0, 225.0), Some(Target::Seek));
    assert_eq!(at(300.0, 240.0), Some(Target::Video));
    assert_eq!(at(240.0, 100.0), Some(Target::Video));
    assert_eq!(
        target(&g, Point::new(30.0, 225.0), false),
        Some(Target::Video)
    );
}

#[test]
fn the_position_reads_as_media_kits_labels() {
    assert_eq!(position_text(s(5), s(12)), "00:05 / 00:12");
    assert_eq!(position_text(s(65), s(600)), "01:05 / 10:00");
    // Up to an hour's reference the minutes run on.
    assert_eq!(position_text(s(3600), s(3600)), "60:00 / 60:00");
    assert_eq!(position_text(s(61), s(3601)), "00:01:01 / 01:00:01");
    assert_eq!(
        duration_label(s(90_061), s(90_061)),
        "001:01:01:01",
        "past a day"
    );
    assert_eq!(volume_icon(0.0), MaterialIcon::VolumeOff);
    assert_eq!(volume_icon(49.9), MaterialIcon::VolumeDown);
    assert_eq!(volume_icon(50.0), MaterialIcon::VolumeUp);
}

// -------------------------------------------------------- showing, hiding

#[test]
fn the_controls_show_while_the_pointer_moves_and_hide_3_s_after() {
    let p = halfway();
    let mut window = Frames::new(WINDOW);
    let t0 = Instant::now();
    assert!(!window.frame(view(p), &[], t0), "idle: no frames");
    assert!(!near(window.pixel(on_seek_bar(0.25)), RED), "hidden");
    window.point_at(MIDDLE);
    assert!(window.frame(view(p), &[], t0 + 100 * MS), "fading in");
    assert!(
        !near(window.pixel(on_seek_bar(0.25)), RED),
        "the fade starts"
    );
    window.frame(view(p), &[], t0 + 175 * MS);
    let halfway_in = window.pixel(on_seek_bar(0.25));
    assert!(halfway_in.r > 0.2 && halfway_in.r < 0.8, "{halfway_in:?}");
    assert!(!window.frame(view(p), &[], t0 + 250 * MS), "in: still");
    assert!(
        near(window.pixel(on_seek_bar(0.25)), RED),
        "the played span"
    );
    // Standing still, they stay 3 s from the entry, then fade out.
    window.frame(view(p), &[], t0 + 3099 * MS);
    assert!(near(window.pixel(on_seek_bar(0.25)), RED));
    assert!(window.frame(view(p), &[], t0 + 3100 * MS), "fading out");
    assert!(!window.frame(view(p), &[], t0 + 3250 * MS), "out: still");
    assert!(!near(window.pixel(on_seek_bar(0.25)), RED));
    assert!(!is_white(window.pixel(SHOWN_PROBE)));
    // A move brings them back and starts the timer again.
    window.point_at(Point::new(250.0, 100.0));
    window.frame(view(p), &[], t0 + 4000 * MS);
    window.frame(view(p), &[], t0 + 4200 * MS);
    assert!(is_white(window.pixel(SHOWN_PROBE)));
    window.point_at(Point::new(260.0, 100.0));
    window.frame(view(p), &[], t0 + 6000 * MS);
    window.frame(view(p), &[], t0 + 8900 * MS);
    assert!(
        is_white(window.pixel(SHOWN_PROBE)),
        "3 s from the last move"
    );
    window.frame(view(p), &[], t0 + 9000 * MS);
    window.frame(view(p), &[], t0 + 9200 * MS);
    assert!(!is_white(window.pixel(SHOWN_PROBE)));
}

#[test]
fn the_pointer_leaving_hides_them_at_once() {
    let t0 = Instant::now();
    let (mut window, t) = shown(halfway(), t0);
    window.point_at(OUTSIDE);
    assert!(window.frame(view(halfway()), &[], t + 10 * MS));
    window.frame(view(halfway()), &[], t + 85 * MS);
    let fading = window.pixel(SHOWN_PROBE);
    assert!(!is_white(fading) && fading.r > 0.2, "{fading:?}");
    assert!(!window.frame(view(halfway()), &[], t + 160 * MS));
    assert!(near(window.pixel(SHOWN_PROBE), BACKGROUND));
}

#[test]
fn the_bottom_gradient_darkens_the_lower_half() {
    let p = halfway();
    let backdrop = || {
        container(Space::new())
            .width(W)
            .height(H)
            .style(|_| crate::ui::core::theme::boxed(Some(Color::WHITE), 0.0, None))
    };
    let view = || -> Element<'static, Command> {
        column![
            stack![backdrop(), video_controls(p, |c| c)]
                .width(W)
                .height(H),
            Space::new().height(40),
        ]
        .into()
    };
    let mut window = Frames::new(WINDOW);
    let t0 = Instant::now();
    window.frame(view(), &[], t0);
    window.point_at(Point::new(240.0, 60.0));
    window.frame(view(), &[], t0 + 10 * MS);
    window.frame(view(), &[], t0 + 200 * MS);
    let close = |a: Color, b: Color| {
        let [ar, ag, ab, _] = a.into_rgba8();
        let [br, bg, bb, _] = b.into_rgba8();
        ar.abs_diff(br) <= 6 && ag.abs_diff(bg) <= 6 && ab.abs_diff(bb) <= 6
    };
    // Clear down to halfway, then to black at 38 % at the bottom edge.
    assert!(near(window.pixel(Point::new(300.0, 120.0)), Color::WHITE));
    let bottom = window.pixel(Point::new(300.0, H - 1.0));
    let expected = over(
        Color {
            a: GRADIENT_COLOR.a * ((H - 0.5) / H - 0.5) / 0.5,
            ..GRADIENT_COLOR
        },
        Color::WHITE,
    );
    assert!(close(bottom, expected), "{bottom:?} vs {expected:?}");
    let three_quarters = window.pixel(Point::new(300.0, 0.75 * H));
    let expected = over(
        Color {
            a: GRADIENT_COLOR.a * 0.5,
            ..GRADIENT_COLOR
        },
        Color::WHITE,
    );
    assert!(close(three_quarters, expected), "{three_quarters:?}");
}

#[test]
fn the_cursor_is_the_hand_over_what_takes_a_click() {
    let t0 = Instant::now();
    let (mut window, t) = shown(halfway(), t0);
    let mut at = |p: Point, ms: u32| {
        window.point_at(p);
        window.frame(view(halfway()), &[], t + ms * MS);
        window.interaction()
    };
    assert_eq!(at(MIDDLE, 10), mouse::Interaction::Idle, "the arrow");
    assert_eq!(at(PLAY, 20), mouse::Interaction::Pointer);
    assert_eq!(at(VOLUME, 30), mouse::Interaction::Pointer);
    assert_eq!(at(FULLSCREEN, 40), mouse::Interaction::Pointer);
    assert_eq!(at(on_seek_bar(0.7), 50), mouse::Interaction::Pointer);
    assert_eq!(at(Point::new(300.0, 240.0), 60), mouse::Interaction::Idle);
}

// ---------------------------------------------------------------- seek bar

#[test]
fn hovering_the_seek_bar_thickens_it_and_grows_its_thumb() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    // Row 209 lies above the bar at rest (210.4 to 213.6), inside it
    // hovered (209.2 to 214.8). The thumb's left edge sits at (448 - 6) ×
    // 0.5 past the bar's start.
    let above = Point::new(400.0, 209.0);
    let thumb = Point::new(16.0 + 221.0 + 8.0, 207.0);
    assert!(near(window.pixel(above), BACKGROUND));
    assert!(!near(window.pixel(thumb), RED));
    window.point_at(on_seek_bar(0.75));
    assert!(window.frame(view(p), &[], t + 10 * MS), "it grows");
    window.frame(view(p), &[], t + 170 * MS);
    assert!(!near(window.pixel(above), BACKGROUND), "thicker");
    assert!(near(window.pixel(thumb), RED), "the thumb");
    // The hovered span, from the start to the pointer, over the bar.
    let bar = over(SEEK_BAR_COLOR, BACKGROUND);
    assert!(near(
        window.pixel(Point::new(300.0, SEEK_Y)),
        over(SEEK_BAR_COLOR, bar)
    ));
    assert!(
        near(window.pixel(Point::new(400.0, SEEK_Y)), bar),
        "past the pointer"
    );
    assert!(near(window.pixel(Point::new(200.0, SEEK_Y)), RED), "played");
    // Off it: back to thin, the span and the thumb gone.
    window.point_at(MIDDLE);
    window.frame(view(p), &[], t + 200 * MS);
    window.frame(view(p), &[], t + 360 * MS);
    assert!(near(window.pixel(above), BACKGROUND));
    assert!(!near(window.pixel(thumb), RED));
    assert!(near(window.pixel(Point::new(300.0, SEEK_Y)), bar));
}

#[test]
fn the_buffered_span_lies_over_the_bar_under_the_played_one() {
    let t0 = Instant::now();
    let p = Playback {
        buffer: Duration::from_millis(7_500),
        ..halfway()
    };
    let (window, _) = shown(p, t0);
    let bar = over(SEEK_BAR_COLOR, BACKGROUND);
    assert!(near(window.pixel(on_seek_bar(0.25)), RED));
    assert!(near(
        window.pixel(on_seek_bar(0.6)),
        over(SEEK_BAR_COLOR, bar)
    ));
    assert!(near(window.pixel(on_seek_bar(0.9)), bar));
}

#[test]
fn a_press_on_the_seek_bar_seeks_on_each_move_and_on_release() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    window.point_at(on_seek_bar(0.25));
    window.frame(view(p), &[], t + 10 * MS);
    let (_, sent) = window.frame_messages(view(p), &[press()], t + 20 * MS);
    assert!(sent.is_empty(), "a press alone seeks nowhere: {sent:?}");
    window.point_at(on_seek_bar(0.5));
    let (_, sent) = window.frame_messages(view(p), &[], t + 30 * MS);
    assert_eq!(sent, vec![Command::Seek(s(5))]);
    window.point_at(on_seek_bar(0.75));
    let (_, sent) = window.frame_messages(view(p), &[], t + 40 * MS);
    assert_eq!(sent, vec![Command::Seek(Duration::from_millis(7_500))]);
    // While pressed the played span follows the pointer.
    assert!(near(window.pixel(on_seek_bar(0.7)), RED));
    // Held, they stay past the 3 s (the press stopped the timer).
    window.frame(view(p), &[], t + 4_000 * MS);
    assert!(is_white(window.pixel(SHOWN_PROBE)), "held");
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 4_100 * MS);
    assert_eq!(sent, vec![Command::Seek(Duration::from_millis(7_500))]);
    // The release started a new timer.
    window.frame(view(p), &[], t + 7_050 * MS);
    assert!(is_white(window.pixel(SHOWN_PROBE)));
    window.frame(view(p), &[], t + 7_100 * MS);
    window.frame(view(p), &[], t + 7_300 * MS);
    assert!(
        !is_white(window.pixel(SHOWN_PROBE)),
        "3 s after the release"
    );
}

#[test]
fn a_drag_past_the_bar_seeks_to_its_end() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    window.point_at(on_seek_bar(0.5));
    window.frame(view(p), &[], t + 10 * MS);
    window.frame(view(p), &[press()], t + 20 * MS);
    window.point_at(Point::new(W - 2.0, 150.0));
    let (_, sent) = window.frame_messages(view(p), &[], t + 30 * MS);
    assert_eq!(sent, vec![Command::Seek(s(10))]);
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 40 * MS);
    assert_eq!(sent, vec![Command::Seek(s(10))]);
}

#[test]
fn a_completed_clip_resets_the_played_span_until_it_moves() {
    let t0 = Instant::now();
    let end = Playback {
        playing: false,
        position: s(10),
        ..halfway()
    };
    let (mut window, t) = shown(end, t0);
    assert!(
        near(window.pixel(on_seek_bar(0.9)), RED),
        "played to the end"
    );
    let completed = Playback {
        completed: true,
        ..end
    };
    window.frame(view(completed), &[], t + 10 * MS);
    assert!(!near(window.pixel(on_seek_bar(0.9)), RED), "reset");
    assert!(!near(window.pixel(on_seek_bar(0.01)), RED));
    // The next position shows again.
    let replayed = Playback {
        playing: true,
        completed: false,
        position: s(1),
        ..end
    };
    window.frame(view(replayed), &[], t + 20 * MS);
    assert!(near(window.pixel(on_seek_bar(0.05)), RED));
    assert!(!near(window.pixel(on_seek_bar(0.5)), RED));
}

// ----------------------------------------------------------------- buttons

#[test]
fn the_play_and_fullscreen_buttons_report_on_release() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    window.point_at(PLAY);
    window.frame(view(p), &[], t + 10 * MS);
    let (_, sent) = window.frame_messages(view(p), &[press()], t + 20 * MS);
    assert!(sent.is_empty());
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 600 * MS);
    assert_eq!(sent, vec![Command::PlayOrPause]);
    window.point_at(FULLSCREEN);
    window.frame(view(p), &[], t + 1_200 * MS);
    let (_, sent) = window.frame_messages(view(p), &[press(), release()], t + 1_300 * MS);
    assert_eq!(sent, vec![Command::ToggleFullscreen]);
    // A press that wanders past the touch slop is no tap.
    window.point_at(PLAY);
    window.frame(view(p), &[], t + 1_900 * MS);
    window.frame(view(p), &[press()], t + 2_000 * MS);
    window.point_at(Point::new(PLAY.x + 20.0, PLAY.y));
    window.frame(view(p), &[], t + 2_010 * MS);
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 2_020 * MS);
    assert!(sent.is_empty(), "{sent:?}");
}

#[test]
fn a_hovered_button_lights_in_forui_ghost_and_material_ink() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    // A corner of the play button, clear of its icon.
    let corner = Point::new(22.0, 226.0);
    assert!(near(window.pixel(corner), BACKGROUND));
    window.point_at(PLAY);
    window.frame(view(p), &[], t + 10 * MS);
    window.frame(view(p), &[], t + 70 * MS);
    let hovered = over(INK_HOVERED, BUTTON_LIT);
    assert!(
        near(window.pixel(corner), hovered),
        "{:?}",
        window.pixel(corner)
    );
    window.frame(view(p), &[press()], t + 100 * MS);
    window.frame(view(p), &[], t + 310 * MS);
    let pressed = over(INK_PRESSED, hovered);
    assert!(
        near(window.pixel(corner), pressed),
        "{:?}",
        window.pixel(corner)
    );
}

#[test]
fn the_play_icon_turns_into_pause_over_200_ms() {
    let t0 = Instant::now();
    let paused = Playback {
        playing: false,
        ..halfway()
    };
    let (mut window, t) = shown(paused, t0);
    // Inside the play triangle, between the pause bars.
    let probe = Point::new(36.0, 241.0);
    assert!(is_white(window.pixel(probe)), "play");
    assert!(window.frame(view(halfway()), &[], t + 10 * MS), "it turns");
    window.frame(view(halfway()), &[], t + 210 * MS);
    assert!(near(window.pixel(probe), BACKGROUND), "pause");
}

// ----------------------------------------------------------------- volume

#[test]
fn the_volume_button_mutes_restores_and_from_zero_goes_full() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    window.point_at(VOLUME);
    window.frame(view(p), &[], t + 10 * MS);
    let (_, sent) = window.frame_messages(view(p), &[press(), release()], t + 20 * MS);
    assert_eq!(sent, vec![Command::SetVolume(0.0)], "mute");
    let muted = Playback { volume: 0.0, ..p };
    let (_, sent) = window.frame_messages(view(muted), &[press(), release()], t + 500 * MS);
    assert_eq!(sent, vec![Command::SetVolume(80.0)], "restored");
    // At 0 without a mute of its own (media-kit#250): full.
    let (_, sent) = window.frame_messages(view(muted), &[press(), release()], t + 1_000 * MS);
    assert_eq!(sent, vec![Command::SetVolume(FULL_VOLUME)]);
}

#[test]
fn the_mute_goes_with_the_bars() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    window.point_at(VOLUME);
    window.frame(view(p), &[], t + 10 * MS);
    window.frame(view(p), &[press(), release()], t + 20 * MS);
    let muted = Playback { volume: 0.0, ..p };
    // Hidden and unmounted: the button's state went with them.
    window.frame(view(muted), &[], t + 3_010 * MS);
    window.frame(view(muted), &[], t + 3_200 * MS);
    assert!(!is_white(window.pixel(SHOWN_PROBE)));
    window.point_at(Point::new(VOLUME.x + 2.0, VOLUME.y));
    window.frame(view(muted), &[], t + 3_300 * MS);
    let (_, sent) = window.frame_messages(view(muted), &[press(), release()], t + 3_400 * MS);
    assert_eq!(sent, vec![Command::SetVolume(FULL_VOLUME)], "not the 80");
}

#[test]
fn hovering_the_volume_opens_its_slider() {
    let t0 = Instant::now();
    let silent = Playback {
        volume: 0.0,
        ..halfway()
    };
    let (mut window, t) = shown(silent, t0);
    // The thumb's left half at volume 0 (its centre on the track's start,
    // 116), inside the open strip.
    let thumb = Point::new(112.0, 242.0);
    assert!(near(window.pixel(thumb), BACKGROUND), "closed");
    window.point_at(VOLUME);
    assert!(window.frame(view(silent), &[], t + 10 * MS));
    window.frame(view(silent), &[], t + 85 * MS);
    let opening = window.pixel(thumb);
    assert!(!is_white(opening) && opening.r > 0.1, "{opening:?}");
    window.frame(view(silent), &[], t + 170 * MS);
    assert!(is_white(window.pixel(thumb)), "open");
    // Off the group it closes.
    window.point_at(MIDDLE);
    window.frame(view(silent), &[], t + 200 * MS);
    window.frame(view(silent), &[], t + 360 * MS);
    assert!(near(window.pixel(thumb), BACKGROUND));
}

/// A window with the volume slider open.
fn slider_open(p: Playback, t0: Instant) -> (Frames, Instant) {
    let (mut window, t) = shown(p, t0);
    window.point_at(VOLUME);
    window.frame(view(p), &[], t + 10 * MS);
    window.frame(view(p), &[], t + 200 * MS);
    (window, t + 200 * MS)
}

/// The slider's point for `volume`.
fn on_slider(volume: f32) -> Point {
    Point::new(116.0 + 52.0 * volume / 100.0, 242.0)
}

#[test]
fn the_slider_sets_the_volume_where_a_tap_lands() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = slider_open(p, t0);
    window.point_at(on_slider(25.0));
    window.frame(view(p), &[], t + 10 * MS);
    assert_eq!(window.interaction(), mouse::Interaction::Pointer);
    let (_, sent) = window.frame_messages(view(p), &[press()], t + 20 * MS);
    assert!(sent.is_empty(), "the tap-down waits");
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 60 * MS);
    assert_eq!(sent, vec![Command::SetVolume(25.0)]);
    // Held 100 ms, the tap-down comes before the release.
    window.point_at(on_slider(50.0));
    window.frame(view(p), &[], t + 500 * MS);
    window.frame(view(p), &[press()], t + 510 * MS);
    let (_, sent) = window.frame_messages(view(p), &[], t + 600 * MS);
    assert!(sent.is_empty());
    let (_, sent) = window.frame_messages(view(p), &[], t + 610 * MS);
    assert_eq!(sent, vec![Command::SetVolume(50.0)]);
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 700 * MS);
    assert!(sent.is_empty());
}

#[test]
fn a_drag_on_the_slider_follows_the_pointer() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = slider_open(p, t0);
    window.point_at(on_slider(25.0));
    window.frame(view(p), &[], t + 10 * MS);
    window.frame(view(p), &[press()], t + 20 * MS);
    window.point_at(on_slider(50.0));
    let (_, sent) = window.frame_messages(view(p), &[], t + 30 * MS);
    assert_eq!(
        sent,
        vec![Command::SetVolume(50.0)],
        "the drag starts there"
    );
    window.point_at(on_slider(100.0));
    let (_, sent) = window.frame_messages(view(p), &[], t + 40 * MS);
    assert_eq!(sent, vec![Command::SetVolume(100.0)]);
    window.point_at(Point::new(200.0, 242.0));
    let (_, sent) = window.frame_messages(view(p), &[], t + 50 * MS);
    assert!(sent.is_empty(), "past the end: still 100");
    let (_, sent) = window.frame_messages(view(p), &[release()], t + 60 * MS);
    assert!(sent.is_empty());
}

#[test]
fn the_wheel_over_the_volume_button_steps_it_by_5() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    let lines = |y: f32| wheel(mouse::ScrollDelta::Lines { x: 0.0, y });
    // Over the video: nothing (modifyVolumeOnScroll is off).
    let (_, sent) = window.frame_messages(view(p), &[lines(1.0)], t + 10 * MS);
    assert!(sent.is_empty());
    window.point_at(VOLUME);
    window.frame(view(p), &[], t + 20 * MS);
    let (_, sent) = window.frame_messages(view(p), &[lines(1.0)], t + 30 * MS);
    assert_eq!(sent, vec![Command::SetVolume(85.0)], "up");
    let (_, sent) = window.frame_messages(view(p), &[lines(-2.0)], t + 40 * MS);
    assert_eq!(
        sent,
        vec![Command::SetVolume(75.0)],
        "down, one step an event"
    );
    let loud = Playback { volume: 98.0, ..p };
    let (_, sent) = window.frame_messages(view(loud), &[lines(1.0)], t + 50 * MS);
    assert_eq!(sent, vec![Command::SetVolume(100.0)], "clamped");
    // A trackpad's pixel scroll was a pan, not a scroll signal.
    let pan = wheel(mouse::ScrollDelta::Pixels { x: 0.0, y: 10.0 });
    let (_, sent) = window.frame_messages(view(p), &[pan], t + 60 * MS);
    assert!(sent.is_empty());
    // Shift turns the wheel sideways; Flutter turned it back.
    let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::SHIFT));
    let sideways = wheel(mouse::ScrollDelta::Lines { x: 1.0, y: 0.0 });
    let (_, sent) = window.frame_messages(view(p), &[shift, sideways], t + 70 * MS);
    assert_eq!(sent, vec![Command::SetVolume(85.0)]);
}

// ------------------------------------------------------------ double press

#[test]
fn two_taps_within_400_ms_toggle_fullscreen() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    let tap = [press(), release()];
    let (_, sent) = window.frame_messages(view(p), &tap, t + 600 * MS);
    assert!(sent.is_empty(), "one tap");
    let (_, sent) = window.frame_messages(view(p), &tap, t + 900 * MS);
    assert_eq!(sent, vec![Command::ToggleFullscreen]);
    let (_, sent) = window.frame_messages(view(p), &tap, t + 1_400 * MS);
    assert!(sent.is_empty(), "500 ms after the last");
    // A tap on the seek bar counts; one the play button won does not.
    window.point_at(on_seek_bar(0.5));
    window.frame(view(p), &[], t + 2_000 * MS);
    window.frame(view(p), &tap, t + 2_100 * MS);
    let (_, sent) = window.frame_messages(view(p), &tap, t + 2_300 * MS);
    assert!(sent.contains(&Command::ToggleFullscreen), "{sent:?}");
    window.point_at(PLAY);
    window.frame(view(p), &[], t + 3_000 * MS);
    window.frame(view(p), &tap, t + 3_100 * MS);
    let (_, sent) = window.frame_messages(view(p), &tap, t + 3_300 * MS);
    assert_eq!(sent, vec![Command::PlayOrPause]);
}

#[test]
fn a_scrolled_page_moves_the_controls_and_their_taps_with_it() {
    // The page's scrollable translates the cursor, not the event.
    let p = halfway();
    let view = || -> Element<'static, Command> {
        iced::widget::scrollable(column![
            Space::new().height(200),
            container(video_controls(p, |c| c)).width(W).height(H),
            Space::new().height(400),
        ])
        .height(Length::Fill)
        .into()
    };
    let mut window = Frames::new(WINDOW);
    let t0 = Instant::now();
    window.point_at(Point::new(240.0, 10.0));
    window.frame(view(), &[], t0);
    let down = wheel(mouse::ScrollDelta::Pixels { x: 0.0, y: -150.0 });
    window.frame(view(), &[down], t0 + 10 * MS);
    // The video's middle, 150 px up the window now.
    window.point_at(Point::new(240.0, 185.0));
    window.frame(view(), &[], t0 + 600 * MS);
    let tap = [press(), release()];
    window.frame(view(), &tap, t0 + 700 * MS);
    let (_, sent) = window.frame_messages(view(), &tap, t0 + 900 * MS);
    assert_eq!(sent, vec![Command::ToggleFullscreen]);
    // A press on the play button that moves a little is still its tap.
    let play = Point::new(PLAY.x, PLAY.y + 200.0 - 150.0);
    window.point_at(play);
    window.frame(view(), &[], t0 + 1_500 * MS);
    window.frame(view(), &[press()], t0 + 1_510 * MS);
    window.point_at(Point::new(play.x + 3.0, play.y));
    window.frame(view(), &[], t0 + 1_520 * MS);
    let (_, sent) = window.frame_messages(view(), &[release()], t0 + 1_530 * MS);
    assert_eq!(sent, vec![Command::PlayOrPause]);
}

#[test]
fn a_tap_within_400_ms_of_their_creation_toggles_fullscreen() {
    // `last` starts as the controls are made: a double click whose first
    // click started the video lands its second on them.
    let p = halfway();
    let mut window = Frames::new(WINDOW);
    window.point_at(MIDDLE);
    let t0 = Instant::now();
    window.frame(view(p), &[], t0);
    let (_, sent) = window.frame_messages(view(p), &[press(), release()], t0 + 300 * MS);
    assert_eq!(sent, vec![Command::ToggleFullscreen]);
}

#[test]
fn the_fullscreen_button_shows_where_the_video_is() {
    let t0 = Instant::now();
    let p = halfway();
    let (mut window, t) = shown(p, t0);
    // The fullscreen icon's corners point out, fullscreen_exit's in: the
    // outer corner of the top-left bracket is ink only for the first.
    let outer = Point::new(434.0, 234.0);
    assert!(is_white(window.pixel(outer)));
    let fullscreen = |p: Playback| -> Element<'static, Command> {
        column![
            container(video_controls(p, |c| c).fullscreen(true))
                .width(W)
                .height(H),
            Space::new().height(40),
        ]
        .into()
    };
    window.frame(fullscreen(p), &[], t + 10 * MS);
    assert!(!is_white(window.pixel(outer)));
}

// ------------------------------------------------------------- buffering

#[test]
fn buffering_fades_a_spinner_in_over_the_video() {
    let waiting = Playback {
        buffering: true,
        ..halfway()
    };
    let mut window = Frames::new(WINDOW);
    let t0 = Instant::now();
    let ink = |w: &Frames| w.ink(Point::new(216.0, 111.0), Point::new(264.0, 159.0));
    window.frame(view(halfway()), &[], t0);
    assert_eq!(ink(&window), 0);
    // Hidden controls or not, it shows.
    assert!(window.frame(view(waiting), &[], t0 + 10 * MS));
    window.frame(view(waiting), &[], t0 + 30 * MS);
    window.frame(view(waiting), &[], t0 + 700 * MS);
    assert!(ink(&window) > 200, "spinning: {}", ink(&window));
    assert!(
        window.frame(view(waiting), &[], t0 + 710 * MS),
        "it spins on"
    );
    window.frame(view(halfway()), &[], t0 + 800 * MS);
    assert!(
        !window.frame(view(halfway()), &[], t0 + 960 * MS),
        "gone: still"
    );
    assert_eq!(ink(&window), 0);
}

// ------------------------------------------------------------ the block

#[test]
fn disabled_controls_take_no_pointer() {
    let p = halfway();
    let blocked = |p: Playback| -> Element<'static, Command> {
        column![
            container(video_controls(p, |c| c).enabled(false))
                .width(W)
                .height(H),
            Space::new().height(40),
        ]
        .into()
    };
    let mut window = Frames::new(WINDOW);
    let t0 = Instant::now();
    window.point_at(PLAY);
    window.frame(blocked(p), &[], t0);
    let (_, sent) = window.frame_messages(blocked(p), &[press(), release()], t0 + 500 * MS);
    assert!(sent.is_empty());
    window.frame(blocked(p), &[], t0 + 700 * MS);
    assert!(!is_white(window.pixel(SHOWN_PROBE)), "never shown");
}

// ------------------------------------------------------------- shortcuts

fn key_press(key: Key, modifiers: Modifiers, repeat: bool) -> keyboard::Event {
    keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers,
        text: None,
        repeat,
    }
}

fn plain(key: Key) -> Option<Shortcut> {
    shortcut(&key_press(key, Modifiers::empty(), false))
}

fn letter(c: &str) -> Key {
    Key::Character(c.into())
}

#[test]
fn every_default_shortcut() {
    assert_eq!(plain(Key::Named(Named::MediaPlay)), Some(Shortcut::Play));
    assert_eq!(plain(Key::Named(Named::MediaPause)), Some(Shortcut::Pause));
    assert_eq!(
        plain(Key::Named(Named::MediaPlayPause)),
        Some(Shortcut::PlayOrPause)
    );
    assert_eq!(
        plain(Key::Named(Named::MediaTrackNext)),
        Some(Shortcut::Next)
    );
    assert_eq!(
        plain(Key::Named(Named::MediaTrackPrevious)),
        Some(Shortcut::Previous)
    );
    assert_eq!(plain(Key::Named(Named::Space)), Some(Shortcut::PlayOrPause));
    assert_eq!(plain(letter("j")), Some(Shortcut::SeekBy(-10_000)));
    assert_eq!(plain(letter("i")), Some(Shortcut::SeekBy(10_000)));
    assert_eq!(
        plain(Key::Named(Named::ArrowLeft)),
        Some(Shortcut::SeekBy(-2_000))
    );
    assert_eq!(
        plain(Key::Named(Named::ArrowRight)),
        Some(Shortcut::SeekBy(2_000))
    );
    assert_eq!(
        plain(Key::Named(Named::ArrowUp)),
        Some(Shortcut::VolumeBy(5.0))
    );
    assert_eq!(
        plain(Key::Named(Named::ArrowDown)),
        Some(Shortcut::VolumeBy(-5.0))
    );
    assert_eq!(plain(letter("f")), Some(Shortcut::ToggleFullscreen));
    assert_eq!(
        plain(Key::Named(Named::Escape)),
        Some(Shortcut::ExitFullscreen)
    );
    // Not L (media_kit bound I), nor anything else.
    assert_eq!(plain(letter("l")), None);
    assert_eq!(plain(letter("k")), None);
    assert_eq!(plain(Key::Named(Named::Enter)), None);
}

#[test]
fn a_shortcut_takes_no_modifier_and_repeats() {
    for modifiers in [
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::LOGO,
    ] {
        let event = key_press(Key::Named(Named::Space), modifiers, false);
        assert_eq!(shortcut(&event), None, "{modifiers:?}");
    }
    let held = key_press(Key::Named(Named::ArrowRight), Modifiers::empty(), true);
    assert_eq!(shortcut(&held), Some(Shortcut::SeekBy(2_000)));
    // Caps Lock gives the capital; the logical key is the same.
    assert_eq!(plain(letter("F")), Some(Shortcut::ToggleFullscreen));
    let released = keyboard::Event::KeyReleased {
        key: Key::Named(Named::Space),
        modified_key: Key::Named(Named::Space),
        physical_key: Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
    };
    assert_eq!(shortcut(&released), None, "only a press");
}

#[test]
fn a_shortcuts_seek_lands_where_mpv_put_it() {
    let at = |ms: u64| Duration::from_millis(ms);
    let length = s(30);
    assert_eq!(seek_target(s(5), 2_000, length), Some(s(7)));
    assert_eq!(seek_target(s(5), -2_000, length), Some(s(3)));
    // A negative absolute time counts from the end.
    assert_eq!(seek_target(s(4), -10_000, length), Some(s(24)));
    assert_eq!(seek_target(s(1), -2_000, length), Some(s(29)));
    assert_eq!(seek_target(s(1), -60_000, length), Some(Duration::ZERO));
    // ...which needs the length.
    assert_eq!(seek_target(s(1), -2_000, Duration::ZERO), None);
    // Past the end, the end.
    assert_eq!(seek_target(s(25), 10_000, length), Some(length));
    // Whole milliseconds, truncated toward zero.
    assert_eq!(
        seek_target(Duration::from_micros(4_000_500), 2_000, length),
        Some(at(6_000))
    );
    assert_eq!(
        seek_target(Duration::from_micros(1_999_500), -2_000, length),
        Some(Duration::ZERO),
        "−0.5 ms is 0"
    );
    assert_eq!(seek_target(s(5), 2_000, Duration::ZERO), Some(s(7)));
}
