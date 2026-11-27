//! The right panel's tests:
//!
//! - `lyrics_panel_test.dart`: a drop into the open editor re-seeds it with
//!   the import (and a rejected one leaves the edit alone); Save keeps the
//!   editor on a failed write, on text typed while it waited, and for a
//!   session the user has left; an unsaved edit survives a tab switch and a
//!   collapse, and Cancel drops it; the karaoke reader follows the playhead
//!   after a seek, reveals the line when Focus opens mid-song and re-picks
//!   it for new lyrics; the Focus view takes a drop (or says why not) and
//!   holds the keyboard; and an editor made under a dialog leaves it the
//!   keyboard, which a closing dialog hands back only to an editor owed it.
//! - `lyrics_panel_short_height_test.dart`: the editor's and the sync view's
//!   actions stay on a short panel and take their taps; a tall panel keeps
//!   its toolbar and the pinned hint, a short one scrolls the hint.
//!
//! Where Dart drove a fake view model through the widget, these drive the
//! [`LyricsPanelState`] with the data such a fake would hand the panel, and
//! a headless interface for what is on screen ([`PanelUi`]: it keeps the
//! widget tree between views, as the app does, and runs the state's
//! requests on it as the runtime runs its tasks).

use std::sync::Arc;

use iced::advanced::widget::operation::{self, focusable, Outcome};
use iced::widget::text_editor::{Action, Edit};
use iced::{window, Font, Point, Size};
use iced_test::core::clipboard;
use iced_test::core::renderer::Headless;
use iced_test::renderer::Renderer as TestRenderer;
use iced_test::runtime::user_interface::{self, UserInterface};
use iced_test::selector::{Selector, Text as TextTarget};
use iced_test::simulator::{press_key, typewrite};
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::util::dart_date_time::DateTime;

use super::*;

// ---------------------------------------------------------- the harness

/// A headless window that keeps the widget tree between views (see the
/// module docs).
struct PanelUi {
    cache: Option<user_interface::Cache>,
    renderer: TestRenderer,
    size: Size,
    cursor: mouse::Cursor,
}

impl PanelUi {
    fn new(size: Size) -> Self {
        let renderer = iced_test::futures::futures::executor::block_on(TestRenderer::new(
            Font::with_name("Fira Sans"),
            Pixels(16.0),
            None,
        ))
        .expect("a headless renderer");
        Self {
            cache: Some(user_interface::Cache::default()),
            renderer,
            size,
            cursor: mouse::Cursor::Unavailable,
        }
    }

    fn build<'a>(
        &mut self,
        view: Element<'a, Message>,
    ) -> UserInterface<'a, Message, Theme, TestRenderer> {
        UserInterface::build(
            view,
            self.size,
            self.cache.take().unwrap_or_default(),
            &mut self.renderer,
        )
    }

    /// Feeds `events` to `view` one at a time (a move puts the cursor where
    /// it went); what it published.
    fn run<'a>(
        &mut self,
        view: Element<'a, Message>,
        events: impl IntoIterator<Item = Event>,
    ) -> Vec<Message> {
        let mut ui = self.build(view);
        let mut messages = Vec::new();
        for event in events {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = event {
                self.cursor = mouse::Cursor::Available(position);
            }
            let _ = ui.update(
                std::slice::from_ref(&event),
                self.cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
        }
        self.cache = Some(ui.into_cache());
        messages
    }

    /// Runs the panel's requests on `view`, as the runtime runs their tasks;
    /// a reveal's scroll lands where its animation ends.
    fn apply<'a>(&mut self, view: Element<'a, Message>, requests: Vec<PanelRequest>) {
        let mut ui = self.build(view);
        for request in requests {
            let mut op: Box<dyn Operation> = match request {
                PanelRequest::Focus(id) => Box::new(focusable::focus(id)),
                PanelRequest::Reveal {
                    list,
                    line,
                    alignment,
                    ..
                } => {
                    let mut reveal = Reveal::new(list, line, alignment);
                    ui.operate(&self.renderer, &mut operation::black_box(&mut reveal));
                    let Outcome::Some(scroll) = reveal.finish() else {
                        continue;
                    };
                    Box::new(operation::scrollable::scroll_to(
                        scroll.list,
                        operation::scrollable::AbsoluteOffset {
                            x: None,
                            y: Some(scroll.to),
                        },
                    ))
                }
            };
            ui.operate(&self.renderer, op.as_mut());
        }
        self.cache = Some(ui.into_cache());
    }

    /// The first target of `selector` in `view`.
    fn find<'a, S>(&mut self, view: Element<'a, Message>, selector: S) -> Option<S::Output>
    where
        S: Selector + Send,
        S::Output: Clone + Send,
    {
        let mut ui = self.build(view);
        let mut op = selector.find();
        ui.operate(&self.renderer, &mut operation::black_box(&mut op));
        self.cache = Some(ui.into_cache());
        match op.finish() {
            Outcome::Some(output) => output,
            _ => None,
        }
    }

    /// Whether the widget `id` has the keyboard.
    fn is_focused<'a>(&mut self, view: Element<'a, Message>, id: widget::Id) -> bool {
        let mut ui = self.build(view);
        let mut op = focusable::is_focused(id);
        ui.operate(&self.renderer, &mut operation::black_box(&mut op));
        self.cache = Some(ui.into_cache());
        matches!(op.finish(), Outcome::Some(true))
    }

    /// Whether `label` is laid out inside its list's viewport (its middle
    /// shows) — not merely in the list.
    fn on_screen<'a>(&mut self, view: Element<'a, Message>, label: &str) -> bool {
        let Some(t) = self.find(view, label) else {
            return false;
        };
        t.visible_bounds()
            .is_some_and(|v| v.height >= t.bounds().height / 2.0)
    }
}

/// A frame: the runtime's redraw request reaching the widgets.
fn redraw() -> Event {
    Event::Window(window::Event::RedrawRequested(std::time::Instant::now()))
}

