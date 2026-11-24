// The page-level cases of Dart's technique_example_player_test.dart, on the
// keyed player set the technique page keeps (the page itself is ported
// elsewhere): players survive scrolling and earlier removals, a clip whose
// file arrives gets a player, and a take pauses and blocks every example;
// and the video controls' keyboard and fullscreen.

use super::*;
use iced::keyboard::{key::Named, Key};
use iced::widget::{column, container, scrollable, Space};
use iced::{mouse, Event, Length, Point};
use iced_test::simulator;
use vocara_services::media_player::{FakeMediaPlayers, FakePlayerHandle};

use crate::ui::core::widgets::wrap::Wrap;
use crate::ui::technique_library::widgets::media::technique_audio_player::UNAVAILABLE_CLIP_MESSAGE;
use crate::ui::technique_library::widgets::media::video_controls::{Command, Shortcut};

const MISSING: &str = "asset://01J00000000000000000000000";

fn clip<'a>(id: &'a str, path: &'a str) -> ExampleClip<'a> {
    ExampleClip {
        id,
        path,
        video: false,
    }
}

/// The two audio examples M0 and M1 of Dart's `_technique()`.
fn two() -> [ExampleClip<'static>; 2] {
    [
        clip("M0", "/tmp/example-0.wav"),
        clip("M1", "/tmp/example-1.wav"),
    ]
}

fn key(id: &str) -> ExampleKey {
    ExampleKey {
        id: id.to_string(),
        unavailable: false,
        video: false,
    }
}

// Regression (Dart): the page was a lazy ListView, so once the Examples block
// scrolled past the cache extent its players were disposed — a clip followed
// while reading the cues stopped, and came back at 0:00.
#[test]
fn scrolling_the_examples_out_of_view_keeps_their_players() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(two(), &players);
    assert_eq!(players.built().len(), 2);
    examples.seek_analyzed("M1", Duration::from_secs(3)); // somewhere past 0:00

    {
        // The examples under a description long enough to scroll them far
        // out of view, then back.
        let page = scrollable(column![
            examples.view("M0", false).expect("M0"),
            examples.view("M1", false).expect("M1"),
            Space::new().height(6000),
        ])
        .height(Length::Fill);
        let mut ui = simulator(container(page));
        ui.point_at(Point::new(500.0, 300.0));
        let wheel = |y| {
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y },
            })
        };
        let _ = ui.simulate([wheel(-6000.0), wheel(6000.0)]);
    }

    let built = players.built();
    assert_eq!(built.len(), 2, "no fresh player");
    assert!(!built.iter().any(FakePlayerHandle::disposed));
    match examples.get("M1") {
        Some(ExamplePlayer::Audio(p)) => {
            assert_eq!(p.position(), Duration::from_secs(3), "not back at 0:00")
        }
        other => panic!("M1's player: {other:?}"),
    }
}

// Regression (Dart): the player read "not on this device" once, at mount:
// the same clip handed its real file by a reload kept saying so.
#[test]
fn a_clip_whose_file_arrives_in_a_reload_gets_its_player() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(
        [clip("M0", "/tmp/example-0.wav"), clip("M1", MISSING)],
        &players,
    );
    {
        let mut ui = simulator(examples.view("M1", false).expect("M1"));
        assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_ok());
    }
    assert_eq!(players.built().len(), 1);

    examples.sync(two(), &players); // its bytes arrived
    {
        let mut ui = simulator(examples.view("M1", false).expect("M1"));
        assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_err());
    }
    let built = players.built();
    assert_eq!(built.len(), 2);
    assert!(!built[0].disposed(), "M0 kept its player");
}

// Regression (Dart): the cards were unkeyed, so removing one handed every
// later card's slot to the next clip and rebuilt its player — the example
// playing below the removed one stopped.
#[test]
fn removing_an_earlier_example_keeps_a_later_cards_player() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(two(), &players);
    let _ = examples.update(
        Message::Audio(key("M1"), audio::Message::PlayOrPause),
        &players,
        false,
        None,
    );

    examples.sync([clip("M1", "/tmp/example-1.wav")], &players);

    let built = players.built();
    assert_eq!(built.len(), 2, "M1 was not rebuilt");
    assert!(built[0].disposed(), "only M0");
    assert!(!built[1].disposed());
    assert_eq!(examples.len(), 1);
    match examples.get("M1") {
        Some(ExamplePlayer::Audio(p)) => assert!(p.is_playing(), "still playing"),
        other => panic!("M1's player: {other:?}"),
    }
}

