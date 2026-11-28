//! Port of `ui/technique_library/technique_library_screen.dart`: the
//! technique library home — a flat pool of vocal techniques browsed by a tag
//! filter plus search. Framework agnostic: tags (including framework tags
//! like EVT/CVT) are the only organizing axis, and a technique can carry
//! many.
//!
//! The page, drawn as the library is (`DESIGN.md`, the approved
//! `Home.dc.html`): its unified toolbar (back, the title, then — once there
//! is something to search — the search field, and the New technique icon
//! button at its right end); below it the spinner, the read error with
//! Retry, the empty state, or the "Techniques" heading with its count and
//! the tag filter (a pop-up button) over the grid of cards, or a line
//! saying nothing matches the search or the filter ([`no_match`]). A card
//! opens its technique; hovered, it
//! lifts and shows its trash (delete, after a confirmation); its tags
//! filter the library.
//!
//! The view model's work runs off the UI thread ([`task`]); what it reports
//! comes back as [`Message::Landed`]. The Dart screen's own `await`s keep
//! their order: a create that finished opens its technique (or says it
//! could not), a delete that finished says whether it failed and lets the
//! next confirmed one run, and the return from a technique's page reloads
//! the list and opens card taps again.
//!
//! A card is the library's (`DESIGN.md`, Cards): radius 8, `#18181C` with
//! an inset `Studio::BORDER` outline; under the pointer it lifts to
//! `#1C1C21` with a `#3A3A43` outline and a soft shadow, over 120 ms,
//! linear (the Dart `AnimatedContainer`'s timing).
//!
//! The page draws its snack bars itself, over its bottom edge, while
//! it is on top; once it leaves (or is covered) the app keeps them up over
//! the page on top (`app::messenger`), as Dart's `ScaffoldMessenger`, above
//! every page, did. A new one waits behind the ones up, as `showSnackBar`
//! queued it; each slides up and back down as Material's fixed snack bar
//! did, its time counted from the end of its entrance (`ui::snack`); the
//! timer is a subscription.

mod anchored;
/// The page's message for what its snack bars report.
fn snack_message(event: SnackEvent) -> Message {
    match event {
        SnackEvent::Expired(seq) => Message::SnackExpired(seq),
        SnackEvent::Frame(at) => Message::SnackFrame(at),
        SnackEvent::Swiped(seq) => Message::SnackSwiped(seq),
    }
}

#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, center, column, container, hover, responsive, row, stack, Column, Row, Space,
};
use iced::{Element, Length, Padding, Task};
use vocara_model::models::technique::Technique;
use vocara_model::util::dart_string;
use vocara_store::sync::sync_engine::RemoteApplied;

use crate::app::library::RemoteAppliedBroadcast;

use self::anchored::Anchored;
use super::view_model::technique_library_view_model::{
    Done, Effect, TechniqueLibraryViewModel, UNTAGGED_FILTER,
};
use super::widgets::technique_dialogs::NewTechniqueDialog;
use crate::router::{Navigation, Route};
use crate::services::Services;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, bare_button, boxed, font, height, radius, type_scale, Studio};
use crate::ui::core::widgets::animated::animated_container;
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::controls::{icon_button, popup_button};
use crate::ui::core::widgets::fading_scrollbar::{no_scrollbar, scrollable, Fading};
use crate::ui::core::widgets::line_icon::glyphs;
use crate::ui::core::widgets::studio_button::studio_button;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::text_field::text_input;
use crate::ui::core::widgets::toolbar;
use crate::ui::home::widgets::practice_project_card::{card_look, FILL_HOVERED};
use crate::ui::snack::{fixed_bar, SnackEvent, SnackQueue, SnackStyle};
use crate::ui::{Screen, SnackBar, Update};

/// The snack bar after a create that did not happen.
pub const CREATE_FAILED: &str = "Couldn't create the technique.";
/// The snack bar after a delete that did not happen.
pub const DELETE_FAILED: &str = "Couldn't delete the technique.";

/// How long a snack bar shows (Material's default, 4 s).
pub const SNACK_DURATION: Duration = Duration::from_millis(4000);

/// The library's padding, as Home's: 28 above the heading, 32 at the sides
/// and the bottom.
pub const LIBRARY_PADDING: Padding = Padding {
    top: 28.0,
    right: 32.0,
    bottom: 32.0,
    left: 32.0,
};
/// The space between the heading and the first row of cards.
pub const HEADING_GAP: f32 = 16.0;
/// The heading row's height (the 15 px heading's 20 px line; the 22 px
/// tag filter beside it).
pub const HEADING_HEIGHT: f32 = 22.0;
/// How long a card takes to lift (`AnimatedContainer`).
pub const CARD_ANIMATION: Duration = Duration::from_millis(120);