/// A drop of `paths` at `at`, the way the platform reports one.
fn drop_events(at: Point, paths: &[&str]) -> Vec<Event> {
    let mut events = vec![Event::Mouse(mouse::Event::CursorMoved { position: at })];
    for p in paths {
        events.push(Event::Window(window::Event::FileHovered(PathBuf::from(p))));
    }
    for p in paths {
        events.push(Event::Window(window::Event::FileDropped(PathBuf::from(p))));
    }
    events.push(redraw());
    events
}

fn space() -> Event {
    press_key(
        keyboard::Key::Named(keyboard::key::Named::Space),
        Some(" ".into()),
    )
}

fn tab_key() -> Event {
    press_key(keyboard::Key::Named(keyboard::key::Named::Tab), None)
}

/// The panel's message count of `kind`.
fn count(messages: &[Message], kind: &Message) -> usize {
    messages.iter().filter(|m| *m == kind).count()
}

// ------------------------------------------------------- the fake model

/// What a fake detail view model hands the panel (`_FakeVm`, `_ReaderVm`,
/// `_KeysVm`), with the view model's own rules where the tests lean on
/// them.
struct Vm {
    open: bool,
    tab: RightTab,
    mode: LyricsMode,
    focus: bool,
    text: String,
    lyrics: Vec<LyricLine>,
    draft: Option<String>,
    session: u64,
    sync_index: i64,
    position_ms: i64,
    under_dialog: bool,
    revision: u64,
    track: Option<AudioTrack>,
}

impl Vm {
    /// The panel in EDIT mode over one line of lyrics.
    fn editing() -> Self {
        let mut vm = Self::reading(vec![LyricLine::new("old line", None)]);
        vm.text = "old line".into();
        vm.set_lyrics_mode(LyricsMode::Edit);
        vm
    }

    /// The panel reading `lyrics`.
    fn reading(lyrics: Vec<LyricLine>) -> Self {
        Self {
            open: true,
            tab: RightTab::Lyrics,
            mode: LyricsMode::Read,
            focus: false,
            text: lyrics
                .iter()
                .map(|l| l.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            lyrics,
            draft: None,
            session: 0,
            sync_index: 0,
            position_ms: 0,
            under_dialog: false,
            revision: 0,
            track: None,
        }
    }

    /// As the real view model: leaving the editor (Save, Cancel) drops its
    /// draft, and opening it starts a new editing session.
    fn set_lyrics_mode(&mut self, mode: LyricsMode) {
        if mode == self.mode {
            return;
        }
        if mode != LyricsMode::Edit {
            self.draft = None;
        }
        if mode == LyricsMode::Edit {
            self.session += 1;
        }
        self.mode = mode;
        self.revision += 1;
    }

    /// Mimics the real view model: only a valid lyrics file replaces the
    /// lyrics; a rejected drop (wrong type, unreadable, busy) toasts and
    /// changes nothing.
    fn import(&mut self, path: &str) {
        if path.ends_with(".lrc") {
            self.text = "dropped line one\ndropped line two".into();
            self.lyrics = vec![
                LyricLine::new("dropped line one", None),
                LyricLine::new("dropped line two", None),
            ];
            self.revision += 1;
        }
    }

    /// The same pick as the real view model: the synced line with the
    /// latest time at or before the playhead.
    fn current_lyric_index(&self) -> i64 {
        let (mut idx, mut best) = (-1, -1);
        for (i, l) in self.lyrics.iter().enumerate() {
            if let Some(t) = l.time_ms {
                if t <= self.position_ms && t >= best {
                    best = t;
                    idx = i as i64;
                }
            }
        }
        idx
    }

    fn data<'a>(&'a self, as_text: &'a dyn Fn() -> String) -> RightPanelData<'a> {
        RightPanelData {
            lyrics_open: self.open,
            right_tab: self.tab,
            lyrics_mode: self.mode,
            lyrics_focus: self.focus,
            has_lyrics: !self.lyrics.is_empty(),
            lyrics: &self.lyrics,
            lyrics_as_text: as_text,
            lyrics_draft: self.draft.as_deref(),
            lyrics_edit_session: self.session,
            sync_index: self.sync_index,
            current_lyric_index: self.current_lyric_index(),
            selected_track: self.track.as_ref(),
            selected_track_notes: self.track.as_ref().map_or(&[], |t| &t.notes),
            selected_track_techniques: self.track.as_ref().map_or(&[], |t| &t.technique_markers),
            all_notes: &[],
            under_dialog: self.under_dialog,
            revision: self.revision,
        }
    }
}

/// `vm`'s data in `$data`, for the statements that follow.
macro_rules! with_data {
    ($vm:expr, $data:ident, $body:block) => {{
        let text = $vm.text.clone();
        let as_text = move || text.clone();
        let $data = $vm.data(&as_text);
        $body
    }};
}

/// Syncs the panel to `vm` and runs what it asks on the panel's view.
fn sync(ui: &mut PanelUi, state: &mut LyricsPanelState, vm: &Vm) {
    let requests = with_data!(vm, data, { state.sync_requests(&data) });
    with_data!(vm, data, {
        ui.apply(right_panel(state, data), requests);
    });
}

/// The editor's text, as the panel shows it.
fn editor_text(state: &LyricsPanelState) -> String {
    state.editor_text().expect("the editor is open")
}

/// `tester.enterText(editor, text)`: replaces the editor's text; the edit is
/// the view model's draft.
fn enter_text(state: &mut LyricsPanelState, vm: &mut Vm, text: &str) {
    let _ = state.perform(Action::SelectAll);
    if let Some(draft) = state.perform(Action::Edit(Edit::Paste(Arc::new(text.to_string())))) {
        vm.draft = Some(draft);
    }
}

/// Save tapped: the ticket and the text handed to `applyEditedLyrics`.
fn tap_save(state: &LyricsPanelState, vm: &Vm) -> SaveTicket {
    with_data!(vm, data, { state.save_ticket(&data) }).expect("an open editor")
}

/// The write landed (`saved`): the view model takes the text, and the panel
/// leaves edit mode if it should.
fn save_lands(state: &LyricsPanelState, vm: &mut Vm, ticket: &SaveTicket, saved: bool) {
    if saved {
        vm.text = ticket.text().to_string();
    }
    let close = with_data!(vm, data, { state.save_landed(ticket, saved, &data) });
    if close {
        vm.set_lyrics_mode(LyricsMode::Read);
    }
}

/// A drop (or a picked file) of `path`, as the screen runs it.
fn import(state: &mut LyricsPanelState, vm: &mut Vm, path: &str) {
    let ticket = with_data!(vm, data, { state.begin_import(&data) });
    vm.import(path);
    if with_data!(vm, data, { state.end_import(ticket, &data) }) {
        vm.draft = None;
    }
}

const PANEL: Size = Size {
    width: 1440.0,
    height: 900.0,
};

// ---------------------------------------------------- lyrics_panel_test

#[test]
fn a_drop_while_editing_reseeds_the_editor_with_the_import() {
    // Dropping a .lrc while the editor was open imported into the view
    // model but left the editor on its stale snapshot — Save then silently
    // overwrote the just-imported lyrics.
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    assert_eq!(editor_text(&state), "old line");

    // The user has unsaved edits in the open editor.
    enter_text(&mut state, &mut vm, "user typed draft");
    assert_eq!(editor_text(&state), "user typed draft");
    assert_eq!(vm.draft.as_deref(), Some("user typed draft"));

    // The drop lands on the panel's body.
    let dropped = with_data!(vm, data, {
        ui.run(
            right_panel(&state, data),
            drop_events(Point::new(150.0, 400.0), &["/tmp/song.lrc"]),
        )
    });
    assert_eq!(
        dropped,
        vec![Message::ImportLyricsFromFile("/tmp/song.lrc".into())]
    );
    import(&mut state, &mut vm, "/tmp/song.lrc");
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        editor_text(&state),
        "dropped line one\ndropped line two",
        "the open editor must show the imported lyrics, not a stale snapshot that Save would \
         write back over the import"
    );

