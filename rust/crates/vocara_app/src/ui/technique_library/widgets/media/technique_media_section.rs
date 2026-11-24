//! Port of `ui/technique_library/widgets/media/technique_media_section.dart`:
//! the "Examples" block in the technique viewer — the technique's audio/video
//! clips, each with an inline player, plus a drop/browse zone to add more.
//!
//! Stateless over the data; all mutations go through the host so the view
//! model owns persistence and file IO. The Dart widget took plain data and
//! callbacks; here it takes plain data ([`TechniqueMediaSection`], its
//! constructor's parameters) and reports [`Message`]s, which
//! [`MediaSectionState::update`] turns into the callbacks' calls
//! ([`Event`]s) for the host to carry out.
//!
//! # How the technique page composes it
//!
//! The page keeps a [`MediaSectionState`] (what Dart's `_MediaTile` and
//! `_AddZone` states held: the hovered card, a drag over the zone) and an
//! [`ExamplePlayers`] (the cards' players; see `example_players`). Then:
//!
//! - on every change to the examples it shows, [`sync_players`];
//! - `view`: [`TechniqueMediaSection::view`] with the view model's data, the
//!   analysis panel's builder (`analysisBuilder`) and its own message
//!   constructor (`Message::Media`, say);
//! - `update`: its `Message::Media(m)` goes to [`MediaSectionState::update`];
//!   the [`Event`] that comes back is the Dart callback to run —
//!   `onAddPaths`, `onRemove` (the page's confirmation first), `onRename`,
//!   `onExport`, `onToggleExpand`, `onStartRecord` (showing the error it
//!   returns in a snack bar, as `_RecordRow._start` did), `onStopRecord`,
//!   `onPlaybackPosition` — or a step the Dart widget took itself and the
//!   page now runs: [`browse`] (the file picker) and the rename dialog
//!   ([`rename_dialog`], through the app's dialog stack);
//! - `subscription`: [`subscription`] (the players' poll);
//! - the view model's signals: `micArmed` (`pauseSignal`) →
//!   [`ExamplePlayers::pause_all`]; `analysisScrub` →
//!   [`ExamplePlayers::seek_analyzed`].
//!
//! # Differences from Flutter
//!
//! The hover icons fade (120 ms), the zone's padding and drag highlight
//! move (120 ms) and the chevron turns (150 ms), all linear, as the Dart
//! implicit animations did; the turned chevron at rest is the up chevron
//! (the same glyph turned half a turn). iced has no subtree opacity: the
//! faded zone fades each of its colors instead, and the hover icons their
//! glyphs'. No italic Inter face is bundled, so "Empty content"
//! asks for italic and draws upright. The Dart level meter's fill had no
//! height (a childless `ColoredBox` under `Align` takes the smallest size),
//! so it never showed; here it fills the meter's 8 px, as intended.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path as FilePath, PathBuf};
use std::time::Duration;

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::canvas::{self, Frame, Geometry, Path};
use iced::widget::{
    button, canvas as canvas_widget, column, container, keyed_column, mouse_area, row, text_input,
    Column, Space,
};
use iced::{
    mouse, widget, Color, Element, Font, Length, Padding, Point, Rectangle, Renderer, Size,
    Subscription, Task, Theme,
};
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_model::util::dart_string;
use vocara_services::media_player::MediaPlayerFactory;

use super::example_players::{self, ExampleClip, ExamplePlayers};
use super::technique_audio_player as audio;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::Curve;
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::animated::{animated, animated_container, Decoration};
use crate::ui::core::widgets::confirm_dialog::{self, modal, DialogResult, Fade};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::visible_drop_target::visible_drop_target;

/// How long the hover icons take to fade in or out (`AnimatedOpacity`).
pub const HOVER_ANIMATION: Duration = Duration::from_millis(120);
/// How long the chevron takes to turn (`AnimatedRotation`).
pub const CHEVRON_ANIMATION: Duration = Duration::from_millis(150);
/// How long the drop zone takes to change (`AnimatedContainer`).
pub const ZONE_ANIMATION: Duration = Duration::from_millis(120);

/// What the zone says while the technique has no example.
pub const ADD_FIRST: &str = "Drag an audio or video clip here, or click to browse";
/// What the compact zone says once it has some.
pub const ADD_ANOTHER: &str = "Add another example";
/// The read view's placeholder when there is no example.
pub const EMPTY: &str = "Empty content";
/// The record row's idle button.
pub const RECORD: &str = "Record an example";
/// The record row's button while a take records.
pub const STOP: &str = "Stop";
/// The record row while a take's start is pending (permission prompt, mic
/// open).
pub const STARTING: &str = "Starting the microphone…";
/// The record row while a stopped take is imported.
pub const SAVING: &str = "Saving example…";

