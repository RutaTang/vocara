//! The settings page, driven as `settings_screen_test.dart` drove it: the
//! Repair-library flow (confirm → `repair_library` → report toast), the
//! diagnostics export (save panel seam → bundle on disk → toast), the
//! Check-for-updates row (manual check, result line, download dialog), the
//! license page, and the device cards' input-channel and headphones
//! choices. Device enumeration, the update fetch and the save panel are
//! fakes; the settings document lives in a temporary folder.
//!
//! The page alone, over the app's services: its tasks run to completion and
//! their messages are fed back; its timers are fired by hand. The license
//! registry is checked with the licenses (`licenses.rs`), the settings
//! service with the service.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::Size;
use iced_test::selector::{Candidate, Target};
use iced_test::Simulator;
use serde_json::{Map, Value};
use tempfile::TempDir;
use vocara_model::config::environment::AppEnvironment;
use vocara_model::diagnostics::{DiagnosticsLog, DiagnosticsOptions};
use vocara_model::models::audio_device::{AudioDevice, AudioTransport};
use vocara_services::app_settings_service::{DeviceQueries, SettingsStorage};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::update_check_service::FetchError;

use super::*;
use crate::router::Navigation;
use crate::ui::core::widgets::confirm_dialog::{Dialog, DialogResult, DialogState, DialogStep};
use crate::ui::home::testing::run_task;
use crate::ui::DialogRequest;

/// Tall enough that every settings section is on screen without scrolling.
const TALL: Size = Size::new(1400.0, 2600.0);

/// The app's fonts (Inter, and the Lucide icons), so text measures as it
/// does in the app.
fn fonts() -> iced::Settings {
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
        let hit = matches!(&candidate, Candidate::Text { content, .. } if *content == label);
        if !hit {
            return None;
        }
        seen += 1;
        (seen == n + 1).then(|| Target::from(candidate))
    }
}

/// The first text containing `fragment`.
fn containing(fragment: &str) -> impl FnMut(Candidate<'_>) -> Option<Target> + Send {
    let fragment = fragment.to_string();
    move |candidate: Candidate<'_>| {
        let hit =
            matches!(&candidate, Candidate::Text { content, .. } if content.contains(&fragment));
        hit.then(|| Target::from(candidate))
    }
}

// --------------------------------------------------------------- fakes

/// The host's devices, as a test edits them (Dart's `enumerateOutputs`,
/// `enumerateInputs` and `inputChannelCount` closures).
#[derive(Default)]
struct Devices {
    outputs: Mutex<Vec<AudioDevice>>,
    inputs: Mutex<Vec<AudioDevice>>,
    channels: AtomicU32,
    /// How many times the outputs were enumerated.
    reads: AtomicUsize,
}

impl Devices {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn set_outputs(&self, devices: Vec<AudioDevice>) {
        *self.outputs.lock().unwrap() = devices;
    }

    fn set_inputs(&self, devices: Vec<AudioDevice>) {
        *self.inputs.lock().unwrap() = devices;
    }

    fn queries(self: &Arc<Self>) -> DeviceQueries {
        let (outs, ins, channels) = (self.clone(), self.clone(), self.clone());
        DeviceQueries {
            enumerate_outputs: Box::new(move || {
                outs.reads.fetch_add(1, Ordering::SeqCst);
                outs.outputs.lock().unwrap().clone()
            }),
            enumerate_inputs: Box::new(move || ins.inputs.lock().unwrap().clone()),
            input_channel_count: Box::new(move |_| channels.channels.load(Ordering::SeqCst)),
        }
    }
}

fn output(uid: &str, name: &str, transport: AudioTransport) -> AudioDevice {
    AudioDevice {
        transport,
        ..AudioDevice::new(uid, name)
    }
}

fn input(uid: &str, name: &str) -> AudioDevice {
    AudioDevice {
        is_input: true,
        ..AudioDevice::new(uid, name)
    }
}

/// A settings document store whose writes all fail (a full or locked disk).
struct FullDisk;

impl SettingsStorage for FullDisk {
    fn read(&self, _: &str, _: &str) -> io::Result<Option<Map<String, Value>>> {
        Ok(None)
    }

    fn write(&self, _: &str, _: &str, _: &Map<String, Value>) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
}

/// An update check against a manifest advertising `latest`, from 1.0.0.
fn updates_to(latest: &str) -> Arc<UpdateCheckService> {
    let body = format!(
        r#"{{"latest": "{latest}", "url": "https://example.com/dl", "notes": "Better vibrato analysis."}}"#
    );
    Arc::new(UpdateCheckService::with_fetcher(
        Box::new(move |_| Ok(body.clone())),
        "1.0.0",
    ))
}

/// What the save panel was asked, and what the bundle hook made.
#[derive(Default)]
struct Calls {
    suggested: Mutex<Vec<String>>,
    bundles: Mutex<Vec<PathBuf>>,
    exports: AtomicUsize,
}

// ------------------------------------------------------------- harness

/// The page over its services, and what it asked of the app.
struct Page {
    screen: SettingsScreen,
    services: Services,
    navigations: Vec<Navigation>,
    dialog: Option<DialogRequest<Message>>,
    _storage: TempDir,
}

/// Makes the page's seams, given the temporary storage folder.
type MakeHooks = Box<dyn FnOnce(&Path) -> SettingsHooks>;

/// Runs on the settings before the page opens.
type Before = Box<dyn FnOnce(&AppSettingsService)>;

/// How a test sets the page up.
struct Setup {
    devices: Arc<Devices>,
    updates: Arc<UpdateCheckService>,
    storage: Option<Arc<dyn SettingsStorage>>,
    hooks: Option<MakeHooks>,
    before: Option<Before>,
}

impl Setup {
    fn new() -> Self {
        Self {
            devices: Devices::new(),
            // Dart's default: the manifest names the running version.
            updates: updates_to("1.0.0"),
            storage: None,
            hooks: None,
            before: None,
        }
    }

    fn devices(mut self, devices: &Arc<Devices>) -> Self {
        self.devices = devices.clone();
        self
    }

    fn updates(mut self, updates: Arc<UpdateCheckService>) -> Self {
        self.updates = updates;
        self
    }

