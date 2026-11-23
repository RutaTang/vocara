//! Port of `ui/home/widgets/home_screen.dart`: the dark collection of song
//! practices — the header (New practice, Techniques, Settings), the update
//! banner, and the practice grid with its loading, empty and error states.
//!
//! The view model ([`HomeViewModel`]) holds the list and its commands; this
//! screen runs their effects off the UI thread and lands their outcomes, and
//! owns what Dart's `_HomeScreenState` owned: the open flow and its
//! double-tap guard, the delete queue, the failure toast, and the update
//! auto-check timer.
//!
//! Dart's timers (`Timer`, `Future.delayed`) are [`Timer`]s here: the screen
//! lists the delays it is waiting out and [`Screen::subscription`] runs each
//! as a one-shot subscription, so the page leaving the stack cancels them,
//! as `dispose` cancelled the Dart timer (and as `mounted` dropped a delayed
//! continuation). Remote-applied events arrive the same way, from the
//! library's broadcast, for as long as the page is on the stack (covered or
//! not, as the Dart view model listened).
//!
//! Two Flutter overlays draw inside the page here, under the window drag
//! bar: the "Loading practice" dialog (a non-dismissible `showDialog` with
//! no result to give, which the app's dialog stack cannot close on a timer)
//! and the SnackBar. The studio dialogs (new practice, delete confirmation,
//! update download) go through the app's dialog stack and cover the whole
//! window, as Flutter's dialog routes did.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use iced::alignment::{Horizontal, Vertical};
use iced::futures::{stream, Stream, StreamExt};
use iced::widget::{
    button, center, column, container, opaque, responsive, row, scrollable, stack, text, tooltip,
    Space,
};
use iced::{Color, Element, Length, Padding, Shadow, Subscription, Task, Theme, Vector};
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::update_check_service::UpdateCheckService;
use vocara_store::sync::sync_engine::RemoteApplied;

use super::view_model::{Effect, HomeViewModel, Landing};
use super::widgets::new_practice_dialog::new_practice_dialog;
use super::widgets::practice_project_card::practice_project_card;
use super::widgets::update_notice::{self, update_notice_banner};
use crate::app::library::RemoteAppliedBroadcast;
use crate::router::Route;
use crate::services::Services;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::{Screen, Update};

/// How long after home mounts the once-per-run update auto-check fires: a
/// beat later, so it never competes with first paint.
pub const UPDATE_CHECK_DELAY: Duration = Duration::from_secs(5);

/// How long the "Loading practice" dialog holds before the push (so it
/// never just flashes).
pub const OPENING_HOLD: Duration = Duration::from_millis(1500);

/// How long a failure toast stays up.
pub const SNACK_DURATION: Duration = Duration::from_millis(2800);

/// The practice grid's geometry (`SliverGridDelegateWithMaxCrossAxisExtent`
/// and the `GridView`'s padding).
const GRID_MAX_CARD_WIDTH: f32 = 280.0;
const GRID_CARD_HEIGHT: f32 = 150.0;
const GRID_SPACING: f32 = 16.0;
const GRID_PADDING: f32 = 24.0;

/// The toasts.
const CREATE_FAILED: &str = "Could not create the practice. Please try again.";
const DELETE_FAILED: &str = "Could not delete the practice. Please try again.";

/// A delay the screen is waiting out (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timer {
    /// The update auto-check, [`UPDATE_CHECK_DELAY`] after mount.
    UpdateCheck,
    /// The opening dialog's hold before open `n` pushes its practice.
    Opening(u64),
    /// Toast `n`'s display time.
    Snack(u64),
}

impl Timer {
    /// How long it waits.
    pub fn delay(self) -> Duration {
        match self {
            Timer::UpdateCheck => UPDATE_CHECK_DELAY,
            Timer::Opening(_) => OPENING_HOLD,
            Timer::Snack(_) => SNACK_DURATION,
        }
    }

    /// What it sends when it fires.
    pub fn message(self) -> Message {
        match self {
            Timer::UpdateCheck => Message::UpdateCheckDue,
            Timer::Opening(n) => Message::OpeningElapsed(n),
            Timer::Snack(n) => Message::SnackExpired(n),
        }
    }
}

