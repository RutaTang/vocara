//! Port of `ui/technique_library/technique_library_screen.dart`: the
//! technique library home — a flat pool of vocal techniques browsed by a tag
//! filter plus search. Framework agnostic: tags (including framework tags
//! like EVT/CVT) are the only organizing axis, and a technique can carry
//! many.
//!
//! The page: a header (back, title, then — once there is something to
//! filter — the search box and the tag filter, and "New technique" flush
//! right), and below it the spinner, the read error with Retry, the empty
//! state, "No techniques match this filter." or the grid of cards. A card
//! opens its technique; hovered, it shows its trash (delete, after a
//! confirmation); its tag chips filter the library.
//!
//! The view model's work runs off the UI thread ([`task`]); what it reports
//! comes back as [`Message::Landed`]. The Dart screen's own `await`s keep
//! their order: a create that finished opens its technique (or says it
//! could not), a delete that finished says whether it failed and lets the
//! next confirmed one run, and the return from a technique's page reloads
//! the list and opens card taps again.
//!
//! A card's border turns coral while the pointer is over it, over 120 ms,
//! linear (the Dart `AnimatedContainer`).
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

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, center, column, container, hover, responsive, row, scrollable, stack, text_input,
    Column, Row, Space,
};
use iced::{Background, Border, Element, Length, Padding, Shadow, Task, Theme};
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
use crate::ui::core::theme::{self, alpha, bare_button, font, Studio};
use crate::ui::core::widgets::animated::{animated_container, ink_hover, Decoration};
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::fading_scrollbar::Fading;
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::snack::{fixed_bar, SnackEvent, SnackQueue, SnackStyle};
use crate::ui::{Screen, SnackBar, Update};

/// The snack bar after a create that did not happen.
pub const CREATE_FAILED: &str = "Couldn't create the technique.";
/// The snack bar after a delete that did not happen.
pub const DELETE_FAILED: &str = "Couldn't delete the technique.";

/// How long a snack bar shows (Material's default, 4 s).
pub const SNACK_DURATION: Duration = Duration::from_millis(4000);

/// The grid's padding (all round).
pub const GRID_PADDING: f32 = 24.0;
/// How long a card's border takes to change (`AnimatedContainer`).
pub const CARD_ANIMATION: Duration = Duration::from_millis(120);