    fn storage(mut self, storage: Arc<dyn SettingsStorage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// The seams, given the temporary storage folder.
    fn hooks(mut self, hooks: impl FnOnce(&Path) -> SettingsHooks + 'static) -> Self {
        self.hooks = Some(Box::new(hooks));
        self
    }

    fn before(mut self, before: impl FnOnce(&AppSettingsService) + 'static) -> Self {
        self.before = Some(Box::new(before));
        self
    }

    /// Mounts the page and lets its first device refresh land.
    fn mount(self) -> Page {
        let storage = TempDir::new().expect("a temporary folder");
        let mut services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
        let local = Arc::new(LocalStorageService::with_root(storage.path()));
        let document: Arc<dyn SettingsStorage> = self.storage.unwrap_or_else(|| local.clone());
        services.app_settings = Arc::new(AppSettingsService::with_queries(
            document,
            self.devices.queries(),
        ));
        services.local_storage = local;
        services.update_check = self.updates;
        if let Some(before) = self.before {
            before(&services.app_settings);
        }
        let hooks = match self.hooks {
            Some(hooks) => hooks(storage.path()),
            None => test_hooks(storage.path()),
        };
        let (screen, first) = SettingsScreen::with_hooks(&services, hooks);
        let mut page = Page {
            screen,
            services,
            navigations: Vec::new(),
            dialog: None,
            _storage: storage,
        };
        page.drive(first);
        page
    }
}

/// Seams that cancel the save panel and make no bundle; the storage folder
/// is the app's own.
fn test_hooks(root: &Path) -> SettingsHooks {
    let root = root.to_string_lossy().into_owned();
    SettingsHooks {
        pick_save_path: Arc::new(|_| Ok(None)),
        export_diagnostics: Arc::new(|| Err(io::Error::other("no bundle in this test"))),
        app_storage_root: Arc::new(move || Ok(root.clone())),
    }
}

impl Page {
    fn settings(&self) -> &AppSettingsService {
        &self.services.app_settings
    }

    /// Runs `task` to completion, feeding back what it reports.
    fn drive(&mut self, task: Task<Message>) {
        for message in run_task(task) {
            self.send(message);
        }
    }

    /// Handles `message`; its work is run to completion.
    fn send(&mut self, message: Message) {
        let task = self.send_held(message);
        self.drive(task);
    }

    /// Handles `message`, handing back its work unrun.
    fn send_held(&mut self, message: Message) -> Task<Message> {
        let update = self.screen.update(message, &self.services);
        if let Some(navigation) = update.navigation {
            self.navigations.push(navigation);
        }
        if let Some(dialog) = update.dialog {
            assert!(self.dialog.is_none(), "one dialog at a time");
            self.dialog = Some(dialog);
        }
        update.task
    }

    /// Closes the open dialog with `result`.
    fn answer(&mut self, result: DialogResult) {
        let dialog = self.dialog.take().expect("a dialog is open");
        self.send((dialog.on_result)(result));
    }

    fn ui(&self) -> Simulator<'_, Message> {
        Simulator::with_size(fonts(), TALL, self.screen.view())
    }

    /// What clicking the `n`-th `label` reports, unhandled.
    fn clicked_nth(&self, label: &str, n: usize) -> Vec<Message> {
        let mut ui = self.ui();
        ui.click(nth(label, n))
            .unwrap_or_else(|e| panic!("{label} #{n}: {e:?}"));
        ui.into_messages().collect()
    }

    fn clicked(&self, label: &str) -> Vec<Message> {
        self.clicked_nth(label, 0)
    }

    /// Clicks `label` and handles what it reports.
    fn click(&mut self, label: &str) {
        self.click_nth(label, 0);
    }

    fn click_nth(&mut self, label: &str, n: usize) {
        for message in self.clicked_nth(label, n) {
            self.send(message);
        }
    }

    /// Clicks the widget with `id` and handles what it reports.
    fn click_id(&mut self, id: iced::widget::Id) {
        let messages: Vec<Message> = {
            let mut ui = self.ui();
            ui.click(id).expect("the widget is on screen");
            ui.into_messages().collect()
        };
        for message in messages {
            self.send(message);
        }
    }

    fn shows(&self, label: &str) -> bool {
        self.ui().find(nth(label, 0)).is_ok()
    }

    fn shows_containing(&self, fragment: &str) -> bool {
        self.ui().find(containing(fragment)).is_ok()
    }

    /// How many texts read `label`.
    fn count(&self, label: &str) -> usize {
        let mut ui = self.ui();
        (0..)
            .take_while(|&n| ui.find(nth(label, n)).is_ok())
            .count()
    }
}

// ---------------------------------------------------------------- the page

#[test]
fn the_about_block_shows_the_app_version_and_every_row() {
    let page = Setup::new().mount();
    assert!(page.shows(&format!(
        "Version {} (build {})",
        UpdateCheckService::APP_VERSION,
        UpdateCheckService::APP_BUILD_NUMBER
    )));
    assert!(page.shows("Check for updates"));
    assert!(page.shows("Licenses"));
    assert!(page.shows("Repair library"));
    assert!(page.shows("Export diagnostics…"));
}

#[test]
fn the_page_lays_out_its_sections_under_the_header() {
    let page = Setup::new().mount();
    assert!(page.shows("Settings"));
    for label in [
        "Audio output",
        "Audio input",
        "Monitoring",
        "Maintenance",
        "About",
    ] {
        assert!(page.shows(label), "{label}");
    }
    // Each device group offers a System default row and a refresh.
    assert_eq!(page.count(SYSTEM_DEFAULT), 2);
    assert!(page.shows("Follow the macOS output device"));
    assert!(page.shows("Follow the macOS input device"));
    assert_eq!(page.count("Refresh devices"), 2);
    // The monitor reverb, at its default, and the engine's rate.
    assert!(page.ui().find(REVERB_SLIDER).is_ok());
    assert!(page.shows("Reverb"));
    assert!(page.shows("20%"));
    assert!(page.shows(
        "The engine runs at 48.0 kHz; devices are resampled to/from this rate automatically. \
         The input device is used for recording and live monitoring."
    ));
    // Nothing to warn about, nothing to mark, nothing to choose between.
    assert!(!page.shows_containing("could not be saved"));
    assert!(!page.shows_containing("Bluetooth"));
    assert!(!page.shows("Headphones only"));
    assert!(!page.shows("Input channel"));
}

#[test]
fn the_back_chevron_pops_or_goes_home() {
    let mut page = Setup::new().mount();
    page.click("Back");
    assert_eq!(page.navigations, [Navigation::PopOrGo(Route::Home)]);
}

// ------------------------------------------------------------ the devices

#[test]
fn the_device_cards_list_the_devices_and_pick_one() {
    let devices = Devices::new();
    devices.set_outputs(vec![
        AudioDevice {
            rate: 48_000,
            is_default: true,
            ..output("spk", "MacBook Pro Speakers", AudioTransport::BuiltIn)
        },
        AudioDevice {
            rate: 44_100,
            ..output("hdmi", "LG Monitor", AudioTransport::Hdmi)
        },
    ]);
    devices.set_inputs(vec![AudioDevice {
        is_default: true,
        ..input("mic", "MacBook Pro Microphone")
    }]);
    let mut page = Setup::new().devices(&devices).mount();
    assert!(page.shows("MacBook Pro Speakers"));
    assert!(page.shows("48.0 kHz · system default"));
    assert!(page.shows("LG Monitor"));
    assert!(page.shows("44.1 kHz"));
    assert!(page.shows("MacBook Pro Microphone"));
    assert!(page.shows("system default"));
    // System default is the choice in both groups.
    assert_eq!(page.count(&glyph(Icon::Check)), 2);

    page.click("LG Monitor");
    assert_eq!(page.settings().output_device_uid().as_deref(), Some("hdmi"));
    page.click("MacBook Pro Microphone");
    assert_eq!(page.settings().input_device_uid().as_deref(), Some("mic"));
    assert_eq!(page.count(&glyph(Icon::Check)), 2);

    // Back to following the system.
    page.click_nth(SYSTEM_DEFAULT, 0);
    assert_eq!(page.settings().output_device_uid(), None);
    page.click_nth(SYSTEM_DEFAULT, 1);
    assert_eq!(page.settings().input_device_uid(), None);
}

