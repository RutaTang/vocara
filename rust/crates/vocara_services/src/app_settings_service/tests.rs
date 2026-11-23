//! Port of `test/app_settings_service_test.dart`, plus the service-level
//! assertions of `settings_screen_test.dart` and
//! `song_practice_take_flow_test.dart` that exercise these settings.
//!
//! The Dart engine fake implemented the service's own methods; here the real
//! [`GraphAudioPlaybackService`] runs over fake bindings that record the same
//! calls (its methods are one engine call each). Where a Dart test parked
//! `load()` on a read gate and raced it from the test's own isolate, the port
//! runs the load on a thread parked on the same gate.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use serde_json::{json, Map, Value};
use vocara_model::models::audio_device::{AudioDevice, AudioTransport};

use super::*;
use crate::engine::{EngineBindings, EngineError};

// --- Fakes ------------------------------------------------------------------------

/// In-memory storage: serves one preloaded document and records writes.
#[derive(Default)]
struct FakeStorage {
    s: Mutex<StorageState>,
    gate: Condvar,
}

#[derive(Default)]
struct StorageState {
    doc: Option<Map<String, Value>>,
    last_write: Option<Map<String, Value>>,
    writes: usize,
    /// When true every write fails, like a full or read-only disk.
    fail_writes: bool,
    /// Reads that fail before one succeeds again, like a transient I/O error.
    fail_reads: usize,
    /// While armed and not open, a read parks: the window in which a user
    /// choice can race the fire-and-forget load.
    gate_armed: bool,
    gate_open: bool,
    parked: usize,
}

impl FakeStorage {
    fn s(&self) -> MutexGuard<'_, StorageState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_doc(&self, doc: Value) {
        self.s().doc = Some(as_map(doc));
    }

    fn last(&self, key: &str) -> Option<Value> {
        self.s()
            .last_write
            .as_ref()
            .and_then(|w| w.get(key).cloned())
    }

    /// Dart's `storage.readGate = Completer()`.
    fn arm_gate(&self) {
        let mut s = self.s();
        s.gate_armed = true;
        s.gate_open = false;
    }

    /// Waits until a read is parked on the gate.
    fn wait_parked(&self) {
        let mut s = self.s();
        while s.parked == 0 {
            s = self
                .gate
                .wait_timeout(s, Duration::from_secs(10))
                .unwrap()
                .0;
        }
    }

    /// Dart's `gate.complete()`: this read and every later one pass.
    fn open_gate(&self) {
        self.s().gate_open = true;
        self.gate.notify_all();
    }
}

impl SettingsStorage for FakeStorage {
    fn read(&self, _collection: &str, _id: &str) -> io::Result<Option<Map<String, Value>>> {
        let mut s = self.s();
        if s.gate_armed && !s.gate_open {
            s.parked += 1;
            self.gate.notify_all();
            while !s.gate_open {
                s = self.gate.wait(s).unwrap();
            }
        }
        if s.fail_reads > 0 {
            s.fail_reads -= 1;
            return Err(io::Error::other("read failed"));
        }
        Ok(s.doc.clone())
    }

    fn write(&self, _collection: &str, _id: &str, json: &Map<String, Value>) -> io::Result<()> {
        let mut s = self.s();
        if s.fail_writes {
            return Err(io::Error::other("disk full"));
        }
        s.last_write = Some(json.clone());
        s.writes += 1;
        Ok(())
    }
}

/// Engine bindings that record the device uids, gains and routes pushed.
struct FakeEngine {
    s: Mutex<EngineState>,
}

struct EngineState {
    output_calls: Vec<String>,
    input_calls: Vec<String>,
    monitor_gains: Vec<f32>,
    /// What the engine reports it is RENDERING to, which is not the
    /// selection while the duplex mic unit owns the output. "" = nothing open
    /// or unknown.
    actual_output: String,
    /// What `set_output_device` answers: whether the engine took the device.
    takes_output: bool,
    /// Every output the settings named echo-free ("" = none), in call order.
    echo_free: Vec<String>,
    /// Every input gain pushed (linear), in order.
    input_gain_calls: Vec<f32>,
    /// The input stream state word and the device it is bound to.
    stream_state: u64,
    bound_input: String,
    /// Every input-channel mode pushed, in order.
    input_channel_calls: Vec<u32>,
}

impl FakeEngine {
    fn s(&self) -> MutexGuard<'_, EngineState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl EngineBindings for FakeEngine {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    fn actual_output_device(&self) -> String {
        self.s().actual_output.clone()
    }

    fn set_output_device(&self, uid: &str) -> Result<(), EngineError> {
        let mut s = self.s();
        s.output_calls.push(uid.to_string());
        if s.takes_output {
            Ok(())
        } else {
            Err(EngineError::Refused {
                call: "ve_set_output_device",
                code: -1,
            })
        }
    }

    fn set_input_device(&self, uid: &str) -> Result<(), EngineError> {
        self.s().input_calls.push(uid.to_string());
        Ok(())
    }

    fn set_monitor_gain(&self, gain: f32) -> Result<(), EngineError> {
        self.s().monitor_gains.push(gain);
        Ok(())
    }

    fn set_echo_free(&self, uid: &str) -> Result<(), EngineError> {
        self.s().echo_free.push(uid.to_string());
        Ok(())
    }

    fn set_input_gain(&self, gain: f32) -> Result<(), EngineError> {
        self.s().input_gain_calls.push(gain);
        Ok(())
    }

    fn set_monitor_reverb(&self, _room: f32, _damping: f32, _wet: f32) -> Result<(), EngineError> {
        Ok(())
    }

    fn input_stream_state(&self) -> u64 {
        self.s().stream_state
    }

    fn actual_input_device(&self) -> String {
        self.s().bound_input.clone()
    }

    fn set_input_channel(&self, mode: u32) -> Result<(), EngineError> {
        self.s().input_channel_calls.push(mode);
        Ok(())
    }
}

/// A fake engine and the service over it (what the settings attach).
struct Engine {
    fake: Arc<FakeEngine>,
    svc: Arc<GraphAudioPlaybackService>,
}

impl Engine {
    fn new() -> Engine {
        Engine::with(|_| {})
    }

    fn with(setup: impl FnOnce(&mut EngineState)) -> Engine {
        let mut state = EngineState {
            output_calls: Vec::new(),
            input_calls: Vec::new(),
            monitor_gains: Vec::new(),
            actual_output: String::new(),
            takes_output: true,
            echo_free: Vec::new(),
            input_gain_calls: Vec::new(),
            stream_state: 0,
            bound_input: String::new(),
            input_channel_calls: Vec::new(),
        };
        setup(&mut state);
        let fake = Arc::new(FakeEngine {
            s: Mutex::new(state),
        });
        let svc = Arc::new(GraphAudioPlaybackService::with_bindings(
            Arc::clone(&fake) as Arc<dyn EngineBindings>,
            None,
        ));
        Engine { fake, svc }
    }

    fn s(&self) -> MutexGuard<'_, EngineState> {
        self.fake.s()
    }

    fn handle(&self) -> Arc<GraphAudioPlaybackService> {
        Arc::clone(&self.svc)
    }
}

// --- Devices ------------------------------------------------------------------------

fn out(uid: &str, name: &str) -> AudioDevice {
    AudioDevice {
        rate: 48_000,
        ..AudioDevice::new(uid, name)
    }
}

fn mic(uid: &str, name: &str) -> AudioDevice {
    AudioDevice {
        rate: 48_000,
        is_input: true,
        ..AudioDevice::new(uid, name)
    }
}

fn dflt(d: AudioDevice) -> AudioDevice {
    AudioDevice {
        is_default: true,
        ..d
    }
}

fn bt(d: AudioDevice) -> AudioDevice {
    AudioDevice {
        is_bluetooth: true,
        ..d
    }
}

fn via(d: AudioDevice, transport: AudioTransport) -> AudioDevice {
    AudioDevice { transport, ..d }
}

fn built_in_out() -> AudioDevice {
    dflt(out("built-in-out", "MacBook Pro Speakers"))
}

fn headphones() -> AudioDevice {
    out("usb-hp", "USB Headphones")
}

fn built_in_mic() -> AudioDevice {
    dflt(mic("built-in-mic", "MacBook Pro Microphone"))
}

fn interface() -> AudioDevice {
    mic("usb-2in", "USB Audio Interface")
}

fn other_interface() -> AudioDevice {
    mic("usb-other", "Another Interface")
}

fn as_map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not a document: {other}"),
    }
}

// --- Fixture ------------------------------------------------------------------------

type Shared<T> = Arc<Mutex<T>>;

/// Mutable live device lists the injected enumerators read from, so tests
/// can simulate unplug and replug between refreshes.
struct Fixture {
    outs: Shared<Vec<AudioDevice>>,
    ins: Shared<Vec<AudioDevice>>,
    /// Input channel counts by device uid ("" = the system default input).
    channels: Shared<HashMap<String, u32>>,
    storage: Arc<FakeStorage>,
    service: Arc<AppSettingsService>,
}

