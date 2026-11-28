// The examples section on screen, headless over fake media players: the
// section-level cases of Dart's technique_detail_layout_test,
// technique_example_player_test, technique_example_identity_test and
// technique_save_selection_test (the screen and the view model around the
// section are ported elsewhere; here a small page stands in for them).

use std::borrow::Cow;

use crate::ui::core::text;

use iced::widget::{button, column, scrollable, Space};
use iced::{window, Event as IcedEvent, Size};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use vocara_model::util::dart_date_time::DateTime;
use vocara_services::media_player::{FakeMediaPlayers, FakePlayerHandle};

use super::*;
use crate::ui::core::widgets::confirm_dialog::{DialogState, DialogStep};
use crate::ui::technique_library::widgets::media::example_players::{ExampleKey, ExamplePlayer};
use crate::ui::technique_library::widgets::media::technique_audio_player::UNAVAILABLE_CLIP_MESSAGE;

const SIZE: Size = Size::new(1200.0, 1400.0);

/// The analysis panel the page builds (`TechniqueExampleAnalysis`).
const ANALYSIS: &str = "The example's analysis";

/// The placeholder `asset://` path the repository hydrates an example to
/// when its pooled bytes are missing on this device.
const MISSING: &str = "asset://01J00000000000000000000000";

/// The page's messages: the section's, and the analysis panel's.
#[derive(Debug, Clone, PartialEq)]
enum Host {
    Media(Message),
    Analysis,
}

/// The app's fonts (Inter, and the Lucide icons), so text measures as it
/// does in the app.
fn settings() -> iced::Settings {
    let mut settings = crate::app::settings();
    settings.fonts = font::FACES
        .iter()
        .copied()
        .chain([icons::FONT_BYTES])
        .map(Cow::Borrowed)
        .collect();
    settings
}

fn glyph(icon: Icon) -> String {
    char::from(icon).to_string()
}

/// The `n`-th text reading `label` (0 first), in tree order.
fn nth(label: &str, n: usize) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let label = label.to_string();
    let mut seen = 0;
    move |candidate: Candidate<'_>| {
        let Candidate::Text { content, .. } = &candidate else {
            return None;
        };
        if *content != label {
            return None;
        }
        seen += 1;
        (seen == n + 1).then(|| Target::from(candidate))
    }
}

fn example(id: &str, path: &str, label: &str) -> TechniqueMedia {
    TechniqueMedia {
        id: id.to_string(),
        path: path.to_string(),
        kind: TechniqueMedia::kind_for_path(path),
        label: label.to_string(),
        added_at: DateTime::local_ymd(2026, 6, 20),
    }
}

/// The two audio examples M0 and M1 of Dart's `_technique()`.
fn two() -> Vec<TechniqueMedia> {
    vec![
        example("M0", "/tmp/example-0.wav", "Example 0"),
        example("M1", "/tmp/example-1.wav", "Example 1"),
    ]
}

fn key(id: &str) -> ExampleKey {
    ExampleKey {
        id: id.to_string(),
        unavailable: false,
        video: false,
    }
}

