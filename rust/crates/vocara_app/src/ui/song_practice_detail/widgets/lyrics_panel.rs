//! Port of `ui/song_practice_detail/widgets/lyrics_panel.dart`: the right
//! panel with three tabs — Lyrics (karaoke reader / editor / tap-to-time
//! sync), Notes and Techniques (the selected take's time-pinned notes and
//! technique markers) — collapsible to a thin rail; and the full-window
//! lyrics Focus view.
//!
//! The panel takes plain data ([`RightPanelData`], what Dart read off the
//! detail view model) and reports [`Message`]s named after the view-model
//! calls and screen callbacks the Dart widgets made. What Dart kept in the
//! widgets' own `State` — the editor's text, the readers' latched line, the
//! lists' scroll positions — is a [`LyricsPanelState`] the screen keeps:
//!
//! - After every update the screen calls [`LyricsPanelState::sync`] with the
//!   data, and runs the task it returns. That is where Dart's lifecycle
//!   lands: the editor is created (seeded from the unsaved draft, else the
//!   lyrics; autofocused unless a dialog is over the page) and disposed with
//!   the tab; the readers latch the line under the playhead (`_onTick`) or
//!   re-pick it for new lyrics (`didUpdateWidget`) and scroll it into view;
//!   the sync list re-centres its target on every notification of the view
//!   model.
//! - A drop or a picked file runs through [`LyricsPanelState::begin_import`]
//!   and [`LyricsPanelState::end_import`] around the view model's import
//!   (`_importDrop`); Save through [`LyricsPanelState::save_ticket`] and
//!   [`LyricsPanelState::save_landed`] around `applyEditedLyrics`. Both
//!   keep Dart's guards: a successful import into the open editor replaces
//!   its text (Save must not write the stale snapshot back over the import),
//!   and Save leaves edit mode only for the session it was tapped in, once
//!   the text is on disk and nothing was typed meanwhile.
//!
//! How the Dart structure maps:
//!
//! - `Scrollable.ensureVisible` → a widget operation that finds the line and
//!   its list and scrolls the list so the line sits at the same alignment
//!   (0.4 for the reader, 0.5 for the sync list), clamped to the list. iced
//!   lists are not lazy, so every line has a place to scroll to: Dart's
//!   extrapolation for lines `ListView.builder` had not built is not needed.
//!   The scroll animates as Dart's did, from where the list is: over
//!   320 ms easing out for the readers, 200 ms on `Curves.ease` (the
//!   default curve) for the sync list. The operation reports where the list
//!   is and where it goes ([`Message::Scroll`]); the screen then feeds the
//!   panel frames ([`Message::ScrollFrame`], subscribed to while
//!   [`LyricsPanelState::is_scrolling`]) and each one scrolls the list
//!   along. A user's own scroll meanwhile does not stop it, as it stopped
//!   Flutter's (its ballistic activity took over).
//! - The sync view's `Focus(autofocus)` taking Space → the view handles a
//!   Space press nothing else took, and takes it (the screen's shortcuts skip
//!   captured keys). iced hands every widget the key, not the focused one
//!   alone: a focused text field drawn over the page (a dialog) or earlier
//!   in the tree takes it first, and the line is not stamped; one later in
//!   the tree (none is: the panel is the page's last column) would type it
//!   as well. The Focus
//!   view takes every key while it is up ([`holds_keyboard`]), so nothing
//!   hidden beneath it hears one.
//! - `LayoutBuilder` → `responsive`; a part that comes and goes with the
//!   height (the toolbar, the pinned hint) keeps a slot, so the editor and
//!   the sync list keep their widget state (Dart keyed the tab body).
//!
//! Where iced differs: a tooltip (`material_tooltip`, which fades as
//! Material's did) stays below its button, kept inside the window, where
//! Flutter's flipped above when there was no room below.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iced::advanced::widget::operation::scrollable::AbsoluteOffset;
use iced::advanced::widget::operation::{Operation, Outcome, Scrollable};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry};
use iced::widget::text::{Alignment, Wrapping};
use iced::widget::{
    button, canvas as canvas_widget, center, column, container, responsive, row, scrollable, stack,
    text, text_editor, Column, Space,
};
use iced::{
    keyboard, mouse, widget, Border, Color, Element, Event, Length, Padding, Pixels, Point,
    Rectangle, Renderer, Task, Theme, Vector,
};
use vocara_model::models::audio_track::AudioTrack;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::format::format_clock_ms;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::{Curve, Implicit};
use crate::ui::core::theme::{self, alpha, boxed, fill, font, Studio};
use crate::ui::core::widgets::holds_keyboard::holds_keyboard;
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_text::{fill_spaced_text, studio_text};
use crate::ui::core::widgets::visible_drop_target::visible_drop_target;
use crate::ui::core::widgets::wrap::Wrap;

#[cfg(test)]
mod tests;

/// The open panel's width.
pub const PANEL_WIDTH: f32 = 300.0;

/// The collapsed rail's width.
pub const RAIL_WIDTH: f32 = 34.0;

/// Below this tab height the editor and the sync view drop the toolbar
/// above them. The dock can leave the side panels as little as the
/// timeline's floor, and the toolbar then pushed the editor's Save/Cancel
/// and the sync view's Clear/Tap/Done under the dock, out of reach. Both
/// keep a way out without it (Cancel, Done); the reader is a plain list and
/// keeps it at any height.
pub const TOOLBAR_MIN_HEIGHT: f32 = 128.0;

/// Below this height the editor tightens its spacing, so Save and Cancel
/// stay on screen with a usable line of text above them.
pub const EDITOR_COMPACT_BELOW: f32 = 120.0;

/// Below this height the sync view's how-to hint scrolls with the lines
/// instead of sitting pinned above them: pinned, it pushed Clear / Tap /
/// Done under the dock on a short panel (the minimum window with the dock
/// open).
pub const PINNED_HINT_MIN_HEIGHT: f32 = 176.0;

/// The sync view's how-to hint.
pub const SYNC_HINT: &str = "Play, then press Space (or Tap) on each line as you reach it. \
                             Click a line to re-time it.";

/// The extensions the lyrics file picker offers — those the view model's
/// import validation accepts.
pub const LYRICS_FILE_EXTENSIONS: [&str; 3] = ["lrc", "txt", "text"];

/// The Focus view's answer to a drop while the panel's editor is open.
pub const EXIT_FOCUS_TO_IMPORT: &str = "Exit focus to import into the lyrics you are editing.";

/// The editor's placeholder.
pub const EDITOR_PLACEHOLDER: &str = "Paste lyrics (plain text or .lrc), one line each…";

/// The right panel's tab (the view model's `RightTab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RightTab {
    #[default]
    Lyrics,
    Notes,
    Techniques,
}

/// What the Lyrics tab shows (the view model's `LyricsMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyricsMode {
    #[default]
    Read,
    Edit,
    Sync,
}

/// One of `vm.allNotes`: a note and the take it is on. Owned: the view
/// model hands the list out by value, and the screen keeps it for the
/// panel to borrow.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteRef {
    pub track_id: String,
    pub track_name: String,
    pub note: PracticeNote,
}

