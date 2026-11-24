//! Port of `ui/settings/settings_screen.dart`: global app settings — audio
//! devices and monitoring, library maintenance (repair, diagnostics export),
//! and the About block (version, updates, licenses).
//!
//! The Dart screen kept its state in `_SettingsScreenState` and read the
//! [`AppSettingsService`] and the [`UpdateCheckService`] with
//! `context.watch`; here the screen holds both and reads them as they are
//! whenever iced draws it. What redraws it: its own messages (every call it
//! runs comes back as one), the settings' listeners (a subscription for as
//! long as the page is on the stack, as the Dart `watch` lasted), and, while
//! an update check this screen did not start is in flight (Home's
//! auto-check), a short watch tick — the update service has no way to drop a
//! listener, and one left registered per opening would pile up.
//!
//! Every service call that blocks runs off the UI thread as a task: the
//! device refresh (enumeration, and the last-known-name persist it may
//! make), each settings choice (the setter persists), the update check, the
//! repair, the save panel and the diagnostics copy. The choices run one at a
//! time, in the order they were made, as Dart's single isolate ran them: two
//! of them racing on worker threads could land the older one last. A
//! reverb drag's ticks queued behind a slow persist collapse into the last
//! one, and the slider shows the value asked for while it waits. Dart applied
//! each choice the moment it was made, so those still waiting when the page
//! leaves are applied all the same, after the one in flight.
//!
//! The device lists refresh once on open, then every 1.5 s so plugging or
//! unplugging a device updates them live (the refresh only notifies when a
//! set actually changed). Toasts (Dart's floating SnackBars) and the license
//! page (a `MaterialPageRoute` pushed over Settings, sliding in and back out
//! over it as the app's pages do, `slide::cupertino`) draw inside the page,
//! under the window drag bar; the studio dialogs (the repair confirmation,
//! the update download dialog) go through the app's dialog stack. A toast
//! still up when Settings leaves stays up over Home for the time it has left
//! (the app's, `app::messenger`), as under Dart's app-wide
//! `ScaffoldMessenger`. A toast fades in and out as Material's floating
//! snack bar did, its 3.5 s counted from the end of its entrance
//! (`ui::snack`).
//!
//! Flutter's Material slider and switch are drawn here with iced's slider,
//! under Material's overlay (`slider_overlay`: it grows over 100 ms under
//! the pointer on the thumb or a drag), and Material 3's switch as forui
//! themed it (`material_switch`: its 300 ms toggle, stretching thumb, hover
//! and press rings, and drag); the switch's semantics label has no
//! counterpart.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::futures::channel::mpsc;
use iced::futures::{Stream, StreamExt};
use iced::widget::{
    button, column, container, row, scrollable, slider, stack, text, Column, Space,
};
use iced::{
    Background, Border, Color, Element, Length, Padding, Shadow, Subscription, Task, Theme, Vector,
};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_device::AudioDevice;
use vocara_model::util::export_copy::{export_copy, exports_into};
use vocara_services::app_settings_service::{AppSettingsService, ListenerId};
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::persistence::RepairReport;
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::update_check_service::{UpdateCheckService, UpdateStatus};

use super::licenses::{self, LicensesPage};
use crate::app::library::Library;
use crate::router::Route;
use crate::services::Services;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::{self, Controller};
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, Studio};
use crate::ui::core::widgets::animated::ink_hover;
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::fading_scrollbar::Fading;
use crate::ui::core::widgets::material_switch::material_switch;
use crate::ui::core::widgets::slide::{cupertino, slide, PAGE_TRANSITION};
use crate::ui::core::widgets::slider_overlay::slider_overlay;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::home::widgets::update_notice::UpdateDownloadDialog;
use crate::ui::snack::{SnackEvent, SnackQueue, SnackStyle, SnackTimer};
use crate::ui::{Screen, SnackBar, Update};
use crate::util::to_string_as_fixed;

/// How often the device lists are re-read while Settings is open.
pub const DEVICE_POLL: Duration = Duration::from_millis(1500);

/// How long a result toast stays up.
pub const TOAST_DURATION: Duration = Duration::from_millis(3500);

/// How often the page re-reads an update check it did not start, while
/// that check is in flight (see the module docs).
pub const UPDATE_WATCH: Duration = Duration::from_millis(100);

/// The name the diagnostics save panel suggests.
pub const DIAGNOSTICS_FILE_NAME: &str = "vocara-diagnostics.txt";

/// The headphones switch (an id to find it by: it has no text).
pub const HEADPHONES_SWITCH: iced::widget::Id = iced::widget::Id::new("settings-headphones-switch");

/// The monitor reverb slider.
pub const REVERB_SLIDER: iced::widget::Id = iced::widget::Id::new("settings-reverb-slider");

/// The toasts.
pub const REPAIR_FAILED: &str = "The repair did not finish. It is safe to try again later.";
pub const SAVE_PANEL_FAILED: &str = "Could not open the save dialog.";
pub const DIAGNOSTICS_INTO_APP_STORAGE: &str =
    "That folder holds Vocara's own files. Save the diagnostics somewhere else.";
pub const DIAGNOSTICS_SAVED: &str =
    "Diagnostics saved. The file holds technical logs only, never your audio.";
pub const DIAGNOSTICS_FAILED: &str = "Could not save the diagnostics file.";

/// The banners.
const PERSIST_FAILED: &str = "Your settings could not be saved to disk. They are in effect now, \
     but the app will start with the previous ones. Check that the disk is not full or locked.";
const BLUETOOTH_CONFLICT: &str = "The microphone is a Bluetooth headset. Bluetooth can't do \
     high-quality sound and a microphone at once, so recording drops the headset to call \
     quality. Pick a different microphone (e.g. the built-in mic).";

/// The radius of the raised cards, and of a row's hover inside one (inside
/// its 1 px border).
const CARD_RADIUS: f32 = 10.0;
const ROW_RADIUS: f32 = CARD_RADIUS - 1.0;