#[test]
fn an_unplugged_choice_stays_checked_on_a_disabled_row() {
    let devices = Devices::new();
    devices.set_outputs(vec![
        AudioDevice {
            is_default: true,
            ..output("spk", "MacBook Pro Speakers", AudioTransport::BuiltIn)
        },
        output("usb", "USB Audio CODEC", AudioTransport::Usb),
    ]);
    let mut page = Setup::new().devices(&devices).mount();
    page.click("USB Audio CODEC");
    assert_eq!(page.settings().output_device_uid().as_deref(), Some("usb"));

    // Unplugged: the engine runs on the system default, but the choice is
    // kept, under its last-known name.
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("spk", "MacBook Pro Speakers", AudioTransport::BuiltIn)
    }]);
    page.send(Message::Poll);
    assert!(page.shows("USB Audio CODEC"));
    assert!(page.shows("Unavailable · using system default"));
    assert!(page.shows("Follow the macOS output device · currently active"));
    // Not a live route to pick: its row takes no tap.
    assert!(page.clicked("USB Audio CODEC").is_empty());
    assert_eq!(page.settings().output_device_uid().as_deref(), Some("usb"));
}

#[test]
fn an_unplugged_choice_with_no_known_name_shows_its_uid() {
    let page = Setup::new()
        .before(|settings| settings.set_output_device(Some("AppleUSBAudioEngine:42")))
        .mount();
    assert!(page.shows("AppleUSBAudioEngine:42"));
    assert!(page.shows("Unavailable · using system default"));
}

#[test]
fn refresh_devices_rereads_the_lists_and_one_asked_for_meanwhile_follows() {
    let devices = Devices::new();
    let mut page = Setup::new().devices(&devices).mount();
    assert_eq!(devices.reads.load(Ordering::SeqCst), 1, "once on open");
    devices.set_outputs(vec![output("new", "New Headphones", AudioTransport::Usb)]);
    page.click_nth("Refresh devices", 0);
    assert_eq!(devices.reads.load(Ordering::SeqCst), 2);
    assert!(page.shows("New Headphones"));

    // A poll while that refresh runs is owed, not dropped or doubled.
    let running = page.send_held(Message::Poll);
    let owed = page.send_held(Message::RefreshDevices);
    assert!(run_task(owed).is_empty());
    page.drive(running);
    assert_eq!(devices.reads.load(Ordering::SeqCst), 4);
    assert_eq!(DEVICE_POLL, Duration::from_millis(1500));
}

// Dart: "offers the input-channel choice for a 2-channel input only".
#[test]
fn the_input_channel_choice_is_offered_for_a_2_channel_input_only() {
    let devices = Devices::new();
    devices.channels.store(2, Ordering::SeqCst);
    let mut page = Setup::new().devices(&devices).mount();
    assert!(page.shows("Input channel"));
    assert!(page.shows("Auto"));
    assert_eq!(
        page.settings().input_channel(),
        GraphAudioPlaybackService::INPUT_CHANNEL_AUTO
    );
    page.click("Input 2");
    assert_eq!(
        page.settings().input_channel(),
        GraphAudioPlaybackService::INPUT_CHANNEL_2
    );
    page.click("Input 1");
    assert_eq!(
        page.settings().input_channel(),
        GraphAudioPlaybackService::INPUT_CHANNEL_1
    );

    // A mono input has nothing to choose between.
    devices.channels.store(1, Ordering::SeqCst);
    page.settings().refresh_devices();
    assert!(!page.shows("Input channel"));
}

// Dart: "names the device the input-channel choice applies to". The chosen
// interface is unplugged, so the default records — and a choice made under
// the chosen one's offline row lands on the default.
#[test]
fn the_input_channel_choice_names_the_device_it_applies_to() {
    let devices = Devices::new();
    devices.set_inputs(vec![AudioDevice {
        is_default: true,
        ..input("second", "Second Interface")
    }]);
    devices.channels.store(2, Ordering::SeqCst);
    let page = Setup::new()
        .devices(&devices)
        .before(|settings| settings.set_input_device(Some("unplugged")))
        .mount();
    assert!(page.shows(
        "Second Interface — Auto follows the input you sing into. Pick one to keep a second \
         source out."
    ));
}

// Dart: "offers the headphones mark for an output no name can tell".
#[test]
fn the_headphones_mark_is_offered_for_an_output_no_name_can_tell() {
    let devices = Devices::new();
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("scarlett", "Scarlett 2i2 USB", AudioTransport::Usb)
    }]);
    let mut page = Setup::new().devices(&devices).mount();
    assert!(page.shows("Headphones only"));
    assert!(page.shows_containing("Scarlett 2i2 USB — turn on"));
    assert!(page.shows_containing("Leave it off if speakers ever play from it"));
    assert!(!page.settings().monitor_allowed());

    page.click_id(HEADPHONES_SWITCH);
    assert!(page.settings().output_marked_headphones());
    assert!(page.settings().monitor_allowed());
    // And off again.
    page.click_id(HEADPHONES_SWITCH);
    assert!(!page.settings().output_marked_headphones());

    // The built-in speakers never take the mark.
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("spk", "MacBook Pro Speakers", AudioTransport::BuiltIn)
    }]);
    page.settings().refresh_devices();
    assert!(!page.shows("Headphones only"));
}

#[test]
fn the_mark_goes_to_the_output_the_card_showed() {
    // The tap carries the device the card was drawn for, not whatever
    // renders by the time it lands.
    let devices = Devices::new();
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("scarlett", "Scarlett 2i2 USB", AudioTransport::Usb)
    }]);
    let mut page = Setup::new().devices(&devices).mount();
    let tap: Vec<Message> = {
        let mut ui = page.ui();
        ui.click(HEADPHONES_SWITCH).expect("the switch");
        ui.into_messages().collect()
    };
    assert_eq!(
        tap,
        [Message::Choose(Choice::Headphones {
            uid: "scarlett".into(),
            on: true
        })]
    );
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("focusrite", "Focusrite USB", AudioTransport::Usb)
    }]);
    page.settings().refresh_devices();
    for message in tap {
        page.send(message);
    }
    assert!(!page.settings().output_marked_headphones(), "not this one");
    devices.set_outputs(vec![AudioDevice {
        is_default: true,
        ..output("scarlett", "Scarlett 2i2 USB", AudioTransport::Usb)
    }]);
    page.settings().refresh_devices();
    assert!(page.settings().output_marked_headphones());
}