/// What the panel reads of the detail view model, as it is now.
#[derive(Clone, Copy)]
pub struct RightPanelData<'a> {
    /// `vm.lyricsOpen`: the panel is open (else the rail shows).
    pub lyrics_open: bool,
    /// `vm.rightTab`.
    pub right_tab: RightTab,
    /// `vm.lyricsMode`.
    pub lyrics_mode: LyricsMode,
    /// `vm.lyricsFocus`: the Focus view is up (the screen shows
    /// [`lyrics_focus_overlay`] over the page while it is).
    pub lyrics_focus: bool,
    /// `vm.hasLyrics`.
    pub has_lyrics: bool,
    /// `vm.lyrics`.
    pub lyrics: &'a [LyricLine],
    /// `vm.lyricsAsText()`, asked only when needed (seeding the editor, an
    /// import).
    pub lyrics_as_text: &'a dyn Fn() -> String,
    /// `vm.lyricsDraft`: the unsaved edit, kept while the editor is gone.
    pub lyrics_draft: Option<&'a str>,
    /// `vm.lyricsEditSession`: bumped each time the editor is opened.
    pub lyrics_edit_session: u64,
    /// `vm.syncIndex`: the next line Tap stamps.
    pub sync_index: i64,
    /// `vm.currentLyricIndex(position)`: the synced line under the playhead,
    /// or −1.
    pub current_lyric_index: i64,
    /// `vm.selectedTrack`.
    pub selected_track: Option<&'a AudioTrack>,
    /// `vm.selectedTrackNotes`.
    pub selected_track_notes: &'a [PracticeNote],
    /// `vm.selectedTrackTechniques`.
    pub selected_track_techniques: &'a [TechniqueMarker],
    /// `vm.allNotes`.
    pub all_notes: &'a [NoteRef],
    /// A dialog is over the page (Dart's `!ModalRoute.isCurrentOf(context)`):
    /// an editor made now does not take the keyboard from it.
    pub under_dialog: bool,
    /// Bumped on every notification of the view model: the sync list
    /// re-centres its target when it moves (Dart's `didUpdateWidget`).
    pub revision: u64,
}

/// What the panel asks of the screen, each named after the call the Dart
/// widget made.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A tab: `vm.setRightTab(tab)`.
    SetRightTab(RightTab),
    /// The collapse button or the rail: `vm.toggleLyricsPanel()`.
    ToggleLyricsPanel,
    /// `vm.setLyricsMode(mode)` (the toolbar's toggles, Paste lyrics, Cancel,
    /// Done).
    SetLyricsMode(LyricsMode),
    /// `vm.setLyricsFocus(on)` (the toolbar's Focus, the Focus view's exit).
    SetLyricsFocus(bool),
    /// Import lyrics file (`_browseImport`): run [`browse_lyrics_file`],
    /// which answers with [`Message::ImportLyricsFromFile`].
    BrowseImport,
    /// A file dropped on the tab or picked (`_importDrop(path)`), or dropped
    /// on the Focus view: `vm.importLyricsFromFile(path)`, between
    /// [`LyricsPanelState::begin_import`] and
    /// [`LyricsPanelState::end_import`].
    ImportLyricsFromFile(PathBuf),
    /// The editor was used: hand it to [`LyricsPanelState::perform`]; an
    /// edit is `vm.lyricsDraft = text`.
    EditorAction(text_editor::Action),
    /// Save: `vm.applyEditedLyrics(text)` with the text of
    /// [`LyricsPanelState::save_ticket`]; when it lands,
    /// [`LyricsPanelState::save_landed`] says whether to
    /// `vm.setLyricsMode(LyricsMode.read)`.
    ApplyEditedLyrics,
    /// Space or Tap in the sync view: `vm.stampNextLyric()`.
    StampNextLyric,
    /// A line of the sync list: `vm.stampLyricAt(index)`.
    StampLyricAt(usize),
    /// Clear: `vm.clearLyricTimes()`.
    ClearLyricTimes,
    /// A synced line in a reader: `vm.seekToLyric(line)`.
    SeekToLyric(LyricLine),
    /// A note or technique tile: `vm.seek(position)`.
    Seek(Duration),
    /// A note of the all-notes list: `vm.selectTrack(trackId)`.
    SelectTrack(String),
    /// The notes header's +: `onAddNote(trackId)`.
    AddNote(String),
    /// A note tile of the selected take: `onEditNote(trackId, note)`.
    EditNote { track_id: String, note_id: String },
    /// The techniques header's +: `onAddTechnique(trackId)`.
    AddTechnique(String),
    /// A technique tile's remove button: `onEditTechnique(trackId, marker)`.
    EditTechnique { track_id: String, marker_id: String },
    /// The Focus view says why it took no drop: `onToast(message)`.
    Toast(String),
    /// A reveal found its line: scroll its list there
    /// ([`LyricsPanelState::start_scroll`]).
    Scroll(ScrollStart),
    /// A frame while a list scrolls to a revealed line
    /// ([`LyricsPanelState::scroll_frame`]).
    ScrollFrame(Instant),
}

/// How a reveal scrolls (`ensureVisible`'s `duration` and `curve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMotion {
    pub duration: Duration,
    pub curve: Curve,
}

impl ScrollMotion {
    /// A reader's `_reveal`: 320 ms, easing out.
    pub const READER: Self = Self {
        duration: Duration::from_millis(320),
        curve: Curve::EaseOut,
    };
    /// The sync list's `_scrollToTarget`: 200 ms on the default curve.
    pub const SYNC: Self = Self {
        duration: Duration::from_millis(200),
        curve: Curve::Ease,
    };
}

/// Where a revealed line's list is, and where it scrolls to.
#[derive(Debug, Clone, PartialEq)]
pub struct RevealScroll {
    pub list: widget::Id,
    pub from: f32,
    pub to: f32,
}

/// A reveal that found its line, and how it scrolls there.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollStart {
    pub scroll: RevealScroll,
    pub motion: ScrollMotion,
}

/// The platform's open panel for a lyrics file, filtered to the extensions
/// the import accepts (`_browseImport`); a picked file comes back as
/// [`Message::ImportLyricsFromFile`], a cancel as nothing. Call it from
/// `update` (the panel is set up on the UI thread).
pub fn browse_lyrics_file() -> Task<Message> {
    let picking = vocara_services::file_panels::AsyncFileDialog::new()
        .add_filter("Lyrics", &LYRICS_FILE_EXTENSIONS)
        .pick_file();
    Task::future(picking).then(|file| match file {
        Some(file) => Task::done(Message::ImportLyricsFromFile(file.path().to_path_buf())),
        None => Task::none(),
    })
}

// ------------------------------------------------------------- the state

static NEXT_PANEL: AtomicU64 = AtomicU64::new(0);

/// The open editor (`_LyricsEditorState`).
struct Editor {
    content: text_editor::Content,
    /// Which editor this is: one made again (a re-seed, a tab switch and
    /// back) is another, as Dart's re-created `State` was.
    instance: u64,
    /// Made under a dialog, so it did not take the keyboard: it is taken
    /// once the page is on top again. Only then: owed for any dialog over an
    /// editor the user had clicked away from, it took the keyboard back as
    /// the dialog closed. (Dart also checked that nothing on the page had
    /// taken the keyboard meanwhile; under a modal dialog nothing on it can.)
    focus_owed: bool,
}

/// A karaoke reader's latch (`_LyricsReaderState`).
#[derive(Debug, Clone, PartialEq)]
struct Reader {
    /// The highlighted line, or −1.
    active: i64,
    /// The lyrics `active` indexes into.
    lines: Vec<LyricLine>,
}