/// The repair result as one plain sentence with the report's numbers.
///
/// Its own function, and tested on its own, because the three counts are
/// checked INDEPENDENTLY — integrity, the asset sweep, and the pool's
/// byte-level verification — so combinations the screen tests cannot easily
/// stage (a clean database holding a bit-rotted audio file) still have to
/// read as one coherent sentence.
pub fn repair_summary(report: &RepairReport) -> String {
    // Damaged audio is named on its own, never folded into a count of things
    // that were fixed: nothing was done about those files, and the sentence
    // has to say so.
    let damaged = if report.corrupt_pool_files > 0 {
        format!(
            " {} {} damaged and may not play correctly; nothing was changed.",
            count(report.corrupt_pool_files, "audio file"),
            if report.corrupt_pool_files == 1 {
                "looks"
            } else {
                "look"
            }
        )
    } else {
        String::new()
    };
    if report.violations_found == 0 && report.assets_swept == 0 {
        // Nothing to fix and a damaged file are both true at once, and the
        // all-clear head then contradicts the very next sentence. Say what
        // the repair DID instead of vouching for a library it just reported
        // damage in.
        let head = if report.corrupt_pool_files > 0 {
            "Repair finished. Nothing needed fixing."
        } else {
            "Repair finished. Everything looks consistent."
        };
        return format!("{head}{damaged}");
    }
    let mut parts = vec![format!("found {}", count(report.violations_found, "issue"))];
    if report.violations_found > 0 {
        parts.push(format!("repaired {}", report.violations_repaired));
    }
    parts.push(format!(
        "reclaimed {}",
        count(report.assets_swept, "unused file")
    ));
    format!("Repair finished: {}.{damaged}", parts.join(", "))
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The About block's version line (the license page shows the same).
pub fn version_line() -> String {
    format!(
        "Version {} (build {})",
        UpdateCheckService::APP_VERSION,
        UpdateCheckService::APP_BUILD_NUMBER
    )
}

/// The Check-for-updates row's one-line status: the last result, or what the
/// row does before any check has run.
pub fn update_subtitle(updates: &UpdateCheckService, checking: bool) -> String {
    if checking {
        return "Checking…".to_string();
    }
    match (updates.status(), updates.available()) {
        (UpdateStatus::UpdateAvailable, Some(info)) => format!(
            "Vocara {} is available. Click for the download.",
            info.version
        ),
        (UpdateStatus::UpToDate, _) => "You have the latest version.".to_string(),
        _ => {
            if updates.last_checked_at().is_none() {
                "Compares this build against the latest release.".to_string()
            } else if updates.manifest_unreadable() {
                "The update information could not be read. You can try again later.".to_string()
            } else {
                "Could not reach the update server. You can try again later.".to_string()
            }
        }
    }
}

/// A device's subtitle: its rate and whether it is the system default.
fn device_subtitle(d: &AudioDevice) -> String {
    let mut parts = Vec::new();
    if d.rate > 0 {
        parts.push(format!(
            "{} kHz",
            to_string_as_fixed(d.rate as f64 / 1000.0, 1)
        ));
    }
    if d.is_default {
        parts.push("system default".to_string());
    }
    parts.join(" · ")
}

// ------------------------------------------------------------------- seams

/// The save panel: the chosen destination for the suggested file name, or
/// `None` when the user cancelled; an error when it could not be shown.
pub type PickSavePath = Arc<dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync>;

/// Produces the diagnostics bundle: the path of a temporary bundle, which the
/// screen deletes once it has copied it.
pub type ExportDiagnostics = Arc<dyn Fn() -> io::Result<PathBuf> + Send + Sync>;

/// The app's own storage folder, which the diagnostics export refuses to
/// write into.
pub type AppStorageRoot = Arc<dyn Fn() -> io::Result<String> + Send + Sync>;

/// What the Dart screen took as test seams (`pickSavePath`,
/// `exportDiagnostics`, `appStorageRoot`). `openUrl` has no counterpart: the
/// update dialog's browser launch is the app's (`open_url_task`).
#[derive(Clone)]
pub struct SettingsHooks {
    pub pick_save_path: PickSavePath,
    pub export_diagnostics: ExportDiagnostics,
    pub app_storage_root: AppStorageRoot,
}

impl SettingsHooks {
    /// The app's: the platform save panel, the app-wide diagnostics log, and
    /// the application support directory `services` store under.
    pub fn production(services: &Services) -> Self {
        let storage = services.local_storage.clone();
        Self {
            pick_save_path: Arc::new(save_panel),
            export_diagnostics: Arc::new(|| DiagnosticsLog::instance().export_archive()),
            app_storage_root: Arc::new(move || {
                storage
                    .root_dir()
                    .map(|dir| dir.to_string_lossy().into_owned())
            }),
        }
    }
}

/// The platform save panel (`file_panels`; on macOS it runs on the main
/// thread for the calling worker): the destination path, or `None` on
/// cancel. Blocking.
fn save_panel(suggested_name: &str) -> Result<Option<String>, String> {
    Ok(vocara_services::file_panels::FileDialog::new()
        .set_file_name(suggested_name)
        .save_file()
        .map(|path| path.to_string_lossy().into_owned()))
}

// ---------------------------------------------------------------- messages

/// A settings change the user made: each one blocks on the settings persist,
/// so they run off the UI thread, one at a time, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    /// An output device by uid, or `None` for System Default.
    OutputDevice(Option<String>),
    /// An input device by uid, or `None` for System Default.
    InputDevice(Option<String>),
    /// Which input(s) of the current input device record (an
    /// `INPUT_CHANNEL_*` mode).
    InputChannel(u32),
    /// The monitor reverb amount (0..1).
    MonitorReverb(f64),
    /// The headphones mark of output `uid` (the one the card showed).
    Headphones { uid: String, on: bool },
}

impl Choice {
    /// Applies it (blocking: the persist).
    fn apply(self, settings: &AppSettingsService) {
        match self {
            Choice::OutputDevice(uid) => settings.set_output_device(uid.as_deref()),
            Choice::InputDevice(uid) => settings.set_input_device(uid.as_deref()),
            Choice::InputChannel(mode) => settings.set_input_channel(mode),
            Choice::MonitorReverb(wet) => settings.set_monitor_reverb(wet),
            Choice::Headphones { uid, on } => settings.set_output_is_headphones(&uid, on),
        }
    }

    fn reverb(&self) -> Option<f64> {
        match self {
            Choice::MonitorReverb(wet) => Some(*wet),
            _ => None,
        }
    }
}

/// How a diagnostics export that had a destination ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exported {
    /// The destination was inside the app's own storage: nothing written.
    IntoAppStorage,
    /// The bundle was copied there.
    Saved,
    /// The bundle could not be made or copied.
    Failed,
}

/// What the settings screen reacts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The header's back chevron (`popOrGo(context, Routes.home)`).
    Back,
    /// The device poll's tick.
    Poll,
    /// A device card's "Refresh devices".
    RefreshDevices,
    /// A device refresh finished.
    Refreshed,
    /// The settings notified a change.
    SettingsChanged,
    /// A device row, the input channel, the reverb slider or the headphones
    /// switch.
    Choose(Choice),
    /// The choice in flight was applied.
    Chosen,
    /// "Check for updates".
    CheckForUpdates,
    /// The manual check finished.
    UpdatesChecked,
    /// The update watch's tick (see the module docs).
    UpdateWatch,
    /// The update download dialog closed.
    DownloadDialogClosed,
    /// "Repair library": confirm first.
    RepairLibrary,
    /// The repair confirmation closed: whether it was confirmed.
    RepairAnswered(bool),
    /// The repair finished: its report, or why it failed.
    Repaired(Result<RepairReport, String>),
    /// "Export diagnostics…": open the save panel.
    ExportDiagnostics,
    /// The save panel closed: the destination, `None` cancelled, or why it
    /// could not be shown.
    SavePathPicked(Result<Option<String>, String>),
    /// The export to the picked destination finished.
    Exported(Exported),
    /// "Licenses": open the license page.
    OpenLicenses,
    /// The license page's own messages.
    Licenses(licenses::Message),
    /// Toast `n` has been up long enough.
    ToastExpired(u64),
    /// A frame while the license page slides, or its licenses come in.
    Frame(Instant),
    /// A frame while a toast enters or leaves.
    ToastFrame(Instant),
}