#[test]
fn a_bluetooth_headset_mic_is_warned_about() {
    let devices = Devices::new();
    let airpods = |is_input| AudioDevice {
        is_default: true,
        is_input,
        is_bluetooth: true,
        ..output("airpods", "AirPods Pro", AudioTransport::Bluetooth)
    };
    devices.set_outputs(vec![airpods(false)]);
    devices.set_inputs(vec![airpods(true)]);
    let page = Setup::new().devices(&devices).mount();
    assert!(page.shows(
        "The microphone is a Bluetooth headset. Bluetooth can't do high-quality sound and a \
         microphone at once, so recording drops the headset to call quality. Pick a different \
         microphone (e.g. the built-in mic)."
    ));
}

#[test]
fn settings_that_did_not_reach_the_disk_say_so() {
    let mut page = Setup::new().storage(Arc::new(FullDisk)).mount();
    assert!(!page.shows_containing("could not be saved"));
    page.send(Message::Choose(Choice::MonitorReverb(0.5)));
    assert!(page.settings().persist_failed());
    assert!(page.shows(
        "Your settings could not be saved to disk. They are in effect now, but the app will \
         start with the previous ones. Check that the disk is not full or locked."
    ));
    // In effect all the same.
    assert!(page.shows("50%"));
}

// ------------------------------------------------------- the monitoring

#[test]
fn choices_apply_one_at_a_time_in_order_and_a_reverb_drag_collapses() {
    let devices = Devices::new();
    devices.channels.store(2, Ordering::SeqCst);
    let mut page = Setup::new().devices(&devices).mount();
    // The first is applied at once; the rest wait their turn.
    let first = page.send_held(Message::Choose(Choice::MonitorReverb(0.3)));
    for later in [
        Message::Choose(Choice::MonitorReverb(0.4)),
        Message::Choose(Choice::MonitorReverb(0.45)),
        Message::Choose(Choice::InputChannel(
            GraphAudioPlaybackService::INPUT_CHANNEL_1,
        )),
        Message::Choose(Choice::MonitorReverb(0.6)),
        Message::Choose(Choice::MonitorReverb(0.7)),
    ] {
        let task = page.send_held(later);
        assert!(run_task(task).is_empty(), "it waits its turn");
    }
    // Drag ticks still waiting collapse into the last; the channel pick
    // between them keeps its place.
    assert_eq!(
        page.screen.pending_choices(),
        [
            Choice::MonitorReverb(0.3),
            Choice::MonitorReverb(0.45),
            Choice::InputChannel(GraphAudioPlaybackService::INPUT_CHANNEL_1),
            Choice::MonitorReverb(0.7),
        ]
    );
    // The slider shows the value asked for while it waits.
    assert!(page.shows("70%"));
    page.drive(first);
    assert!(page.screen.pending_choices().is_empty());
    assert_eq!(page.settings().monitor_reverb_wet(), 0.7);
    assert_eq!(
        page.settings().input_channel(),
        GraphAudioPlaybackService::INPUT_CHANNEL_1
    );
    assert!(page.shows("70%"));
}

// Dart applied each choice the moment it was made: one still waiting when
// the page goes is applied all the same, after the one in flight.
#[test]
fn a_choice_still_waiting_when_the_page_leaves_is_applied_after_the_one_in_flight() {
    let mut page = Setup::new().mount();
    let in_flight = page.send_held(Message::Choose(Choice::MonitorReverb(0.3)));
    let waiting = page.send_held(Message::Choose(Choice::MonitorReverb(0.8)));
    assert!(run_task(waiting).is_empty());
    page.screen.dispose(&page.services);
    // Its turn comes after the one in flight, which has not run yet.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(page.settings().monitor_reverb_wet(), 0.2);
    page.drive(in_flight);
    for _ in 0..500 {
        if page.settings().monitor_reverb_wet() == 0.8 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(page.settings().monitor_reverb_wet(), 0.8);
}

// Settings left with a choice still waiting behind one in flight, then
// opened again and a newer choice made: the newer one lands last. (Each page
// kept its own line once, so the reopened page's choice ran at once and the
// left page's older one landed after it.)
#[test]
fn a_choice_on_a_reopened_page_lands_after_the_left_pages_waiting_ones() {
    let mut page = Setup::new().mount();
    let in_flight = page.send_held(Message::Choose(Choice::MonitorReverb(0.3)));
    let waiting = page.send_held(Message::Choose(Choice::MonitorReverb(0.8)));
    assert!(run_task(waiting).is_empty());
    page.screen.dispose(&page.services);
    // Opened again over the same services, and a newer choice made there.
    let hooks = test_hooks(page._storage.path());
    let (reopened, _first) = SettingsScreen::with_hooks(&page.services, hooks);
    page.screen = reopened;
    let newer = page.send_held(Message::Choose(Choice::MonitorReverb(0.5)));
    let newer = std::thread::spawn(move || run_task(newer));
    // It waits for the left page's choices.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(page.settings().monitor_reverb_wet(), 0.2);
    page.drive(in_flight);
    newer.join().expect("the newer choice applied");
    assert_eq!(page.settings().monitor_reverb_wet(), 0.5);
}

#[test]
fn the_reverb_readout_rounds_the_wet_amount() {
    let mut page = Setup::new().mount();
    page.send(Message::Choose(Choice::MonitorReverb(0.425)));
    assert!(page.shows("43%"));
    page.send(Message::Choose(Choice::MonitorReverb(1.0)));
    assert!(page.shows("100%"));
}

// --------------------------------------------------------- the updates

// Dart: "manual update check surfaces a newer release and its dialog".
#[test]
fn a_manual_update_check_surfaces_a_newer_release_and_its_dialog() {
    let mut page = Setup::new().updates(updates_to("9.9.9")).mount();
    assert!(page.shows("Compares this build against the latest release."));

    // Checking: the row says so and takes no second tap.
    let check = page.send_held(Message::CheckForUpdates);
    assert!(page.shows("Checking…"));
    assert!(page.clicked("Check for updates").is_empty());
    page.drive(check);

    // The download dialog opened; behind it the row shows the result.
    let Some(DialogRequest {
        dialog: Dialog::UpdateDownload(dialog),
        ..
    }) = &page.dialog
    else {
        panic!("the update dialog, through the app's dialog stack")
    };
    assert_eq!(dialog.info().version, "9.9.9");
    let mut state = DialogState::new(Dialog::UpdateDownload(dialog.clone()));
    let later: Vec<_> = {
        let mut ui = Simulator::with_size(fonts(), TALL, state.view());
        assert!(ui.find("Vocara 9.9.9 is available").is_ok());
        assert!(ui.find("Better vibrato analysis.").is_ok());
        ui.click("Later").expect("Later");
        ui.into_messages().collect()
    };
    let steps: Vec<_> = later.into_iter().map(|m| state.step(m)).collect();
    assert_eq!(steps, [DialogStep::Closed(DialogResult::Closed)]);
    page.answer(DialogResult::Closed);
    assert!(page.shows("Vocara 9.9.9 is available. Click for the download."));
    assert!(page.navigations.is_empty());
}

// Dart: "manual update check reports up to date without a dialog".
#[test]
fn a_manual_update_check_reports_up_to_date_without_a_dialog() {
    let mut page = Setup::new().mount();
    page.click("Check for updates");
    assert!(page.shows("You have the latest version."));
    assert!(page.dialog.is_none());
}

// Dart: "a failed manual check degrades to a gentle retry line".
#[test]
fn a_failed_manual_check_degrades_to_a_gentle_retry_line() {
    let offline = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| Err(FetchError("offline".into()))),
        "1.0.0",
    ));
    let mut page = Setup::new().updates(offline).mount();
    page.click("Check for updates");
    assert!(page.shows("Could not reach the update server. You can try again later."));
    assert!(page.dialog.is_none());

    // A manifest that was reached but could not be read is not blamed on
    // the network.
    let unreadable = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(|_| Ok(r#"{"latest": "not a version"}"#.to_string())),
        "1.0.0",
    ));
    let mut page = Setup::new().updates(unreadable).mount();
    page.click("Check for updates");
    assert!(page.shows("The update information could not be read. You can try again later."));
}

