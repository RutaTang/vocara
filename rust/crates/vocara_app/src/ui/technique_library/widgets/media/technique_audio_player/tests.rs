// The inline audio player over a fake media player (a test cannot run
// AVFoundation, as a Dart widget test could not initialize media_kit): every
// player built is released, a clip whose file is not on this device renders
// as unavailable, and the bar drives the player as the Dart one did.

use super::*;
use iced::widget::{column, container};
use iced::Point;
use iced_test::simulator;
use vocara_services::media_player::{
    FakeMediaPlayer, FakeMediaPlayers, FakePlayerHandle, NullMediaPlayer, Result as PlayerResult,
};

/// The placeholder `asset://` path the repository hydrates an example to when
/// its pooled bytes are missing on this device.
const MISSING: &str = "asset://01J00000000000000000000000";
const CLIP: &str = "/tmp/example.wav";

/// A player whose fake reports a loaded 10 s clip.
fn loaded() -> (TechniqueAudioPlayer, FakePlayerHandle, FakeMediaPlayers) {
    let players = FakeMediaPlayers::new();
    let mut audio = TechniqueAudioPlayer::new(CLIP, &players);
    let handle = players.built().pop().expect("a player was built");
    handle.set_state(|s| s.duration = Duration::from_secs(10));
    let _ = audio.update(Message::Tick, false);
    (audio, handle, players)
}

/// The bar's scrubber spans x 34..928 of the 1024 px window (4 padding, the
/// 24 px play button, 6, then 10, the 76 px readout and 10 on the right),
/// centred on the 32 px row.
const SCRUB_LEFT: f32 = 34.0;
const SCRUB_WIDTH: f32 = 894.0;
const ROW_MID: f32 = 16.0;

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}
fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}
fn move_to(x: f32, y: f32) -> Event {
    Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(x, y),
    })
}

/// What the view publishes for `events`, the pointer starting at `at`.
fn messages(
    audio: &TechniqueAudioPlayer,
    play_blocked: bool,
    at: Point,
    events: Vec<Event>,
) -> Vec<Message> {
    let mut ui = simulator(audio.view(play_blocked));
    ui.point_at(at);
    let _ = ui.simulate(events);
    ui.into_messages().collect()
}

// Regression (Dart): media_kit read `asset://` as a bundled Flutter asset
// and threw AFTER the player was built; the catch dropped the reference, so
// every mount leaked a native player nothing disposed.
#[test]
fn an_audio_example_whose_file_is_missing_shows_as_unavailable_and_leaks_no_player() {
    let players = FakeMediaPlayers::new();
    let audio = TechniqueAudioPlayer::new(MISSING, &players);
    {
        let mut ui = simulator(audio.view(false));
        assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_ok());
        assert!(UNAVAILABLE_CLIP_MESSAGE.contains("isn't on this device"));
    }
    assert!(audio.is_unavailable() && !audio.has_player());
    drop(audio);
    assert!(
        players.built().iter().all(FakePlayerHandle::disposed),
        "a player built for the clip must be released with it"
    );
}

#[test]
fn a_clip_that_fails_to_open_releases_its_player_and_reads_a_static_zero() {
    let players = FakeMediaPlayers::failing_open();
    let audio = TechniqueAudioPlayer::new(CLIP, &players);
    let built = players.built();
    assert_eq!(built.len(), 1);
    assert!(built[0].disposed(), "released at once, not leaked");
    // Dart's catch: no player, the readout degrades to a static 0:00.
    assert!(!audio.has_player() && !audio.is_unavailable());
    assert!(!audio.needs_tick());
    let mut ui = simulator(audio.view(false));
    assert!(ui.find("0:00 / 0:00").is_ok());
}

/// A factory of null players: a build without media playback.
struct NullPlayers;

impl MediaPlayerFactory for NullPlayers {
    fn create(&self, _: MediaKind) -> PlayerResult<Box<dyn MediaPlayer>> {
        Ok(Box::new(NullMediaPlayer))
    }
}

#[test]
fn a_build_without_playback_shows_every_clip_unavailable() {
    let audio = TechniqueAudioPlayer::new(CLIP, &NullPlayers);
    assert!(audio.is_unavailable() && !audio.has_player());
    let mut ui = simulator(audio.view(false));
    assert!(ui.find(UNAVAILABLE_CLIP_MESSAGE).is_ok());
}

#[test]
fn the_clip_opens_paused_so_its_duration_shows_before_the_first_play() {
    let (audio, handle, _players) = loaded();
    assert_eq!(handle.kind(), MediaKind::Audio);
    assert_eq!(handle.opened(), vec![(CLIP.to_string(), false)]);
    assert_eq!(handle.plays(), 0);
    assert_eq!(audio.duration(), Duration::from_secs(10));
    let mut ui = simulator(audio.view(false));
    assert!(ui.find("0:00 / 0:10").is_ok());
}