// ------------------------------------------------------------------ screen

/// The settings page (see the module docs).
pub struct SettingsScreen {
    settings: Arc<AppSettingsService>,
    updates: Arc<UpdateCheckService>,
    library: Arc<Library>,
    finalizer: Arc<RecordingFinalizer>,
    hooks: SettingsHooks,

    /// A device refresh is in flight; another was asked for meanwhile.
    refreshing: bool,
    refresh_owed: bool,

    /// The choice being applied, and those waiting their turn.
    applying: Option<Choice>,
    queued: VecDeque<Choice>,
    /// The order the choices' threads run in, and the next turn to give.
    turns: Arc<Turns>,
    turns_given: u64,

    /// The manual update check this page started is in flight.
    checking_updates: bool,

    // One maintenance action at a time: each row disables itself while it
    // runs.
    /// The repair confirmation is up (a second tap opens no second one).
    asking_repair: bool,
    repairing: bool,
    /// The save panel is up (a second tap opens no second one).
    picking: bool,
    exporting: bool,

    /// The license page, while it is open over Settings.
    licenses: Option<Box<LicensesPage>>,
    /// The license page's route animation, while it slides in or out.
    licenses_route: Option<Box<Controller>>,
    /// The license page sliding out after its Back, drawn until it has.
    closed_licenses: Option<Box<LicensesPage>>,
    /// The last frame's time, what the drawing reads.
    now: Instant,

    /// The result toasts (one at a time: each replaces the one up).
    toasts: SnackQueue,
}

impl std::fmt::Debug for SettingsScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsScreen")
            .field("applying", &self.applying)
            .field("queued", &self.queued)
            .field("repairing", &self.repairing)
            .field("exporting", &self.exporting)
            .field("licenses", &self.licenses.is_some())
            .field("toasts", &self.toasts)
            .finish_non_exhaustive()
    }
}

impl SettingsScreen {
    /// The page over the app's services; the device lists' first refresh
    /// starts at once.
    pub fn new(services: &Services) -> (Self, Task<Message>) {
        Self::with_hooks(services, SettingsHooks::production(services))
    }

    /// The page with its save panel, diagnostics bundle and app storage
    /// folder supplied (tests).
    pub fn with_hooks(services: &Services, hooks: SettingsHooks) -> (Self, Task<Message>) {
        let mut screen = SettingsScreen {
            settings: services.app_settings.clone(),
            updates: services.update_check.clone(),
            library: services.library.clone(),
            finalizer: RecordingFinalizer::instance(),
            hooks,
            refreshing: false,
            refresh_owed: false,
            applying: None,
            queued: VecDeque::new(),
            turns: Arc::default(),
            turns_given: 0,
            checking_updates: false,
            asking_repair: false,
            repairing: false,
            picking: false,
            exporting: false,
            licenses: None,
            licenses_route: None,
            closed_licenses: None,
            now: Instant::now(),
            toasts: SnackQueue::new(),
        };
        // Refresh once on open (the poll then keeps the lists live).
        let first = screen.refresh();
        (screen, first)
    }

    /// Whether the license page slides, or its licenses come in or go, at
    /// `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.licenses_route
            .as_ref()
            .is_some_and(|r| r.is_animating(now))
            || self.licenses.as_ref().is_some_and(|l| l.is_animating(now))
    }

    /// The toast on screen, if any.
    pub fn toast(&self) -> Option<&str> {
        self.toasts.current().map(|t| t.text.as_str())
    }

    /// The number of the toast on screen ([`Message::ToastExpired`] takes
    /// it down).
    pub fn toast_seq(&self) -> Option<u64> {
        self.toasts.current_seq()
    }

    /// The toast's timer, while one is up.
    pub fn toast_timer(&self) -> Option<SnackTimer> {
        self.toasts.timer()
    }

    /// Whether the repair is running.
    pub fn repairing(&self) -> bool {
        self.repairing
    }

    /// Whether the diagnostics export (after its save panel) is running.
    pub fn exporting(&self) -> bool {
        self.exporting
    }

    /// Whether the license page is open.
    pub fn licenses_open(&self) -> bool {
        self.licenses.is_some()
    }

    /// The choice being applied, and those waiting.
    pub fn pending_choices(&self) -> Vec<Choice> {
        self.applying
            .iter()
            .chain(self.queued.iter())
            .cloned()
            .collect()
    }

    /// Whether the page watches an update check it did not start.
    pub fn watches_update_check(&self) -> bool {
        self.updates.checking() && !self.checking_updates
    }

    /// Whether an update check is in flight, this page's or another's.
    fn update_checking(&self) -> bool {
        self.checking_updates || self.updates.checking()
    }

    /// The reverb the slider shows: the last one asked for while it waits to
    /// be applied, else the settings'.
    fn reverb_shown(&self) -> f64 {
        self.queued
            .iter()
            .rev()
            .chain(self.applying.iter())
            .find_map(Choice::reverb)
            .unwrap_or_else(|| self.settings.monitor_reverb_wet())
    }

    /// Re-reads the device lists off the UI thread; one asked for while one
    /// runs follows it.
    fn refresh(&mut self) -> Task<Message> {
        if self.refreshing {
            self.refresh_owed = true;
            return Task::none();
        }
        self.refreshing = true;
        let settings = Arc::clone(&self.settings);
        Task::perform(
            crate::util::blocking(move || settings.refresh_devices()),
            |_| Message::Refreshed,
        )
    }

    /// Queues `choice` behind the one being applied; a reverb drag tick
    /// replaces the one still waiting (its value would be overwritten
    /// before anyone heard it).
    fn choose(&mut self, choice: Choice) -> Task<Message> {
        if choice.reverb().is_some() && self.queued.back().is_some_and(|c| c.reverb().is_some()) {
            self.queued.pop_back();
        }
        self.queued.push_back(choice);
        self.apply_next()
    }

    /// Starts the next waiting choice, unless one is being applied.
    fn apply_next(&mut self) -> Task<Message> {
        if self.applying.is_some() {
            return Task::none();
        }
        let Some(choice) = self.queued.pop_front() else {
            return Task::none();
        };
        self.applying = Some(choice.clone());
        let turn = self.next_turn();
        let (turns, settings) = (Arc::clone(&self.turns), Arc::clone(&self.settings));
        Task::perform(
            crate::util::blocking(move || turns.take(turn, || choice.apply(&settings))),
            |_| Message::Chosen,
        )
    }

    /// The turn of the next thread applying choices.
    fn next_turn(&mut self) -> u64 {
        let turn = self.turns_given;
        self.turns_given += 1;
        turn
    }

    /// Replaces any toast on screen with `text` (`clearSnackBars`, then
    /// `showSnackBar`).
    fn show_toast(&mut self, text: impl Into<String>) {
        let bar = SnackBar::new(text, TOAST_DURATION, true, SnackStyle::Floating, toast_bar);
        self.toasts.show(bar, Instant::now());
    }

    fn check_for_updates(&mut self) -> Update<Message> {
        if self.update_checking() {
            return Update::none();
        }
        self.checking_updates = true;
        let updates = Arc::clone(&self.updates);
        Task::perform(crate::util::blocking(move || updates.check()), |_| {
            Message::UpdatesChecked
        })
        .into()
    }

    fn repair_library(&mut self) -> Update<Message> {
        if self.asking_repair || self.repairing {
            return Update::none();
        }
        self.asking_repair = true;
        Update::dialog(
            ConfirmDialog::new("Repair library?")
                .body(
                    "Vocara checks the library for inconsistencies, fixes what it finds, and \
                     reclaims unused space. Your practices and recordings are not touched. \
                     This can take a moment.",
                )
                .confirm_label("Repair")
                .destructive(false)
                .icon(Icon::Wrench),
            |result| Message::RepairAnswered(result.confirmed()),
        )
    }

    fn repair_answered(&mut self, confirmed: bool) -> Update<Message> {
        self.asking_repair = false;
        if !confirmed || self.repairing {
            return Update::none();
        }
        self.repairing = true;
        let library = Arc::clone(&self.library);
        Task::perform(
            crate::util::blocking(move || {
                library
                    .persistence()
                    .repair_library()
                    .map_err(|e| e.to_string())
            }),
            |joined| Message::Repaired(joined.and_then(|r| r)),
        )
        .into()
    }

    fn export_diagnostics(&mut self) -> Update<Message> {
        if self.picking || self.exporting {
            return Update::none();
        }
        self.picking = true;
        let pick = Arc::clone(&self.hooks.pick_save_path);
        Task::perform(
            // A panic in the GUI call says what a failure to show it says
            // (Dart caught everything the panel threw).
            crate::util::blocking(move || pick(DIAGNOSTICS_FILE_NAME)),
            |joined| Message::SavePathPicked(joined.and_then(|r| r)),
        )
        .into()
    }

    fn save_path_picked(&mut self, picked: Result<Option<String>, String>) -> Update<Message> {
        self.picking = false;
        let destination = match picked {
            Ok(Some(destination)) => destination,
            Ok(None) => return Update::none(),
            Err(_) => {
                // As a take's export says it: thrown out of the tap, it said
                // nothing.
                self.show_toast(SAVE_PANEL_FAILED);
                return Update::none();
            }
        };
        self.exporting = true;
        let hooks = self.hooks.clone();
        let finalizer = Arc::clone(&self.finalizer);
        Task::perform(
            crate::util::blocking(move || export_to(&hooks, &finalizer, &destination)),
            |joined| Message::Exported(joined.unwrap_or(Exported::Failed)),
        )
        .into()
    }

    fn exported(&mut self, outcome: Exported) {
        self.exporting = false;
        self.show_toast(match outcome {
            Exported::IntoAppStorage => DIAGNOSTICS_INTO_APP_STORAGE,
            Exported::Saved => DIAGNOSTICS_SAVED,
            Exported::Failed => DIAGNOSTICS_FAILED,
        });
    }
}

