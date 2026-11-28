//! Global, persisted app settings: the audio output and input devices, the
//! input channel per device, the headphones marks, the record and monitor
//! gains and the monitor reverb (port of
//! `lib/data/services/app_settings_service.dart`).
//!
//! It brokers between the persisted choice and the currently active engine:
//! enumeration is static (works with no song open), while applying a choice
//! is forwarded to the attached [`GraphAudioPlaybackService`] and persisted
//! for the next engine. Dart's `ChangeNotifier` becomes
//! [`AppSettingsService::add_listener`]: listeners run wherever Dart called
//! `notifyListeners`, after the state lock is released, at most once per
//! release (several Dart notifies inside one call reach a listener once; the
//! panel redraws from the state either way).
//!
//! # Threading
//!
//! The service is `Send + Sync` and every method is synchronous. One state
//! mutex holds what the Dart object owned, and each method's synchronous body
//! runs under it, as Dart's ran on one isolate; the device queries and the
//! engine calls it makes are made under it too (Dart made them inline). The
//! storage I/O is not: [`AppSettingsService::load`] reads with the lock
//! released (Dart's `await _storage.read`), which is where a user's choice
//! can race it, and each persist snapshots the document under the lock and
//! writes it with the lock released. Persists run one at a time (a mutex
//! taken before the state lock), so documents land in the order they were
//! snapshotted, as Dart's per-file write chain ordered them.
//!
//! The methods that persist (every setter but the live gain ones, and
//! [`AppSettingsService::load`]) block on the disk: the app calls them off
//! the UI thread. Listeners run on the caller's thread with no lock held, so
//! one may read the settings or even call a setter.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Map, Value};
use vocara_model::diagnostics::DiagnosticsLog;
use vocara_model::models::audio_device::{AudioDevice, AudioTransport};
use vocara_model::util::dart_num::{clamp_f64, pow_f64};

use crate::engine;
use crate::graph_audio_playback_service::GraphAudioPlaybackService;
use crate::local_storage_service::LocalStorageService;

/// Where the settings document lives: the [`LocalStorageService`] calls the
/// service uses, as a seam (a test serves the document from memory, parks
/// its read or fails it).
pub trait SettingsStorage: Send + Sync {
    /// The document, `None` when there is none to keep; an error when it is
    /// there but could not be read.
    fn read(&self, collection: &str, id: &str) -> io::Result<Option<Map<String, Value>>>;

    /// Writes the whole document.
    fn write(&self, collection: &str, id: &str, json: &Map<String, Value>) -> io::Result<()>;
}

impl SettingsStorage for LocalStorageService {
    fn read(&self, collection: &str, id: &str) -> io::Result<Option<Map<String, Value>>> {
        LocalStorageService::read(self, collection, id)
    }

    fn write(&self, collection: &str, id: &str, json: &Map<String, Value>) -> io::Result<()> {
        LocalStorageService::write(self, collection, id, json)
    }
}

/// The host device queries, injectable for tests (Dart's `enumerateOutputs`,
/// `enumerateInputs` and `inputChannelCount` constructor arguments).
pub struct DeviceQueries {
    pub enumerate_outputs: Box<dyn Fn() -> Vec<AudioDevice> + Send + Sync>,
    pub enumerate_inputs: Box<dyn Fn() -> Vec<AudioDevice> + Send + Sync>,
    /// The input channel count of the device with this uid ("" = the system
    /// default input); 0 when absent or unreadable.
    pub input_channel_count: Box<dyn Fn(&str) -> u32 + Send + Sync>,
}

impl DeviceQueries {
    /// The live CoreAudio queries ([`engine::enumerate_output_devices`] and
    /// its siblings).
    pub fn live() -> DeviceQueries {
        DeviceQueries {
            enumerate_outputs: Box::new(engine::enumerate_output_devices),
            enumerate_inputs: Box::new(engine::enumerate_input_devices),
            input_channel_count: Box::new(engine::input_device_channel_count),
        }
    }
}

impl Default for DeviceQueries {
    fn default() -> DeviceQueries {
        DeviceQueries::live()
    }
}

/// A registered listener, to remove it with
/// [`AppSettingsService::remove_listener`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn() + Send + Sync>;

// One JSON document under the app-support `settings` collection.
const COLLECTION: &str = "settings";
const DOC_ID: &str = "audio";
const K_OUTPUT_UID: &str = "outputDeviceUid";
const K_INPUT_UID: &str = "inputDeviceUid";
const K_OUTPUT_NAME: &str = "outputDeviceName";
const K_INPUT_NAME: &str = "inputDeviceName";
const K_REVERB_WET: &str = "monitorReverbWet";
const K_RECORD_GAIN_DB: &str = "recordGainDb";
const K_MONITOR_GAIN: &str = "monitorGain";
const K_RECORDING_OFFSET_MS: &str = "recordingOffsetMs";
const K_INPUT_CHANNELS: &str = "inputChannelByUid";
const K_HEADPHONE_OUTPUTS: &str = "headphoneOutputUids";

// Fixed monitor-reverb character; the user controls only the wet amount.
const MONITOR_REVERB_ROOM: f64 = 0.6;
const MONITOR_REVERB_DAMPING: f64 = 0.5;

const DEFAULT_MONITOR_REVERB_WET: f64 = 0.2;
// Mic-input gain (= the engine's default): +12 dB records a typical take with
// tracking headroom (peaks ~-6 dBFS); the take file is 32-bit float, so even a
// hotter fader never clips the recording.
const DEFAULT_RECORD_GAIN_DB: f64 = 12.0;
// Hear-yourself level (linear); 0 = off (e.g. on a speaker).
const DEFAULT_MONITOR_GAIN: f64 = 0.0;

/// Name fragments of loudspeakers: the generic words, and the common
/// Bluetooth speaker lines whose names never say so (UE BOOM, JBL Flip...).
/// Only unambiguous ones: a headphone line sharing a speaker brand's name
/// would lose its monitor.
const SPEAKER_NAMES: &[&str] = &[
    "speaker",
    "soundbar",
    "homepod",
    "sonos",
    "boom", // UE BOOM / MEGABOOM / WONDERBOOM / HYPERBOOM
    "jbl flip",
    "jbl charge",
    "jbl clip",
    "jbl go",
    "jbl xtreme",
    "jbl pulse",
    "partybox",
    "soundlink flex",
    "soundlink micro",
    "soundlink revolve",
    "soundlink color",
    "beosound",
    "echo dot",
    // A display's own speakers ("LG UltraFine Display Audio", Apple's
    // "Display Audio"), which can come in over USB and so pass the transport
    // rule.
    "display audio",
];

/// Global, persisted app settings (see the module doc).
pub struct AppSettingsService {
    storage: Arc<dyn SettingsStorage>,
    queries: DeviceQueries,
    state: Mutex<State>,
    /// Persists run one at a time, in snapshot order. Taken before `state`.
    persist_order: Mutex<()>,
    listeners: Mutex<Vec<(ListenerId, Listener)>>,
    next_listener: AtomicU64,
}