/// The picker's type group (`XTypeGroup(label: ...)`).
const PICKER_LABEL: &str = "Audio or video";

/// The section's data: the Dart widget's constructor parameters, with each
/// optional callback as whether the host offers it. Start from
/// [`TechniqueMediaSection::new`] (Dart's defaults) and set the rest.
#[derive(Debug, Clone, Copy)]
pub struct TechniqueMediaSection<'a> {
    pub media: &'a [TechniqueMedia],
    pub importing: bool,

    /// Media id of the example whose analysis is currently shown: its inline
    /// player drives the analysis playhead so the spectrogram follows along
    /// as it plays. An id, not a path: byte-identical examples share one
    /// pooled file, and a path matched every one of them.
    pub analyzed_id: Option<&'a str>,

    /// Whether a take is arming or recording: no example can be started
    /// then.
    pub mic_live: bool,

    /// Media id of the example whose inline analysis is expanded
    /// (accordion), or none.
    pub expanded_id: Option<&'a str>,

    /// Whether a card's inline analysis can be toggled open/closed
    /// (`onToggleExpand` given).
    pub expandable: bool,

    /// True while a recording is in flight (drives the live preview
    /// placement).
    pub live_monitor: bool,

    /// Whether an example can be copied out of the app (`onExport` given);
    /// offered on every card, read view included, as it changes nothing.
    pub exportable: bool,

    /// When false (the read view), the clips are play-only: no add/drop zone
    /// and no per-clip rename/remove controls.
    pub editable: bool,

    /// False freezes every mutation control while keeping playback alive —
    /// used while the technique save is committing, when a clip added,
    /// removed, renamed or recorded during the await would be silently
    /// thrown away as the save lands. Unlike [`editable`](Self::editable) it
    /// keeps the add zone and record row visible (just inert), so the layout
    /// doesn't jump for the moment the commit is in flight.
    pub enabled: bool,

    /// Whether a "Record" affordance is offered (edit view: `onStartRecord`
    /// and `onStopRecord` given).
    pub recordable: bool,
    pub is_recording: bool,
    pub saving_recording: bool,

    /// True while an example recording's start is still pending (the mic
    /// permission prompt, the mic open). [`saving_recording`](Self::saving_recording)
    /// covers this window too, so the controls stay busy; this only picks
    /// the record row's label.
    pub starting_recording: bool,

    /// Whether a new example recording can start now (false e.g. while a
    /// drag-drop import is still copying).
    pub can_record: bool,

    /// The record level meter's value (`recordLevel`; 0 with none).
    pub record_level: f32,
}

impl<'a> TechniqueMediaSection<'a> {
    /// The section over `media` with Dart's defaults: editable, enabled, no
    /// recording, nothing analyzed or expanded, no export.
    pub fn new(media: &'a [TechniqueMedia]) -> Self {
        Self {
            media,
            importing: false,
            analyzed_id: None,
            mic_live: false,
            expanded_id: None,
            expandable: false,
            live_monitor: false,
            exportable: false,
            editable: true,
            enabled: true,
            recordable: false,
            is_recording: false,
            saving_recording: false,
            starting_recording: false,
            can_record: true,
            record_level: 0.0,
        }
    }

    /// Whether the cards offer rename/remove: a frozen edit surface
    /// (in-flight save) hides the hover rename/remove icons too — the clip
    /// stays playable.
    fn mutable(&self) -> bool {
        self.editable && self.enabled
    }

    /// Whether the zone takes taps and drops: not while an import is in
    /// flight (a second import can't be launched on top of the first), nor
    /// while a take records or saves, nor frozen.
    fn zone_enabled(&self) -> bool {
        self.enabled && !self.importing && !self.is_recording && !self.saving_recording
    }

    /// Whether the record row's Record is live.
    fn record_live(&self) -> bool {
        self.can_record && self.enabled
    }

    /// Whether the card of `m` offers its analysis: audio clips only, when
    /// the page has an analysis to show and lets the cards toggle it.
    fn can_expand(&self, m: &TechniqueMedia, has_analysis: bool) -> bool {
        !m.is_video() && has_analysis && self.expandable
    }

    fn media_of(&self, id: &str) -> Option<&'a TechniqueMedia> {
        self.media.iter().find(|m| m.id == id)
    }
}

/// Builds the analysis panel (`analysisBuilder`): shown inside the expanded
/// card (and, while recording an example, under the record control). Called
/// at most once per view.
pub type AnalysisBuilder<'a, M> = Box<dyn FnOnce() -> Element<'a, M> + 'a>;

