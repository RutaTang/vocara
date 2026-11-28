//! The page's keyboard shortcuts (`shortcuts`): Space plays and pauses as
//! the play button does, Delete and Backspace ask to delete the selected
//! clip as its menu does, Escape lets go of it; and none of them acts while
//! a field, a menu or an editor holds the keyboard. (The app's dialogs
//! take every key before the page sees one: `app`'s tests.)

use iced::keyboard::{self, key::Named};

use super::super::shortcuts::{shortcut, Shortcut};
use super::*;

fn key_with(named: Named, modifiers: keyboard::Modifiers) -> keyboard::Event {
    let key = keyboard::Key::Named(named);
    keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers,
        // What a field types for it, as the platform hands it.
        text: (named == Named::Space).then(|| " ".into()),
        repeat: false,
    }
}

/// `named` pressed on the page as it is now: what the page published.
fn press(h: &Harness, ui: &mut LiveUi, named: Named) -> Vec<Message> {
    let event = IcedEvent::Keyboard(key_with(named, keyboard::Modifiers::empty()));
    ui.run(h.screen.view(), [event])
}

/// `named` pressed, and what it published fed back to the page.
fn tap(h: &mut Harness, ui: &mut LiveUi, named: Named) -> Vec<Message> {
    let messages = press(h, ui, named);
    h.feed(messages.clone());
    messages
}

fn plays(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        matches!(
            m,
            Message::Transport(detail_transport_bar::Message::TogglePlay)
        )
    })
}

/// The page with Take 1's clip selected.
fn clip_selected() -> (Harness, String) {
    let mut h = Harness::open(Setup::default());
    let clip = h.track("t1").clips[0].id.clone();
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::SelectClip(clip.clone()),
    });
    assert_eq!(h.vm().selected_clip_id(), Some(clip.as_str()));
    (h, clip)
}

#[test]
fn a_plain_first_press_is_a_shortcut_and_a_modified_or_repeated_one_is_not() {
    let none = keyboard::Modifiers::empty();
    assert_eq!(
        shortcut(&key_with(Named::Space, none)),
        Some(Shortcut::PlayPause)
    );
    assert_eq!(
        shortcut(&key_with(Named::Backspace, none)),
        Some(Shortcut::DeleteClip)
    );
    assert_eq!(
        shortcut(&key_with(Named::Delete, none)),
        Some(Shortcut::DeleteClip)
    );
    assert_eq!(
        shortcut(&key_with(Named::Escape, none)),
        Some(Shortcut::Deselect)
    );
    for modifiers in [
        keyboard::Modifiers::LOGO,
        keyboard::Modifiers::CTRL,
        keyboard::Modifiers::ALT,
        keyboard::Modifiers::SHIFT,
    ] {
        assert_eq!(shortcut(&key_with(Named::Backspace, modifiers)), None);
    }
    let mut held = key_with(Named::Space, none);
    if let keyboard::Event::KeyPressed { repeat, .. } = &mut held {
        *repeat = true;
    }
    assert_eq!(shortcut(&held), None);
    assert_eq!(shortcut(&key_with(Named::Enter, none)), None);
}

// Space did nothing on the page: Play was the transport button alone.
#[test]
fn space_plays_and_pauses_as_the_play_button_does() {
    let mut h = Harness::open(Setup::default());
    let mut ui = LiveUi::new(h.size);
    assert!(plays(&tap(&mut h, &mut ui, Named::Space)));
    assert!(h.vm().playback().is_playing());
    assert!(h.shows("Pause"));
    assert!(plays(&tap(&mut h, &mut ui, Named::Space)));
    assert!(!h.vm().playback().is_playing());
    assert!(h.shows("Play"));
}

#[test]
fn space_does_nothing_while_the_play_button_is_dead() {
    // Nothing to play.
    let mut h = Harness::open(Setup {
        practice: practice(Vec::new()),
        ..Setup::default()
    });
    let mut ui = LiveUi::new(h.size);
    assert!(!plays(&tap(&mut h, &mut ui, Named::Space)));
    assert!(!h.vm().playback().is_playing());

    // REC's arm window: the backing it started jumped back to the take's
    // start the moment the mic armed.
    let ready = Gate::new();
    let (mut h, _mic) = take_flow(Some(&ready), ok_analyze());
    h.send(Message::Transport(
        detail_transport_bar::Message::StartRecording,
    ));
    h.pump_until(|h| h.repo.stored().vocal_tracks.len() == 2);
    assert!(h.vm().recording().is_starting_take());
    let mut ui = LiveUi::new(h.size);
    assert!(!plays(&press(&h, &mut ui, Named::Space)));
    ready.open();
    h.settle();
}

