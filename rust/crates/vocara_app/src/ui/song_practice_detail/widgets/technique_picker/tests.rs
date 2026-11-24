//! The technique picker's tests: the picker half of `marker_dialogs_test.dart`
//! (what it says when the library cannot be read), and what it lets through,
//! shows and returns.

use iced::keyboard::{self, key::Named};
use iced::Point;
use vocara_model::util::dart_date_time::DateTime;

use super::*;

fn technique(id: &str, name: &str, aliases: &[&str], tags: &[&str]) -> Technique {
    let at = DateTime::utc_ymd(2026, 1, 1);
    Technique {
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        ..Technique::new(id, name, at, at)
    }
}

fn library() -> Vec<Technique> {
    vec![
        technique("t1", "Twang", &["Edge"], &["Quality"]),
        technique("t2", "Mixed voice", &[], &["Register", "Belt"]),
        technique("t3", "Vocal fry", &["Creak"], &[]),
    ]
}

fn loaded(list: Option<Vec<Technique>>) -> TechniquePicker {
    let mut picker = TechniquePicker::new(DEFAULT_TITLE);
    picker.loaded(list);
    picker
}

/// What the picker shows, asked for `label`.
fn shows(picker: &TechniquePicker, label: &str) -> bool {
    let mut ui = iced_test::simulator(picker.view());
    ui.find(label).is_ok()
}

// ------------------------------------------------ marker_dialogs_test

#[test]
fn a_library_that_could_not_be_read_is_not_called_empty() {
    let picker = loaded(None);
    assert!(shows(
        &picker,
        "Couldn't open the technique library. Try again in a moment."
    ));
    assert!(!shows(
        &picker,
        "No techniques yet. Add some in the technique library first."
    ));
}

// ------------------------------------------------------- the states

#[test]
fn it_spins_until_the_library_arrives_then_says_when_it_is_empty() {
    let mut picker = TechniquePicker::new("Add technique at 00:12");
    assert!(picker.is_loading());
    assert!(shows(&picker, "Add technique at 00:12"));
    assert!(!shows(
        &picker,
        "No techniques yet. Add some in the technique library first."
    ));
    picker.loaded(Some(Vec::new()));
    assert!(!picker.is_loading());
    assert!(shows(
        &picker,
        "No techniques yet. Add some in the technique library first."
    ));
    assert!(!shows(
        &picker,
        "Couldn't open the technique library. Try again in a moment."
    ));
}

#[test]
fn each_technique_shows_its_name_and_its_tags() {
    let picker = loaded(Some(library()));
    assert!(shows(&picker, "Twang"));
    assert!(shows(&picker, "Quality"));
    assert!(shows(&picker, "Register · Belt"));
    assert!(shows(&picker, "Vocal fry"));
    // The search field, with its hint.
    let mut ui = iced_test::simulator(picker.view());
    assert!(ui.find(picker.search_id()).is_ok());
}

// --------------------------------------------------------- the search

fn names(picker: &TechniquePicker) -> Vec<&str> {
    picker.filtered().iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn the_search_matches_names_aliases_and_tags_in_any_case() {
    let mut picker = loaded(Some(library()));
    assert_eq!(names(&picker), ["Twang", "Mixed voice", "Vocal fry"]);
    for (query, expected) in [
        ("tw", vec!["Twang"]),
        ("  MIXED ", vec!["Mixed voice"]),
        ("edge", vec!["Twang"]),
        ("creak", vec!["Vocal fry"]),
        ("belt", vec!["Mixed voice"]),
        ("o", vec!["Mixed voice", "Vocal fry"]),
        ("   ", vec!["Twang", "Mixed voice", "Vocal fry"]),
        ("zzz", vec![]),
    ] {
        picker.update(Message::Query(query.into()));
        assert_eq!(names(&picker), expected, "{query:?}");
    }
    // Nothing through: said so.
    picker.update(Message::Query("zzz".into()));
    assert!(shows(&picker, "No techniques match."));
    assert!(!shows(&picker, "Twang"));
}

#[test]
fn typing_goes_through_the_search_field() {
    let picker = loaded(Some(library()));
    let mut ui = iced_test::simulator(picker.view());
    ui.click(picker.search_id()).expect("the search field");
    let _ = ui.typewrite("fr");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::Query("f".into()), Message::Query("fr".into())]
    );
}

#[test]
fn the_search_field_keeps_its_text_inset_in_the_fixed_well() {
    // The 34-tall well holds the glyph (10 inside its 1 px border), 8, then
    // the field: forui's content padding less compact density's (10
    // across, 6 down) around a 13 px line, centred in the well.
    let picker = loaded(Some(library()));
    let mut ui = iced_test::simulator(picker.view());
    let title = ui.find(DEFAULT_TITLE).expect("the title").bounds();
    let field = ui
        .find(picker.search_id())
        .expect("the search field")
        .bounds();
    let card_left = (1024.0 - WIDTH) / 2.0;
    assert_eq!(title.x, card_left + 1.0 + 20.0, "{title:?}");
    assert_eq!(field.x, title.x + 11.0 + 14.0 + 8.0, "{field:?}");
    assert_eq!(
        field.width,
        WIDTH - 2.0 * 21.0 - 2.0 * 11.0 - 14.0 - 8.0,
        "{field:?}"
    );
    assert_eq!(field.height, 13.0 + 12.0, "{field:?}");
    // Under the title (16 px) and 12, the well; the field centred in the
    // 32 inside its border.
    let well_top = title.y + 16.0 + 12.0;
    assert_eq!(field.y, well_top + 1.0 + (32.0 - 25.0) / 2.0, "{field:?}");
}

// ------------------------------------------------------ the outcomes

#[test]
fn a_tap_picks_the_technique() {
    let mut picker = loaded(Some(library()));
    picker.update(Message::Query("mix".into()));
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(picker.view());
        ui.click("Mixed voice").expect("the row");
        ui.into_messages().collect()
    };
    assert_eq!(messages, vec![Message::Pick("t2".into())]);
    let picked = picker.update(messages[0].clone());
    assert_eq!(picked, Some(Some(library()[1].clone())));
    // A technique the list no longer holds keeps the picker open.
    assert_eq!(picker.update(Message::Pick("gone".into())), None);
}

#[test]
fn escape_and_the_barrier_cancel_it() {
    let mut picker = loaded(Some(library()));
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(picker.view());
        let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
        ui.point_at(Point::new(5.0, 5.0));
        let _ = ui.simulate(iced_test::simulator::click());
        // A tap on the card is not a barrier tap.
        ui.click("Add technique at playhead").expect("the title");
        ui.into_messages().collect()
    };
    assert_eq!(messages, vec![Message::Cancel, Message::Cancel]);
    assert_eq!(picker.update(Message::Cancel), Some(None));
}