/// What the section's widgets report.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The pointer entered (`true`) or left the card of this example.
    Hover(String, bool),
    /// A drag the zone would take entered (`true`) or left it.
    DragHover(bool),
    /// A card's player.
    Player(example_players::Message),
    /// A card's name or chevron: toggle its analysis.
    ToggleExpand(String),
    /// A card's Export.
    Export(String),
    /// A card's Rename: ask for a caption, starting from `current`.
    Rename { id: String, current: String },
    /// The rename dialog closed: the trimmed caption (empty: back to the
    /// file name), or nothing when cancelled.
    Renamed { id: String, label: Option<String> },
    /// A card's Remove.
    Remove(String),
    /// A tap on the zone: browse.
    Browse,
    /// The picker closed with these files (none: cancelled).
    Picked(Vec<String>),
    /// Files dropped on the zone.
    Dropped(Vec<PathBuf>),
    /// The record row's Record.
    StartRecord,
    /// The record row's Stop.
    StopRecord,
}

/// What a [`Message`] asks of the host: Dart's callbacks, and the steps the
/// Dart widget took itself that need the app (the picker, the dialog).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `onAddPaths`: every path dropped or picked (the view model filters,
    /// and reports the unsupported ones).
    AddPaths(Vec<String>),
    /// Open the file picker: run [`browse`].
    Browse,
    /// `onRemove(id)`.
    Remove(String),
    /// Ask for the example's new caption: show [`rename_dialog`].
    Rename { id: String, current: String },
    /// `onRename(id, label)`.
    Renamed { id: String, label: String },
    /// `onExport(id)`.
    Export(String),
    /// `onToggleExpand(id)`.
    ToggleExpand(String),
    /// `onStartRecord()`; an error it returns is shown in a snack bar.
    StartRecord,
    /// `onStopRecord()`.
    StopRecord,
    /// `onPlaybackPosition(at)`: the analyzed example's player moved.
    Position(Duration),
}

/// What Dart's `_MediaTile` and `_AddZone` states held, kept by the page.
#[derive(Debug, Clone, Default)]
pub struct MediaSectionState {
    /// The card under the pointer.
    hovered: Option<String>,
    /// A drag the zone would take is over it.
    dragging: bool,
}

impl MediaSectionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The example whose card is under the pointer.
    pub fn hovered(&self) -> Option<&str> {
        self.hovered.as_deref()
    }

    /// Whether a drag the zone would take is over it.
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// Handles a message of the section showing `section`, whose players
    /// are `players` (built by `factory`); returns what the host must do, if
    /// anything. A tap reaches nothing where the Dart control it came from
    /// is now inert.
    pub fn update(
        &mut self,
        message: Message,
        section: &TechniqueMediaSection<'_>,
        players: &mut ExamplePlayers,
        factory: &dyn MediaPlayerFactory,
    ) -> Option<Event> {
        match message {
            Message::Hover(id, true) => {
                self.hovered = Some(id);
                None
            }
            Message::Hover(id, false) => {
                if self.hovered.as_deref() == Some(id.as_str()) {
                    self.hovered = None;
                }
                None
            }
            Message::DragHover(hover) => {
                self.dragging = hover;
                None
            }
            // Only the analyzed clip's player is linked to the analysis
            // playhead (both directions).
            Message::Player(m) => players
                .update(m, factory, section.mic_live, section.analyzed_id)
                .map(Event::Position),
            // An audio card's name or chevron (neither toggles without an
            // analysis to show, which only the view knows of).
            Message::ToggleExpand(id) => {
                let expandable =
                    section.expandable && section.media_of(&id).is_some_and(|m| !m.is_video());
                expandable.then_some(Event::ToggleExpand(id))
            }
            Message::Export(id) => section.exportable.then_some(Event::Export(id)),
            Message::Rename { id, current } => {
                section.mutable().then_some(Event::Rename { id, current })
            }
            // Dart awaited the dialog, then called the card's `onRename`
            // whatever had happened meanwhile (the view model guards an
            // in-flight save).
            Message::Renamed { id, label } => label.map(|label| Event::Renamed { id, label }),
            Message::Remove(id) => section.mutable().then_some(Event::Remove(id)),
            Message::Browse => section.zone_enabled().then_some(Event::Browse),
            // `_browse`: nothing picked, nothing to add.
            Message::Picked(paths) => (!paths.is_empty()).then_some(Event::AddPaths(paths)),
            Message::Dropped(paths) => {
                if !section.zone_enabled() || paths.is_empty() {
                    return None;
                }
                // Hand over EVERY dropped path: the view model filters, and
                // it needs the unsupported ones to report them. Filtering
                // here made a drop of only unsupported files do nothing at
                // all — no import, no notice — and under-counted the skips
                // in a mixed drop.
                Some(Event::AddPaths(
                    paths.iter().map(|p| path_string(p.as_path())).collect(),
                ))
            }
            Message::StartRecord => {
                let live = section.recordable
                    && !section.saving_recording
                    && !section.is_recording
                    && section.record_live();
                live.then_some(Event::StartRecord)
            }
            // A frozen surface also inerts Stop: finalizing a take
            // mid-commit would stage a clip the airborne save has already
            // snapshotted past (the VM guards this too).
            Message::StopRecord => {
                let live = section.recordable
                    && !section.saving_recording
                    && section.is_recording
                    && section.enabled;
                live.then_some(Event::StopRecord)
            }
        }
    }
}