/// What the panel's widgets kept between builds in Dart; the screen keeps
/// one per panel and [`sync`](Self::sync)s it after every update.
pub struct LyricsPanelState {
    uid: u64,
    editor: Option<Editor>,
    editors_made: u64,
    reader: Option<Reader>,
    focus_reader: Option<Reader>,
    /// The sync list is on screen, built at this revision.
    sync_list: Option<u64>,
    /// The lists scrolling to a revealed line, each from where it was.
    scrolls: Vec<(widget::Id, Implicit<f32>)>,
}

impl Default for LyricsPanelState {
    fn default() -> Self {
        Self::new()
    }
}

/// Something [`LyricsPanelState::sync`] asks of the widgets on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelRequest {
    /// Give the keyboard to the widget (the editor).
    Focus(widget::Id),
    /// Scroll `list` so `line` sits at `alignment` of its height, moving
    /// as `motion` says (`Scrollable.ensureVisible`).
    Reveal {
        list: widget::Id,
        line: widget::Id,
        alignment: f32,
        motion: ScrollMotion,
    },
}

impl PanelRequest {
    /// The request as a task.
    pub fn into_task(self) -> Task<Message> {
        match self {
            PanelRequest::Focus(id) => iced::widget::operation::focus(id),
            PanelRequest::Reveal {
                list,
                line,
                alignment,
                motion,
            } => iced::advanced::widget::operate(Reveal::new(list, line, alignment))
                .map(move |scroll| Message::Scroll(ScrollStart { scroll, motion })),
        }
    }
}

/// What a drop or a picked file remembers while the import runs
/// (`_importDrop`'s first half).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportTicket {
    editing: bool,
    before: String,
}

/// What Save remembers while its write runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveTicket {
    text: String,
    session: u64,
    editor: u64,
}

impl SaveTicket {
    /// The text to hand `vm.applyEditedLyrics`.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Which list a widget id belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Reader,
    Focus,
    Sync,
}

impl LyricsPanelState {
    pub fn new() -> Self {
        Self {
            uid: NEXT_PANEL.fetch_add(1, Ordering::Relaxed),
            editor: None,
            editors_made: 0,
            reader: None,
            focus_reader: None,
            sync_list: None,
            scrolls: Vec::new(),
        }
    }

    /// A reveal found its line at `now`: its list scrolls there from where
    /// it is, as `motion` says (a later reveal of the same list takes over
    /// from where this one got).
    pub fn start_scroll<T: Send + 'static>(&mut self, start: ScrollStart, now: Instant) -> Task<T> {
        let ScrollStart { scroll, motion } = start;
        let from = self
            .scrolls
            .iter()
            .find(|(id, _)| *id == scroll.list)
            .map_or(scroll.from, |(_, a)| a.value(now));
        self.scrolls.retain(|(id, _)| *id != scroll.list);
        let mut along = Implicit::new(from, motion.duration, motion.curve);
        along.retarget(scroll.to, now);
        if !along.is_animating(now) {
            return scroll_list(scroll.list, scroll.to);
        }
        self.scrolls.push((scroll.list, along));
        Task::none()
    }

    /// A frame at `now`: each scrolling list moves along; those there stop.
    pub fn scroll_frame<T: Send + 'static>(&mut self, now: Instant) -> Task<T> {
        let moves: Vec<Task<T>> = self
            .scrolls
            .iter()
            .map(|(id, along)| scroll_list(id.clone(), along.value(now)))
            .collect();
        self.scrolls.retain(|(_, along)| along.is_animating(now));
        Task::batch(moves)
    }

    /// Whether a list scrolls to a revealed line at `now`.
    pub fn is_scrolling(&self, now: Instant) -> bool {
        self.scrolls
            .iter()
            .any(|(_, along)| along.is_animating(now))
    }

    /// The editor's widget id.
    pub fn editor_id(&self) -> widget::Id {
        widget::Id::from(format!("lyrics-{}-editor", self.uid))
    }

    fn list_id(&self, list: List) -> widget::Id {
        widget::Id::from(format!("lyrics-{}-{list:?}", self.uid))
    }

    fn line_id(&self, list: List, i: usize) -> widget::Id {
        widget::Id::from(format!("lyrics-{}-{list:?}-{i}", self.uid))
    }

    /// Brings the panel's widgets up to `data` (see the module docs) and
    /// returns what they must do on screen, as a task.
    pub fn sync(&mut self, data: &RightPanelData<'_>) -> Task<Message> {
        Task::batch(
            self.sync_requests(data)
                .into_iter()
                .map(PanelRequest::into_task),
        )
    }

    /// [`Self::sync`], handing back the requests instead of a task.
    pub fn sync_requests(&mut self, data: &RightPanelData<'_>) -> Vec<PanelRequest> {
        let mut requests = Vec::new();
        let lyrics_tab = data.lyrics_open && data.right_tab == RightTab::Lyrics;

        // The editor: made with the tab in edit mode, gone with it.
        if lyrics_tab && data.lyrics_mode == LyricsMode::Edit {
            match &mut self.editor {
                None => {
                    // Reopens on the unsaved draft, if any: a tab switch or a
                    // collapse of the panel disposes the editor.
                    let text = match data.lyrics_draft {
                        Some(draft) => draft.to_string(),
                        None => (data.lyrics_as_text)(),
                    };
                    self.editors_made += 1;
                    // Not under a dialog: an import landing while one was
                    // open re-made the editor, whose autofocus took the
                    // dialog's keys.
                    let focus_owed = data.under_dialog;
                    if !focus_owed {
                        requests.push(PanelRequest::Focus(self.editor_id()));
                    }
                    self.editor = Some(Editor {
                        content: text_editor::Content::with_text(&text),
                        instance: self.editors_made,
                        focus_owed,
                    });
                }
                Some(editor) => {
                    if editor.focus_owed && !data.under_dialog {
                        editor.focus_owed = false;
                        requests.push(PanelRequest::Focus(self.editor_id()));
                    }
                }
            }
        } else {
            self.editor = None;
        }

        // The readers: the panel's, and the Focus view's.
        let reader_shown = lyrics_tab && data.lyrics_mode == LyricsMode::Read && data.has_lyrics;
        if let Some(idx) = sync_reader(&mut self.reader, reader_shown, data) {
            requests.extend(self.reveal(List::Reader, idx, data.lyrics.len(), 0.4));
        }
        if let Some(idx) = sync_reader(&mut self.focus_reader, data.lyrics_focus, data) {
            requests.extend(self.reveal(List::Focus, idx, data.lyrics.len(), 0.4));
        }

        // The sync list: on screen, it re-centres its target on every
        // notification (not when it is first built).
        if lyrics_tab && data.lyrics_mode == LyricsMode::Sync && data.has_lyrics {
            match self.sync_list {
                Some(revision) if revision != data.revision => {
                    self.sync_list = Some(data.revision);
                    requests.extend(self.reveal(
                        List::Sync,
                        data.sync_index,
                        data.lyrics.len(),
                        0.5,
                    ));
                }
                Some(_) => {}
                None => self.sync_list = Some(data.revision),
            }
        } else {
            self.sync_list = None;
        }
        requests
    }

    /// `_reveal(idx)`: scroll line `idx` of `list` into view, if it is a
    /// line.
    fn reveal(&self, list: List, idx: i64, lines: usize, alignment: f32) -> Option<PanelRequest> {
        let i = usize::try_from(idx).ok().filter(|&i| i < lines)?;
        let motion = match list {
            List::Sync => ScrollMotion::SYNC,
            List::Reader | List::Focus => ScrollMotion::READER,
        };
        Some(PanelRequest::Reveal {
            list: self.list_id(list),
            line: self.line_id(list, i),
            alignment,
            motion,
        })
    }

    /// Applies an editor action ([`Message::EditorAction`]); the new text
    /// when it changed it — the user's edits only, not focus or selection
    /// churn: the draft must never be re-written behind a re-seed. Set it
    /// as `vm.lyricsDraft`.
    pub fn perform(&mut self, action: text_editor::Action) -> Option<String> {
        let editor = self.editor.as_mut()?;
        if !action.is_edit() {
            editor.content.perform(action);
            return None;
        }
        let before = editor.content.text();
        editor.content.perform(action);
        let after = editor.content.text();
        (after != before).then_some(after)
    }

    /// The open editor's text, if the editor is open.
    pub fn editor_text(&self) -> Option<String> {
        self.editor.as_ref().map(|e| e.content.text())
    }

    /// Before `vm.importLyricsFromFile` runs for a drop or a picked file:
    /// what the lyrics were, when the editor is open.
    pub fn begin_import(&self, data: &RightPanelData<'_>) -> ImportTicket {
        let editing = data.lyrics_mode == LyricsMode::Edit;
        ImportTicket {
            editing,
            before: if editing {
                (data.lyrics_as_text)()
            } else {
                String::new()
            },
        }
    }

    /// Once the import landed (the view model validated, read and toasted
    /// as it does). A successful edit-mode import goes INTO the open editor:
    /// the drop is an explicit "replace the lyrics" action, so the editor
    /// must show (and Save must apply) the imported text; a failed or no-op
    /// import leaves the edit in progress untouched. True when the import
    /// replaces the draft too — set `vm.lyricsDraft = null`, then
    /// [`sync`](Self::sync): the editor is made again from the imported
    /// lyrics (or it would reopen on the draft).
    pub fn end_import(&mut self, ticket: ImportTicket, data: &RightPanelData<'_>) -> bool {
        if !ticket.editing || (data.lyrics_as_text)() == ticket.before {
            return false;
        }
        self.editor = None;
        true
    }

    /// Save was tapped: the text to write, and which editing session it was
    /// tapped in. `None` with no editor open.
    pub fn save_ticket(&self, data: &RightPanelData<'_>) -> Option<SaveTicket> {
        let editor = self.editor.as_ref()?;
        Some(SaveTicket {
            text: editor.content.text(),
            session: data.lyrics_edit_session,
            editor: editor.instance,
        })
    }

    /// `vm.applyEditedLyrics` answered `saved`: whether to leave edit mode
    /// (`vm.setLyricsMode(LyricsMode.read)`). Only once the text is on disk
    /// — a failed write reverts the model, so closing regardless would drop
    /// the draft with nothing but a toast to show for it. Nor over text
    /// typed while the write waited (it queues behind other saves): that is
    /// still the draft. And only the editing session Save was tapped in: the
    /// user may have moved on to Sync, or left and opened the editor again.
    pub fn save_landed(&self, ticket: &SaveTicket, saved: bool, data: &RightPanelData<'_>) -> bool {
        if !saved {
            return false;
        }
        let now = match &self.editor {
            Some(editor) if editor.instance == ticket.editor => editor.content.text(),
            _ => data
                .lyrics_draft
                .map_or_else(|| ticket.text.clone(), str::to_string),
        };
        now == ticket.text
            && data.lyrics_mode == LyricsMode::Edit
            && data.lyrics_edit_session == ticket.session
    }

    /// The line the panel's reader highlights, if it is on screen.
    pub fn reader_line(&self) -> Option<i64> {
        self.reader.as_ref().map(|r| r.active)
    }

    /// The line the Focus view highlights, if it is up.
    pub fn focus_line(&self) -> Option<i64> {
        self.focus_reader.as_ref().map(|r| r.active)
    }
}

