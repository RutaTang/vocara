// The inline video player over a fake media player: lazy creation, the
// unavailable clip, the release of every player built, the blocked poster,
// and frames uploaded only when they change.

use super::*;
use iced::widget::container;
use iced::Point;
use iced_test::simulator;
use vocara_services::media_player::{FakeMediaPlayers, FakePlayerHandle};

const MISSING: &str = "asset://01J00000000000000000000000";
const CLIP: &str = "/tmp/example.mp4";

/// The poster's centre in the 1024 px window: the box is 520 × 292.5.
const POSTER: Point = Point::new(260.0, 146.0);

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
    // Blocked: controls take nothing, not even Pause (Dart's IgnorePointer).
    video.update(
        Message::Controls(audio::Message::PlayOrPause),
        &players,
        true,
    );
    assert_eq!(handle.pauses(), 0);
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
fn the_controls_show_while_hovered_and_drive_the_player() {
    let players = FakeMediaPlayers::new();
    let (mut video, handle) = started(&players);
    handle.set_state(|s| s.duration = Duration::from_secs(10));
    video.update(Message::Tick, &players, false);
    {
        let mut ui = simulator(container(video.view(false)));
        assert!(ui.find("0:00 / 0:10").is_err(), "hidden until hovered");
    }
    video.update(Message::Hover(true), &players, false);
    {
        let mut ui = simulator(container(video.view(false)));
        assert!(ui.find("0:00 / 0:10").is_ok());
    }
    {
        let mut ui = simulator(container(video.view(true)));
        assert!(ui.find("0:00 / 0:10").is_err(), "none while blocked");
    }
    video.update(
        Message::Controls(audio::Message::ChangeEnd(Some(0.5))),
        &players,
        false,
    );
    assert_eq!(handle.seeks(), vec![Duration::from_secs(5)]);
    video.update(
        Message::Controls(audio::Message::PlayOrPause),
        &players,
        false,
    );
    assert_eq!(handle.pauses(), 1, "it opened playing");
}

#[test]
fn the_box_is_16_by_9_and_at_most_520_wide() {
    let size = aspect_size(Size::new(520.0, f32::INFINITY), ASPECT_RATIO);
    assert_eq!(size, Size::new(520.0, 292.5));
    let tight = aspect_size(Size::new(520.0, 90.0), ASPECT_RATIO);
    assert_eq!(tight, Size::new(160.0, 90.0));
    let unbounded = aspect_size(Size::new(f32::INFINITY, 90.0), ASPECT_RATIO);
    assert_eq!(unbounded, Size::new(160.0, 90.0));

    let video = TechniqueVideoPlayer::new(MISSING);
    let mut ui = simulator(container(video.view(false)));
    let message = ui.find(UNAVAILABLE_CLIP_MESSAGE).expect("shown").bounds();
    let centre = message.center();
    assert!((centre.x - 260.0).abs() < 1.0 && (centre.y - 146.25).abs() < 1.0);
}