/// A path as the Dart side saw it (a string).
fn path_string(path: &FilePath) -> String {
    path.to_string_lossy().into_owned()
}

/// The clip the players key `m` by.
pub fn clip_of(m: &TechniqueMedia) -> ExampleClip<'_> {
    ExampleClip {
        id: &m.id,
        path: &m.path,
        video: m.is_video(),
    }
}

/// Reconciles `players` with the examples now shown (call it on every
/// change to them).
pub fn sync_players(
    players: &mut ExamplePlayers,
    media: &[TechniqueMedia],
    factory: &dyn MediaPlayerFactory,
) {
    players.sync(media.iter().map(clip_of), factory);
}

/// The players' poll, while one needs it.
pub fn subscription(players: &ExamplePlayers) -> Subscription<Message> {
    players.subscription().map(Message::Player)
}

/// `_browse`: the platform's open panel for one or more clips; the files
/// come back as [`Message::Picked`]. Call it from `update` (the panel is
/// set up on the UI thread).
pub fn browse() -> Task<Message> {
    // Derive the picker filter from the same source as drag-drop's
    // isSupportedMedia so the two entry paths accept identical formats.
    let picking = vocara_services::file_panels::AsyncFileDialog::new()
        .add_filter(PICKER_LABEL, &TechniqueMedia::picker_extensions())
        .pick_files();
    Task::future(picking).map(|files| {
        Message::Picked(
            files
                .unwrap_or_default()
                .iter()
                .map(|f| path_string(f.path()))
                .collect(),
        )
    })
}

/// The rename dialog for example `id`, captioned `current` now, and how its
/// result comes back (show them with `Update::dialog`).
pub fn rename_dialog(
    id: String,
    current: &str,
) -> (
    RenameExampleDialog,
    impl FnOnce(DialogResult) -> Message + Send + 'static,
) {
    (RenameExampleDialog::new(current), move |result| {
        Message::Renamed {
            id,
            label: result.text(),
        }
    })
}