/// What the home screen reacts to.
#[derive(Debug, Clone)]
pub enum Message {
    /// The header's "New practice": ask for a title.
    NewPractice,
    /// The new-practice prompt closed: the trimmed title, or nothing.
    Titled(Option<String>),
    /// The header's "Techniques".
    OpenTechniques,
    /// The header's settings button.
    OpenSettings,
    /// A card's tap: open practice `id`.
    Open(String),
    /// The opening dialog's hold for open `n` is over: push the practice.
    OpeningElapsed(u64),
    /// A card's trash: confirm deleting it.
    Delete { id: String, title: String },
    /// The delete confirmation closed.
    DeleteConfirmed { id: String, confirmed: bool },
    /// The error state's Retry.
    Retry,
    /// A command's outcome came back.
    Landed(Landing),
    /// Nothing of practice `id` is landing any more (a save that outlived
    /// its screen has settled).
    Settled(String),
    /// Sync applied remote rows.
    RemoteApplied(RemoteApplied),
    /// Time for the update auto-check.
    UpdateCheckDue,
    /// The auto-check finished: the banner reads its outcome.
    UpdateChecked,
    /// The update banner's buttons.
    Banner(update_notice::Message),
    /// The update download dialog closed.
    DownloadDialogClosed,
    /// Toast `n` has been up long enough.
    SnackExpired(u64),
}

/// An open in flight: the opening dialog, then the pushed practice until it
/// is popped.
#[derive(Debug, Clone)]
struct Opening {
    /// Which open it is (its timer's key).
    seq: u64,
    /// The practice it opens.
    id: String,
    /// Whether the opening dialog still shows (the push has not happened).
    dialog: bool,
}

/// A failure toast.
#[derive(Debug, Clone)]
struct Snack {
    seq: u64,
    text: String,
}

/// The home page (see the module docs).
pub struct HomeScreen {
    vm: HomeViewModel,
    updates: Arc<UpdateCheckService>,
    remote_applied: Arc<RemoteAppliedBroadcast>,
    finalizer: Arc<RecordingFinalizer>,

    /// Guards the open flow against a double-tap: without it a second tap
    /// shows a second dialog and the stale pop lands on the just-pushed
    /// detail route, bouncing the user straight back to Home. Held from the
    /// tap until the pushed practice is popped (Dart's awaited push).
    opening: Option<Opening>,
    opens: u64,

    /// The once-per-app-run update auto-check, deferred a beat after home
    /// mounts so it never competes with first paint. The service itself
    /// makes repeat mounts a no-op; this timer only spaces out the first
    /// one.
    update_check_pending: bool,

    snack: Option<Snack>,
    snacks: u64,

    /// Deletes confirmed while another one still ran, in the order they were
    /// confirmed: each runs once the one before it is done.
    queued_deletes: VecDeque<String>,
}

impl std::fmt::Debug for HomeScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeScreen")
            .field("vm", &self.vm)
            .field("opening", &self.opening)
            .field("snack", &self.snack)
            .finish_non_exhaustive()
    }
}

/// Runs a view-model effect on a blocking thread; its outcome comes back as
/// [`Message::Landed`].
fn run(effect: Effect) -> Task<Message> {
    match effect {
        Effect::Load(run) => run.into_task(|o| Message::Landed(Landing::Loaded(o))),
        Effect::Create(run) => run.into_task(|o| Message::Landed(Landing::Created(Box::new(o)))),
        Effect::Delete { id, run } => {
            run.into_task(move |outcome| Message::Landed(Landing::Deleted { id, outcome }))
        }
    }
}

fn run_all(effects: impl IntoIterator<Item = Option<Effect>>) -> Task<Message> {
    Task::batch(effects.into_iter().flatten().map(run))
}

impl HomeScreen {
    /// The home page over the app's services: its view model starts the
    /// first list read at once, and the update auto-check timer starts.
    pub fn new(services: &Services) -> (Self, Task<Message>) {
        let (vm, first) = HomeViewModel::new(services.song_repo.clone());
        let screen = HomeScreen {
            vm,
            updates: services.update_check.clone(),
            remote_applied: services.library.remote_applied.clone(),
            finalizer: RecordingFinalizer::instance(),
            opening: None,
            opens: 0,
            update_check_pending: true,
            snack: None,
            snacks: 0,
            queued_deletes: VecDeque::new(),
        };
        (screen, run(first))
    }