/// The order the choices are applied in across threads: each thread takes
/// its turn, given when the page started it, after every earlier one has
/// had its own (the page's tasks, then the flush of what still waited when
/// the page left).
#[derive(Debug, Default)]
struct Turns {
    served: Mutex<u64>,
    next: Condvar,
}

impl Turns {
    /// Waits for `turn`, runs `work`, then hands over to the next turn,
    /// however `work` ends.
    fn take<T>(&self, turn: u64, work: impl FnOnce() -> T) -> T {
        struct Served<'a>(&'a Turns);
        impl Drop for Served<'_> {
            fn drop(&mut self) {
                *self.0.served.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                self.0.next.notify_all();
            }
        }
        let mut served = self.served.lock().unwrap_or_else(PoisonError::into_inner);
        while *served < turn {
            served = self
                .next
                .wait(served)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(served);
        let _served = Served(self);
        work()
    }
}

/// The diagnostics export once the save panel gave a destination
/// (blocking).
fn export_to(hooks: &SettingsHooks, finalizer: &RecordingFinalizer, destination: &str) -> Exported {
    // Into the app's own storage: the bundle replaced one of the library's
    // files (its database, a take's audio).
    if into_app_storage(hooks, destination) {
        return Exported::IntoAppStorage;
    }
    let Ok(bundle) = (hooks.export_diagnostics)() else {
        return Exported::Failed;
    };
    // Held, as a take's export is: a quit mid-copy cut the file short.
    let copied = crate::util::run_caught(|| finalizer.hold(|| export_copy(&bundle, destination)));
    // The bundle was only the copy's source: drop it and its temp folder,
    // whether the copy worked or not (a failed copy leaves it as much as a
    // good one did).
    DiagnosticsLog::discard_archive(&bundle);
    match copied {
        Ok(()) => Exported::Saved,
        Err(_) => Exported::Failed,
    }
}

fn into_app_storage(hooks: &SettingsHooks, path: &str) -> bool {
    match crate::util::run_caught(|| (hooks.app_storage_root)()) {
        Ok(root) => exports_into(&root, path),
        // No folder to judge by: the copy says what failed.
        Err(_) => false,
    }
}

impl Screen for SettingsScreen {
    type Message = Message;

