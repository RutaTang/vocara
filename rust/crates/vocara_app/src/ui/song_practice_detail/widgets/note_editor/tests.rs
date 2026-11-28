//! The note editor's tests: the note-editor half of
//! `marker_dialogs_test.dart` (a note with no text cannot be added or
//! saved; a drag too short for a range is titled as the point it is saved
//! as), and what the editor shows and returns.

use std::sync::Arc;

use iced::keyboard::{self, key::Named};
use iced_test::Simulator;
use vocara_model::util::dart_date_time::DateTime;

use super::*;

/// The detail view model's `normalizeMarkerRange` (with its 120 ms
/// `kMinMarkerRangeMs`), which the screen passes in.
fn normalize(start_ms: i64, end_ms: Option<i64>) -> (i64, Option<i64>) {
    let Some(end_ms) = end_ms else {
        return (start_ms.max(0), None);
    };
    let s = start_ms.min(end_ms).max(0);
    let e = start_ms.max(end_ms);
    if e - s < 120 {
        return (s, None);
    }
    (s, Some(e))
}

fn note(position_ms: i64, end_ms: Option<i64>, content: &str) -> PracticeNote {
    PracticeNote {
        id: "n".into(),
        position_ms,
        end_ms,
        content: content.into(),
        created_at: DateTime::utc_ymd(2026, 1, 1),
        updated_at: DateTime::utc_ymd(2026, 1, 1),
    }
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// `enterText`: the field's whole text at once.
fn enter_text(editor: &mut NoteEditor, text: &str) {
    assert_eq!(
        editor.update(Message::Edit(text_editor::Action::SelectAll)),
        Step::Stay
    );
    assert_eq!(
        editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new(text.to_string()))
        ))),
        Step::Stay
    );
}

