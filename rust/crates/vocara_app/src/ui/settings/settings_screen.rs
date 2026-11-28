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
//! The About block is also where updates are looked after: "Check for
//! updates" (a manual check, which opens the update dialog when it finds a
//! release; its line follows an update under way — the download's progress,
//! the install, a failure — as the app redraws on every change of the
//! self-update), and "Check automatically", the switch for the answer Home
//! asks for once (until answered, no automatic check runs).
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
//! under the page's toolbar; the studio dialogs (the repair confirmation,
//! the update download dialog) go through the app's dialog stack. A toast
//! still up when Settings leaves stays up over Home for the time it has left
//! (the app's, `app::messenger`), as under Dart's app-wide
//! `ScaffoldMessenger`. A toast fades in and out as Material's floating
//! snack bar did, its 3.5 s counted from the end of its entrance
//! (`ui::snack`).
//!
//! The page is drawn as `DESIGN.md`'s grouped settings, as macOS sets them
//! out: a readable column at most [`CONTENT_WIDTH`] wide, centred; each
//! section a sentence-case title over a rounded `SURFACE` group whose
//! 36 px rows are split by inset 1 px separators; labels at 13 px,
//! explanations at 11 px; a check at the right of the chosen device
//! (`ACCENT_BRIGHT`, the selection), a chevron at the right of a row that
//! opens or runs something; line icons in `TEXT_SECONDARY`. The monitor
//! reverb is the design's thin slider with its tabular percentage, the
//! input channel a segmented control, the headphones mark the design's
//! switch (`controls::toggle_switch`, in place of Material's), and the
//! warnings calm notices in the warning colour. The rows (Dart's
//! `InkWell`s) show no hover or press highlight, as macOS settings rows
//! show none and as in Dart: their ink went on the `Scaffold`'s
//! `Material`, under the opaque card (`tappable`).

use std::collections::{BTreeSet, VecDeque};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crate::ui::core::text;
use iced::alignment::{Horizontal, Vertical};
use iced::futures::channel::mpsc;
use iced::futures::{Stream, StreamExt};
use iced::widget::{button, column, container, row, stack, Column, Space};
use iced::{mouse, Color, Element, Event, Length, Padding, Pixels, Subscription, Task, Theme};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_device::AudioDevice;
use vocara_model::util::export_copy::{export_copy, exports_into};
use vocara_services::app_settings_service::{AppSettingsService, ListenerId};
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::persistence::RepairReport;
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::self_update::{Phase, SelfInstall, SelfUpdate};
use vocara_services::update_check_service::{UpdateCheckService, UpdateStatus};

use super::licenses::{self, LicensesPage};
use crate::app::library::Library;
use crate::router::Route;
use crate::services::Services;
use crate::ui::core::icons::{self, Icon};
use crate::ui::core::motion::{self, Controller};
use crate::ui::core::theme::{self, alpha, bare_button, boxed, font, radius, type_scale, Studio};
use crate::ui::core::widgets::confirm_dialog::ConfirmDialog;
use crate::ui::core::widgets::controls::{segmented, thin_slider, toggle_switch};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::line_icon::{glyphs, line_icon};
use crate::ui::core::widgets::slide::{cupertino, slide, PAGE_TRANSITION};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::toolbar;
use crate::ui::core::widgets::wrap::Wrap;
use crate::ui::home::widgets::update_notice::{self, UpdateDownloadDialog};
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

/// The recording offset slider.
pub const OFFSET_SLIDER: iced::widget::Id = iced::widget::Id::new("settings-offset-slider");

/// The page's scroll view.
pub const SCROLL: iced::widget::Id = iced::widget::Id::new("settings-scroll");

/// The toasts.
pub const REPAIR_FAILED: &str = "The repair did not finish. It is safe to try again later.";
pub const SAVE_PANEL_FAILED: &str = "Could not open the save dialog.";
pub const DIAGNOSTICS_INTO_APP_STORAGE: &str =
    "That folder holds Vocara's own files. Save the diagnostics somewhere else.";
pub const DIAGNOSTICS_SAVED: &str =
    "Diagnostics saved. The file holds technical logs only, never your audio.";
pub const DIAGNOSTICS_FAILED: &str = "Could not save the diagnostics file.";
pub const DEVICES_REFRESHED: &str = "Device lists refreshed.";
pub const AUTO_CHECK_NOT_SAVED: &str =
    "Could not save the update setting. It applies until Vocara quits.";

/// The "Check automatically" row's explanation: what an automatic check
/// sends, and what it does not.
pub const AUTO_CHECK_SUBTITLE: &str =
    "Asks GitHub for a newer Vocara each time it starts. Nothing about your practices is sent.";

/// The "Check automatically" switch (an id to find it by: it has no text).
pub const AUTO_CHECK_SWITCH: iced::widget::Id = iced::widget::Id::new("settings-auto-check-switch");

/// The banners.
const PERSIST_FAILED: &str = "Your settings could not be saved to disk. They are in effect now, \
     but the app will start with the previous ones. Check that the disk is not full or locked.";
const BLUETOOTH_CONFLICT: &str = "The microphone is a Bluetooth headset. Bluetooth can't do \
     high-quality sound and a microphone at once, so recording drops the headset to call \
     quality. Pick a different microphone (e.g. the built-in mic).";