    fn update(&mut self, message: Message, _services: &Services) -> Update<Message> {
        match message {
            Message::Back => Update::pop_or_go(Route::Home),
            Message::Poll | Message::RefreshDevices => self.refresh().into(),
            Message::Refreshed => {
                self.refreshing = false;
                if std::mem::take(&mut self.refresh_owed) {
                    return self.refresh().into();
                }
                Update::none()
            }
            // The state changed; iced redraws.
            Message::SettingsChanged | Message::UpdateWatch | Message::DownloadDialogClosed => {
                Update::none()
            }
            Message::Choose(choice) => self.choose(choice).into(),
            Message::Chosen => {
                self.applying = None;
                self.apply_next().into()
            }
            Message::CheckForUpdates => self.check_for_updates(),
            Message::UpdatesChecked => {
                self.checking_updates = false;
                match self.updates.available() {
                    Some(info) => Update::dialog(UpdateDownloadDialog::new(info), |_| {
                        Message::DownloadDialogClosed
                    }),
                    None => Update::none(),
                }
            }
            Message::RepairLibrary => self.repair_library(),
            Message::RepairAnswered(confirmed) => self.repair_answered(confirmed),
            Message::Repaired(outcome) => {
                self.repairing = false;
                match outcome {
                    Ok(report) => self.show_toast(repair_summary(&report)),
                    Err(_) => self.show_toast(REPAIR_FAILED),
                }
                Update::none()
            }
            Message::ExportDiagnostics => self.export_diagnostics(),
            Message::SavePathPicked(picked) => self.save_path_picked(picked),
            Message::Exported(outcome) => {
                self.exported(outcome);
                Update::none()
            }
            Message::OpenLicenses => {
                self.licenses = Some(Box::new(LicensesPage::new()));
                let mut route = Controller::new(PAGE_TRANSITION, PAGE_TRANSITION);
                route.forward(Instant::now());
                self.licenses_route = Some(Box::new(route));
                self.closed_licenses = None;
                Update::none()
            }
            Message::Licenses(message) => {
                if let Some(page) = &mut self.licenses {
                    if page.update(message) {
                        // Back: the page slides back out from where it got.
                        let now = Instant::now();
                        let mut route = self
                            .licenses_route
                            .take()
                            .filter(|r| r.is_animating(now))
                            .map_or_else(
                                || Controller::completed(PAGE_TRANSITION, PAGE_TRANSITION),
                                |r| *r,
                            );
                        route.reverse(now);
                        self.licenses_route = Some(Box::new(route));
                        self.closed_licenses = self.licenses.take();
                    }
                }
                Update::none()
            }
            Message::Frame(at) => {
                self.now = self.now.max(at);
                let now = self.now;
                if let Some(page) = &mut self.licenses {
                    page.frame(at);
                }
                if self
                    .licenses_route
                    .as_ref()
                    .is_some_and(|r| !r.is_animating(now))
                {
                    self.licenses_route = None;
                    self.closed_licenses = None;
                }
                Update::none()
            }
            Message::ToastExpired(seq) => {
                self.toasts.expired(seq, Instant::now());
                Update::none()
            }
            Message::ToastFrame(at) => {
                self.toasts.frame(at);
                Update::none()
            }
        }
    }

    /// The page left: the choices still waiting were made all the same (Dart
    /// applied each as it was made), so they are applied on a thread of
    /// their own, after the one in flight. The rest of the page's work runs
    /// out on its own; what it reports is dropped.
    fn dispose(&mut self, _services: &Services) {
        let waiting: Vec<Choice> = self.queued.drain(..).collect();
        if waiting.is_empty() {
            return;
        }
        let turn = self.next_turn();
        let (turns, settings) = (Arc::clone(&self.turns), Arc::clone(&self.settings));
        let spawned = std::thread::Builder::new()
            .name("settings-flush".into())
            .spawn(move || {
                turns.take(turn, || {
                    for choice in waiting {
                        choice.apply(&settings);
                    }
                })
            });
        if let Err(e) = spawned {
            DiagnosticsLog::instance().log("settings", &format!("flush failed: {e}"));
        }
    }

    /// The toast, for the time it has left; it replaced the ones before it.
    fn take_snack_bars(&mut self) -> Vec<SnackBar> {
        self.toasts.take(Instant::now())
    }

    fn view(&self) -> Element<'_, Message> {
        let settings = || -> Element<'_, Message> {
            column![
                header(),
                scrollable(self.body())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .fading(),
            ]
            .into()
        };
        let route = self.licenses_route.as_ref().map(|r| cupertino(r, self.now));
        let page: Element<'_, Message> = match (&self.licenses, &self.closed_licenses, route) {
            // Sliding in over Settings, which slides a third of the way out.
            (Some(licenses), _, Some((moving, covered, shadow))) => stack![
                slide(settings(), covered).inert(),
                slide(licenses.view().map(Message::Licenses), moving).shadow(shadow),
            ]
            .into(),
            (Some(licenses), ..) => licenses.view().map(Message::Licenses),
            // Sliding back out after its Back, Settings sliding back in.
            (None, Some(closed), Some((moving, covered, shadow))) => stack![
                slide(settings(), covered),
                slide(closed.view().map(Message::Licenses), moving)
                    .shadow(shadow)
                    .inert(),
            ]
            .into(),
            (None, ..) => settings(),
        };
        let mut layers = stack![container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(toasts) = self.toasts.view() {
            layers = layers.push(toasts.map(|never| match never {}));
        }
        layers.into()
    }

    /// The device poll, the settings' changes, the toast's time, and the
    /// update watch — for as long as the page is on the stack, covered or
    /// not (Dart's timer and `watch` lasted until `dispose`).
    fn subscription(&self, _on_top: bool) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::time::every(DEVICE_POLL).map(|_| Message::Poll),
            Subscription::run_with(SettingsFeed(Arc::clone(&self.settings)), settings_changes)
                .map(|()| Message::SettingsChanged),
        ];
        subscriptions.push(self.toasts.subscription().map(|event| match event {
            SnackEvent::Expired(seq) => Message::ToastExpired(seq),
            SnackEvent::Frame(at) => Message::ToastFrame(at),
        }));
        subscriptions.push(motion::frames(self.is_animating(Instant::now())).map(Message::Frame));
        if self.watches_update_check() {
            subscriptions.push(iced::time::every(UPDATE_WATCH).map(|_| Message::UpdateWatch));
        }
        Subscription::batch(subscriptions)
    }
}

/// The settings service as a subscription's identity (the service itself, by
/// address).
struct SettingsFeed(Arc<AppSettingsService>);

impl Hash for SettingsFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

/// Removes its listener when the stream holding it is dropped.
struct Listening {
    settings: Arc<AppSettingsService>,
    id: ListenerId,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.settings.remove_listener(self.id);
    }
}

/// One item per notify of the settings, until the stream is dropped (the
/// page left the stack), which unregisters the listener.
fn settings_changes(feed: &SettingsFeed) -> impl Stream<Item = ()> {
    let (sender, receiver) = mpsc::unbounded();
    let id = feed.0.add_listener(Box::new(move || {
        let _ = sender.unbounded_send(());
    }));
    let listening = Listening {
        settings: Arc::clone(&feed.0),
        id,
    };
    receiver.map(move |()| {
        let _ = &listening;
    })
}

// -------------------------------------------------------------------- view

impl SettingsScreen {
    fn body(&self) -> Element<'_, Message> {
        let s = &*self.settings;
        let mut content = Column::new().width(Length::Fill);
        if s.persist_failed() {
            content = content
                .push(warning_banner(PERSIST_FAILED))
                .push(Space::new().height(20));
        }

        // Audio output, and the headphones mark for it. Only an output no
        // name can tell takes the mark: earbuds need none, speakers take
        // none.
        content = content
            .push(section_label("Audio Output"))
            .push(Space::new().height(10))
            .push(device_card(
                &s.output_devices(),
                s.output_device_uid().as_deref(),
                s.output_device_name().as_deref(),
                "Follow the macOS output device",
                Choice::OutputDevice,
            ));
        let to_mark = if s.output_markable() {
            s.output_to_mark()
        } else {
            None
        };
        if let Some(device) = to_mark {
            content = content
                .push(Space::new().height(12))
                .push(headphones_card(device, s.output_marked_headphones()));
        }

