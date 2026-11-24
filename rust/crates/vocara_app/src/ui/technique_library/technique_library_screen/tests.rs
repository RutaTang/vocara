//! The library page on screen: its states, the header, the tag menu, the
//! cards, and the create / delete / open flows (`technique_library_layout_test.dart`,
//! `technique_dialogs_test.dart`'s flow through the page).
//!
//! The page runs headless over the real repositories (an in-memory library)
//! or, where the Dart tests faked the repository, over a fake. Its view
//! model's effects run here, in order, when a test says so — a delete's
//! media cleanup can be held, as the Dart test parked it on a completer.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use iced::keyboard::{self, key::Named};
use iced::{mouse, Point, Rectangle, Size};
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::util::id::new_id;

use super::*;
use crate::app::library::Library;
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult};
use crate::ui::technique_library::view_model::technique_library_view_model::tests::{
    remote, technique, FakeMedia, FakeRepo,
};
use crate::ui::technique_library::view_model::technique_library_view_model::MediaCleanup;
use crate::ui::DialogRequest;

const WIDE: Size = Size::new(1440.0, 900.0);

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

/// The `n`-th text reading `label` (0 first), in tree order: the page
/// first, then an open menu.
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

/// The page, its services, and what it asked for.
struct Harness {
    screen: TechniqueLibraryScreen,
    services: Services,
    size: Size,
    /// Effects started and not yet run, in order.
    pending: VecDeque<Effect>,
    /// Media cleanups whose rows are gone, not yet run.
    cleanups: VecDeque<MediaCleanup>,
    navigations: Vec<Navigation>,
    dialog: Option<DialogRequest<Message>>,
}

impl Harness {
    fn new(services: Services) -> Self {
        let (screen, load) = TechniqueLibraryScreen::open(&services);
        Self {
            screen,
            services,
            size: WIDE,
            pending: load.into_iter().collect(),
            cleanups: VecDeque::new(),
            navigations: Vec::new(),
            dialog: None,
        }
    }

    /// Over the in-memory library holding `techniques`.
    fn library(techniques: Vec<Technique>) -> Self {
        let services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        for t in &techniques {
            services.technique_repo.save_technique(t).expect("saved");
        }
        Self::new(services)
    }

    /// Over a fake repository (and fake media files).
    fn fake(repo: &Arc<FakeRepo>) -> Self {
        let mut services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        services.technique_repo = repo.clone();
        services.technique_media = Arc::new(FakeMedia::default());
        Self::new(services)
    }

    fn send(&mut self, message: Message) {
        let step = self.screen.handle(message);
        self.absorb(step);
    }

    fn absorb(&mut self, step: Step) {
        self.pending.extend(step.effects);
        if let Some(navigation) = step.update.navigation {
            self.navigations.push(navigation);
        }
        if let Some(dialog) = step.update.dialog {
            self.dialog = Some(dialog);
        }
    }

    /// Runs the pending effects (not the cleanups they lead to), landing
    /// what each reports, until none is pending.
    fn run_effects(&mut self) {
        while let Some(effect) = self.pending.pop_front() {
            let (done, cleanup) = effect.run();
            self.cleanups.extend(cleanup);
            self.send(Message::Landed(done));
        }
    }

    /// Runs everything, cleanups included, until nothing is left.
    fn settle(&mut self) {
        loop {
            self.run_effects();
            let Some(cleanup) = self.cleanups.pop_front() else {
                return;
            };
            self.send(Message::Landed(cleanup.run()));
        }
    }

