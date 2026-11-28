// The inline video player over a fake media player: lazy creation, the
// unavailable clip, the release of every player built, the blocked poster,
// frames uploaded only when they change, the controls' commands, each
// keyboard shortcut, and fullscreen.

use super::*;
use iced::keyboard::key::Named;
use iced::widget::container;
use iced::Point;
use iced_test::simulator;
use vocara_services::media_player::{FakeMediaPlayers, FakePlayerHandle, FULL_VOLUME};

const MISSING: &str = "asset://01J00000000000000000000000";
const CLIP: &str = "/tmp/example.mp4";

/// The poster's centre in the 1024 px window: the box is 520 wide, its
/// video 518 × 291.375 inside the 1 px border.
const POSTER: Point = Point::new(260.0, 146.0);

/// The inline controls' fullscreen button: 16 in from the video's right
/// edge, centred on the bottom bar (its 56 px at the video's bottom).
const FULLSCREEN_BUTTON: Point = Point::new(1.0 + 518.0 - 16.0 - 22.0, 1.0 + 291.375 - 28.0);

fn click() -> Vec<Event> {
    vec![
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]
}

/// What a click at `at` on the view publishes.
fn clicked(video: &TechniqueVideoPlayer, play_blocked: bool, at: Point) -> Vec<Message> {
    let mut ui = simulator(container(video.view(play_blocked)));
    ui.point_at(at);
    let _ = ui.simulate(click());
    ui.into_messages().collect()
}

fn started(players: &FakeMediaPlayers) -> (TechniqueVideoPlayer, FakePlayerHandle) {
    let mut video = TechniqueVideoPlayer::new(CLIP);
    video.update(Message::Play, players, false);
    let handle = players.built().pop().expect("a player was built");
    (video, handle)
}

#[test]
fn play_on_a_video_example_whose_file_is_missing_shows_it_as_unavailable() {
    let players = FakeMediaPlayers::new();
    let mut video = TechniqueVideoPlayer::new(MISSING);
    for m in clicked(&video, false, POSTER) {
        video.update(m, &players, false);
    }
    // Even a Play that got through builds nothing.
    video.update(Message::Play, &players, false);
    assert!(video.is_unavailable() && !video.has_player());
    {
        let mut ui = simulator(container(video.view(false)));
        assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_ok());
    }
    drop(video);
    assert!(players.built().iter().all(FakePlayerHandle::disposed));
}

// Dart: the fake could not back a VideoController, standing in for any
// failure after the player was built; it threw out of the tap and leaked.
#[test]
fn a_video_example_that_fails_to_open_releases_its_player() {
    let players = FakeMediaPlayers::failing_open();
    let mut video = TechniqueVideoPlayer::new(CLIP);
    let sent = clicked(&video, false, POSTER);
    assert_eq!(sent, vec![Message::Play]);
    for m in sent {
        video.update(m, &players, false);
    }
    let built = players.built();
    assert_eq!(built.len(), 1);
    assert!(built[0].disposed());
    assert!(video.is_unavailable() && !video.has_player());
    let mut ui = simulator(container(video.view(false)));
    assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_ok());
}

#[test]
fn the_player_is_created_on_the_first_play_and_opens_playing() {
    let players = FakeMediaPlayers::new();
    let mut video = TechniqueVideoPlayer::new(CLIP);
    assert!(players.built().is_empty(), "no player just for showing it");
    assert!(!video.needs_tick());
    video.update(Message::Play, &players, false);
    video.update(Message::Play, &players, false);
    let built = players.built();
    assert_eq!(built.len(), 1, "once");
    assert_eq!(built[0].kind(), MediaKind::Video);
    assert_eq!(built[0].opened(), vec![(CLIP.to_string(), true)]);
    assert!(video.has_player() && video.is_playing() && video.needs_tick());
    drop(video);
    assert!(built[0].disposed(), "released with the widget");
}

#[test]
fn a_blocked_poster_takes_no_input() {
    let players = FakeMediaPlayers::new();
    let mut video = TechniqueVideoPlayer::new(CLIP);
    assert!(clicked(&video, true, POSTER).is_empty());
    video.update(Message::Play, &players, true);
    assert!(players.built().is_empty());
}