/// What the Dart object owned.
struct State {
    output_device_uid: Option<String>,
    input_device_uid: Option<String>,
    /// Last-known display names of the persisted devices, persisted alongside
    /// the uids so the settings panel can label an unplugged choice by name.
    output_device_name: Option<String>,
    input_device_name: Option<String>,
    monitor_reverb_wet: f64,
    record_gain_db: f64,
    monitor_gain: f64,
    /// See [`AppSettingsService::recording_offset_ms`].
    recording_offset_ms: f64,
    output_devices: Vec<AudioDevice>,
    input_devices: Vec<AudioDevice>,
    engine: Option<Arc<GraphAudioPlaybackService>>,
    /// The input-channel choice (an `INPUT_CHANNEL_*` mode) per uid of the
    /// device capturing (see [`State::input_key`]), in insertion order (the
    /// document's). A device with no entry is on Auto. Per device because it
    /// describes how THAT device is wired: input 2 of one interface is not
    /// input 2 of the next.
    input_channel_by_uid: Vec<(String, u32)>,
    /// The uids whose choice the user made this session (see `user_chosen`):
    /// [`AppSettingsService::load`] adopts the stored choice of every other
    /// device.
    input_channels_chosen: HashSet<String>,
    input_channel_count: u32,
    /// The outputs the user has said only headphones play from (see
    /// [`AppSettingsService::set_output_is_headphones`]), by uid, in
    /// insertion order: what no name can tell, as with wired headphones on
    /// the jack or an audio interface.
    headphone_outputs: Vec<String>,
    /// The uids whose mark the user set or cleared this session: a load
    /// adopts only the others'.
    headphone_marks_chosen: HashSet<String>,
    /// The persisted keys the user has already chosen this session. A load
    /// is fire-and-forget (the app starts it and drops it), so it can resolve
    /// at ANY moment, including after the singer has moved a fader. A field
    /// the user set is never adopted from the document: otherwise the change
    /// is reverted on screen, re-pushed to the engine, and then written back
    /// as the old value by the next persist, which serializes every field at
    /// once. Keyed by the document's own keys, so a setter and its load guard
    /// cannot drift apart.
    user_chosen: HashSet<&'static str>,
    /// Load reads still parked, and whether a persist was held back by one.
    loads_in_flight: u32,
    persist_deferred: bool,
    /// The document's last read failed: the load ran on defaults.
    read_failed: bool,
    /// The rendering device when the lists do not know it, read afresh
    /// WITHOUT adopting that read: adopting it would be a device refresh,
    /// whose switches (a persisted device back, the input channel) on the
    /// 1 s monitor tick reopened the mic in the middle of a take.
    unlisted_output: Option<AudioDevice>,
    /// The fresh read for an unlisted rendering device came back empty: the
    /// list could not be read (the engine renders to SOMETHING), so nothing
    /// is known about that device.
    unlisted_unreadable: bool,
    /// Last uid the engine reported rendering to. CACHED, not read per
    /// access: [`AppSettingsService::monitor_allowed`] is read from view
    /// builds (the MON fader's enabled state), and resolving it natively walks
    /// the device list. Refreshed wherever the routing can have moved, which
    /// is also why the ENGINE owns the hard mute rather than trusting this: a
    /// stale value here is bounded by one tick, and the engine's own reroute
    /// check has no such window.
    actual_output_uid: String,
    /// The echo-free output last named to the attached engine; `None` = not
    /// named yet.
    echo_free_pushed: Option<String>,
    /// The outputs as [`AppSettingsService::refresh_output_devices`] last read
    /// them, for the engine attached now.
    watched_outputs: Vec<AudioDevice>,
    persist_failed: bool,
    /// Listeners to notify once the lock is released.
    notify: bool,
    /// A last-known name changed: persist once the lock is released (Dart's
    /// fire-and-forget `_persist()` from the device refresh).
    persist_names: bool,
}

impl State {
    fn new() -> State {
        State {
            output_device_uid: None,
            input_device_uid: None,
            output_device_name: None,
            input_device_name: None,
            monitor_reverb_wet: DEFAULT_MONITOR_REVERB_WET,
            record_gain_db: DEFAULT_RECORD_GAIN_DB,
            monitor_gain: DEFAULT_MONITOR_GAIN,
            recording_offset_ms: 0.0,
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            engine: None,
            input_channel_by_uid: Vec::new(),
            input_channels_chosen: HashSet::new(),
            input_channel_count: 0,
            headphone_outputs: Vec::new(),
            headphone_marks_chosen: HashSet::new(),
            user_chosen: HashSet::new(),
            loads_in_flight: 0,
            persist_deferred: false,
            read_failed: false,
            unlisted_output: None,
            unlisted_unreadable: false,
            actual_output_uid: String::new(),
            echo_free_pushed: None,
            watched_outputs: Vec::new(),
            persist_failed: false,
            notify: false,
            persist_names: false,
        }
    }
}

// --- Device helpers ------------------------------------------------------------

/// The device with `uid` from a list, or `None` (also for a missing or empty
/// uid).
fn by_uid<'a>(list: &'a [AudioDevice], uid: Option<&str>) -> Option<&'a AudioDevice> {
    let uid = uid.filter(|u| !u.is_empty())?;
    list.iter().find(|d| d.uid == uid)
}

/// An explicit pick that isn't in the live list (i.e. unplugged).
fn is_missing(list: &[AudioDevice], uid: Option<&str>) -> bool {
    uid.is_some_and(|u| !u.is_empty()) && by_uid(list, uid).is_none()
}

/// The system default in `devices` ("" when none is marked).
fn default_in(devices: &[AudioDevice]) -> String {
    devices
        .iter()
        .find(|d| d.is_default)
        .map_or_else(String::new, |d| d.uid.clone())
}

/// The first device marked default.
fn first_default(list: &[AudioDevice]) -> Option<&AudioDevice> {
    list.iter().find(|d| d.is_default)
}

/// The effective device (an explicit pick, else the system default) from a
/// list. A pick that is absent is no device at all.
fn effective<'a>(list: &'a [AudioDevice], uid: Option<&str>) -> Option<&'a AudioDevice> {
    if uid.is_some_and(|u| !u.is_empty()) {
        return by_uid(list, uid);
    }
    first_default(list)
}

/// The device a pick resolves to in `list`: the pick while connected, else
/// the system default (what the engine uses on System Default, or while the
/// pick is unplugged, which read as no device at all, and no warning, while
/// the take recorded from a headset's mic).
fn in_use<'a>(list: &'a [AudioDevice], uid: Option<&str>) -> Option<&'a AudioDevice> {
    by_uid(list, uid).or_else(|| first_default(list))
}

/// A change signature over a device set (the fields the panel renders), to
/// skip no-op rebuilds.
fn device_sig(ds: &[AudioDevice]) -> String {
    ds.iter()
        .map(|d| format!("{}:{}:{}:{}", d.uid, d.is_default, d.rate, d.name))
        .collect::<Vec<_>>()
        .join("|")
}

/// The persisted device reappeared: absent from the previous list, present
/// in the fresh one. The previous list must be non-empty so the very first
/// enumeration (load or attach, where the apply already pushed the choice)
/// doesn't count.
fn returned(before: &[AudioDevice], after: &[AudioDevice], uid: Option<&str>) -> bool {
    !before.is_empty() && is_missing(before, uid) && !is_missing(after, uid)
}

fn named_speaker(d: &AudioDevice) -> bool {
    let n = d.name.to_lowercase();
    SPEAKER_NAMES.iter().any(|s| n.contains(s))
}

/// Whether `d` can carry a headphones mark at all. Decided by its TRANSPORT
/// (see [`AudioTransport::can_carry_headphones_mark`]): its name is
/// translated by the system, so a name rule let a German Mac's built-in
/// speakers ("MacBook Pro-Lautsprecher") or its one device for speakers and
/// jack ("Integrierte Ausgabe") be marked, and after the headphones came out,
/// every take recorded the whole backing and the monitor fed the speakers. A
/// device named as a speaker is refused on top.
fn can_carry_mark(d: &AudioDevice) -> bool {
    d.transport.can_carry_headphones_mark() && !named_speaker(d)
}

fn is_surely_headphones(out: Option<&AudioDevice>) -> bool {
    let Some(out) = out.filter(|o| !named_speaker(o)) else {
        return false;
    };
    let n = out.name.to_lowercase();
    n.contains("airpod") || n.contains("earpod") || n.contains("earbud") || n.contains("buds")
}

fn is_headphone_like(out: Option<&AudioDevice>) -> bool {
    // Unknown: the safe default, off.
    let Some(d) = out.filter(|o| !named_speaker(o)) else {
        return false;
    };
    let n = d.name.to_lowercase();
    // Only a name that says so, or the uid of Apple silicon's headphone jack,
    // which (unlike its name, "Externe Kopfhörer" on a German Mac) is never
    // translated. Not "any Bluetooth device": Bluetooth is speakers as often
    // as headphones, a speaker missing from SPEAKER_NAMES fed the monitor
    // straight back into the mic (it is kept out of the AEC reference, so
    // nothing cancels it), and on macOS the transport was never even read
    // until the headphones mark needed it. Other headphones are the user's to
    // mark (`set_output_is_headphones`), all but the jack's: see
    // `is_headphone_jack`.
    is_surely_headphones(out)
        || is_headphone_jack(d)
        || n.contains("headphone")
        || n.contains("earphone")
}