    /// Answers the open dialog.
    fn answer(&mut self, result: DialogResult) {
        let dialog = self.dialog.take().expect("a dialog is open");
        self.send((dialog.on_result)(result));
    }

    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(settings(), self.size, self.screen.view())
    }

    /// Drives the page's view once and feeds back what it reported.
    fn drive(&mut self, steps: impl FnOnce(&mut Simulator<'_, Message>)) -> Vec<Message> {
        let messages: Vec<Message> = {
            let mut ui = self.ui();
            steps(&mut ui);
            ui.into_messages().collect()
        };
        for m in messages.clone() {
            self.send(m);
        }
        messages
    }

    fn click(&mut self, label: &str) -> Vec<Message> {
        self.click_nth(label, 0)
    }

    fn click_nth(&mut self, label: &str, n: usize) -> Vec<Message> {
        self.drive(|ui| {
            ui.click(nth(label, n))
                .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
        })
    }

    fn find(&self, label: &str) -> Option<Rectangle> {
        self.ui().find(nth(label, 0)).ok().map(|t| t.bounds())
    }

    fn shows(&self, label: &str) -> bool {
        self.find(label).is_some()
    }

    /// How many texts read `label` (a card shows its texts twice: at rest,
    /// and the face it shows while hovered).
    fn count(&self, label: &str) -> usize {
        let mut ui = self.ui();
        (0..)
            .take_while(|&n| ui.find(nth(label, n)).is_ok())
            .count()
    }

    fn names(&self) -> Vec<String> {
        self.screen
            .vm
            .filtered()
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    fn id_of(&self, name: &str) -> String {
        let all = self.screen.vm.all();
        all.iter()
            .find(|t| t.name == name)
            .map(|t| t.id.clone())
            .unwrap_or_else(|| panic!("{name} is listed"))
    }

    /// The trash of the card for `name` (shown on the hovered face).
    fn delete_card(&mut self, name: &str) {
        let index = self
            .screen
            .vm
            .filtered()
            .iter()
            .position(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} is on the grid"));
        let messages = self.click_nth(&glyph(Icon::Trash2), index);
        assert_eq!(
            messages,
            vec![Message::Delete {
                id: self.id_of(name),
                name: name.to_string()
            }]
        );
    }
}

/// A technique with a ULID, for the real repositories.
fn stored(name: &str, tags: &[&str]) -> Technique {
    technique(&new_id(), name, tags)
}

/// The messages `task` produces, run on the runtime.
async fn messages_of(task: Task<Message>) -> Vec<Message> {
    use iced::futures::StreamExt;
    use iced_test::runtime::{task::into_stream, Action};
    let Some(stream) = into_stream(task) else {
        return Vec::new();
    };
    stream
        .filter_map(|action| async move {
            match action {
                Action::Output(message) => Some(message),
                _ => None,
            }
        })
        .collect()
        .await
}

#[tokio::test]
async fn an_effect_runs_off_the_ui_thread_a_delete_reporting_its_rows_then_its_media() {
    let repo = FakeRepo::with(vec![technique("a", "A", &[])]);
    let media = Arc::new(FakeMedia::default());
    let (mut vm, _) = TechniqueLibraryViewModel::new(repo.clone(), media.clone());
    let delete = vm.execute_delete("a").expect("a delete");
    assert_eq!(
        messages_of(task(delete)).await,
        vec![
            Message::Landed(Done::Deleted {
                id: "a".into(),
                outcome: Ok(())
            }),
            Message::Landed(Done::MediaCleaned { id: "a".into() }),
        ]
    );
    assert!(repo.ids().is_empty());
    assert_eq!(*media.cleanups.lock().unwrap(), ["a"]);

    // A failed delete reports its rows only.
    repo.set(vec![technique("b", "B", &[])]);
    repo.refuse_writes.store(true, Ordering::SeqCst);
    let mut vm = TechniqueLibraryViewModel::new(repo.clone(), media.clone()).0;
    let delete = vm.execute_delete("b").expect("a delete");
    let messages = messages_of(task(delete)).await;
    assert!(matches!(
        messages.as_slice(),
        [Message::Landed(Done::Deleted {
            outcome: Err(_),
            ..
        })]
    ));
}

#[tokio::test]
async fn the_page_opens_with_its_first_load_running() {
    let repo = FakeRepo::with(vec![technique("a", "Twang", &[])]);
    let h = Harness::fake(&repo);
    let (mut screen, opening) = TechniqueLibraryScreen::new(&h.services);
    let messages = messages_of(opening).await;
    assert!(matches!(
        messages.as_slice(),
        [Message::Landed(Done::Loaded { outcome: Ok(_), .. })]
    ));
    for m in messages {
        let _ = screen.update(m, &h.services);
    }
    assert_eq!(screen.view_model().all().len(), 1);
}

#[test]
fn the_page_loads_then_shows_the_grid() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"])]);
    assert!(h.screen.vm.load().running());
    assert!(!h.shows("Twang"), "a spinner while the first load runs");
    h.settle();
    assert!(h.shows("Twang"));
    assert!(h.shows("EVT"));
    assert!(h.shows("A summary for Twang that is reasonably long to fill the card."));
}