/// The readable column's widest, centred in the page.
pub const CONTENT_WIDTH: f32 = 680.0;
/// The page's padding around the column.
const PAGE_PADDING: Padding = Padding {
    top: 28.0,
    right: 32.0,
    bottom: 40.0,
    left: 32.0,
};
/// The space between two sections, and between two groups of a section.
const GROUP_GAP: f32 = 28.0;
const GROUP_INNER_GAP: f32 = 12.0;
/// The space after a row's icon, and where the rows' text starts in a
/// group with icons (its separators start there).
const ICON_GAP: f32 = 10.0;
const ICON_ROW_INSET: f32 = 12.0 + 14.0 + ICON_GAP;
/// The reverb row's label column, and its value's.
const LABEL_WIDTH: f32 = 80.0;
const VALUE_WIDTH: f32 = 44.0;

/// The device groups' first row: the device the system picks.
pub const SYSTEM_DEFAULT: &str = "System default";

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

/// The version this binary was built as and its build number ("0.4.1
/// (5)"), as the diagnostics bundle writes it when no bundle says.
pub fn built_version() -> String {
    format!(
        "{} ({})",
        UpdateCheckService::APP_VERSION,
        UpdateCheckService::APP_BUILD_NUMBER
    )
}

/// The About block's version line (the license page shows the same).
pub fn version_line() -> String {
    format!(
        "Version {} (build {})",
        UpdateCheckService::APP_VERSION,
        UpdateCheckService::APP_BUILD_NUMBER
    )
}

/// The Check-for-updates row's one-line status: an update under way (its
/// download's progress, its install, its failure), else the last check's
/// result, or what the row does before any check has run.
pub fn update_subtitle(
    updates: &UpdateCheckService,
    updater: &SelfUpdate,
    checking: bool,
) -> String {
    if checking {
        return "Checking…".to_string();
    }
    let version = || {
        updater
            .version()
            .or_else(|| updates.available().map(|i| i.version))
            .unwrap_or_default()
    };
    match updater.phase() {
        Phase::Downloading { done, total } => {
            return format!(
                "{} {}",
                update_notice::downloading_line(&version()),
                update_notice::progress_line(done, total)
            );
        }
        Phase::WaitingForWork => return update_notice::waiting_line(&version()),
        Phase::Installing | Phase::Relaunching => {
            return update_notice::installing_line(&version());
        }
        Phase::Failed(reason) => return update_notice::failed_line(&reason),
        Phase::Idle => {}
    }
    match (updates.status(), updates.available()) {
        (UpdateStatus::UpdateAvailable, Some(info)) => {
            if matches!(updater.self_install(&info), SelfInstall::Blocked(_)) {
                format!(
                    "Vocara {} is available. Click for the download.",
                    info.version
                )
            } else {
                format!("Vocara {} is available. Click to update.", info.version)
            }
        }
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
            // Outside a bundle (no Info.plist to read), the version this
            // binary was built as, which About shows.
            export_diagnostics: Arc::new(|| {
                DiagnosticsLog::instance().export_archive_with_version(&built_version())
            }),
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
    /// The recording offset, in whole ms.
    RecordingOffset(f64),
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
            Choice::RecordingOffset(ms) => settings.set_recording_offset_ms(ms),
            Choice::Headphones { uid, on } => settings.set_output_is_headphones(&uid, on),
        }
    }

    fn reverb(&self) -> Option<f64> {
        match self {
            Choice::MonitorReverb(wet) => Some(*wet),
            _ => None,
        }
    }

    fn offset(&self) -> Option<f64> {
        match self {
            Choice::RecordingOffset(ms) => Some(*ms),
            _ => None,
        }
    }

    /// Whether this and `other` are ticks of one slider's drag (the later
    /// replaces the earlier while both wait).
    fn same_slider(&self, other: &Choice) -> bool {
        (self.reverb().is_some() && other.reverb().is_some())
            || (self.offset().is_some() && other.offset().is_some())
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
    /// The page scrolled to this offset (kept, to put it back where it was
    /// when the license page, which takes its widgets, goes).
    Scrolled(f32),
    /// The settings notified a change.
    SettingsChanged,
    /// A device row, the input channel, the reverb slider or the headphones
    /// switch.
    Choose(Choice),
    /// The reverb slider was let go: nothing more to do (each move already
    /// chose).
    ReverbReleased,
    /// The recording offset slider was let go (as the reverb's).
    OffsetReleased,
    /// The choice in flight was applied.
    Chosen,
    /// "Check for updates".
    CheckForUpdates,
    /// The manual check finished.
    UpdatesChecked,
    /// "Check automatically" switched on or off.
    SetAutoCheck(bool),
    /// The answer was written, or why it could not be.
    AutoCheckSaved(Result<(), String>),
    /// The check a switch turned on ran (it opens no dialog: the banner and
    /// the row say what it found).
    QuietlyChecked,
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
    /// Toast `n` was swiped away.
    ToastSwiped(u64),
}

// ------------------------------------------------------------------ screen

/// The settings page (see the module docs).
pub struct SettingsScreen {
    settings: Arc<AppSettingsService>,
    updates: Arc<UpdateCheckService>,
    updater: Arc<SelfUpdate>,
    library: Arc<Library>,
    finalizer: Arc<RecordingFinalizer>,
    hooks: SettingsHooks,

