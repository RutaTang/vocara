//! The arrange on the page: where the interpretation menu opens, the rows
//! keeping their places as the backing switches, the takes keeping their
//! colours, and what a lane's menu calls the take.

use super::*;

/// The colour of `name`'s header stripe, as drawn.
fn stripe(h: &Harness, name: &str) -> [u8; 4] {
    let label = header_label(h, name);
    let section = h.find("Vocal takes").expect("the takes header").bounds();
    // The stripe runs down the header's left edge, where the section row's
    // 14 px inset starts.
    let x = section.x - 14.0 + 1.0;
    let mut ui = LiveUi::new(h.size);
    ui.snapshot(h.screen.view())
        .pixel(x as usize, label.center_y() as usize)
}

// Duplicating Take 1 turned Take 2 from ochre to sage, and a reorder
// recoloured the takes the same way: a take's colour was its place.
#[test]
fn a_take_keeps_its_colour_as_takes_are_duplicated_or_reordered() {
    let mut h = Harness::open(Setup {
        practice: practice(vec![take("t1", "Take 1"), take("t2", "Take 2")]),
        ..Setup::default()
    });
    let (one, two) = (stripe(&h, "Take 1"), stripe(&h, "Take 2"));
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ContextMenu,
    });
    h.send(Message::TrackMenu(TrackAction::Duplicate));
    h.settle();
    assert_eq!(h.practice().vocal_tracks.len(), 3, "Take 1 copy");
    assert_eq!(stripe(&h, "Take 2"), two, "Take 2, a lane further down");
    // Take 2 dropped on Take 1's lane: above it now.
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::CrossLaneMove("t2".into()),
    });
    h.settle();
    let at = |id: &str| h.practice().vocal_tracks.iter().position(|t| t.id == id);
    assert!(at("t2") < at("t1"), "reordered");
    assert_eq!(stripe(&h, "Take 2"), two);
    assert_eq!(stripe(&h, "Take 1"), one);
}

/// The dialog `action` on `track`'s lane menu asks.
fn asked(h: &mut Harness, track: &str, action: TrackAction) -> Dialog {
    h.send(Message::Row {
        track_id: track.into(),
        message: track_row::Message::ContextMenu,
    });
    h.send(Message::TrackMenu(action));
    h.dialog.take().expect("a dialog").dialog
}

// A take's lane menu spoke of a "track" where the rest of the page says
// take: "Rename track", "This removes the track…".
#[test]
fn a_takes_rename_and_delete_say_take_and_the_backings_say_track() {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.accompaniment = Some(Arc::new(backing()));
    let mut h = Harness::open(Setup {
        practice: p,
        ..Setup::default()
    });
    let Dialog::Prompt(rename) = asked(&mut h, "t1", TrackAction::Rename) else {
        panic!("a prompt");
    };
    assert_eq!(
        (rename.title.as_str(), rename.hint.as_str()),
        ("Rename take", "Take name")
    );
    let Dialog::Confirm(delete) = asked(&mut h, "t1", TrackAction::Delete) else {
        panic!("a confirmation");
    };
    assert_eq!(
        delete.body.as_deref(),
        Some("This removes the take, its audio and its notes.")
    );
    let Dialog::Prompt(rename) = asked(&mut h, "acc", TrackAction::Rename) else {
        panic!("a prompt");
    };
    assert_eq!(
        (rename.title.as_str(), rename.hint.as_str()),
        ("Rename track", "Track name")
    );
    let Dialog::Confirm(delete) = asked(&mut h, "acc", TrackAction::Delete) else {
        panic!("a confirmation");
    };
    assert_eq!(
        delete.body.as_deref(),
        Some("This removes the track, its audio file and its notes.")
    );
}

// The interpretation pop-up opened at the press: clicked at the button's
// right end, its left edge sat there, over the button, running toward the
// right panel.
#[test]
fn the_interpretation_menu_opens_under_its_button_wherever_it_is_pressed() {
    let mut h = Harness::open(Setup::default());
    let label = h.find("Interpretation 1").expect("the pop-up").bounds();
    let mut ui = LiveUi::new(h.size);
    // On the chevron, at the button's right end.
    ui.point_at(Point::new(label.x + label.width + 12.0, label.center_y()));
    let messages = ui.run(h.screen.view(), [live_ui::press(), live_ui::release()]);
    h.feed(messages);
    assert_eq!(h.screen.open_menu(), Some(&OpenMenu::Interpretations));
    let row = ui
        .find(h.screen.view(), nth("New interpretation", 0))
        .expect("the menu")
        .bounds();
    assert!(
        row.y > label.y + label.height,
        "below the button: {row:?}, {label:?}"
    );
    assert!(
        row.x < label.x + label.width / 2.0,
        "from the button's left edge, not the press: {row:?}, {label:?}"
    );
}

// Switching an Accompaniment to an empty Original moved every row below the
// backing by 4 px: its drop zone was taller than the lane it stood in for.
#[test]
fn the_rows_below_the_backing_stay_put_as_the_backing_switches() {
    let mut p = practice(vec![take("t1", "Take 1")]);
    p.accompaniment = Some(Arc::new(backing()));
    let mut h = Harness::open(Setup {
        practice: p,
        ..Setup::default()
    });
    let takes = |h: &Harness| h.find("Vocal takes").expect("the takes header").bounds();
    let before = takes(&h);
    h.send(Message::SetPlayReference(true));
    h.settle();
    assert!(h.shows("Drag the original song here, or click to browse"));
    assert_eq!(takes(&h), before);
}