    /// The view model.
    pub fn vm(&self) -> &HomeViewModel {
        &self.vm
    }

    /// The delays the screen is waiting out now.
    pub fn timers(&self) -> Vec<Timer> {
        let mut timers = Vec::new();
        if self.update_check_pending {
            timers.push(Timer::UpdateCheck);
        }
        if let Some(o) = self.opening.as_ref().filter(|o| o.dialog) {
            timers.push(Timer::Opening(o.seq));
        }
        if let Some(s) = &self.snack {
            timers.push(Timer::Snack(s.seq));
        }
        timers
    }

    /// The practice being opened (the open guard), if any.
    pub fn opening(&self) -> Option<&str> {
        self.opening.as_ref().map(|o| o.id.as_str())
    }

    /// The toast on screen, if any.
    pub fn snack(&self) -> Option<&str> {
        self.snack.as_ref().map(|s| s.text.as_str())
    }

    /// Opens practice `id`: the "Loading practice" dialog over the library
    /// for a beat ([`OPENING_HOLD`]), then the push. A rapid second tap
    /// while one open is in flight is ignored.
    fn open_detail(&mut self, id: String) {
        if self.opening.is_some() {
            return; // ignore a rapid second tap while one open is in flight
        }
        self.opens += 1;
        self.opening = Some(Opening {
            seq: self.opens,
            id,
            dialog: true,
        });
    }

    /// Surfaces create/delete failures as a transient toast the moment the
    /// command settles (mirrors the detail screen's SnackBar pattern).
    /// Without this, a failed mutation is silently dropped and the UI
    /// diverges from SQLite.
    fn on_mutation_result(&mut self) -> Vec<Option<Effect>> {
        let mut effects = Vec::new();
        if self.vm.create_project().error() {
            self.show_error(CREATE_FAILED);
            effects.push(self.vm.clear_create_result());
        }
        if self.vm.delete_project().error() {
            self.show_error(DELETE_FAILED);
            effects.push(self.vm.clear_delete_result());
        }
        effects
    }

    /// Replaces any toast on screen with `message`.
    fn show_error(&mut self, message: &str) {
        self.snacks += 1;
        self.snack = Some(Snack {
            seq: self.snacks,
            text: message.to_string(),
        });
    }

    fn landed(&mut self, landing: Landing) -> Update<Message> {
        let created = match &landing {
            Landing::Created(outcome) => match &**outcome {
                Ok(practice) => Some(practice.id.clone()),
                Err(_) => None,
            },
            _ => None,
        };
        let mutation = !matches!(landing, Landing::Loaded(_));
        let mut effects = vec![self.vm.land(landing)];
        if mutation {
            effects.extend(self.on_mutation_result());
        }
        // One delete at a time: the next one confirmed meanwhile runs once
        // this one is done.
        if !self.vm.delete_project().running() {
            if let Some(id) = self.queued_deletes.pop_front() {
                effects.push(self.vm.execute_delete(id));
            }
        }
        // A practice created from the prompt opens at once.
        if let Some(id) = created {
            self.open_detail(id);
        }
        Update::task(run_all(effects))
    }

    fn header(&self) -> Element<'_, Message> {
        let bar = row![
            text("Vocara")
                .size(18)
                .font(font::BOLD)
                .color(Studio::TEXT_PRIMARY),
            Space::new().width(Length::Fill),
            studio_button("New practice", Message::NewPractice)
                .icon(Icon::Plus)
                .variant(StudioButtonVariant::Accent),
            Space::new().width(10),
            studio_button("Techniques", Message::OpenTechniques).icon(Icon::Library),
            Space::new().width(10),
            header_icon_button(Icon::Settings, "Settings", Message::OpenSettings),
        ]
        .align_y(Vertical::Center);
        // 56 tall on `Studio.surface`, `Border(bottom: Studio.border)`: the
        // line takes the last pixel.
        container(column![
            container(bar)
                .padding(Padding::from([0, 24]))
                .height(55)
                .align_y(Vertical::Center),
            container(Space::new())
                .width(Length::Fill)
                .height(1)
                .style(theme::fill(Studio::BORDER)),
        ])
        .width(Length::Fill)
        .height(56)
        .style(theme::fill(Studio::SURFACE))
        .into()
    }

    fn body(&self) -> Element<'_, Message> {
        let vm = &self.vm;
        if vm.load().running() && vm.projects().is_empty() {
            return center(spinner(36.0, 4.0, Studio::TEXT_SECONDARY)).into();
        }
        // A failed list read must read as an error (with Retry), never
        // collapse into the empty state — otherwise a load failure looks
        // like "No practices yet".
        if vm.load().error() && vm.projects().is_empty() {
            return error_state();
        }
        if vm.projects().is_empty() {
            return empty_state();
        }
        practice_grid(vm.projects())
    }
}