/// Apple silicon's built-in headphone jack, listed as a device of its own
/// while something is plugged in. The monitor is allowed on it (as on its
/// English name, "External Headphones"), but it can never carry a headphones
/// mark: it can't tell headphones from desk speakers plugged into it, and an
/// unplug-and-swap can happen while the app isn't looking, so a mark kept for
/// it would one day apply to speakers.
fn is_headphone_jack(d: &AudioDevice) -> bool {
    d.transport == AudioTransport::BuiltIn && d.uid.to_lowercase().contains("headphone")
}

fn db_to_linear(db: f64) -> f64 {
    pow_f64(10.0, db / 20.0)
}

/// A stored string, `None` for any other value.
fn loaded_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

/// The stored per-device input-channel choices, keeping only well-formed
/// entries (a known mode, an integer, under a string uid): one corrupt entry
/// costs that device's choice, not the others'.
fn loaded_channels(v: Option<&Value>) -> Vec<(String, u32)> {
    let Some(Value::Object(map)) = v else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(uid, mode)| {
            // An integer, as Dart's `is int` (a stored `2.0` is a double).
            let mode = mode.as_i64()?;
            let auto = i64::from(GraphAudioPlaybackService::INPUT_CHANNEL_AUTO);
            let two = i64::from(GraphAudioPlaybackService::INPUT_CHANNEL_2);
            (auto..=two)
                .contains(&mode)
                .then(|| (uid.clone(), mode as u32))
        })
        .collect()
}

/// Non-finite is not a preference: Dart's `num.clamp` resolves NaN and
/// Infinity through `compareTo` and returns the range MAXIMUM, so a corrupt
/// `1e400` would arrive as 40 dB of input gain or a fully open monitor rather
/// than the default.
fn loaded_double(v: Option<&Value>, fallback: f64) -> f64 {
    v.and_then(Value::as_f64)
        .filter(|d| d.is_finite())
        .unwrap_or(fallback)
}

/// Dart's `runtimeType` of a storage failure, for the log (never its
/// message, which carries the path).
fn error_type(_: &io::Error) -> &'static str {
    "FileSystemException"
}

impl State {
    /// The device the mic captures from, as the key of its input-channel
    /// choice: the chosen one while it is connected, else the system default
    /// (what the engine records from when the selection is System Default,
    /// or when the chosen device is unplugged). Keyed by the selection
    /// instead, a choice made for one interface followed the default onto
    /// the next.
    fn input_key(&self) -> String {
        self.key_in(&self.input_devices)
    }

    /// [`State::input_key`] against `devices`.
    fn key_in(&self, devices: &[AudioDevice]) -> String {
        let uid = self.input_device_uid.as_deref();
        if let Some(u) = uid.filter(|u| !u.is_empty() && !is_missing(devices, Some(u))) {
            return u.to_string();
        }
        let fallback = default_in(devices);
        if !fallback.is_empty() {
            fallback
        } else {
            uid.unwrap_or("").to_string()
        }
    }

    /// The choice for the device that `key` names.
    fn channel_for(&self, key: &str) -> u32 {
        self.input_channel_by_uid
            .iter()
            .find(|(uid, _)| uid == key)
            .map_or(GraphAudioPlaybackService::INPUT_CHANNEL_AUTO, |(_, m)| *m)
    }

    /// Sets `key`'s choice, keeping its place in the document (a new one
    /// goes last), as a Dart map's `[]=` did.
    fn set_channel(&mut self, key: &str, mode: u32) {
        match self
            .input_channel_by_uid
            .iter_mut()
            .find(|(uid, _)| uid == key)
        {
            Some(entry) => entry.1 = mode,
            None => self.input_channel_by_uid.push((key.to_string(), mode)),
        }
    }

    fn input_channel(&self) -> u32 {
        self.channel_for(&self.input_key())
    }

    fn add_mark(&mut self, uid: &str) {
        if !self.headphone_outputs.iter().any(|u| u == uid) {
            self.headphone_outputs.push(uid.to_string());
        }
    }

    /// A mark counts only where the panel can show it, so one nobody could
    /// see to clear never stays in force: a device since renamed as a
    /// speaker, or newly on the speaker list.
    fn marked_headphones(&self, d: Option<&AudioDevice>) -> bool {
        d.is_some_and(|d| self.headphone_outputs.contains(&d.uid) && can_carry_mark(d))
    }

    /// The device to judge feedback safety on: what the engine is actually
    /// rendering to, else the effective selection. `None` (judged unsafe)
    /// while the device list cannot be read at all: the selection is then no
    /// stand-in for what renders (on System Default it is the device the
    /// default has just LEFT, and the engine's own mute does not cover a
    /// default that moves).
    fn rendering_device(&self) -> Option<&AudioDevice> {
        if !self.actual_output_uid.is_empty() {
            if let Some(d) = self.known_rendering() {
                return Some(d);
            }
            if self.unlisted_unreadable {
                return None;
            }
        }
        effective(&self.output_devices, self.output_device_uid.as_deref())
    }

    /// The device the engine reports rendering to, from the lists or, when
    /// they have never seen it (a speaker turned on since this page
    /// enumerated, which the system default then moved to), from the fresh
    /// read `refresh_actual_output` took. `None` when neither knows it.
    fn known_rendering(&self) -> Option<&AudioDevice> {
        let uid = self.actual_output_uid.as_str();
        if uid.is_empty() {
            return None;
        }
        by_uid(&self.output_devices, Some(uid))
            .or_else(|| self.unlisted_output.as_ref().filter(|u| u.uid == uid))
    }

    fn monitor_allowed(&self) -> bool {
        let d = self.rendering_device();
        self.marked_headphones(d) || is_headphone_like(d)
    }

    fn echo_free_output(&self) -> String {
        match self.known_rendering() {
            Some(d) if self.marked_headphones(Some(d)) || is_surely_headphones(Some(d)) => {
                d.uid.clone()
            }
            _ => String::new(),
        }
    }

    fn output_markable(&self) -> bool {
        self.rendering_device()
            .is_some_and(|d| can_carry_mark(d) && !is_surely_headphones(Some(d)))
    }

    /// Whether the output `uid` last named echo-free would still be judged so
    /// if it rendered: a name held through a mic open must not outlive the
    /// mark it came from.
    fn still_echo_free(&self, uid: &str) -> bool {
        if uid.is_empty() {
            return true;
        }
        let d = by_uid(&self.output_devices, Some(uid))
            .or_else(|| self.unlisted_output.as_ref().filter(|u| u.uid == uid));
        d.is_some_and(|d| self.marked_headphones(Some(d)) || is_surely_headphones(Some(d)))
    }

    fn conflict(
        &self,
        outputs: &[AudioDevice],
        inputs: &[AudioDevice],
        bound_input: Option<&str>,
    ) -> bool {
        let inp = by_uid(inputs, bound_input)
            .or_else(|| in_use(inputs, self.input_device_uid.as_deref()));
        let Some(inp) = inp.filter(|i| i.is_bluetooth) else {
            return false;
        };
        // Its own output side: the same uid, or, should the system list a
        // headset's two directions under two uids, one Bluetooth product
        // named alike.
        outputs
            .iter()
            .any(|o| o.uid == inp.uid || (o.is_bluetooth && o.name == inp.name))
    }

    /// The monitor gain actually sent to the engine: the fader value on
    /// headphones, forced to 0 on a speaker so a persisted or raised fader
    /// can never feed back after switching output.
    fn apply_monitor_gain(&self) {
        if let Some(e) = &self.engine {
            e.set_monitor_gain(if self.monitor_allowed() {
                self.monitor_gain
            } else {
                0.0
            });
        }
    }