/// A reader's lifecycle for this update; the line to reveal, if any.
fn sync_reader(slot: &mut Option<Reader>, shown: bool, data: &RightPanelData<'_>) -> Option<i64> {
    if !shown {
        *slot = None;
        return None;
    }
    let idx = data.current_lyric_index;
    match slot {
        // Made: latched on the line under the playhead, revealed.
        None => {
            *slot = Some(Reader {
                active: idx,
                lines: data.lyrics.to_vec(),
            });
            Some(idx)
        }
        Some(reader) => {
            // New lyrics (an import or edit while reading) re-time the
            // lines, so the latched index belongs to the OLD list: it would
            // light whatever line now sits there, and the same-index
            // early-out would keep it until the playhead next crossed a line
            // — never, while paused. Compared by value: the practice is
            // rebuilt on unrelated edits too, and that must not yank the
            // scroll back.
            if reader.lines.as_slice() != data.lyrics {
                reader.lines = data.lyrics.to_vec();
                reader.active = idx;
                return Some(idx);
            }
            if idx == reader.active {
                return None;
            }
            reader.active = idx;
            Some(idx)
        }
    }
}

// ---------------------------------------------------------- ensureVisible

/// `list` scrolled to `y`.
fn scroll_list<T: Send + 'static>(list: widget::Id, y: f32) -> Task<T> {
    iced::widget::operation::scroll_to(
        list,
        AbsoluteOffset {
            x: None,
            y: Some(y),
        },
    )
}

/// `Scrollable.ensureVisible(line, alignment)`: finds `line` inside `list`,
/// and where the list is and where it must scroll so the line's top sits at
/// `alignment` of the room the viewport has around it, clamped to the
/// list's extent ([`RevealScroll`]).
pub(crate) struct Reveal {
    list: widget::Id,
    line: widget::Id,
    alignment: f32,
    viewport: Option<(Rectangle, Rectangle, f32)>,
    target: Option<Rectangle>,
}

impl Reveal {
    pub(crate) fn new(list: widget::Id, line: widget::Id, alignment: f32) -> Self {
        Self {
            list,
            line,
            alignment,
            viewport: None,
            target: None,
        }
    }
}

/// The offset `Scrollable.ensureVisible` scrolls to: the line's leading
/// edge in the content less `alignment` of the room around it, clamped to
/// the list.
pub fn reveal_offset(
    viewport: Rectangle,
    content: Rectangle,
    line: Rectangle,
    alignment: f32,
) -> f32 {
    let leading = line.y - content.y;
    let offset = leading - (viewport.height - line.height) * alignment;
    offset.clamp(0.0, (content.height - viewport.height).max(0.0))
}

impl Operation<RevealScroll> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<RevealScroll>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(&self.line) {
            self.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&self.list) {
            self.viewport = Some((bounds, content_bounds, translation.y));
        }
    }

    fn finish(&self) -> Outcome<RevealScroll> {
        let (Some((viewport, content, from)), Some(line)) = (self.viewport, self.target) else {
            return Outcome::None;
        };
        Outcome::Some(RevealScroll {
            list: self.list.clone(),
            from,
            to: reveal_offset(viewport, content, line, self.alignment),
        })
    }
}

// --------------------------------------------------------------- the panel