/// What a card is kept by in the widget tree: its example (Flutter keyed
/// each card `ValueKey(m.id)`: unkeyed, removing a card handed every later
/// slot to the next clip and rebuilt its player mid-playback).
fn card_key(id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// `color` with its alpha scaled by `opacity` (the faded zone).
fn fade(color: Color, opacity: f32) -> Color {
    alpha(color, color.a * opacity)
}

impl<'a> TechniqueMediaSection<'a> {
    /// The section: the caption, a card per example, then — editing — the
    /// add zone, the record row and the live preview; or, read-only with no
    /// example, the placeholder. `analysis` builds the analysis panel (none
    /// in contexts without analysis); `on_message` wraps the section's
    /// messages in the host's.
    pub fn view<M, F>(
        self,
        state: &'a MediaSectionState,
        players: &'a ExamplePlayers,
        analysis: Option<AnalysisBuilder<'a, M>>,
        on_message: F,
    ) -> Element<'a, M>
    where
        M: Clone + 'a,
        F: Fn(Message) -> M + Clone + 'a,
    {
        let has_analysis = analysis.is_some();
        let mut analysis = analysis;
        let mut header = row![
            studio_text("EXAMPLES")
                .size(10.5)
                .font(font::BOLD)
                .letter_spacing(0.6)
                .color(Studio::ACCENT_BRIGHT),
            Space::new().width(8),
        ]
        .align_y(Vertical::Center);
        if self.importing {
            header = header.push(spinner(12.0, 1.8, Studio::TEXT_SECONDARY));
        }
        let mut content: Column<'a, M> = column![
            Element::from(header).map(on_message.clone()),
            Space::new().height(10),
        ];

        let mut cards = keyed_column(Vec::new()).spacing(12);
        for m in self.media {
            // Pair each audio example with a collapsible analysis. While
            // recording an example the live preview shows under the record
            // control instead, so the card's static analysis is hidden then.
            let shows_analysis =
                self.expanded_id == Some(m.id.as_str()) && (!self.live_monitor || !self.editable);
            let panel = if shows_analysis {
                analysis.take().map(|build| build())
            } else {
                None
            };
            cards = cards.push(
                // Keyed by clip.
                card_key(&m.id),
                self.card(m, state, players, has_analysis, panel, &on_message),
            );
        }
        if !self.media.is_empty() {
            content = content.push(cards).push(Space::new().height(12));
        }

        if self.editable {
            content = content.push(Element::from(self.add_zone(state)).map(on_message.clone()));
            if self.recordable {
                content = content
                    .push(Space::new().height(8))
                    .push(self.record_row().map(on_message.clone()));
            }
            // Live preview while recording an example (where the new clip
            // has no card yet) — surfaced so a recording is never a blank
            // panel.
            if self.live_monitor {
                if let Some(build) = analysis.take() {
                    content = content.push(Space::new().height(12)).push(build());
                }
            }
        } else if self.media.is_empty() {
            // Read view with no clips: keep the field visible with a
            // placeholder.
            content = content.push(
                text(EMPTY)
                    .size(13)
                    .font(Font {
                        style: iced::font::Style::Italic,
                        ..font::REGULAR
                    })
                    .color(Studio::TEXT_FAINT),
            );
        }
        content.width(Length::Fill).into()
    }

    /// One example, as a card: a header (icon + name, rename/remove on hover
    /// while editing, and an expand chevron), its inline player, and — when
    /// expanded — its paired analysis. Each recording is thus paired
    /// one-to-one with its analysis.
    fn card<M, F>(
        &self,
        m: &'a TechniqueMedia,
        state: &'a MediaSectionState,
        players: &'a ExamplePlayers,
        has_analysis: bool,
        analysis: Option<Element<'a, M>>,
        on_message: &F,
    ) -> Element<'a, M>
    where
        M: Clone + 'a,
        F: Fn(Message) -> M + Clone + 'a,
    {
        let hovered = state.hovered() == Some(m.id.as_str());
        let editable = self.mutable();
        let can_expand = self.can_expand(m, has_analysis);
        let expanded = self.expanded_id == Some(m.id.as_str());
        let label = m.display_label();

        let mut header = row![
            icons::icon(
                if m.is_video() {
                    Icon::Video
                } else {
                    Icon::Music2
                },
                13.0,
                Studio::TEXT_FAINT,
            ),
            Space::new().width(6),
            // The name area toggles the analysis (when expandable).
            button(
                studio_text(label.clone())
                    .size(12)
                    .font(font::MEDIUM)
                    .color(Studio::TEXT_SECONDARY)
                    .width(Length::Fill)
                    .ellipsis(1),
            )
            .width(Length::Fill)
            .padding(0)
            .style(bare_button)
            .on_press_maybe(can_expand.then(|| Message::ToggleExpand(m.id.clone()))),
        ]
        .align_y(Vertical::Center);
        if editable || self.exportable {
            header = header.push(Space::new().width(8)).push(hover_actions(
                hovered,
                self.exportable,
                editable,
                m.id.clone(),
                label,
            ));
        }
        if can_expand {
            header = header.push(Space::new().width(8)).push(expand_chevron(
                expanded,
                Message::ToggleExpand(m.id.clone()),
            ));
        }

        // The page syncs its players with the examples it shows; until it
        // has, the card holds the player's place.
        let player: Element<'a, Message> = match players.view(&m.id, self.mic_live) {
            Some(player) => player.map(Message::Player),
            None => Space::new().height(audio::HEIGHT).into(),
        };

        let mut body: Column<'a, M> = column![
            Element::from(header).map(on_message.clone()),
            Space::new().height(8),
            player.map(on_message.clone()),
        ];
        if let Some(analysis) = analysis {
            body = body
                .push(Space::new().height(14))
                .push(divider())
                .push(Space::new().height(14))
                .push(analysis);
        }
        let border = if expanded {
            alpha(Studio::ACCENT, 0.4)
        } else {
            Studio::BORDER
        };
        // 12/10/12/12 of padding inside the 1 px border (a decoration's
        // border pads its child; an iced border does not).
        let card = container(body.width(Length::Fill))
            .width(Length::Fill)
            .padding(Padding {
                top: 11.0,
                right: 13.0,
                bottom: 13.0,
                left: 13.0,
            })
            .style(move |_| boxed(Some(Studio::SURFACE), 10.0, Some(border)));
        mouse_area(card)
            .on_enter(on_message(Message::Hover(m.id.clone(), true)))
            .on_exit(on_message(Message::Hover(m.id.clone(), false)))
            .into()
    }

    /// The drop + browse zone. Full size when empty, compact once clips
    /// exist.
    fn add_zone(&self, state: &MediaSectionState) -> Element<'a, Message> {
        let enabled = self.zone_enabled();
        let dense = !self.media.is_empty();
        let dragging = state.dragging() && enabled;
        let opacity = if enabled { 1.0 } else { 0.5 };
        let fg = fade(
            if dragging {
                Studio::ACCENT
            } else {
                Studio::TEXT_SECONDARY
            },
            opacity,
        );
        let (fill, border) = if dragging {
            (alpha(Studio::ACCENT, 0.08), alpha(Studio::ACCENT, 0.6))
        } else {
            (Studio::PANEL, Studio::BORDER)
        };
        let content = row![
            icons::icon(Icon::Plus, 15.0, fg),
            Space::new().width(8),
            studio_text(if dense { ADD_ANOTHER } else { ADD_FIRST })
                .size(12.5)
                .font(font::MEDIUM)
                .color(fg)
                .ellipsis(1),
        ]
        .align_y(Vertical::Center);
        // 16 across and 12 (dense) or 22 down, inside the 1 px border. The
        // padding and the drag's tint move over 120 ms, linear (the Dart
        // `AnimatedContainer`); the faded zone's opacity applies at once.
        let zone = animated_container(content, ZONE_ANIMATION)
            .width(Length::Fill)
            .align((0.0, -1.0))
            .padding([if dense { 13.0 } else { 23.0 }, 17.0])
            .decoration(Decoration::boxed(fill, 10.0, border, 1.0))
            .opacity(opacity);
        let tap = button(zone)
            .width(Length::Fill)
            .padding(0)
            .style(bare_button)
            .on_press_maybe(enabled.then_some(Message::Browse));
        // Only where the zone can be seen (see VisibleDropTarget): a drop
        // over a dialog above it added the clip to this technique.
        visible_drop_target(tap, Message::Dropped)
            .on_hover(Message::DragHover)
            .enable(enabled)
            .into()
    }

    /// Record-an-example control under the drop zone: a Record button that
    /// shows a "Starting…" state while the mic opens, becomes a level meter +
    /// Stop while recording, then a "Saving…" state while the clip is
    /// imported as a persisted example.
    fn record_row(&self) -> Element<'a, Message> {
        if self.saving_recording {
            return row![
                spinner(13.0, 1.8, Studio::TEXT_SECONDARY),
                Space::new().width(8),
                // A pending start is busy too, but nothing is recorded yet:
                // it read "Saving example…" through the permission prompt
                // and the mic open.
                text(if self.starting_recording {
                    STARTING
                } else {
                    SAVING
                })
                .size(12)
                .color(Studio::TEXT_FAINT),
            ]
            .align_y(Vertical::Center)
            .into();
        }
        if self.is_recording {
            return row![
                canvas_widget(LevelMeter {
                    level: self.record_level,
                })
                .width(Length::Fill)
                .height(METER_HEIGHT),
                Space::new().width(10),
                rec_button(
                    STOP,
                    Icon::Square,
                    true,
                    self.enabled.then_some(Message::StopRecord),
                ),
            ]
            .align_y(Vertical::Center)
            .into();
        }
        container(rec_button(
            RECORD,
            Icon::Mic,
            false,
            self.record_live().then_some(Message::StartRecord),
        ))
        .width(Length::Fill)
        .align_x(Horizontal::Left)
        .into()
    }
}