    // Save now applies the imported content — no silent overwrite.
    let ticket = tap_save(&state, &vm);
    assert_eq!(ticket.text(), "dropped line one\ndropped line two");
}

#[test]
fn a_failed_save_keeps_the_editor_open_on_the_draft() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "user typed draft");
    let ticket = tap_save(&state, &vm);
    assert_eq!(ticket.text(), "user typed draft");
    save_lands(&state, &mut vm, &ticket, false);
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        vm.mode,
        LyricsMode::Edit,
        "a write that didn't land must not close the editor"
    );
    assert_eq!(
        editor_text(&state),
        "user typed draft",
        "the unsaved draft must survive for a retry"
    );
}

#[test]
fn text_typed_while_the_save_waits_stays_in_the_open_editor() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "first verse");
    let ticket = tap_save(&state, &vm);
    enter_text(&mut state, &mut vm, "first verse\nsecond");
    save_lands(&state, &mut vm, &ticket, true);
    sync(&mut ui, &mut state, &vm);
    assert_eq!(ticket.text(), "first verse");
    assert_eq!(vm.mode, LyricsMode::Edit);
    assert_eq!(editor_text(&state), "first verse\nsecond");

    // Saved again with nothing typed after, it closes.
    let ticket = tap_save(&state, &vm);
    save_lands(&state, &mut vm, &ticket, true);
    assert_eq!(ticket.text(), "first verse\nsecond");
    assert_eq!(vm.mode, LyricsMode::Read);
}

#[test]
fn a_save_that_lands_after_the_user_moved_on_to_sync_leaves_sync() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "first verse");
    let ticket = tap_save(&state, &vm);
    // The toolbar's Sync, while the write waits.
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Timer)),
        vec![Message::SetLyricsMode(LyricsMode::Sync)]
    );
    vm.set_lyrics_mode(LyricsMode::Sync);
    sync(&mut ui, &mut state, &vm);
    save_lands(&state, &mut vm, &ticket, true);
    assert_eq!(vm.mode, LyricsMode::Sync);
}

#[test]
fn a_late_save_leaves_an_editor_opened_again_in_the_meantime() {
    // Save, then Sync, then Edit before the write landed: the old Save
    // closed the editor the user had just reopened.
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "first verse");
    let ticket = tap_save(&state, &vm);
    vm.set_lyrics_mode(LyricsMode::Sync);
    sync(&mut ui, &mut state, &vm);
    vm.set_lyrics_mode(LyricsMode::Edit);
    sync(&mut ui, &mut state, &vm);
    save_lands(&state, &mut vm, &ticket, true);
    assert_eq!(vm.mode, LyricsMode::Edit);
}

#[test]
fn an_unsaved_edit_survives_a_switch_to_another_tab_and_back() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "user typed draft");

    vm.tab = RightTab::Notes;
    sync(&mut ui, &mut state, &vm);
    assert!(state.editor_text().is_none());
    vm.tab = RightTab::Lyrics;
    sync(&mut ui, &mut state, &vm);
    assert_eq!(editor_text(&state), "user typed draft");
    assert_eq!(vm.text, "old line", "a tab switch saves nothing");
}

#[test]
fn an_unsaved_edit_survives_collapsing_the_panel() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "user typed draft");
    vm.open = false;
    sync(&mut ui, &mut state, &vm);
    vm.open = true;
    sync(&mut ui, &mut state, &vm);
    assert_eq!(editor_text(&state), "user typed draft");
}

#[test]
fn an_unsaved_edit_is_dropped_by_cancel() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "user typed draft");
    let cancel = with_data!(vm, data, {
        let at = ui
            .find(right_panel(&state, data), "Cancel")
            .expect("Cancel")
            .bounds()
            .center();
        ui.run(
            right_panel(&state, data),
            [
                Event::Mouse(mouse::Event::CursorMoved { position: at }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
        )
    });
    assert_eq!(cancel, vec![Message::SetLyricsMode(LyricsMode::Read)]);
    vm.set_lyrics_mode(LyricsMode::Read);
    sync(&mut ui, &mut state, &vm);
    assert!(state.editor_text().is_none());

    vm.set_lyrics_mode(LyricsMode::Edit); // open the editor again
    sync(&mut ui, &mut state, &vm);
    assert_eq!(editor_text(&state), "old line");
}

#[test]
fn a_rejected_drop_leaves_the_in_progress_edit_untouched() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(PANEL);
    sync(&mut ui, &mut state, &vm);
    enter_text(&mut state, &mut vm, "user typed draft");
    import(&mut state, &mut vm, "/tmp/photo.png"); // rejected
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        editor_text(&state),
        "user typed draft",
        "a failed import must not clobber what the user is typing"
    );
    assert_eq!(vm.draft.as_deref(), Some("user typed draft"));
}