impl Fixture {
    fn new() -> Fixture {
        let outs = Arc::new(Mutex::new(vec![built_in_out()]));
        let ins = Arc::new(Mutex::new(vec![built_in_mic()]));
        let channels = Arc::new(Mutex::new(HashMap::from([
            (String::new(), 1),
            (built_in_mic().uid, 1),
            (interface().uid, 2),
        ])));
        let storage = Arc::new(FakeStorage::default());
        let service = build(&outs, &ins, &channels, &storage);
        Fixture {
            outs,
            ins,
            channels,
            storage,
            service,
        }
    }

    /// Another service over the same lists and storage (a restart).
    fn build(&self) -> Arc<AppSettingsService> {
        build(&self.outs, &self.ins, &self.channels, &self.storage)
    }

    fn outs(&self, devices: Vec<AudioDevice>) {
        *self.outs.lock().unwrap() = devices;
    }

    fn ins(&self, devices: Vec<AudioDevice>) {
        *self.ins.lock().unwrap() = devices;
    }

    fn channel_count(&self, uid: &str, count: u32) {
        self.channels.lock().unwrap().insert(uid.to_string(), count);
    }

    fn doc(&self, doc: Value) {
        self.storage.set_doc(doc);
    }

    fn listen(&self) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        self.service.add_listener(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        count
    }

    /// Starts `service.load()` on a thread and waits until it is parked in
    /// the gated read.
    fn parked_load(&self, service: &Arc<AppSettingsService>) -> thread::JoinHandle<()> {
        self.storage.arm_gate();
        let s = Arc::clone(service);
        let loading = thread::spawn(move || s.load());
        self.storage.wait_parked();
        loading
    }
}

fn build(
    outs: &Shared<Vec<AudioDevice>>,
    ins: &Shared<Vec<AudioDevice>>,
    channels: &Shared<HashMap<String, u32>>,
    storage: &Arc<FakeStorage>,
) -> Arc<AppSettingsService> {
    let (o, i, c) = (Arc::clone(outs), Arc::clone(ins), Arc::clone(channels));
    Arc::new(AppSettingsService::with_queries(
        Arc::clone(storage) as Arc<dyn SettingsStorage>,
        DeviceQueries {
            enumerate_outputs: Box::new(move || o.lock().unwrap().clone()),
            enumerate_inputs: Box::new(move || i.lock().unwrap().clone()),
            input_channel_count: Box::new(move |uid| {
                c.lock().unwrap().get(uid).copied().unwrap_or(0)
            }),
        },
    ))
}

const AUTO: u32 = GraphAudioPlaybackService::INPUT_CHANNEL_AUTO;
const INPUT_1: u32 = GraphAudioPlaybackService::INPUT_CHANNEL_1;
const INPUT_2: u32 = GraphAudioPlaybackService::INPUT_CHANNEL_2;

// --- unplugged persisted device -------------------------------------------------------

#[test]
fn an_unplugged_device_is_reported_missing_but_the_uid_and_last_known_name_are_kept() {
    let f = Fixture::new();
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "outputDeviceName": headphones().name,
    }));
    f.service.load();

    assert!(f.service.output_device_missing());
    // The choice is preserved.
    assert_eq!(f.service.output_device_uid(), Some(headphones().uid));
    assert_eq!(f.service.output_device_name(), Some(headphones().name));
    // No explicit input pick.
    assert!(!f.service.input_device_missing());
}

#[test]
fn monitor_allowed_evaluates_the_effective_device_and_a_missing_pick_is_off() {
    let f = Fixture::new();
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.service.load();
    // The persisted output is unplugged, so there is no known effective
    // device and the hear-yourself monitor stays at the safe default (off).
    assert!(!f.service.monitor_allowed());

    f.outs(vec![built_in_out(), headphones()]);
    f.service.refresh_devices();
    // Back on the (headphone-named) pick, the monitor is allowed again.
    assert!(f.service.monitor_allowed());
}

#[test]
fn monitor_allowed_follows_what_the_engine_renders_to_not_the_pick() {
    // The scenario the selection-based test cannot see: headphones are
    // selected and present, but opening the mic hands the output to the
    // duplex unit, which renders through the microphone's endpoint or the
    // system default (here, the built-in speakers). Judging the selection
    // would leave the monitor up and howling into them.
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    assert!(f.service.monitor_allowed(), "on the chosen device");

    engine.s().actual_output = built_in_out().uid;
    engine.s().monitor_gains.clear();
    f.service.reconcile_monitor_safety();
    assert!(!f.service.monitor_allowed());
    assert_eq!(engine.s().monitor_gains, [0.0]);

    // Back on the chosen device, the fader is honoured again.
    engine.s().actual_output = headphones().uid;
    f.service.reconcile_monitor_safety();
    assert!(f.service.monitor_allowed());
}

#[test]
fn the_echo_canceller_is_told_only_the_device_the_engine_renders_to() {
    // On earbuds the mic hears none of the backing the canceller is handed:
    // it adapted into the in-key voice. Judged on the device actually
    // rendering, never the selection.
    let airpods = bt(out("airpods", "AirPods Pro"));
    let f = Fixture::new();
    f.outs(vec![built_in_out(), airpods.clone()]);
    f.doc(json!({"outputDeviceUid": airpods.uid}));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    assert_eq!(
        engine.s().echo_free.last().unwrap(),
        "",
        "the engine reports nothing yet: the selection is not evidence"
    );

    engine.s().actual_output = airpods.uid.clone();
    f.service.reconcile_monitor_safety();
    assert_eq!(engine.s().echo_free.last().unwrap(), &airpods.uid);

    engine.s().actual_output = built_in_out().uid;
    f.service.reconcile_monitor_safety();
    assert_eq!(
        engine.s().echo_free.last().unwrap(),
        "",
        "the speakers carry an echo"
    );
}

#[test]
fn the_outputs_only_refresh_switches_back_to_a_chosen_output_that_returns_and_leaves_the_mic_alone()
{
    // A song screen can't have the Settings panel polling over it: a chosen
    // output unplugged there and plugged back in stayed on the fallback.
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.ins(vec![built_in_mic(), interface()]);
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "inputDeviceUid": interface().uid,
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().output_calls.clear();
    engine.s().input_calls.clear();

    // Unplugged mid-song.
    f.outs(vec![built_in_out()]);
    f.ins(vec![built_in_mic()]);
    f.service.refresh_output_devices();
    assert!(engine.s().output_calls.is_empty());
    // The panel's list is left alone: a headset blipping out of the read as
    // the mic switches its profile dropped the echo-free choice and muted the
    // monitor.
    assert!(f
        .service
        .output_devices()
        .iter()
        .any(|d| d.uid == headphones().uid));

    // Plugged back in.
    f.outs(vec![built_in_out(), headphones()]);
    f.ins(vec![built_in_mic(), interface()]);
    f.service.refresh_output_devices();
    assert_eq!(engine.s().output_calls, [headphones().uid]);
    assert!(
        engine.s().input_calls.is_empty(),
        "a mid-take mic switch splices"
    );

    // Nothing changed since.
    f.service.refresh_output_devices();
    assert_eq!(engine.s().output_calls, [headphones().uid]);

    // Back as the system default, which the fallback stream already plays
    // on: still bound to it, or the next default change takes the backing
    // along.
    f.outs(vec![built_in_out()]);
    f.service.refresh_output_devices();
    f.outs(vec![built_in_out(), headphones()]);
    engine.s().actual_output = headphones().uid;
    f.service.refresh_output_devices();
    assert_eq!(
        engine.s().output_calls,
        [headphones().uid, headphones().uid]
    );
}

#[test]
fn a_returning_output_the_engine_refuses_is_not_reported_as_switched() {
    // The song's device watch skips its own re-open of a lost output when
    // this says it switched; a refused switch must leave that to run.
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.service.load();
    let engine = Engine::with(|s| s.takes_output = false);
    f.service.attach_engine(engine.handle());
    f.outs(vec![built_in_out()]);
    f.service.refresh_output_devices();
    f.outs(vec![built_in_out(), headphones()]);
    assert!(!f.service.refresh_output_devices());
    assert_eq!(
        engine.s().output_calls.last().unwrap(),
        &headphones().uid,
        "it was asked"
    );
}

#[test]
fn a_song_opened_after_the_chosen_output_came_back_does_not_switch_to_it_again_on_its_first_tick() {
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.service.load();
    let first = Engine::new();
    f.service.attach_engine(first.handle());
    // Unplugged in the first song.
    f.outs(vec![built_in_out()]);
    assert!(!f.service.refresh_output_devices());
    f.service.detach_engine(&first.svc);

    // Back before the next song opens.
    f.outs(vec![built_in_out(), headphones()]);
    let next = Engine::new();
    f.service.attach_engine(next.handle());
    assert_eq!(
        next.s().output_calls,
        [headphones().uid],
        "the attach binds it"
    );
    assert!(!f.service.refresh_output_devices());
    assert_eq!(
        next.s().output_calls,
        [headphones().uid],
        "no second rebuild"
    );
}