    /// Everything the engine derives from the rendering device: the
    /// monitor's feedback safety and the echo canceller's route
    /// ([`AppSettingsService::echo_free_output`]).
    fn apply_output_route(&mut self) {
        self.apply_monitor_gain();
        // Named only on a change (and on every attach): the engine re-checks
        // it against what renders at each of its own polls, so repeating it
        // each tick adds nothing. Nor is it withdrawn while the engine reports
        // nothing rendering (the mic opening): the engine applies it only
        // while that very device renders, so keeping it is safe, and
        // withdrawing it left the canceller adapting on the earbuds once the
        // unit bound, until the next tick named them again.
        if self.actual_output_uid.is_empty() {
            if let Some(held) = &self.echo_free_pushed {
                if self.still_echo_free(held) {
                    return;
                }
            }
        }
        let echo_free = self.echo_free_output();
        if self.echo_free_pushed.as_deref() != Some(echo_free.as_str()) {
            if let Some(e) = &self.engine {
                e.set_echo_free(&echo_free);
            }
            self.echo_free_pushed = Some(echo_free);
        }
    }

    /// Tracks device renames: while a persisted device is connected, keeps
    /// its stored display name current (the name labels the offline
    /// pseudo-row in the panel).
    fn sync_last_known_names(&mut self) {
        let mut dirty = false;
        if let Some(out) = by_uid(&self.output_devices, self.output_device_uid.as_deref()) {
            if self.output_device_name.as_deref() != Some(out.name.as_str()) {
                self.output_device_name = Some(out.name.clone());
                dirty = true;
            }
        }
        if let Some(inp) = by_uid(&self.input_devices, self.input_device_uid.as_deref()) {
            if self.input_device_name.as_deref() != Some(inp.name.as_str()) {
                self.input_device_name = Some(inp.name.clone());
                dirty = true;
            }
        }
        // Fire-and-forget in Dart: a failed name persist only degrades the
        // offline label, and the persist absorbs the failure itself.
        if dirty {
            self.persist_names = true;
        }
    }

    /// Takes `doc`'s value for every field the user has not chosen this
    /// session (`None`: the defaults).
    fn adopt(&mut self, doc: Option<&Map<String, Value>>) {
        let get = |key: &str| doc.and_then(|d| d.get(key));
        // A uid and its last-known display name are ONE choice: adopt or skip
        // them together, or an adopted name would label the user's new device.
        if !self.user_chosen.contains(K_OUTPUT_UID) {
            self.output_device_uid = loaded_string(get(K_OUTPUT_UID));
            self.output_device_name = loaded_string(get(K_OUTPUT_NAME));
        }
        if !self.user_chosen.contains(K_INPUT_UID) {
            self.input_device_uid = loaded_string(get(K_INPUT_UID));
            self.input_device_name = loaded_string(get(K_INPUT_NAME));
        }
        // Clamp every loaded value to the same range the live setters enforce:
        // a tampered or out-of-range stored value must not reach the engine
        // verbatim (e.g. a huge recordGainDb is an enormous input gain,
        // monitorGain > 1 howling feedback) and must keep the mixer fader math
        // valid.
        if !self.user_chosen.contains(K_REVERB_WET) {
            self.monitor_reverb_wet = clamp_f64(
                loaded_double(get(K_REVERB_WET), DEFAULT_MONITOR_REVERB_WET),
                0.0,
                1.0,
            );
        }
        if !self.user_chosen.contains(K_RECORD_GAIN_DB) {
            self.record_gain_db = clamp_f64(
                loaded_double(get(K_RECORD_GAIN_DB), DEFAULT_RECORD_GAIN_DB),
                AppSettingsService::RECORD_GAIN_MIN_DB,
                AppSettingsService::RECORD_GAIN_MAX_DB,
            );
        }
        if !self.user_chosen.contains(K_MONITOR_GAIN) {
            self.monitor_gain = clamp_f64(
                loaded_double(get(K_MONITOR_GAIN), DEFAULT_MONITOR_GAIN),
                0.0,
                1.0,
            );
        }
        if !self.user_chosen.contains(K_RECORDING_OFFSET_MS) {
            self.recording_offset_ms = clamp_f64(
                loaded_double(get(K_RECORDING_OFFSET_MS), 0.0),
                AppSettingsService::RECORDING_OFFSET_MIN_MS,
                AppSettingsService::RECORDING_OFFSET_MAX_MS,
            );
        }
        for (uid, mode) in loaded_channels(get(K_INPUT_CHANNELS)) {
            if !self.input_channels_chosen.contains(&uid) {
                self.set_channel(&uid, mode);
            }
        }
        if let Some(Value::Array(marks)) = get(K_HEADPHONE_OUTPUTS) {
            for uid in marks.iter().filter_map(Value::as_str) {
                if !self.headphone_marks_chosen.contains(uid) {
                    self.add_mark(uid);
                }
            }
        }
    }

    /// The whole settings document, as Dart's map literal wrote it.
    fn document(&self) -> Map<String, Value> {
        let opt = |s: &Option<String>| s.clone().map_or(Value::Null, Value::String);
        let mut doc = Map::new();
        doc.insert(K_OUTPUT_UID.into(), opt(&self.output_device_uid));
        doc.insert(K_INPUT_UID.into(), opt(&self.input_device_uid));
        doc.insert(K_OUTPUT_NAME.into(), opt(&self.output_device_name));
        doc.insert(K_INPUT_NAME.into(), opt(&self.input_device_name));
        doc.insert(K_REVERB_WET.into(), Value::from(self.monitor_reverb_wet));
        doc.insert(K_RECORD_GAIN_DB.into(), Value::from(self.record_gain_db));
        doc.insert(K_MONITOR_GAIN.into(), Value::from(self.monitor_gain));
        let channels: Map<String, Value> = self
            .input_channel_by_uid
            .iter()
            .map(|(uid, mode)| (uid.clone(), Value::from(*mode)))
            .collect();
        doc.insert(K_INPUT_CHANNELS.into(), Value::Object(channels));
        doc.insert(
            K_HEADPHONE_OUTPUTS.into(),
            Value::Array(
                self.headphone_outputs
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        // Not a key Dart wrote: last, after every one it did.
        doc.insert(
            K_RECORDING_OFFSET_MS.into(),
            Value::from(self.recording_offset_ms),
        );
        doc
    }

    fn set_persist_failed(&mut self, failed: bool) {
        if self.persist_failed == failed {
            return;
        }
        self.persist_failed = failed;
        self.notify = true;
    }
}

impl AppSettingsService {
    /// Record/input gain range (dB) for the mic-input fader.
    pub const RECORD_GAIN_MIN_DB: f64 = 0.0;
    pub const RECORD_GAIN_MAX_DB: f64 = 40.0;

    /// The recording offset's range (ms): wide enough for a Bluetooth
    /// headset's codec or a speaker across the room, narrow enough that a
    /// slip of the slider cannot move a take by a beat.
    pub const RECORDING_OFFSET_MIN_MS: f64 = -250.0;
    pub const RECORDING_OFFSET_MAX_MS: f64 = 250.0;

    /// The settings over `storage`, with the live CoreAudio device queries.
    pub fn new(storage: Arc<dyn SettingsStorage>) -> AppSettingsService {
        AppSettingsService::with_queries(storage, DeviceQueries::live())
    }

    /// The settings with injected device queries (tests, and a host with no
    /// devices to list).
    pub fn with_queries(
        storage: Arc<dyn SettingsStorage>,
        queries: DeviceQueries,
    ) -> AppSettingsService {
        AppSettingsService {
            storage,
            queries,
            state: Mutex::new(State::new()),
            persist_order: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
            next_listener: AtomicU64::new(0),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // --- Listeners ------------------------------------------------------------

    /// Registers a callback run on every change (Dart's `addListener`).
    pub fn add_listener(&self, listener: Box<dyn Fn() + Send + Sync>) -> ListenerId {
        let id = ListenerId(self.next_listener.fetch_add(1, Ordering::Relaxed));
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, Arc::from(listener)));
        id
    }

    /// Unregisters a callback (Dart's `removeListener`).
    pub fn remove_listener(&self, id: ListenerId) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(l, _)| *l != id);
    }

    fn notify_listeners(&self) {
        let listeners: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for l in listeners {
            l();
        }
    }

