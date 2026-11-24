//! The dock's tests: its tab strip (every analysis tab, the mixer set apart,
//! the Take/Live toggle only where the open tab has one), its height kept to
//! the room the workspace has, the resize grabber's moves, the heal it asks
//! for, and `playhead_repaint_test.dart`'s 'opening the dock keeps the tab
//! strip where it was scrolled'. (The rest of that file's dock cases — what
//! a tick repaints — has no iced counterpart: the canvases cache by their
//! inputs, which the tabs' own tests pin.)

use iced::widget::text;
use iced::Size;
use iced_test::selector::{Candidate, Target};

use super::*;
use crate::ui::song_practice_detail::widgets::live_ui::{self, LiveUi};

fn dock<'a>(open: bool, tab: DockTab, supports_live: bool) -> BottomDock<'a> {
    BottomDock {
        open,
        tab,
        supports_live,
        live: false,
        dock_height: 200.0,
        max_height: 600.0,
        content: open.then(|| Element::from(text("CONTENT"))),
    }
}

#[test]
fn every_tab_is_labelled_as_dart_labelled_it() {
    let labels: Vec<&str> = DockTab::ALL.iter().map(|t| tab_label(*t)).collect();
    assert_eq!(
        labels,
        [
            "Analysis Summary",
            "Spectrogram",
            "Power",
            "LTAS",
            "Vowel",
            "Pitch",
            "Vibrato",
            "Timbre",
            "Register",
            "Range",
            "Dynamics",
            "Mixer"
        ]
    );
}

#[test]
fn the_closed_dock_shows_its_tabs_and_the_open_toggle() {
    let mut ui = iced_test::simulator(bottom_dock(dock(false, DockTab::Spectrogram, true)));
    for t in DockTab::ALL {
        assert!(
            ui.find(tab_label(t).to_uppercase().as_str()).is_ok(),
            "{t:?}"
        );
    }
    assert!(ui
        .find(char::from(Icon::PanelBottomOpen).to_string().as_str())
        .is_ok());
    // No Take/Live toggle and no content while closed.
    assert!(ui.find("Live").is_err());
    assert!(ui.find("CONTENT").is_err());
}

#[test]
fn the_open_dock_shows_its_content_and_the_live_toggle_where_the_tab_has_one() {
    let mut ui = iced_test::simulator(bottom_dock(dock(true, DockTab::Spectrogram, true)));
    assert!(ui.find("CONTENT").is_ok());
    assert!(ui.find("Live").is_ok());
    assert!(ui
        .find(char::from(Icon::PanelBottomClose).to_string().as_str())
        .is_ok());
    let mut ui = iced_test::simulator(bottom_dock(dock(true, DockTab::Range, false)));
    assert!(ui.find("Live").is_err());
}

#[test]
fn a_tab_and_the_toggle_report_their_calls() {
    let mut ui = iced_test::simulator(bottom_dock(dock(false, DockTab::Spectrogram, false)));
    ui.click("POWER").expect("the Power tab");
    ui.click(char::from(Icon::PanelBottomOpen).to_string().as_str())
        .expect("the toggle");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(matches!(
        messages.as_slice(),
        [Message::ShowDockTab(DockTab::Power), Message::ToggleDock]
    ));
}

// The content shows as dragged, but never taller than the workspace has
// room for: on a window shorter than the dock at its tallest, the timeline
// collapsed and the dock's bottom was cut off.
#[test]
fn the_content_keeps_to_the_room_the_workspace_has() {
    assert_eq!(content_height(300.0, 1000.0), 300.0);
    assert_eq!(content_height(300.0, 200.0), 200.0 - CHROME_HEIGHT);
    assert_eq!(content_height(300.0, 10.0), 0.0);
}

// From the height on screen: a drag down from a capped dock moves it at
// once (from the stored height it first had to make up the excess).
#[test]
fn a_resize_moves_the_height_on_screen() {
    assert_eq!(resized_height(400.0, 200.0, 10.0), 190.0);
    assert_eq!(resized_height(150.0, 200.0, -10.0), 160.0);
}

#[test]
fn the_dock_heals_only_a_take_it_shows() {
    assert!(heals(true, true, false, true));
    assert!(!heals(false, true, false, true), "closed");
    assert!(!heals(true, false, false, true), "the mixer");
    assert!(!heals(true, true, true, true), "live");
    assert!(!heals(true, true, false, false), "nothing lacking");
}

#[test]
fn a_drag_on_the_grabber_reports_each_move_past_the_slop() {
    let mut ui = LiveUi::new(Size::new(800.0, 400.0));
    let messages = ui.run(
        bottom_dock(dock(true, DockTab::Mixer, false)),
        [
            live_ui::move_to(400.0, 6.0),
            live_ui::press(),
            // Inside the slop: swallowed.
            live_ui::move_to(400.0, 6.5),
            live_ui::move_to(400.0, 4.0),
            live_ui::move_to(400.0, -6.0),
            live_ui::move_to(400.0, -1.0),
            live_ui::release(),
            live_ui::move_to(400.0, 20.0),
        ],
    );
    let moves: Vec<(f32, f32)> = messages
        .iter()
        .filter_map(|m| match m {
            Message::Resize { dy, cap } => Some((*dy, *cap)),
            _ => None,
        })
        .collect();
    assert_eq!(
        moves,
        [(-10.0, 600.0 - CHROME_HEIGHT), (5.0, 600.0 - CHROME_HEIGHT)]
    );
}

// playhead_repaint_test: 'opening the dock keeps the tab strip where it was
// scrolled' — unkeyed, the strip was rebuilt from scratch, its scroll back
// at the start just as a tab past the window's edge opened the dock.
#[test]
fn opening_the_dock_keeps_the_tab_strip_where_it_was_scrolled() {
    let mut ui = LiveUi::new(Size::new(520.0, 400.0));
    let closed = || bottom_dock(dock(false, DockTab::Spectrogram, false));
    assert_eq!(strip_scroll(&mut ui, closed()), 0.0);
    // Scroll the strip towards its end (a tab past the window's edge).
    let _ = ui.run(
        closed(),
        [live_ui::move_to(200.0, 18.0), live_ui::scroll(-400.0, 0.0)],
    );
    let scrolled = strip_scroll(&mut ui, closed());
    assert!(scrolled > 0.0, "the strip scrolled");
    // That tab opens the dock: the strip stays where it was (below the
    // grabber now).
    let open = bottom_dock(dock(true, DockTab::Dynamics, false));
    assert_eq!(strip_scroll(&mut ui, open), scrolled);
}

/// How far the tab strip (the dock's only scrollable here) is scrolled.
fn strip_scroll(ui: &mut LiveUi, view: Element<'_, Message>) -> f32 {
    let strip = ui.find(view, |c: Candidate<'_>| match &c {
        Candidate::Scrollable { .. } => Some(Target::from(c)),
        _ => None,
    });
    match strip {
        Some(Target::Scrollable { translation, .. }) => translation.x,
        other => panic!("the tab strip: {other:?}"),
    }
}