/// How a test sets the section's data up.
type Setup = fn(&mut TechniqueMediaSection<'_>);

/// The section's defaults, as the edit view passes them.
fn edit(_: &mut TechniqueMediaSection<'_>) {}

/// The read view: play-only clips, export offered.
fn read(s: &mut TechniqueMediaSection<'_>) {
    s.editable = false;
    s.exportable = true;
}

/// The page around the section: its state, and its players over fakes.
struct Page {
    media: Vec<TechniqueMedia>,
    state: MediaSectionState,
    players: ExamplePlayers,
    factory: FakeMediaPlayers,
    /// Whether the page has an analysis panel to show.
    analysis: bool,
}

impl Page {
    fn new(media: Vec<TechniqueMedia>) -> Self {
        let mut page = Self {
            media: Vec::new(),
            state: MediaSectionState::new(),
            players: ExamplePlayers::new(),
            factory: FakeMediaPlayers::new(),
            analysis: false,
        };
        page.show(media);
        page
    }

    fn with_analysis(mut self) -> Self {
        self.analysis = true;
        self
    }

    /// The examples change (load, reload, edit draft): the players follow.
    fn show(&mut self, media: Vec<TechniqueMedia>) {
        self.media = media;
        sync_players(&mut self.players, &self.media, &self.factory);
    }

    fn section<'a>(
        &'a self,
        setup: &impl Fn(&mut TechniqueMediaSection<'a>),
    ) -> TechniqueMediaSection<'a> {
        let mut section = TechniqueMediaSection::new(&self.media);
        setup(&mut section);
        section
    }

    fn view<'a>(&'a self, setup: &impl Fn(&mut TechniqueMediaSection<'a>)) -> Element<'a, Host> {
        let analysis = self.analysis.then(|| {
            Box::new(|| button(text(ANALYSIS)).on_press(Host::Analysis).into())
                as AnalysisBuilder<'a, Host>
        });
        self.section(setup)
            .view(&self.state, &self.players, analysis, Host::Media)
    }

    fn ui<'a>(&'a self, setup: &impl Fn(&mut TechniqueMediaSection<'a>)) -> Simulator<'a, Host> {
        Simulator::with_size(settings(), SIZE, self.view(setup))
    }

    /// Hands `message` to the section, as the page's `update` does.
    fn update(
        &mut self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        message: Message,
    ) -> Option<Event> {
        let mut section = TechniqueMediaSection::new(&self.media);
        setup(&mut section);
        self.state
            .update(message, &section, &mut self.players, &self.factory)
    }

    /// Drives the view once, then feeds what the section reported back;
    /// returns the events (Dart's callbacks) that came of it.
    fn drive(
        &mut self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        steps: impl FnOnce(&mut Simulator<'_, Host>),
    ) -> Vec<Event> {
        let messages: Vec<Host> = {
            let mut ui = self.ui(setup);
            steps(&mut ui);
            ui.into_messages().collect()
        };
        messages
            .into_iter()
            .filter_map(|m| match m {
                Host::Media(m) => self.update(setup, m),
                Host::Analysis => None,
            })
            .collect()
    }

    fn click(
        &mut self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        label: &str,
    ) -> Vec<Event> {
        self.click_nth(setup, label, 0)
    }

    fn click_nth(
        &mut self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        label: &str,
        n: usize,
    ) -> Vec<Event> {
        self.drive(setup, |ui| {
            ui.click(nth(label, n))
                .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
        })
    }

    fn find(
        &self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        label: &str,
    ) -> Option<Rectangle> {
        self.find_nth(setup, label, 0)
    }

    fn find_nth(
        &self,
        setup: &impl Fn(&mut TechniqueMediaSection<'_>),
        label: &str,
        n: usize,
    ) -> Option<Rectangle> {
        self.ui(setup).find(nth(label, n)).ok().map(|t| t.bounds())
    }

    fn shows(&self, setup: &impl Fn(&mut TechniqueMediaSection<'_>), label: &str) -> bool {
        self.find(setup, label).is_some()
    }

    /// How many texts read `label`.
    fn count(&self, setup: &impl Fn(&mut TechniqueMediaSection<'_>), label: &str) -> usize {
        let mut ui = self.ui(setup);
        (0..)
            .take_while(|&n| ui.find(nth(label, n)).is_ok())
            .count()
    }

    fn built(&self) -> Vec<FakePlayerHandle> {
        self.factory.built()
    }
}

fn move_to(ui: &mut Simulator<'_, Host>, at: Point) {
    ui.point_at(at);
    let _ = ui.simulate([IcedEvent::Mouse(mouse::Event::CursorMoved { position: at })]);
}

/// A drop of `paths` at `at`, the way the platform reports one: one event
/// per file, each after a move to the pointer (macOS's event loop reads it
/// there, see `visible_drop_target`).
fn drop_at(ui: &mut Simulator<'_, Host>, at: Point, paths: &[&str]) {
    ui.point_at(at);
    let moved = || IcedEvent::Mouse(mouse::Event::CursorMoved { position: at });
    let mut events = Vec::new();
    for p in paths {
        events.push(moved());
        events.push(IcedEvent::Window(window::Event::FileHovered(
            PathBuf::from(p),
        )));
    }
    for p in paths {
        events.push(moved());
        events.push(IcedEvent::Window(window::Event::FileDropped(
            PathBuf::from(p),
        )));
    }
    events.push(IcedEvent::Window(window::Event::RedrawRequested(
        iced::time::Instant::now(),
    )));
    let _ = ui.simulate(events);
}

// ------------------------------------------------------------- the layout

// Dart: 'viewer with body + media lays out without overflow' and 'view mode
// with clips: Examples shown but play-only'.
#[test]
fn the_viewer_shows_each_example_with_its_player_and_no_add_zone() {
    let page = Page::new(vec![
        example("m1", "/tmp/a.m4a", ""),
        example("m2", "/tmp/b.mp4", ""),
    ]);
    assert!(page.shows(&read, "Examples"));
    assert!(page.shows(&read, "a.m4a"));
    assert!(page.shows(&read, "b.mp4"));
    assert!(page.shows(&read, &glyph(Icon::Music2)));
    assert!(page.shows(&read, &glyph(Icon::Video)));
    // The audio example's bar, and the video's poster (its player waits for
    // the first play).
    assert!(page.shows(&read, "0:00 / 0:00"));
    assert_eq!(page.built().len(), 1);
    // The block shows, but with no add/drop zone (read-only).
    assert!(!page.shows(&read, ADD_FIRST));
    assert!(!page.shows(&read, ADD_ANOTHER));
    assert!(!page.shows(&read, EMPTY));
    assert!(!page.shows(&read, RECORD));
    // Nor rename/remove.
    assert!(!page.shows(&read, &glyph(Icon::Pencil)));
    assert!(!page.shows(&read, &glyph(Icon::Trash2)));
}

// Dart: 'empty technique: view shows every field with placeholders'.
#[test]
fn an_empty_read_view_keeps_the_block_with_its_placeholder() {
    let page = Page::new(Vec::new());
    // The Examples label shows too, but with no add/drop zone.
    assert!(page.shows(&read, "Examples"));
    assert!(page.shows(&read, EMPTY));
    assert!(!page.shows(&read, ADD_FIRST));
}

// Dart: 'tapping Edit opens the form with Cancel/Save in header' — media is
// addable while editing too.
#[test]
fn editing_offers_the_add_zone_full_size_then_compact() {
    let recordable = |s: &mut TechniqueMediaSection<'_>| s.recordable = true;
    // From the zone's label to the record row under it: the label's half,
    // the zone's padding and border under it, then 8.
    let below_label = |page: &Page, label: &str| {
        let label = page.find(&recordable, label).expect("the zone");
        let record = page.find(&recordable, RECORD).expect("the record row");
        record.y - label.y
    };
    let page = Page::new(Vec::new());
    assert!(page.shows(&edit, "Examples"));
    assert!(page.shows(&edit, ADD_FIRST));
    assert!(!page.shows(&edit, EMPTY), "the zone, not the placeholder");
    let full = below_label(&page, ADD_FIRST);

    let page = Page::new(two());
    assert!(page.shows(&edit, ADD_ANOTHER));
    assert!(!page.shows(&edit, ADD_FIRST));
    assert!(
        page.find(&edit, ADD_ANOTHER).expect("the zone").y
            > page.find(&edit, "Example 1").expect("the card").y,
        "under the cards"
    );
    // Compact once clips exist: 7 of padding, not 22.
    let dense = below_label(&page, ADD_ANOTHER);
    assert!((full - dense - 15.0).abs() < 0.5, "{full} vs {dense}");
}

// ---------------------------------------------------------- the add zone

// Regression guard (Dart): the drop zone used to filter unsupported files
// out BEFORE calling the view model, so a drop of only unsupported files did
// nothing at all — no import, and the notice the VM already had for exactly
// this case was unreachable.
#[test]
fn a_drop_hands_over_every_path_even_an_unsupported_one() {
    let mut page = Page::new(Vec::new());
    let zone = page.find(&edit, ADD_FIRST).expect("the zone").center();
    let events = page.drive(&edit, |ui| drop_at(ui, zone, &["/tmp/notes.pdf"]));
    assert_eq!(
        events,
        vec![Event::AddPaths(vec!["/tmp/notes.pdf".to_string()])]
    );
    assert!(!page.state.dragging(), "the drop ended the drag");

    // A mixed drop: every path, the view model counts the skips.
    let events = page.drive(&edit, |ui| drop_at(ui, zone, &["/a.wav", "/b.pdf"]));
    assert_eq!(
        events,
        vec![Event::AddPaths(vec!["/a.wav".into(), "/b.pdf".into()])]
    );
}

#[test]
fn a_drag_over_the_zone_highlights_it() {
    let mut page = Page::new(Vec::new());
    let zone = page.find(&edit, ADD_FIRST).expect("the zone").center();
    // One drag that enters the zone, then leaves the window.
    let messages: Vec<Host> = {
        let mut ui = page.ui(&edit);
        ui.point_at(zone);
        let _ = ui.simulate([IcedEvent::Window(window::Event::FileHovered(
            PathBuf::from("/a.wav"),
        ))]);
        let _ = ui.simulate([IcedEvent::Window(window::Event::FilesHoveredLeft)]);
        ui.into_messages().collect()
    };
    assert_eq!(
        messages,
        vec![
            Host::Media(Message::DragHover(true)),
            Host::Media(Message::DragHover(false))
        ]
    );
    let mut messages = messages.into_iter();
    for _ in 0..2 {
        let Some(Host::Media(m)) = messages.next() else {
            unreachable!()
        };
        let over = m == Message::DragHover(true);
        assert_eq!(page.update(&edit, m), None);
        assert_eq!(page.state.dragging(), over);
    }
}

#[test]
fn a_tap_on_the_zone_browses_and_adds_what_was_picked() {
    let mut page = Page::new(Vec::new());
    assert_eq!(page.click(&edit, ADD_FIRST), vec![Event::Browse]);
    // `_browse`: nothing picked, nothing added.
    assert_eq!(page.update(&edit, Message::Picked(Vec::new())), None);
    assert_eq!(
        page.update(&edit, Message::Picked(vec!["/x.m4a".into()])),
        Some(Event::AddPaths(vec!["/x.m4a".into()]))
    );
}

// False while an import is in flight: ignore taps and drops so a second
// import can't be launched on top of the first (nor while a take records or
// saves, nor while the technique save commits).
#[test]
fn the_zone_is_inert_while_an_import_a_take_or_a_save_is_in_flight() {
    fn importing(s: &mut TechniqueMediaSection<'_>) {
        s.importing = true;
    }
    fn recording(s: &mut TechniqueMediaSection<'_>) {
        s.recordable = true;
        s.is_recording = true;
    }
    fn saving(s: &mut TechniqueMediaSection<'_>) {
        s.recordable = true;
        s.saving_recording = true;
    }
    fn frozen(s: &mut TechniqueMediaSection<'_>) {
        s.enabled = false;
    }
    let setups: [(&str, Setup); 4] = [
        ("importing", importing),
        ("recording", recording),
        ("saving", saving),
        ("frozen", frozen),
    ];
    for (what, setup) in setups {
        let mut page = Page::new(Vec::new());
        // Still there (the layout doesn't jump), just inert.
        let zone = page
            .find(&setup, ADD_FIRST)
            .unwrap_or_else(|| panic!("{what}: the zone shows"))
            .center();
        assert!(
            page.click(&setup, ADD_FIRST).is_empty(),
            "{what}: no browse"
        );
        let events = page.drive(&setup, |ui| drop_at(ui, zone, &["/a.wav"]));
        assert!(events.is_empty(), "{what}: no import");
        assert!(!page.state.dragging(), "{what}: no highlight");
        // A tap or drop already on its way lands on an inert zone too.
        assert_eq!(page.update(&setup, Message::Browse), None, "{what}");
        assert_eq!(
            page.update(&setup, Message::Dropped(vec!["/a.wav".into()])),
            None,
            "{what}"
        );
    }
}

// ------------------------------------------------------------- the cards

// Dart: 'an example can be exported, from the read view too' — a recorded
// example was reachable only by digging through the app's container, its
// file named by a hash.
#[test]
fn an_example_can_be_exported_from_the_read_view_too() {
    let mut page = Page::new(vec![example("m1", "/tmp/take.wav", "Recording 1")]);
    assert_eq!(page.count(&read, &glyph(Icon::Download)), 1);
    assert_eq!(
        page.click(&read, &glyph(Icon::Download)),
        vec![Event::Export("m1".into())]
    );
    // Not offered, not shown.
    let no_export = |s: &mut TechniqueMediaSection<'_>| s.editable = false;
    assert!(!page.shows(&no_export, &glyph(Icon::Download)));
    assert_eq!(page.update(&no_export, Message::Export("m1".into())), None);
}

#[test]
fn a_cards_actions_show_while_the_pointer_is_over_it() {
    let mut page = Page::new(two());
    let editing = |s: &mut TechniqueMediaSection<'_>| s.exportable = true;
    assert_eq!(page.count(&editing, &glyph(Icon::Download)), 2);
    assert_eq!(page.count(&editing, &glyph(Icon::Pencil)), 2);
    assert_eq!(page.count(&editing, &glyph(Icon::Trash2)), 2);
    let label = page.find(&editing, "Example 1").expect("M1's card");
    let below = page.find(&editing, ADD_ANOTHER).expect("the zone");

    let _ = page.drive(&editing, |ui| move_to(ui, label.center()));
    assert_eq!(page.state.hovered(), Some("M1"));
    let _ = page.drive(&editing, |ui| {
        move_to(ui, label.center());
        move_to(ui, below.center());
    });
    assert_eq!(page.state.hovered(), None);

    // A late exit from a card the pointer already left for another one
    // leaves the other one hovered.
    let _ = page.update(&editing, Message::Hover("M0".into(), true));
    let _ = page.update(&editing, Message::Hover("M1".into(), false));
    assert_eq!(page.state.hovered(), Some("M0"));
}

#[test]
fn remove_goes_to_the_page() {
    let mut page = Page::new(two());
    assert_eq!(
        page.click_nth(&edit, &glyph(Icon::Trash2), 1),
        vec![Event::Remove("M1".into())]
    );
    // Nothing to remove in the read view.
    assert_eq!(page.update(&read, Message::Remove("M1".into())), None);
}

// Dart: 'Save and Cancel are disabled while the save is in flight' (the
// section's part): a frozen edit surface hides the hover rename/remove
// icons too — the clip stays playable — and keeps the add zone and the
// record row visible, just inert.
#[test]
fn a_frozen_section_keeps_playback_and_export_but_takes_no_edit() {
    fn frozen(s: &mut TechniqueMediaSection<'_>) {
        s.enabled = false;
        s.exportable = true;
        s.recordable = true;
    }
    fn frozen_recording(s: &mut TechniqueMediaSection<'_>) {
        frozen(s);
        s.is_recording = true;
    }
    let mut page = Page::new(two());
    assert!(!page.shows(&frozen, &glyph(Icon::Pencil)));
    assert!(!page.shows(&frozen, &glyph(Icon::Trash2)));
    assert_eq!(page.count(&frozen, &glyph(Icon::Download)), 2);
    assert!(page.shows(&frozen, ADD_ANOTHER));
    assert!(page.shows(&frozen, RECORD));
    assert!(page.click(&frozen, RECORD).is_empty(), "Record is inert");
    assert!(page.click(&frozen_recording, STOP).is_empty(), "Stop too");
    for message in [
        Message::Rename {
            id: "M0".into(),
            current: "Example 0".into(),
        },
        Message::Remove("M0".into()),
        Message::StartRecord,
    ] {
        assert_eq!(page.update(&frozen, message.clone()), None, "{message:?}");
    }
    assert_eq!(page.update(&frozen_recording, Message::StopRecord), None);

    // Playback stays alive.
    let _ = page.click(&frozen, "Play");
    assert_eq!(page.built()[0].plays(), 1);
}

// Dart: `_promptLabel` — the trimmed label, or nothing if cancelled; an
// empty value clears back to the file name.
#[test]
fn rename_asks_for_a_caption_and_an_empty_one_clears_it() {
    let mut page = Page::new(two());
    let events = page.click(&edit, &glyph(Icon::Pencil));
    assert_eq!(
        events,
        vec![Event::Rename {
            id: "M0".into(),
            current: "Example 0".into(),
        }]
    );

    // What the page shows, and how the answer comes back.
    let answer = |page: &mut Page, typed: Option<&str>| {
        let (dialog, on_result) = rename_dialog("M0".into(), "Example 0");
        let mut state = DialogState::new(dialog);
        let step = match typed {
            Some(t) => {
                assert_eq!(
                    state.step(confirm_dialog::Message::Input(t.into())),
                    DialogStep::Stay
                );
                state.step(confirm_dialog::Message::Confirm)
            }
            None => state.step(confirm_dialog::Message::Cancel),
        };
        let DialogStep::Closed(result) = step else {
            panic!("the dialog closed: {step:?}");
        };
        page.update(&edit, on_result(result))
    };
    assert_eq!(
        answer(&mut page, Some("  Twang demo ")),
        Some(Event::Renamed {
            id: "M0".into(),
            label: "Twang demo".into(),
        })
    );
    assert_eq!(
        answer(&mut page, Some("   ")),
        Some(Event::Renamed {
            id: "M0".into(),
            label: String::new(),
        }),
        "an empty caption clears it"
    );
    assert_eq!(answer(&mut page, None), None, "cancelled");
}

#[test]
fn the_rename_dialog_shows_the_caption_and_saves_or_cancels() {
    let drive = |dialog: &RenameExampleDialog,
                 steps: &dyn Fn(&mut Simulator<'_, confirm_dialog::Message>)| {
        let mut ui = Simulator::with_size(settings(), SIZE, dialog.view());
        steps(&mut ui);
        ui.into_messages().collect::<Vec<_>>()
    };
    let mut dialog = RenameExampleDialog::new("Example 0");
    let messages = drive(&dialog, &|ui| {
        // The field draws its own 28 px well, under the 18 px title, 14,
        // its 16 px label and 6, across the card's content.
        use crate::ui::core::widgets::confirm_dialog::{CARD_PADDING, CARD_WIDTH};
        let title = ui.find("Rename example").expect("the title").bounds();
        let field = ui.find("Example 0").expect("the field").bounds();
        assert_eq!(field.x, title.x, "{field:?}");
        assert_eq!(field.y, title.y + 18.0 + 14.0 + 16.0 + 6.0, "{field:?}");
        assert_eq!(field.width, CARD_WIDTH - 2.0 * CARD_PADDING, "{field:?}");
        assert_eq!(field.height, theme::FIELD_HEIGHT, "{field:?}");
        assert!(ui.find("Caption").is_ok(), "the field's label");
        ui.click("Example 0").expect("the field holds the caption");
        let _ = ui.tap_key(iced::keyboard::Key::Named(iced::keyboard::key::Named::End));
        let _ = ui.typewrite("!");
        let _ = ui.tap_key(iced::keyboard::Key::Named(
            iced::keyboard::key::Named::Enter,
        ));
    });
    assert_eq!(
        messages,
        vec![
            confirm_dialog::Message::Input("Example 0!".into()),
            confirm_dialog::Message::Confirm
        ]
    );
    assert_eq!(
        dialog.update(confirm_dialog::Message::Input("Example 0!".into())),
        None
    );
    assert_eq!(dialog.text(), "Example 0!");
    assert_eq!(
        dialog.update(confirm_dialog::Message::Confirm),
        Some(DialogResult::Text(Some("Example 0!".into())))
    );

    // Save, Cancel, Escape and the barrier.
    let messages = drive(&dialog, &|ui| {
        ui.click("Save").expect("Save");
        ui.click("Cancel").expect("Cancel");
        let _ = ui.tap_key(iced::keyboard::Key::Named(
            iced::keyboard::key::Named::Escape,
        ));
        ui.point_at(Point::new(5.0, 5.0));
        let _ = ui.simulate(iced_test::simulator::click());
    });
    assert_eq!(
        messages,
        vec![
            confirm_dialog::Message::Confirm,
            confirm_dialog::Message::Cancel,
            confirm_dialog::Message::Cancel,
            confirm_dialog::Message::Cancel,
        ]
    );
    assert_eq!(
        dialog.update(confirm_dialog::Message::Cancel),
        Some(DialogResult::Text(None))
    );
}

// Dart: 'an uncaptioned example named by the app reads by its kind, never a
// hash', and the "Recording N" captions the view model gives recorded
// examples: a card reads its example's display label.
#[test]
fn a_card_reads_its_examples_caption_or_a_name_that_says_something() {
    let hash = format!("9f64a747{}806a", "0".repeat(52));
    let page = Page::new(vec![
        example("r1", "/t/take-1.wav", "Recording 1"),
        example("r2", "/t/take-2.wav", "Recording 2"),
        example("r3", "/t/take-3.wav", "Recording 3"),
        example("h", &format!("/pool/{hash}.wav"), ""),
        example("v", &format!("/pool/{hash}.mp4"), ""),
        example("u", "/t/01M5Q7CW5YY1DCXTX5ZY7TPT68.wav", ""),
        example("n", "/t/My Demo.wav", ""),
        example("c", &format!("/pool/{hash}.m4a"), "  Twang  "),
    ]);
    for label in [
        "Recording 1",
        "Recording 2",
        "Recording 3",
        "Video example",
        "My Demo.wav",
        "Twang",
    ] {
        assert!(page.shows(&read, label), "{label}");
    }
    assert_eq!(page.count(&read, "Audio example"), 2);
    assert!(!page.shows(&read, &format!("{hash}.wav")));
}

// --------------------------------------------------- expansion and analysis

// Dart: 'two byte-identical examples open and select independently'.
// Selection and expansion were keyed by path, so both cards opened with the
// analysis panel and a playhead-linked player, and the second example could
// never be selected on its own.
#[test]
fn byte_identical_examples_open_and_select_independently() {
    let pooled = "/pool/9f64a747.wav";
    let mut page = Page::new(vec![
        example("A", pooled, "Take 0"),
        example("B", pooled, "Take 1"),
    ])
    .with_analysis();
    let first = |s: &mut TechniqueMediaSection<'_>| {
        s.expandable = true;
        s.expanded_id = Some("A");
        s.analyzed_id = Some("A");
    };
    assert_eq!(
        page.count(&first, ANALYSIS),
        1,
        "only the selected example shows its analysis"
    );
    assert_eq!(page.count(&first, &glyph(Icon::ChevronUp)), 1, "A is open");
    assert_eq!(page.count(&first, &glyph(Icon::ChevronDown)), 1);

    // Expanding the second card selects IT, not the first sharing its file.
    assert_eq!(
        page.click(&first, &glyph(Icon::ChevronDown)),
        vec![Event::ToggleExpand("B".into())]
    );
    let second = |s: &mut TechniqueMediaSection<'_>| {
        s.expandable = true;
        s.expanded_id = Some("B");
        s.analyzed_id = Some("B");
    };
    assert_eq!(page.count(&second, ANALYSIS), 1);
    let analysis = page.find(&second, ANALYSIS).expect("the analysis").y;
    let take1 = page.find(&second, "Take 1").expect("B's card").y;
    assert!(analysis > take1, "in B's card");

    // Only B's player drives the playhead.
    let built = page.built();
    for (id, handle) in ["A", "B"].into_iter().zip(&built) {
        handle.set_state(|s| s.duration = Duration::from_secs(10));
        let _ = page.update(
            &second,
            Message::Player(example_players::Message::Audio(
                key(id),
                audio::Message::PlayOrPause,
            )),
        );
    }
    built[0].set_state(|s| s.position = Duration::from_secs(1));
    built[1].set_state(|s| s.position = Duration::from_secs(2));
    assert_eq!(
        page.update(&second, Message::Player(example_players::Message::Tick)),
        Some(Event::Position(Duration::from_secs(2)))
    );
}

#[test]
fn only_an_audio_card_with_an_analysis_to_show_toggles_it() {
    let mut page = Page::new(vec![
        example("a", "/tmp/a.wav", "Audio"),
        example("v", "/tmp/v.mp4", "Video"),
    ])
    .with_analysis();
    let expandable = |s: &mut TechniqueMediaSection<'_>| s.expandable = true;
    // The name area toggles the analysis (when expandable).
    assert_eq!(
        page.click(&expandable, "Audio"),
        vec![Event::ToggleExpand("a".into())]
    );
    assert_eq!(
        page.count(&expandable, &glyph(Icon::ChevronDown)),
        1,
        "audio only"
    );
    assert!(page.click(&expandable, "Video").is_empty());
    assert_eq!(
        page.update(&expandable, Message::ToggleExpand("v".into())),
        None
    );

    // No toggle offered: no chevron, the name is inert.
    assert!(!page.shows(&edit, &glyph(Icon::ChevronDown)));
    assert!(page.click(&edit, "Audio").is_empty());

    // No analysis in this context: nothing to expand.
    let mut page = Page::new(vec![example("a", "/tmp/a.wav", "Audio")]);
    assert!(!page.shows(&expandable, &glyph(Icon::ChevronDown)));
    assert!(page.click(&expandable, "Audio").is_empty());
}

// While recording an example the live preview shows under the record
// control instead, so the card's static analysis is hidden then; a read
// view's card keeps its own (a practice take's preview is the analysis'
// own).
#[test]
fn while_an_example_records_its_live_preview_shows_under_the_record_control() {
    let page = Page::new(two()).with_analysis();
    let recording = |s: &mut TechniqueMediaSection<'_>| {
        s.expandable = true;
        s.expanded_id = Some("M0");
        s.recordable = true;
        s.is_recording = true;
        s.live_monitor = true;
    };
    assert_eq!(page.count(&recording, ANALYSIS), 1);
    let analysis = page.find(&recording, ANALYSIS).expect("the preview").y;
    let stop = page.find(&recording, STOP).expect("Stop").y;
    assert!(analysis > stop, "under the record control");

    let idle = |s: &mut TechniqueMediaSection<'_>| {
        s.expandable = true;
        s.expanded_id = Some("M0");
        s.recordable = true;
    };
    let analysis = page.find(&idle, ANALYSIS).expect("the card's analysis").y;
    let second = page.find(&idle, "Example 1").expect("M1's card").y;
    assert!(analysis < second, "inside M0's card");

    let practising = |s: &mut TechniqueMediaSection<'_>| {
        s.editable = false;
        s.expandable = true;
        s.expanded_id = Some("M0");
        s.live_monitor = true;
    };
    assert_eq!(page.count(&practising, ANALYSIS), 1);
    let analysis = page.find(&practising, ANALYSIS).expect("the card's").y;
    assert!(analysis < page.find(&practising, "Example 1").expect("M1").y);

    // Recording with nothing expanded: the preview alone.
    let nothing_open = |s: &mut TechniqueMediaSection<'_>| {
        s.recordable = true;
        s.is_recording = true;
        s.live_monitor = true;
    };
    assert_eq!(page.count(&nothing_open, ANALYSIS), 1);
}

// ------------------------------------------------------------ the record row

#[test]
fn the_record_row_follows_the_take() {
    let mut page = Page::new(two());
    let idle = |s: &mut TechniqueMediaSection<'_>| s.recordable = true;
    assert_eq!(page.click(&idle, RECORD), vec![Event::StartRecord]);

    // e.g. while a drag-drop import is still copying.
    let busy = |s: &mut TechniqueMediaSection<'_>| {
        s.recordable = true;
        s.can_record = false;
    };
    assert!(page.shows(&busy, RECORD));
    assert!(page.click(&busy, RECORD).is_empty());

    // A pending start is busy too, but nothing is recorded yet.
    let starting = |s: &mut TechniqueMediaSection<'_>| {
        s.recordable = true;
        s.saving_recording = true;
        s.starting_recording = true;
    };
    assert!(page.shows(&starting, STARTING));
    assert!(!page.shows(&starting, RECORD));
    let saving = |s: &mut TechniqueMediaSection<'_>| {
        s.recordable = true;
        s.saving_recording = true;
    };
    assert!(page.shows(&saving, SAVING));
    assert_eq!(page.update(&saving, Message::StartRecord), None);

    let recording = |s: &mut TechniqueMediaSection<'_>| {
        s.recordable = true;
        s.is_recording = true;
        s.record_level = 0.5;
    };
    assert!(!page.shows(&recording, RECORD));
    assert_eq!(page.click(&recording, STOP), vec![Event::StopRecord]);
    assert_eq!(page.update(&recording, Message::StartRecord), None);
    assert_eq!(page.update(&idle, Message::StopRecord), None);

    // No recorder offered (or the read view): no row.
    assert!(!page.shows(&edit, RECORD));
    assert!(!page.shows(
        &|s: &mut TechniqueMediaSection<'_>| {
            s.recordable = true;
            s.editable = false;
        },
        RECORD
    ));
}