/// `RightPanel`: the rail while collapsed, else the tab bar over the open
/// tab.
pub fn right_panel<'a>(
    state: &'a LyricsPanelState,
    data: RightPanelData<'a>,
) -> Element<'a, Message> {
    if !data.lyrics_open {
        return rail();
    }
    let body = match data.right_tab {
        RightTab::Notes => notes_view(data),
        RightTab::Techniques => techniques_view(data),
        RightTab::Lyrics => lyrics_tab(state, data),
    };
    container(row![
        vrule(),
        column![tab_bar(data.right_tab), hrule(), body]
            .width(Length::Fill)
            .height(Length::Fill),
    ])
    .width(PANEL_WIDTH)
    .height(Length::Fill)
    .style(fill(Studio::SURFACE))
    .into()
}

/// A 1 px `Studio.border` line across.
fn hrule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(fill(Studio::BORDER))
        .into()
}

/// A 1 px `Studio.border` line down (the panel's left border).
fn vrule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(1)
        .height(Length::Fill)
        .style(fill(Studio::BORDER))
        .into()
}

/// A button that draws nothing of its own around `content`.
fn bare<'a>(
    content: impl Into<Element<'a, Message>>,
    on_press: Message,
) -> button::Button<'a, Message> {
    button(content)
        .padding(0)
        .style(theme::bare_button)
        .on_press(on_press)
}

/// A Material tooltip below `target`, its top 24 px below the target's
/// centre (`target_height` tall).
fn with_tip<'a>(
    target: Element<'a, Message>,
    tip: &str,
    target_height: f32,
) -> Element<'a, Message> {
    material_tooltip(target, tip, 24.0 - target_height / 2.0).into()
}

/// `_IconBtn`: a 15 px icon in a 6 px pad (coral-washed when `active`),
/// with its tooltip.
fn icon_btn<'a>(icon: Icon, tip: &str, on_tap: Message, active: bool) -> Element<'a, Message> {
    let face = container(icons::icon(
        icon,
        15.0,
        if active {
            Studio::ACCENT_BRIGHT
        } else {
            Studio::TEXT_SECONDARY
        },
    ))
    .padding(6)
    .style(move |_| {
        if active {
            boxed(Some(alpha(Studio::ACCENT, 0.16)), 6.0, None)
        } else {
            boxed(None, 0.0, None)
        }
    });
    with_tip(
        container(bare(face, on_tap)).padding([0, 1]).into(),
        tip,
        27.0,
    )
}

/// `_TabBar`: the three tabs and the collapse button.
fn tab_bar<'a>(active: RightTab) -> Element<'a, Message> {
    container(
        row![
            tab("Lyrics", active == RightTab::Lyrics, RightTab::Lyrics),
            Space::new().width(4),
            tab("Notes", active == RightTab::Notes, RightTab::Notes),
            Space::new().width(4),
            tab(
                "Techniques",
                active == RightTab::Techniques,
                RightTab::Techniques
            ),
            Space::new().width(Length::Fill),
            // Panel-toggle icon, matching the left inspector's collapse
            // button.
            icon_btn(
                Icon::PanelRightClose,
                "Collapse",
                Message::ToggleLyricsPanel,
                false
            ),
        ]
        .align_y(Vertical::Center)
        .height(Length::Fill),
    )
    .padding(Padding {
        left: 12.0,
        right: 6.0,
        ..Padding::ZERO
    })
    .height(40)
    .into()
}

/// `_Tab`: an upper-cased label, raised and outlined when active.
fn tab<'a>(label: &str, active: bool, to: RightTab) -> Element<'a, Message> {
    let face = container(
        studio_text(label.to_uppercase())
            .size(11)
            .font(font::BOLD)
            .letter_spacing(0.6)
            .color(if active {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_FAINT
            })
            .wrapping(Wrapping::None),
    )
    // 5 × 10, inside a 1 px border (transparent when inactive).
    .padding([6, 11])
    .style(move |_| {
        if active {
            boxed(Some(Studio::SURFACE_RAISED), 6.0, Some(Studio::BORDER))
        } else {
            boxed(None, 6.0, None)
        }
    });
    bare(face, Message::SetRightTab(to)).into()
}

/// `_Rail`: the collapsed panel — a tap anywhere opens it.
fn rail<'a>() -> Element<'a, Message> {
    let content = column![
        Space::new().height(12),
        icons::icon(Icon::PanelRightOpen, 15.0, Studio::TEXT_SECONDARY),
        Space::new().height(18),
        // Clipped, not overflowing, where the dock leaves the panels less
        // height than the label.
        canvas_widget(RotatedLabel {
            label: "LYRICS   NOTES   TECHNIQUES",
        })
        .width(Length::Fill)
        .height(Length::Fill),
    ]
    .align_x(Horizontal::Center)
    .width(Length::Fill)
    .height(Length::Fill);
    container(row![
        vrule(),
        button(content)
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::bare_button)
            .on_press(Message::ToggleLyricsPanel),
    ])
    .width(RAIL_WIDTH)
    .height(Length::Fill)
    .style(fill(Studio::SURFACE))
    .into()
}

/// `RotatedBox(quarterTurns: 1, child: Text(label))`: the label (11 px
/// w700, 1.5 letter spacing) reading top to bottom from the top of its box,
/// clipped at its bottom.
struct RotatedLabel {
    label: &'static str,
}

impl canvas::Program<Message> for RotatedLabel {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.translate(Vector::new(bounds.width / 2.0, 0.0));
        frame.rotate(std::f32::consts::FRAC_PI_2);
        fill_spaced_text(
            &mut frame,
            canvas::Text {
                content: self.label.to_string(),
                position: Point::ORIGIN,
                color: Studio::TEXT_SECONDARY,
                size: Pixels(11.0),
                font: font::BOLD,
                align_x: Alignment::Left,
                align_y: Vertical::Center,
                ..canvas::Text::default()
            },
            1.5,
        );
        vec![frame.into_geometry()]
    }
}

// ---------------------------------------------------------- the lyrics tab

/// `_LyricsTab`: the toolbar (while there are lyrics, and room for it) over
/// the mode's body, which takes a dropped lyrics file.
fn lyrics_tab<'a>(state: &'a LyricsPanelState, data: RightPanelData<'a>) -> Element<'a, Message> {
    responsive(move |size| {
        let toolbar = data.has_lyrics
            && (data.lyrics_mode == LyricsMode::Read || size.height >= TOOLBAR_MIN_HEIGHT);
        // The slot stays when the toolbar goes, so the body keeps the editor
        // or the sync list it holds (Dart keyed it).
        let top: Element<'a, Message> = if toolbar {
            lyrics_toolbar(data.lyrics_mode)
        } else {
            Space::new().height(0).into()
        };
        let body = visible_drop_target(lyrics_body(state, data), |paths: Vec<PathBuf>| {
            // A drop has at least one file.
            Message::ImportLyricsFromFile(paths.into_iter().next().unwrap_or_default())
        });
        Element::from(column![top, body].width(Length::Fill).height(Length::Fill))
    })
    .into()
}

/// What the Lyrics tab shows in its mode. Edit is always available — it's
/// how the first lyrics get pasted or typed.
fn lyrics_body<'a>(state: &'a LyricsPanelState, data: RightPanelData<'a>) -> Element<'a, Message> {
    match data.lyrics_mode {
        LyricsMode::Edit => match &state.editor {
            Some(editor) => lyrics_editor(state, editor),
            // Not made yet (the next sync makes it).
            None => Space::new().width(Length::Fill).height(Length::Fill).into(),
        },
        LyricsMode::Read if data.has_lyrics => lyrics_reader(state, data, false),
        LyricsMode::Sync if data.has_lyrics => lyrics_sync(state, data),
        _ => empty_state(),
    }
}