#[test]
fn a_video_started_while_a_take_records_is_paused_again() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    // Blocked: controls take nothing, not even Pause (Dart's IgnorePointer),
    // nor a key (ExcludeFocus).
    video.update(Message::Controls(Command::PlayOrPause), &players, true);
    video.update(Message::Key(Shortcut::Pause), &players, true);
    assert_eq!(handle.pauses(), 0);
    // Nor do they reach the controls.
    {
        let mut ui = simulator(container(video.view(true)));
        ui.point_at(FULLSCREEN_BUTTON);
        let _ = ui.simulate(click());
        assert!(ui.into_messages().next().is_none());
    }
    // ...but whatever plays it then is paused on the next poll.
    video.update(Message::Tick, &players, true);
    assert_eq!(handle.pauses(), 1);
    assert!(!video.is_playing());
}

#[test]
fn the_pause_signal_pauses_a_started_video() {
    let players = FakeMediaPlayers::new();
    let mut idle = TechniqueVideoPlayer::new(CLIP);
    idle.pause_signal(); // nothing to pause yet
    let (mut video, handle) = started(&players);
    video.pause_signal();
    assert_eq!(handle.pauses(), 1);
}

#[test]
fn frames_are_uploaded_only_when_they_change() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    assert_eq!(video.frame_revision(), None);
    handle.push_frame(2, 1, vec![255; 8]);
    video.update(Message::Tick, &players, false);
    assert_eq!(video.frame_revision(), Some(1));
    let first = video.frame.as_ref().map(|f| f.handle.id());
    video.update(Message::Tick, &players, false);
    assert_eq!(
        video.frame.as_ref().map(|f| f.handle.id()),
        first,
        "the same image: nothing re-uploaded"
    );
    handle.push_frame(2, 1, vec![0; 8]);
    video.update(Message::Tick, &players, false);
    assert_eq!(video.frame_revision(), Some(2));
    assert_ne!(video.frame.as_ref().map(|f| f.handle.id()), first);
}

#[test]
fn the_controls_drive_the_player() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    handle.set_state(|s| s.duration = Duration::from_secs(10));
    video.update(Message::Tick, &players, false);
    assert_eq!(video.playback().duration, Duration::from_secs(10));
    assert_eq!(video.playback().volume, FULL_VOLUME, "a new player's");
    video.update(
        Message::Controls(Command::Seek(Duration::from_secs(5))),
        &players,
        false,
    );
    assert_eq!(handle.seeks(), vec![Duration::from_secs(5)]);
    assert_eq!(video.playback().position, Duration::from_secs(5));
    video.update(Message::Controls(Command::PlayOrPause), &players, false);
    assert_eq!(handle.pauses(), 1, "it opened playing");
    video.update(Message::Controls(Command::SetVolume(30.0)), &players, false);
    assert_eq!(handle.volumes(), vec![30.0]);
    assert_eq!(video.playback().volume, 30.0);
    // The poll brings the rest of what the controls show.
    handle.set_state(|s| {
        s.buffering = true;
        s.buffer = Duration::from_secs(8);
        s.completed = true;
    });
    video.update(Message::Tick, &players, false);
    let shown = video.playback();
    assert!(shown.buffering && shown.completed);
    assert_eq!(shown.buffer, Duration::from_secs(8));
}

/// A key press with no modifier.
fn key(key: keyboard::Key) -> keyboard::Event {
    keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    }
}

fn named(named: Named) -> keyboard::Event {
    key(keyboard::Key::Named(named))
}

fn letter(c: &str) -> keyboard::Event {
    key(keyboard::Key::Character(c.into()))
}

