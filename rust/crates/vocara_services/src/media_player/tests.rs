use super::*;

const V: u32 = CM_TIME_FLAG_VALID;

// ------------------------------------------------------------ CMTime → Duration

#[test]
fn a_valid_cm_time_reads_as_its_seconds() {
    assert_eq!(
        cm_time_to_duration(3, 2, V),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(
        cm_time_to_duration(1001, 30000, V),
        Some(Duration::from_nanos(33_366_666)),
        "rounded down to the nanosecond"
    );
    assert_eq!(cm_time_to_duration(0, 600, V), Some(Duration::ZERO));
    // Rounded and other informational flags do not matter.
    assert_eq!(
        cm_time_to_duration(600, 600, V | 1 << 1),
        Some(Duration::from_secs(1))
    );
}

#[test]
fn an_indefinite_duration_is_not_known_yet() {
    // An AVPlayerItem's duration until it loads: kCMTimeIndefinite.
    assert_eq!(cm_time_to_duration(0, 0, V | CM_TIME_FLAG_INDEFINITE), None);
    assert_eq!(cm_time_to_duration(5, 1, V | CM_TIME_FLAG_INDEFINITE), None);
}

#[test]
fn infinite_and_invalid_times_are_not_numbers() {
    assert_eq!(
        cm_time_to_duration(0, 0, V | CM_TIME_FLAG_POSITIVE_INFINITY),
        None
    );
    assert_eq!(
        cm_time_to_duration(0, 0, V | CM_TIME_FLAG_NEGATIVE_INFINITY),
        None
    );
    // kCMTimeInvalid: all zero, not even valid.
    assert_eq!(cm_time_to_duration(0, 0, 0), None);
    assert_eq!(cm_time_to_duration(600, 600, 0), None, "no valid flag");
}

#[test]
fn a_zero_or_negative_timescale_never_divides() {
    assert_eq!(cm_time_to_duration(10, 0, V), None);
    assert_eq!(cm_time_to_duration(10, -600, V), None);
}

#[test]
fn a_time_before_zero_reads_as_zero() {
    assert_eq!(cm_time_to_duration(-300, 600, V), Some(Duration::ZERO));
    assert_eq!(cm_time_to_duration(i64::MIN, 1, V), Some(Duration::ZERO));
}

#[test]
fn the_largest_times_do_not_overflow() {
    assert_eq!(
        cm_time_to_duration(i64::MAX, 1, V),
        Some(Duration::from_secs(i64::MAX as u64))
    );
    let d = cm_time_to_duration(i64::MAX, i32::MAX, V).expect("a number");
    assert_eq!(d.as_secs(), (i64::MAX as u64) / (i32::MAX as u64));
    // The remainder's nanoseconds stay below a second.
    let d = cm_time_to_duration(i32::MAX as i64 - 1, i32::MAX, V).expect("a number");
    assert_eq!(d.as_secs(), 0);
    assert!(d.subsec_nanos() < 1_000_000_000);
}

#[test]
fn a_seek_target_round_trips_through_cm_time() {
    for d in [
        Duration::ZERO,
        Duration::from_millis(1234),
        Duration::from_nanos(1),
        Duration::from_secs(3600 * 5),
    ] {
        let (value, timescale) = duration_to_cm_time(d);
        assert_eq!(timescale, CM_TIME_NANOSECOND_TIMESCALE);
        assert_eq!(cm_time_to_duration(value, timescale, V), Some(d));
    }
    assert_eq!(
        duration_to_cm_time(Duration::MAX),
        (i64::MAX, CM_TIME_NANOSECOND_TIMESCALE),
        "saturates"
    );
}

// ------------------------------------------------------------- BGRA → RGBA

#[test]
fn bgra_becomes_opaque_rgba() {
    let src = [1, 2, 3, 0, 10, 20, 30, 128];
    assert_eq!(
        bgra_to_rgba(&src, 2, 1, 8),
        Some(vec![3, 2, 1, 255, 30, 20, 10, 255])
    );
}

#[test]
fn row_padding_is_skipped() {
    // 2×2 pixels, rows padded to 12 bytes (CoreVideo aligns bytes-per-row);
    // the padding (0xEE) must not leak into the frame.
    let src = [
        1, 2, 3, 4, 5, 6, 7, 8, 0xEE, 0xEE, 0xEE, 0xEE, //
        9, 10, 11, 12, 13, 14, 15, 16, 0xEE, 0xEE, 0xEE, 0xEE,
    ];
    assert_eq!(
        bgra_to_rgba(&src, 2, 2, 12),
        Some(vec![
            3, 2, 1, 255, 7, 6, 5, 255, //
            11, 10, 9, 255, 15, 14, 13, 255,
        ])
    );
}

#[test]
fn the_last_row_need_not_carry_its_padding() {
    let src = [
        1, 2, 3, 4, 0xEE, 0xEE, 0xEE, 0xEE, //
        5, 6, 7, 8,
    ];
    assert_eq!(
        bgra_to_rgba(&src, 1, 2, 8),
        Some(vec![3, 2, 1, 255, 7, 6, 5, 255])
    );
}

#[test]
fn a_geometry_the_bytes_cannot_hold_is_refused() {
    let src = [0u8; 16];
    assert_eq!(
        bgra_to_rgba(&src, 2, 2, 4),
        None,
        "rows narrower than the width"
    );
    assert_eq!(bgra_to_rgba(&src, 2, 3, 8), None, "too few bytes");
    assert_eq!(bgra_to_rgba(&src, 0, 2, 8), None, "empty");
    assert_eq!(bgra_to_rgba(&src, 2, 0, 8), None, "empty");
    assert_eq!(
        bgra_to_rgba(&src, usize::MAX, 1, usize::MAX),
        None,
        "overflow"
    );
    assert_eq!(bgra_to_rgba(&src, 1, usize::MAX, 8), None, "overflow");
}

// ---------------------------------------------------------- the state mapping

fn native() -> NativeState {
    NativeState {
        item_status: NativeItemStatus::ReadyToPlay,
        player_failed: false,
        error: None,
        time_control: NativeTimeControl::Paused,
        rate: 0.0,
        current_time: Some(Duration::ZERO),
        duration: Some(Duration::from_secs(10)),
    }
}

#[test]
fn a_clip_opening_paused_is_loading_until_ready() {
    let opening = NativeState {
        item_status: NativeItemStatus::Unknown,
        duration: None,
        ..native()
    };
    assert_eq!(
        map_state(&opening),
        PlayerState {
            loading: true,
            ..PlayerState::default()
        }
    );
    assert_eq!(
        map_state(&native()),
        PlayerState {
            duration: Duration::from_secs(10),
            ..PlayerState::default()
        }
    );
}

#[test]
fn playing_starts_when_asked_not_when_sound_comes() {
    // Right after play(): the rate is set, the player waits for data.
    let waiting = NativeState {
        time_control: NativeTimeControl::Waiting,
        rate: 1.0,
        ..native()
    };
    assert!(map_state(&waiting).playing);
    let playing = NativeState {
        time_control: NativeTimeControl::Playing,
        rate: 1.0,
        current_time: Some(Duration::from_secs(3)),
        ..native()
    };
    let s = map_state(&playing);
    assert!(s.playing && !s.completed);
    assert_eq!(s.position, Duration::from_secs(3));
    assert!(!map_state(&native()).playing, "paused");
}

#[test]
fn stopped_at_the_end_is_completed() {
    let ended = NativeState {
        current_time: Some(Duration::from_secs(10)),
        ..native()
    };
    let s = map_state(&ended);
    assert!(s.completed && !s.playing);
    // A scrub to the end seeks to the whole millisecond below the duration.
    let scrubbed = NativeState {
        current_time: Some(Duration::from_millis(1234)),
        duration: Some(Duration::from_micros(1_234_600)),
        ..native()
    };
    assert!(map_state(&scrubbed).completed);
    // Mid-clip, or still playing through the end, is not.
    let mid = NativeState {
        current_time: Some(Duration::from_secs(5)),
        ..native()
    };
    assert!(!map_state(&mid).completed);
    let still_playing = NativeState {
        time_control: NativeTimeControl::Playing,
        rate: 1.0,
        current_time: Some(Duration::from_secs(10)),
        ..native()
    };
    assert!(!map_state(&still_playing).completed);
    // A clip with no known length never completes.
    let unknown = NativeState {
        duration: None,
        ..native()
    };
    assert!(!map_state(&unknown).completed);
}

#[test]
fn the_position_never_runs_past_the_duration() {
    let past = NativeState {
        current_time: Some(Duration::from_secs(11)),
        ..native()
    };
    assert_eq!(map_state(&past).position, Duration::from_secs(10));
    // With no duration yet there is nothing to clamp to.
    let unknown = NativeState {
        item_status: NativeItemStatus::Unknown,
        current_time: Some(Duration::from_secs(2)),
        duration: None,
        ..native()
    };
    assert_eq!(map_state(&unknown).position, Duration::from_secs(2));
    // An invalid current time reads as the start.
    let invalid = NativeState {
        current_time: None,
        ..native()
    };
    assert_eq!(map_state(&invalid).position, Duration::ZERO);
}

#[test]
fn a_failed_item_reports_its_error_and_never_plays() {
    let failed = NativeState {
        item_status: NativeItemStatus::Failed,
        error: Some("The file couldn't be opened.".to_string()),
        time_control: NativeTimeControl::Waiting,
        rate: 1.0,
        duration: None,
        ..native()
    };
    let s = map_state(&failed);
    assert_eq!(s.error.as_deref(), Some("The file couldn't be opened."));
    assert!(!s.playing && !s.loading && !s.completed);

    let player_failed = NativeState {
        player_failed: true,
        ..native()
    };
    assert_eq!(
        map_state(&player_failed).error.as_deref(),
        Some("playback failed"),
        "a failure without a description still reads as one"
    );
}

// ---------------------------------------------------------- null and fake

#[test]
fn the_null_player_reports_every_clip_unavailable() {
    let mut player = NullMediaPlayer;
    assert_eq!(
        player.open("/tmp/example.wav", false),
        Err(MediaPlayerError::Unavailable)
    );
    player.play();
    let s = player.state();
    assert!(!s.playing && s.error.is_some());
    assert_eq!(player.frame(), None);
}

#[cfg(not(all(feature = "device", target_os = "macos")))]
#[test]
fn a_build_without_device_makes_null_players() {
    let mut player = SystemMediaPlayers
        .create(MediaKind::Video)
        .expect("a player");
    assert_eq!(
        player.open("/tmp/example.mp4", true),
        Err(MediaPlayerError::Unavailable)
    );
}

#[test]
fn the_fake_records_what_it_is_asked_and_its_disposal() {
    let players = FakeMediaPlayers::new();
    let mut player = players.create(MediaKind::Audio).expect("a player");
    player.open("/tmp/example.wav", false).expect("opens");
    player.play_or_pause();
    player.seek(Duration::from_millis(1500));
    player.play_or_pause();
    let handle = players.built().pop().expect("built");
    assert_eq!(handle.kind(), MediaKind::Audio);
    assert_eq!(
        handle.opened(),
        vec![("/tmp/example.wav".to_string(), false)]
    );
    assert_eq!((handle.plays(), handle.pauses()), (1, 1));
    assert_eq!(handle.seeks(), vec![Duration::from_millis(1500)]);
    assert!(!handle.disposed());
    drop(player);
    assert!(handle.disposed());
}

#[test]
fn a_failing_fake_fails_its_open_and_frames_bump_their_revision() {
    let players = FakeMediaPlayers::failing_open();
    let mut player = players.create(MediaKind::Video).expect("a player");
    assert!(matches!(
        player.open("/tmp/example.mp4", true),
        Err(MediaPlayerError::Open { .. })
    ));
    let handle = &players.built()[0];
    assert_eq!(player.frame(), None);
    handle.push_frame(1, 1, vec![1, 2, 3, 255]);
    handle.push_frame(1, 1, vec![4, 5, 6, 255]);
    let frame = player.frame().expect("a frame");
    assert_eq!(frame.revision, 2);
    assert_eq!(*frame.rgba, vec![4, 5, 6, 255]);
}