    /// A device refresh is in flight; another was asked for meanwhile.
    refreshing: bool,
    refresh_owed: bool,
    /// "Refresh devices" was pressed: its refresh ends with a toast.
    refresh_asked: bool,
    /// How far down the page is scrolled.
    scroll: f32,

    /// The choice being applied, and those waiting their turn.
    applying: Option<Choice>,
    queued: VecDeque<Choice>,
    /// The order the choices' threads run in: one line for every Settings
    /// page over this settings service ([`turn_line`]).
    turns: Arc<TurnLine>,

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
            updater: services.self_update.clone(),
            library: services.library.clone(),
            finalizer: RecordingFinalizer::instance(),
            hooks,
            refreshing: false,
            refresh_owed: false,
            refresh_asked: false,
            scroll: 0.0,
            applying: None,
            queued: VecDeque::new(),
            turns: turn_line(&services.app_settings),
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

    /// Whether the screen wants frames at `now`: until a frame at or past
    /// the end has been handled, as only such a frame settles the licences
    /// page's slide (`Message::Frame`). Judged by the clock alone, a last
    /// frame handled late left the page part-way.
    pub fn wants_frames(&self, now: Instant) -> bool {
        self.is_animating(now) || self.is_animating(self.now)
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

    /// The recording offset the slider shows (as [`Self::reverb_shown`]).
    fn offset_shown(&self) -> f64 {
        self.queued
            .iter()
            .rev()
            .chain(self.applying.iter())
            .find_map(Choice::offset)
            .unwrap_or_else(|| self.settings.recording_offset_ms())
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

    /// Queues `choice` behind the one being applied; a slider's drag tick
    /// replaces the one still waiting (its value would be overwritten
    /// before anyone heard it).
    fn choose(&mut self, choice: Choice) -> Task<Message> {
        if self.queued.back().is_some_and(|c| c.same_slider(&choice)) {
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
        let turn = self.turns.next_turn();
        let (turns, settings) = (Arc::clone(&self.turns), Arc::clone(&self.settings));
        Task::perform(
            crate::util::blocking(move || turns.turns.take(turn, || choice.apply(&settings))),
            |_| Message::Chosen,
        )
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
        self.check_task(Message::UpdatesChecked).into()
    }

    /// The update check off the UI thread, then `done`. When it finds a
    /// release, whether this copy can replace itself is found out too
    /// (`SelfUpdate::prepare`), so the row and the dialog offer "Update now"
    /// or the release page from the start.
    fn check_task(&self, done: Message) -> Task<Message> {
        let updates = Arc::clone(&self.updates);
        let updater = Arc::clone(&self.updater);
        Task::perform(
            crate::util::blocking(move || {
                updates.check();
                if updates.available().is_some() {
                    updater.prepare();
                }
            }),
            move |_| done,
        )
    }

    /// "Check automatically" switched: kept at once (the switch reads it
    /// back), written off the UI thread; turned on, a check runs now — what
    /// the user just agreed to — unless one runs or found a release already.
    fn set_auto_check(&mut self, enabled: bool) -> Update<Message> {
        self.updates.set_auto_check(enabled);
        let updates = Arc::clone(&self.updates);
        let save = Task::perform(
            crate::util::blocking(move || updates.save_auto_check().map_err(|e| e.to_string())),
            |saved| Message::AutoCheckSaved(saved.and_then(|s| s)),
        );
        let check = if enabled && !self.update_checking() && self.updates.available().is_none() {
            self.checking_updates = true;
            self.check_task(Message::QuietlyChecked)
        } else {
            Task::none()
        };
        Task::batch([save, check]).into()
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

/// The order choices are applied in for one settings service, shared by
/// every Settings page over it: a page reopened while the one before it
/// still flushes its waiting choices takes its turns after them, so an
/// older choice never lands after a newer one (which a line per page let
/// happen: the reopened page's first choice ran at once, and the left
/// page's flush, still waiting on a slow device switch, applied its older
/// choice last).
#[derive(Debug, Default)]
struct TurnLine {
    turns: Turns,
    given: AtomicU64,
}

impl TurnLine {
    /// The turn of the next thread applying choices.
    fn next_turn(&self) -> u64 {
        self.given.fetch_add(1, Ordering::SeqCst)
    }
}

/// The line of [settings]: the same for every page over the same service
/// (the app has one; each test its own).
fn turn_line(settings: &Arc<AppSettingsService>) -> Arc<TurnLine> {
    type Lines = Vec<(Weak<AppSettingsService>, Arc<TurnLine>)>;
    static LINES: Mutex<Lines> = Mutex::new(Vec::new());
    let mut lines = LINES.lock().unwrap_or_else(PoisonError::into_inner);
    lines.retain(|(service, _)| service.strong_count() > 0);
    if let Some((_, line)) = lines
        .iter()
        .find(|(service, _)| std::ptr::eq(service.as_ptr(), Arc::as_ptr(settings)))
    {
        return Arc::clone(line);
    }
    let line = Arc::new(TurnLine::default());
    lines.push((Arc::downgrade(settings), Arc::clone(&line)));
    line
}

/// The order the choices are applied in across threads: each thread takes
/// its turn, given when a page started it, after every earlier one has had
/// its own (the pages' tasks, then the flush of what still waited when a
/// page left).
#[derive(Debug, Default)]
struct Turns {
    served: Mutex<Served>,
    next: Condvar,
}

/// How far a line of turns has got.
#[derive(Debug, Default)]
struct Served {
    /// How many turns have been served: the next turn is this one.
    count: u64,
    /// Later turns that will never be taken ([`Turns::give_up`]).
    given_up: BTreeSet<u64>,
}

impl Served {
    /// The turn served, and every one given up right after it.
    fn advance(&mut self) {
        self.count += 1;
        while self.given_up.remove(&self.count) {
            self.count += 1;
        }
    }
}

impl Turns {
    /// Waits for `turn`, runs `work`, then hands over to the next turn,
    /// however `work` ends.
    fn take<T>(&self, turn: u64, work: impl FnOnce() -> T) -> T {
        struct Done<'a>(&'a Turns);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.lock().advance();
                self.0.next.notify_all();
            }
        }
        let mut served = self.lock();
        while served.count < turn {
            served = self
                .next
                .wait(served)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(served);
        let _done = Done(self);
        work()
    }

    /// Gives `turn` up: the thread that was to take it never started. The
    /// turns after it go on without it, where they waited for it for the
    /// rest of the run — every later choice on every Settings page, the
    /// line being shared.
    fn give_up(&self, turn: u64) {
        let mut served = self.lock();
        if served.count == turn {
            served.advance();
        } else if served.count < turn {
            served.given_up.insert(turn);
        }
        drop(served);
        self.next.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Served> {
        self.served.lock().unwrap_or_else(PoisonError::into_inner)
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
            Message::Poll => self.refresh().into(),
            Message::RefreshDevices => {
                self.refresh_asked = true;
                self.refresh().into()
            }
            Message::Refreshed => {
                self.refreshing = false;
                if std::mem::take(&mut self.refresh_owed) {
                    return self.refresh().into();
                }
                // The button's refresh says it ran: an unchanged list
                // otherwise looks like a press that did nothing.
                if std::mem::take(&mut self.refresh_asked) {
                    self.show_toast(DEVICES_REFRESHED);
                }
                Update::none()
            }
            Message::Scrolled(y) => {
                self.scroll = y;
                Update::none()
            }
            // The state changed; iced redraws.
            Message::SettingsChanged
            | Message::UpdateWatch
            | Message::DownloadDialogClosed
            | Message::ReverbReleased
            | Message::OffsetReleased => Update::none(),
            Message::Choose(choice) => self.choose(choice).into(),
            Message::Chosen => {
                self.applying = None;
                self.apply_next().into()
            }
            Message::CheckForUpdates => self.check_for_updates(),
            Message::UpdatesChecked => {
                self.checking_updates = false;
                match self.updates.available() {
                    Some(info) => Update::dialog(
                        UpdateDownloadDialog::new(info, Arc::clone(&self.updater)),
                        |_| Message::DownloadDialogClosed,
                    ),
                    None => Update::none(),
                }
            }
            Message::SetAutoCheck(enabled) => self.set_auto_check(enabled),
            Message::AutoCheckSaved(saved) => {
                if saved.is_err() {
                    self.show_toast(AUTO_CHECK_NOT_SAVED);
                }
                Update::none()
            }
            Message::QuietlyChecked => {
                self.checking_updates = false;
                Update::none()
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
                // The page gathers its registry off the UI thread, under a
                // spinner, as its `FutureBuilder` did.
                Update::task(LicensesPage::load().map(Message::Licenses))
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
                        // Settings comes back where it was: the license
                        // page took its widgets, its scroll with them.
                        return Update::task(iced::widget::operation::scroll_to(
                            SCROLL,
                            iced::widget::scrollable::AbsoluteOffset {
                                x: None,
                                y: Some(self.scroll),
                            },
                        ));
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
            Message::ToastSwiped(seq) => {
                self.toasts.swiped(seq, Instant::now());
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
        let turn = self.turns.next_turn();
        let (turns, settings) = (Arc::clone(&self.turns), Arc::clone(&self.settings));
        let spawned = std::thread::Builder::new()
            .name("settings-flush".into())
            .spawn(move || {
                turns.turns.take(turn, || {
                    for choice in waiting {
                        choice.apply(&settings);
                    }
                })
            });
        if let Err(e) = spawned {
            // Its choices are lost, but not the line: the turns after this
            // one must not wait for a thread that never ran.
            self.turns.turns.give_up(turn);
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
                    .id(SCROLL)
                    .on_scroll(|viewport| Message::Scrolled(viewport.absolute_offset().y))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .fading(),
            ]
            .into()
        };
        let route = self.licenses_route.as_ref().map(|r| cupertino(r, self.now));
        // Settings is the first of a stack whether a license page slides
        // over it or not: iced keeps a widget's state only while the view
        // keeps its place, so Settings bare once the page had slid out was
        // a new scroll view, at the top, and the scroll put back was lost.
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
            (None, ..) => stack![slide(settings(), 0.0)].into(),
        };
        let mut layers = stack![container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(toasts) = self.toasts.view() {
            layers = layers.push(toasts.map(toast_message));
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
        subscriptions.push(self.toasts.subscription().map(toast_message));
        subscriptions.push(motion::frames(self.wants_frames(Instant::now())).map(Message::Frame));
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
                .push(notice(PERSIST_FAILED))
                .push(Space::new().height(GROUP_GAP));
        }

        // Audio output, and the headphones mark for it. Only an output no
        // name can tell takes the mark: earbuds need none, speakers take
        // none.
        content = content.push(group_title("Audio output")).push(device_group(
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
                .push(Space::new().height(GROUP_INNER_GAP))
                .push(group(vec![headphones_row(
                    device,
                    s.output_marked_headphones(),
                )]));
        }

        // Audio input, its channel choice and the Bluetooth warning.
        content = content
            .push(Space::new().height(GROUP_GAP))
            .push(group_title("Audio input"))
            .push(device_group(
                &s.input_devices(),
                s.input_device_uid().as_deref(),
                s.input_device_name().as_deref(),
                "Follow the macOS input device",
                Choice::InputDevice,
            ));
        // Only a device with more than one input has a choice to make.
        if s.input_channel_count() >= 2 {
            content = content
                .push(Space::new().height(GROUP_INNER_GAP))
                .push(group(vec![input_channel_row(
                    &s.input_channel_device_name(),
                    s.input_channel(),
                )]));
        }
        if s.bluetooth_in_out_conflict() {
            content = content
                .push(Space::new().height(GROUP_INNER_GAP))
                .push(notice(BLUETOOTH_CONFLICT));
        }
        // Under the device lists, which it is about (it sat under the
        // reverb).
        content = content.push(footnote(engine_footnote(s.engine_sample_rate())));

        // Recording.
        content = content
            .push(Space::new().height(GROUP_GAP))
            .push(group_title("Recording"))
            .push(group(vec![offset_row(self.offset_shown())]))
            .push(footnote(OFFSET_FOOTNOTE.to_string()));

        // Monitoring.
        content = content
            .push(Space::new().height(GROUP_GAP))
            .push(group_title("Monitoring"))
            .push(group(vec![reverb_row(self.reverb_shown())]));

        // Maintenance.
        content = content
            .push(Space::new().height(GROUP_GAP))
            .push(group_title("Maintenance"))
            .push(group(vec![
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
                ),
                separator(ICON_ROW_INSET),
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
                ),
            ]));

        // About.
        let checking = self.update_checking();
        content = content
            .push(Space::new().height(GROUP_GAP))
            .push(group_title("About"))
            .push(group(vec![
                info_row(Icon::Info, "Vocara", version_line()),
                separator(ICON_ROW_INSET),
                action_row(
                    Icon::RefreshCw,
                    "Check for updates",
                    update_subtitle(&self.updates, &self.updater, checking),
                    (!checking).then_some(Message::CheckForUpdates),
                    checking,
                ),
                separator(ICON_ROW_INSET),
                auto_check_row(self.updates.auto_check_enabled()),
                separator(ICON_ROW_INSET),
                action_row(
                    Icon::ScrollText,
                    "Licenses",
                    "Open-source components that ship with Vocara.",
                    Some(Message::OpenLicenses),
                    false,
                ),
            ]));

        // The readable column, centred.
        container(
            container(content)
                .width(Length::Fill)
                .max_width(CONTENT_WIDTH),
        )
        .center_x(Length::Fill)
        .padding(PAGE_PADDING)
        .into()
    }
}

/// The page's toolbar (`widgets::toolbar`): the back button and
/// "Settings".
fn header<'a>() -> Element<'a, Message> {
    toolbar::page_toolbar(
        row![
            toolbar::back_button(Message::Back),
            Space::new().width(4),
            toolbar::title("Settings"),
        ]
        .align_y(Vertical::Center),
    )
}

/// A group's title over its box: sentence case, 13 px semibold
/// `TEXT_PRIMARY`, 8 above the box, 2 in from its edge.
fn group_title<'a>(label: &'a str) -> Element<'a, Message> {
    container(
        studio_text(label)
            .size(type_scale::TITLE)
            .font(font::SEMIBOLD)
            .line_height(Pixels(18.0))
            .color(Studio::TEXT_PRIMARY),
    )
    .padding(Padding {
        bottom: 8.0,
        left: 2.0,
        ..Padding::ZERO
    })
    .into()
}

/// A footnote under a group: 11 px `TEXT_FAINT` on 16 px lines, 8 under
/// the box, as macOS sets one.
fn footnote<'a>(line: String) -> Element<'a, Message> {
    container(
        text(line)
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .color(Studio::TEXT_FAINT),
    )
    .padding(Padding {
        top: 8.0,
        left: 2.0,
        right: 2.0,
        ..Padding::ZERO
    })
    .into()
}

/// A 1 px `BORDER` separator between a group's rows, `inset` in from its
/// left (under the rows' text, as macOS insets them).
fn separator<'a>(inset: f32) -> Element<'a, Message> {
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(Studio::BORDER)),
    )
    .padding(Padding {
        left: inset,
        ..Padding::ZERO
    })
    .into()
}