/// `Divider(height: 1, thickness: 1, color: Studio.border)`.
fn divider<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(Studio::BORDER))
        .into()
}

/// A card's hover icons — Export (when `exportable`), then Rename and
/// Remove (when `editable`) — faded in while the card is `hovered` and out
/// when it is not, over 120 ms (`AnimatedOpacity`); there, and tappable,
/// either way (a faded widget keeps its place and its taps).
fn hover_actions<'a>(
    hovered: bool,
    exportable: bool,
    editable: bool,
    id: String,
    label: String,
) -> Element<'a, Message> {
    let actions = move |opacity: f32| -> Element<'a, Message> {
        let mut actions = row![].align_y(Vertical::Center);
        if exportable {
            actions = actions.push(hover_icon(
                Icon::Download,
                "Export",
                Message::Export(id.clone()),
                opacity,
            ));
            if editable {
                actions = actions.push(Space::new().width(4));
            }
        }
        if editable {
            actions = actions
                .push(hover_icon(
                    Icon::Pencil,
                    "Rename",
                    Message::Rename {
                        id: id.clone(),
                        current: label.clone(),
                    },
                    opacity,
                ))
                .push(Space::new().width(4))
                .push(hover_icon(
                    Icon::Trash2,
                    "Remove",
                    Message::Remove(id.clone()),
                    opacity,
                ));
        }
        actions.into()
    };
    let target = if hovered { 1.0 } else { 0.0 };
    animated(target, HOVER_ANIMATION, Curve::Linear, actions).into()
}

