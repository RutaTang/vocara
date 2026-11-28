//! The arrange on the page: where the interpretation menu opens, and the
//! rows keeping their places as the backing switches.

use super::*;

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