#[test]
fn the_disc_plays_and_pauses() {
    let (mut audio, handle, _players) = loaded();
    let disc = Point::new(25.0, ROW_MID);
    let sent = messages(&audio, false, disc, vec![press(), release()]);
    assert_eq!(sent, vec![Message::PlayOrPause]);
    for m in sent {
        let _ = audio.update(m, false);
    }
    assert_eq!(handle.plays(), 1);
    assert!(audio.is_playing(), "playing shows at once");
    let _ = audio.update(Message::PlayOrPause, false);
    assert_eq!(handle.pauses(), 1);
    assert!(!audio.is_playing());
}

// Regression (Dart): an example played on into the take compared against
// it, paused only at the arm.
#[test]
fn play_waits_for_the_take_to_end_but_pause_stays_live() {
    let (mut audio, handle, _players) = loaded();
    let disc = Point::new(25.0, ROW_MID);
    assert!(
        messages(&audio, true, disc, vec![press(), release()]).is_empty(),
        "the disc is off"
    );
    // A press queued before the block lands after it: still no play.
    let _ = audio.update(Message::PlayOrPause, true);
    assert_eq!(handle.plays(), 0);

    let _ = audio.update(Message::PlayOrPause, false);
    assert!(audio.is_playing());
    assert_eq!(
        messages(&audio, true, disc, vec![press(), release()]),
        vec![Message::PlayOrPause],
        "Pause stays live"
    );
    let _ = audio.update(Message::PlayOrPause, true);
    assert_eq!(handle.pauses(), 1);
}

// Dart: 'a player started by any means while a take records is paused
// again'. A pointer block left the video controls' own Space shortcut live:
// it started a video into the take.
#[test]
fn a_player_started_by_any_means_while_a_take_records_is_paused_again() {
    let (mut player, handle) = FakeMediaPlayer::new(MediaKind::Audio, false);
    handle.set_playing(true);
    let state = player.state();
    assert!(pause_while_play_blocked(&mut player, &state, true));
    assert_eq!(handle.pauses(), 1);

    handle.set_playing(true);
    let state = player.state();
    assert!(!pause_while_play_blocked(&mut player, &state, false));
    assert_eq!(handle.pauses(), 1, "no take: it plays");

    // The audio player holds to it too: started behind its back, it is
    // paused on the next poll.
    let (mut audio, handle, _players) = loaded();
    handle.set_playing(true);
    let _ = audio.update(Message::Tick, true);
    assert_eq!(handle.pauses(), 1);
    assert!(!audio.is_playing());
}

#[test]
fn the_pause_signal_pauses_where_it_is() {
    let (mut audio, handle, _players) = loaded();
    let _ = audio.update(Message::PlayOrPause, false);
    audio.pause_signal();
    assert_eq!(handle.pauses(), 1);
    assert!(!audio.is_playing());
}

#[test]
fn playback_drives_the_analysis_only_while_playing() {
    let (mut audio, handle, _players) = loaded();
    // Paused: the readout follows, the analysis does not.
    handle.set_state(|s| s.position = Duration::from_secs(2));
    assert_eq!(audio.update(Message::Tick, false), None);
    assert_eq!(audio.position(), Duration::from_secs(2));

    let _ = audio.update(Message::PlayOrPause, false);
    handle.set_state(|s| s.position = Duration::from_secs(3));
    assert_eq!(
        audio.update(Message::Tick, false),
        Some(Duration::from_secs(3))
    );
    // No move, no event.
    assert_eq!(audio.update(Message::Tick, false), None);
    let mut ui = simulator(audio.view(false));
    assert!(ui.find("0:03 / 0:10").is_ok());
}

#[test]
fn a_drag_holds_the_bar_against_position_events() {
    let (mut audio, handle, _players) = loaded();
    let _ = audio.update(Message::PlayOrPause, false);
    let _ = audio.update(Message::ChangeStart, false);
    let _ = audio.update(Message::Changed(0.5), false);
    handle.set_state(|s| s.position = Duration::from_secs(1));
    assert_eq!(audio.update(Message::Tick, false), None, "dropped");
    let mut ui = simulator(audio.view(false));
    assert!(ui.find("0:05 / 0:10").is_ok(), "the readout shows the drag");
}

#[test]
fn a_tap_on_the_bar_seeks_there_and_tells_the_analysis_even_paused() {
    let (mut audio, handle, _players) = loaded();
    let at = Point::new(SCRUB_LEFT + SCRUB_WIDTH * 0.25, ROW_MID);
    let sent = messages(&audio, false, at, vec![press(), release()]);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0], Message::ChangeStart);
    assert!(matches!(sent[1], Message::Changed(f) if (f - 0.25).abs() < 0.01));
    assert!(matches!(sent[2], Message::ChangeEnd(Some(f)) if (f - 0.25).abs() < 0.01));
    let reports: Vec<_> = sent
        .into_iter()
        .filter_map(|m| audio.update(m, false))
        .collect();
    assert_eq!(reports.len(), 1);
    let target = reports[0];
    assert!((target.as_millis() as i64 - 2500).abs() <= 15, "{target:?}");
    assert_eq!(handle.seeks(), vec![target]);
    assert_eq!(audio.position(), target, "the thumb stays where released");
}