/// `_HoverIcon`: a 14 px faint glyph with a click cursor and a tooltip,
/// drawn at `opacity`.
fn hover_icon<'a>(icon: Icon, tip: &'a str, on_tap: Message, opacity: f32) -> Element<'a, Message> {
    let color = alpha(Studio::TEXT_FAINT, opacity);
    let target = button(icons::icon(icon, 14.0, color))
        .padding(0)
        .style(bare_button)
        .on_press(on_tap);
    // Material's tooltip sits 24 px below the target's centre.
    material_tooltip(target, tip, 24.0 - 14.0 / 2.0).into()
}

/// `_ExpandChevron`: the expand/collapse affordance, a chevron that turns
/// half a turn when open, over 150 ms (`AnimatedRotation`); its color
/// changes at once. At rest it is the down or the up chevron (the down one
/// turned half a turn); while it turns, the chevron's path, turned.
fn expand_chevron<'a>(expanded: bool, on_tap: Message) -> Element<'a, Message> {
    let color = if expanded {
        Studio::ACCENT_BRIGHT
    } else {
        Studio::TEXT_FAINT
    };
    let face = move |turns: f32| -> Element<'a, Message> {
        let glyph: Element<'a, Message> = if turns == 0.0 {
            icons::icon(Icon::ChevronDown, 16.0, color).into()
        } else if turns == 0.5 {
            icons::icon(Icon::ChevronUp, 16.0, color).into()
        } else {
            canvas_widget(TurnedChevron { turns, color })
                .width(16)
                .height(16)
                .into()
        };
        button(container(glyph).padding(4))
            .padding(0)
            .style(bare_button)
            .on_press(on_tap.clone())
            .into()
    };
    let turns = if expanded { 0.5 } else { 0.0 };
    animated(turns, CHEVRON_ANIMATION, Curve::Linear, face).into()
}

/// Lucide's `chevron-down` (`m6 9 6 6 6-6` on its 24 grid, a 2-unit round
/// stroke), `turns` of a turn about its centre.
struct TurnedChevron {
    turns: f32,
    color: Color,
}

impl canvas::Program<Message> for TurnedChevron {
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
        let scale = bounds.width / 24.0;
        frame.translate(iced::Vector::new(bounds.width / 2.0, bounds.height / 2.0));
        frame.rotate(self.turns * std::f32::consts::TAU);
        let at = |x: f32, y: f32| Point::new((x - 12.0) * scale, (y - 12.0) * scale);
        let chevron = Path::new(|p| {
            p.move_to(at(6.0, 9.0));
            p.line_to(at(12.0, 15.0));
            p.line_to(at(18.0, 9.0));
        });
        frame.stroke(
            &chevron,
            canvas::Stroke::default()
                .with_color(self.color)
                .with_width(2.0 * scale)
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
        vec![frame.into_geometry()]
    }
}

/// `_RecBtn`: a filled pill (coral, or red for `danger`), dark label; grey
/// and inert with no `on_tap`.
fn rec_button<'a>(
    label: &'a str,
    icon: Icon,
    danger: bool,
    on_tap: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let fill = match (enabled, danger) {
        (false, _) => Studio::SURFACE_RAISED,
        (true, true) => Studio::RECORD,
        (true, false) => Studio::ACCENT,
    };
    let fg = if enabled {
        Studio::BACKGROUND
    } else {
        Studio::TEXT_FAINT
    };
    let face = container(
        row![
            icons::icon(icon, 13.0, fg),
            Space::new().width(6),
            text(label).size(12).font(font::BOLD).color(fg),
        ]
        .align_y(Vertical::Center),
    )
    .padding([7, 12])
    .style(move |_| boxed(Some(fill), 7.0, None));
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press_maybe(on_tap)
        .into()
}

/// The level meter's height.
const METER_HEIGHT: f32 = 8.0;

/// The record level meter: an inset track, radius 4, filled in the record
/// red from the left to `level` (clamped to 0..1).
struct LevelMeter {
    level: f32,
}

/// How wide the meter's fill is for `level` in a `width`-wide track.
pub fn meter_fill(level: f32, width: f32) -> f32 {
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    level * width
}

impl canvas::Program<Message> for LevelMeter {
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
        let track = Path::rounded_rectangle(Point::ORIGIN, bounds.size(), 4.0.into());
        frame.fill(&track, Studio::INSET);
        let filled = meter_fill(self.level, bounds.width);
        if filled > 0.0 {
            // The track's rounded shape, cut at the fill (the clip of
            // `Clip.antiAlias` with the track's radius).
            frame.with_clip(
                Rectangle::new(Point::ORIGIN, Size::new(filled, bounds.height)),
                |frame| frame.fill(&track, Studio::RECORD),
            );
        }
        vec![frame.into_geometry()]
    }
}

// ------------------------------------------------------------ the dialog