/// Drives the editor's view once and feeds what it reported back.
fn drive(editor: &mut NoteEditor, steps: impl FnOnce(&mut Simulator<'_, Message>)) -> Vec<Step> {
    let messages: Vec<Message> = {
        let mut ui = iced_test::simulator(editor.view());
        steps(&mut ui);
        ui.into_messages().collect()
    };
    messages.into_iter().map(|m| editor.update(m)).collect()
}

// ------------------------------------------------- marker_dialogs_test.dart

#[test]
fn a_note_with_no_text_cannot_be_added_or_saved() {
    // Empty, "Save" closed as if it had saved, the note keeping its old text.
    let existing = note(0, None, "breath");
    let mut editor = NoteEditor::new(Some(&existing), Duration::ZERO, None, normalize);
    assert_eq!(editor.text(), "breath");
    enter_text(&mut editor, "   ");
    assert!(!editor.can_save());
    let steps = drive(&mut editor, |ui| {
        ui.click("Save").expect("the save button");
    });
    assert!(steps.is_empty(), "the dialog stays open: {steps:?}");
    assert!(iced_test::simulator(editor.view()).find("Save").is_ok());

    enter_text(&mut editor, "sharp");
    let steps = drive(&mut editor, |ui| {
        ui.click("Save").expect("the save button");
    });
    assert_eq!(
        steps,
        vec![Step::Closed(Some(NoteEditorResult {
            content: Some("sharp".into()),
            deleted: false,
        }))]
    );
}

#[test]
fn a_drag_too_short_for_a_range_is_titled_as_the_point_it_is_saved_as() {
    let editor = NoteEditor::new(None, ms(12_000), Some(ms(12_050)), normalize);
    assert_eq!(editor.title(), "Note at 00:12");
    assert!(iced_test::simulator(editor.view())
        .find("Note at 00:12")
        .is_ok());
}

// ------------------------------------------------------------------ the rest

#[test]
fn a_range_is_titled_by_its_span_earlier_edge_first() {
    let forward = NoteEditor::new(None, ms(12_000), Some(ms(20_000)), normalize);
    assert_eq!(forward.title(), "Note across 00:12 → 00:20");
    // A drag drawn right to left.
    let backward = NoteEditor::new(None, ms(20_000), Some(ms(12_000)), normalize);
    assert_eq!(backward.title(), "Note across 00:12 → 00:20");
}

#[test]
fn an_existing_note_is_titled_by_its_own_range() {
    let existing = note(65_000, Some(70_000), "flat");
    let editor = NoteEditor::new(Some(&existing), ms(3_000), None, normalize);
    assert_eq!(editor.title(), "Note across 01:05 → 01:10");
    let point = note(65_000, None, "flat");
    let editor = NoteEditor::new(Some(&point), ms(3_000), Some(ms(9_000)), normalize);
    assert_eq!(editor.title(), "Note at 01:05");
}

#[test]
fn a_new_note_is_added_and_an_existing_one_saved_or_deleted() {
    let mut adding = NoteEditor::new(None, ms(1_000), None, normalize);
    assert!(!adding.editing());
    let mut ui = iced_test::simulator(adding.view());
    assert!(ui.find("Add").is_ok());
    assert!(ui.find("Save").is_err());
    assert!(ui.find("Delete").is_err(), "nothing to delete yet");
    drop(ui);
    enter_text(&mut adding, "  breath runs out \n");
    let steps = drive(&mut adding, |ui| {
        ui.click("Add").expect("the add button");
    });
    assert_eq!(
        steps,
        vec![Step::Closed(Some(NoteEditorResult {
            content: Some("breath runs out".into()),
            deleted: false,
        }))]
    );

    let existing = note(0, None, "breath");
    let mut editing = NoteEditor::new(Some(&existing), Duration::ZERO, None, normalize);
    let steps = drive(&mut editing, |ui| {
        ui.click("Delete").expect("the delete button");
    });
    assert_eq!(
        steps,
        vec![Step::Closed(Some(NoteEditorResult {
            content: None,
            deleted: true,
        }))]
    );
}

#[test]
fn cancel_escape_and_the_barrier_close_it_with_nothing() {
    let existing = note(0, None, "breath");
    let mut editor = NoteEditor::new(Some(&existing), Duration::ZERO, None, normalize);
    let steps = drive(&mut editor, |ui| {
        ui.click("Cancel").expect("the cancel button");
        let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
        // Below the title-bar band, whose clicks are the window's.
        ui.point_at(iced::Point::new(5.0, 100.0));
        let _ = ui.simulate(iced_test::simulator::click());
    });
    assert_eq!(steps, vec![Step::Closed(None); 3]);
}

/// A click on the barrier, below the title-bar band.
fn click_barrier(ui: &mut Simulator<'_, Message>) {
    ui.point_at(iced::Point::new(5.0, 100.0));
    let _ = ui.simulate(iced_test::simulator::click());
}

#[test]
fn the_barrier_keeps_a_note_with_unsaved_text_open() {
    // A stray click outside the card threw the typed text away silently.
    let mut editor = NoteEditor::new(None, Duration::ZERO, None, normalize);
    enter_text(&mut editor, "breath runs out");
    let steps = drive(&mut editor, click_barrier);
    assert_eq!(steps, vec![Step::Stay]);
    assert_eq!(editor.text(), "breath runs out");
    // An existing note edited: the same.
    let existing = note(0, None, "breath");
    let mut editing = NoteEditor::new(Some(&existing), Duration::ZERO, None, normalize);
    enter_text(&mut editing, "breath, then flat");
    assert_eq!(drive(&mut editing, click_barrier), vec![Step::Stay]);
    // Changed back, nothing is lost: the barrier dismisses it.
    enter_text(&mut editing, "breath ");
    assert_eq!(drive(&mut editing, click_barrier), vec![Step::Closed(None)]);
    // Escape is a deliberate cancel.
    let steps = drive(&mut editor, |ui| {
        let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
    });
    assert_eq!(steps, vec![Step::Closed(None)]);
}

/// Cmd+Return (Ctrl+Return off macOS).
fn command_return() -> iced::Event {
    iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(Named::Enter),
        modified_key: keyboard::Key::Named(Named::Enter),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::COMMAND,
        text: None,
        repeat: false,
    })
}