#[test]
fn only_a_product_named_as_earbuds_is_echo_free() {
    // A speaker taken for headphones would leave its whole echo in every
    // take: not a device merely on Bluetooth, and not a headphone PORT
    // ("External Headphones" is the jack of an Apple-silicon Mac whatever is
    // plugged into it, desk speakers included).
    let f = Fixture::new();
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    let echo_free_on = |name: &str, bluetooth: bool| {
        let device = AudioDevice {
            is_bluetooth: bluetooth,
            ..out(&format!("out-{name}"), name)
        };
        f.outs(vec![built_in_out(), device.clone()]);
        f.service.refresh_devices();
        engine.s().actual_output = device.uid.clone();
        f.service.set_output_device(Some(&device.uid));
        let last = engine.s().echo_free.last().unwrap().clone();
        last
    };

    for name in [
        "AirPods Pro",
        "AirPods Max",
        "Galaxy Buds2",
        "Beats Studio Buds",
    ] {
        assert_eq!(echo_free_on(name, true), format!("out-{name}"), "{name}");
    }
    assert_eq!(
        echo_free_on("EarPods (USB-C)", false),
        "out-EarPods (USB-C)"
    );
    for name in ["WH-1000XM5", "UE BOOM 3", "Speaker Buds Box"] {
        assert_eq!(echo_free_on(name, true), "", "{name}");
    }
    for port in [
        "External Headphones",
        "USB-C to 3.5mm Headphone Jack Adapter",
        "MacBook Pro Speakers",
    ] {
        assert_eq!(echo_free_on(port, false), "", "{port}");
    }
}

#[test]
fn a_speaker_the_default_moves_to_is_judged_as_itself_not_as_the_selection() {
    // The page enumerated with earbuds as the default; a Bluetooth speaker
    // turned on since became the default and the engine follows it. Judged
    // against the stale list, it fell back to the selection (the earbuds) and
    // the monitor and the canceller both took it for headphones.
    let airpods = dflt(bt(out("airpods", "AirPods Pro")));
    let speakers = out("built-in-out", "MacBook Pro Speakers");
    let f = Fixture::new();
    f.outs(vec![speakers.clone(), airpods.clone()]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "airpods".into());
    f.service.attach_engine(engine.handle());
    f.service.set_monitor_gain_live(0.5);
    assert!(f.service.monitor_allowed());
    assert_eq!(engine.s().echo_free.last().unwrap(), &airpods.uid);

    let boom = dflt(bt(out("boom", "UE BOOM 3")));
    f.outs(vec![
        speakers,
        bt(out("airpods", "AirPods Pro")),
        boom.clone(),
    ]);
    engine.s().actual_output = boom.uid;
    engine.s().monitor_gains.clear();
    f.service.reconcile_monitor_safety();
    assert!(!f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.0);
    assert_eq!(engine.s().echo_free.last().unwrap(), "");
}

#[test]
fn the_1_s_monitor_tick_never_switches_the_mic() {
    // The chosen USB mic was unplugged when the page opened, so the engine
    // records from the built-in one; it is plugged back in, unseen (Settings
    // is closed). Mid-take the default output moves to earbuds the lists do
    // not know either. Judging them by re-reading the lists ran the device
    // refresh's switches on the tick: the mic was reopened on the USB device
    // in the middle of the take.
    let usb_mic = mic("usb-mic", "USB Microphone");
    let f = Fixture::new();
    f.doc(json!({"inputDeviceUid": usb_mic.uid}));
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = built_in_out().uid);
    f.service.attach_engine(engine.handle());
    f.ins(vec![built_in_mic(), usb_mic]);
    engine.s().input_calls.clear();
    engine.s().input_channel_calls.clear();
    f.outs(vec![
        out("built-in-out", "MacBook Pro Speakers"),
        dflt(bt(out("airpods", "AirPods Pro"))),
    ]);
    engine.s().actual_output = "airpods".into();
    f.service.reconcile_monitor_safety();
    assert!(engine.s().input_calls.is_empty());
    assert!(engine.s().input_channel_calls.is_empty());
    // The earbuds are still judged as themselves.
    assert_eq!(engine.s().echo_free.last().unwrap(), "airpods");
    assert!(f.service.monitor_allowed());
}

#[test]
fn the_route_is_named_to_the_engine_on_a_change_not_every_tick() {
    let f = Fixture::new();
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = built_in_out().uid);
    f.service.attach_engine(engine.handle());
    let named = engine.s().echo_free.len();
    for _ in 0..5 {
        f.service.reconcile_monitor_safety();
    }
    assert_eq!(engine.s().echo_free.len(), named);
    // A new engine is always told.
    let next = Engine::with(|s| s.actual_output = built_in_out().uid);
    f.service.attach_engine(next.handle());
    assert_eq!(next.s().echo_free, [""]);
}

#[test]
fn a_tick_while_the_mic_opens_keeps_the_earbuds_named() {
    // Opening the mic hands the output to the duplex unit, and the engine
    // reports nothing rendering until it binds. Taken as "not headphones",
    // that withdrew the earbuds: once the unit bound, the canceller adapted
    // on them until the next tick named them again.
    let airpods = dflt(bt(out("airpods", "AirPods Pro")));
    let f = Fixture::new();
    f.outs(vec![
        out("built-in-out", "MacBook Pro Speakers"),
        airpods.clone(),
    ]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "airpods".into());
    f.service.attach_engine(engine.handle());
    assert_eq!(engine.s().echo_free, std::slice::from_ref(&airpods.uid));
    engine.s().actual_output = String::new();
    f.service.reconcile_monitor_safety();
    engine.s().actual_output = airpods.uid.clone();
    f.service.reconcile_monitor_safety();
    assert_eq!(engine.s().echo_free, [airpods.uid]);
    // A new engine reporting nothing yet is still told something.
    let next = Engine::new();
    f.service.attach_engine(next.handle());
    assert_eq!(next.s().echo_free, [""]);
}

#[test]
fn the_mon_fader_hears_of_a_rendering_device_resolved_on_a_later_tick() {
    // The default moved to a speaker turned on since the lists were read,
    // and the first fresh read did not list it yet: the selection (the
    // earbuds) was judged meanwhile. When the next read found the speaker,
    // the engine's gain went to 0 but the fader was never told.
    let airpods = dflt(bt(out("airpods", "AirPods Pro")));
    let f = Fixture::new();
    f.outs(vec![airpods.clone()]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "airpods".into());
    f.service.attach_engine(engine.handle());
    f.service.set_monitor_gain_live(0.5);
    engine.s().actual_output = "boom".into();
    f.service.reconcile_monitor_safety();
    assert!(f.service.monitor_allowed());
    f.outs(vec![airpods, bt(out("boom", "UE BOOM 3"))]);
    let notified = f.listen();
    f.service.reconcile_monitor_safety();
    assert!(!f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.0);
    assert_eq!(notified.load(Ordering::SeqCst), 1);
    // Nothing new on the next tick: no notify.
    f.service.reconcile_monitor_safety();
    assert_eq!(notified.load(Ordering::SeqCst), 1);
}

#[test]
fn a_device_list_that_cannot_be_read_judges_the_monitor_unsafe() {
    // System Default on the earbuds; the default moves to a speaker the lists
    // never saw, and the fresh read fails (an empty list). The engine's mute
    // covers a pick it is not rendering, not a default that moves, so judging
    // the selection here fed the monitor into the speaker for as long as the
    // reads kept failing.
    let airpods = dflt(bt(out("airpods", "AirPods Pro")));
    let f = Fixture::new();
    f.outs(vec![airpods.clone()]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "airpods".into());
    f.service.attach_engine(engine.handle());
    f.service.set_monitor_gain_live(0.5);
    let notified = f.listen();
    f.outs(vec![]);
    engine.s().actual_output = "boom".into();
    f.service.reconcile_monitor_safety();
    assert!(!f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.0);
    assert_eq!(notified.load(Ordering::SeqCst), 1);
    // The read recovers without the device (one the panel can never list):
    // back to judging the selection, as for any unenumerable device, and the
    // fader is told.
    f.outs(vec![airpods]);
    f.service.reconcile_monitor_safety();
    assert!(f.service.monitor_allowed());
    assert_eq!(notified.load(Ordering::SeqCst), 2);
}

#[test]
fn a_bluetooth_speaker_never_allows_the_monitor() {
    // A Bluetooth speaker taken for headphones fed the monitor straight back
    // into the mic. Bluetooth alone allows nothing: only a name that says
    // headphones does, or the user's mark.
    let f = Fixture::new();
    f.service.load();
    let allowed_on = |name: &str, bluetooth: bool| {
        let device = AudioDevice {
            is_bluetooth: bluetooth,
            transport: if bluetooth {
                AudioTransport::Bluetooth
            } else {
                AudioTransport::BuiltIn
            },
            ..out(&format!("bt-{name}"), name)
        };
        f.outs(vec![built_in_out(), device.clone()]);
        f.service.refresh_devices();
        f.service.set_output_device(Some(&device.uid));
        f.service.monitor_allowed()
    };

    for speaker in [
        "UE BOOM 3",
        "JBL Flip 6",
        "Living Room Speaker",
        "Sonos Roam",
        "Bose SoundLink Flex",
    ] {
        assert!(!allowed_on(speaker, true), "{speaker}");
    }
    for headphones in ["AirPods Pro", "Galaxy Buds2", "Sony Headphones"] {
        assert!(allowed_on(headphones, true), "{headphones}");
    }
    // Headphones whose name does not say so: the user's to mark.
    for unnamed in ["WH-1000XM5", "Bose QC45", "Marshall Emberton"] {
        assert!(!allowed_on(unnamed, true), "{unnamed}");
    }
    assert!(!allowed_on("WH-1000XM5", true));
    f.service.set_output_is_headphones("bt-WH-1000XM5", true);
    assert!(f.service.monitor_allowed());
    assert!(
        !allowed_on("MacBook Pro Speakers", false),
        "built-in speakers stay off"
    );
}