/// `_promptLabel`'s dialog: a tiny single-field dialog to rename a clip. It
/// closes with the trimmed label ([`DialogResult::Text`]`(Some(..))`; an
/// empty value clears back to the file name), or with `Text(None)` if
/// cancelled (Cancel, a barrier tap, Escape).
///
/// The app's dialog stack owns it (`Dialog::RenameExample`), as it owns the
/// other studio dialogs: it draws [`view`](Self::view) over the window, runs
/// [`focus`](Self::focus) when it opens (`autofocus`), and feeds it the
/// messages until [`update`](Self::update) says it closed. (Dart owned the
/// field's controller in the dialog's state so it was disposed only after
/// the exit transition — the use-after-dispose `_StudioPrompt` /
/// `_NewTechniqueDialog` already fixed; the state here is plain data and
/// goes with the dialog.)
#[derive(Debug, Clone)]
pub struct RenameExampleDialog {
    text: String,
    input: widget::Id,
}

impl RenameExampleDialog {
    /// The dialog with the field holding `current`.
    pub fn new(current: &str) -> Self {
        Self {
            text: current.to_string(),
            input: widget::Id::unique(),
        }
    }

    /// The caption as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The field's id.
    pub fn input_id(&self) -> widget::Id {
        self.input.clone()
    }

    /// The task that puts the keyboard in the field when the dialog opens
    /// (`autofocus: true`).
    pub fn focus<T: Send + 'static>(&self) -> Task<T> {
        iced::widget::operation::focus(self.input.clone())
    }

    /// Handles `message`; `Some` when the dialog closes.
    pub fn update(&mut self, message: confirm_dialog::Message) -> Option<DialogResult> {
        match message {
            confirm_dialog::Message::Input(text) => {
                self.text = text;
                None
            }
            confirm_dialog::Message::Cancel => Some(DialogResult::Text(None)),
            // Save, or Return in the field: the trimmed text, empty or not.
            confirm_dialog::Message::Confirm => Some(DialogResult::Text(Some(
                dart_string::trim(&self.text).to_string(),
            ))),
        }
    }

    /// The dialog over the whole window, all in: the barrier
    /// (`Color(0xB0000000)`) and the centred 380-wide card.
    pub fn view(&self) -> Element<'_, confirm_dialog::Message> {
        self.view_at(Fade::SHOWN)
    }

    /// The dialog at `fade` (`confirm_dialog`'s module docs).
    pub fn view_at(&self, fade: Fade) -> Element<'_, confirm_dialog::Message> {
        use confirm_dialog::Message;
        let field = text_input("Caption", &self.text)
            .id(self.input.clone())
            .on_input(Message::Input)
            .on_submit(Message::Confirm)
            .size(14)
            .line_height(1.0)
            .padding(theme::FIELD_PADDING)
            .style(fade.input(theme::studio_input));
        let card = container(
            column![
                text("Rename example")
                    .size(15)
                    .font(font::BOLD)
                    .color(fade.color(Studio::TEXT_PRIMARY)),
                Space::new().height(14),
                // 12 by 10 inside the well's 1 px border.
                container(field)
                    .width(Length::Fill)
                    .padding([11, 13])
                    .style(fade.style(theme::inset(8.0))),
                Space::new().height(18),
                row![
                    Space::new().width(Length::Fill),
                    dialog_button("Cancel", Message::Cancel, false, fade),
                    Space::new().width(8),
                    dialog_button("Save", Message::Confirm, true, fade),
                ],
            ]
            .width(Length::Fill),
        )
        .width(380)
        // 20 inside the 1 px border.
        .padding(21)
        .style(fade.style(|_| boxed(Some(Studio::SURFACE_RAISED), 16.0, Some(Studio::BORDER))));
        modal(container(card).padding(24).into(), fade)
    }
}

/// `_DialogButton`: the rename dialog's flat buttons (no hover change), at
/// the dialog's `fade`.
fn dialog_button<'a>(
    label: &'a str,
    on_tap: confirm_dialog::Message,
    accent: bool,
    fade: Fade,
) -> Element<'a, confirm_dialog::Message> {
    let (fill, border, fg) = if accent {
        (
            alpha(Studio::ACCENT, 0.16),
            alpha(Studio::ACCENT, 0.45),
            Studio::ACCENT_BRIGHT,
        )
    } else {
        (Studio::SURFACE_RAISED, Studio::BORDER, Studio::TEXT_PRIMARY)
    };
    let (fill, border, fg) = (fade.color(fill), fade.color(border), fade.color(fg));
    // 14 by 8 inside the 1 px border.
    let face = container(text(label).size(13).font(font::SEMIBOLD).color(fg))
        .padding([9, 15])
        .style(move |_| boxed(Some(fill), 7.0, Some(border)));
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(on_tap)
        .into()
}

#[cfg(test)]
mod tests;