#[test]
fn a_cards_border_turns_coral_over_120_ms_under_the_pointer() {
    use crate::ui::core::motion::Lerp;
    use crate::ui::core::widgets::animated::testing::{near, over, Frames, MS};
    use iced::time::Instant;

    let twang = stored("Twang", &[]);
    let vm = Harness::library(vec![twang.clone()]).screen.vm;
    let mut window = Frames::new(Size::new(280.0, 150.0));
    let view =
        || -> Element<'_, Message> { container(card(&twang, &vm)).width(280).height(150).into() };
    // The left border, halfway down, over the card's face.
    let edge = Point::new(0.0, 75.0);
    let (rest, lit) = (Studio::BORDER, alpha(Studio::ACCENT, 0.6));
    let shows = |w: &Frames, border: Color| near(w.pixel(edge), over(border, Studio::PANEL));
    let t0 = Instant::now();
    window.frame(view(), &[], t0);
    assert!(shows(&window, rest));
    window.point_at(Point::new(140.0, 75.0));
    assert!(window.frame(view(), &[], t0));
    assert!(shows(&window, rest), "start");
    window.frame(view(), &[], t0 + 60 * MS);
    assert!(
        shows(&window, rest.lerp(lit, 0.5)),
        "middle: {:?}",
        window.pixel(edge)
    );
    assert!(!window.frame(view(), &[], t0 + 120 * MS));
    assert!(shows(&window, lit), "end");
}

#[test]
fn the_grid_keeps_cards_at_most_300_wide() {
    // 1392 px of content: five columns of 265.6.
    let (columns, width) = grid_columns(1440.0);
    assert_eq!(columns, 5);
    assert!((width - 265.6).abs() < 1e-3);
    // 1152 px: four of 276.
    assert_eq!(grid_columns(1200.0), (4, 276.0));
    // Narrower than one card still gets one column.
    assert_eq!(grid_columns(100.0), (1, 52.0));
}

// The card's tags wrap in rows, but a tag too long for one row wrapped
// inside its chip as well, and three of them pushed the fixed-height card
// past its cell.
#[test]
fn long_tags_stay_on_one_line_inside_their_card() {
    for size in [Size::new(1200.0, 800.0), WIDE] {
        let long: Vec<String> = (0..3)
            .map(|i| format!("a tag long enough to wrap onto a second line in its chip {i}"))
            .collect();
        let tags: Vec<&str> = long.iter().map(String::as_str).collect();
        let mut h = Harness::library(vec![stored("Twang", &tags), stored("Belt", &tags)]);
        h.size = size;
        h.settle();
        // The first row of cards: under the 56 px header and the grid's
        // 24 px margin, 168 tall.
        let card_bottom = 56.0 + GRID_PADDING + CARD_HEIGHT;
        for tag in &long {
            let bounds = h.find(tag).unwrap_or_else(|| panic!("{tag} shown"));
            assert!(bounds.height <= 10.5 * 1.3 + 0.5, "{tag}: one line");
            assert!(
                bounds.y + bounds.height <= card_bottom - 1.0,
                "{tag} at {bounds:?} stays inside its card ({size:?})"
            );
        }
    }
}

#[test]
fn a_tapped_chip_filters_the_grid_and_a_hovered_card_offers_its_trash() {
    // A large window like the real app, where cards are full width.
    let mut techniques = vec![
        stored("Twang", &["EVT"]),
        stored("Curbing", &["CVT"]),
        stored("Mixed voice", &[]),
    ];
    for i in 4..24 {
        techniques.push(stored(
            &format!("Technique number {i} with a long name"),
            &["EVT"],
        ));
    }
    let mut h = Harness::library(techniques);
    h.settle();
    assert_eq!(h.screen.vm.filtered().len(), 23);

    let messages = h.click("EVT");
    assert_eq!(messages, vec![Message::TapTag("EVT".into())]);
    assert_eq!(h.screen.vm.active_tag(), Some("EVT"));
    assert!(!h.shows("Curbing") && !h.shows("Mixed voice"));
    // 'Twang' carries the EVT tag, so it survives the filter.
    assert!(h.shows("Twang"));
    // Its tag shows as the active one, and in the filter button.
    // 21 cards carry it (each card draws its face twice: at rest and
    // hovered), and the filter button names it.
    assert_eq!(h.count("EVT"), 21 * 2 + 1);

    // A tap on the card opens it; its trash (on the hovered face) deletes.
    h.delete_card("Twang");
    let messages = h.click("Twang");
    assert_eq!(messages, vec![Message::Open(h.id_of("Twang"))]);
}