#[test]
fn a_drag_follows_the_pointer_and_seeks_on_release() {
    let (mut audio, handle, _players) = loaded();
    let x = |f: f32| SCRUB_LEFT + SCRUB_WIDTH * f;
    let sent = messages(
        &audio,
        false,
        Point::new(x(0.1), ROW_MID),
        vec![
            press(),
            move_to(x(0.1) + 3.0, ROW_MID),
            move_to(x(0.6), ROW_MID),
            move_to(x(0.75), ROW_MID),
            release(),
        ],
    );
    assert_eq!(sent.first(), Some(&Message::ChangeStart), "no tap first");
    assert_eq!(sent.last(), Some(&Message::ChangeEnd(None)));
    assert!(matches!(sent[sent.len() - 2], Message::Changed(f) if (f - 0.75).abs() < 0.01));
    let reports: Vec<_> = sent
        .into_iter()
        .filter_map(|m| audio.update(m, false))
        .collect();
    assert_eq!(reports.len(), 1);
    assert!((reports[0].as_millis() as i64 - 7500).abs() <= 15);
    assert_eq!(handle.seeks(), reports);
}

#[test]
fn a_press_that_wanders_off_vertically_is_no_tap() {
    let (audio, _handle, _players) = loaded();
    let at = Point::new(SCRUB_LEFT + 100.0, ROW_MID);
    let sent = messages(
        &audio,
        false,
        at,
        vec![press(), move_to(at.x, at.y + 30.0), release()],
    );
    assert!(sent.is_empty(), "{sent:?}");
}

#[test]
fn a_scrub_before_the_duration_loads_goes_nowhere() {
    let players = FakeMediaPlayers::new();
    let mut audio = TechniqueAudioPlayer::new(CLIP, &players);
    assert_eq!(audio.update(Message::ChangeEnd(Some(0.5)), false), None);
    assert!(players.built()[0].seeks().is_empty());
}

#[test]
fn the_seek_signal_moves_playback_without_echoing_it_back() {
    let (mut audio, handle, _players) = loaded();
    audio.seek_signal(Duration::from_millis(4200));
    assert_eq!(handle.seeks(), vec![Duration::from_millis(4200)]);
    assert_eq!(audio.position(), Duration::from_millis(4200));
    // The seek's own position event while paused is not reported back.
    assert_eq!(audio.update(Message::Tick, false), None);
}

#[test]
fn it_is_polled_only_while_loading_playing_or_settling_a_command() {
    let players = FakeMediaPlayers::new();
    let mut audio = TechniqueAudioPlayer::new(CLIP, &players);
    let handle = players.built().pop().expect("built");
    handle.set_state(|s| s.loading = true);
    let _ = audio.update(Message::Tick, false);
    assert!(audio.needs_tick(), "until the duration loads");
    handle.set_state(|s| {
        s.loading = false;
        s.duration = Duration::from_secs(10);
    });
    let _ = audio.update(Message::Tick, false);
    assert!(!audio.needs_tick(), "loaded and paused: idle");

    let _ = audio.update(Message::PlayOrPause, false);
    assert!(audio.needs_tick());
    // Played to the end: it stops by itself, and the polls stop with it.
    handle.set_state(|s| {
        s.playing = false;
        s.completed = true;
        s.position = s.duration;
    });
    for _ in 0..=SETTLE_TICKS {
        let _ = audio.update(Message::Tick, false);
    }
    assert!(!audio.needs_tick());
    assert_eq!(audio.position(), Duration::from_secs(10));
}

#[test]
fn the_readout_and_the_bar_read_like_dart() {
    assert_eq!(format_clock(Duration::ZERO), "0:00");
    assert_eq!(format_clock(Duration::from_millis(65_999)), "1:05");
    assert_eq!(format_clock(Duration::from_secs(3661)), "61:01");
    assert_eq!(fraction_at(-5.0, 100.0), 0.0);
    assert_eq!(fraction_at(150.0, 100.0), 1.0);
    assert_eq!(fraction_at(25.0, 100.0), 0.25);
    assert_eq!(fraction_at(10.0, 0.0), 0.0, "no width, no NaN");
    // The thumb centres on the fill's end and stays on the track.
    assert_eq!(thumb_left(0.5, 100.0), 45.0);
    assert_eq!(thumb_left(0.0, 100.0), 0.0);
    assert_eq!(thumb_left(1.0, 100.0), 90.0);
    assert_eq!(thumb_left(0.5, 4.0), 0.0);
}

#[test]
fn the_unavailable_row_keeps_the_players_footprint() {
    let players = FakeMediaPlayers::new();
    let live = TechniqueAudioPlayer::new(CLIP, &players);
    let missing = TechniqueAudioPlayer::new(MISSING, &players);
    let mut ui = simulator(column![
        container(live.view(false)),
        container(missing.view(false))
    ]);
    let message = ui.find(UNAVAILABLE_CLIP_MESSAGE).expect("shown").bounds();
    // The second row starts a row (HEIGHT) down and is as tall.
    assert!(message.y > HEIGHT && message.y + message.height < 2.0 * HEIGHT);
}