/// A tappable row of a group: `on_tap` `None` disables it. A tap on an
/// enabled row reports `on_tap`, and the pointer over it is the click
/// cursor (the basic one on a disabled row).
///
/// It shows no hover or press highlight, as macOS settings rows show
/// none, and as in Dart: its rows were `InkWell`s inside the cards' opaque
/// `Container`s with no `Material` between, so the card covered their ink.
fn tappable<'a>(
    content: impl Into<Element<'a, Message>>,
    on_tap: Option<Message>,
) -> Element<'a, Message> {
    button(content)
        .padding(0)
        .width(Length::Fill)
        .style(bare_button)
        .on_press_maybe(on_tap)
        .into()
}

/// A group (`DESIGN.md`: a rounded box): `Studio::SURFACE`, radius 8, an
/// inset `BORDER` outline, its rows inside it.
fn group(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    container(Column::with_children(rows).width(Length::Fill))
        .width(Length::Fill)
        .style(|_: &Theme| boxed(Some(Studio::SURFACE), radius::CARD, Some(Studio::BORDER)))
        .into()
}

/// A row's padding: 12 at the sides, 9 above and below a 18 px line (a
/// 36 px row).
const ROW_PADDING: Padding = Padding {
    top: 9.0,
    right: 12.0,
    bottom: 9.0,
    left: 12.0,
};