impl Screen for HomeScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::NewPractice => {
                Update::dialog(new_practice_dialog(), |r| Message::Titled(r.text()))
            }
            Message::Titled(None) => Update::none(),
            Message::Titled(Some(title)) => run_all([self.vm.execute_create(title)]).into(),
            Message::OpenTechniques => Update::push(Route::Techniques),
            Message::OpenSettings => Update::push(Route::Settings),
            Message::Open(id) => {
                self.open_detail(id);
                Update::none()
            }
            Message::OpeningElapsed(seq) => match &mut self.opening {
                Some(o) if o.seq == seq && o.dialog => {
                    // Close the dialog, then enter.
                    o.dialog = false;
                    Update::push(Route::Song { id: o.id.clone() })
                }
                _ => Update::none(),
            },
            Message::Delete { id, title } => Update::dialog(
                ConfirmDialog::new(format!("Delete \"{title}\"?"))
                    .body("This removes the practice and all of its notes. This cannot be undone."),
                move |r| Message::DeleteConfirmed {
                    id,
                    confirmed: r.confirmed(),
                },
            ),
            Message::DeleteConfirmed {
                confirmed: false, ..
            } => Update::none(),
            Message::DeleteConfirmed {
                id,
                confirmed: true,
            } => {
                // One delete at a time, and the command drops a call made
                // while it runs: one confirmed while another still ran was
                // ignored, its card left with no word said. It runs once that
                // one is done instead (as the technique library's do).
                if self.vm.delete_project().running() {
                    self.queued_deletes.push_back(id);
                    Update::none()
                } else {
                    run_all([self.vm.execute_delete(id)]).into()
                }
            }
            Message::Retry => run_all([self.vm.execute_load()]).into(),
            Message::Landed(landing) => self.landed(landing),
            // A load running now may have read before the landing: refresh
            // owes one after it.
            Message::Settled(_) => run_all([self.vm.refresh()]).into(),
            Message::RemoteApplied(event) => run_all([self.vm.on_remote_applied(&event)]).into(),
            Message::UpdateCheckDue => {
                if !self.update_check_pending {
                    return Update::none();
                }
                self.update_check_pending = false;
                // Fire-and-forget: the check is non-blocking and swallows
                // every failure.
                let updates = Arc::clone(&self.updates);
                Task::perform(crate::util::blocking(move || updates.auto_check()), |_| {
                    Message::UpdateChecked
                })
                .into()
            }
            Message::UpdateChecked | Message::DownloadDialogClosed => Update::none(),
            Message::Banner(message) => match update_notice::update(&self.updates, message) {
                Some(dialog) => Update::dialog(dialog, |_| Message::DownloadDialogClosed),
                None => Update::none(),
            },
            Message::SnackExpired(seq) => {
                if self.snack.as_ref().is_some_and(|s| s.seq == seq) {
                    self.snack = None;
                }
                Update::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let page = column![
            self.header(),
            update_notice_banner(&self.updates).map(Message::Banner),
            container(self.body())
                .width(Length::Fill)
                .height(Length::Fill),
        ];
        let mut layers = stack![container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(snack) = &self.snack {
            layers = layers.push(snack_bar(&snack.text));
        }
        if self.opening.as_ref().is_some_and(|o| o.dialog) {
            layers = layers.push(opening_dialog());
        }
        layers.into()
    }

    fn subscription(&self, _on_top: bool) -> Subscription<Message> {
        let mut subscriptions: Vec<Subscription<Message>> = self
            .timers()
            .into_iter()
            .map(|timer| Subscription::run_with(timer, timer_stream))
            .collect();
        // Freshness reloads when sync applies remote rows; with no sync
        // configured the stream simply never fires.
        subscriptions.push(
            Subscription::run_with(Feed(Arc::clone(&self.remote_applied)), feed_stream)
                .map(Message::RemoteApplied),
        );
        Subscription::batch(subscriptions)
    }

    /// The pushed practice was popped (Dart's awaited push completed).
    fn returned(&mut self, _services: &Services) -> Update<Message> {
        let opening = match self.opening.take() {
            Some(o) if !o.dialog => o,
            other => {
                // Not the practice this screen pushed (Techniques and
                // Settings were not awaited).
                self.opening = other;
                return Update::none();
            }
        };
        // A take stopped there (or an import, a delete) can outlive the
        // screen, and the list read now may not hold it: read it again once
        // it has landed, without holding the open guard meanwhile. Asked
        // before this read: one landing while it reads may not be in it, and
        // is no longer landing after.
        let landing = self.finalizer.is_landing(&opening.id);
        let mut update = Update::task(run_all([self.vm.refresh()]));
        if landing {
            let finalizer = Arc::clone(&self.finalizer);
            let id = opening.id;
            update = update.and_task(Task::perform(
                crate::util::blocking({
                    let id = id.clone();
                    move || finalizer.settled(&id)
                }),
                move |_| Message::Settled(id),
            ));
        }
        update
    }

    fn dispose(&mut self, _services: &Services) {
        self.vm.dispose();
    }
}