#[test]
fn an_empty_library_pitches_building_one_with_no_filter() {
    let mut h = Harness::library(Vec::new());
    h.settle();
    assert!(h.shows("Build your technique library"));
    assert!(h.shows(
        "Collect the techniques you train, in your own words. Add a technique, tag it however \
         your method does, and attach audio or video examples."
    ));
    // Search + tag filter only appear once there's something to filter.
    assert!(!h.shows("All tags"));
    assert!(!h.shows(&glyph(Icon::Search)));
    assert!(h.shows("New technique"));
}

// A failed list read used to fall through to the empty state,
// indistinguishable from a genuinely empty library and with no way to retry.
#[test]
fn a_load_failure_shows_the_error_state_and_retry_reloads() {
    let repo = FakeRepo::with(vec![technique("1", "Twang", &["EVT"])]);
    repo.fail_reads.store(1, Ordering::SeqCst);
    let mut h = Harness::fake(&repo);
    h.settle();

    // The error state — never the empty-library pitch — with a Retry
    // button.
    assert!(h.shows("Could not load your techniques"));
    assert!(h.shows("Something went wrong reading your library. Your data is safe."));
    assert!(h.shows("Retry"));
    assert!(!h.shows("Build your technique library"));

    // Retry re-runs the load; the repo now succeeds and the grid appears.
    assert_eq!(h.click("Retry"), vec![Message::Retry]);
    h.settle();
    assert!(h.shows("Twang"));
    assert!(!h.shows("Could not load your techniques"));
}

#[test]
fn the_header_cluster_right_anchors_and_new_technique_hugs_the_edge() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"]), stored("Curbing", &["CVT"])]);
    h.settle();
    // The tag filter is a compact dropdown button (default label "All
    // tags").
    assert!(h.shows("All tags"));
    // New technique sits flush against the right edge.
    let new = h.find("New technique").expect("New technique");
    assert!(
        new.x + new.width > WIDE.width - 40.0,
        "New technique should hug the right edge: {new:?}"
    );
    // The whole cluster is right-anchored: the search box sits in the right
    // half (not parked next to the title on the left).
    let search = h.find(&glyph(Icon::Search)).expect("the search glyph");
    assert!(search.center_x() > WIDE.width * 0.45, "{search:?}");
    let title = h.find("Technique library").expect("the title");
    assert!(
        title.x < 60.0,
        "the title sits by the back chevron: {title:?}"
    );
}

#[test]
fn the_back_chevron_leaves_for_home() {
    let mut h = Harness::library(Vec::new());
    assert_eq!(h.click(&glyph(Icon::ChevronLeft)), vec![Message::Back]);
    assert_eq!(h.navigations, vec![Navigation::PopOrGo(Route::Home)]);
}

#[test]
fn the_tag_menu_lists_tags_and_filters_on_select() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"]), stored("Curbing", &["CVT"])]);
    h.settle();

    // Open the dropdown.
    assert_eq!(h.click("All tags"), vec![Message::OpenTagMenu]);
    assert!(h.screen.tag_menu_open());
    // The menu lists the tags with their counts, after "All tags" (the
    // card shows CVT too: the menu's entry is the last).
    assert_eq!(h.count("CVT"), 3);
    assert_eq!(
        h.count("All tags"),
        2,
        "the button and the menu's first entry"
    );
    assert!(!h.shows("Untagged"), "every technique has a tag");

    // Selecting a tag filters the grid and closes the menu.
    assert_eq!(
        h.click_nth("CVT", 2),
        vec![Message::PickTag(Some("CVT".into()))]
    );
    assert!(!h.screen.tag_menu_open());
    assert_eq!(h.names(), ["Curbing"]);
    assert!(h.shows("Curbing"));
    assert!(!h.shows("Twang"));
    // The button names the filter.
    assert!(!h.shows("All tags"));

    // "All tags" brings everything back.
    h.click("CVT");
    h.send(Message::PickTag(None));
    assert_eq!(h.names().len(), 2);
}