        // Audio input, its channel choice and the Bluetooth warning.
        content = content
            .push(Space::new().height(24))
            .push(section_label("Audio Input"))
            .push(Space::new().height(10))
            .push(device_card(
                &s.input_devices(),
                s.input_device_uid().as_deref(),
                s.input_device_name().as_deref(),
                "Follow the macOS input device",
                Choice::InputDevice,
            ));
        // Only a device with more than one input has a choice to make.
        if s.input_channel_count() >= 2 {
            content = content
                .push(Space::new().height(12))
                .push(input_channel_card(
                    &s.input_channel_device_name(),
                    s.input_channel(),
                ));
        }
        if s.bluetooth_in_out_conflict() {
            content = content
                .push(Space::new().height(12))
                .push(warning_banner(BLUETOOTH_CONFLICT));
        }

        // Monitoring.
        content = content
            .push(Space::new().height(24))
            .push(section_label("Monitoring"))
            .push(Space::new().height(10))
            .push(reverb_card(self.reverb_shown()))
            .push(Space::new().height(14))
            .push(
                text(format!(
                    "The engine runs at {} kHz; devices are resampled to/from this rate \
                     automatically. The input device is used for recording and live monitoring.",
                    to_string_as_fixed(s.engine_sample_rate() / 1000.0, 1)
                ))
                .size(12)
                .line_height(1.45)
                .color(Studio::TEXT_FAINT),
            );

        // Maintenance.
        content = content
            .push(Space::new().height(24))
            .push(section_label("Maintenance"))
            .push(Space::new().height(10))
            .push(card(vec![
                action_row(
                    Icon::Wrench,
                    "Repair library",
                    if self.repairing {
                        "Repairing…"
                    } else {
                        "Checks the library for inconsistencies and reclaims unused space."
                    },
                    (!self.repairing).then_some(Message::RepairLibrary),
                    self.repairing,
                    Corners::TOP,
                ),
                divider(),
                action_row(
                    Icon::FileDown,
                    "Export diagnostics…",
                    if self.exporting {
                        "Exporting…"
                    } else {
                        "Saves a text file of technical logs for a bug report. It contains no \
                         audio."
                    },
                    (!self.exporting).then_some(Message::ExportDiagnostics),
                    self.exporting,
                    Corners::BOTTOM,
                ),
            ]));

        // About.
        let checking = self.update_checking();
        content = content
            .push(Space::new().height(24))
            .push(section_label("About"))
            .push(Space::new().height(10))
            .push(card(vec![
                info_row(Icon::Info, "Vocara", version_line()),
                divider(),
                action_row(
                    Icon::RefreshCw,
                    "Check for updates",
                    update_subtitle(&self.updates, checking),
                    (!checking).then_some(Message::CheckForUpdates),
                    checking,
                    Corners::NONE,
                ),
                divider(),
                action_row(
                    Icon::ScrollText,
                    "Licenses",
                    "Open-source components that ship with Vocara.",
                    Some(Message::OpenLicenses),
                    false,
                    Corners::BOTTOM,
                ),
            ]));

        container(content)
            .width(Length::Fill)
            .padding(Padding {
                top: 20.0,
                right: 24.0,
                bottom: 24.0,
                left: 24.0,
            })
            .into()
    }
}

/// `_Header`: 56 tall on `Studio.surface` with its bottom border: the back
/// chevron and "Settings".
fn header<'a>() -> Element<'a, Message> {
    // `_IconButton`: 34 square, an 18 px chevron, a click cursor.
    let back = button(
        container(icons::icon(Icon::ChevronLeft, 18.0, Studio::TEXT_SECONDARY))
            .center_x(34)
            .center_y(34),
    )
    .padding(0)
    .style(bare_button)
    .on_press(Message::Back);
    let bar = row![
        back,
        Space::new().width(6),
        studio_text("Settings")
            .size(18)
            .font(font::BOLD)
            .letter_spacing(0.3)
            .color(Studio::TEXT_PRIMARY),
    ]
    .align_y(Vertical::Center);
    container(column![
        container(bar)
            .padding(Padding::from([0, 12]))
            .height(55)
            .align_y(Vertical::Center),
        rule(),
    ])
    .width(Length::Fill)
    .height(56)
    .style(theme::fill(Studio::SURFACE))
    .into()
}

/// A full-width 1 px line in `Studio.border`.
fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(Studio::BORDER))
        .into()
}

/// `Divider(height: 1, color: Studio.border)` between a card's rows.
fn divider<'a>() -> Element<'a, Message> {
    rule()
}

/// `_SectionLabel`: the text in capitals, 11 px bold, 1.2 letter spacing,
/// `Studio.textSecondary`.
fn section_label<'a>(label: &str) -> Element<'a, Message> {
    studio_text(label.to_uppercase())
        .size(11)
        .font(font::BOLD)
        .letter_spacing(1.2)
        .color(Studio::TEXT_SECONDARY)
        .into()
}

/// Which of a card's corners a row's hover fills (the card clips its rows to
/// its rounded shape).
#[derive(Debug, Clone, Copy)]
struct Corners {
    top: bool,
    bottom: bool,
}

impl Corners {
    const NONE: Corners = Corners {
        top: false,
        bottom: false,
    };
    const TOP: Corners = Corners {
        top: true,
        bottom: false,
    };
    const BOTTOM: Corners = Corners {
        top: false,
        bottom: true,
    };

    fn radius(self) -> Radius {
        let top = if self.top { ROW_RADIUS } else { 0.0 };
        let bottom = if self.bottom { ROW_RADIUS } else { 0.0 };
        Radius {
            top_left: top,
            top_right: top,
            bottom_right: bottom,
            bottom_left: bottom,
        }
    }
}

/// A tappable row of a card: `on_tap` `None` disables it. Its hover
/// highlight fades in and out (the `InkWell`'s, `ink_hover`); none on a
/// disabled row.
fn tappable<'a>(
    content: impl Into<Element<'a, Message>>,
    on_tap: Option<Message>,
    corners: Corners,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let row = button(content)
        .padding(0)
        .width(Length::Fill)
        .style(bare_button)
        .on_press_maybe(on_tap);
    if enabled {
        ink_hover(row, Studio::HOVER, corners.radius())
    } else {
        row.into()
    }
}

/// `_Card`: the raised card the rows sit in — `Studio.surfaceRaised`, radius
/// 10, `Studio.border` — its rows inside the border.
fn card(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    container(Column::with_children(rows).width(Length::Fill))
        .width(Length::Fill)
        .padding(1)
        .style(theme::card(CARD_RADIUS))
        .into()
}