/// A timer's one-shot stream: its message after its delay, then nothing.
fn timer_stream(timer: &Timer) -> impl Stream<Item = Message> {
    let (delay, message) = (timer.delay(), timer.message());
    stream::once(async move {
        tokio::time::sleep(delay).await;
        message
    })
    .chain(stream::pending())
}

/// The library's remote-applied broadcast, as a subscription's identity
/// (the broadcast itself, by address).
#[derive(Clone)]
struct Feed(Arc<RemoteAppliedBroadcast>);

impl Hash for Feed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

fn feed_stream(feed: &Feed) -> impl Stream<Item = RemoteApplied> {
    feed.0.subscribe()
}

/// `_HeaderIconButton`: a bare icon button for the home chrome — 34 px
/// square on `Studio.surfaceRaised`, radius 7, `Studio.border`, a 16 px
/// icon, a click cursor and a tooltip.
fn header_icon_button<'a>(icon: Icon, tip: &'a str, on_tap: Message) -> Element<'a, Message> {
    let target = button(
        container(icons::icon(icon, 16.0, Studio::TEXT_SECONDARY))
            .center_x(34)
            .center_y(34)
            .style(|_: &Theme| boxed(Some(Studio::SURFACE_RAISED), 7.0, Some(Studio::BORDER))),
    )
    .padding(0)
    .style(bare_button)
    .on_press(on_tap);
    tooltip(
        target,
        container(text(tip).size(theme::TOOLTIP_TEXT_SIZE))
            .padding(theme::TOOLTIP_PADDING)
            .style(theme::tooltip),
        tooltip::Position::Bottom,
    )
    // Material's tooltip sits 24 px below the target's centre.
    .gap(24.0 - 34.0 / 2.0)
    .into()
}

/// How many cards fit across `width` (`SliverGridDelegateWithMax
/// CrossAxisExtent`: at most 280 wide each, at least one).
pub fn grid_columns(width: f32) -> usize {
    let count = (width / (GRID_MAX_CARD_WIDTH + GRID_SPACING)).ceil();
    if count.is_finite() && count >= 1.0 {
        count as usize
    } else {
        1
    }
}