#[test]
fn the_tag_menu_offers_untagged_when_a_technique_has_no_tag() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"]), stored("Mixed voice", &[])]);
    h.settle();
    h.click("All tags");
    assert!(h.shows("Untagged"));
    h.click("Untagged");
    assert_eq!(h.screen.vm.active_tag(), Some(UNTAGGED_FILTER));
    assert_eq!(h.names(), ["Mixed voice"]);
    // The button reads "Untagged" for the sentinel.
    assert!(h.shows("Untagged"));
}

// It read the library once: a reload landing while it was open left it
// offering the tags and counts from before.
#[test]
fn the_open_tag_menu_follows_a_reload() {
    let repo = FakeRepo::with(vec![technique("1", "Twang", &["EVT"])]);
    let mut h = Harness::fake(&repo);
    h.settle();
    h.click("All tags");
    assert_eq!(h.count("CVT"), 0);

    repo.set(vec![
        technique("1", "Twang", &["EVT"]),
        technique("2", "Curbing", &["CVT"]),
    ]);
    let reload = h.screen.vm.refresh();
    h.pending.extend(reload);
    h.settle();
    assert!(h.screen.tag_menu_open());
    // The card's chip (twice) and the menu's entry.
    assert_eq!(h.count("CVT"), 3);
}

#[test]
fn escape_or_a_press_outside_closes_the_tag_menu_and_nothing_beneath_sees_it() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"])]);
    h.settle();
    h.click("All tags");
    let messages = h.drive(|ui| {
        let _ = ui.tap_key(keyboard::Key::Named(Named::Escape));
    });
    assert_eq!(messages, vec![Message::DismissTagMenu]);
    assert!(!h.screen.tag_menu_open());

    h.click("All tags");
    // A tap on a card beneath the open menu only closes the menu.
    assert_eq!(h.click("Twang"), vec![Message::DismissTagMenu]);
    assert!(h.navigations.is_empty());
}

#[test]
fn typing_in_the_search_box_filters_the_cards() {
    let mut h = Harness::library(vec![stored("Twang", &["EVT"]), stored("Curbing", &["CVT"])]);
    h.settle();
    // `enterText`: the field's whole text at once.
    h.send(Message::Query("Twan".into()));
    let messages = h.drive(|ui| {
        ui.click(SEARCH).expect("the search field");
        let _ = ui.tap_key(keyboard::Key::Named(Named::End));
        let _ = ui.typewrite("g");
    });
    assert_eq!(messages, vec![Message::Query("Twang".into())]);
    assert_eq!(h.names(), ["Twang"]);

    h.send(Message::Query("nothing like it".into()));
    assert!(h.shows("No techniques match this filter."));
}

// technique_dialogs_test, through the page: the dialog's name (trimmed by
// the dialog itself, tested with it) creates the technique, which then
// opens.
#[test]
fn new_technique_asks_for_a_name_creates_it_and_opens_it() {
    let mut h = Harness::library(vec![stored("Twang", &[])]);
    h.settle();
    assert_eq!(h.click("New technique"), vec![Message::NewTechnique]);
    assert!(
        matches!(
            h.dialog,
            Some(DialogRequest {
                dialog: Dialog::NewTechnique(_),
                ..
            })
        ),
        "the app's dialog stack shows the new-technique dialog"
    );

    h.answer(DialogResult::Text(Some("Belt".into())));
    assert!(h.dialog.is_none(), "the dialog answered");
    assert!(h.screen.vm.create_technique().running());
    h.settle();

    let id = h.id_of("Belt");
    assert_eq!(h.screen.vm.all()[0].name, "Belt", "first in the list");
    assert_eq!(
        h.navigations,
        vec![Navigation::Push(Route::Technique { id: id.clone() })]
    );
    assert!(h
        .services
        .technique_repo
        .get_technique(&id)
        .is_ok_and(|t| t.name == "Belt"));
}

#[test]
fn a_cancelled_new_technique_creates_nothing() {
    let mut h = Harness::library(Vec::new());
    h.settle();
    h.send(Message::NewTechnique);
    // Cancel, Escape and a blank name all answer `Text(None)` (the dialog's
    // own tests).
    h.answer(DialogResult::Text(None));
    assert!(h.pending.is_empty());
    assert!(!h.screen.vm.create_technique().running());
}