// Regression (Dart): the examples play outside the engine, so its echo
// canceller has no reference to subtract — a clip left playing on speakers
// was recorded into the take it was compared with.
#[test]
fn starting_a_practice_take_pauses_every_example_player() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(two(), &players);
    assert_eq!(players.built().len(), 2);

    examples.pause_all(); // micArmed

    assert!(players.built().iter().all(|p| p.pauses() > 0));

    // Nor can one be started again until the take ends: paused only at the
    // arm, an example played on into the take.
    {
        let mut ui = simulator(examples.view("M0", true).expect("M0"));
        ui.point_at(Point::new(25.0, 24.0));
        let _ = ui.simulate(iced_test::simulator::click());
        assert!(ui.into_messages().next().is_none(), "Play is off");
    }
    let _ = examples.update(
        Message::Audio(key("M0"), audio::Message::PlayOrPause),
        &players,
        true,
        None,
    );
    assert!(players.built().iter().all(|p| p.plays() == 0));
}

#[test]
fn only_the_analyzed_clip_drives_and_follows_the_analysis() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(two(), &players);
    let built = players.built();
    for (id, handle) in ["M0", "M1"].into_iter().zip(&built) {
        handle.set_state(|s| s.duration = Duration::from_secs(10));
        let _ = examples.update(
            Message::Audio(key(id), audio::Message::PlayOrPause),
            &players,
            false,
            Some("M1"),
        );
    }
    built[0].set_state(|s| s.position = Duration::from_secs(1));
    built[1].set_state(|s| s.position = Duration::from_secs(2));
    assert_eq!(
        examples.update(Message::Tick, &players, false, Some("M1")),
        Some(Report::Position(Duration::from_secs(2)))
    );
    // A scrub of M0's bar is not M1's to report.
    assert_eq!(
        examples.update(
            Message::Audio(key("M0"), audio::Message::ChangeEnd(Some(0.5))),
            &players,
            false,
            Some("M1"),
        ),
        None
    );
    examples.seek_analyzed("M1", Duration::from_secs(4));
    assert_eq!(built[1].seeks(), vec![Duration::from_secs(4)]);
    assert_eq!(
        built[0].seeks(),
        vec![Duration::from_secs(5)],
        "its own scrub only"
    );
}

#[test]
fn a_message_from_a_replaced_player_reaches_nothing() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync([clip("M1", MISSING)], &players);
    examples.sync([clip("M1", "/tmp/example-1.wav")], &players);
    let stale = ExampleKey {
        id: "M1".to_string(),
        unavailable: true,
        video: false,
    };
    let _ = examples.update(
        Message::Audio(stale, audio::Message::PlayOrPause),
        &players,
        false,
        None,
    );
    assert_eq!(players.built()[0].plays(), 0);
}

#[test]
fn a_video_example_waits_for_its_first_play() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(
        [ExampleClip {
            id: "V",
            path: "/tmp/example.mp4",
            video: true,
        }],
        &players,
    );
    assert!(players.built().is_empty());
    assert!(!examples.needs_tick());
    let video_key = ExampleKey {
        id: "V".to_string(),
        unavailable: false,
        video: true,
    };
    let _ = examples.update(
        Message::Video(video_key, video::Message::Play),
        &players,
        false,
        None,
    );
    assert_eq!(players.built().len(), 1);
    assert!(examples.needs_tick(), "playing: polled");
    examples.sync([], &players);
    assert!(players.built()[0].disposed());
    assert!(examples.is_empty());
}

#[test]
fn an_idle_page_is_not_polled() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync(two(), &players);
    // The fakes load at once and sit paused.
    assert!(!examples.needs_tick());
    let _ = examples.update(
        Message::Audio(key("M0"), audio::Message::PlayOrPause),
        &players,
        false,
        None,
    );
    assert!(examples.needs_tick());
}

// ------------------------------------------- the video controls' keyboard