#[test]
fn a_check_started_elsewhere_shows_as_checking_until_it_ends() {
    // Home's auto-check, in flight while Settings is open.
    let (release, parked) = std::sync::mpsc::channel::<()>();
    let parked = Mutex::new(parked);
    let updates = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(move |_| {
            let _ = parked.lock().unwrap().recv();
            Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())
        }),
        "1.0.0",
    ));
    updates.set_auto_check(true);
    let mut page = Setup::new().updates(updates.clone()).mount();
    let elsewhere = {
        let updates = updates.clone();
        std::thread::spawn(move || updates.auto_check())
    };
    for _ in 0..500 {
        if updates.checking() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(updates.checking());
    assert!(page.screen.watches_update_check());
    assert!(page.shows("Checking…"));
    assert!(page.clicked("Check for updates").is_empty(), "disabled");

    release.send(()).expect("the check is parked");
    elsewhere.join().expect("the check ends");
    assert!(!page.screen.watches_update_check());
    page.send(Message::UpdateWatch);
    assert!(page.shows("You have the latest version."));
}

/// "Check automatically" reads the answer Home asked for (off until it is
/// given), keeps a new one at once and writes it; turned on, it checks
/// right away, quietly — no dialog: the row and the banner say what it
/// found.
#[test]
fn check_automatically_is_a_switch_that_keeps_its_answer() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&fetches);
    let updates = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(r#"{"latest": "9.9.9", "url": "https://example.com/dl"}"#.to_string())
        }),
        "1.0.0",
    ));
    let mut page = Setup::new().updates(updates.clone()).mount();
    assert!(page.shows("Check automatically"));
    assert!(page.shows(AUTO_CHECK_SUBTITLE));
    assert_eq!(updates.auto_check_preference(), None, "unanswered: off");

    page.click_id(AUTO_CHECK_SWITCH);
    assert_eq!(updates.auto_check_preference(), Some(true));
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "checked right away");
    assert!(page.dialog.is_none(), "quietly");
    assert!(page.shows("Vocara 9.9.9 is available. Click for the download."));
    assert!(page.screen.toast().is_none());

    // Off: kept, and nothing more is fetched.
    page.click_id(AUTO_CHECK_SWITCH);
    assert_eq!(updates.auto_check_preference(), Some(false));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

/// The answer the switch keeps lands in the settings folder, for the next
/// launch to read.
#[test]
fn the_switch_writes_its_answer_for_the_next_launch() {
    let mut page = Setup::new().mount();
    let storage = page.services.local_storage.clone();
    // Kept in the app's storage, as the app's service keeps it (offline
    // here: no test contacts the network).
    let kept_in_storage = || {
        UpdateCheckService::with_fetcher(Box::new(|_| Err(FetchError("offline".into()))), "1.0.0")
            .with_preferences(storage.clone())
    };
    let updates = Arc::new(kept_in_storage());
    page.screen.updates = Arc::clone(&updates);
    page.services.update_check = Arc::clone(&updates);
    page.click_id(AUTO_CHECK_SWITCH);
    let written = storage
        .read("settings", "updates")
        .expect("readable")
        .expect("written");
    assert_eq!(written.get("autoCheck"), Some(&Value::Bool(true)));
    assert_eq!(kept_in_storage().auto_check_preference(), Some(true));
}

/// While an update runs, the row follows it: the download's progress, the
/// install, a failure.
#[test]
fn the_row_follows_an_update_under_way() {
    use crate::ui::home::widgets::update_notice::testing::{image, release, updates};
    use vocara_services::self_update::Phase;
    let fake = updates(true);
    let mut page = Setup::new().mount();
    page.screen.updater = fake.updater.clone();
    fake.updater.prepare();
    fake.updater.start(&release(Some(&image())));
    fake.let_through(1);
    fake.wait_for(&Phase::Downloading {
        done: 1000,
        total: 2500,
    });
    assert!(page.shows("Downloading Vocara 9.9.9… 1 kB of 3 kB"));
    fake.let_through(3);
    fake.settle();
    assert!(page.shows("Installing Vocara 9.9.9… Vocara will relaunch."));

    let failing = updates(false);
    *failing.installer.fails_with.lock().unwrap() = Some("the disk is full".into());
    page.screen.updater = failing.updater.clone();
    failing.updater.start(&release(Some(&image())));
    failing.settle();
    assert!(page.shows("Update failed: the disk is full"));
}

/// A release with its image, on a copy that can replace itself: the row
/// says a click updates, and the dialog the check opens offers "Update now".
#[test]
fn a_release_vocara_can_install_itself_is_offered_as_an_update() {
    use crate::ui::home::widgets::update_notice::testing::{image, manifest, updates};
    let body = manifest("9.9.9", "Better vibrato analysis.", Some(&image()));
    let checks = Arc::new(UpdateCheckService::with_fetcher(
        Box::new(move |_| Ok(body.clone())),
        "1.0.0",
    ));
    let fake = updates(false);
    let mut page = Setup::new().updates(checks).mount();
    page.screen.updater = fake.updater.clone();
    page.click("Check for updates");
    assert!(page.shows("Vocara 9.9.9 is available. Click to update."));
    let Some(DialogRequest {
        dialog: Dialog::UpdateDownload(dialog),
        ..
    }) = &page.dialog
    else {
        panic!("the update dialog")
    };
    let state = DialogState::new(Dialog::UpdateDownload(dialog.clone()));
    let mut ui = Simulator::with_size(fonts(), TALL, state.view());
    assert!(ui.find("Update now").is_ok());
    assert!(ui.find("Open Download Page").is_err());
}

// ----------------------------------------------------------- the repair