/// `_LyricsToolbar`: import, edit, sync and focus.
fn lyrics_toolbar<'a>(mode: LyricsMode) -> Element<'a, Message> {
    let toggle =
        |to: LyricsMode| Message::SetLyricsMode(if mode == to { LyricsMode::Read } else { to });
    container(row![
        Space::new().width(Length::Fill),
        icon_btn(
            Icon::FileUp,
            "Import lyrics file",
            Message::BrowseImport,
            false
        ),
        icon_btn(
            Icon::Pencil,
            "Edit lyrics",
            toggle(LyricsMode::Edit),
            mode == LyricsMode::Edit
        ),
        icon_btn(
            Icon::Timer,
            "Sync (tap-to-time)",
            toggle(LyricsMode::Sync),
            mode == LyricsMode::Sync
        ),
        icon_btn(
            Icon::Maximize2,
            "Focus",
            Message::SetLyricsFocus(true),
            false
        ),
    ])
    .padding(ltrb(12.0, 8.0, 6.0, 4.0))
    .into()
}

/// `EdgeInsets.fromLTRB`.
fn ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

/// `_EmptyState`: no lyrics yet — paste, import, or drop. Centred whenever
/// there is room; on a panel shorter than the content (tiny window with the
/// dock open) it scrolls instead of overflowing.
fn empty_state<'a>() -> Element<'a, Message> {
    let content = column![
        icons::icon(Icon::Quote, 26.0, Studio::TEXT_FAINT),
        Space::new().height(14),
        text("No lyrics yet")
            .size(13)
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_SECONDARY),
        Space::new().height(16),
        studio_button("Paste lyrics", Message::SetLyricsMode(LyricsMode::Edit))
            .icon(Icon::Clipboard)
            .variant(StudioButtonVariant::Accent),
        Space::new().height(10),
        studio_button("Import lyrics file", Message::BrowseImport)
            .icon(Icon::FileUp)
            .variant(StudioButtonVariant::Subtle),
        Space::new().height(12),
        text("or drop a .lrc or .txt file here")
            .size(12)
            .align_x(Alignment::Center)
            .color(Studio::TEXT_FAINT),
    ]
    .align_x(Horizontal::Center);
    center(scrollable(container(content).padding(24)).style(theme::scrollbar)).into()
}

// -------------------------------------------------------------- the reader

/// `_LyricsReader`: the karaoke reader — the current line highlighted and
/// kept in view as the playhead moves (see [`LyricsPanelState::sync`]); a
/// synced line seeks to itself. `large` is the full-screen Focus variant.
fn lyrics_reader<'a>(
    state: &'a LyricsPanelState,
    data: RightPanelData<'a>,
    large: bool,
) -> Element<'a, Message> {
    let list = if large { List::Focus } else { List::Reader };
    let latch = if large {
        &state.focus_reader
    } else {
        &state.reader
    };
    let active = latch.as_ref().map_or(-1, |r| r.active);
    let mut lines = Column::new();
    for (i, l) in data.lyrics.iter().enumerate() {
        let id = state.line_id(list, i);
        if l.is_blank() {
            lines = lines.push(
                container(Space::new().height(if large { 28 } else { 14 }))
                    .width(Length::Fill)
                    .id(id),
            );
            continue;
        }
        let is_active = i as i64 == active;
        let label = text(l.text.as_str())
            .size(match (large, is_active) {
                (true, true) => 34,
                (true, false) => 26,
                (false, true) => 16,
                (false, false) => 14,
            })
            .font(if is_active { font::BOLD } else { font::MEDIUM })
            .line_height(1.35)
            .color(if is_active {
                Studio::ACCENT_BRIGHT
            } else if l.is_synced() {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_SECONDARY
            })
            .align_x(if large {
                Alignment::Center
            } else {
                Alignment::Left
            })
            .width(Length::Fill);
        let line: Element<'a, Message> = if l.is_synced() {
            bare(label, Message::SeekToLyric(l.clone()))
                .width(Length::Fill)
                .into()
        } else {
            label.into()
        };
        lines = lines.push(
            container(line)
                .padding([if large { 9 } else { 5 }, 0])
                .width(Length::Fill)
                .id(id),
        );
    }
    scrollable(lines.padding(if large { [120, 40] } else { [20, 16] }))
        .id(state.list_id(list))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::scrollbar)
        .into()
}

// -------------------------------------------------------------- the editor

/// `_LyricsEditor`: the lyrics as text, with Cancel and Save.
fn lyrics_editor<'a>(state: &'a LyricsPanelState, editor: &'a Editor) -> Element<'a, Message> {
    let id = state.editor_id();
    responsive(move |size| {
        let compact = size.height < EDITOR_COMPACT_BELOW;
        let field = text_editor(&editor.content)
            .id(id.clone())
            .placeholder(EDITOR_PLACEHOLDER)
            .on_action(Message::EditorAction)
            .size(14)
            .line_height(1.4)
            .padding(0)
            .height(Length::Fill)
            .style(editor_style);
        let well = container(field)
            // 10 (4 compact) × 12, inside the well's 1 px border.
            .padding(if compact { [5, 13] } else { [11, 13] })
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| boxed(Some(Studio::INSET), 8.0, Some(Studio::BORDER)));
        Element::from(
            container(
                column![
                    well,
                    Space::new().height(if compact { 6 } else { 12 }),
                    row![
                        studio_button("Cancel", Message::SetLyricsMode(LyricsMode::Read)),
                        Space::new().width(Length::Fill),
                        studio_button("Save", Message::ApplyEditedLyrics)
                            .variant(StudioButtonVariant::Accent),
                    ],
                ]
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .padding(if compact { 8 } else { 12 })
            .width(Length::Fill)
            .height(Length::Fill),
        )
    })
    .into()
}

/// The editor's text: `Studio.textPrimary` on the well (no fill or border of
/// its own), the hint faint, Material's selection.
fn editor_style(_: &Theme, _: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: Studio::TEXT_FAINT,
        value: Studio::TEXT_PRIMARY,
        selection: alpha(theme::Zinc::PRIMARY, 0.40),
    }
}

// ------------------------------------------------------------ the sync view

/// `_LyricsSync._fmt`: `mm:ss.cc`.
pub fn sync_time(ms: i64) -> String {
    let s = ms.div_euclid(1000);
    format!(
        "{:02}:{:02}.{:02}",
        s.div_euclid(60),
        s.rem_euclid(60),
        ms.rem_euclid(1000) / 10
    )
}

/// The how-to hint, full width on its pale coral wash.
fn sync_hint<'a>() -> Element<'a, Message> {
    container(
        text(SYNC_HINT)
            .size(11)
            .line_height(1.4)
            .color(Studio::TEXT_SECONDARY),
    )
    .padding(ltrb(14.0, 10.0, 14.0, 10.0))
    .width(Length::Fill)
    .style(fill(alpha(Studio::ACCENT, 0.06)))
    .into()
}