/// Eighty lines, one every `step_ms`.
fn timed(step_ms: i64) -> Vec<LyricLine> {
    (0..80)
        .map(|i| LyricLine::new(format!("line {i}"), Some(i * step_ms)))
        .collect()
}

/// The Focus view over `vm` (its reader), in a 900 × 600 window.
fn focus_view<'a>(state: &'a LyricsPanelState, data: RightPanelData<'a>) -> Element<'a, Message> {
    lyrics_focus_overlay(state, data)
}

fn sync_focus(ui: &mut PanelUi, state: &mut LyricsPanelState, vm: &Vm) {
    let requests = with_data!(vm, data, { state.sync_requests(&data) });
    with_data!(vm, data, {
        ui.apply(focus_view(state, data), requests);
    });
}

fn focus_on_screen(ui: &mut PanelUi, state: &LyricsPanelState, vm: &Vm, label: &str) -> bool {
    with_data!(vm, data, { ui.on_screen(focus_view(state, data), label) })
}

#[test]
fn the_karaoke_reader_follows_the_playhead_after_a_seek() {
    let mut vm = Vm::reading(timed(1000));
    vm.focus = true;
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(900.0, 600.0));
    sync_focus(&mut ui, &mut state, &vm);
    vm.position_ms = 3000;
    sync_focus(&mut ui, &mut state, &vm);
    assert!(focus_on_screen(&mut ui, &state, &vm, "line 3"));

    // A ruler click / transport seek to 0:50…
    vm.position_ms = 50000;
    sync_focus(&mut ui, &mut state, &vm);
    assert!(focus_on_screen(&mut ui, &state, &vm, "line 50"));
    assert!(!focus_on_screen(&mut ui, &state, &vm, "line 3"));

    // …and playback carries on from there.
    for s in 51..=55 {
        vm.position_ms = s * 1000;
        sync_focus(&mut ui, &mut state, &vm);
    }
    assert!(focus_on_screen(&mut ui, &state, &vm, "line 55"));
    assert_eq!(state.focus_line(), Some(55));
}

#[test]
fn opening_focus_mid_song_reveals_the_active_line() {
    let mut vm = Vm::reading(timed(1000));
    vm.position_ms = 40000;
    vm.focus = true;
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(900.0, 600.0));
    sync_focus(&mut ui, &mut state, &vm);
    assert!(focus_on_screen(&mut ui, &state, &vm, "line 40"));
    assert_eq!(state.focus_line(), Some(40), "highlighted");
}

#[test]
fn new_lyrics_repick_and_reveal_the_line_under_the_playhead() {
    // The highlighted index belonged to the OLD lyrics: after an import or
    // edit under a paused playhead it lit whatever line now sat at that
    // index, and stayed there until the playhead next crossed a line.
    let mut vm = Vm::reading(timed(1000));
    vm.position_ms = 5000;
    vm.focus = true;
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(900.0, 600.0));
    sync_focus(&mut ui, &mut state, &vm);
    assert_eq!(state.focus_line(), Some(5));

    // Re-timed lyrics: 5 s is now line 50's.
    vm.lyrics = timed(100);
    sync_focus(&mut ui, &mut state, &vm);
    assert!(focus_on_screen(&mut ui, &state, &vm, "line 50"));
    assert_eq!(state.focus_line(), Some(50));

    // Lyrics equal by value (an unrelated rebuild of the practice) keep the
    // latch, and request no scroll.
    vm.lyrics = timed(100);
    let requests = with_data!(vm, data, { state.sync_requests(&data) });
    assert!(requests.is_empty());
}

#[test]
fn the_panel_reader_follows_the_playhead_too() {
    let mut vm = Vm::reading(timed(1000));
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 500.0));
    sync(&mut ui, &mut state, &vm);
    assert_eq!(state.reader_line(), Some(0));
    vm.position_ms = 60000;
    sync(&mut ui, &mut state, &vm);
    assert_eq!(state.reader_line(), Some(60));
    let shown = with_data!(vm, data, {
        ui.on_screen(right_panel(&state, data), "line 60")
    });
    assert!(shown);
    // A synced line seeks to itself.
    let tapped = with_data!(vm, data, {
        let at = ui
            .find(right_panel(&state, data), "line 60")
            .expect("line 60")
            .visible_bounds()
            .expect("visible")
            .center();
        ui.run(
            right_panel(&state, data),
            [
                Event::Mouse(mouse::Event::CursorMoved { position: at }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
        )
    });
    assert_eq!(
        tapped,
        vec![Message::SeekToLyric(LyricLine::new("line 60", Some(60000)))]
    );
}

#[test]
fn a_lyrics_file_dropped_on_the_focus_overlay_is_imported() {
    let mut vm = Vm::reading(timed(1000));
    vm.focus = true;
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(900.0, 600.0));
    sync_focus(&mut ui, &mut state, &vm);
    let dropped = with_data!(vm, data, {
        ui.run(
            focus_view(&state, data),
            drop_events(Point::new(450.0, 300.0), &["/tmp/song.lrc"]),
        )
    });
    assert_eq!(
        dropped,
        vec![Message::ImportLyricsFromFile("/tmp/song.lrc".into())]
    );
}

#[test]
fn while_editing_the_focus_overlay_says_why_it_takes_no_drop() {
    // The editor kept under the overlay would Save its text over the import.
    let mut vm = Vm::reading(timed(1000));
    vm.focus = true;
    vm.set_lyrics_mode(LyricsMode::Edit);
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(900.0, 600.0));
    sync_focus(&mut ui, &mut state, &vm);
    let dropped = with_data!(vm, data, {
        ui.run(
            focus_view(&state, data),
            drop_events(Point::new(450.0, 300.0), &["/tmp/song.lrc"]),
        )
    });
    assert_eq!(dropped.len(), 1);
    assert!(matches!(&dropped[0], Message::Toast(t) if t.contains("Exit focus")));
}