/// The widest a card gets (`maxCrossAxisExtent`, Home's).
pub const CARD_MAX_WIDTH: f32 = 280.0;
/// Every card's height (`mainAxisExtent`): the name, two lines of summary
/// and the tag row in the card's padding (10 + 18 + 4 + 32 + 10 + 18 + 12).
pub const CARD_HEIGHT: f32 = 104.0;
/// The space between cards, both ways.
pub const CARD_SPACING: f32 = 16.0;
/// At most this many tags show on a card, then a "+N" marker.
pub const MAX_CHIPS: usize = 3;
/// The widest a tag on a card gets before it is ellipsized.
pub const TAG_MAX_WIDTH: f32 = 112.0;
/// The tag row's height.
pub const TAG_HEIGHT: f32 = 18.0;

/// The search field's width in the toolbar.
pub const SEARCH_WIDTH: f32 = 220.0;
/// The widest the tag filter's pop-up button gets.
pub const TAG_FILTER_MAX_WIDTH: f32 = 200.0;
/// The tag menu's width.
pub const MENU_WIDTH: f32 = 220.0;

/// The search field's clear button's tooltip.
pub const CLEAR_SEARCH: &str = "Clear search";

/// The empty library's button.
pub const NEW_TECHNIQUE: &str = "New technique";

/// The search field's id.
pub const SEARCH: iced::widget::Id = iced::widget::Id::new("technique-library-search");

/// The technique library page.
#[derive(Debug)]
pub struct TechniqueLibraryScreen {
    /// The view model (boxed: every page sits in one enum, and its state
    /// is large).
    vm: Box<TechniqueLibraryViewModel>,
    /// Guards against a double-tap pushing the detail route twice (two
    /// stacked identical screens the user must back out of twice). Released
    /// when the pushed page comes back.
    opening: bool,
    /// The grid's scrollable, and how far down it was last scrolled: the
    /// technique page pushed over the library takes its widgets with it
    /// (the pages beneath are not drawn), so its pop puts the offset back.
    grid_id: iced::widget::Id,
    grid_scroll: f32,
    /// What the sync engine announces after applying remote rows (the
    /// library's broadcast): a freshness reload.
    remote_applied: Arc<RemoteAppliedBroadcast>,
    /// Whether the tag filter's menu is open.
    tag_menu: bool,
    /// Deletes confirmed while another still ran, in order: each runs once
    /// the one before it is done (Dart's `while (running) await
    /// _nextChange(...)`).
    queued_deletes: VecDeque<String>,
    /// The snack bars: the first shows, the rest wait their turn (the
    /// `ScaffoldMessenger` queue).
    snacks: SnackQueue,
}

/// What the library reacts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The header's back chevron (`popOrGo(context, Routes.home)`).
    Back,
    /// The search text changed.
    Query(String),
    /// The technique grid scrolled to this offset (kept, to put the grid
    /// back where it was when the technique opened over it is popped).
    GridScrolled(f32),
    /// The tag filter: open its menu, close it, or pick an entry (`None`
    /// for "All tags", [`UNTAGGED_FILTER`] for "Untagged").
    OpenTagMenu,
    DismissTagMenu,
    PickTag(Option<String>),
    /// A card's tag chip: filter to that tag.
    TapTag(String),
    /// "New technique": ask for a name.
    NewTechnique,
    /// The new-technique dialog closed: the name to create, or `None`.
    NewTechniqueNamed(Option<String>),
    /// A card was tapped: open its technique.
    Open(String),
    /// A card's trash: confirm, then delete.
    Delete {
        id: String,
        name: String,
    },
    /// The delete confirmation closed.
    DeleteConfirmed {
        id: String,
        confirmed: bool,
    },
    /// The read error's Retry.
    Retry,
    /// A view model effect reported.
    Landed(Done),
    /// Sync applied remote changes (`Persistence.remoteApplied`).
    ///
    /// SEAM: nothing sends it until the library exposes the remote-applied
    /// stream (`vocara_services::persistence`); then the screen's
    /// subscription forwards each event here. With no sync configured the
    /// Dart stream never fired either.
    RemoteApplied(RemoteApplied),
    /// The showing snack bar's time is up.
    SnackExpired(u64),
    /// A frame while a snack bar enters or leaves.
    SnackFrame(Instant),
    /// Snack bar `n` was swiped away.
    SnackSwiped(u64),
}

/// What handling one message asked for: the view model's work, to run off
/// the UI thread, and the rest of the update.
struct Step {
    effects: Vec<Effect>,
    update: Update<Message>,
}