/// `_LyricsSync`: the lines with their times, the next one to stamp
/// outlined; Space or Tap stamps it, a click re-times a line.
fn lyrics_sync<'a>(state: &'a LyricsPanelState, data: RightPanelData<'a>) -> Element<'a, Message> {
    let view = responsive(move |size| {
        let pin_hint = size.height >= PINNED_HINT_MIN_HEIGHT;
        let mut items = Column::new();
        if !pin_hint {
            // The hint's slot at the top of the list when it scrolls with
            // the lines, spanning the list's full width as it does pinned.
            items = items.push(container(sync_hint()).padding(Padding {
                bottom: 10.0,
                ..Padding::ZERO
            }));
        }
        for i in 0..data.lyrics.len() {
            let line = sync_line(state, data, i);
            // Scrolling, the lines take the side padding themselves.
            items = items.push(if pin_hint {
                line
            } else {
                container(line).padding([0, 10]).into()
            });
        }
        let list = scrollable(items.padding(if pin_hint {
            Padding::from([10, 10])
        } else {
            Padding {
                bottom: 10.0,
                ..Padding::ZERO
            }
        }))
        .id(state.list_id(List::Sync))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::scrollbar);
        // The hint's pinned slot stays when it goes, so the list keeps its
        // place in the tree.
        let top: Element<'a, Message> = if pin_hint {
            sync_hint()
        } else {
            Space::new().height(0).into()
        };
        Element::from(
            column![
                top,
                list,
                hrule(),
                container(row![
                    studio_button("Clear", Message::ClearLyricTimes)
                        .variant(StudioButtonVariant::Danger),
                    Space::new().width(Length::Fill),
                    studio_button("Tap (Space)", Message::StampNextLyric)
                        .icon(Icon::Target)
                        .variant(StudioButtonVariant::Accent),
                    Space::new().width(8),
                    studio_button("Done", Message::SetLyricsMode(LyricsMode::Read)),
                ])
                .padding(10),
            ]
            .width(Length::Fill)
            .height(Length::Fill),
        )
    });
    space_stamps(view.into())
}

/// The sync view takes a Space press nothing else took (Dart's autofocused
/// `Focus(onKeyEvent)`): it stamps the next line, and the key is handled.
fn space_stamps(content: Element<'_, Message>) -> Element<'_, Message> {
    Wrap::<Message, ()>::new(
        content,
        Box::new(|_, event, _, _, shell| {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Space),
                repeat: false,
                ..
            }) = event
            {
                if !shell.is_event_captured() {
                    shell.publish(Message::StampNextLyric);
                    shell.capture_event();
                }
            }
        }),
    )
    .into()
}

/// Line `i` of the sync list: its time and text; a tap stamps it.
fn sync_line<'a>(
    state: &'a LyricsPanelState,
    data: RightPanelData<'a>,
    i: usize,
) -> Element<'a, Message> {
    let id = state.line_id(List::Sync, i);
    let l = &data.lyrics[i];
    if l.is_blank() {
        return container(Space::new().height(10))
            .width(Length::Fill)
            .id(id)
            .into();
    }
    let target = i as i64 == data.sync_index;
    let time = studio_text(l.time_ms.map_or_else(|| "—".to_string(), sync_time))
        .size(11)
        .font(font::SEMIBOLD)
        .tabular()
        .color(if l.is_synced() {
            Studio::ACCENT
        } else {
            Studio::TEXT_FAINT
        })
        .wrapping(Wrapping::None);
    // At most two lines of 13 px text, the second ellipsized.
    let words = studio_text(l.text.as_str())
        .size(13)
        .color(if target {
            Studio::TEXT_PRIMARY
        } else {
            Studio::TEXT_SECONDARY
        })
        .width(Length::Fill)
        .ellipsis(2);
    let face = container(row![container(time).width(52), words].align_y(Vertical::Center))
        // 6 × 8, inside a 1 px border (transparent off target).
        .padding([7, 9])
        .width(Length::Fill)
        .style(move |_| {
            if target {
                boxed(
                    Some(alpha(Studio::ACCENT, 0.12)),
                    6.0,
                    Some(alpha(Studio::ACCENT, 0.5)),
                )
            } else {
                boxed(None, 6.0, None)
            }
        });
    container(bare(face, Message::StampLyricAt(i)).width(Length::Fill))
        .padding([2, 0])
        .width(Length::Fill)
        .id(id)
        .into()
}

// ------------------------------------------------------- notes, techniques

/// `_Hint`: a faint 12 px note in a list.
fn hint<'a>(s: &'static str) -> Element<'a, Message> {
    container(text(s).size(12).line_height(1.4).color(Studio::TEXT_FAINT))
        .padding([4, 0])
        .into()
}

/// The tabs' list: a scrolling column, padded (14, 14, 14, 16).
fn tab_list<'a>(items: Column<'a, Message>) -> Element<'a, Message> {
    scrollable(
        items
            .padding(ltrb(14.0, 14.0, 14.0, 16.0))
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::scrollbar)
    .into()
}

/// `vm.seek(Duration(milliseconds: ms))`.
fn seek(ms: i64) -> Message {
    Message::Seek(Duration::from_millis(u64::try_from(ms).unwrap_or(0)))
}

/// `_NotesView`: the selected take's notes (a + to pin one at the playhead,
/// a tap seeks and edits), or with nothing selected every take's notes (a
/// tap selects the take and seeks).
fn notes_view<'a>(data: RightPanelData<'a>) -> Element<'a, Message> {
    let mut items = Column::new();
    if let Some(track) = data.selected_track {
        // Notes annotate vocal takes only; the backing track gets no add
        // button.
        let can_annotate = !track.is_backing();
        items = items.push(notes_header(
            &track.display_name,
            can_annotate.then(|| Message::AddNote(track.id.clone())),
            "Add note at playhead",
        ));
        if !can_annotate {
            items = items.push(hint("Notes apply to vocal takes only."));
        } else if data.selected_track_notes.is_empty() {
            items = items.push(hint(
                "No notes on this take yet.\nUse + to pin one at the playhead, or tap and drag \
                 the timeline lane.",
            ));
        } else {
            for n in data.selected_track_notes {
                items = items.push(note_tile(
                    n,
                    None,
                    vec![
                        seek(n.position_ms),
                        Message::EditNote {
                            track_id: track.id.clone(),
                            note_id: n.id.clone(),
                        },
                    ],
                ));
            }
        }
        return tab_list(items);
    }
    items = items.push(notes_header("All notes", None, "Add note at playhead"));
    if data.all_notes.is_empty() {
        items = items.push(hint("Select a track and pin notes on the timeline."));
    } else {
        for r in data.all_notes {
            items = items.push(note_tile(
                &r.note,
                Some(&r.track_name),
                vec![
                    Message::SelectTrack(r.track_id.clone()),
                    seek(r.note.position_ms),
                ],
            ));
        }
    }
    tab_list(items)
}

/// `_NotesHeader`: the take's name, upper-cased, and the + (with
/// `add_tip`) when it can be annotated.
fn notes_header<'a>(title: &str, on_add: Option<Message>, add_tip: &str) -> Element<'a, Message> {
    let mut header = row![studio_text(title.to_uppercase())
        .size(11)
        .font(font::BOLD)
        .letter_spacing(0.8)
        .color(Studio::TEXT_FAINT)
        .width(Length::Fill)
        .ellipsis(1)]
    .align_y(Vertical::Center);
    if let Some(on_add) = on_add {
        let plus = container(icons::icon(Icon::Plus, 15.0, Studio::ACCENT))
            .center_x(24)
            .center_y(24)
            .style(|_| boxed(Some(Studio::SURFACE_RAISED), 6.0, Some(Studio::BORDER)));
        header = header.push(with_tip(bare(plus, on_add).into(), add_tip, 24.0));
    }
    container(header)
        .padding(Padding {
            bottom: 10.0,
            ..Padding::ZERO
        })
        .into()
}