/// The device card: "System Default", every device, the unplugged persisted
/// device when there is one, and "Refresh devices".
fn device_card<'a>(
    devices: &[AudioDevice],
    selected_uid: Option<&str>,
    last_known_name: Option<&str>,
    default_subtitle: &str,
    choose: fn(Option<String>) -> Choice,
) -> Element<'a, Message> {
    let explicit = selected_uid.filter(|uid| !uid.is_empty());
    // The persisted device is unplugged: the engine runs on the system
    // default, but the choice is kept (and restored when the device returns).
    let missing = explicit.filter(|uid| !devices.iter().any(|d| d.uid == *uid));

    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    // "System Default" follows whatever the OS picks. While the selected
    // device is unplugged it is the effective route, so say so.
    rows.push(device_row(
        "System Default".to_string(),
        if missing.is_some() {
            format!("{default_subtitle} · currently active")
        } else {
            default_subtitle.to_string()
        },
        explicit.is_none(),
        Some(Message::Choose(choose(None))),
        Corners::TOP,
    ));
    for d in devices {
        rows.push(divider());
        rows.push(device_row(
            d.name.clone(),
            device_subtitle(d),
            explicit == Some(d.uid.as_str()),
            Some(Message::Choose(choose(Some(d.uid.clone())))),
            Corners::NONE,
        ));
    }
    if let Some(uid) = missing {
        // The unplugged persisted selection: still the checked choice, but
        // disabled (not tappable) and marked as unavailable. Labelled by its
        // last-known name, else its raw uid.
        rows.push(divider());
        rows.push(device_row(
            last_known_name
                .filter(|n| !n.is_empty())
                .unwrap_or(uid)
                .to_string(),
            "Unavailable · using system default".to_string(),
            true,
            None,
            Corners::NONE,
        ));
    }
    rows.push(divider());
    rows.push(refresh_row());
    card(rows)
}

/// `_DeviceRow`: the speaker, the name and its subtitle, and a check when
/// selected. A row with no tap is the unplugged persisted selection: dimmed,
/// its selection accents included.
fn device_row<'a>(
    title: String,
    subtitle: String,
    selected: bool,
    on_tap: Option<Message>,
    corners: Corners,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let accent = if enabled {
        Studio::ACCENT
    } else {
        Studio::TEXT_FAINT
    };
    let title_color = if !enabled {
        Studio::TEXT_FAINT
    } else if selected {
        Studio::TEXT_PRIMARY
    } else {
        Studio::TEXT_SECONDARY
    };
    let mut label = column![studio_text(title)
        .size(13.5)
        .font(if selected {
            font::SEMIBOLD
        } else {
            font::MEDIUM
        })
        .color(title_color)
        .ellipsis(1)];
    if !subtitle.is_empty() {
        label = label
            .push(Space::new().height(2))
            .push(text(subtitle).size(11).color(Studio::TEXT_FAINT));
    }
    let mut content = row![
        icons::icon(
            Icon::Speaker,
            16.0,
            if selected { accent } else { Studio::TEXT_FAINT }
        ),
        Space::new().width(12),
        label.width(Length::Fill),
    ]
    .align_y(Vertical::Center);
    if selected {
        content = content.push(icons::icon(Icon::Check, 16.0, accent));
    }
    tappable(
        container(content).padding(Padding::from([12, 14])),
        on_tap,
        corners,
    )
}

/// `_RefreshRow`: "Refresh devices".
fn refresh_row<'a>() -> Element<'a, Message> {
    tappable(
        container(
            row![
                icons::icon(Icon::RotateCw, 14.0, Studio::TEXT_SECONDARY),
                Space::new().width(12),
                text("Refresh devices")
                    .size(12.5)
                    .color(Studio::TEXT_SECONDARY),
            ]
            .align_y(Vertical::Center),
        )
        .padding(Padding::from([11, 14])),
        Some(Message::RefreshDevices),
        Corners::BOTTOM,
    )
}

/// `_WarningBanner`: the record colour at 10 % with a 40 % border, a warning
/// glyph and the text.
fn warning_banner<'a>(message: &'a str) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::TriangleAlert, 16.0, Studio::RECORD),
            Space::new().width(10),
            text(message)
                .size(12)
                .line_height(1.4)
                .color(Studio::TEXT_SECONDARY)
                .width(Length::Fill),
        ]
        .align_y(Vertical::Top),
    )
    .width(Length::Fill)
    // 14 / 12 inside the 1 px border (a decoration's border pads its child;
    // an iced border does not).
    .padding(Padding::from([13, 15]))
    .style(|_: &Theme| {
        boxed(
            Some(alpha(Studio::RECORD, 0.10)),
            CARD_RADIUS,
            Some(alpha(Studio::RECORD, 0.4)),
        )
    })
    .into()
}

/// `_MonitorReverbCard`: the waves, "Reverb", the slider and its
/// percentage.
fn reverb_card<'a>(wet: f64) -> Element<'a, Message> {
    let value = wet.clamp(0.0, 1.0);
    // Flutter's track sits the overlay radius (12) in from each end; iced's
    // runs edge to edge with the thumb's centre a radius (6) in.
    let reverb = container(slider_overlay(
        slider(0.0..=1.0, value, |wet| {
            Message::Choose(Choice::MonitorReverb(wet))
        })
        .step(0.01)
        .height(24)
        .style(reverb_slider_style),
        value as f32,
        6.0,
        // `RoundSliderOverlayShape(overlayRadius: 12)`.
        12.0,
    ))
    .id(REVERB_SLIDER)
    .padding(Padding::from([0, 6]))
    .width(Length::Fill);
    container(
        row![
            icons::icon(Icon::Waves, 16.0, Studio::TEXT_FAINT),
            Space::new().width(12),
            container(text("Reverb").size(13).color(Studio::TEXT_SECONDARY)).width(52),
            reverb,
            container(
                text(format!("{}%", (wet * 100.0).round() as i64))
                    .size(12)
                    .color(Studio::TEXT_FAINT)
                    .width(Length::Fill)
                    .align_x(Horizontal::Right),
            )
            .width(38),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    // 14 / 8 inside the 1 px border.
    .padding(Padding::from([9, 15]))
    .style(theme::card(CARD_RADIUS))
    .into()
}

/// The reverb slider: a 3 px track, coral up to a 6 px coral thumb, the
/// inset colour after it.
fn reverb_slider_style(_: &Theme, _: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(Studio::ACCENT),
                Background::Color(Studio::INSET),
            ),
            width: 3.0,
            border: Border {
                radius: 1.5.into(),
                ..Border::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 6.0 },
            background: Background::Color(Studio::ACCENT),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
        },
    }
}