#[test]
fn the_meter_fills_to_the_level() {
    assert_eq!(meter_fill(0.5, 200.0), 100.0);
    assert_eq!(meter_fill(0.0, 200.0), 0.0);
    assert_eq!(meter_fill(-1.0, 200.0), 0.0);
    assert_eq!(meter_fill(1.7, 200.0), 200.0);
    assert_eq!(meter_fill(f32::NAN, 200.0), 0.0);
}

// ------------------------------------------------------------- the players

// Dart: 'starting a practice take pauses every example player' — the
// examples play outside the engine, so its echo canceller has no reference
// to subtract: a clip left playing on speakers was recorded into the take
// it was compared with.
#[test]
fn a_take_pauses_every_example_and_blocks_their_play() {
    let mut page = Page::new(two());
    assert_eq!(page.built().len(), 2);
    page.players.pause_all(); // micArmed
    assert!(page.built().iter().all(|p| p.pauses() > 0));

    // Nor can one be started again until the take ends: paused only at the
    // arm, an example played on into the take.
    let live = |s: &mut TechniqueMediaSection<'_>| s.mic_live = true;
    assert!(page.click(&live, "Play").is_empty());
    let _ = page.update(
        &live,
        Message::Player(example_players::Message::Audio(
            key("M0"),
            audio::Message::PlayOrPause,
        )),
    );
    assert!(page.built().iter().all(|p| p.plays() == 0));

    // Once it ends, they play.
    let _ = page.click(&edit, "Play");
    assert_eq!(page.built()[0].plays(), 1);
}