impl Step {
    fn none() -> Self {
        Update::none().into()
    }

    fn effect(effect: Option<Effect>) -> Self {
        Self {
            effects: effect.into_iter().collect(),
            update: Update::none(),
        }
    }

    fn into_update(self) -> Update<Message> {
        self.effects
            .into_iter()
            .fold(self.update, |update, effect| update.and_task(task(effect)))
    }
}

impl From<Update<Message>> for Step {
    fn from(update: Update<Message>) -> Self {
        Self {
            effects: Vec::new(),
            update,
        }
    }
}

impl From<Task<Message>> for Step {
    fn from(task: Task<Message>) -> Self {
        Update::task(task).into()
    }
}

/// Runs a view model effect on a blocking thread; what it reports comes
/// back as [`Message::Landed`] — a delete's media cleanup after its rows,
/// in that order, whether or not the page is still there to hear it.
pub fn task(effect: Effect) -> Task<Message> {
    let lost = effect.lost("the worker was lost");
    Task::perform(crate::util::blocking(move || effect.run()), move |joined| {
        joined.unwrap_or((lost, None))
    })
    .then(|(done, cleanup)| {
        let landed = Task::done(Message::Landed(done));
        match cleanup {
            None => landed,
            Some(cleanup) => {
                let lost = cleanup.lost();
                landed.chain(Task::perform(
                    crate::util::blocking(move || cleanup.run()),
                    move |joined| Message::Landed(joined.unwrap_or(lost)),
                ))
            }
        }
    })
}

impl TechniqueLibraryScreen {
    /// The page over the shared repositories, its first load started.
    pub fn new(services: &Services) -> (Self, Task<Message>) {
        let (screen, load) = Self::open(services);
        (screen, Step::effect(load).into_update().task)
    }

    fn open(services: &Services) -> (Self, Option<Effect>) {
        let (vm, load) = TechniqueLibraryViewModel::new(
            services.technique_repo.clone(),
            services.technique_media.clone(),
        );
        let screen = Self {
            vm: Box::new(vm),
            opening: false,
            grid_id: iced::widget::Id::unique(),
            grid_scroll: 0.0,
            remote_applied: services.library.remote_applied.clone(),
            tag_menu: false,
            queued_deletes: VecDeque::new(),
            snacks: SnackQueue::new(),
        };
        (screen, load)
    }

    /// The view model.
    pub fn view_model(&self) -> &TechniqueLibraryViewModel {
        &self.vm
    }

    /// The snack bar showing, if any.
    pub fn snack(&self) -> Option<&str> {
        self.snacks.current().map(|s| s.text.as_str())
    }

    /// Whether the tag filter's menu is open.
    pub fn tag_menu_open(&self) -> bool {
        self.tag_menu
    }

    fn handle(&mut self, message: Message) -> Step {
        match message {
            Message::Back => Update::pop_or_go(Route::Home).into(),
            Message::GridScrolled(y) => {
                self.grid_scroll = y;
                Step::none()
            }
            Message::Query(q) => {
                self.vm.set_query(q);
                Step::none()
            }
            Message::OpenTagMenu => {
                self.tag_menu = true;
                Step::none()
            }
            Message::DismissTagMenu => {
                self.tag_menu = false;
                Step::none()
            }
            Message::PickTag(tag) => {
                self.vm.set_tag(tag);
                self.tag_menu = false;
                Step::none()
            }
            Message::TapTag(tag) => {
                self.vm.set_tag(Some(tag));
                Step::none()
            }
            Message::NewTechnique => self.show_new_technique_dialog().into(),
            Message::NewTechniqueNamed(name) => self.create(name),
            Message::Open(id) => match self.open_detail(id) {
                Some(navigation) => Update::navigate(navigation).into(),
                None => Step::none(),
            },
            Message::Delete { id, name } => Update::dialog(
                ConfirmDialog::new(format!("Delete \"{name}\"?")).body(
                    "This removes the technique and its example clips. This cannot be undone.",
                ),
                move |result| Message::DeleteConfirmed {
                    id,
                    confirmed: result.confirmed(),
                },
            )
            .into(),
            Message::DeleteConfirmed { id, confirmed } => {
                if !confirmed {
                    return Step::none();
                }
                // One delete at a time, and the command drops a second as
                // re-entry: one confirmed while another still cleaned up its
                // media was ignored, then reported as failed. It runs once
                // that one is done instead.
                if self.vm.delete_technique().running() {
                    self.queued_deletes.push_back(id);
                    return Step::none();
                }
                Step::effect(self.vm.execute_delete(id))
            }
            Message::Retry => Step::effect(self.vm.execute_load()),
            Message::Landed(done) => self.landed(done),
            Message::RemoteApplied(event) => Step::effect(self.vm.on_remote_applied(&event)),
            Message::SnackExpired(seq) => {
                self.snacks.expired(seq, Instant::now());
                Step::none()
            }
            Message::SnackFrame(at) => {
                self.snacks.frame(at);
                Step::none()
            }
            Message::SnackSwiped(seq) => {
                self.snacks.swiped(seq, Instant::now());
                Step::none()
            }
        }
    }