/// The widest a card gets (`maxCrossAxisExtent`).
pub const CARD_MAX_WIDTH: f32 = 300.0;
/// Every card's height (`mainAxisExtent`).
pub const CARD_HEIGHT: f32 = 168.0;
/// The space between cards, both ways.
pub const CARD_SPACING: f32 = 16.0;
/// At most this many tags show on a card, then a "+N" marker.
pub const MAX_CHIPS: usize = 3;

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

    fn header(&self) -> Element<'_, Message> {
        let back = button(
            container(icons::icon(Icon::ChevronLeft, 20.0, Studio::TEXT_SECONDARY))
                .center_x(32)
                .center_y(32),
        )
        .padding(0)
        .style(bare_button)
        .on_press(Message::Back);
        let mut bar = row![
            back,
            Space::new().width(6),
            studio_text("Technique library")
                .size(18)
                .font(font::BOLD)
                .letter_spacing(0.3)
                .color(Studio::TEXT_PRIMARY),
            // The space absorbs the slack so the search + tag filter + New
            // technique cluster sits flush right. The tag filter is a
            // dropdown (not inline chips) so it stays compact no matter how
            // many tags.
            Space::new().width(Length::Fill),
        ]
        .height(Length::Fill)
        .align_y(Vertical::Center);
        // Search + tag filter only appear once there's something to filter.
        if !self.vm.all().is_empty() {
            bar = bar
                .push(search_box(self.vm.query()))
                .push(Space::new().width(10))
                .push(self.tag_filter())
                .push(Space::new().width(12));
        }
        bar = bar.push(
            studio_button("New technique", Message::NewTechnique)
                .icon(Icon::Plus)
                .variant(StudioButtonVariant::Accent),
        );
        column![
            container(bar)
                .width(Length::Fill)
                .height(55)
                .padding(Padding {
                    left: 12.0,
                    right: 16.0,
                    ..Padding::ZERO
                })
                .style(theme::fill(Studio::SURFACE)),
            rule(),
        ]
        .into()
    }

    /// The tag filter: a compact dropdown button showing the active filter,
    /// which opens an anchored, scrollable menu of all tags. Scales to any
    /// number of tags without widening the header.
    fn tag_filter(&self) -> Element<'_, Message> {
        let active = self.vm.active_tag().is_some();
        let label = match self.vm.active_tag() {
            None => "All tags",
            Some(UNTAGGED_FILTER) => "Untagged",
            Some(tag) => tag,
        };
        let (fill, border, icon, fg) = if active {
            (
                alpha(Studio::ACCENT, 0.16),
                alpha(Studio::ACCENT, 0.55),
                Studio::ACCENT_BRIGHT,
                Studio::ACCENT_BRIGHT,
            )
        } else {
            (
                Studio::SURFACE_RAISED,
                Studio::BORDER,
                Studio::TEXT_SECONDARY,
                Studio::TEXT_PRIMARY,
            )
        };
        let content = row![
            icons::icon(Icon::Tag, 13.0, icon),
            Space::new().width(7),
            container(
                studio_text(label)
                    .size(13)
                    .font(font::SEMIBOLD)
                    .color(fg)
                    .ellipsis(1),
            )
            .max_width(150),
            Space::new().width(6),
            icons::icon(Icon::ChevronDown, 14.0, Studio::TEXT_SECONDARY),
        ]
        .align_y(Vertical::Center);
        // 11 / 8 of padding inside the 1 px border (a decoration's border
        // pads its child; an iced border does not).
        let trigger = button(
            container(content)
                .height(Length::Fill)
                .align_y(Vertical::Center),
        )
        .height(34)
        .padding(Padding {
            left: 12.0,
            right: 9.0,
            ..Padding::ZERO
        })
        .style(move |_: &Theme, _| button::Style {
            background: Some(Background::Color(fill)),
            text_color: fg,
            border: Border {
                color: border,
                width: 1.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        })
        .on_press(Message::OpenTagMenu);
        let menu = self.tag_menu.then(|| self.tag_menu());
        Anchored::new(trigger, menu, Message::DismissTagMenu).into()
    }

    /// The tag filter's menu: "All tags", "Untagged" when some technique has
    /// no tag, then every tag with its count.
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
            // `Divider(height: 9, thickness: 1)`.
            items = items.push(container(rule()).height(9).align_y(Vertical::Center));
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
        container(scrollable(items).style(theme::no_scrollbar))
            .width(240)
            .max_height(440)
            .padding(1)
            .style(theme::menu)
            .clip(true)
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
            return center(
                text("No techniques match this filter.")
                    .size(13)
                    .color(Studio::TEXT_FAINT),
            )
            .into();
        }
        grid(vm)
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
        Step::effect(reload).into_update()
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

/// A full-width 1 px line in `Studio.border`.
fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(Studio::BORDER))
        .into()
}

/// The search box: 240 × 34, a search glyph before the field.
fn search_box(query: &str) -> Element<'_, Message> {
    container(
        row![
            icons::icon(Icon::Search, 14.0, Studio::TEXT_FAINT),
            Space::new().width(8),
            text_input("Search techniques", query)
                .id(SEARCH)
                .on_input(Message::Query)
                .size(13)
                .line_height(1.0)
                .padding(0)
                .style(theme::studio_input),
        ]
        .align_y(Vertical::Center),
    )
    .width(240)
    .height(34)
    // 10 inside the 1 px border.
    .padding([0, 11])
    .align_y(Vertical::Center)
    .style(theme::inset(8.0))
    .into()
}