/// The panel with the Focus view stacked over it while `vm.focus`, as the
/// workspace stacks them.
fn under_focus<'a>(state: &'a LyricsPanelState, data: RightPanelData<'a>) -> Element<'a, Message> {
    let panel = container(right_panel(state, data)).height(700);
    if data.lyrics_focus {
        stack![panel, lyrics_focus_overlay(state, data)].into()
    } else {
        stack![panel].into()
    }
}

fn sync_under_focus(ui: &mut PanelUi, state: &mut LyricsPanelState, vm: &Vm) {
    let requests = with_data!(vm, data, { state.sync_requests(&data) });
    with_data!(vm, data, {
        ui.apply(under_focus(state, data), requests);
    });
}

/// Five unsynced lines (`_KeysVm`).
fn five_lines() -> Vec<LyricLine> {
    (0..5)
        .map(|i| LyricLine::new(format!("line {i}"), None))
        .collect()
}

#[test]
fn space_stamps_nothing_hidden_beneath_the_focus_view_and_again_after() {
    let mut vm = Vm::reading(five_lines());
    vm.set_lyrics_mode(LyricsMode::Sync);
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(1200.0, 800.0));
    sync_under_focus(&mut ui, &mut state, &vm);
    let keys = |ui: &mut PanelUi, state: &LyricsPanelState, vm: &Vm, events: Vec<Event>| {
        with_data!(vm, data, { ui.run(under_focus(state, data), events) })
    };
    let stamped = keys(&mut ui, &state, &vm, vec![space()]);
    assert_eq!(count(&stamped, &Message::StampNextLyric), 1);

    vm.focus = true;
    sync_under_focus(&mut ui, &mut state, &vm);
    let stamped = keys(&mut ui, &state, &vm, vec![space(), space()]);
    assert_eq!(
        count(&stamped, &Message::StampNextLyric),
        0,
        "the hidden sync view took the keys"
    );
    // Nor does Tab walk back to it.
    let stamped = keys(&mut ui, &state, &vm, vec![tab_key(), space()]);
    assert_eq!(
        count(&stamped, &Message::StampNextLyric),
        0,
        "Tab handed the hidden sync view the keys"
    );

    vm.focus = false;
    sync_under_focus(&mut ui, &mut state, &vm);
    let stamped = keys(&mut ui, &state, &vm, vec![space()]);
    assert_eq!(
        count(&stamped, &Message::StampNextLyric),
        1,
        "the sync view has the keyboard back"
    );
}

#[test]
fn typing_reaches_no_hidden_editor_which_keeps_the_keyboard() {
    let mut vm = Vm::reading(five_lines());
    vm.set_lyrics_mode(LyricsMode::Edit);
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(1200.0, 800.0));
    sync_under_focus(&mut ui, &mut state, &vm);
    let editor = state.editor_id();
    let focused = with_data!(vm, data, {
        ui.is_focused(under_focus(&state, data), editor.clone())
    });
    assert!(focused, "the editor took the keyboard when it opened");

    vm.focus = true;
    sync_under_focus(&mut ui, &mut state, &vm);
    let typed = with_data!(vm, data, {
        let mut events: Vec<Event> = typewrite("ab").collect();
        events.push(tab_key());
        ui.run(under_focus(&state, data), events)
    });
    assert!(
        !typed.iter().any(|m| matches!(m, Message::EditorAction(_))),
        "the typing went into the hidden editor: {typed:?}"
    );

    vm.focus = false;
    sync_under_focus(&mut ui, &mut state, &vm);
    let typed = with_data!(vm, data, {
        assert!(ui.is_focused(under_focus(&state, data), editor.clone()));
        ui.run(under_focus(&state, data), typewrite("c"))
    });
    assert!(
        typed
            .iter()
            .any(|m| matches!(m, Message::EditorAction(a) if a.is_edit())),
        "the editor has the keyboard back"
    );
}

#[test]
fn a_dialog_closing_over_an_unfocused_editor_leaves_it_so() {
    // Every dialog over the editor counted as owing it the keyboard: one the
    // user had clicked away from took it back as the dialog closed.
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let first = with_data!(vm, data, { state.sync_requests(&data) });
    assert_eq!(first, vec![PanelRequest::Focus(state.editor_id())]);
    vm.under_dialog = true;
    assert!(with_data!(vm, data, { state.sync_requests(&data) }).is_empty());
    vm.under_dialog = false;
    assert!(
        with_data!(vm, data, { state.sync_requests(&data) }).is_empty(),
        "the editor took the keyboard"
    );
}

#[test]
fn an_import_landing_under_a_dialog_leaves_it_the_keyboard() {
    // The editor is made again to show an import, and its autofocus took
    // the keys from a dialog opened meanwhile: the name typed into a rename
    // went into the hidden lyrics.
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let _ = with_data!(vm, data, { state.sync_requests(&data) });
    let ticket = with_data!(vm, data, { state.begin_import(&data) });
    vm.under_dialog = true; // a rename opens while the import runs
    let _ = with_data!(vm, data, { state.sync_requests(&data) });
    vm.import("/tmp/song.lrc");
    if with_data!(vm, data, { state.end_import(ticket, &data) }) {
        vm.draft = None;
    }
    let remade = with_data!(vm, data, { state.sync_requests(&data) });
    assert!(remade.is_empty(), "the hidden editor took the keys");
    assert_eq!(editor_text(&state), "dropped line one\ndropped line two");

    // Closed, the dialog hands the keyboard to the editor it covered.
    vm.under_dialog = false;
    let closed = with_data!(vm, data, { state.sync_requests(&data) });
    assert_eq!(closed, vec![PanelRequest::Focus(state.editor_id())]);
}

// ---------------------------------------- lyrics_panel_short_height_test

/// Twelve unsynced lines (the short-height fake).
fn twelve_lines() -> Vec<LyricLine> {
    (0..12)
        .map(|i| LyricLine::new(format!("line {i}"), None))
        .collect()
}

