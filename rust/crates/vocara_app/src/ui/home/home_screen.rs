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
//! and the SnackBar. The dialog comes in and goes as its `DialogRoute` did
//! (`confirm_dialog::DialogMotion`): it fades out over the library as the
//! practice slides in, taking no input. The studio dialogs (new practice, delete confirmation,
//! update download) go through the app's dialog stack and cover the whole
//! window, as Flutter's dialog routes did.
//!
//! The toast is the page's own while Home is on top and the app shows none;
//! a page Home pushes takes it over (the app keeps it up over that page,
//! `app::messenger`), and a toast Home shows while the app shows one
//! replaces that one (`clearSnackBars`), as under Dart's app-wide
//! `ScaffoldMessenger`. It fades in and out as Material's floating snack bar
//! did, its 2.8 s counted from the end of its entrance (`ui::snack`).

use std::collections::VecDeque;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::futures::{stream, Stream, StreamExt};
use iced::widget::{button, center, column, container, opaque, responsive, row, stack, Space};
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
use crate::ui::core::motion;
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::confirm_dialog::{barrier, ConfirmDialog, DialogMotion, Fade};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::material_tooltip::material_tooltip;
use crate::ui::core::widgets::slide::inert;
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::snack::{SnackEvent, SnackQueue, SnackStyle, SnackTimer};
use crate::ui::{Screen, SnackBar, Update};

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
}

impl Timer {
    /// How long it waits.
    pub fn delay(self) -> Duration {
        match self {
            Timer::UpdateCheck => UPDATE_CHECK_DELAY,
            Timer::Opening(_) => OPENING_HOLD,
        }
    }

    /// What it sends when it fires.
    pub fn message(self) -> Message {
        match self {
            Timer::UpdateCheck => Message::UpdateCheckDue,
            Timer::Opening(n) => Message::OpeningElapsed(n),
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
    /// A frame while a toast enters or leaves.
    SnackFrame(Instant),
    /// Toast `n` was swiped away.
    SnackSwiped(u64),
    /// A frame while the opening dialog comes in or goes.
    DialogFrame(Instant),
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

    /// The failure toasts (one at a time: each replaces the one up).
    snacks: SnackQueue,

    /// The opening dialog's route animation, from its open until it has
    /// gone.
    loading: Option<DialogMotion>,
    /// The last frame's time, what the dialog's drawing reads.
    now: Instant,

    /// Deletes confirmed while another one still ran, in the order they were
    /// confirmed: each runs once the one before it is done.
    queued_deletes: VecDeque<String>,
}

impl std::fmt::Debug for HomeScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeScreen")
            .field("vm", &self.vm)
            .field("opening", &self.opening)
            .field("snacks", &self.snacks)
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
            snacks: SnackQueue::new(),
            loading: None,
            now: Instant::now(),
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
        timers
    }

    /// The practice being opened (the open guard), if any.
    pub fn opening(&self) -> Option<&str> {
        self.opening.as_ref().map(|o| o.id.as_str())
    }

    /// The toast on screen, if any.
    pub fn snack(&self) -> Option<&str> {
        self.snacks.current().map(|s| s.text.as_str())
    }

    /// The toast's timer, while one is up.
    pub fn snack_timer(&self) -> Option<SnackTimer> {
        self.snacks.timer()
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
        self.loading = Some(DialogMotion::opening(Instant::now()));
    }

    /// The opening dialog's fade, while it is drawn.
    pub fn loading_fade(&self, now: Instant) -> Option<Fade> {
        self.loading.as_ref().map(|m| m.fade(now))
    }

    /// Whether the opening dialog comes in or goes at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.loading.as_ref().is_some_and(|m| m.is_animating(now))
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

    /// Replaces any toast on screen with `message` (`clearSnackBars`, then
    /// `showSnackBar`).
    fn show_error(&mut self, message: &str) {
        let bar = SnackBar::new(
            message,
            SNACK_DURATION,
            true,
            SnackStyle::Floating,
            snack_bar,
        );
        self.snacks.show(bar, Instant::now());
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
            studio_text("Vocara")
                .size(18)
                .font(font::BOLD)
                .letter_spacing(0.3)
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
                    if let Some(loading) = &mut self.loading {
                        loading.close(Instant::now());
                    }
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
                self.snacks.expired(seq, Instant::now());
                Update::none()
            }
            Message::SnackFrame(at) => {
                self.snacks.frame(at);
                Update::none()
            }
            Message::SnackSwiped(seq) => {
                self.snacks.swiped(seq, Instant::now());
                Update::none()
            }
            Message::DialogFrame(at) => {
                self.now = self.now.max(at);
                if self.loading.as_ref().is_some_and(|m| m.is_gone(self.now)) {
                    self.loading = None;
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
        if let Some(snacks) = self.snacks.view() {
            layers = layers.push(snacks.map(snack_message));
        }
        if let Some(fade) = self.loading_fade(self.now) {
            let dialog = opening_dialog(fade);
            // Going, it takes no input.
            let open = self.opening.as_ref().is_some_and(|o| o.dialog);
            layers = layers.push(if open { dialog } else { inert(dialog) });
        }
        layers.into()
    }

    fn subscription(&self, _on_top: bool) -> Subscription<Message> {
        let mut subscriptions: Vec<Subscription<Message>> = self
            .timers()
            .into_iter()
            .map(|timer| Subscription::run_with(timer, timer_stream))
            .collect();
        subscriptions.push(self.snacks.subscription().map(snack_message));
        subscriptions
            .push(motion::frames(self.is_animating(Instant::now())).map(Message::DialogFrame));
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

    /// The toast, for the time it has left; it replaced the ones before it.
    fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        self.snacks.take(Instant::now())
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
    // Material's tooltip sits 24 px below the target's centre.
    material_tooltip(target, tip, 24.0 - 34.0 / 2.0).into()
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
            .fading()
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
/// load rather than a jump to a blank screen), at `fade`. Its barrier is not
/// dismissible: it takes every tap.
fn opening_dialog<'a>(fade: Fade) -> Element<'a, Message> {
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
            studio_text("Loading practice")
                .size(13.5)
                .font(font::SEMIBOLD)
                .letter_spacing(0.2)
                .color(Studio::TEXT_PRIMARY),
        ]
        .align_x(Horizontal::Center),
    )
    .width(168)
    // 26 by 24 inside the card's 1 px border (a Flutter `Container`'s
    // padding includes its decoration's border).
    .padding(Padding::from([27, 25]))
    .style(|_: &Theme| container::Style {
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.4),
            offset: Vector::new(0.0, 10.0),
            blur_radius: 28.0,
        },
        ..boxed(Some(Studio::SURFACE_RAISED), 16.0, Some(Studio::BORDER))
    });
    let barrier = barrier::<Message>(alpha(Color::BLACK, 0.28), fade.barrier, None);
    stack![barrier, center(opaque(fade.card(card)))].into()
}

/// The page's message for what its toasts report.
fn snack_message(event: SnackEvent) -> Message {
    match event {
        SnackEvent::Expired(seq) => Message::SnackExpired(seq),
        SnackEvent::Frame(at) => Message::SnackFrame(at),
        SnackEvent::Swiped(seq) => Message::SnackSwiped(seq),
    }
}

/// The failure toast: a floating SnackBar (set 16 in from the page's edges
/// at its bottom by the queue, `snack::FLOATING_MARGIN`),
/// `Studio.surfaceRaised`, radius 8, `Studio.border`, 13 px text. (The
/// app draws it too, once the page has handed it over.)
fn snack_bar(message: &str) -> Element<'_, Infallible> {
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
    bar.into()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod through_the_app;