fn video_clip(id: &'static str) -> ExampleClip<'static> {
    ExampleClip {
        id,
        path: "/tmp/example.mp4",
        video: true,
    }
}

fn video_key(id: &str) -> ExampleKey {
    ExampleKey {
        id: id.to_string(),
        unavailable: false,
        video: true,
    }
}

/// Video `id`'s message `m`.
fn to_video(
    examples: &mut ExamplePlayers,
    players: &FakeMediaPlayers,
    id: &str,
    m: video::Message,
) -> Option<Report> {
    examples.update(Message::Video(video_key(id), m), players, false, None)
}

/// What the page sends for a Space none of its widgets took, with
/// `content` in it.
fn space_through<'a>(
    examples: &'a ExamplePlayers,
    content: Element<'a, Message>,
    play_blocked: bool,
) -> Vec<Message> {
    let page = examples.shortcuts(content, play_blocked, |m| m);
    let mut ui = simulator(page);
    let _ = ui.tap_key(Key::Named(Named::Space));
    ui.into_messages().collect()
}

#[test]
fn the_first_video_started_holds_the_keyboard_until_a_take_takes_it() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync([video_clip("V1"), video_clip("V2")], &players);
    assert_eq!(examples.keyboard_holder(), None, "nothing started");
    to_video(&mut examples, &players, "V1", video::Message::Play);
    assert_eq!(examples.keyboard_holder(), Some(&video_key("V1")));
    // V2's autofocus found V1's controls focused.
    to_video(&mut examples, &players, "V2", video::Message::Play);
    assert_eq!(examples.keyboard_holder(), Some(&video_key("V1")));
    // A take: ExcludeFocus took the focus, and nothing gave it back.
    examples.set_play_blocked(false);
    assert_eq!(examples.keyboard_holder(), Some(&video_key("V1")));
    examples.set_play_blocked(true);
    examples.set_play_blocked(false);
    assert_eq!(examples.keyboard_holder(), None);
}

#[test]
fn the_keyboard_goes_with_its_video() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync([video_clip("V1"), video_clip("V2")], &players);
    to_video(&mut examples, &players, "V1", video::Message::Play);
    examples.sync([video_clip("V2")], &players);
    assert_eq!(examples.keyboard_holder(), None);
}

#[test]
fn the_page_hands_the_keys_nothing_took_to_the_video_holding_the_keyboard() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync([video_clip("V1"), video_clip("V2")], &players);
    // No video started: no controls to hear it.
    assert!(space_through(&examples, Space::new().into(), false).is_empty());
    to_video(&mut examples, &players, "V1", video::Message::Play);
    to_video(&mut examples, &players, "V2", video::Message::Play);
    let space = vec![Message::Video(
        video_key("V1"),
        video::Message::Key(Shortcut::PlayOrPause),
    )];
    assert_eq!(space_through(&examples, Space::new().into(), false), space);
    // Not while a take blocks them (ExcludeFocus).
    assert!(space_through(&examples, Space::new().into(), true).is_empty());
    // A key a widget of the page took (a focused field) is its own.
    let field = Wrap::<Message, ()>::new(
        Space::new(),
        Box::new(|_, event, _, _, shell| {
            if matches!(event, Event::Keyboard(_)) {
                shell.capture_event();
            }
        }),
    );
    assert!(space_through(&examples, field.into(), false).is_empty());
    // A video in fullscreen takes its keys in its own view.
    to_video(
        &mut examples,
        &players,
        "V1",
        video::Message::Key(Shortcut::ToggleFullscreen),
    );
    assert!(space_through(&examples, Space::new().into(), false).is_empty());
}

#[test]
fn a_video_in_fullscreen_is_shown_over_the_window() {
    let players = FakeMediaPlayers::new();
    let mut examples = ExamplePlayers::new();
    examples.sync([video_clip("V1")], &players);
    to_video(&mut examples, &players, "V1", video::Message::Play);
    assert!(examples.overlay().is_none());
    let toggle = video::Message::Controls(Command::ToggleFullscreen);
    assert_eq!(
        to_video(&mut examples, &players, "V1", toggle.clone()),
        Some(Report::Window(WindowRequest::EnterFullscreen))
    );
    {
        // Its keys come back addressed to it.
        let mut ui = simulator(examples.overlay().expect("in fullscreen"));
        let _ = ui.tap_key(Key::Named(Named::Escape));
        let sent: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            sent,
            vec![Message::Video(
                video_key("V1"),
                video::Message::Key(Shortcut::ExitFullscreen)
            )]
        );
    }
    assert_eq!(
        to_video(&mut examples, &players, "V1", toggle),
        Some(Report::Window(WindowRequest::ExitFullscreen))
    );
    assert!(examples.overlay().is_none());
}