/// A row's title (13 px, `color`) and, when there is one, its subtitle
/// (11 px `TEXT_FAINT`) under it.
fn titled<'a>(title: String, subtitle: String, color: Color) -> Column<'a, Message> {
    let mut label = column![studio_text(title)
        .size(type_scale::TITLE)
        .line_height(Pixels(18.0))
        .color(color)
        .ellipsis(1)];
    if !subtitle.is_empty() {
        label = label.push(
            text(subtitle)
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .color(Studio::TEXT_FAINT),
        );
    }
    label.width(Length::Fill)
}

/// The device group: "System default", every device, the unplugged
/// persisted device when there is one, and "Refresh devices".
fn device_group<'a>(
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
    // "System default" follows whatever the OS picks. While the selected
    // device is unplugged it is the effective route, so say so.
    rows.push(device_row(
        SYSTEM_DEFAULT.to_string(),
        if missing.is_some() {
            format!("{default_subtitle} · currently active")
        } else {
            default_subtitle.to_string()
        },
        explicit.is_none(),
        Some(Message::Choose(choose(None))),
    ));
    for d in devices {
        rows.push(separator(ROW_PADDING.left));
        rows.push(device_row(
            d.name.clone(),
            device_subtitle(d),
            explicit == Some(d.uid.as_str()),
            Some(Message::Choose(choose(Some(d.uid.clone())))),
        ));
    }
    if let Some(uid) = missing {
        // The unplugged persisted selection: still the checked choice, but
        // disabled (not tappable) and marked as unavailable. Labelled by its
        // last-known name, else its raw uid.
        rows.push(separator(ROW_PADDING.left));
        rows.push(device_row(
            last_known_name
                .filter(|n| !n.is_empty())
                .unwrap_or(uid)
                .to_string(),
            "Unavailable · using system default".to_string(),
            true,
            None,
        ));
    }
    rows.push(separator(ROW_PADDING.left));
    rows.push(refresh_row());
    group(rows)
}