/// The panel as the workspace lays it out: as tall as the dock leaves it.
fn short_panel(mode: LyricsMode, height: f32) -> (Vm, LyricsPanelState, PanelUi) {
    let mut vm = Vm::reading(twelve_lines());
    vm.text = "line 0\nline 1".into();
    vm.set_lyrics_mode(mode);
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(1440.0, height));
    sync(&mut ui, &mut state, &vm);
    (vm, state, ui)
}

fn bounds_of(
    ui: &mut PanelUi,
    state: &LyricsPanelState,
    vm: &Vm,
    label: &str,
) -> Option<TextTarget> {
    with_data!(vm, data, { ui.find(right_panel(state, data), label) })
}

#[test]
fn edit_and_sync_keep_their_actions_on_screen_on_a_short_panel() {
    let actions: [(LyricsMode, &[&str]); 2] = [
        (LyricsMode::Edit, &["Cancel", "Save"]),
        (LyricsMode::Sync, &["Clear", "Tap (Space)", "Done"]),
    ];
    for (mode, labels) in actions {
        // 173: the minimum window with the dock open at its default height.
        // 120: the timeline's floor, the dock dragged as tall as it goes.
        for height in [173.0, 120.0] {
            let (vm, state, mut ui) = short_panel(mode, height);
            for label in labels {
                let t = bounds_of(&mut ui, &state, &vm, label).expect(label);
                let b = t.bounds();
                assert!(
                    b.y >= 0.0 && b.y + b.height <= height && b.x + b.width <= PANEL_WIDTH,
                    "{label} at {b:?} lies outside the {height} px panel ({mode:?})"
                );
            }
        }
    }
}

/// Taps `label`'s middle.
fn tap_label(ui: &mut PanelUi, state: &LyricsPanelState, vm: &Vm, label: &str) -> Vec<Message> {
    let at = bounds_of(ui, state, vm, label)
        .expect(label)
        .bounds()
        .center();
    with_data!(vm, data, {
        ui.run(
            right_panel(state, data),
            [
                Event::Mouse(mouse::Event::CursorMoved { position: at }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
        )
    })
}

#[test]
fn at_the_floor_the_actions_still_take_their_taps() {
    let (vm, state, mut ui) = short_panel(LyricsMode::Sync, 120.0);
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Tap (Space)"),
        vec![Message::StampNextLyric]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Done"),
        vec![Message::SetLyricsMode(LyricsMode::Read)]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Clear"),
        vec![Message::ClearLyricTimes]
    );

    let (vm, state, mut ui) = short_panel(LyricsMode::Edit, 120.0);
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Save"),
        vec![Message::ApplyEditedLyrics]
    );
}

/// The toolbar's sync toggle (its Lucide glyph).
fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

/// Whether the hint is laid out inside the sync list (it scrolls with the
/// lines) rather than pinned above it.
fn hint_scrolls(ui: &mut PanelUi, state: &LyricsPanelState, vm: &Vm) -> bool {
    let hint = bounds_of(ui, state, vm, SYNC_HINT).expect("the hint");
    let line = bounds_of(ui, state, vm, "line 0").expect("the first line");
    // Pinned, the hint sits above the list, outside it; scrolling, it is the
    // list's first item, above its lines.
    let list = with_data!(vm, data, {
        ui.find(right_panel(state, data), state.list_id(List::Sync))
    })
    .expect("the sync list")
    .bounds();
    assert!(line.bounds().y >= list.y && hint.bounds().y < line.bounds().y);
    hint.bounds().y >= list.y
}

#[test]
fn a_tall_panel_keeps_its_toolbar_and_the_pinned_sync_hint() {
    let (vm, state, mut ui) = short_panel(LyricsMode::Sync, 600.0);
    assert!(bounds_of(&mut ui, &state, &vm, &glyph(Icon::Timer)).is_some());
    assert!(
        !hint_scrolls(&mut ui, &state, &vm),
        "the hint stays pinned above the lines"
    );

    let (vm, state, mut ui) = short_panel(LyricsMode::Edit, 600.0);
    assert!(bounds_of(&mut ui, &state, &vm, &glyph(Icon::Pencil)).is_some());
    // The editor keeps its normal spacing: Save's button ends 12 px above
    // the panel's bottom.
    let save = bounds_of(&mut ui, &state, &vm, "Save")
        .expect("Save")
        .bounds();
    let button_bottom: f32 =
        save.center_y() + crate::ui::core::widgets::studio_button::HEIGHT / 2.0;
    assert!(
        (600.0 - button_bottom - 12.0).abs() < 0.5,
        "Save ends at {button_bottom}"
    );
}

#[test]
fn the_sync_hint_scrolls_with_the_lines_on_a_short_panel() {
    let (vm, state, mut ui) = short_panel(LyricsMode::Sync, 173.0);
    assert!(hint_scrolls(&mut ui, &state, &vm));
    // The tab keeps its toolbar down to 128 px, then drops it.
    assert!(bounds_of(&mut ui, &state, &vm, &glyph(Icon::Timer)).is_some());
    let (vm, state, mut ui) = short_panel(LyricsMode::Sync, 120.0);
    assert!(bounds_of(&mut ui, &state, &vm, &glyph(Icon::Timer)).is_none());
    assert!(hint_scrolls(&mut ui, &state, &vm));
}

// ------------------------------------------------------------- the pieces

#[test]
fn sync_times_read_minutes_seconds_and_hundredths() {
    assert_eq!(sync_time(0), "00:00.00");
    assert_eq!(sync_time(61_234), "01:01.23");
    assert_eq!(sync_time(3_599_999), "59:59.99");
    assert_eq!(sync_time(6_000_000), "100:00.00");
}