/// One row in the tag menu, styled like the app's other studio menus
/// (full-bleed hover). The active filter is shown by its accent label
/// color; the count stays pinned to the right so every row's count lines
/// up.
fn menu_item<'a>(
    label: String,
    count: usize,
    active: bool,
    on_tap: Message,
) -> Element<'a, Message> {
    let color = if active {
        Studio::ACCENT
    } else {
        Studio::TEXT_PRIMARY
    };
    let row = button(
        row![
            studio_text(label)
                .size(13)
                .font(if active {
                    font::SEMIBOLD
                } else {
                    font::REGULAR
                })
                .color(color)
                .width(Length::Fill)
                .ellipsis(1),
            Space::new().width(10),
            text(count.to_string()).size(12).color(if active {
                Studio::ACCENT
            } else {
                Studio::TEXT_FAINT
            }),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding([9, 14])
    .style(bare_button)
    .on_press(on_tap);
    // An `InkWell(hoverColor: Studio.hover)`.
    ink_hover(row, Studio::HOVER, 0.0)
}

/// How the grid lays out at a content `width`: the column count and the
/// card width (`SliverGridDelegateWithMaxCrossAxisExtent`: as many columns
/// as keep each card at most 300 wide).
pub fn grid_columns(width: f32) -> (usize, f32) {
    let extent = (width - 2.0 * GRID_PADDING).max(0.0);
    let columns = ((extent / (CARD_MAX_WIDTH + CARD_SPACING)).ceil() as usize).max(1);
    let usable = (extent - CARD_SPACING * (columns - 1) as f32).max(0.0);
    (columns, usable / columns as f32)
}

/// The cards, in rows of [`grid_columns`], scrolling under a 24 px margin.
fn grid(vm: &TechniqueLibraryViewModel) -> Element<'_, Message> {
    responsive(move |size| {
        let (columns, width) = grid_columns(size.width);
        let filtered = vm.filtered();
        let mut rows = Column::new().spacing(CARD_SPACING).padding(GRID_PADDING);
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
        scrollable(rows)
            .width(Length::Fill)
            .height(Length::Fill)
            .fading()
            .into()
    })
    .into()
}

/// A technique's card: at rest, and — while the pointer is over it — with
/// its trash and its coral border, which comes in over 120 ms.
fn card<'a>(t: &'a Technique, vm: &TechniqueLibraryViewModel) -> Element<'a, Message> {
    animated_container(
        hover(card_face(t, vm, false), card_face(t, vm, true)),
        CARD_ANIMATION,
    )
    .border_above()
    .style(|hovered| {
        let border = if hovered {
            alpha(Studio::ACCENT, 0.6)
        } else {
            Studio::BORDER
        };
        Decoration::boxed(Studio::PANEL, 10.0, border, 1.0)
    })
    .into()
}

fn card_face<'a>(
    t: &'a Technique,
    vm: &TechniqueLibraryViewModel,
    hovered: bool,
) -> Element<'a, Message> {
    let mut title = row![studio_text(t.name.as_str())
        .size(16)
        .font(font::SEMIBOLD)
        .color(Studio::TEXT_PRIMARY)
        .width(Length::Fill)
        .ellipsis(1)]
    .align_y(Vertical::Center);
    if !t.media.is_empty() {
        let kind = if t.media.iter().any(|m| m.is_video()) {
            Icon::Video
        } else {
            Icon::Music2
        };
        title = title
            .push(Space::new().width(8))
            .push(icons::icon(kind, 13.0, Studio::TEXT_FAINT));
        if t.media.len() > 1 {
            title = title.push(Space::new().width(2)).push(
                text(t.media.len().to_string())
                    .size(11)
                    .font(font::SEMIBOLD)
                    .color(Studio::TEXT_FAINT),
            );
        }
    }
    if hovered {
        title = title.push(Space::new().width(8)).push(
            button(icons::icon(Icon::Trash2, 15.0, Studio::TEXT_FAINT))
                .padding(0)
                .style(bare_button)
                .on_press(Message::Delete {
                    id: t.id.clone(),
                    name: t.name.clone(),
                }),
        );
    }
    let subtitle = dart_string::trim(&t.summary);
    let summary: Element<'a, Message> = if subtitle.is_empty() {
        Space::new().into()
    } else {
        // Three lines at most, the third ellipsized.
        studio_text(subtitle)
            .size(12.5)
            .line_height(1.35)
            .color(Studio::TEXT_SECONDARY)
            .width(Length::Fill)
            .ellipsis(3)
            .into()
    };
    let mut content = column![
        title,
        Space::new().height(6),
        // The summary absorbs the slack and is clipped, so the card content
        // can never overflow its fixed grid-cell height regardless of
        // summary length or how many rows the tags wrap to.
        container(summary)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true),
    ];
    if !t.tags.is_empty() {
        content = content
            .push(Space::new().height(8))
            .push(tag_chips(&t.tags, vm));
    }
    // 16 of padding inside the 1 px border (the card's, drawn over the
    // face).
    button(
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(17)
            .clip(true),
    )
    .padding(0)
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_: &Theme, _| button::Style {
        background: Some(Background::Color(Studio::PANEL)),
        text_color: Studio::TEXT_PRIMARY,
        border: Border {
            radius: 10.0.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: false,
    })
    .on_press(Message::Open(t.id.clone()))
    .into()
}