    /// `showNewTechniqueDialog(context)`, on the app's dialog stack; the
    /// name comes back as [`Message::NewTechniqueNamed`].
    fn show_new_technique_dialog(&mut self) -> Update<Message> {
        Update::dialog(NewTechniqueDialog::new(), |result| {
            Message::NewTechniqueNamed(result.text())
        })
    }

    /// `_createTechnique` once the dialog gave a name: create it; the rest
    /// follows when the create lands.
    fn create(&mut self, name: Option<String>) -> Step {
        let Some(name) = name else {
            return Step::none();
        };
        match self.vm.execute_create(name) {
            Some(effect) => Step::effect(Some(effect)),
            // Dropped as re-entry: the `await` returned at once, with no
            // created technique to open.
            None => {
                self.show_snack(CREATE_FAILED);
                Step::none()
            }
        }
    }

    /// `_openDetail`: pushes the technique's page, unless an open is
    /// already in flight (a rapid second tap is ignored).
    fn open_detail(&mut self, id: String) -> Option<Navigation> {
        if self.opening {
            return None;
        }
        self.opening = true;
        Some(Navigation::Push(Route::Technique { id }))
    }

    /// A view model effect reported: land it, then carry on where the Dart
    /// screen's `await` of a create or delete resumed.
    fn landed(&mut self, done: Done) -> Step {
        let creating = self.vm.create_technique().running();
        let deleting = self.vm.delete_technique().running();
        let mut step = Step::effect(self.vm.land(done));
        // The menu hangs under the tag filter, which shows only while there
        // is something to filter: with the library emptied it closes (Dart's
        // popup route stayed up, offering "All tags 0").
        if self.vm.all().is_empty() {
            self.tag_menu = false;
        }
        if creating && !self.vm.create_technique().running() {
            match self.vm.create_technique().result() {
                Some(Ok(technique)) => {
                    let id = technique.id.clone();
                    if let Some(navigation) = self.open_detail(id) {
                        step.update = step.update.and_navigate(navigation);
                    }
                }
                _ => self.show_snack(CREATE_FAILED),
            }
        }
        if deleting && !self.vm.delete_technique().running() {
            if !self.vm.delete_technique().completed() {
                self.show_snack(DELETE_FAILED);
            }
            // The next confirmed delete, which waited for this one.
            if let Some(id) = self.queued_deletes.pop_front() {
                step.effects.extend(self.vm.execute_delete(id));
            }
        }
        step
    }

    /// `showSnackBar(SnackBar(content: Text(text)))`: shown now, or after
    /// the ones before it.
    fn show_snack(&mut self, text: &str) {
        let bar = SnackBar::new(text, SNACK_DURATION, false, SnackStyle::Fixed, fixed_bar);
        self.snacks.show(bar, Instant::now());
    }

    // ---------------------------------------------------------------- view