/// `_DeviceRow`: the name and its subtitle, and a check at the right when
/// selected (`ACCENT_BRIGHT`: the selection). A row with no tap is the
/// unplugged persisted selection: dimmed, its check included.
fn device_row<'a>(
    title: String,
    subtitle: String,
    selected: bool,
    on_tap: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let mut content = row![titled(
        title,
        subtitle,
        if enabled {
            Studio::TEXT_PRIMARY
        } else {
            Studio::TEXT_FAINT
        },
    )]
    .spacing(12)
    .align_y(Vertical::Center);
    if selected {
        content = content.push(icons::icon(
            Icon::Check,
            14.0,
            if enabled {
                Studio::ACCENT_BRIGHT
            } else {
                Studio::TEXT_FAINT
            },
        ));
    }
    tappable(container(content).padding(ROW_PADDING), on_tap)
}

/// `_RefreshRow`: "Refresh devices", its glyph at the right.
fn refresh_row<'a>() -> Element<'a, Message> {
    tappable(
        container(
            row![
                studio_text("Refresh devices")
                    .size(type_scale::TITLE)
                    .line_height(Pixels(18.0))
                    .color(Studio::TEXT_SECONDARY)
                    .width(Length::Fill),
                icons::icon(Icon::RotateCw, 13.0, Studio::TEXT_FAINT),
            ]
            .align_y(Vertical::Center),
        )
        .padding(ROW_PADDING),
        Some(Message::RefreshDevices),
    )
}

/// A calm notice (a setting not saved, a Bluetooth headset mic): the
/// warning glyph in `WARN_TEXT` and the text in `TEXT_SECONDARY`, on the
/// warning colour at 6 % with a faint outline of it.
fn notice<'a>(message: &'a str) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::TriangleAlert, 14.0, Studio::WARN_TEXT),
            text(message)
                .size(type_scale::BODY)
                .line_height(Pixels(18.0))
                .color(Studio::TEXT_SECONDARY)
                .width(Length::Fill),
        ]
        .spacing(10)
        .align_y(Vertical::Top),
    )
    .width(Length::Fill)
    .padding(Padding::from([10, 12]))
    .style(|_: &Theme| {
        boxed(
            Some(alpha(Studio::WARN, 0.06)),
            radius::CARD,
            Some(alpha(Studio::WARN, 0.22)),
        )
    })
    .into()
}