#[test]
fn a_create_that_fails_says_so_for_four_seconds() {
    let repo = FakeRepo::with(Vec::new());
    repo.refuse_writes.store(true, Ordering::SeqCst);
    let mut h = Harness::fake(&repo);
    h.settle();
    h.send(Message::NewTechniqueNamed(Some("Belt".into())));
    h.settle();
    assert!(h.navigations.is_empty());
    assert_eq!(h.screen.snack(), Some(CREATE_FAILED));
    assert!(h.shows(CREATE_FAILED));

    // A second waits its turn behind the first.
    h.send(Message::NewTechniqueNamed(Some("Belt".into())));
    h.settle();
    assert_eq!(h.screen.snacks.len(), 2);
    let seq = h.screen.snacks.current_seq().expect("one showing");
    h.send(Message::SnackExpired(seq + 1));
    assert_eq!(h.screen.snacks.len(), 2, "a stale timer hides nothing");
    h.send(Message::SnackExpired(seq));
    assert_eq!(h.screen.snacks.len(), 1);
    let seq = h.screen.snacks.current_seq().expect("the next");
    h.send(Message::SnackExpired(seq));
    assert_eq!(h.screen.snack(), None);
    assert!(h.shows(CREATE_FAILED), "leaving");
    h.send(Message::SnackFrame(
        std::time::Instant::now() + crate::ui::snack::TRANSITION,
    ));
    assert!(!h.shows(CREATE_FAILED), "gone");
}

#[test]
fn a_create_while_another_runs_is_dropped_and_says_so() {
    let mut h = Harness::library(Vec::new());
    h.settle();
    h.send(Message::NewTechniqueNamed(Some("Belt".into())));
    h.send(Message::NewTechniqueNamed(Some("Twang".into())));
    assert_eq!(h.screen.snack(), Some(CREATE_FAILED));
    h.settle();
    assert_eq!(h.names(), ["Belt"]);
}

#[test]
fn an_open_is_guarded_until_the_technique_page_comes_back() {
    let mut h = Harness::library(vec![stored("Twang", &[]), stored("Belt", &[])]);
    h.settle();
    let twang = h.id_of("Twang");
    h.click("Twang");
    // A rapid second tap while one open is in flight is ignored.
    h.click("Belt");
    assert_eq!(
        h.navigations,
        vec![Navigation::Push(Route::Technique { id: twang })]
    );

    // Back from the page: the list reloads (its edits and deletes), and
    // card taps open again.
    let returned = {
        let services = h.services.clone();
        let reload = h.screen.vm.load().running();
        assert!(!reload);
        h.screen.returned(&services)
    };
    drop(returned);
    assert!(h.screen.vm.load().running(), "the reload started");
    h.click("Belt");
    assert_eq!(h.navigations.len(), 2);
}

#[test]
fn the_return_reloads_what_the_technique_page_changed() {
    let repo = FakeRepo::with(vec![technique("a", "A", &[]), technique("b", "B", &[])]);
    let mut h = Harness::fake(&repo);
    h.settle();
    h.send(Message::Open("a".into()));
    repo.set(vec![technique("b", "B", &[])]); // deleted on its page
    let reload = h.screen.vm.refresh();
    // What `returned` does, with the effect kept here.
    h.screen.opening = false;
    h.pending.extend(reload);
    h.settle();
    assert_eq!(h.names(), ["B"]);
}

#[test]
fn a_delete_asks_first_and_a_cancel_deletes_nothing() {
    let mut h = Harness::library(vec![stored("Twang", &[])]);
    h.settle();
    h.delete_card("Twang");
    let Some(DialogRequest {
        dialog: Dialog::Confirm(confirm),
        ..
    }) = &h.dialog
    else {
        panic!("a confirmation")
    };
    assert_eq!(confirm.title, "Delete \"Twang\"?");
    assert_eq!(
        confirm.body.as_deref(),
        Some("This removes the technique and its example clips. This cannot be undone.")
    );
    assert_eq!(confirm.confirm_label, "Delete");
    assert!(confirm.destructive);

    h.answer(DialogResult::Confirmed(false));
    assert!(h.pending.is_empty());
    assert!(h.shows("Twang"));

    h.delete_card("Twang");
    h.answer(DialogResult::Confirmed(true));
    h.settle();
    assert!(!h.shows("Twang"));
    assert!(h.shows("Build your technique library"));
    assert!(h
        .services
        .technique_repo
        .list_techniques()
        .unwrap()
        .is_empty());
    assert_eq!(h.screen.snack(), None);
}