    /// The page's toolbar (`widgets::toolbar`, as the library's): the back
    /// button and the title, then — once there is something to search — the
    /// search field, and the New technique icon button at its right end.
    fn header(&self) -> Element<'_, Message> {
        let mut bar = row![
            toolbar::back_button(Message::Back),
            Space::new().width(4),
            toolbar::title("Technique library"),
            Space::new().width(Length::Fill),
        ]
        .height(Length::Fill)
        .align_y(Vertical::Center);
        // The search only appears once there's something to search.
        if !self.vm.all().is_empty() {
            bar = bar
                .push(search_field(self.vm.query()))
                .push(Space::new().width(10));
        }
        bar = bar.push(icon_button(glyphs::PLUS, Message::NewTechnique).tooltip("New technique"));
        toolbar::page_toolbar(bar)
    }

    /// The tag filter: a pop-up button naming the active filter, which
    /// opens an anchored, scrollable menu of all tags. Scales to any number
    /// of tags without widening the row.
    fn tag_filter(&self) -> Element<'_, Message> {
        let label = match self.vm.active_tag() {
            None => "All tags",
            Some(UNTAGGED_FILTER) => "Untagged",
            Some(tag) => tag,
        };
        let trigger = container(
            popup_button(label, Some(Message::OpenTagMenu))
                .placeholder(self.vm.active_tag().is_none()),
        )
        .max_width(TAG_FILTER_MAX_WIDTH);
        let menu = self.tag_menu.then(|| self.tag_menu());
        Anchored::new(trigger, menu, Message::DismissTagMenu).into()
    }

    /// The tag filter's menu (`DESIGN.md`, Menus): "All tags", "Untagged"
    /// when some technique has no tag, then every tag with its count; the
    /// active filter checked.
    fn tag_menu(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        let mut items = Column::new().width(Length::Fill).push(menu_item(
            "All tags".into(),
            vm.all().len(),
            vm.active_tag().is_none(),
            Message::PickTag(None),
        ));
        let untagged = vm.untagged_count();
        if untagged > 0 {
            items = items.push(menu_item(
                "Untagged".into(),
                untagged,
                vm.active_tag() == Some(UNTAGGED_FILTER),
                Message::PickTag(Some(UNTAGGED_FILTER.into())),
            ));
        }
        let facets = vm.tag_facets();
        if !facets.is_empty() {
            // A 1 px separator, 4 above and below.
            items = items.push(container(rule()).padding([4, 6]));
        }
        for facet in facets {
            let active = vm.is_tag_active(&facet.tag);
            items = items.push(menu_item(
                facet.tag.clone(),
                facet.count,
                active,
                Message::PickTag(Some(facet.tag)),
            ));
        }
        container(no_scrollbar(scrollable(items.padding(4))))
            .width(MENU_WIDTH)
            .max_height(440)
            .style(theme::menu)
            .clip(true)
            .into()
    }

    /// The heading over the cards, as the library's "Practices": the 15 px
    /// "Techniques" and how many there are in `TEXT_FAINT` ("3 of 9" while
    /// a filter or a search narrows them), the tag filter at its right.
    fn heading(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        let (shown, all) = (vm.filtered().len(), vm.all().len());
        let count = if shown == all {
            all.to_string()
        } else {
            format!("{shown} of {all}")
        };
        row![
            studio_text("Techniques")
                .size(type_scale::HEADING)
                .font(font::SEMIBOLD)
                .line_height(iced::Pixels(20.0))
                .color(Studio::TEXT_PRIMARY),
            Space::new().width(8),
            studio_text(count)
                .size(type_scale::BODY)
                .line_height(iced::Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_FAINT),
            Space::new().width(Length::Fill),
            self.tag_filter(),
        ]
        .height(HEADING_HEIGHT)
        .align_y(Vertical::Center)
        .into()
    }

    fn body(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        if vm.load().running() && vm.all().is_empty() {
            return center(spinner(36.0, 4.0, Studio::TEXT_SECONDARY)).into();
        }
        // A failed list read must read as an error (with Retry), never
        // collapse into the empty state — otherwise a load failure looks
        // like "Build your technique library" over a library that still
        // exists. Mirrors the home screen's practice list. Only when nothing
        // is cached: with techniques already on screen, a failed refresh
        // shouldn't blank them out.
        if vm.load().error() && vm.all().is_empty() {
            return error_state();
        }
        if vm.all().is_empty() {
            return empty_state();
        }
        if vm.filtered().is_empty() {
            // The heading stays, so the filter can still be changed.
            return column![
                container(self.heading()).padding(Padding {
                    bottom: 0.0,
                    ..LIBRARY_PADDING
                }),
                center(
                    studio_text(no_match(
                        !dart_string::trim(vm.query()).is_empty(),
                        vm.active_tag().is_some(),
                    ))
                    .size(type_scale::BODY)
                    .line_height(iced::Pixels(18.0))
                    .color(Studio::TEXT_FAINT),
                ),
            ]
            .into();
        }
        grid(self)
    }
}
impl Screen for TechniqueLibraryScreen {
    type Message = Message;