    /// Releases the state lock, then runs what the locked section asked for:
    /// the listeners, and a last-known-name persist.
    fn settle(&self, mut st: MutexGuard<'_, State>) {
        let notify = std::mem::take(&mut st.notify);
        let persist = std::mem::take(&mut st.persist_names);
        drop(st);
        if notify {
            self.notify_listeners();
        }
        if persist {
            self.persist();
        }
    }

    /// Releases the state lock inside a persist, collecting the notify it
    /// asked for into `notify` (the persist runs the listeners once its
    /// ordering lock is released too, so a listener may call a setter). A
    /// name-persist request is left for the next settle (none arises inside a
    /// persist).
    fn release(mut st: MutexGuard<'_, State>, notify: &mut bool) {
        *notify |= std::mem::take(&mut st.notify);
    }

    // --- Reading ----------------------------------------------------------------

    /// The persisted output device uid, or `None` = system default.
    pub fn output_device_uid(&self) -> Option<String> {
        self.state().output_device_uid.clone()
    }

    /// The persisted input device uid, or `None` = system default.
    pub fn input_device_uid(&self) -> Option<String> {
        self.state().input_device_uid.clone()
    }

    /// Last-known display name of the persisted output device, or `None`
    /// when following the system default. Kept current while the device is
    /// connected so the panel can still label the choice after it's
    /// unplugged.
    pub fn output_device_name(&self) -> Option<String> {
        self.state().output_device_name.clone()
    }

    /// Last-known display name of the persisted input device (see
    /// [`AppSettingsService::output_device_name`]).
    pub fn input_device_name(&self) -> Option<String> {
        self.state().input_device_name.clone()
    }

    /// True when an output device is persisted but absent from the live list
    /// (unplugged). The engine falls back to the system default; the uid
    /// stays persisted so the choice is restored when the device returns.
    pub fn output_device_missing(&self) -> bool {
        let st = self.state();
        is_missing(&st.output_devices, st.output_device_uid.as_deref())
    }

    /// True when an input device is persisted but absent from the live list
    /// (see [`AppSettingsService::output_device_missing`]).
    pub fn input_device_missing(&self) -> bool {
        let st = self.state();
        is_missing(&st.input_devices, st.input_device_uid.as_deref())
    }

    /// The currently enumerated output devices.
    pub fn output_devices(&self) -> Vec<AudioDevice> {
        self.state().output_devices.clone()
    }

    /// The currently enumerated input devices.
    pub fn input_devices(&self) -> Vec<AudioDevice> {
        self.state().input_devices.clone()
    }

    /// The selected input device (uid and name), or `None` when following the
    /// system default. Used to point recording and live analysis at the same
    /// device as the engine (not the OS default).
    pub fn selected_input_device(&self) -> Option<AudioDevice> {
        let st = self.state();
        let uid = st.input_device_uid.as_deref().filter(|u| !u.is_empty())?;
        Some(
            by_uid(&st.input_devices, Some(uid))
                .cloned()
                .unwrap_or_else(|| {
                    AudioDevice::new(uid, st.input_device_name.clone().unwrap_or_default())
                }),
        )
    }

    /// Which input(s) of the current input device the engine records and
    /// monitors: an `INPUT_CHANNEL_*` mode, Auto unless chosen for this
    /// device.
    pub fn input_channel(&self) -> u32 {
        self.state().input_channel()
    }

    /// The name of the device the input-channel choice applies to: the one
    /// capturing, which is the system default while the chosen device is
    /// unplugged (the panel names it, or a choice made under the offline row
    /// landed on another interface unseen).
    pub fn input_channel_device_name(&self) -> String {
        let st = self.state();
        by_uid(&st.input_devices, Some(&st.input_key()))
            .map_or_else(String::new, |d| d.name.clone())
    }

    /// The current input device's channel count (0 = unknown): the panel
    /// offers the input-channel choice only from 2. Read from the device
    /// itself, so it is known with no song open and no mic running; refreshed
    /// with the device list.
    pub fn input_channel_count(&self) -> u32 {
        self.state().input_channel_count
    }

    /// Monitor reverb amount (0..1): how much space the singer hears on
    /// their own voice.
    pub fn monitor_reverb_wet(&self) -> f64 {
        self.state().monitor_reverb_wet
    }

    /// Record/input gain in dB: how loud the captured voice is recorded.
    pub fn record_gain_db(&self) -> f64 {
        self.state().record_gain_db
    }

    /// Monitor (hear-yourself) gain, linear 0..1; 0 = off.
    pub fn monitor_gain(&self) -> f64 {
        self.state().monitor_gain
    }

    /// The recording offset in ms: how much earlier new takes land on the
    /// timeline than the measured round trip alone puts them (negative:
    /// later). For the delay no device reports, which a DAW's recording delay
    /// corrects.
    pub fn recording_offset_ms(&self) -> f64 {
        self.state().recording_offset_ms
    }

    /// The engine's fixed processing rate (Hz), shown in the panel.
    pub fn engine_sample_rate(&self) -> f64 {
        self.state()
            .engine
            .as_ref()
            .map_or(48_000.0, |e| e.sample_rate())
    }

    /// True when a take would record from a Bluetooth headset that also
    /// plays: opening its mic forces its call mode (HFP), which degrades what
    /// it plays (a hardware limit; the UI warns to pick another mic). A take
    /// always plays through the duplex mic unit, which renders to the mic's
    /// own device when that has an output side (else to the system default),
    /// whatever output is picked: so the input decides, and the output pick
    /// does not.
    pub fn bluetooth_in_out_conflict(&self) -> bool {
        let st = self.state();
        st.conflict(&st.output_devices, &st.input_devices, None)
    }

    /// [`AppSettingsService::bluetooth_in_out_conflict`] judged on a fresh
    /// enumeration, without adopting it (like
    /// [`AppSettingsService::sync_input_channel`]), and on the device the mic
    /// is open on, if it is: the lists are polled only while Settings is
    /// open, so a song page judged by the lists from when it opened, and a
    /// mic left open on a fallback stays there after the pick returns.
    pub fn bluetooth_in_out_conflict_now(&self) -> bool {
        let st = self.state();
        let inputs = (self.queries.enumerate_inputs)();
        let bound = match &st.engine {
            Some(e) if GraphAudioPlaybackService::input_state_running(e.input_stream_state()) => {
                // No uid while open: bound to the system default (picked, or
                // a fallback), as in `sync_input_channel`.
                let b = e.actual_input_device();
                Some(if b.is_empty() { default_in(&inputs) } else { b })
            }
            _ => None,
        };
        st.conflict(
            &(self.queries.enumerate_outputs)(),
            &inputs,
            bound.as_deref(),
        )
    }

    /// Whether the audible monitor (hear-yourself) is allowed: only when the
    /// device actually RENDERING is headphones (no acoustic path back to the
    /// mic). On a built-in or external SPEAKER the mic re-captures the
    /// monitored voice and howls, so the monitor is locked off.
    ///
    /// Judged from what the engine is playing through, not from the
    /// selection: while the duplex mic unit owns the output, playback follows
    /// the microphone's endpoint or the system default, so "the user picked
    /// AirPods" can be true while the sound is coming out of the built-in
    /// speakers. The engine hard-mutes the monitor in that case anyway (its
    /// `EVENT_OUTPUT_REROUTED`); this keeps the fader honest about it. Falls
    /// back to the selection when the engine reports nothing or names a
    /// device the panel cannot enumerate: never worse than judging the
    /// selection alone.
    pub fn monitor_allowed(&self) -> bool {
        self.state().monitor_allowed()
    }

    /// The device the engine reports rendering to when it is surely
    /// headphones, so the mic hears none of the backing; "" otherwise. The
    /// engine's echo canceller then passes the mic through (only while that
    /// device still renders) instead of adapting to a reference the mic never
    /// hears, which pulled an in-key voice under. Stricter than
    /// [`AppSettingsService::monitor_allowed`], twice over:
    ///  * only the device the engine REPORTS counts: never the selection,
    ///    and never a device the lists do not know;
    ///  * only a PRODUCT named as earbuds counts, not any Bluetooth device and
    ///    not a headphone PORT ("External Headphones" is the jack, whatever is
    ///    plugged in), or an output the user marked as headphones-only
    ///    ([`AppSettingsService::set_output_is_headphones`]). A speaker taken
    ///    for headphones here would leave its whole echo in every take.
    pub fn echo_free_output(&self) -> String {
        self.state().echo_free_output()
    }