#[test]
fn each_shortcut_drives_the_player() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    handle.set_state(|s| {
        s.duration = Duration::from_secs(30);
        s.position = Duration::from_secs(5);
    });
    let mut press = |event: keyboard::Event| {
        let message = key_message(&event).expect("a shortcut");
        video.update(message, &players, false)
    };
    // Space and the media keys.
    press(named(Named::Space));
    assert_eq!(
        (handle.plays(), handle.pauses()),
        (0, 1),
        "it opened playing"
    );
    press(named(Named::MediaPlay));
    assert_eq!(handle.plays(), 1);
    press(named(Named::MediaPause));
    assert_eq!(handle.pauses(), 2);
    press(named(Named::MediaPlayPause));
    assert_eq!(handle.plays(), 2);
    press(named(Named::MediaTrackNext));
    press(named(Named::MediaTrackPrevious));
    assert_eq!(
        (handle.plays(), handle.pauses()),
        (2, 2),
        "one clip: nothing"
    );
    assert!(handle.seeks().is_empty());
    // The seeks, from where it is now.
    press(named(Named::ArrowRight));
    press(named(Named::ArrowLeft));
    press(letter("i"));
    press(letter("j"));
    // From 5 s, 10 s back is mpv's 5 s before the end.
    press(letter("j"));
    let s = Duration::from_secs;
    assert_eq!(handle.seeks(), vec![s(7), s(5), s(15), s(5), s(25)]);
    // The volume, in steps of 5 within 0–100.
    press(named(Named::ArrowUp));
    press(named(Named::ArrowDown));
    press(named(Named::ArrowDown));
    assert_eq!(handle.volumes(), vec![100.0, 95.0, 90.0]);
    // Fullscreen: F toggles, Escape leaves (and does nothing outside it).
    assert_eq!(press(letter("f")), Some(WindowRequest::EnterFullscreen));
    assert_eq!(
        press(named(Named::Escape)),
        Some(WindowRequest::ExitFullscreen)
    );
    assert_eq!(press(named(Named::Escape)), None);
    assert_eq!(press(letter("F")), Some(WindowRequest::EnterFullscreen));
    assert_eq!(press(letter("f")), Some(WindowRequest::ExitFullscreen));
    // Anything else is no shortcut.
    assert_eq!(key_message(&letter("a")), None);
}

#[test]
fn the_fullscreen_button_enters_fullscreen_and_escape_leaves() {
    let players = FakeMediaPlayers::new();
    let (mut video, _handle) = started(&players);
    assert!(video.fullscreen_view().is_none());
    let sent = clicked(&video, false, FULLSCREEN_BUTTON);
    assert_eq!(sent, vec![Message::Controls(Command::ToggleFullscreen)]);
    let request = video.update(sent[0].clone(), &players, false);
    assert_eq!(request, Some(WindowRequest::EnterFullscreen));
    assert_eq!(
        WindowRequest::EnterFullscreen.mode(),
        window::Mode::Fullscreen
    );
    assert!(video.is_fullscreen());
    // The fullscreen view takes every key: its shortcuts, and the rest.
    let sent: Vec<Message> = {
        let mut ui = simulator(video.fullscreen_view().expect("in fullscreen"));
        let held = ui.tap_key(keyboard::Key::Character("a".into()));
        assert_eq!(held, iced::event::Status::Captured, "no key gets past it");
        let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
        ui.into_messages().collect()
    };
    assert_eq!(sent, vec![Message::Key(Shortcut::ExitFullscreen)]);
    let request = video.update(sent[0].clone(), &players, false);
    assert_eq!(request, Some(WindowRequest::ExitFullscreen));
    assert_eq!(WindowRequest::ExitFullscreen.mode(), window::Mode::Windowed);
    assert!(!video.is_fullscreen() && video.fullscreen_view().is_none());
    // Its own fullscreen button leaves too (at the window's bottom right).
    video.update(Message::Key(Shortcut::ToggleFullscreen), &players, false);
    let sent: Vec<Message> = {
        let mut ui = simulator(video.fullscreen_view().expect("in fullscreen"));
        ui.point_at(Point::new(1024.0 - 16.0 - 22.0, 768.0 - 28.0));
        let _ = ui.simulate(click());
        ui.into_messages().collect()
    };
    assert_eq!(sent, vec![Message::Controls(Command::ToggleFullscreen)]);
    let request = video.update(sent[0].clone(), &players, false);
    assert_eq!(request, Some(WindowRequest::ExitFullscreen));
}