/// The card grid: cards at most 280 wide and 150 tall, 16 apart, inside a
/// 24 padding, scrolling vertically.
fn practice_grid(projects: &[PracticeSummary]) -> Element<'_, Message> {
    responsive(move |size| {
        let columns = grid_columns((size.width - 2.0 * GRID_PADDING).max(0.0));
        let mut rows = column![].spacing(GRID_SPACING);
        for chunk in projects.chunks(columns) {
            let mut cells = row![].spacing(GRID_SPACING).height(GRID_CARD_HEIGHT);
            for practice in chunk {
                cells = cells.push(
                    container(practice_project_card(
                        practice,
                        Message::Open(practice.id.clone()),
                        Message::Delete {
                            id: practice.id.clone(),
                            title: practice.title.clone(),
                        },
                    ))
                    .width(Length::Fill)
                    .height(Length::Fill),
                );
            }
            // A short last row keeps the cells as wide as the rest.
            for _ in chunk.len()..columns {
                cells = cells.push(Space::new().width(Length::Fill));
            }
            rows = rows.push(cells);
        }
        scrollable(container(rows).padding(GRID_PADDING).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::scrollbar)
            .into()
    })
    .into()
}

/// A centred icon, heading and line (the empty and error states).
fn state_message<'a>(
    icon: Icon,
    heading: &'a str,
    line: &'a str,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut content = column![
        icons::icon(icon, 48.0, Studio::TEXT_FAINT),
        Space::new().height(16),
        text(heading)
            .size(20)
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY),
        Space::new().height(8),
        text(line)
            .size(13)
            .color(Studio::TEXT_SECONDARY)
            .align_x(Horizontal::Center),
    ]
    .align_x(Horizontal::Center);
    if let Some(action) = action {
        content = content.push(Space::new().height(20)).push(action);
    }
    center(content).into()
}

/// No practices yet. No inline button — the header's "New practice" is the
/// single, always-present way to create one.
fn empty_state<'a>() -> Element<'a, Message> {
    state_message(
        Icon::Music2,
        "No practices yet",
        "Create your first song practice to start recording and taking notes.",
        None,
    )
}

/// Shown when the practice list fails to load — distinct from the empty
/// state so a read error is never mistaken for a lost library. Offers a
/// Retry that re-runs the load.
fn error_state<'a>() -> Element<'a, Message> {
    state_message(
        Icon::TriangleAlert,
        "Could not load your practices",
        "Something went wrong reading your library. Your data is safe.",
        Some(
            studio_button("Retry", Message::Retry)
                .icon(Icon::RefreshCw)
                .into(),
        ),
    )
}

/// A small "loading practice" dialog shown over the library while a project
/// opens (it stays on the library, dimmed, so opening feels like a quick
/// load rather than a jump to a blank screen). Its barrier is not
/// dismissible: it takes every tap.
fn opening_dialog<'a>() -> Element<'a, Message> {
    // Accent disc behind the spinner — reads as a deliberate, branded
    // loader.
    let disc = container(spinner(24.0, 2.6, Studio::ACCENT))
        .padding(14)
        .width(52)
        .height(52)
        .style(|_: &Theme| boxed(Some(alpha(Studio::ACCENT, 0.1)), 26.0, None));
    let card = container(
        column![
            disc,
            Space::new().height(16),
            text("Loading practice")
                .size(13.5)
                .font(font::SEMIBOLD)
                .color(Studio::TEXT_PRIMARY),
        ]
        .align_x(Horizontal::Center),
    )
    .width(168)
    .padding(Padding::from([26, 24]))
    .style(|_: &Theme| container::Style {
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.4),
            offset: Vector::new(0.0, 10.0),
            blur_radius: 28.0,
        },
        ..boxed(Some(Studio::SURFACE_RAISED), 16.0, Some(Studio::BORDER))
    });
    let barrier = opaque(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::fill(alpha(Color::BLACK, 0.28))),
    );
    stack![barrier, center(opaque(card))].into()
}

/// The failure toast: a floating SnackBar 16 in from the page's edges at its
/// bottom, `Studio.surfaceRaised`, radius 8, `Studio.border`, 13 px text.
fn snack_bar(message: &str) -> Element<'_, Message> {
    let bar = container(text(message).size(13).color(Studio::TEXT_PRIMARY))
        .width(Length::Fill)
        .padding(Padding::from([14, 16]))
        .style(|_: &Theme| container::Style {
            shadow: Shadow {
                color: alpha(Color::BLACK, 0.4),
                offset: Vector::new(0.0, 3.0),
                blur_radius: 8.0,
            },
            ..boxed(Some(Studio::SURFACE_RAISED), 8.0, Some(Studio::BORDER))
        });
    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .align_y(Vertical::Bottom)
        .into()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod through_the_app;