/// The small tag chips on a card: up to three, then a "+N" overflow marker.
/// Tapping one filters the library to that tag.
fn tag_chips<'a>(tags: &'a [String], vm: &TechniqueLibraryViewModel) -> Element<'a, Message> {
    let mut chips = Row::new().spacing(6).width(Length::Fill);
    for tag in tags.iter().take(MAX_CHIPS) {
        chips = chips.push(mini_tag(tag, vm.is_tag_active(tag)));
    }
    let overflow = tags.len().saturating_sub(MAX_CHIPS);
    if overflow > 0 {
        chips = chips.push(
            text(format!("+{overflow}"))
                .size(11)
                .color(Studio::TEXT_FAINT),
        );
    }
    chips.wrap().vertical_spacing(6).into()
}

fn mini_tag(label: &str, active: bool) -> Element<'_, Message> {
    let (fill, border, fg) = if active {
        (
            alpha(Studio::ACCENT, 0.16),
            alpha(Studio::ACCENT, 0.45),
            Studio::ACCENT_BRIGHT,
        )
    } else {
        (
            Studio::SURFACE_RAISED,
            Studio::BORDER,
            Studio::TEXT_SECONDARY,
        )
    };
    // One line: the wrap bounds a chip to the card's width, and a long tag
    // wrapped inside its chip grew the card past its fixed height.
    button(
        studio_text(label)
            .size(10.5)
            .font(font::SEMIBOLD)
            .color(fg)
            .ellipsis(1),
    )
    // 7 / 2 inside the 1 px border.
    .padding([3, 8])
    .clip(true)
    .style(move |_: &Theme, _| button::Style {
        background: Some(Background::Color(fill)),
        text_color: fg,
        border: Border {
            color: border,
            width: 1.0,
            radius: 5.0.into(),
        },
        shadow: Shadow::default(),
        snap: false,
    })
    // The chip takes the tap: it does not also open the card.
    .on_press(Message::TapTag(label.to_string()))
    .into()
}

/// Shown when the technique list fails to load — distinct from the empty
/// state so a read error is never mistaken for a lost library. Offers a
/// Retry that re-runs the load.
fn error_state<'a>() -> Element<'a, Message> {
    center(
        column![
            icons::icon(Icon::TriangleAlert, 48.0, Studio::TEXT_FAINT),
            Space::new().height(16),
            text("Could not load your techniques")
                .size(20)
                .font(font::SEMIBOLD)
                .color(Studio::TEXT_PRIMARY),
            Space::new().height(8),
            text("Something went wrong reading your library. Your data is safe.")
                .size(13)
                .align_x(Horizontal::Center)
                .color(Studio::TEXT_SECONDARY),
            Space::new().height(20),
            studio_button("Retry", Message::Retry).icon(Icon::RefreshCw),
        ]
        .align_x(Horizontal::Center),
    )
    .into()
}

fn empty_state<'a>() -> Element<'a, Message> {
    center(
        column![
            container(icons::icon(Icon::Library, 26.0, Studio::ACCENT))
                .center_x(56)
                .center_y(56)
                .style(|_| theme::boxed(Some(alpha(Studio::ACCENT, 0.1)), 14.0, None)),
            Space::new().height(16),
            text("Build your technique library")
                .size(20)
                .font(font::SEMIBOLD)
                .color(Studio::TEXT_PRIMARY),
            Space::new().height(8),
            text(
                "Collect the techniques you train, in your own words. Add a technique, tag it \
                 however your method does, and attach audio or video examples.",
            )
            .size(13)
            .line_height(1.45)
            .width(380)
            .align_x(Horizontal::Center)
            .color(Studio::TEXT_SECONDARY),
        ]
        .align_x(Horizontal::Center),
    )
    .into()
}