    /// Freshness reloads when sync applies remote rows (with no sync
    /// configured the stream simply never fires), and the showing snack
    /// bar's timer.
    fn subscription(&self, _on_top: bool) -> iced::Subscription<Message> {
        let applied = self
            .remote_applied
            .subscription()
            .map(Message::RemoteApplied);
        let snacks = self.snacks.subscription().map(snack_message);
        iced::Subscription::batch([applied, snacks])
    }

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        self.handle(message).into_update()
    }

    fn view(&self) -> Element<'_, Message> {
        let page = column![
            self.header(),
            container(self.body())
                .width(Length::Fill)
                .height(Length::Fill)
        ];
        let mut layers = stack![container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(snacks) = self.snacks.view() {
            layers = layers.push(snacks.map(snack_message));
        }
        layers.into()
    }

    /// The technique's page came back: reload for its edits and deletes
    /// (not awaited: card taps are open again while it loads).
    fn returned(&mut self, _services: &Services) -> Update<Message> {
        let reload = self.vm.refresh();
        self.opening = false;
        // Back where it was: the technique page took the grid's widgets.
        let restore = iced::widget::operation::scroll_to(
            self.grid_id.clone(),
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(self.grid_scroll),
            },
        );
        Step::effect(reload).into_update().and_task(restore)
    }

    fn dispose(&mut self, _services: &Services) {
        self.vm.dispose();
    }

    /// The snack bar showing, for the time it has left, then the ones
    /// waiting, for all of theirs; each queued behind the ones before it.
    fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        self.snacks.take(Instant::now())
    }
}

/// A full-width 1 px line in `Studio::BORDER`.
fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(Studio::BORDER))
        .into()
}

/// What the page says when nothing passes the search and the tag filter:
/// worded for whichever of them narrows the list.
pub fn no_match(searching: bool, filtering: bool) -> &'static str {
    match (searching, filtering) {
        (true, false) => "No techniques match your search.",
        (true, true) => "No techniques match your search and filter.",
        (false, _) => "No techniques match this filter.",
    }
}

/// The search field (`DESIGN.md`, Text fields): 28 px, the field's own
/// well (its outline the accent at 60 % while focused), a 13 px search
/// glyph in `TEXT_FAINT` inside it at the left, 12 px text. While it holds
/// a search, a clear button sits inside its right end, and Escape in it
/// clears it too, as in a macOS search field.
fn search_field(query: &str) -> Element<'_, Message> {
    let input = text_input("Search techniques", query)
        .id(SEARCH)
        .on_input(Message::Query)
        .on_escape(Message::Query(String::new()))
        .size(theme::FIELD_TEXT_SIZE)
        .line_height(iced::Pixels(type_scale::LINE))
        .padding(Padding {
            left: 28.0,
            right: 28.0,
            ..theme::FIELD_PADDING
        })
        .caret_color(Studio::ACCENT)
        .style(theme::field);
    // The glyph over the field's left inset; it takes no input, so a press
    // on it lands on the field.
    let glyph = container(icons::icon(Icon::Search, 13.0, Studio::TEXT_FAINT))
        .padding(Padding {
            left: 9.0,
            ..Padding::ZERO
        })
        .center_y(theme::FIELD_HEIGHT);
    let mut field = stack![input, glyph];
    if !query.is_empty() {
        field = field.push(
            container(
                icon_button(Icon::X, Message::Query(String::new()))
                    .panel()
                    .icon_size(12.0)
                    .tooltip(CLEAR_SEARCH),
            )
            .width(Length::Fill)
            .align_x(Horizontal::Right)
            .padding(Padding {
                right: 2.0,
                ..Padding::ZERO
            })
            .center_y(theme::FIELD_HEIGHT),
        );
    }
    field.width(SEARCH_WIDTH).into()
}

/// One row of the tag menu (`DESIGN.md`, Menus): 24 px, radius 4, a
/// `HOVER` fill under the pointer; the label at 12 px with a check before
/// the active filter, its count in `TEXT_FAINT` pinned to the right so
/// every row's count lines up.
fn menu_item<'a>(
    label: String,
    count: usize,
    active: bool,
    on_tap: Message,
) -> Element<'a, Message> {
    let check: Element<'a, Message> = if active {
        icons::icon(Icon::Check, 12.0, Studio::TEXT_PRIMARY).into()
    } else {
        Space::new().width(12).into()
    };
    button(
        row![
            check,
            Space::new().width(6),
            studio_text(label)
                .size(type_scale::BODY)
                .line_height(iced::Pixels(type_scale::LINE))
                .font(if active { font::MEDIUM } else { font::REGULAR })
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill)
                .ellipsis(1),
            Space::new().width(10),
            studio_text(count.to_string())
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_FAINT),
        ]
        .height(Length::Fill)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .height(height::PANEL_BUTTON)
    .padding([0, 8])
    .style(|t, status| iced::widget::button::Style {
        border: iced::Border {
            radius: radius::SMALL.into(),
            ..iced::Border::default()
        },
        ..theme::menu_item(t, status)
    })
    .on_press(on_tap)
    .into()
}