// Dart: "Repair library confirms, runs repairLibrary and toasts the report".
#[test]
fn repair_library_confirms_runs_the_repair_and_toasts_the_report() {
    let mut page = Setup::new().mount();
    page.click("Repair library");
    // A plain-language confirmation.
    let Some(DialogRequest {
        dialog: Dialog::Confirm(confirm),
        ..
    }) = &page.dialog
    else {
        panic!("the repair confirmation")
    };
    assert_eq!(confirm.title, "Repair library?");
    assert!(confirm
        .body
        .as_deref()
        .is_some_and(|b| b.contains("Your practices and recordings are not touched.")));
    assert_eq!(confirm.confirm_label, "Repair");
    assert!(!confirm.destructive);
    assert!(matches!(confirm.icon, Some(Icon::Wrench)));
    // One confirmation at a time.
    assert!(page.clicked("Repair library").len() == 1);
    let again = page.send_held(Message::RepairLibrary);
    assert!(run_task(again).is_empty());

    let dialog = page.dialog.take().expect("open");
    let repair = page.send_held((dialog.on_result)(DialogResult::Confirmed(true)));
    assert!(page.screen.repairing());
    assert!(page.shows("Repairing…"));
    assert!(
        page.clicked("Repair library").is_empty(),
        "disabled while it runs"
    );
    page.drive(repair);

    // A clean in-memory store: nothing found, nothing swept.
    assert!(!page.screen.repairing());
    assert_eq!(
        page.screen.toast(),
        Some("Repair finished. Everything looks consistent.")
    );
    assert!(page.shows("Repair finished. Everything looks consistent."));
    assert!(page.shows("Checks the library for inconsistencies and reclaims unused space."));
}

// Dart: "Repair library cancel runs nothing".
#[test]
fn repair_library_cancel_runs_nothing() {
    let mut page = Setup::new().mount();
    page.click("Repair library");
    page.answer(DialogResult::Confirmed(false));
    assert!(!page.screen.repairing());
    assert!(page.screen.toast().is_none());
    assert!(!page.shows_containing("Repair finished"));
    // And it can be asked again.
    page.click("Repair library");
    assert!(page.dialog.is_some());
}

#[test]
fn a_repair_that_fails_says_it_is_safe_to_retry() {
    let mut page = Setup::new().mount();
    page.send(Message::Repaired(Err("SqliteException(11)".into())));
    assert_eq!(page.screen.toast(), Some(REPAIR_FAILED));
    assert!(page.shows("The repair did not finish. It is safe to try again later."));
}

fn report(found: usize, repaired: usize, swept: usize, corrupt: usize) -> RepairReport {
    RepairReport {
        violations_found: found,
        violations_repaired: repaired,
        assets_swept: swept,
        corrupt_pool_files: corrupt,
    }
}

// Dart: repairSummary "a damaged file is not reported as an all-clear". A
// clean database can still hold a bit-rotted pool file: the integrity check
// and verify_pool_integrity are independent, so this triple is reachable —
// and used to read "Everything looks consistent. 1 audio file looks
// damaged", which contradicts itself.
#[test]
fn a_damaged_file_is_not_reported_as_an_all_clear() {
    let s = repair_summary(&report(0, 0, 0, 1));
    assert!(!s.contains("Everything looks consistent"));
    assert!(s.contains("Nothing needed fixing."));
    assert!(s.contains("1 audio file looks damaged"));
}

// Dart: "the all-clear survives when nothing is damaged".
#[test]
fn the_all_clear_survives_when_nothing_is_damaged() {
    assert_eq!(
        repair_summary(&report(0, 0, 0, 0)),
        "Repair finished. Everything looks consistent."
    );
}

// Dart: "more than one damaged file reads as plural".
#[test]
fn more_than_one_damaged_file_reads_as_plural() {
    assert!(repair_summary(&report(0, 0, 0, 2)).contains("2 audio files look damaged"));
}

#[test]
fn a_repair_that_did_something_counts_it() {
    assert_eq!(
        repair_summary(&report(3, 2, 1, 0)),
        "Repair finished: found 3 issues, repaired 2, reclaimed 1 unused file."
    );
    assert_eq!(
        repair_summary(&report(0, 0, 4, 1)),
        "Repair finished: found 0 issues, reclaimed 4 unused files. 1 audio file looks damaged \
         and may not play correctly; nothing was changed."
    );
}

// ------------------------------------------------------ the diagnostics

/// A diagnostics log of the test's own under `dir/log`, holding one entry.
fn own_log(dir: &Path) -> Arc<DiagnosticsLog> {
    let log_dir = dir.join("log");
    std::fs::create_dir_all(&log_dir).expect("the log folder");
    let log = DiagnosticsLog::for_testing(DiagnosticsOptions {
        directory: Some(log_dir),
        ..DiagnosticsOptions::default()
    });
    log.log("settings-test", "one entry for the bundle");
    Arc::new(log)
}

/// Seams that pick `destination` and bundle `log`, recording both.
fn exporting_to(
    destination: PathBuf,
    log: Arc<DiagnosticsLog>,
    calls: &Arc<Calls>,
) -> SettingsHooks {
    let (picks, bundles) = (calls.clone(), calls.clone());
    let destination = destination.to_string_lossy().into_owned();
    SettingsHooks {
        pick_save_path: Arc::new(move |suggested| {
            picks.suggested.lock().unwrap().push(suggested.to_string());
            Ok(Some(destination.clone()))
        }),
        export_diagnostics: Arc::new(move || {
            bundles.exports.fetch_add(1, Ordering::SeqCst);
            let bundle = log.export_archive()?;
            bundles.bundles.lock().unwrap().push(bundle.clone());
            Ok(bundle)
        }),
        app_storage_root: Arc::new(|| {
            Ok(std::env::temp_dir()
                .join("vocara-no-such-root")
                .to_string_lossy()
                .into_owned())
        }),
    }
}

// Dart: "Export diagnostics writes the bundle to the picked path".
#[test]
fn export_diagnostics_writes_the_bundle_to_the_picked_path() {
    let dir = TempDir::new().expect("a temporary folder");
    let destination = dir.path().join("vocara-diagnostics.txt");
    let calls = Arc::new(Calls::default());
    let hooks = exporting_to(destination.clone(), own_log(dir.path()), &calls);
    let mut page = Setup::new().hooks(move |_| hooks).mount();

    // The save panel first, then the export once it gave a destination.
    let pick = page.send_held(Message::ExportDiagnostics);
    let picked = run_task(pick);
    assert_eq!(*calls.suggested.lock().unwrap(), ["vocara-diagnostics.txt"]);
    assert!(!page.screen.exporting(), "not while the panel is up");
    let mut export = Task::none();
    for message in picked {
        export = page.send_held(message);
    }
    assert!(page.screen.exporting());
    assert!(page.shows("Exporting…"));
    assert!(
        page.clicked("Export diagnostics…").is_empty(),
        "disabled while it runs"
    );
    page.drive(export);

    assert!(!page.screen.exporting());
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_SAVED));
    assert!(page.shows_containing("Diagnostics saved"));
    let exported = std::fs::read_to_string(&destination).expect("the bundle was written");
    assert!(exported.contains("Vocara diagnostics bundle"));
    assert!(exported.contains("one entry for the bundle"));
}