// The command dropped the second as re-entry: the technique stayed and the
// page said it couldn't be deleted.
#[test]
fn a_delete_confirmed_while_another_cleans_up_runs_after_it() {
    let mut h = Harness::library(vec![stored("Twang", &[]), stored("Belting", &[])]);
    h.settle();

    h.delete_card("Twang");
    h.answer(DialogResult::Confirmed(true));
    h.run_effects(); // its media cleanup parks
    assert_eq!(h.cleanups.len(), 1);
    assert!(!h.shows("Twang"));
    assert!(h.screen.vm.delete_technique().running());

    h.delete_card("Belting");
    h.answer(DialogResult::Confirmed(true));
    assert!(h.pending.is_empty(), "it waits for the first");
    h.settle();

    assert!(!h.shows("Belting"));
    assert!(h
        .services
        .technique_repo
        .list_techniques()
        .unwrap()
        .is_empty());
    assert_eq!(h.screen.snack(), None);
    assert!(!h.shows(DELETE_FAILED));
}

#[test]
fn a_delete_that_fails_says_so_and_keeps_the_card() {
    let repo = FakeRepo::with(vec![technique("a", "Twang", &[])]);
    let mut h = Harness::fake(&repo);
    h.settle();
    repo.refuse_writes.store(true, Ordering::SeqCst);
    h.send(Message::DeleteConfirmed {
        id: "a".into(),
        confirmed: true,
    });
    h.settle();
    assert!(h.shows("Twang"));
    assert_eq!(h.screen.snack(), Some(DELETE_FAILED));
}

#[test]
fn a_remote_change_to_a_technique_reloads_the_list() {
    let repo = FakeRepo::with(vec![technique("a", "Twang", &[])]);
    let mut h = Harness::fake(&repo);
    h.settle();
    repo.set(vec![technique("a", "Twang (renamed)", &[])]);
    h.send(Message::RemoteApplied(remote(
        &[("practices", "P1")],
        &["P1"],
        &[],
    )));
    assert!(h.pending.is_empty(), "no technique touched");
    h.send(Message::RemoteApplied(remote(
        &[("techniques", "a")],
        &[],
        &["a"],
    )));
    h.settle();
    assert!(h.shows("Twang (renamed)"));
}

#[test]
fn a_card_shows_its_examples_and_at_most_three_tags() {
    use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
    let mut t = technique("a", "Twang", &["One", "Two", "Three", "Four", "Five"]);
    let clip = |kind| TechniqueMedia {
        id: "m".into(),
        path: "p".into(),
        kind,
        label: String::new(),
        added_at: t.created_at,
    };
    t.media = vec![
        clip(TechniqueMediaKind::Audio),
        clip(TechniqueMediaKind::Video),
    ];
    let repo = FakeRepo::with(vec![t]);
    let mut h = Harness::fake(&repo);
    h.settle();
    assert!(h.shows("Three"));
    assert!(!h.shows("Four"));
    assert!(h.shows("+2"));
    // Any video shows the video glyph, with the clip count.
    assert!(h.shows(&glyph(Icon::Video)));
    assert!(!h.shows(&glyph(Icon::Music2)));
    assert!(h.shows("2"));
}

#[test]
fn hovering_a_card_draws_it_differently() {
    let mut h = Harness::library(vec![stored("Twang", &[])]);
    h.settle();
    let snapshot = |h: &Harness, cursor: Option<Point>| {
        let mut ui = h.ui();
        if let Some(at) = cursor {
            ui.point_at(at);
            let _ = ui.simulate([iced::Event::Mouse(mouse::Event::CursorMoved {
                position: at,
            })]);
        }
        ui.snapshot(&theme::app_theme()).expect("snapshot")
    };
    let card = h.find("Twang").expect("the card");
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("card.png");
    assert!(snapshot(&h, None).matches_image(&path).expect("write"));
    assert!(!snapshot(&h, Some(card.center()))
        .matches_image(&path)
        .expect("compare"));
    assert!(snapshot(&h, Some(Point::new(1400.0, 850.0)))
        .matches_image(&path)
        .expect("compare"));
}