    /// The output a headphones mark would apply to: the device the engine
    /// renders to, else the selection. `None` when neither is known.
    pub fn output_to_mark(&self) -> Option<AudioDevice> {
        self.state().rendering_device().cloned()
    }

    /// Whether [`AppSettingsService::output_to_mark`] can be marked as
    /// headphones: its transport allows a mark and its name says no speaker;
    /// and a device its name already tells as earbuds needs no mark.
    pub fn output_markable(&self) -> bool {
        self.state().output_markable()
    }

    /// Whether the user has marked [`AppSettingsService::output_to_mark`] as
    /// headphones.
    pub fn output_marked_headphones(&self) -> bool {
        let st = self.state();
        st.marked_headphones(st.rendering_device())
    }

    /// Whether a locked monitor should point to the headphones mark in
    /// Settings: only when the device playing is the one Settings will offer
    /// it for, the selection. (Settings opens from Home, where nothing
    /// renders; while the mic unit has moved playback to its own device, the
    /// card there would be for another output, and the engine mutes the moved
    /// monitor anyway.)
    pub fn suggest_headphones_mark(&self) -> bool {
        let st = self.state();
        let playing = st.rendering_device();
        let chosen = effective(&st.output_devices, st.output_device_uid.as_deref());
        st.output_markable() && matches!((playing, chosen), (Some(p), Some(c)) if p.uid == c.uid)
    }

    /// True when the last settings write did NOT reach disk (a full or
    /// read-only disk, revoked permissions). The panel keeps showing what the
    /// user picked (the engine really is on it), but a restart reads the
    /// older document back, so the UI has to say so instead of quietly lying.
    pub fn persist_failed(&self) -> bool {
        self.state().persist_failed
    }

    // --- Engine routing -------------------------------------------------------

    /// The input-channel choice of the device capturing NOW, read off a fresh
    /// enumeration of the inputs without adopting it: the list is polled only
    /// while Settings is open, and the device poll owns adopting it (with its
    /// switches back to a returned device).
    fn channel_capturing_now(&self, st: &State) -> u32 {
        st.channel_for(&st.key_in(&(self.queries.enumerate_inputs)()))
    }

    /// Before a take: points the attached engine at the input-channel choice
    /// of the device capturing now (an interface plugged in, or a default that
    /// moved, since the screen opened). Only that: a full
    /// [`AppSettingsService::refresh_devices`] there also switched a returned
    /// device back synchronously, re-opening the mic or blocking on the output
    /// host right as the take armed.
    ///
    /// With the mic already open (monitoring), the device capturing is the
    /// one the stream is bound to: it retries the chosen device only when it
    /// opens, so a mic open on the fallback stays there after the chosen one
    /// returns, and keyed on the returned one, the fallback recorded under
    /// its choice.
    pub fn sync_input_channel(&self) {
        let st = self.state();
        let Some(engine) = &st.engine else {
            return;
        };
        let open = GraphAudioPlaybackService::input_state_running(engine.input_stream_state());
        let key = if !open {
            st.key_in(&(self.queries.enumerate_inputs)())
        } else {
            let bound = engine.actual_input_device();
            if bound.is_empty() {
                default_in(&(self.queries.enumerate_inputs)())
            } else {
                bound
            }
        };
        engine.set_input_channel(st.channel_for(&key));
    }

    /// Re-reads the current input's channel count; true when it changed.
    /// Never notifies.
    fn refresh_input_channel_count(&self, st: &mut State) -> bool {
        let count = (self.queries.input_channel_count)(&st.input_key());
        if count == st.input_channel_count {
            return false;
        }
        st.input_channel_count = count;
        true
    }

    /// Re-evaluates monitor feedback safety against the device the engine is
    /// CURRENTLY rendering to and pushes the resulting gain. Cheap; call it
    /// from any tick that could have seen the output move: opening the mic
    /// hands the output to the duplex unit, which can reroute playback
    /// without any device set changing, so
    /// [`AppSettingsService::refresh_devices`] alone would not notice.
    pub fn reconcile_monitor_safety(&self) {
        let mut st = self.state();
        let changed = self.refresh_actual_output(&mut st);
        st.apply_output_route();
        // The MON fader's enabled state is derived from this, so a change has
        // to reach the panel.
        if changed {
            st.notify = true;
        }
        self.settle(st);
    }

    /// Re-reads the engine's rendering device into `actual_output_uid`; true
    /// when what it is judged as changed: the device moved, or a fresh read
    /// of one the lists do not know came out differently from the last (found
    /// it, or could read the list again). Never notifies: callers own that.
    fn refresh_actual_output(&self, st: &mut State) -> bool {
        let actual = st
            .engine
            .as_ref()
            .map_or_else(String::new, |e| e.actual_output_device());
        let known = actual.is_empty()
            || by_uid(&st.output_devices, Some(&actual)).is_some()
            || st.unlisted_output.as_ref().is_some_and(|u| u.uid == actual);
        let unlisted_before = st.unlisted_output.clone();
        let mut unreadable = false;
        if !known {
            let fresh = (self.queries.enumerate_outputs)();
            st.unlisted_output = by_uid(&fresh, Some(&actual)).cloned();
            unreadable = fresh.is_empty();
        }
        // Dart compared the cached device by identity: a fresh read's device
        // never equals the one it replaces (different uids), so equality says
        // the same.
        let rejudged =
            unlisted_before != st.unlisted_output || unreadable != st.unlisted_unreadable;
        st.unlisted_unreadable = unreadable;
        if actual == st.actual_output_uid {
            return rejudged;
        }
        st.actual_output_uid = actual;
        true
    }

    /// Pushes the current devices, gains and reverb into the active engine.
    /// Used by both the attach and the load, so a song opened BEFORE the load
    /// resolved (which attached the engine with constructor defaults) still
    /// gets the persisted values once they arrive.
    fn apply_all_to_engine(&self, st: &mut State) {
        let Some(engine) = st.engine.clone() else {
            return;
        };
        if let Some(out) = st.output_device_uid.as_deref().filter(|u| !u.is_empty()) {
            engine.set_output_device(out);
        }
        if let Some(inp) = st.input_device_uid.as_deref().filter(|u| !u.is_empty()) {
            engine.set_input_device(inp);
        }
        engine.set_monitor_reverb(
            MONITOR_REVERB_ROOM,
            MONITOR_REVERB_DAMPING,
            st.monitor_reverb_wet,
        );
        engine.set_input_gain(db_to_linear(st.record_gain_db));
        engine.set_input_channel(st.input_channel());
        engine.set_record_offset_ms(st.recording_offset_ms);
        self.refresh_actual_output(st);
        st.apply_output_route();
    }

    // --- Loading, devices -------------------------------------------------------

    /// Loads the persisted settings and enumerates the devices. Safe to call
    /// at startup; blocking (a disk read), so off the UI thread.
    ///
    /// The app starts this and moves on, so it resolves at an arbitrary
    /// point in the session. Two consequences shape the body: adoption is PER
    /// FIELD (a value the user already chose is skipped), and every field is
    /// parsed tolerantly, because one wrong-typed entry in an otherwise valid
    /// document must not cost the apply to the engine, the only call that
    /// pushes persisted devices and gains to an engine attached before the
    /// read resolved. A strict read here left the device panel permanently
    /// empty with nothing on screen to say why.
    pub fn load(&self) {
        self.state().loads_in_flight += 1;
        let read = self.storage.read(COLLECTION, DOC_ID);
        let mut st = self.state();
        let doc = match read {
            Ok(doc) => {
                st.read_failed = false;
                doc
            }
            Err(_) => {
                // Never let a read failure abort startup: run on defaults
                // meanwhile. The document is still there, though, and no write
                // may replace its values with them (see `persist`).
                st.read_failed = true;
                None
            }
        };
        st.loads_in_flight -= 1;
        st.adopt(doc.as_ref());
        // If a screen attached the engine before this load resolved, it
        // adopted constructor defaults: re-push the now-loaded values to it.
        self.apply_all_to_engine(&mut st);
        self.refresh_devices_locked(&mut st);
        // The refresh only notifies when a device SET changed, and by now it
        // usually has not (the attach enumerates first). The loaded gains
        // still have to reach the panel, so notify unconditionally.
        st.notify = true;
        // A choice persisted while the read was parked was held back (see
        // `persist`); write it now that the untouched fields hold the
        // document.
        let deferred = st.persist_deferred && st.loads_in_flight == 0;
        if deferred {
            st.persist_deferred = false;
        }
        self.settle(st);
        if deferred {
            self.persist();
        }
    }