/// The `mm:ss` (or `mm:ss → mm:ss` for a range) chip of a tile.
fn time_chip<'a>(start_ms: i64, end_ms: Option<i64>) -> Element<'a, Message> {
    let label = match end_ms {
        Some(end) if end > start_ms => {
            format!("{} → {}", format_clock_ms(start_ms), format_clock_ms(end))
        }
        _ => format_clock_ms(start_ms),
    };
    container(
        text(label)
            .size(10)
            .font(font::SEMIBOLD)
            .color(Studio::ACCENT)
            .wrapping(Wrapping::None),
    )
    .padding([2, 6])
    .style(|_| boxed(Some(Studio::INSET), 4.0, None))
    .into()
}

/// A tile: `content` in a raised, outlined card (padded `padding`, inside
/// its 1 px border), 6 px over the next.
fn tile<'a>(content: Element<'a, Message>, padding: Padding) -> Element<'a, Message> {
    container(
        container(content)
            .padding(padding)
            .width(Length::Fill)
            .style(|_| boxed(Some(Studio::PANEL), 6.0, Some(Studio::BORDER))),
    )
    .padding(Padding {
        bottom: 6.0,
        ..Padding::ZERO
    })
    .into()
}

/// `_NoteTile`: the note's time and text (and its take's name, in the
/// all-notes list); a tap reports `on_tap`, in order.
fn note_tile<'a>(
    note: &'a PracticeNote,
    subtitle: Option<&'a str>,
    on_tap: Vec<Message>,
) -> Element<'a, Message> {
    let mut words = column![text(note.content.as_str())
        .size(12.5)
        .line_height(1.35)
        .color(Studio::TEXT_PRIMARY)]
    .width(Length::Fill);
    if let Some(sub) = subtitle {
        words = words.push(Space::new().height(3)).push(
            studio_text(sub)
                .size(10)
                .color(Studio::TEXT_FAINT)
                .ellipsis(1),
        );
    }
    let content = row![
        time_chip(note.position_ms, note.end_ms),
        Space::new().width(8),
        words
    ];
    tap_area(tile(content.into(), Padding::from(10)), on_tap)
}

/// `_TechniquesView`: the selected take's technique markers, mirroring the
/// notes. Backing tracks get no add button; tapping a tile seeks to the
/// marker, and removal is an explicit button so a tap can't accidentally
/// trigger a delete prompt.
fn techniques_view<'a>(data: RightPanelData<'a>) -> Element<'a, Message> {
    let Some(track) = data.selected_track else {
        return tab_list(column![hint(
            "Select a take to see the techniques it uses."
        )]);
    };
    let can_annotate = !track.is_backing();
    let mut items = column![notes_header(
        &track.display_name,
        can_annotate.then(|| Message::AddTechnique(track.id.clone())),
        "Add technique at playhead",
    )];
    if !can_annotate {
        items = items.push(hint("Techniques apply to vocal takes only."));
    } else if data.selected_track_techniques.is_empty() {
        items = items.push(hint(
            "No techniques on this take yet.\nUse + to pin one at the playhead, or tap and drag \
             the timeline lane.",
        ));
    } else {
        for m in data.selected_track_techniques {
            items = items.push(technique_tile(
                m,
                seek(m.position_ms),
                Message::EditTechnique {
                    track_id: track.id.clone(),
                    marker_id: m.id.clone(),
                },
            ));
        }
    }
    tab_list(items)
}

/// `_TechniqueTile`: the marker's time, glyph and technique, and a remove
/// button.
fn technique_tile<'a>(
    marker: &'a TechniqueMarker,
    on_tap: Message,
    on_remove: Message,
) -> Element<'a, Message> {
    let remove = with_tip(
        bare(
            container(icons::icon(Icon::Trash2, 14.0, Studio::TEXT_FAINT)).padding(4),
            on_remove,
        )
        .into(),
        "Remove technique",
        22.0,
    );
    let content = row![
        time_chip(marker.position_ms, marker.end_ms),
        Space::new().width(8),
        icons::icon(Icon::Waypoints, 13.0, Studio::ACCENT),
        Space::new().width(6),
        studio_text(marker.technique_name.as_str())
            .size(12.5)
            .color(Studio::TEXT_PRIMARY)
            .width(Length::Fill)
            .ellipsis(1),
        remove,
    ]
    .align_y(Vertical::Center);
    tap_area(
        tile(content.into(), ltrb(10.0, 10.0, 7.0, 10.0)),
        vec![on_tap],
    )
}

/// A tap target (`GestureDetector(onTap)` under a click cursor): a press and
/// release over `content` that nothing inside it took (a button within)
/// reports `messages`, in order.
fn tap_area<'a>(content: Element<'a, Message>, messages: Vec<Message>) -> Element<'a, Message> {
    Wrap::<Message, bool>::new(
        content,
        Box::new(move |pressed, event, bounds, cursor, shell| match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if cursor.is_over(bounds) && !shell.is_event_captured() {
                    *pressed = true;
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if *pressed => {
                *pressed = false;
                if cursor.is_over(bounds) {
                    for m in &messages {
                        shell.publish(m.clone());
                    }
                }
                shell.capture_event();
            }
            _ => {}
        }),
    )
    .interaction(Box::new(|_, bounds, cursor| {
        cursor
            .is_over(bounds)
            .then_some(mouse::Interaction::Pointer)
    }))
    .into()
}

// -------------------------------------------------------------- Focus view

/// `LyricsFocusOverlay`: full-window large-text karaoke for hands-free
/// singing. The screen stacks it over the page while `vm.lyricsFocus`.
///
/// The panel beneath takes no drop through it (see
/// [`visible_drop_target`]), so the overlay takes it: a lyrics file dropped
/// on the lyrics in focus did nothing at all. Nor the keyboard
/// ([`holds_keyboard`]).
pub fn lyrics_focus_overlay<'a>(
    state: &'a LyricsPanelState,
    data: RightPanelData<'a>,
) -> Element<'a, Message> {
    let editing = data.lyrics_mode == LyricsMode::Edit;
    let body = container(stack![
        lyrics_reader(state, data, true),
        container(icon_btn(
            Icon::Minimize2,
            "Exit focus",
            Message::SetLyricsFocus(false),
            false
        ))
        .padding(Padding {
            top: 14.0,
            right: 16.0,
            ..Padding::ZERO
        })
        .align_x(Horizontal::Right)
        .align_y(Vertical::Top)
        .width(Length::Fill)
        .height(Length::Fill),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(fill(alpha(Studio::BACKGROUND, 0.97)));
    let target = visible_drop_target(body, move |paths: Vec<PathBuf>| {
        // The panel's editor is kept under the overlay with the text it
        // opened on, and its Save would put that back over the import.
        if editing {
            Message::Toast(EXIT_FOCUS_TO_IMPORT.to_string())
        } else {
            Message::ImportLyricsFromFile(paths.into_iter().next().unwrap_or_default())
        }
    });
    // An opaque layer: nothing beneath it sees the pointer.
    holds_keyboard(iced::widget::opaque(target))
}