#[test]
fn an_unenumerable_rendering_device_falls_back_to_judging_the_selection() {
    // Aggregate or virtual devices and uid-format drift can name something
    // the panel never enumerates. Refusing to judge at all would mute a
    // legitimate headphone monitor with nothing on screen to explain it, so
    // this degrades to the pre-existing behaviour.
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "aggregate-42".into());
    f.service.attach_engine(engine.handle());
    f.service.reconcile_monitor_safety();
    assert!(f.service.monitor_allowed());
}

#[test]
fn selected_input_device_falls_back_to_the_last_known_name() {
    let f = Fixture::new();
    f.doc(json!({
        "inputDeviceUid": "usb-mic",
        "inputDeviceName": "USB Microphone",
    }));
    f.service.load();
    let selected = f.service.selected_input_device().unwrap();
    assert_eq!(selected.uid, "usb-mic");
    assert_eq!(selected.name, "USB Microphone");
}

// --- device returning after an unplug -------------------------------------------------

#[test]
fn a_returning_device_switches_the_engine_back_live_and_keeps_the_persisted_uid() {
    let f = Fixture::new();
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "outputDeviceName": headphones().name,
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    // Drop the attach-time apply.
    engine.s().output_calls.clear();

    f.outs(vec![built_in_out(), headphones()]);
    f.service.refresh_devices();

    // Switched back live, and still persisted.
    assert_eq!(engine.s().output_calls, [headphones().uid]);
    assert_eq!(f.service.output_device_uid(), Some(headphones().uid));
    assert!(!f.service.output_device_missing());
}

#[test]
fn an_unrelated_hot_plug_does_not_re_push_a_connected_device() {
    let f = Fixture::new();
    f.doc(json!({"inputDeviceUid": built_in_mic().uid}));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().input_calls.clear();
    engine.s().output_calls.clear();

    // A new output appears.
    f.outs(vec![built_in_out(), headphones()]);
    f.service.refresh_devices();

    // The mic never left: no re-apply; no output pick to restore.
    assert!(engine.s().input_calls.is_empty());
    assert!(engine.s().output_calls.is_empty());
}

#[test]
fn unplugging_does_not_push_anything_the_engine_falls_back_natively() {
    let f = Fixture::new();
    f.doc(json!({"outputDeviceUid": headphones().uid}));
    f.outs(vec![built_in_out(), headphones()]);
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().output_calls.clear();

    f.outs(vec![built_in_out()]);
    f.service.refresh_devices();

    assert!(engine.s().output_calls.is_empty());
    assert!(f.service.output_device_missing());
}

// --- last-known device names ------------------------------------------------------------

#[test]
fn last_known_names_are_captured_on_selection_and_persisted_alongside_the_uids() {
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.service.load();

    f.service.set_output_device(Some(&headphones().uid));
    assert_eq!(f.service.output_device_name(), Some(headphones().name));
    assert_eq!(
        f.storage.last("outputDeviceUid"),
        Some(json!(headphones().uid))
    );
    assert_eq!(
        f.storage.last("outputDeviceName"),
        Some(json!(headphones().name))
    );

    // Back to System Default: the name is cleared too.
    f.service.set_output_device(None);
    assert_eq!(f.service.output_device_name(), None);
    assert_eq!(f.storage.last("outputDeviceName"), Some(Value::Null));
}

#[test]
fn last_known_names_follow_a_rename_of_a_connected_persisted_device() {
    let f = Fixture::new();
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "outputDeviceName": "Old Name",
    }));
    f.outs(vec![built_in_out(), headphones()]);
    f.service.load();

    assert_eq!(f.service.output_device_name(), Some(headphones().name));
    // The refreshed name was persisted.
    assert_eq!(
        f.storage.last("outputDeviceName"),
        Some(json!(headphones().name))
    );
}

#[test]
fn a_failing_name_persist_is_absorbed() {
    let f = Fixture::new();
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "outputDeviceName": "Old Name",
    }));
    f.storage.s().fail_writes = true;
    f.outs(vec![built_in_out(), headphones()]);
    f.service.load();

    // The in-memory rename still took effect; only persistence degraded (and
    // says so).
    assert_eq!(f.service.output_device_name(), Some(headphones().name));
    assert!(f.service.persist_failed());
}

// --- a write the disk refuses ------------------------------------------------------------

#[test]
fn a_refused_write_is_reported_in_state_not_lost() {
    // Every setter is wired into the panel as a callback with nothing to
    // return to, so the failure has to live in observable state: otherwise
    // the panel shows a value the disk never took.
    let f = Fixture::new();
    f.storage.s().fail_writes = true;
    let notified = f.listen();

    f.service.set_output_device(Some(&headphones().uid));

    assert!(f.service.persist_failed());
    // The panel gets to render the banner.
    assert!(notified.load(Ordering::SeqCst) > 0);
    // The choice is live regardless: only its durability degraded.
    assert_eq!(f.service.output_device_uid(), Some(headphones().uid));
}

#[test]
fn the_failure_flag_clears_once_a_write_lands_again() {
    let f = Fixture::new();
    f.storage.s().fail_writes = true;
    f.service.set_input_device(Some(&built_in_mic().uid));
    assert!(f.service.persist_failed());

    f.storage.s().fail_writes = false;
    f.service.commit_audio_gains();
    assert!(!f.service.persist_failed());
    assert_eq!(
        f.storage.last("inputDeviceUid"),
        Some(json!(built_in_mic().uid))
    );
}

// --- a document that could not be read --------------------------------------------------

// The load runs on defaults when the read fails, and the next change wrote the
// whole document from memory: one transient read error cost the chosen
// devices, gains, input channels and headphones marks for good.
fn stored() -> Value {
    json!({
        "outputDeviceUid": "headphones",
        "outputDeviceName": "Headphones",
        "recordGainDb": 3.0,
        "monitorGain": 0.5,
        "headphoneOutputUids": ["headphones"],
    })
}

#[test]
fn an_unread_document_is_read_again_before_a_change_is_written_over_it() {
    let f = Fixture::new();
    f.doc(stored());
    f.storage.s().fail_reads = 1;
    f.service.load();
    assert_eq!(f.service.record_gain_db(), 12.0, "running on defaults");

    f.service.set_monitor_reverb(0.7);
    assert_eq!(f.storage.last("monitorReverbWet"), Some(json!(0.7)));
    assert_eq!(f.storage.last("outputDeviceUid"), Some(json!("headphones")));
    assert_eq!(f.storage.last("recordGainDb"), Some(json!(3.0)));
    assert_eq!(f.storage.last("monitorGain"), Some(json!(0.5)));
    assert_eq!(
        f.storage.last("headphoneOutputUids"),
        Some(json!(["headphones"]))
    );
    assert_eq!(f.service.record_gain_db(), 3.0, "adopted once read");
}

#[test]
fn an_unread_document_is_left_as_it_is_while_it_still_cannot_be_read() {
    let f = Fixture::new();
    f.doc(stored());
    f.storage.s().fail_reads = 2;
    f.service.load();

    f.service.set_monitor_reverb(0.7);
    assert_eq!(f.storage.s().writes, 0);
    assert!(f.service.persist_failed(), "the panel says so");
    assert_eq!(f.service.monitor_reverb_wet(), 0.7, "the choice is live");
}

// --- load racing a live user choice ------------------------------------------------------

#[test]
fn a_choice_made_while_load_is_in_flight_survives_it() {
    // The load is started and left to finish whenever, so it can resolve
    // after the singer has already picked a device or moved a fader. Adopting
    // the document wholesale reverted the choice on screen, re-pushed the
    // stale value to the engine, and then persisted it back.
    let f = Fixture::new();
    f.doc(json!({
        "inputDeviceUid": "stale-mic",
        "inputDeviceName": "Stale Mic",
        "monitorGain": 0.9,
        "recordGainDb": 3.0,
    }));
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());

    // Parked inside read().
    let loading = f.parked_load(&f.service);
    f.service.set_input_device(Some(&built_in_mic().uid));
    f.service.set_monitor_gain_live(0.25);
    f.storage.open_gate();
    loading.join().unwrap();

    assert_eq!(f.service.input_device_uid(), Some(built_in_mic().uid));
    assert_eq!(f.service.input_device_name(), Some(built_in_mic().name));
    assert_eq!(f.service.monitor_gain(), 0.25);
    // Untouched fields still adopt the document.
    assert_eq!(f.service.record_gain_db(), 3.0);
    // And the user's value is what gets persisted next, not the document's.
    f.service.commit_audio_gains();
    assert_eq!(f.storage.last("monitorGain"), Some(json!(0.25)));
    assert_eq!(
        f.storage.last("inputDeviceUid"),
        Some(json!(built_in_mic().uid))
    );
}