#[test]
fn command_return_saves_without_breaking_the_line() {
    let mut editor = NoteEditor::new(None, Duration::ZERO, None, normalize);
    let id = editor.input_id();
    let steps = drive(&mut editor, |ui| {
        ui.click(id).expect("the field");
        let _ = ui.typewrite("pitchflat");
        // The caret between the words, where Return breaks the line.
        for _ in 0..4 {
            let _ = ui.tap_key(keyboard::Key::Named(Named::ArrowLeft));
        }
        let _ = ui.simulate([command_return()]);
    });
    assert_eq!(
        steps.last(),
        Some(&Step::Closed(Some(NoteEditorResult {
            content: Some("pitchflat".into()),
            deleted: false,
        }))),
        "{steps:?}"
    );
    // With nothing to save it does nothing, as the dead Add button.
    let mut empty = NoteEditor::new(None, Duration::ZERO, None, normalize);
    let id = empty.input_id();
    let steps = drive(&mut empty, |ui| {
        ui.click(id).expect("the field");
        let _ = ui.simulate([command_return()]);
    });
    assert!(steps.iter().all(|s| *s == Step::Stay), "{steps:?}");
    assert_eq!(empty.text(), "");
}

#[test]
fn typing_goes_through_the_field_and_return_breaks_the_line() {
    let mut editor = NoteEditor::new(None, Duration::ZERO, None, normalize);
    let id = editor.input_id();
    let steps = drive(&mut editor, |ui| {
        ui.click(id).expect("the field");
        let _ = ui.typewrite("pitch");
        let _ = ui.tap_key(keyboard::Key::Named(Named::Enter));
        let _ = ui.typewrite("flat");
    });
    assert!(steps.iter().all(|s| *s == Step::Stay), "{steps:?}");
    assert_eq!(editor.text(), "pitch\nflat");
}

#[test]
fn a_save_with_nothing_to_save_closes_with_nothing() {
    // `_save`: the button is dead then, but the path pops null.
    let mut editor = NoteEditor::new(None, Duration::ZERO, None, normalize);
    assert_eq!(editor.update(Message::Save), Step::Closed(None));
}

#[test]
fn the_card_is_at_most_460_wide_and_centred() {
    let editor = NoteEditor::new(None, ms(12_000), None, normalize);
    // The default 1024 px window: the title inside the card's 20 px
    // padding (its outline is inset).
    let mut ui = iced_test::simulator(editor.view());
    let title = ui.find("Note at 00:12").expect("the title").bounds();
    let card_left = (1024.0 - CARD_MAX_WIDTH) / 2.0;
    assert!((title.x - (card_left + 20.0)).abs() < 0.5, "{title:?}");
}

#[test]
fn the_field_holds_three_to_six_lines_and_its_inset() {
    // `minLines: 3, maxLines: 6` of 12 px on an 18 px line, and the field
    // padding: 6 above and below, 10 at the sides.
    let line = field_line();
    assert_eq!(line, 18.0);
    let mut editor = NoteEditor::new(None, Duration::ZERO, None, normalize);
    let long = "line\n".repeat(12);
    for (text, lines) in [("short", MIN_LINES), (long.as_str(), MAX_LINES)] {
        enter_text(&mut editor, text);
        let mut ui = iced_test::simulator(editor.view());
        let field = ui.find(editor.input_id()).expect("the field").bounds();
        assert!(
            (field.height - (lines * line + 12.0)).abs() < 1e-3,
            "{lines} lines: {field:?}"
        );
        // The card's 20: the field draws its own well.
        let card_left = (1024.0 - CARD_MAX_WIDTH) / 2.0;
        assert_eq!(field.x, card_left + 20.0, "{field:?}");
    }
}