#[test]
fn delete_and_backspace_ask_to_delete_the_selected_clip_as_its_menu_does() {
    let (mut h, clip) = clip_selected();
    // The menu's question, to compare with.
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ClipContextMenu(clip.clone()),
    });
    h.click("Delete clip");
    let Dialog::Confirm(by_menu) = h.dialog().clone() else {
        panic!("a confirmation");
    };
    h.answer(DialogResult::Confirmed(false));

    let mut ui = LiveUi::new(h.size);
    for key in [Named::Delete, Named::Backspace] {
        let messages = tap(&mut h, &mut ui, key);
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::DeleteSelectedClip)),
            "{key:?}: {messages:?}"
        );
        let Dialog::Confirm(by_key) = h.dialog().clone() else {
            panic!("a confirmation");
        };
        assert_eq!(
            (&by_key.title, &by_key.body, &by_key.confirm_label),
            (&by_menu.title, &by_menu.body, &by_menu.confirm_label),
            "{key:?}"
        );
        h.answer(DialogResult::Confirmed(false));
    }
    assert!(h.track("t1").clip_by_id(&clip).is_some(), "asked, not done");

    // Confirmed, it goes.
    tap(&mut h, &mut ui, Named::Backspace);
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert!(h
        .vm()
        .track_by_id("t1")
        .is_none_or(|t| t.clip_by_id(&clip).is_none()));
}

#[test]
fn delete_with_no_clip_selected_does_nothing() {
    let mut h = Harness::open(Setup::default());
    let mut ui = LiveUi::new(h.size);
    assert!(h.vm().selected_clip_id().is_none());
    assert!(tap(&mut h, &mut ui, Named::Delete).is_empty());
    assert!(h.dialog.is_none());
}

#[test]
fn escape_lets_go_of_the_clip_and_keeps_its_take() {
    let (mut h, _) = clip_selected();
    let mut ui = LiveUi::new(h.size);
    tap(&mut h, &mut ui, Named::Escape);
    assert_eq!(h.vm().selected_clip_id(), None);
    assert_eq!(h.vm().selected_track_id(), Some("t1"));
}

#[test]
fn escape_with_a_menu_open_closes_the_menu_alone() {
    let (mut h, _) = clip_selected();
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::ContextMenu,
    });
    assert!(h.screen.open_menu().is_some());
    let mut ui = LiveUi::new(h.size);
    // The menu holds the keyboard: Space and Delete do nothing either.
    assert!(!plays(&tap(&mut h, &mut ui, Named::Space)));
    assert!(h.dialog.is_none());
    tap(&mut h, &mut ui, Named::Escape);
    assert!(h.screen.open_menu().is_none());
    assert!(
        h.vm().selected_clip_id().is_some(),
        "the clip stays selected"
    );
}

#[test]
fn the_note_editor_holds_the_keyboard() {
    let (mut h, _) = clip_selected();
    h.send(Message::Row {
        track_id: "t1".into(),
        message: track_row::Message::AddNote,
    });
    assert!(h.screen.note_editor().is_some());
    let mut ui = LiveUi::new(h.size);
    assert!(!plays(&tap(&mut h, &mut ui, Named::Space)));
    assert!(!h.vm().playback().is_playing());
    tap(&mut h, &mut ui, Named::Backspace);
    assert!(h.dialog.is_none(), "no delete asked");
}

// A focused field's keys are its own: Space types, and Escape, which the
// lyrics editor lets go by as it gives up the focus, does not also let go
// of the clip.
#[test]
fn the_lyrics_editor_keeps_its_keys() {
    let (mut h, _) = clip_selected();
    h.click("Paste lyrics");
    // The editor opens once the clipboard answers (here: empty).
    h.send(Message::Lyrics(lyrics_panel::Message::PastedLyrics(None)));
    let mut ui = LiveUi::new(h.size);
    let field = ui
        .find(h.screen.view(), h.screen.lyrics.editor_id())
        .expect("the lyrics editor")
        .bounds();
    ui.point_at(field.center());
    let focused = ui.run(h.screen.view(), [live_ui::press(), live_ui::release()]);
    h.feed(focused);
    let typed = tap(&mut h, &mut ui, Named::Space);
    assert!(!plays(&typed), "{typed:?}");
    assert!(!h.vm().playback().is_playing());
    assert!(
        typed
            .iter()
            .any(|m| matches!(m, Message::Lyrics(lyrics_panel::Message::EditorAction(_)))),
        "typed into the editor: {typed:?}"
    );
    tap(&mut h, &mut ui, Named::Backspace);
    assert!(h.dialog.is_none(), "no delete asked");
    tap(&mut h, &mut ui, Named::Escape);
    assert!(
        h.vm().selected_clip_id().is_some(),
        "the clip stays selected"
    );
}