#[test]
fn a_choice_persisted_while_load_is_in_flight_does_not_clobber_the_untouched_fields_on_disk() {
    // A persist serializes every field from memory. Run while the load was
    // parked in read(), it wrote the constructor defaults for every field the
    // user had not touched, and the load never wrote again: the next launch
    // lost the saved output device and gains.
    let f = Fixture::new();
    f.outs(vec![built_in_out(), headphones()]);
    f.doc(json!({
        "outputDeviceUid": headphones().uid,
        "outputDeviceName": headphones().name,
        "recordGainDb": 3.0,
        "monitorGain": 0.5,
    }));

    let loading = f.parked_load(&f.service);
    f.service.set_input_device(Some(&built_in_mic().uid));
    // Nothing may race the read: on a real disk it could read back the very
    // defaults the racing write put there.
    assert_eq!(f.storage.s().writes, 0);
    f.storage.open_gate();
    loading.join().unwrap();

    assert_eq!(
        f.storage.last("inputDeviceUid"),
        Some(json!(built_in_mic().uid))
    );
    assert_eq!(
        f.storage.last("outputDeviceUid"),
        Some(json!(headphones().uid))
    );
    assert_eq!(
        f.storage.last("outputDeviceName"),
        Some(json!(headphones().name))
    );
    assert_eq!(f.storage.last("recordGainDb"), Some(json!(3.0)));
    assert_eq!(f.storage.last("monitorGain"), Some(json!(0.5)));
}

#[test]
fn one_wrong_typed_field_costs_only_that_field_not_the_engine_apply() {
    // The reads used to be strict, so a single bad entry threw past the
    // fire-and-forget load: the device list stayed empty and monitoring was
    // permanently unavailable with nothing on screen to say why.
    let f = Fixture::new();
    f.doc(json!({
        "outputDeviceUid": 42,
        "inputDeviceUid": built_in_mic().uid,
        "monitorGain": "loud",
        "recordGainDb": 7.0,
    }));
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().input_calls.clear();

    f.service.load();

    assert_eq!(f.service.output_device_uid(), None, "bad field: default");
    assert_eq!(f.service.monitor_gain(), 0.0, "bad field: default");
    assert_eq!(
        f.service.input_device_uid(),
        Some(built_in_mic().uid),
        "good field"
    );
    assert_eq!(f.service.record_gain_db(), 7.0, "good field");
    // The apply still ran: the engine received the loaded input device.
    assert!(engine.s().input_calls.contains(&built_in_mic().uid));
    assert!(!f.service.input_devices().is_empty(), "the refresh ran");
}

#[test]
fn a_non_finite_stored_gain_falls_back_instead_of_clamping_to_the_maximum() {
    // Dart's clamp resolves NaN and Infinity through compareTo and returns
    // the range MAXIMUM, so a corrupt value would arrive as full-scale gain.
    // A parsed JSON value holds no non-finite number here (serde refuses
    // `1e400`, where Dart's decoder made it Infinity): the guard itself is
    // checked, and a value that is no number at all falls back the same way.
    assert!(serde_json::from_str::<Value>("1e400").is_err());
    assert_eq!(loaded_double(Some(&json!(f64::MAX)), 12.0), f64::MAX);
    assert_eq!(loaded_double(Some(&Value::Null), 12.0), 12.0);
    assert_eq!(loaded_double(None, 12.0), 12.0);
    let f = Fixture::new();
    f.doc(json!({
        "recordGainDb": null,
        "monitorGain": "NaN",
    }));
    f.service.load();
    assert_eq!(f.service.record_gain_db(), 12.0);
    assert_eq!(f.service.monitor_gain(), 0.0);
    // An out-of-range finite value clamps to the range.
    f.doc(json!({"recordGainDb": 1e300, "monitorGain": -4}));
    let restarted = f.build();
    restarted.load();
    assert_eq!(restarted.record_gain_db(), 40.0);
    assert_eq!(restarted.monitor_gain(), 0.0);
}

// --- headphones mark ---------------------------------------------------------------------

/// Wired headphones on an audio interface: no name can tell them from
/// monitors.
fn scarlett() -> AudioDevice {
    via(
        dflt(out("scarlett-out", "Scarlett 2i2 USB")),
        AudioTransport::Usb,
    )
}

fn speakers() -> AudioDevice {
    via(
        out("built-in-out", "MacBook Pro Speakers"),
        AudioTransport::BuiltIn,
    )
}

#[test]
fn a_marked_output_is_echo_free_and_allows_the_monitor() {
    let f = Fixture::new();
    f.outs(vec![speakers(), scarlett()]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = scarlett().uid);
    f.service.attach_engine(engine.handle());
    f.service.set_monitor_gain_live(0.5);
    assert!(f.service.output_markable());
    assert_eq!(f.service.output_to_mark().unwrap().uid, scarlett().uid);
    assert!(!f.service.monitor_allowed());
    assert_eq!(engine.s().echo_free.last().unwrap(), "");

    f.service.set_output_is_headphones(&scarlett().uid, true);
    assert!(f.service.output_marked_headphones());
    assert!(f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.5);
    assert_eq!(engine.s().echo_free.last().unwrap(), &scarlett().uid);
    assert_eq!(
        f.storage.last("headphoneOutputUids"),
        Some(json!([scarlett().uid]))
    );

    // The mark is the device's: the speakers taking over are judged as
    // themselves.
    engine.s().actual_output = speakers().uid;
    f.service.reconcile_monitor_safety();
    assert!(!f.service.monitor_allowed());
    assert_eq!(engine.s().echo_free.last().unwrap(), "");

    engine.s().actual_output = scarlett().uid;
    f.service.reconcile_monitor_safety();
    assert_eq!(engine.s().echo_free.last().unwrap(), &scarlett().uid);
    f.service.set_output_is_headphones(&scarlett().uid, false);
    assert!(!f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.0);
    assert_eq!(engine.s().echo_free.last().unwrap(), "");
    assert_eq!(f.storage.last("headphoneOutputUids"), Some(json!([])));
}

#[test]
fn only_a_device_of_its_own_transport_takes_a_mark_whatever_its_name() {
    // The system translates device names, so no name rule can tell a port or
    // a sink with speakers of its own: on a German Mac the built-in speakers
    // and an Intel Mac's one device for speakers AND jack passed every
    // English check.
    let f = Fixture::new();
    let markable = |name: &str, transport: AudioTransport| {
        f.outs(vec![AudioDevice {
            is_bluetooth: transport == AudioTransport::Bluetooth,
            ..via(dflt(out(&format!("out-{name}"), name)), transport)
        }]);
        f.service.refresh_devices();
        f.service.output_markable()
    };

    f.service.load();
    for (name, transport) in [
        ("MacBook Pro Speakers", AudioTransport::BuiltIn),
        ("MacBook Pro-Lautsprecher", AudioTransport::BuiltIn),
        ("Haut-parleurs MacBook Pro", AudioTransport::BuiltIn),
        ("Integrierte Ausgabe", AudioTransport::BuiltIn),
        ("Built-in Output", AudioTransport::BuiltIn),
        ("External Headphones", AudioTransport::BuiltIn),
        // Displays' own speakers come in over USB as often as not.
        ("LG UltraFine Display Audio", AudioTransport::Usb),
        ("Display Audio", AudioTransport::Usb),
        ("Studio Display Speakers", AudioTransport::Usb),
        ("Studio Display", AudioTransport::DisplayPort),
        ("Living Room TV", AudioTransport::Network),
        ("BlackHole 2ch", AudioTransport::Virtual),
        ("Multi-Output Device", AudioTransport::Aggregate),
        ("Unknown Device", AudioTransport::Unknown),
        ("UE BOOM 3", AudioTransport::Bluetooth),
        ("AirPods Pro", AudioTransport::Bluetooth),
    ] {
        assert!(!markable(name, transport), "{name}");
    }
    for (name, transport) in [
        ("Scarlett 2i2 USB", AudioTransport::Usb),
        ("WH-1000XM5", AudioTransport::Bluetooth),
        ("Apollo Twin", AudioTransport::Thunderbolt),
    ] {
        assert!(markable(name, transport), "{name}");
    }
}

#[test]
fn the_headphone_jack_allows_the_monitor_in_any_language_but_takes_no_mark() {
    // Its uid, unlike its name, is never translated. No mark: it can't tell
    // headphones from desk speakers plugged into it, and a mark kept for it
    // would one day apply to those.
    let f = Fixture::new();
    for name in [
        "External Headphones",
        "Externe Kopfhörer",
        "\u{5916}\u{90e8}\u{30d8}\u{30c3}\u{30c9}\u{30d5}\u{30a9}\u{30f3}",
    ] {
        f.outs(vec![via(
            dflt(out("BuiltInHeadphoneOutputDevice", name)),
            AudioTransport::BuiltIn,
        )]);
        f.service.load();
        let engine = Engine::with(|s| s.actual_output = "BuiltInHeadphoneOutputDevice".into());
        f.service.attach_engine(engine.handle());
        assert!(f.service.monitor_allowed(), "{name}");
        assert!(!f.service.output_markable(), "{name}");
        assert_eq!(engine.s().echo_free.last().unwrap(), "", "{name}");
        f.service.detach_engine(&engine.svc);
    }
}