// Dart: "Export diagnostics refuses the app's own storage". The library
// database and the take pool live there: replacing one of their files with
// the bundle broke the library.
#[test]
fn export_diagnostics_refuses_the_apps_own_storage() {
    let dir = TempDir::new().expect("a temporary folder");
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).expect("the library folder");
    let db = library.join("vocara.db");
    std::fs::write(&db, "DB").expect("the database");
    let calls = Arc::new(Calls::default());
    let mut hooks = exporting_to(db.clone(), own_log(dir.path()), &calls);
    let root = library.to_string_lossy().into_owned();
    hooks.app_storage_root = Arc::new(move || Ok(root.clone()));
    let mut page = Setup::new().hooks(move |_| hooks).mount();

    page.click("Export diagnostics…");
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_INTO_APP_STORAGE));
    assert!(page.shows_containing("Vocara's own files"));
    assert_eq!(std::fs::read_to_string(&db).expect("the database"), "DB");
    assert_eq!(calls.exports.load(Ordering::SeqCst), 0);
    assert!(!page.screen.exporting());
}

#[test]
fn the_apps_own_storage_is_the_folder_the_services_store_under() {
    let dir = TempDir::new().expect("a temporary folder");
    let calls = Arc::new(Calls::default());
    let log = own_log(dir.path());
    let mut page = Setup::new()
        .hooks(move |root| {
            // Production's root, with the panel and the bundle faked.
            let destination = root.join("vocara-diagnostics.txt");
            SettingsHooks {
                app_storage_root: Arc::new({
                    let root = root.to_string_lossy().into_owned();
                    move || Ok(root.clone())
                }),
                ..exporting_to(destination, log, &calls)
            }
        })
        .mount();
    let production = SettingsHooks::production(&page.services);
    assert_eq!(
        (production.app_storage_root)().expect("the root"),
        page.services
            .local_storage
            .root_dir()
            .expect("the root")
            .to_string_lossy()
    );
    page.click("Export diagnostics…");
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_INTO_APP_STORAGE));
}

// Dart: "a save panel that fails to open says so". Thrown out of the tap,
// it said nothing (a take's export says this).
#[test]
fn a_save_panel_that_fails_to_open_says_so() {
    for pick in [
        Arc::new(|_: &str| Err("panel failed".to_string())) as PickSavePath,
        Arc::new(|_: &str| -> Result<Option<String>, String> { panic!("panel failed") })
            as PickSavePath,
    ] {
        let mut page = Setup::new()
            .hooks(move |root| SettingsHooks {
                pick_save_path: pick,
                ..test_hooks(root)
            })
            .mount();
        page.click("Export diagnostics…");
        assert_eq!(page.screen.toast(), Some("Could not open the save dialog."));
        assert!(page.shows("Could not open the save dialog."));
        assert!(!page.screen.exporting());
    }
}

// Dart: "Export diagnostics leaves no temporary bundle behind". The bundle
// is built in a fresh temp folder and then copied to the picked path; every
// export used to leave that folder and its copy of the logs behind.
#[test]
fn export_diagnostics_leaves_no_temporary_bundle_behind() {
    let dir = TempDir::new().expect("a temporary folder");
    let calls = Arc::new(Calls::default());
    let hooks = exporting_to(
        dir.path().join("vocara-diagnostics.txt"),
        own_log(dir.path()),
        &calls,
    );
    let mut page = Setup::new().hooks(move |_| hooks).mount();
    page.click("Export diagnostics…");
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_SAVED));
    let bundles = calls.bundles.lock().unwrap().clone();
    assert_eq!(bundles.len(), 1);
    assert!(!bundles[0].exists(), "temp bundle");
    assert!(
        !bundles[0].parent().expect("its folder").exists(),
        "its temp folder"
    );
}

#[test]
fn a_copy_that_fails_says_so_and_still_drops_the_bundle() {
    let dir = TempDir::new().expect("a temporary folder");
    let calls = Arc::new(Calls::default());
    let hooks = exporting_to(
        dir.path()
            .join("no-such-folder")
            .join("vocara-diagnostics.txt"),
        own_log(dir.path()),
        &calls,
    );
    let mut page = Setup::new().hooks(move |_| hooks).mount();
    page.click("Export diagnostics…");
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_FAILED));
    assert!(page.shows("Could not save the diagnostics file."));
    let bundles = calls.bundles.lock().unwrap().clone();
    assert_eq!(bundles.len(), 1);
    assert!(
        !bundles[0].exists(),
        "a failed copy leaves no bundle either"
    );
}

#[test]
fn a_bundle_that_cannot_be_made_says_so() {
    let mut page = Setup::new()
        .hooks(|root| SettingsHooks {
            pick_save_path: Arc::new({
                let destination = root.join("out").to_string_lossy().into_owned();
                move |_| Ok(Some(destination.clone()))
            }),
            app_storage_root: Arc::new(|| Err(io::Error::other("no folder to judge by"))),
            ..test_hooks(root)
        })
        .mount();
    page.click("Export diagnostics…");
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_FAILED));
}

// Dart: "Export diagnostics does nothing when the save panel is cancelled".
#[test]
fn export_diagnostics_does_nothing_when_the_save_panel_is_cancelled() {
    let picks = Arc::new(AtomicUsize::new(0));
    let counted = picks.clone();
    let mut page = Setup::new()
        .hooks(move |root| SettingsHooks {
            pick_save_path: Arc::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }),
            ..test_hooks(root)
        })
        .mount();
    page.click("Export diagnostics…");
    assert!(page.screen.toast().is_none());
    assert!(!page.shows_containing("Diagnostics saved"));
    assert!(!page.screen.exporting());
    // One panel at a time, and another once it closed.
    let pick = page.send_held(Message::ExportDiagnostics);
    let second = page.send_held(Message::ExportDiagnostics);
    assert!(run_task(second).is_empty());
    page.drive(pick);
    page.click("Export diagnostics…");
    assert_eq!(picks.load(Ordering::SeqCst), 3);
}

// -------------------------------------------------- the toast, licenses

#[test]
fn a_toast_stays_its_time_and_a_new_one_replaces_it() {
    let mut page = Setup::new().mount();
    page.send(Message::Repaired(Err("x".into())));
    let first = page.screen.toast_seq().expect("a toast");
    page.send(Message::Exported(Exported::Saved));
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_SAVED));
    // The replaced toast's time changes nothing.
    page.send(Message::ToastExpired(first));
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_SAVED));
    let second = page.screen.toast_seq().expect("a toast");
    page.send(Message::ToastExpired(second));
    assert!(page.screen.toast().is_none());
    // (Drawn while it leaves.)
    let left = std::time::Instant::now() + crate::ui::snack::TRANSITION;
    page.send(Message::ToastFrame(left));
    assert!(!page.shows_containing("Diagnostics saved"), "gone");
}