/// The monitor reverb row: "Reverb", the thin slider (`DESIGN.md`, Slider)
/// and its percentage, tabular, right-aligned. The slider's values are
/// whole percents, as the Material slider's 0.01 steps were.
fn reverb_row<'a>(wet: f64) -> Element<'a, Message> {
    let value = wet.clamp(0.0, 1.0);
    let reverb = container(thin_slider(
        value as f32,
        |wet| {
            Message::Choose(Choice::MonitorReverb(
                (f64::from(wet) * 100.0).round() / 100.0,
            ))
        },
        Message::ReverbReleased,
    ))
    .id(REVERB_SLIDER)
    .width(Length::Fill);
    container(
        row![
            container(
                studio_text("Reverb")
                    .size(type_scale::TITLE)
                    .line_height(Pixels(18.0))
                    .color(Studio::TEXT_PRIMARY),
            )
            .width(LABEL_WIDTH),
            reverb,
            studio_text(format!("{}%", (wet * 100.0).round() as i64))
                .size(type_scale::BODY)
                .line_height(Pixels(18.0))
                .tabular()
                .color(Studio::TEXT_SECONDARY)
                .width(VALUE_WIDTH)
                .align_x(Horizontal::Right),
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(ROW_PADDING)
    .into()
}

/// What the recording offset is for, under its row.
pub const OFFSET_FOOTNOTE: &str = "Takes are lined up with the backing by the delay the app \
     measures. If yours still land late, move this right; if early, left.";

/// The recording offset row: "Take offset", the thin slider over
/// [`AppSettingsService::RECORDING_OFFSET_MIN_MS`] to `MAX_MS` in whole ms,
/// and its signed value, as the reverb row lays its percentage out.
fn offset_row<'a>(ms: f64) -> Element<'a, Message> {
    let (lo, hi) = (
        AppSettingsService::RECORDING_OFFSET_MIN_MS,
        AppSettingsService::RECORDING_OFFSET_MAX_MS,
    );
    let ms = ms.clamp(lo, hi);
    let at = |ms: f64| ((ms - lo) / (hi - lo)) as f32;
    // Bipolar: filled from no offset, either way; a double click puts it
    // back there.
    let slider = thin_slider(
        at(ms),
        move |v| {
            Message::Choose(Choice::RecordingOffset(
                (lo + f64::from(v) * (hi - lo)).round(),
            ))
        },
        Message::OffsetReleased,
    )
    .origin(at(0.0));
    let slider = container(Wrap::<Message, Option<iced::advanced::mouse::Click>>::new(
        slider,
        Box::new(|last, event, bounds, cursor, shell| {
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                let Some(at) = cursor.position_over(bounds) else {
                    return;
                };
                let click = iced::advanced::mouse::Click::new(at, mouse::Button::Left, *last);
                if click.kind() == iced::advanced::mouse::click::Kind::Double {
                    shell.publish(Message::Choose(Choice::RecordingOffset(0.0)));
                }
                *last = Some(click);
            }
        }),
    ))
    .id(OFFSET_SLIDER)
    .width(Length::Fill);
    container(
        row![
            container(
                studio_text("Take offset")
                    .size(type_scale::TITLE)
                    .line_height(Pixels(18.0))
                    .color(Studio::TEXT_PRIMARY),
            )
            .width(LABEL_WIDTH),
            slider,
            studio_text(offset_label(ms))
                .size(type_scale::BODY)
                .line_height(Pixels(18.0))
                .tabular()
                .color(Studio::TEXT_SECONDARY)
                .width(VALUE_WIDTH + 12.0)
                .align_x(Horizontal::Right),
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(ROW_PADDING)
    .into()
}

/// The footnote under the device lists: the engine's rate (in kHz, one
/// decimal), and what the input device is for.
pub fn engine_footnote(sample_rate: f64) -> String {
    format!(
        "The engine runs at {} kHz; devices are resampled to/from this rate \
         automatically. The input device is used for recording and live monitoring.",
        to_string_as_fixed(sample_rate / 1000.0, 1)
    )
}

/// The recording offset as the row shows it: whole ms, signed ("+20 ms",
/// "0 ms", "-15 ms").
pub fn offset_label(ms: f64) -> String {
    let ms = ms.round() as i64;
    if ms > 0 {
        format!("+{ms} ms")
    } else {
        format!("{ms} ms")
    }
}

/// Whether only headphones play from the current output — wired ones on an
/// audio interface, or a pair whose name does not say so. On a marked
/// output, takes skip echo cancelling (which dulled held in-key notes on an
/// echo the mic never heard) and the monitor is allowed. Offered only for a
/// device that can be headphones of its own (see
/// `AppSettingsService::output_markable`), and plain about what goes wrong
/// on speakers. A row with the explanation under its title and the switch
/// at its right.
fn headphones_row<'a>(device: AudioDevice, marked: bool) -> Element<'a, Message> {
    let label = column![
        studio_text("Headphones only")
            .size(type_scale::TITLE)
            .line_height(Pixels(18.0))
            .color(Studio::TEXT_PRIMARY),
        text(format!(
            "{} — turn on if you always listen on headphones here. Recordings then skip echo \
             cancelling, which keeps long notes clear, and hearing yourself is allowed. \
             Remembered for this output.",
            device.name
        ))
        .size(type_scale::LABEL)
        .line_height(Pixels(type_scale::LINE))
        .color(Studio::TEXT_FAINT),
        Space::new().height(2),
        text(
            "Leave it off if speakers ever play from it: takes would pick up the backing track, \
             and hearing yourself could howl."
        )
        .size(type_scale::LABEL)
        .line_height(Pixels(type_scale::LINE))
        .color(Studio::TEXT_SECONDARY),
    ]
    .width(Length::Fill);
    // The device this row shows, not whatever renders when the tap lands.
    let toggle = Message::Choose(Choice::Headphones {
        uid: device.uid,
        on: !marked,
    });
    container(
        row![
            label,
            container(toggle_switch(marked, Some(toggle))).id(HEADPHONES_SWITCH),
        ]
        .spacing(16)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(ROW_PADDING)
    .into()
}

/// Which input(s) of a multi-input device (an audio interface) are the
/// voice: Auto follows the live one — a mic on either input records at full
/// level, two live inputs blend — while Input 1 or Input 2 keeps the other
/// out (say, a guitar DI on input 2). A row with the segmented control
/// (`DESIGN.md`) at its right.
///
/// `device` is the device the choice applies to ("" when unknown): the one
/// recording, which is the system default while the chosen device is
/// unplugged.
fn input_channel_row<'a>(device: &str, mode: u32) -> Element<'a, Message> {
    let named = if device.is_empty() {
        String::new()
    } else {
        format!("{device} — ")
    };
    let label = titled(
        "Input channel".to_string(),
        format!(
            "{named}Auto follows the input you sing into. Pick one to keep a second source out."
        ),
        Studio::TEXT_PRIMARY,
    );
    let segments = segmented(
        &[
            (GraphAudioPlaybackService::INPUT_CHANNEL_AUTO, "Auto"),
            (GraphAudioPlaybackService::INPUT_CHANNEL_1, "Input 1"),
            (GraphAudioPlaybackService::INPUT_CHANNEL_2, "Input 2"),
        ],
        mode,
        |value| Message::Choose(Choice::InputChannel(value)),
    );
    container(row![label, segments].spacing(16).align_y(Vertical::Center))
        .width(Length::Fill)
        .padding(ROW_PADDING)
        .into()
}

/// `_ActionRow`: a 14 px line icon in `TEXT_SECONDARY`, the title, its
/// explanatory subtitle, and a small spinner while its action runs (a
/// chevron otherwise). `on_tap` `None` disables the row.
fn action_row<'a>(
    icon: Icon,
    title: &'a str,
    subtitle: impl Into<String>,
    on_tap: Option<Message>,
    busy: bool,
) -> Element<'a, Message> {
    let enabled = on_tap.is_some();
    let trailing: Element<'a, Message> = if busy {
        spinner(12.0, 1.6, Studio::TEXT_SECONDARY)
    } else {
        line_icon(glyphs::CHEVRON_RIGHT, 12.0, Studio::TEXT_FAINT).into()
    };
    let content = row![
        icons::icon(
            icon,
            14.0,
            if enabled {
                Studio::TEXT_SECONDARY
            } else {
                Studio::TEXT_DISABLED
            }
        ),
        titled(
            title.to_string(),
            subtitle.into(),
            if enabled {
                Studio::TEXT_PRIMARY
            } else {
                Studio::TEXT_FAINT
            },
        ),
        trailing,
    ]
    .spacing(ICON_GAP)
    .align_y(Vertical::Center);
    tappable(container(content).padding(ROW_PADDING), on_tap)
}

/// "Check automatically": whether Vocara asks GitHub for a newer release
/// each time it starts — the answer to the question Home asks once
/// (`UpdateCheckService::auto_check_preference`; unanswered, it reads off).
/// The headphones row's shape: an icon, the title over its explanation,
/// and the design's switch at its right.
fn auto_check_row<'a>(on: bool) -> Element<'a, Message> {
    container(
        row![
            icons::icon(Icon::Clock, 14.0, Studio::TEXT_SECONDARY),
            titled(
                "Check automatically".to_string(),
                AUTO_CHECK_SUBTITLE.to_string(),
                Studio::TEXT_PRIMARY,
            ),
            container(toggle_switch(on, Some(Message::SetAutoCheck(!on)))).id(AUTO_CHECK_SWITCH),
        ]
        .spacing(ICON_GAP)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(ROW_PADDING)
    .into()
}

/// `_InfoRow`: a row that takes no taps (the About block's version line):
/// its icon, its title and, at the right, its value in `TEXT_SECONDARY`.
fn info_row<'a>(icon: Icon, title: &'a str, value: String) -> Element<'a, Message> {
    container(
        row![
            icons::icon(icon, 14.0, Studio::TEXT_SECONDARY),
            studio_text(title)
                .size(type_scale::TITLE)
                .line_height(Pixels(18.0))
                .color(Studio::TEXT_PRIMARY)
                .width(Length::Fill),
            studio_text(value)
                .size(type_scale::BODY)
                .line_height(Pixels(18.0))
                .tabular()
                .color(Studio::TEXT_SECONDARY),
        ]
        .spacing(ICON_GAP)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(ROW_PADDING)
    .into()
}
/// The page's message for what its toasts report.
fn toast_message(event: SnackEvent) -> Message {
    match event {
        SnackEvent::Expired(seq) => Message::ToastExpired(seq),
        SnackEvent::Frame(at) => Message::ToastFrame(at),
        SnackEvent::Swiped(seq) => Message::ToastSwiped(seq),
    }
}

/// The result toast: the shared floating snack bar
/// (`snack::floating_bar`, set 16 in from the page's edges at its bottom
/// by the queue, `snack::FLOATING_MARGIN`), as Home's. (The app draws it
/// too, once the page has handed it over.)
fn toast_bar(message: &str) -> Element<'_, Infallible> {
    crate::ui::snack::floating_bar(message)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod through_the_app;