/// How the grid lays out at a content `width` (the page's, its padding
/// included): the column count and the card width
/// (`SliverGridDelegateWithMaxCrossAxisExtent`: as many columns as keep
/// each card at most 280 wide, as Home's).
pub fn grid_columns(width: f32) -> (usize, f32) {
    let extent = (width - LIBRARY_PADDING.left - LIBRARY_PADDING.right).max(0.0);
    let columns = ((extent / (CARD_MAX_WIDTH + CARD_SPACING)).ceil() as usize).max(1);
    let usable = (extent - CARD_SPACING * (columns - 1) as f32).max(0.0);
    (columns, usable / columns as f32)
}

/// The heading, then the cards in rows of [`grid_columns`], all in the
/// library's padding, scrolling vertically.
fn grid(screen: &TechniqueLibraryScreen) -> Element<'_, Message> {
    let vm = &*screen.vm;
    responsive(move |size| {
        let (columns, width) = grid_columns(size.width);
        let filtered = vm.filtered();
        let mut rows = Column::new().spacing(CARD_SPACING);
        for chunk in filtered.chunks(columns) {
            let mut cells = Row::new().spacing(CARD_SPACING);
            for technique in chunk {
                cells = cells.push(
                    container(card(technique, vm))
                        .width(width)
                        .height(CARD_HEIGHT),
                );
            }
            rows = rows.push(cells);
        }
        let content = column![screen.heading(), Space::new().height(HEADING_GAP), rows];
        scrollable(
            container(content)
                .padding(LIBRARY_PADDING)
                .width(Length::Fill),
        )
        .id(screen.grid_id.clone())
        .on_scroll(|viewport| Message::GridScrolled(viewport.absolute_offset().y))
        .width(Length::Fill)
        .height(Length::Fill)
        .fading()
        .into()
    })
    .into()
}

/// A technique's card (`DESIGN.md`, Cards, as the library's): its name,
/// its summary and its tags, lifting under the pointer over 120 ms, its
/// trash at the top right while hovered.
fn card<'a>(t: &'a Technique, vm: &TechniqueLibraryViewModel) -> Element<'a, Message> {
    let tap = button(card_face(t, vm))
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(bare_button)
        .on_press(Message::Open(t.id.clone()));
    // The outline over the face: drawn above the content.
    let card = animated_container(tap, CARD_ANIMATION)
        .border_above()
        .style(card_look);
    // The trash at the top right, on the lifted card's fill so a long name
    // runs under it.
    let trash = container(
        container(
            icon_button(
                Icon::Trash2,
                Message::Delete {
                    id: t.id.clone(),
                    name: t.name.clone(),
                },
            )
            .panel()
            .tooltip("Delete technique"),
        )
        .style(|_| boxed(Some(FILL_HOVERED), radius::CONTROL, None)),
    )
    .padding(6)
    .width(Length::Fill)
    .align_x(Horizontal::Right);
    hover(card, trash)
}

/// The card's content: the name (13 px semibold), the summary (12 px
/// `TEXT_SECONDARY`, two lines at most), then the tags and, at the right,
/// the examples it has (an audio or video glyph and their count).
fn card_face<'a>(t: &'a Technique, vm: &TechniqueLibraryViewModel) -> Element<'a, Message> {
    let name = studio_text(t.name.as_str())
        .size(type_scale::TITLE)
        .font(font::SEMIBOLD)
        .line_height(iced::Pixels(18.0))
        .color(Studio::TEXT_PRIMARY)
        .width(Length::Fill)
        .ellipsis(1);
    let subtitle = dart_string::trim(&t.summary);
    let summary: Element<'a, Message> = if subtitle.is_empty() {
        Space::new().into()
    } else {
        // Two lines at most, the second ellipsized.
        studio_text(subtitle)
            .size(type_scale::BODY)
            .line_height(iced::Pixels(type_scale::LINE))
            .color(Studio::TEXT_SECONDARY)
            .width(Length::Fill)
            .ellipsis(2)
            .into()
    };
    let mut meta = Row::new()
        .push(
            container(tag_row(&t.tags, vm))
                .width(Length::Fill)
                .height(TAG_HEIGHT)
                .clip(true),
        )
        .height(TAG_HEIGHT)
        .align_y(Vertical::Center);
    if !t.media.is_empty() {
        let kind = if t.media.iter().any(|m| m.is_video()) {
            Icon::Video
        } else {
            Icon::Music2
        };
        meta = meta
            .push(Space::new().width(8))
            .push(icons::icon(kind, 12.0, Studio::TEXT_FAINT));
        if t.media.len() > 1 {
            meta = meta.push(Space::new().width(3)).push(
                studio_text(t.media.len().to_string())
                    .size(type_scale::LABEL)
                    .line_height(iced::Pixels(type_scale::LINE))
                    .tabular()
                    .color(Studio::TEXT_FAINT),
            );
        }
    }
    container(column![
        name,
        Space::new().height(4),
        // The summary absorbs the slack and is clipped, so the card's
        // content never overflows its fixed grid-cell height.
        container(summary)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true),
        Space::new().height(10),
        meta,
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(Padding {
        top: 10.0,
        right: 12.0,
        bottom: 12.0,
        left: 12.0,
    })
    .clip(true)
    .into()
}

