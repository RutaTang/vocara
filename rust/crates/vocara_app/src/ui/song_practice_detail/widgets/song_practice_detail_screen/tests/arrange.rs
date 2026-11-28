//! The arrange on the page: where the interpretation menu opens, the rows
//! keeping their places as the backing switches, and the takes keeping
//! their colours.

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