// Dart: 'removing an earlier example keeps a later card's player'.
#[test]
fn removing_an_earlier_example_keeps_the_later_cards_player() {
    let mut page = Page::new(two());
    let _ = page.click_nth(&edit, "Play", 1);
    assert!(page.built()[1].plays() > 0, "M1 plays");

    page.show(vec![example("M1", "/tmp/example-1.wav", "Example 1")]);

    let built = page.built();
    assert_eq!(built.len(), 2, "M1 was not rebuilt");
    assert!(built[0].disposed(), "only M0");
    assert!(!built[1].disposed());
    assert!(!page.shows(&edit, "Example 0"));
    assert!(page.shows(&edit, "Example 1"));
    match page.players.get("M1") {
        Some(ExamplePlayer::Audio(p)) => assert!(p.is_playing(), "still playing"),
        other => panic!("M1's player: {other:?}"),
    }
}

// Dart: 'scrolling the examples out of view keeps their players (playback
// does not stop or reset)'.
#[test]
fn scrolling_the_examples_out_of_view_keeps_their_players() {
    let page = Page::new(two());
    page.players.get("M1").expect("M1's player");
    {
        let content =
            scrollable(column![page.view(&read), Space::new().height(6000)]).height(Length::Fill);
        let mut ui = Simulator::with_size(settings(), SIZE, content);
        ui.point_at(Point::new(500.0, 300.0));
        let wheel = |y| {
            IcedEvent::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y },
            })
        };
        let _ = ui.simulate([wheel(-6000.0), wheel(6000.0)]);
    }
    let built = page.built();
    assert_eq!(built.len(), 2, "no fresh player");
    assert!(!built.iter().any(FakePlayerHandle::disposed));
}