/// The tags on a card: up to three, then a "+N" overflow marker, on one
/// line (a tag past the row's end is ellipsized). Tapping one filters the
/// library to it.
fn tag_row<'a>(tags: &'a [String], vm: &TechniqueLibraryViewModel) -> Element<'a, Message> {
    let mut row = Row::new().spacing(4).align_y(Vertical::Center);
    for tag in tags.iter().take(MAX_CHIPS) {
        row = row.push(container(tag_chip(tag, vm.is_tag_active(tag))).max_width(TAG_MAX_WIDTH));
    }
    let overflow = tags.len().saturating_sub(MAX_CHIPS);
    if overflow > 0 {
        row = row.push(
            container(
                studio_text(format!("+{overflow}"))
                    .size(type_scale::LABEL)
                    .line_height(iced::Pixels(type_scale::LINE))
                    .tabular()
                    .color(Studio::TEXT_FAINT),
            )
            .padding([0, 2]),
        );
    }
    row.into()
}

/// A tag (`DESIGN.md`: small, neutral): 18 px, radius 4, 11 px
/// `TEXT_SECONDARY` on `CONTROL` (`TEXT_PRIMARY` under the pointer); the
/// tag the library is filtered by on `SEGMENT_SELECTED` in `TEXT_PRIMARY`.
/// One line: a long tag is ellipsized.
fn tag_chip(label: &str, active: bool) -> Element<'_, Message> {
    button(
        container(
            studio_text(label)
                .size(type_scale::LABEL)
                .line_height(iced::Pixels(type_scale::LINE))
                .ellipsis(1),
        )
        .center_y(Length::Fill),
    )
    .height(TAG_HEIGHT)
    .padding([0, 6])
    .clip(true)
    .style(move |_, status| {
        let hovered = matches!(
            status,
            iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed
        );
        iced::widget::button::Style {
            background: Some(iced::Background::Color(if active {
                Studio::SEGMENT_SELECTED
            } else if hovered {
                Studio::CONTROL_ACTIVE
            } else {
                Studio::CONTROL
            })),
            text_color: if active || hovered {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_SECONDARY
            },
            border: iced::Border {
                radius: radius::SMALL.into(),
                ..iced::Border::default()
            },
            shadow: iced::Shadow::default(),
            snap: true,
        }
    })
    // The tag takes the tap: it does not also open the card.
    .on_press(Message::TapTag(label.to_string()))
    .into()
}

/// A centred icon, heading and line (the empty and error states, as the
/// library's), with an optional action under them.
fn state_message<'a>(
    icon: Icon,
    heading: &'a str,
    line: &'a str,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut content = column![
        icons::icon(icon, 32.0, Studio::TEXT_DISABLED),
        Space::new().height(14),
        studio_text(heading)
            .size(type_scale::HEADING)
            .line_height(iced::Pixels(20.0))
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY),
        Space::new().height(6),
        studio_text(line)
            .size(type_scale::BODY)
            .line_height(iced::Pixels(18.0))
            .color(Studio::TEXT_SECONDARY)
            .width(380)
            .align_x(Horizontal::Center),
    ]
    .align_x(Horizontal::Center);
    if let Some(action) = action {
        content = content.push(Space::new().height(18)).push(action);
    }
    center(content).into()
}

/// Shown when the technique list fails to load — distinct from the empty
/// state so a read error is never mistaken for a lost library. Offers a
/// Retry that re-runs the load.
fn error_state<'a>() -> Element<'a, Message> {
    state_message(
        Icon::TriangleAlert,
        "Could not load your techniques",
        "Something went wrong reading your library. Your data is safe.",
        Some(
            studio_button("Retry", Message::Retry)
                .icon(Icon::RefreshCw)
                .into(),
        ),
    )
}

/// No techniques yet, with a New technique button under the line (the
/// toolbar's + is easy to miss on an empty page).
fn empty_state<'a>() -> Element<'a, Message> {
    state_message(
        Icon::Library,
        "Build your technique library",
        "Collect the techniques you train, in your own words. Add a technique, tag it \
         however your method does, and attach audio or video examples.",
        Some(
            studio_button(NEW_TECHNIQUE, Message::NewTechnique)
                .icon(Icon::Plus)
                .into(),
        ),
    )
}