#[test]
fn a_revealed_line_sits_at_its_alignment_within_the_list() {
    let viewport = Rectangle::new(Point::new(0.0, 100.0), Size::new(300.0, 500.0));
    let content = Rectangle::new(Point::new(0.0, 100.0), Size::new(300.0, 3000.0));
    let line = |y: f32| Rectangle::new(Point::new(0.0, 100.0 + y), Size::new(300.0, 20.0));
    // 0.4 of the room around the line above it.
    assert_eq!(
        reveal_offset(viewport, content, line(1000.0), 0.4),
        1000.0 - 480.0 * 0.4
    );
    // Clamped to the list's extent.
    assert_eq!(reveal_offset(viewport, content, line(10.0), 0.4), 0.0);
    assert_eq!(reveal_offset(viewport, content, line(2980.0), 0.5), 2500.0);
    // A list shorter than its viewport never scrolls.
    let short = Rectangle::new(Point::new(0.0, 100.0), Size::new(300.0, 200.0));
    assert_eq!(reveal_offset(viewport, short, line(150.0), 0.5), 0.0);
}

#[test]
fn the_sync_list_recentres_its_target_on_each_notification_not_when_built() {
    let mut vm = Vm::reading(twelve_lines());
    vm.set_lyrics_mode(LyricsMode::Sync);
    let mut state = LyricsPanelState::new();
    assert!(with_data!(vm, data, { state.sync_requests(&data) }).is_empty());
    // A position tick is no notification.
    vm.position_ms = 500;
    assert!(with_data!(vm, data, { state.sync_requests(&data) }).is_empty());
    vm.sync_index = 7;
    vm.revision += 1;
    let requests = with_data!(vm, data, { state.sync_requests(&data) });
    assert_eq!(
        requests,
        vec![PanelRequest::Reveal {
            list: state.list_id(List::Sync),
            line: state.line_id(List::Sync, 7),
            alignment: 0.5,
            motion: ScrollMotion::SYNC,
        }]
    );
    // Past the last line: nothing to reveal.
    vm.sync_index = 12;
    vm.revision += 1;
    assert!(with_data!(vm, data, { state.sync_requests(&data) }).is_empty());
}

#[test]
fn only_edits_that_change_the_text_are_drafts() {
    let mut vm = Vm::editing();
    let mut state = LyricsPanelState::new();
    let _ = with_data!(vm, data, { state.sync_requests(&data) });
    assert_eq!(state.perform(Action::SelectAll), None);
    assert_eq!(state.perform(Action::Move(text_editor::Motion::Home)), None);
    // A backspace at the very start changes nothing.
    assert_eq!(state.perform(Action::Edit(Edit::Backspace)), None);
    assert_eq!(
        state.perform(Action::Edit(Edit::Insert('X'))),
        Some("Xold line".to_string())
    );
    // With no editor, nothing happens.
    vm.set_lyrics_mode(LyricsMode::Read);
    let _ = with_data!(vm, data, { state.sync_requests(&data) });
    assert_eq!(state.perform(Action::Edit(Edit::Insert('Y'))), None);
}

fn at() -> DateTime {
    DateTime::utc_ymd(2026, 1, 1)
}

fn annotated_take(kind: AudioTrackKind) -> AudioTrack {
    let mut t = AudioTrack::new("t1", kind, "Take 1", at());
    t.notes = vec![PracticeNote {
        id: "n1".into(),
        position_ms: 12_000,
        end_ms: Some(15_000),
        content: "breathy onset".into(),
        created_at: at(),
        updated_at: at(),
    }];
    t.technique_markers = vec![TechniqueMarker {
        id: "m1".into(),
        position_ms: 3_000,
        end_ms: None,
        technique_id: "tech".into(),
        technique_name: "Twang".into(),
        created_at: at(),
        updated_at: at(),
    }];
    t
}

#[test]
fn a_note_tile_seeks_then_edits_its_note() {
    let mut vm = Vm::reading(Vec::new());
    vm.tab = RightTab::Notes;
    vm.track = Some(annotated_take(AudioTrackKind::Vocal));
    let state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    assert!(bounds_of(&mut ui, &state, &vm, "00:12 → 00:15").is_some());
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "breathy onset"),
        vec![
            Message::Seek(Duration::from_millis(12_000)),
            Message::EditNote {
                track_id: "t1".into(),
                note_id: "n1".into()
            },
        ]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Plus)),
        vec![Message::AddNote("t1".into())]
    );
}

#[test]
fn a_technique_tile_seeks_and_only_its_button_removes() {
    let mut vm = Vm::reading(Vec::new());
    vm.tab = RightTab::Techniques;
    vm.track = Some(annotated_take(AudioTrackKind::Vocal));
    let state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Twang"),
        vec![Message::Seek(Duration::from_millis(3_000))]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Trash2)),
        vec![Message::EditTechnique {
            track_id: "t1".into(),
            marker_id: "m1".into()
        }]
    );
}

#[test]
fn a_backing_track_takes_no_notes_or_techniques() {
    for tab in [RightTab::Notes, RightTab::Techniques] {
        let mut vm = Vm::reading(Vec::new());
        vm.tab = tab;
        vm.track = Some(annotated_take(AudioTrackKind::Accompaniment));
        let state = LyricsPanelState::new();
        let mut ui = PanelUi::new(Size::new(300.0, 600.0));
        let hint = if tab == RightTab::Notes {
            "Notes apply to vocal takes only."
        } else {
            "Techniques apply to vocal takes only."
        };
        assert!(bounds_of(&mut ui, &state, &vm, hint).is_some(), "{tab:?}");
        assert!(bounds_of(&mut ui, &state, &vm, &glyph(Icon::Plus)).is_none());
    }
}

#[test]
fn with_nothing_selected_the_tabs_say_what_to_do() {
    let mut vm = Vm::reading(Vec::new());
    let state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    vm.tab = RightTab::Notes;
    assert!(bounds_of(&mut ui, &state, &vm, "ALL NOTES").is_some());
    assert!(bounds_of(
        &mut ui,
        &state,
        &vm,
        "Select a track and pin notes on the timeline."
    )
    .is_some());
    vm.tab = RightTab::Techniques;
    assert!(bounds_of(
        &mut ui,
        &state,
        &vm,
        "Select a take to see the techniques it uses."
    )
    .is_some());
}