/// The colour of `clip`'s header band near its right end (past its name),
/// as drawn.
fn clip_band(h: &Harness, clip: &str) -> [u8; 4] {
    let mut ui = LiveUi::new(h.size);
    let at = ui
        .find(h.screen.view(), track_row::clip_widget_id(clip))
        .expect("the clip")
        .bounds();
    ui.snapshot(h.screen.view())
        .pixel((at.x + at.width - 8.0) as usize, (at.y + 4.0) as usize)
}

// Soloing Take 2 dropped Take 1 from the mix, but Take 1's lane still
// looked as live as the soloed one.
#[test]
fn a_solo_dims_the_lanes_it_silences_as_a_mute_does() {
    let long = |id: &str, name: &str| {
        let mut t = take(id, name);
        t.clips[0].duration_ms = 20_000;
        t
    };
    let mut h = Harness::open(Setup {
        practice: practice(vec![long("t1", "Take 1"), long("t2", "Take 2")]),
        ..Setup::default()
    });
    let c1 = h.track("t1").clips[0].id.clone();
    let c2 = h.track("t2").clips[0].id.clone();
    let (live1, live2) = (clip_band(&h, &c1), clip_band(&h, &c2));
    let toggle = |h: &mut Harness, id: &str, message: track_row::Message| {
        h.send(Message::Row {
            track_id: id.into(),
            message,
        });
        h.settle();
    };
    toggle(&mut h, "t1", track_row::Message::ToggleMute);
    let muted1 = clip_band(&h, &c1);
    assert_ne!(muted1, live1, "a muted take's clips dim");
    toggle(&mut h, "t1", track_row::Message::ToggleMute);
    assert_eq!(clip_band(&h, &c1), live1);

    toggle(&mut h, "t2", track_row::Message::ToggleSolo);
    assert_eq!(clip_band(&h, &c1), muted1, "Take 1, silenced by the solo");
    assert_eq!(clip_band(&h, &c2), live2, "Take 2, soloed, heard");
    toggle(&mut h, "t2", track_row::Message::ToggleSolo);
    assert_eq!(clip_band(&h, &c1), live1, "heard again");
}

/// Take 1 with a clip at 0:00 and one at 0:12, overlapping it; Take 2 with
/// one clip.
fn two_takes() -> Harness {
    let mut t1 = take("t1", "Take 1");
    t1.clips = vec![
        AudioClip {
            start_ms: 0,
            ..AudioClip::new("c1", "/pool/c1.wav", 15_000)
        },
        AudioClip {
            start_ms: 12_000,
            ..AudioClip::new("c2", "/pool/c2.wav", 10_000)
        },
    ];
    let mut t2 = take("t2", "Take 2");
    t2.clips[0].id = "d1".into();
    Harness::open(Setup {
        practice: practice(vec![t1, t2]),
        ..Setup::default()
    })
}

// A right press opened its menu with the selection left where it was: Take
// 2's clip menu over Take 1 selected.
#[test]
fn a_right_press_selects_the_clip_or_take_before_its_menu_opens() {
    let mut h = two_takes();
    assert_eq!(h.vm().selected_track_id(), Some("t1"));
    let mut ui = LiveUi::new(h.size);
    let clip = ui
        .find(h.screen.view(), track_row::clip_widget_id("d1"))
        .expect("Take 2's clip")
        .bounds();
    ui.point_at(clip.center());
    let messages = ui.run(h.screen.view(), live_ui::right_click());
    h.feed(messages);
    assert_eq!(
        h.screen.open_menu(),
        Some(&OpenMenu::Clip {
            track_id: "t2".into(),
            clip_id: "d1".into()
        })
    );
    assert_eq!(h.vm().selected_track_id(), Some("t2"));
    assert_eq!(h.vm().selected_clip_id(), Some("d1"));
    h.send(Message::DismissMenu);

    // Take 1's header: the take.
    let header = header_label(&h, "Take 1");
    ui.point_at(header.center());
    let messages = ui.run(h.screen.view(), live_ui::right_click());
    h.feed(messages);
    assert_eq!(h.screen.open_menu(), Some(&OpenMenu::Track("t1".into())));
    assert_eq!(h.vm().selected_track_id(), Some("t1"));
    assert_eq!(h.vm().selected_clip_id(), None);
    h.send(Message::DismissMenu);

    // On the take already selected, its selected clip stays so.
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::SelectClip("c2".into()),
    });
    ui.point_at(header.center());
    let messages = ui.run(h.screen.view(), live_ui::right_click());
    h.feed(messages);
    assert_eq!(h.vm().selected_clip_id(), Some("c2"));
}

// "Delete this clip?" did not say which, over overlapping clips.
#[test]
fn the_delete_clip_question_names_the_clip_by_its_take_and_start() {
    let mut h = two_takes();
    for (clip, said) in [
        ("c1", "This removes the clip at 00:00 on \"Take 1\""),
        ("c2", "This removes the clip at 00:12 on \"Take 1\""),
    ] {
        h.send(Message::Row {
            track_id: "t1".into(),
            message: track_row::Message::ClipContextMenu(clip.into()),
        });
        h.click("Delete clip");
        let Dialog::Confirm(confirm) = h.dialog() else {
            panic!("a confirmation");
        };
        assert_eq!(confirm.title, "Delete this clip?");
        let body = confirm.body.clone().unwrap_or_default();
        assert!(body.starts_with(said), "{body}");
        h.answer(DialogResult::Confirmed(false));
    }
}