#[test]
fn a_toast_swiped_away_goes_at_once_with_its_time() {
    let mut page = Setup::new().mount();
    page.send(Message::Exported(Exported::Saved));
    let seq = page.screen.toast_seq().expect("a toast");
    // Its `Dismissible` reports it once it has slid out: it goes with no
    // exit of its own (`removeCurrentSnackBar(reason: swipe)`).
    page.send(Message::ToastSwiped(seq));
    assert!(page.screen.toast().is_none());
    assert!(page.screen.toast_timer().is_none(), "its timer with it");
    assert!(
        !page.shows_containing("Diagnostics saved"),
        "not drawn leaving"
    );
    // A stale report takes down nothing.
    page.send(Message::Exported(Exported::Saved));
    page.send(Message::ToastSwiped(seq));
    assert_eq!(page.screen.toast(), Some(DIAGNOSTICS_SAVED));
}

#[test]
fn the_toast_stays_3_5_s_from_the_end_of_its_entrance() {
    use crate::ui::snack::TRANSITION;
    let mut page = Setup::new().mount();
    let before = std::time::Instant::now();
    page.send(Message::Exported(Exported::Saved));
    let after = std::time::Instant::now();
    let timer = page.screen.toast_timer().expect("its timer");
    assert_eq!(TOAST_DURATION, Duration::from_millis(3500));
    assert!(timer.deadline >= before + TRANSITION + TOAST_DURATION);
    assert!(timer.deadline <= after + TRANSITION + TOAST_DURATION);
}

// Dart: "Licenses opens the license page with the hand-maintained
// bundled-component entries registered" (the registry itself is checked in
// licenses.rs).
#[test]
fn licenses_opens_the_license_page_and_its_back_returns_to_settings() {
    let mut page = Setup::new().mount();
    page.click("Licenses");
    assert!(page.screen.licenses_open());
    assert!(page.shows("Powered by iced"));
    assert!(page.shows("Inter (font)"));
    assert!(!page.shows("Check for updates"), "the page covers Settings");
    page.click_id(licenses::BACK);
    assert!(!page.screen.licenses_open());
    assert!(page.shows("Check for updates"));
    assert!(page.navigations.is_empty(), "Settings stays");
}

#[test]
fn the_license_page_slides_in_over_settings_and_back_out() {
    use crate::ui::core::motion::Curve;
    use crate::ui::core::widgets::slide::{cupertino, PAGE_TRANSITION};
    use std::time::Instant;

    let mut page = Setup::new().mount();
    assert!(!page.screen.is_animating(Instant::now()), "idle: no frames");
    let t0 = Instant::now();
    page.send(Message::OpenLicenses);
    assert!(page.screen.licenses_open(), "open at once");
    assert!(
        page.screen.is_animating(Instant::now()),
        "frames while it slides"
    );
    let route = |page: &Page, t| {
        let route = page.screen.licenses_route.as_ref().expect("sliding");
        cupertino(route, t)
    };
    let (moving, covered, _) = route(&page, t0);
    assert!((moving - 1.0).abs() < 1e-3 && covered.abs() < 1e-3, "start");
    let (moving, _, _) = route(&page, t0 + PAGE_TRANSITION / 2);
    let expected = 1.0 - Curve::FastEaseInToSlowEaseOut.transform(0.5);
    assert!((moving - expected).abs() < 0.01, "middle: {moving}");
    let (moving, covered, _) = route(&page, t0 + 2 * PAGE_TRANSITION);
    assert!(
        moving.abs() < 1e-3 && (covered + 1.0 / 3.0).abs() < 1e-3,
        "end"
    );
    page.send(Message::Frame(Instant::now() + PAGE_TRANSITION));
    assert!(page.screen.licenses_route.is_none());

    // Back: it slides out, drawn (not reachable) until it has.
    let t1 = Instant::now();
    page.send(Message::Licenses(licenses::Message::Back));
    assert!(!page.screen.licenses_open(), "closed at once");
    assert!(page.screen.closed_licenses.is_some());
    assert!(
        !page.shows("Powered by iced"),
        "the page going takes no part"
    );
    let (moving, _, _) = route(&page, t1);
    assert!(moving.abs() < 1e-3, "it starts all in");
    let (moving, _, _) = route(&page, t1 + 2 * PAGE_TRANSITION);
    assert!((moving - 1.0).abs() < 1e-3, "and ends all out");
    page.send(Message::Frame(Instant::now() + PAGE_TRANSITION));
    assert!(page.screen.closed_licenses.is_none(), "let go");
    assert!(!page.screen.is_animating(Instant::now()));
}

#[test]
fn the_page_hears_the_settings_notify() {
    let page = Setup::new().mount();
    let feed = SettingsFeed(page.services.app_settings.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime");
    let mut changes = Box::pin(settings_changes(&feed));
    // A change made anywhere (the settings load, an engine's report).
    page.settings().set_monitor_reverb(0.9);
    assert_eq!(rt.block_on(changes.next()), Some(()));
    // Dropped with the page: its listener is removed, and a change after it
    // goes nowhere.
    drop(changes);
    page.settings().set_monitor_reverb(0.1);
    assert_eq!(page.settings().monitor_reverb_wet(), 0.1);
}

// --------------------------------------------------------------- rows

#[test]
fn a_cards_rows_show_no_ink_but_take_taps_under_the_click_cursor() {
    use crate::ui::core::widgets::animated::testing::{Frames, MS};
    use iced::{mouse, Event, Point};

    // Dart's rows were InkWells inside the card's opaque Container, with no
    // Material between: their ink went on the Scaffold's Material, under
    // the card. So neither hovering nor pressing a row tints it.
    let view = || {
        group(vec![
            action_row(
                Icon::Wrench,
                "Repair library",
                "Checks.",
                Some(Message::RepairLibrary),
                false,
            ),
            separator(ICON_ROW_INSET),
            action_row(Icon::FileDown, "Export", "Saves.", None, false),
        ])
    };
    let mut window = Frames::new(Size::new(400.0, 160.0));
    let t0 = Instant::now();
    // Over the first row, clear of its text and chevron.
    let spot = Point::new(300.0, 10.0);
    window.point_at(spot);
    window.frame(view(), &[], t0);
    window.frame(view(), &[], t0 + 200 * MS);
    // Studio.hover is a few steps off the group's color: compare exactly.
    let card_color = Studio::SURFACE.into_rgba8();
    assert_eq!(window.pixel(spot).into_rgba8(), card_color, "hovered");
    assert_eq!(window.interaction(), mouse::Interaction::Pointer);

    let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    let (_, messages) = window.frame_messages(view(), std::slice::from_ref(&press), t0 + 300 * MS);
    assert!(messages.is_empty());
    window.frame(view(), &[], t0 + 600 * MS);
    assert_eq!(window.pixel(spot).into_rgba8(), card_color, "pressed");
    let (_, messages) =
        window.frame_messages(view(), std::slice::from_ref(&release), t0 + 700 * MS);
    assert!(matches!(messages.as_slice(), [Message::RepairLibrary]));

    // A disabled row: no tap, and no click cursor.
    window.point_at(Point::new(300.0, 100.0));
    let (_, messages) = window.frame_messages(view(), &[press, release], t0 + 800 * MS);
    assert!(messages.is_empty());
    assert_ne!(window.interaction(), mouse::Interaction::Pointer);
}