#[test]
fn a_locked_monitor_points_to_the_mark_only_for_the_output_settings_will_show() {
    // The mic unit moved playback to a USB headset while the selection (the
    // default) is the speakers: Settings would show no card, so the fader
    // must not send the user there.
    let headset = via(out("usb-headset", "USB Headset"), AudioTransport::Usb);
    let f = Fixture::new();
    f.outs(vec![
        via(
            dflt(out("built-in-out", "MacBook Pro Speakers")),
            AudioTransport::BuiltIn,
        ),
        headset.clone(),
    ]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "usb-headset".into());
    f.service.attach_engine(engine.handle());
    assert!(f.service.output_markable());
    assert!(!f.service.suggest_headphones_mark());
    // On the chosen output itself, it does.
    f.service.set_output_device(Some(&headset.uid));
    assert!(f.service.suggest_headphones_mark());
}

fn airpods_out() -> AudioDevice {
    via(
        dflt(bt(out("airpods", "AirPods Pro"))),
        AudioTransport::Bluetooth,
    )
}

fn airpods_in() -> AudioDevice {
    via(
        dflt(bt(mic("airpods", "AirPods Pro"))),
        AudioTransport::Bluetooth,
    )
}

#[test]
fn an_unplugged_mic_pick_is_judged_as_the_default_it_falls_back_to() {
    // The engine records from the system default while the chosen mic is
    // unplugged; the check read the missing pick as no device and warned of
    // nothing, while the take recorded over the headset's call mode.
    let f = Fixture::new();
    f.outs(vec![airpods_out()]);
    f.ins(vec![airpods_in()]);
    f.doc(json!({"inputDeviceUid": "usb-mic", "inputDeviceName": "USB Mic"}));
    f.service.load();
    assert!(f.service.input_device_missing());
    assert!(f.service.bluetooth_in_out_conflict());
}

#[test]
fn the_check_at_rec_reads_the_devices_as_they_are_now() {
    // The lists are polled only while Settings is open: judged by the ones
    // from when the song opened, a headset connected since went unwarned.
    let f = Fixture::new();
    // Built-in speakers and mic.
    f.service.load();
    assert!(!f.service.bluetooth_in_out_conflict_now());
    f.outs(vec![airpods_out()]);
    f.ins(vec![airpods_in()]);
    assert!(!f.service.bluetooth_in_out_conflict(), "stale lists");
    assert!(f.service.bluetooth_in_out_conflict_now());
    f.outs(vec![built_in_out()]);
    f.ins(vec![built_in_mic()]);
    assert!(!f.service.bluetooth_in_out_conflict_now());
}

#[test]
fn a_bluetooth_mic_warns_whatever_output_is_picked() {
    // A take plays through the duplex mic unit, which renders to the mic's
    // own device: with the speakers picked as output, the AirPods mic still
    // put the AirPods (playing the take's backing) in call mode, unwarned.
    let f = Fixture::new();
    let not_default = |d: AudioDevice| AudioDevice {
        is_default: false,
        ..d
    };
    f.outs(vec![built_in_out(), not_default(airpods_out())]);
    f.ins(vec![built_in_mic(), not_default(airpods_in())]);
    f.doc(json!({
        "outputDeviceUid": built_in_out().uid,
        "inputDeviceUid": "airpods",
    }));
    f.service.load();
    assert!(f.service.bluetooth_in_out_conflict());
    assert!(f.service.bluetooth_in_out_conflict_now());
}

// REC judges the mic the engine has open, not the returned pick. A mic opened
// on a fallback stays there after the pick returns, and the engine names no
// uid for it: bound to the system default.

fn rec_bt(input: bool, is_default: bool) -> AudioDevice {
    let d = if input {
        mic("airpods", "AirPods Pro")
    } else {
        out("airpods", "AirPods Pro")
    };
    AudioDevice {
        is_default,
        ..via(bt(d), AudioTransport::Bluetooth)
    }
}

#[test]
fn rec_fallen_back_onto_a_bluetooth_default_is_warned() {
    let built_in = mic("built-in-mic", "MacBook Pro Microphone");
    let f = Fixture::new();
    f.outs(vec![rec_bt(false, true)]);
    f.ins(vec![built_in.clone(), rec_bt(true, true)]);
    f.doc(json!({"inputDeviceUid": built_in.uid}));
    f.service.load();
    let engine = Engine::with(|s| s.stream_state = 1);
    f.service.attach_engine(engine.handle());
    assert!(f.service.bluetooth_in_out_conflict_now());
    // Closed: the pick, the built-in mic.
    engine.s().stream_state = 0;
    assert!(!f.service.bluetooth_in_out_conflict_now());
}

#[test]
fn rec_open_on_a_device_it_names_is_judged_on_that_device() {
    let f = Fixture::new();
    f.outs(vec![rec_bt(false, false)]);
    f.ins(vec![built_in_mic(), rec_bt(true, false)]);
    f.doc(json!({"inputDeviceUid": built_in_mic().uid}));
    f.service.load();
    let engine = Engine::with(|s| {
        s.stream_state = 1;
        s.bound_input = "airpods".into();
    });
    f.service.attach_engine(engine.handle());
    assert!(f.service.bluetooth_in_out_conflict_now());
    engine.s().bound_input = built_in_mic().uid;
    assert!(!f.service.bluetooth_in_out_conflict_now());
}

#[test]
fn rec_fallen_back_off_a_bluetooth_pick_is_not_warned() {
    let f = Fixture::new();
    f.outs(vec![rec_bt(false, false)]);
    f.ins(vec![built_in_mic(), rec_bt(true, false)]);
    f.doc(json!({"inputDeviceUid": "airpods"}));
    f.service.load();
    let engine = Engine::with(|s| s.stream_state = 1);
    f.service.attach_engine(engine.handle());
    assert!(!f.service.bluetooth_in_out_conflict_now());
    // Closed: the pick, the headset.
    engine.s().stream_state = 0;
    assert!(f.service.bluetooth_in_out_conflict_now());
}

#[test]
fn the_same_bluetooth_headset_listed_under_two_uids_is_still_one_device() {
    let two = |uid: &str, name: &str, input: bool| {
        let d = if input {
            mic(uid, name)
        } else {
            out(uid, name)
        };
        via(dflt(bt(d)), AudioTransport::Bluetooth)
    };
    let f = Fixture::new();
    f.outs(vec![two("aa:output", "AirPods Pro", false)]);
    f.ins(vec![two("aa:input", "AirPods Pro", true)]);
    f.service.load();
    assert!(f.service.bluetooth_in_out_conflict());
    f.ins(vec![two("bb:input", "Jabra Evolve2", true)]);
    f.service.refresh_devices();
    assert!(!f.service.bluetooth_in_out_conflict());
}

#[test]
fn a_stored_mark_on_a_device_that_now_reads_as_a_speaker_is_not_in_force() {
    // Renamed since it was marked: the card is not shown for it, so a mark
    // still in force would be one nobody could see to clear.
    let renamed = via(
        dflt(bt(out("bt-out", "Party Speaker"))),
        AudioTransport::Bluetooth,
    );
    let f = Fixture::new();
    f.outs(vec![renamed.clone()]);
    f.doc(json!({"headphoneOutputUids": [renamed.uid]}));
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = "bt-out".into());
    f.service.attach_engine(engine.handle());
    f.service.set_monitor_gain_live(0.5);
    assert!(!f.service.output_markable());
    assert!(!f.service.monitor_allowed());
    assert_eq!(*engine.s().monitor_gains.last().unwrap(), 0.0);
    assert_eq!(engine.s().echo_free.last().unwrap(), "");
}

#[test]
fn settings_judges_the_choice_not_what_the_last_song_played_through() {
    // Settings opens from Home, after the song page let its engine go: what
    // that engine rendered to stayed cached, so the card offered (or hid) the
    // mark for the wrong output.
    let f = Fixture::new();
    f.outs(vec![speakers(), scarlett()]);
    f.doc(json!({"outputDeviceUid": scarlett().uid}));
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = speakers().uid);
    f.service.attach_engine(engine.handle());
    assert_eq!(f.service.output_to_mark().unwrap().uid, speakers().uid);
    f.service.detach_engine(&engine.svc);
    assert_eq!(f.service.output_to_mark().unwrap().uid, scarlett().uid);
    assert!(f.service.output_markable());
}

#[test]
fn detaching_another_engine_changes_nothing() {
    let f = Fixture::new();
    f.outs(vec![speakers(), scarlett()]);
    f.service.load();
    let attached = Engine::with(|s| s.actual_output = speakers().uid);
    let other = Engine::new();
    f.service.attach_engine(attached.handle());
    f.service.detach_engine(&other.svc);
    assert_eq!(f.service.output_to_mark().unwrap().uid, speakers().uid);
}