/// Whether only headphones play from the current output — wired ones on an
/// audio interface, or a pair whose name does not say so. On a marked
/// output, takes skip echo cancelling (which dulled held in-key notes on an
/// echo the mic never heard) and the monitor is allowed. Offered only for a
/// device that can be headphones of its own (see
/// `AppSettingsService::output_markable`), and plain about what goes wrong
/// on speakers. Same chrome as the input-channel card.
fn headphones_card<'a>(device: AudioDevice, marked: bool) -> Element<'a, Message> {
    let label = column![
        text("Headphones only")
            .size(13)
            .color(Studio::TEXT_SECONDARY),
        Space::new().height(2),
        text(format!(
            "{} — turn on if you always listen on headphones here. Recordings then skip echo \
             cancelling, which keeps long notes clear, and hearing yourself is allowed. \
             Remembered for this output.",
            device.name
        ))
        .size(11)
        .color(Studio::TEXT_FAINT),
        Space::new().height(2),
        text(
            "Leave it off if speakers ever play from it: takes would pick up the backing track, \
             and hearing yourself could howl."
        )
        .size(11)
        .color(Studio::TEXT_SECONDARY),
    ]
    .width(Length::Fill);
    // The device this card shows, not whatever renders when the tap lands.
    let toggle = Message::Choose(Choice::Headphones {
        uid: device.uid,
        on: !marked,
    });
    container(
        row![
            icons::icon(Icon::Headphones, 16.0, Studio::TEXT_FAINT),
            Space::new().width(12),
            label,
            Space::new().width(12),
            container(material_switch(
                marked,
                toggle,
                Studio::ACCENT,
                Studio::INSET
            ))
            .id(HEADPHONES_SWITCH),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    // 14 / 10 / 10 / 10 inside the 1 px border.
    .padding(Padding {
        top: 11.0,
        right: 11.0,
        bottom: 11.0,
        left: 15.0,
    })
    .style(theme::card(CARD_RADIUS))
    .into()
}

/// Which input(s) of a multi-input device (an audio interface) are the
/// voice: Auto follows the live one — a mic on either input records at full
/// level, two live inputs blend — while Input 1 or Input 2 keeps the other
/// out (say, a guitar DI on input 2). Same chrome as the reverb card; the
/// segmented control matches the backing-track toggle.
///
/// `device` is the device the choice applies to ("" when unknown): the one
/// recording, which is the system default while the chosen device is
/// unplugged.
fn input_channel_card<'a>(device: &str, mode: u32) -> Element<'a, Message> {
    let named = if device.is_empty() {
        String::new()
    } else {
        format!("{device} — ")
    };
    let label = column![
        text("Input channel").size(13).color(Studio::TEXT_SECONDARY),
        Space::new().height(2),
        text(format!(
            "{named}Auto follows the input you sing into. Pick one to keep a second source out."
        ))
        .size(11)
        .color(Studio::TEXT_FAINT),
    ]
    .width(Length::Fill);
    let segments = container(row![
        segment("Auto", GraphAudioPlaybackService::INPUT_CHANNEL_AUTO, mode),
        segment("Input 1", GraphAudioPlaybackService::INPUT_CHANNEL_1, mode),
        segment("Input 2", GraphAudioPlaybackService::INPUT_CHANNEL_2, mode),
    ])
    // 2 inside the 1 px border.
    .padding(3)
    .style(|_: &Theme| boxed(Some(Studio::INSET), 7.0, Some(Studio::BORDER)));
    container(
        row![
            icons::icon(Icon::Mic, 16.0, Studio::TEXT_FAINT),
            Space::new().width(12),
            label,
            segments,
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    // 14 / 10 inside the 1 px border.
    .padding(Padding::from([11, 15]))
    .style(theme::card(CARD_RADIUS))
    .into()
}

/// One segment of the input-channel control.
fn segment<'a>(label: &'a str, value: u32, mode: u32) -> Element<'a, Message> {
    let active = mode == value;
    let face = container(
        studio_text(label)
            .size(11)
            .font(font::BOLD)
            .letter_spacing(0.3)
            .color(if active {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::TEXT_SECONDARY
            }),
    )
    .padding(Padding::from([4, 12]))
    .style(move |_: &Theme| boxed(active.then(|| alpha(Studio::ACCENT, 0.18)), 5.0, None));
    button(face)
        .padding(0)
        .style(bare_button)
        .on_press(Message::Choose(Choice::InputChannel(value)))
        .into()
}

/// `_ActionRow`: an icon, the title, its explanatory subtitle, and a small
/// spinner while its action runs (a chevron otherwise). `on_tap` `None`
/// disables the row.
fn action_row<'a>(
    icon: Icon,
    title: &'a str,
    subtitle: impl Into<String>,
    on_tap: Option<Message>,
    busy: bool,
    corners: Corners,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let trailing: Element<'a, Message> = if busy {
        spinner(14.0, 2.0, Studio::TEXT_SECONDARY)
    } else {
        icons::icon(Icon::ChevronRight, 14.0, Studio::TEXT_FAINT).into()
    };
    let content = row![
        icons::icon(
            icon,
            16.0,
            if enabled {
                Studio::ACCENT
            } else {
                Studio::TEXT_FAINT
            }
        ),
        Space::new().width(12),
        column![
            text(title)
                .size(13.5)
                .font(font::SEMIBOLD)
                .color(if enabled {
                    Studio::TEXT_PRIMARY
                } else {
                    Studio::TEXT_FAINT
                }),
            Space::new().height(2),
            text(subtitle.into())
                .size(11)
                .line_height(1.35)
                .color(Studio::TEXT_FAINT),
        ]
        .width(Length::Fill),
        trailing,
    ]
    .align_y(Vertical::Center);
    tappable(
        container(content).padding(Padding::from([12, 14])),
        on_tap,
        corners,
    )
}

/// `_InfoRow`: a row that takes no taps (the About block's version line).
fn info_row<'a>(icon: Icon, title: &'a str, subtitle: String) -> Element<'a, Message> {
    container(
        row![
            icons::icon(icon, 16.0, Studio::TEXT_FAINT),
            Space::new().width(12),
            column![
                text(title)
                    .size(13.5)
                    .font(font::SEMIBOLD)
                    .color(Studio::TEXT_PRIMARY),
                Space::new().height(2),
                text(subtitle).size(11).color(Studio::TEXT_FAINT),
            ]
            .width(Length::Fill),
        ]
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding::from([12, 14]))
    .into()
}

/// The result toast (home's SnackBar pattern): floating 16 in from the
/// page's edges at its bottom, `Studio.surfaceRaised`, radius 8,
/// `Studio.border`, 13 px text, at `opacity`. (The app draws it too, once
/// the page has handed it over.)
fn toast_bar(message: &str, opacity: f32) -> Element<'_, Infallible> {
    let fade = move |c: Color| alpha(c, c.a * opacity);
    let bar = container(text(message).size(13).color(fade(Studio::TEXT_PRIMARY)))
        .width(Length::Fill)
        .padding(Padding::from([14, 16]))
        .style(move |_: &Theme| container::Style {
            shadow: Shadow {
                color: alpha(Color::BLACK, 0.4 * opacity),
                offset: Vector::new(0.0, 3.0),
                blur_radius: 8.0,
            },
            ..boxed(
                Some(fade(Studio::SURFACE_RAISED)),
                8.0,
                Some(fade(Studio::BORDER)),
            )
        });
    container(bar).width(Length::Fill).padding(16).into()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod through_the_app;