#[test]
fn every_note_of_the_song_selects_its_take_then_seeks() {
    let take = annotated_take(AudioTrackKind::Vocal);
    let refs = [NoteRef {
        track_id: "t1".into(),
        track_name: "Take 1".into(),
        note: take.notes[0].clone(),
    }];
    let mut vm = Vm::reading(Vec::new());
    vm.tab = RightTab::Notes;
    let vm = vm;
    let state = LyricsPanelState::new();
    let as_text = String::new;
    let data = RightPanelData {
        all_notes: &refs,
        ..vm.data(&as_text)
    };
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    let at = ui
        .find(right_panel(&state, data), "breathy onset")
        .expect("the tile")
        .bounds()
        .center();
    assert!(ui.find(right_panel(&state, data), "Take 1").is_some());
    let tapped = ui.run(
        right_panel(&state, data),
        [
            Event::Mouse(mouse::Event::CursorMoved { position: at }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
    );
    assert_eq!(
        tapped,
        vec![
            Message::SelectTrack("t1".into()),
            Message::Seek(Duration::from_millis(12_000))
        ]
    );
}

#[test]
fn the_tabs_the_rail_and_the_toolbar_say_what_they_do() {
    let mut vm = Vm::reading(timed(1000));
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "NOTES"),
        vec![Message::SetRightTab(RightTab::Notes)]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::PanelRightClose)),
        vec![Message::ToggleLyricsPanel]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Pencil)),
        vec![Message::SetLyricsMode(LyricsMode::Edit)]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Maximize2)),
        vec![Message::SetLyricsFocus(true)]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::FileUp)),
        vec![Message::BrowseImport]
    );
    // In a mode, its toggle leads back to reading.
    vm.set_lyrics_mode(LyricsMode::Sync);
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::Timer)),
        vec![Message::SetLyricsMode(LyricsMode::Read)]
    );
    // Collapsed: the rail, which opens it again.
    vm.open = false;
    sync(&mut ui, &mut state, &vm);
    assert_eq!(
        tap_label(&mut ui, &state, &vm, &glyph(Icon::PanelRightOpen)),
        vec![Message::ToggleLyricsPanel]
    );
}

#[test]
fn with_no_lyrics_the_tab_offers_paste_and_import() {
    let vm = Vm::reading(Vec::new());
    let state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    assert!(bounds_of(&mut ui, &state, &vm, "No lyrics yet").is_some());
    assert!(bounds_of(&mut ui, &state, &vm, "or drop a .lrc or .txt file here").is_some());
    assert!(
        bounds_of(&mut ui, &state, &vm, &glyph(Icon::Pencil)).is_none(),
        "no toolbar without lyrics"
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Paste lyrics"),
        vec![Message::SetLyricsMode(LyricsMode::Edit)]
    );
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "Import lyrics file"),
        vec![Message::BrowseImport]
    );
}

#[test]
fn the_sync_list_stamps_the_line_tapped_and_shows_its_times() {
    let mut lines = twelve_lines();
    lines[1].time_ms = Some(61_234);
    let mut vm = Vm::reading(lines);
    vm.set_lyrics_mode(LyricsMode::Sync);
    let mut state = LyricsPanelState::new();
    let mut ui = PanelUi::new(Size::new(300.0, 600.0));
    sync(&mut ui, &mut state, &vm);
    assert!(bounds_of(&mut ui, &state, &vm, "01:01.23").is_some());
    assert!(bounds_of(&mut ui, &state, &vm, "—").is_some());
    assert_eq!(
        tap_label(&mut ui, &state, &vm, "line 3"),
        vec![Message::StampLyricAt(3)]
    );
}

// ------------------------------------------------- the reveal's animation

fn scroll_of(state: &LyricsPanelState, at: std::time::Instant) -> Option<f32> {
    state.scrolls.first().map(|(_, along)| along.value(at))
}

#[test]
fn a_reveal_scrolls_its_list_there_over_its_time_from_where_it_was() {
    use std::time::Instant;
    let mut state = LyricsPanelState::new();
    let list = state.list_id(List::Reader);
    let t0 = Instant::now();
    assert!(!state.is_scrolling(t0), "nothing scrolls: no frames");
    let start = |from, to| ScrollStart {
        scroll: RevealScroll {
            list: list.clone(),
            from,
            to,
        },
        motion: ScrollMotion::READER,
    };
    let _: Task<Message> = state.start_scroll(start(100.0, 420.0), t0);
    assert!(state.is_scrolling(t0 + Duration::from_millis(1)));
    assert_eq!(scroll_of(&state, t0), Some(100.0), "start");
    let half = t0 + Duration::from_millis(160);
    let expected = 100.0 + 320.0 * Curve::EaseOut.transform(0.5);
    let middle = scroll_of(&state, half).expect("scrolling");
    assert!((middle - expected).abs() < 0.5, "middle: {middle}");
    assert_eq!(
        scroll_of(&state, t0 + Duration::from_millis(320)),
        Some(420.0),
        "end"
    );
    // The next line's reveal takes over from where this one got.
    let _: Task<Message> = state.start_scroll(start(0.0, 600.0), half);
    assert!((scroll_of(&state, half).expect("scrolling") - middle).abs() < 0.5);
    // Its last frame lets it go, and the frames stop.
    let _: Task<Message> = state.scroll_frame(half + Duration::from_millis(320));
    assert!(state.scrolls.is_empty());
    assert!(!state.is_scrolling(half + Duration::from_millis(320)));
    // Already there: nothing animates.
    let _: Task<Message> = state.start_scroll(start(50.0, 50.0), t0);
    assert!(state.scrolls.is_empty());
}

#[test]
fn the_readers_ease_out_over_320_ms_and_the_sync_list_eases_over_200() {
    assert_eq!(
        (ScrollMotion::READER.duration, ScrollMotion::READER.curve),
        (Duration::from_millis(320), Curve::EaseOut)
    );
    assert_eq!(
        (ScrollMotion::SYNC.duration, ScrollMotion::SYNC.curve),
        (Duration::from_millis(200), Curve::Ease)
    );
    let state = LyricsPanelState::new();
    let reader = state.reveal(List::Reader, 2, 5, 0.4);
    assert!(matches!(
        reader,
        Some(PanelRequest::Reveal { motion, .. }) if motion == ScrollMotion::READER
    ));
}