#[test]
fn clearing_a_mark_during_a_mic_open_withdraws_the_name_at_once() {
    // A name is held while the engine reports nothing rendering (the mic
    // opening), but not past the mark it came from: the engine would act on
    // it the moment the device bound.
    let f = Fixture::new();
    f.outs(vec![speakers(), scarlett()]);
    f.service.load();
    let engine = Engine::with(|s| s.actual_output = scarlett().uid);
    f.service.attach_engine(engine.handle());
    f.service.set_output_is_headphones(&scarlett().uid, true);
    assert_eq!(engine.s().echo_free.last().unwrap(), &scarlett().uid);
    engine.s().actual_output = String::new();
    f.service.reconcile_monitor_safety();
    assert_eq!(
        engine.s().echo_free.last().unwrap(),
        &scarlett().uid,
        "held through the open"
    );
    f.service.set_output_is_headphones(&scarlett().uid, false);
    assert_eq!(engine.s().echo_free.last().unwrap(), "");
}

#[test]
fn a_stored_mark_is_loaded_but_not_over_one_changed_while_loading() {
    let f = Fixture::new();
    f.outs(vec![speakers(), scarlett()]);
    f.doc(json!({"headphoneOutputUids": [scarlett().uid, 7, "other-out"]}));
    f.service.load();
    assert!(f.service.output_marked_headphones());

    let again = f.build();
    let loading = f.parked_load(&again);
    again.refresh_devices();
    again.set_output_is_headphones(&scarlett().uid, false);
    f.storage.open_gate();
    loading.join().unwrap();
    assert!(!again.output_marked_headphones());
    assert_eq!(
        f.storage.last("headphoneOutputUids"),
        Some(json!(["other-out"]))
    );
}

// --- input channel -------------------------------------------------------------------------

#[test]
fn the_input_channel_is_chosen_per_input_device_and_persisted_with_it() {
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.service.load();
    assert_eq!(f.service.input_channel(), AUTO);

    f.service.set_input_device(Some(&interface().uid));
    f.service.set_input_channel(INPUT_2);
    assert_eq!(f.service.input_channel(), INPUT_2);

    // Another device has its own choice, Auto until made.
    f.service.set_input_device(Some(&built_in_mic().uid));
    assert_eq!(f.service.input_channel(), AUTO);
    f.service.set_input_channel(INPUT_1);

    f.service.set_input_device(Some(&interface().uid));
    assert_eq!(f.service.input_channel(), INPUT_2);
    assert_eq!(
        f.storage.last("inputChannelByUid"),
        Some(json!({interface().uid: 2, built_in_mic().uid: 1}))
    );
    // In the order they were first chosen, as Dart's map kept them.
    let written = f.storage.s().last_write.clone().unwrap();
    let keys: Vec<&String> = written["inputChannelByUid"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, [&interface().uid, &built_in_mic().uid]);

    // A fresh start reads both back.
    f.storage.s().doc = Some(written);
    let restarted = f.build();
    restarted.load();
    assert_eq!(restarted.input_channel(), INPUT_2);
    restarted.set_input_device(Some(&built_in_mic().uid));
    assert_eq!(restarted.input_channel(), INPUT_1);
}

#[test]
fn the_input_channel_reaches_the_engine_on_attach_on_a_change_and_with_the_input_device() {
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 2},
    }));
    f.service.load();

    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    assert_eq!(engine.s().input_channel_calls, [2], "applied on attach");

    f.service.set_input_channel(INPUT_1);
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        1,
        "applied on change"
    );

    // Switching the input device applies THAT device's choice.
    f.service.set_input_device(Some(&built_in_mic().uid));
    assert_eq!(*engine.s().input_channel_calls.last().unwrap(), 0);
    assert_eq!(engine.s().input_calls.last().unwrap(), &built_in_mic().uid);
}

#[test]
fn the_channel_count_follows_the_current_input() {
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.service.load();
    assert_eq!(f.service.input_channel_count(), 1, "the system default mic");
    f.service.set_input_device(Some(&interface().uid));
    assert_eq!(f.service.input_channel_count(), 2);
    // A device that changes its format is seen on the next device poll.
    f.channel_count(&interface().uid, 1);
    f.service.refresh_devices();
    assert_eq!(f.service.input_channel_count(), 1);
}

#[test]
fn a_corrupt_stored_choice_is_ignored_not_applied() {
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface(), other_interface()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {
            interface().uid: 7,
            built_in_mic().uid: "two",
            other_interface().uid: 1,
        },
    }));
    f.service.load();
    assert_eq!(f.service.input_channel(), AUTO);
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    assert_eq!(
        engine.s().input_channel_calls,
        [0],
        "the bad value never ran"
    );
    // One bad entry costs only that device's choice.
    f.service.set_input_device(Some(&other_interface().uid));
    assert_eq!(f.service.input_channel(), INPUT_1);
}

#[test]
fn a_whole_number_stored_as_a_double_is_no_choice() {
    // Dart's `value is int` refused a stored 1.0 (a double).
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 1.0},
    }));
    f.service.load();
    assert_eq!(f.service.input_channel(), AUTO);
}

#[test]
fn the_technique_pages_own_engine_gets_the_inputs_device_and_channel() {
    // It is never attached here; a new engine starts on Auto, and its takes
    // blended in the input the singer had picked to keep out.
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 1},
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.apply_input_to(&engine.svc);
    assert_eq!(engine.s().input_calls, [interface().uid]);
    assert_eq!(engine.s().input_channel_calls, [1]);
    assert_eq!(engine.s().input_gain_calls.len(), 1, "and the record gain");
}

#[test]
fn the_technique_engine_is_keyed_on_an_interface_plugged_in_since_the_last_poll() {
    // Nothing polls the devices outside Settings: read off the list from
    // launch, the plugged-in interface was missing and its takes ran on the
    // built-in mic's choice.
    let f = Fixture::new();
    f.ins(vec![built_in_mic()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 1},
        "recordGainDb": 0.0,
    }));
    f.service.load();
    // Plugged in; no poll since.
    f.ins(vec![built_in_mic(), interface()]);
    let engine = Engine::new();
    f.service.apply_input_to(&engine.svc);
    assert_eq!(engine.s().input_channel_calls, [1]);
    assert!((engine.s().input_gain_calls[0] - 1.0).abs() < 1e-9);
    assert_eq!(engine.s().input_gain_calls.len(), 1);
}

#[test]
fn a_take_start_with_the_mic_open_keys_on_the_device_it_is_bound_to() {
    // The stream retries the chosen device only when it opens: monitoring
    // that opened on the fallback stays there after the chosen interface
    // returns, and keyed on the interface the fallback recorded under its
    // choice.
    let f = Fixture::new();
    f.ins(vec![built_in_mic()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 2, built_in_mic().uid: 1},
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    // Plugged back in.
    f.ins(vec![built_in_mic(), interface()]);

    // Open, on the fallback.
    engine.s().stream_state = 1;
    engine.s().bound_input = built_in_mic().uid;
    f.service.sync_input_channel();
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        1,
        "the bound mic's"
    );

    // Open on the system default.
    engine.s().bound_input = String::new();
    f.service.sync_input_channel();
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        1,
        "the default's"
    );

    // Closed: the next open takes the interface.
    engine.s().stream_state = 0;
    f.service.sync_input_channel();
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        2,
        "the interface's"
    );
}

#[test]
fn a_take_start_re_keys_the_channel_without_switching_devices() {
    // Doing the full device refresh there also switched a returned device
    // back synchronously (re-opening the mic, or blocking on the output host)
    // right as the take armed. That stays the device poll's.
    let f = Fixture::new();
    f.ins(vec![built_in_mic()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 2},
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().input_calls.clear();
    // Plugged back in.
    f.ins(vec![built_in_mic(), interface()]);

    f.service.sync_input_channel();
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        2,
        "the interface's"
    );
    assert!(engine.s().input_calls.is_empty(), "no device switch");

    // The poll still switches it back.
    f.service.refresh_devices();
    assert_eq!(engine.s().input_calls, [interface().uid]);
}

#[test]
fn system_default_keeps_each_default_device_on_its_own_choice() {
    // Keyed by the selection, a choice made while one interface was the
    // default followed the default onto the next.
    let built_in_not_default = mic("built-in-mic", "MacBook Pro Microphone");
    let interface_default = dflt(mic("usb-2in", "USB Audio Interface"));
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    f.channel_count(&built_in_mic().uid, 2);
    f.service.set_input_channel(INPUT_2);

    f.ins(vec![built_in_not_default, interface_default]);
    f.service.refresh_devices();
    assert_eq!(f.service.input_channel(), AUTO);
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        0,
        "the new default"
    );
    assert_eq!(f.service.input_channel_count(), 2, "the new default's");

    f.ins(vec![built_in_mic(), interface()]);
    f.service.refresh_devices();
    assert_eq!(f.service.input_channel(), INPUT_2);
    assert_eq!(*engine.s().input_channel_calls.last().unwrap(), 2);
}