#[test]
fn a_clip_whose_file_is_missing_reads_unavailable_in_its_card() {
    let mut page = Page::new(vec![
        example("M0", "/tmp/example-0.wav", "Example 0"),
        example("M1", MISSING, "Example 1"),
    ]);
    assert!(page.shows(&read, UNAVAILABLE_CLIP_MESSAGE));
    assert_eq!(page.built().len(), 1);
    // Its bytes arrive in a reload: a player.
    page.show(two());
    assert!(!page.shows(&read, UNAVAILABLE_CLIP_MESSAGE));
    assert_eq!(page.built().len(), 2);
}

#[test]
fn a_video_card_plays_through_the_section() {
    let mut page = Page::new(vec![example("V", "/tmp/clip.mp4", "Clip")]);
    assert!(page.built().is_empty(), "the poster until the first play");
    let _ = page.click(&read, "Play");
    assert_eq!(page.built().len(), 1);
    assert!(page.players.needs_tick(), "playing: polled");
    // The poll reaches the video through the section.
    assert_eq!(
        page.update(&read, Message::Player(example_players::Message::Tick)),
        None,
        "a video drives no analysis"
    );
}

// ---------------------------------------------------------------- animations

mod animations {
    use iced::time::Instant;
    use iced::Point;

    use super::*;
    use crate::ui::core::motion::Lerp;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, BACKGROUND, MS};

    #[test]
    fn the_hover_icons_fade_in_and_out_over_120_ms() {
        let view = |hovered| hover_actions(hovered, true, true, "M0".into(), "Example 0".into());
        let mut window = Frames::new(Size::new(80.0, 20.0));
        let ink = |w: &Frames| w.ink(Point::ORIGIN, Point::new(80.0, 20.0));
        let t0 = Instant::now();
        window.frame(view(false), &[], t0);
        assert_eq!(ink(&window), 0, "hidden at rest");
        assert!(window.frame(view(true), &[], t0));
        assert_eq!(ink(&window), 0, "start");
        window.frame(view(true), &[], t0 + 60 * MS);
        let half = ink(&window);
        assert!(!window.frame(view(true), &[], t0 + 120 * MS));
        let full = ink(&window);
        assert!(full > 40, "shown: {full}");
        assert!(half.abs_diff(full / 2) <= 3, "middle: {half} of {full}");
        // And out again.
        window.frame(view(false), &[], t0 + 200 * MS);
        window.frame(view(false), &[], t0 + 260 * MS);
        assert!(ink(&window).abs_diff(full / 2) <= 3);
        window.frame(view(false), &[], t0 + 320 * MS);
        assert_eq!(ink(&window), 0, "end");
    }

    #[test]
    fn the_chevron_turns_half_a_turn_over_150_ms() {
        let view = |expanded| expand_chevron(expanded, Message::ToggleExpand("M0".into()));
        let mut window = Frames::new(Size::new(24.0, 24.0));
        // A quarter turn puts an arm's end up at the glyph's top right,
        // where neither chevron at rest has ink.
        let turned = |w: &Frames| w.ink(Point::new(12.0, 4.0), Point::new(17.0, 9.0)) > 40;
        let t0 = Instant::now();
        window.frame(view(false), &[], t0);
        let down = window.ink(Point::ORIGIN, Point::new(24.0, 24.0));
        assert!(down > 0 && !turned(&window), "the down chevron");
        assert!(window.frame(view(true), &[], t0));
        assert!(!turned(&window), "start");
        assert!(window.frame(view(true), &[], t0 + 75 * MS));
        assert!(turned(&window), "a quarter turn");
        assert!(!window.frame(view(true), &[], t0 + 150 * MS));
        assert!(!turned(&window), "end: the up chevron");
    }

    #[test]
    fn the_zone_lights_up_for_a_drag_over_120_ms() {
        let page = Page::new(two());
        let section = page.section(&edit);
        let mut state = MediaSectionState::new();
        let mut window = Frames::new(Size::new(300.0, 60.0));
        // Inside the 1 px border, clear of the label.
        let fill = Point::new(4.0, 20.0);
        let (rest, lit) = (Studio::WELL, alpha(Studio::ACCENT, 0.08));
        let t0 = Instant::now();
        window.frame(section.add_zone(&state), &[], t0);
        assert!(near(window.pixel(fill), over(rest, BACKGROUND)));
        state.dragging = true;
        assert!(window.frame(section.add_zone(&state), &[], t0));
        assert!(near(window.pixel(fill), over(rest, BACKGROUND)), "start");
        window.frame(section.add_zone(&state), &[], t0 + 60 * MS);
        // `Color.lerp`, alpha and all.
        let middle = over(rest.lerp(lit, 0.5), BACKGROUND);
        assert!(near(window.pixel(fill), middle), "middle");
        assert!(!window.frame(section.add_zone(&state), &[], t0 + 120 * MS));
        assert!(near(window.pixel(fill), over(lit, BACKGROUND)), "end");
    }
}