    /// Re-queries the available output and input devices (static; no engine
    /// required). Only notifies when a set actually changed, so it's safe to
    /// poll on a timer for hot-plug detection.
    pub fn refresh_devices(&self) {
        let mut st = self.state();
        self.refresh_devices_locked(&mut st);
        self.settle(st);
    }

    fn refresh_devices_locked(&self, st: &mut State) {
        let outs = (self.queries.enumerate_outputs)();
        let ins = (self.queries.enumerate_inputs)();
        let changed = device_sig(&outs) != device_sig(&st.output_devices)
            || device_sig(&ins) != device_sig(&st.input_devices);
        if !changed {
            // Polled with the lists: a device whose input format changed can
            // change whether the panel offers the input-channel choice without
            // any device set changing.
            if self.refresh_input_channel_count(st) {
                st.notify = true;
            }
            return;
        }
        let capturing_before = st.input_key();
        // A persisted device coming back after an unplug: the engine fell back
        // to the system default meanwhile, so switch it back to the user's
        // choice live.
        let out_returned = returned(&st.output_devices, &outs, st.output_device_uid.as_deref());
        let in_returned = returned(&st.input_devices, &ins, st.input_device_uid.as_deref());
        st.output_devices = outs;
        st.input_devices = ins;
        if let Some(engine) = st.engine.clone() {
            if out_returned {
                if let Some(uid) = &st.output_device_uid {
                    engine.set_output_device(uid);
                }
            }
            if in_returned {
                if let Some(uid) = &st.input_device_uid {
                    engine.set_input_device(uid);
                }
            }
            // The mic moved under the same selection (the system default
            // changed, or the chosen device left or came back): it runs on ITS
            // input-channel choice.
            if st.input_key() != capturing_before {
                engine.set_input_channel(st.input_channel());
            }
        }
        self.refresh_input_channel_count(st);
        st.sync_last_known_names();
        // monitor_allowed is derived from the output set, so reconcile the
        // engine's monitor gain whenever it changes: the initial attach can
        // run before the device list is populated and force the gain to 0;
        // without this re-apply the MON fader reads up but the engine stays
        // muted.
        self.refresh_actual_output(st);
        st.apply_output_route();
        st.notify = true;
    }

    /// [`AppSettingsService::refresh_devices`] for the outputs alone, for a
    /// screen the Settings panel can't be polling over (a song): a chosen
    /// output unplugged there and plugged back in stayed on the fallback
    /// until the mic next closed or the song was reopened. Inputs are left to
    /// the take's own device handling: switching the mic mid-take would
    /// splice two mics into it.
    ///
    /// It keeps its own list rather than adopting the read: a Bluetooth
    /// headset leaves the list for a moment while the mic switches it to its
    /// headset profile, and adopting that dropped the echo-free choice and
    /// muted the monitor as a take opened.
    ///
    /// It switches even when the output came back as the system default and
    /// the stream already plays there: that stream follows the default, so
    /// the next default change (AirPods connecting, a display with speakers)
    /// took the backing with it, monitor muted, until the mic next closed. A
    /// brief gap as the headphones reconnect is the lesser cost.
    ///
    /// Returns whether the engine switched, so the caller's own re-open of
    /// the output does not rebuild the stream a second time in the same tick;
    /// a switch the engine refused leaves that re-open to run.
    pub fn refresh_output_devices(&self) -> bool {
        let mut st = self.state();
        let outs = (self.queries.enumerate_outputs)();
        let switch = match (&st.engine, st.output_device_uid.as_deref()) {
            (Some(engine), Some(uid)) if returned(&st.watched_outputs, &outs, Some(uid)) => {
                Some((Arc::clone(engine), uid.to_string()))
            }
            _ => None,
        };
        st.watched_outputs = outs;
        // The switch waits for the engine's audio host, which may be busy for
        // seconds with a mic open (a Bluetooth headset): not with the
        // settings lock held, or every settings read (the monitor-safety
        // reconcile on the same watch tick, the meter's gain) waits too.
        drop(st);
        switch.is_some_and(|(engine, uid)| engine.set_output_device(&uid))
    }

    /// Attaches the active engine. Applies the persisted devices immediately
    /// so a freshly created engine adopts the saved choices, then refreshes
    /// the device lists from the live host.
    pub fn attach_engine(&self, engine: Arc<GraphAudioPlaybackService>) {
        let mut st = self.state();
        st.engine = Some(engine);
        st.echo_free_pushed = None;
        // A list read for an earlier engine (an earlier song, the chosen
        // output unplugged there) would show the output as returning on this
        // engine's first watch tick, though the attach below already binds it
        // there: the stream was rebuilt, audibly, a second after the song
        // opened.
        st.watched_outputs = Vec::new();
        self.apply_all_to_engine(&mut st);
        // Dart deferred this refresh to a microtask only so its notify would
        // not fire inside the widget build that attached the engine; here the
        // listeners run after the call returns, so it ends the attach.
        self.refresh_devices_locked(&mut st);
        self.settle(st);
    }

    /// Detaches the active engine (e.g. its screen closed); a no-op for any
    /// other engine.
    pub fn detach_engine(&self, engine: &Arc<GraphAudioPlaybackService>) {
        let mut st = self.state();
        if !st.engine.as_ref().is_some_and(|e| Arc::ptr_eq(e, engine)) {
            return;
        }
        st.engine = None;
        // What that engine rendered to is not what anything renders to now.
        // Kept, it had Settings (opened from Home, after the song page let go)
        // offer or hide the headphones mark for the last song's output instead
        // of the user's choice. No notify: this runs from a disposing page,
        // and whatever reads these next builds afresh.
        st.actual_output_uid.clear();
        st.unlisted_output = None;
        st.unlisted_unreadable = false;
    }

    /// Points `engine`, one this service does not track (the technique page
    /// owns its own), at the user's input choice: the device, which of its
    /// inputs to record, and the record gain. A new engine starts on Auto and
    /// the default gain, so without them a singer who picked Input 1 to keep
    /// a guitar on input 2 out got it blended into those takes, at a level
    /// they had not set.
    ///
    /// The channel choice is the device capturing NOW's: read off the polled
    /// list, an interface plugged in since launch read as missing, and its
    /// takes ran on the built-in mic's choice.
    pub fn apply_input_to(&self, engine: &GraphAudioPlaybackService) {
        let st = self.state();
        if let Some(uid) = st.input_device_uid.as_deref().filter(|u| !u.is_empty()) {
            engine.set_input_device(uid);
        }
        engine.set_input_channel(self.channel_capturing_now(&st));
        engine.set_input_gain(db_to_linear(st.record_gain_db));
        engine.set_record_offset_ms(st.recording_offset_ms);
    }

    // --- Choices --------------------------------------------------------------