#[test]
fn an_unplugged_choice_gives_way_to_the_device_the_engine_falls_back_to() {
    let f = Fixture::new();
    f.ins(vec![built_in_mic(), interface()]);
    f.doc(json!({
        "inputDeviceUid": interface().uid,
        "inputChannelByUid": {interface().uid: 2},
    }));
    f.service.load();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    assert_eq!(*engine.s().input_channel_calls.last().unwrap(), 2);

    f.ins(vec![built_in_mic()]);
    f.service.refresh_devices();
    assert_eq!(f.service.input_channel(), AUTO);
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        0,
        "the fallback mic"
    );
    assert_eq!(f.service.input_channel_count(), 1, "the fallback mic");

    f.ins(vec![built_in_mic(), interface()]);
    f.service.refresh_devices();
    assert_eq!(f.service.input_channel(), INPUT_2);
    assert_eq!(
        *engine.s().input_channel_calls.last().unwrap(),
        2,
        "it came back"
    );
}

// --- song_practice_take_flow_test.dart (the service-level part) -----------------------------

#[test]
fn a_take_runs_on_the_choice_of_the_default_capturing_when_it_starts() {
    // The interface was the default when its choice was made; the default
    // moved back to the built-in mic (a poll saw it), then to the interface
    // again, unseen, before the take. The take start keys on the device
    // capturing then.
    let mic_dev = |is_default: bool| AudioDevice {
        is_default,
        ..mic("built-in", "Built-in")
    };
    let iface = |is_default: bool| AudioDevice {
        is_default,
        ..mic("iface", "Interface")
    };
    let f = Fixture::new();
    f.channel_count("built-in", 2);
    f.channel_count("iface", 2);
    f.outs(vec![]);
    f.ins(vec![iface(true)]);
    f.service.refresh_devices();
    f.service.set_input_channel(INPUT_2);
    f.ins(vec![mic_dev(true), iface(false)]);
    // The default back on the built-in mic.
    f.service.refresh_devices();

    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    f.ins(vec![mic_dev(false), iface(true)]);
    // What the recording controller calls as the take arms.
    f.service.sync_input_channel();
    assert_eq!(*engine.s().input_channel_calls.last().unwrap(), INPUT_2);
}

// --- settings_screen_test.dart (the service-level part) ----------------------------------

fn on_disk() -> (tempfile::TempDir, Arc<dyn SettingsStorage>) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn SettingsStorage> = Arc::new(LocalStorageService::with_root(dir.path()));
    (dir, storage)
}

#[test]
fn the_input_channel_choice_is_offered_for_a_two_channel_input_only() {
    let channels = Arc::new(Mutex::new(2_u32));
    let c = Arc::clone(&channels);
    let (_dir, storage) = on_disk();
    let settings = AppSettingsService::with_queries(
        storage,
        DeviceQueries {
            enumerate_outputs: Box::new(Vec::<AudioDevice>::new),
            enumerate_inputs: Box::new(Vec::<AudioDevice>::new),
            input_channel_count: Box::new(move |_| *c.lock().unwrap()),
        },
    );
    // The screen's first device refresh.
    settings.refresh_devices();
    // The panel offers the choice from 2 channels.
    assert_eq!(settings.input_channel_count(), 2);
    assert_eq!(settings.input_channel(), AUTO);
    settings.set_input_channel(INPUT_2);
    assert_eq!(settings.input_channel(), INPUT_2);

    // A mono input has nothing to choose between.
    *channels.lock().unwrap() = 1;
    settings.refresh_devices();
    assert_eq!(settings.input_channel_count(), 1);
}

#[test]
fn the_input_channel_choice_names_the_device_it_applies_to() {
    // The chosen interface is unplugged, so the default records, and a
    // choice made under the chosen one's offline row lands on the default.
    let (_dir, storage) = on_disk();
    let settings = AppSettingsService::with_queries(
        storage,
        DeviceQueries {
            enumerate_outputs: Box::new(Vec::<AudioDevice>::new),
            enumerate_inputs: Box::new(|| {
                vec![AudioDevice {
                    is_default: true,
                    is_input: true,
                    ..AudioDevice::new("second", "Second Interface")
                }]
            }),
            input_channel_count: Box::new(|_| 2),
        },
    );
    settings.set_input_device(Some("unplugged"));
    settings.refresh_devices();
    assert_eq!(settings.input_channel_device_name(), "Second Interface");
    assert_eq!(settings.input_channel(), AUTO);
    assert!(!settings.persist_failed());
}

#[test]
fn the_headphones_mark_is_offered_for_an_output_no_name_can_tell() {
    let outs = Arc::new(Mutex::new(vec![AudioDevice {
        is_default: true,
        transport: AudioTransport::Usb,
        ..AudioDevice::new("scarlett", "Scarlett 2i2 USB")
    }]));
    let o = Arc::clone(&outs);
    let (_dir, storage) = on_disk();
    let settings = AppSettingsService::with_queries(
        Arc::clone(&storage),
        DeviceQueries {
            enumerate_outputs: Box::new(move || o.lock().unwrap().clone()),
            enumerate_inputs: Box::new(Vec::<AudioDevice>::new),
            input_channel_count: Box::new(|_| 0),
        },
    );
    settings.refresh_devices();

    // The "Headphones only" card, for the Scarlett.
    assert!(settings.output_markable());
    assert_eq!(settings.output_to_mark().unwrap().name, "Scarlett 2i2 USB");
    assert!(!settings.monitor_allowed());
    settings.set_output_is_headphones("scarlett", true);
    assert!(settings.output_marked_headphones());
    assert!(settings.monitor_allowed());
    // It reached the disk, as the Flutter app would read it back.
    let written = storage.read("settings", "audio").unwrap().unwrap();
    assert_eq!(written["headphoneOutputUids"], json!(["scarlett"]));

    // The built-in speakers never take the mark.
    *outs.lock().unwrap() = vec![AudioDevice {
        is_default: true,
        transport: AudioTransport::BuiltIn,
        ..AudioDevice::new("spk", "MacBook Pro Speakers")
    }];
    settings.refresh_devices();
    assert!(!settings.output_markable());
}

// --- The Rust side --------------------------------------------------------------------------

#[test]
fn the_document_is_written_in_darts_key_order_and_number_format() {
    let (dir, storage) = on_disk();
    let settings = AppSettingsService::with_queries(
        storage,
        DeviceQueries {
            enumerate_outputs: Box::new(Vec::<AudioDevice>::new),
            enumerate_inputs: Box::new(Vec::<AudioDevice>::new),
            input_channel_count: Box::new(|_| 0),
        },
    );
    settings.set_output_device(Some("hp"));
    let text = std::fs::read_to_string(dir.path().join("settings").join("audio.json")).unwrap();
    assert_eq!(
        text,
        "{\"outputDeviceUid\":\"hp\",\"inputDeviceUid\":null,\"outputDeviceName\":null,\
         \"inputDeviceName\":null,\"monitorReverbWet\":0.2,\"recordGainDb\":12.0,\
         \"monitorGain\":0.0,\"inputChannelByUid\":{},\"headphoneOutputUids\":[]}"
    );
}

#[test]
fn a_listener_can_be_removed() {
    let f = Fixture::new();
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    let id = f.service.add_listener(Box::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    }));
    f.service.set_monitor_gain_live(0.3);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    f.service.remove_listener(id);
    f.service.set_monitor_gain_live(0.4);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn a_listener_runs_with_no_lock_held() {
    // It may read the settings, and even persist a choice of its own, from
    // inside a notification (a persist's own included).
    let f = Fixture::new();
    f.storage.s().fail_writes = true;
    let service = Arc::clone(&f.service);
    let reentered = Arc::new(AtomicUsize::new(0));
    let r = Arc::clone(&reentered);
    f.service.add_listener(Box::new(move || {
        let _ = service.monitor_allowed();
        if r.fetch_add(1, Ordering::SeqCst) == 0 {
            service.set_monitor_reverb(0.4);
        }
    }));
    f.service.set_monitor_reverb(0.6);
    assert!(reentered.load(Ordering::SeqCst) >= 2);
    assert_eq!(f.service.monitor_reverb_wet(), 0.4);
    assert!(f.service.persist_failed());
}

#[test]
fn live_gains_clamp_as_darts_clamp_did_and_reach_the_engine() {
    let f = Fixture::new();
    let engine = Engine::new();
    f.service.attach_engine(engine.handle());
    engine.s().input_gain_calls.clear();
    f.service.set_record_gain_db_live(60.0);
    assert_eq!(f.service.record_gain_db(), 40.0);
    f.service.set_record_gain_db_live(f64::NAN);
    // NaN clamps to the maximum, as Dart's compareTo-based clamp did.
    assert_eq!(f.service.record_gain_db(), 40.0);
    f.service.set_record_gain_db_live(-1.0);
    assert_eq!(f.service.record_gain_db(), 0.0);
    assert_eq!(engine.s().input_gain_calls, [100.0, 100.0, 1.0]);
    f.service.set_monitor_gain_live(2.0);
    assert_eq!(f.service.monitor_gain(), 1.0);
    assert_eq!(f.service.engine_sample_rate(), 48_000.0);
}