// The fullscreen view lies over the window's title-bar band, whose presses
// nothing over it takes are the window's (the app's backdrop around the
// page's overlay): a double press on the picture there left fullscreen and
// zoomed the window too.
#[test]
fn a_double_press_on_the_fullscreen_picture_in_the_band_zooms_no_window() {
    use crate::ui::core::widgets::toolbar::{self, drag_region, window_backdrop};
    use iced::time::Instant;
    use std::time::Duration;
    #[derive(Debug, Clone)]
    enum Window {
        Video(Message),
        Drag,
        Maximize,
    }
    /// The view as the app shows it.
    fn view(video: &TechniqueVideoPlayer) -> Element<'_, Window> {
        let page = video.fullscreen_view().expect("in fullscreen");
        drag_region(
            window_backdrop(page.map(Window::Video)),
            Window::Drag,
            Window::Maximize,
        )
    }
    let players = FakeMediaPlayers::new();
    let (mut video, _handle) = started(&players);
    video.update(Message::Key(Shortcut::ToggleFullscreen), &players, false);
    // A tap, then the frame after it: the video times its taps by the
    // frames, the window by the clock.
    let tap_then_frame = |at: Instant| {
        let mut events = click();
        events.push(Event::Window(iced::window::Event::RedrawRequested(at)));
        events
    };
    let mut ui = simulator(view(&video));
    ui.point_at(Point::new(512.0, toolbar::HEIGHT / 2.0));
    let t0 = Instant::now();
    let _ = ui.simulate([Event::Window(iced::window::Event::RedrawRequested(t0))]);
    let _ = ui.simulate(tap_then_frame(t0 + Duration::from_millis(600)));
    std::thread::sleep(Duration::from_millis(60));
    let _ = ui.simulate(tap_then_frame(t0 + Duration::from_millis(800)));
    let sent: Vec<Window> = ui.into_messages().collect();
    assert!(
        matches!(
            sent.as_slice(),
            [Window::Video(Message::Controls(Command::ToggleFullscreen))]
        ),
        "{sent:?}"
    );
}

#[test]
fn a_take_does_not_block_the_fullscreen_controls() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    video.update(Message::Key(Shortcut::ToggleFullscreen), &players, false);
    // The fullscreen route was not under the inline video's IgnorePointer.
    video.update(
        Message::Controls(Command::Seek(Duration::from_secs(2))),
        &players,
        true,
    );
    assert_eq!(handle.seeks(), vec![Duration::from_secs(2)]);
    // Play still waits for the take, and what plays is paused again.
    video.update(Message::Controls(Command::PlayOrPause), &players, true);
    assert_eq!(handle.pauses(), 1, "pause stays live");
    video.update(Message::Controls(Command::PlayOrPause), &players, true);
    video.update(Message::Key(Shortcut::Play), &players, true);
    assert_eq!(handle.plays(), 0);
    handle.set_playing(true);
    video.update(Message::Tick, &players, true);
    assert_eq!(handle.pauses(), 2);
    assert_eq!(
        video.update(Message::Key(Shortcut::ExitFullscreen), &players, true),
        Some(WindowRequest::ExitFullscreen)
    );
}

#[test]
fn the_box_is_16_by_9_and_at_most_520_wide() {
    let size = aspect_size(Size::new(518.0, f32::INFINITY), ASPECT_RATIO);
    assert_eq!(size, Size::new(518.0, 291.375));
    let tight = aspect_size(Size::new(520.0, 90.0), ASPECT_RATIO);
    assert_eq!(tight, Size::new(160.0, 90.0));
    let unbounded = aspect_size(Size::new(f32::INFINITY, 90.0), ASPECT_RATIO);
    assert_eq!(unbounded, Size::new(160.0, 90.0));

    // The video sits inside the 1 px border: 518 × 291.375 at (1, 1).
    let video = TechniqueVideoPlayer::new(MISSING);
    let mut ui = simulator(container(video.view(false)));
    let message = ui.find(UNAVAILABLE_CLIP_MESSAGE).expect("shown").bounds();
    let centre = message.center();
    assert!((centre.x - 260.0).abs() < 1.0, "{centre:?}");
    assert!((centre.y - 146.6875).abs() < 1.0, "{centre:?}");
}