    /// Selects an output device by uid (or `None` = system default). Applies
    /// it live to the active engine and persists it for the next one.
    /// Blocking (the persist).
    pub fn set_output_device(&self, uid: Option<&str>) {
        let mut st = self.state();
        // Marked with the field write, under the lock a late load must take:
        // the race it would otherwise win is closed on the user's side.
        st.user_chosen.insert(K_OUTPUT_UID);
        st.output_device_uid = uid.map(str::to_string);
        st.output_device_name = by_uid(&st.output_devices, uid).map(|d| d.name.clone());
        // Drive the engine toward the resolved device even for a System
        // Default selection: "" makes the native side fall back to the default
        // output. Without this the engine would keep the previously selected
        // device while monitor_allowed is recomputed from the OS default,
        // which could un-mute the hear-yourself monitor into a speaker
        // (feedback). The switch blocks for the host reply, so the engine has
        // finished ACTING on the request before monitor safety is re-evaluated
        // below. That is not the same as being on the requested device: while
        // the mic is open the duplex unit owns the output and the host only
        // RECORDS the pick for later, and a device that fails to open falls
        // back to the default. So the re-evaluation below reads what the
        // engine says it is rendering to, not this uid.
        if let Some(e) = &st.engine {
            e.set_output_device(uid.unwrap_or(""));
        }
        // Re-evaluate monitor safety for the new output (a speaker forces the
        // monitor off).
        self.refresh_actual_output(&mut st);
        st.apply_output_route();
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Selects an input device by uid (or `None` = system default). Blocking
    /// (the persist).
    pub fn set_input_device(&self, uid: Option<&str>) {
        let mut st = self.state();
        st.user_chosen.insert(K_INPUT_UID);
        st.input_device_uid = uid.map(str::to_string);
        st.input_device_name = by_uid(&st.input_devices, uid).map(|d| d.name.clone());
        if let Some(e) = st.engine.clone() {
            // Also on a System Default selection: "" makes the native side
            // fall back to the default input, so the pick actually switches
            // the mic instead of leaving the engine on the previously selected
            // device until restart.
            e.set_input_device(uid.unwrap_or(""));
            // The choice is per device: the new input runs on ITS choice
            // (Auto unless made).
            e.set_input_channel(st.input_channel());
        }
        self.refresh_input_channel_count(&mut st);
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Chooses which input(s) of the current input device the engine records
    /// and monitors (an `INPUT_CHANNEL_*` mode). Applies live and persists for
    /// this device. Blocking (the persist).
    pub fn set_input_channel(&self, mode: u32) {
        let mut st = self.state();
        let key = st.input_key();
        st.input_channels_chosen.insert(key.clone());
        st.set_channel(
            &key,
            mode.clamp(
                GraphAudioPlaybackService::INPUT_CHANNEL_AUTO,
                GraphAudioPlaybackService::INPUT_CHANNEL_2,
            ),
        );
        if let Some(e) = &st.engine {
            e.set_input_channel(st.input_channel());
        }
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Sets the monitor reverb amount (0..1) and applies it live. Blocking
    /// (the persist).
    pub fn set_monitor_reverb(&self, wet: f64) {
        let mut st = self.state();
        st.user_chosen.insert(K_REVERB_WET);
        st.monitor_reverb_wet = clamp_f64(wet, 0.0, 1.0);
        if let Some(e) = &st.engine {
            e.set_monitor_reverb(
                MONITOR_REVERB_ROOM,
                MONITOR_REVERB_DAMPING,
                st.monitor_reverb_wet,
            );
        }
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Sets the recording offset in ms (see
    /// [`AppSettingsService::recording_offset_ms`]), clamped to its range, and
    /// applies it to the next take. Blocking (the persist).
    pub fn set_recording_offset_ms(&self, ms: f64) {
        if !ms.is_finite() {
            return;
        }
        let mut st = self.state();
        st.user_chosen.insert(K_RECORDING_OFFSET_MS);
        st.recording_offset_ms = clamp_f64(
            ms,
            Self::RECORDING_OFFSET_MIN_MS,
            Self::RECORDING_OFFSET_MAX_MS,
        );
        if let Some(e) = &st.engine {
            e.set_record_offset_ms(st.recording_offset_ms);
        }
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Marks the output `uid` (the [`AppSettingsService::output_to_mark`] the
    /// panel showed, not whatever renders by the time the tap lands) as one
    /// only headphones play from, or clears the mark. Takes on a marked
    /// output skip echo cancelling, which pulled held in-key notes under on
    /// an echo the mic never heard, and the monitor is allowed on it. A
    /// speaker playing from a marked output would leave its whole echo in
    /// every take, so the mark is the user's alone to make. Blocking (the
    /// persist).
    pub fn set_output_is_headphones(&self, uid: &str, on: bool) {
        if uid.is_empty() {
            return;
        }
        let mut st = self.state();
        st.headphone_marks_chosen.insert(uid.to_string());
        if on {
            st.add_mark(uid);
        } else {
            st.headphone_outputs.retain(|u| u != uid);
        }
        st.apply_output_route();
        st.notify = true;
        self.settle(st);
        self.persist();
    }

    /// Sets the record/input gain in dB and applies it live, without
    /// persisting (call [`AppSettingsService::commit_audio_gains`] on the
    /// fader's release). For continuous fader drags.
    pub fn set_record_gain_db_live(&self, db: f64) {
        let mut st = self.state();
        st.user_chosen.insert(K_RECORD_GAIN_DB);
        st.record_gain_db = clamp_f64(db, Self::RECORD_GAIN_MIN_DB, Self::RECORD_GAIN_MAX_DB);
        if let Some(e) = &st.engine {
            e.set_input_gain(db_to_linear(st.record_gain_db));
        }
        st.notify = true;
        self.settle(st);
    }

    /// Sets the monitor (hear-yourself) gain (linear 0..1) and applies it
    /// live, without persisting. Forced to 0 at the engine on a speaker
    /// output (see [`AppSettingsService::monitor_allowed`]).
    pub fn set_monitor_gain_live(&self, gain: f64) {
        let mut st = self.state();
        st.user_chosen.insert(K_MONITOR_GAIN);
        st.monitor_gain = clamp_f64(gain, 0.0, 1.0);
        st.apply_monitor_gain();
        st.notify = true;
        self.settle(st);
    }

    /// Persists the record and monitor gains (call on a fader's release).
    /// Blocking.
    pub fn commit_audio_gains(&self) {
        self.persist();
    }

    /// Writes the whole settings document.
    ///
    /// NEVER fails. Every caller is a UI callback with nothing to report
    /// to: a failure lives in [`AppSettingsService::persist_failed`], state
    /// the UI can render, instead of an error no call site can hold (in Dart a
    /// rethrown I/O failure became an unhandled async error, a spurious crash
    /// report, while the panel went on showing a value the disk had refused).
    fn persist(&self) {
        if self.persist_in_order() {
            self.notify_listeners();
        }
    }

    /// [`AppSettingsService::persist`] under the ordering lock; whether the
    /// listeners are to be told (once the lock is released).
    fn persist_in_order(&self) -> bool {
        let _order = self
            .persist_order
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut notify = false;
        let mut st = self.state();
        // The document is every field from memory, and while a load is parked
        // in its read the fields the user has not touched still hold
        // constructor defaults: writing then put those defaults on disk, and
        // the load never wrote again (a read racing the write could even adopt
        // them). Hold the write back; the load issues it once the document's
        // values are in place.
        if st.loads_in_flight > 0 {
            st.persist_deferred = true;
            return false;
        }
        // The document could not be read at load, so every field the user has
        // not set holds a default: written as they stand, a transient read
        // error cost the chosen devices, gains, input channels and headphones
        // marks for good. Read it again first; while it still can't be, leave
        // it as it is.
        if st.read_failed {
            drop(st);
            let read = self.storage.read(COLLECTION, DOC_ID);
            st = self.state();
            match read {
                Err(e) => {
                    DiagnosticsLog::instance().log(
                        "settings",
                        &format!(
                            "settings unreadable ({}); the choice is live but is not written over the stored ones",
                            error_type(&e)
                        ),
                    );
                    st.set_persist_failed(true);
                    Self::release(st, &mut notify);
                    return notify;
                }
                Ok(doc) => {
                    if st.read_failed {
                        st.read_failed = false;
                        st.adopt(doc.as_ref());
                        self.apply_all_to_engine(&mut st);
                        st.notify = true;
                    }
                }
            }
        }
        let doc = st.document();
        Self::release(st, &mut notify);
        let written = self.storage.write(COLLECTION, DOC_ID, &doc);
        let mut st = self.state();
        match written {
            Ok(()) => st.set_persist_failed(false),
            Err(e) => {
                DiagnosticsLog::instance().log(
                    "settings",
                    &format!(
                        "settings persist failed ({}); the choice is live but will not survive a restart",
                        error_type(&e)
                    ),
                );
                st.set_persist_failed(true);
            }
        }
        Self::release(st, &mut notify);
        notify
    }
}
