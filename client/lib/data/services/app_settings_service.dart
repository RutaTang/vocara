import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../domain/models/audio_device.dart';
import 'diagnostics_log.dart';
import 'graph_audio_playback_service.dart';
import 'local_storage_service.dart';

/// Global, persisted app settings — the audio output + input devices. A [ChangeNotifier] so the
/// settings panel rebuilds on change. It brokers between the persisted choice and the
/// currently-active engine: enumeration is static (works with no song open), while applying a
/// choice is forwarded to the attached engine and persisted for the next engine.
class AppSettingsService extends ChangeNotifier {
  /// [enumerateOutputs] / [enumerateInputs] are injectable for tests; they
  /// default to the live CoreAudio enumeration.
  AppSettingsService({
    required LocalStorageService storage,
    List<AudioDevice> Function()? enumerateOutputs,
    List<AudioDevice> Function()? enumerateInputs,
  }) : _storage = storage,
       _enumerateOutputs = enumerateOutputs ?? enumerateOutputDevices,
       _enumerateInputs = enumerateInputs ?? enumerateInputDevices;

  final LocalStorageService _storage;
  final List<AudioDevice> Function() _enumerateOutputs;
  final List<AudioDevice> Function() _enumerateInputs;

  // One JSON document under the app-support `settings` collection.
  static const _collection = 'settings';
  static const _docId = 'audio';
  static const _kOutputUid = 'outputDeviceUid';
  static const _kInputUid = 'inputDeviceUid';
  static const _kOutputName = 'outputDeviceName';
  static const _kInputName = 'inputDeviceName';
  static const _kReverbWet = 'monitorReverbWet';
  static const _kRecordGainDb = 'recordGainDb';
  static const _kMonitorGain = 'monitorGain';

  // Fixed monitor-reverb character; the user controls only the wet amount.
  static const _monitorReverbRoom = 0.6;
  static const _monitorReverbDamping = 0.5;

  /// Record/input gain range (dB) for the mic-input fader.
  static const recordGainMinDb = 0.0;
  static const recordGainMaxDb = 40.0;

  String? _outputDeviceUid;
  String? _inputDeviceUid;
  // Last-known display names of the persisted devices, persisted alongside the
  // UIDs so the settings panel can label an unplugged choice by name.
  String? _outputDeviceName;
  String? _inputDeviceName;
  double _monitorReverbWet = 0.2;
  // Mic-input gain (= the engine's default): +12 dB records a typical take with
  // tracking headroom (peaks ~-6 dBFS); the take file is 32-bit float, so even
  // a hotter fader never clips the recording.
  double _recordGainDb = 12.0;
  double _monitorGain =
      0.0; // hear-yourself level (linear); 0 = off (e.g. on a speaker)
  List<AudioDevice> _outputDevices = const [];
  List<AudioDevice> _inputDevices = const [];
  GraphAudioPlaybackService? _engine;

  /// The persisted keys the user has already chosen this session. [load] is
  /// fire-and-forget (the provider graph starts it and drops the future), so
  /// it can resolve at ANY moment — including after the singer has moved a
  /// fader. A field the user set is never adopted from the document:
  /// otherwise the change is reverted on screen, re-pushed to the engine, and
  /// then written back as the old value by the next [_persist], which
  /// serializes all seven fields at once. Keyed by the same `_kXxx` constants
  /// the document uses, so a setter and its load guard cannot drift apart.
  final Set<String> _userChosen = {};

  /// [load] reads still parked, and whether a [_persist] was held back by one.
  int _loadsInFlight = 0;
  bool _persistDeferred = false;

  /// The persisted output device UID, or null = system default.
  String? get outputDeviceUid => _outputDeviceUid;

  /// The persisted input device UID, or null = system default.
  String? get inputDeviceUid => _inputDeviceUid;

  /// Last-known display name of the persisted output device, or null when
  /// following the system default. Kept current while the device is connected
  /// so the panel can still label the choice after it's unplugged.
  String? get outputDeviceName => _outputDeviceName;

  /// Last-known display name of the persisted input device (see [outputDeviceName]).
  String? get inputDeviceName => _inputDeviceName;

  /// True when an output device is persisted but absent from the live list
  /// (unplugged). The engine falls back to the system default; the UID stays
  /// persisted so the choice is restored when the device returns.
  bool get outputDeviceMissing => _isMissing(_outputDevices, _outputDeviceUid);

  /// True when an input device is persisted but absent from the live list
  /// (see [outputDeviceMissing]).
  bool get inputDeviceMissing => _isMissing(_inputDevices, _inputDeviceUid);

  /// The currently-enumerated output devices.
  List<AudioDevice> get outputDevices => _outputDevices;

  /// The currently-enumerated input devices.
  List<AudioDevice> get inputDevices => _inputDevices;

  /// The selected input device (uid + name), or null when following the system default. Used to
  /// point recording / live analysis at the same device as the engine (not the OS default).
  AudioDevice? get selectedInputDevice {
    final uid = _inputDeviceUid;
    if (uid == null || uid.isEmpty) return null;
    return _byUid(_inputDevices, uid) ??
        AudioDevice(uid: uid, name: _inputDeviceName ?? '');
  }

  /// Monitor reverb amount (0..1) — how much space the singer hears on their own voice.
  double get monitorReverbWet => _monitorReverbWet;

  /// Record/input gain in dB — how loud the captured voice is recorded.
  double get recordGainDb => _recordGainDb;

  /// Monitor (hear-yourself) gain, linear 0..1; 0 = off.
  double get monitorGain => _monitorGain;

  static double _dbToLinear(double db) => math.pow(10.0, db / 20.0).toDouble();

  /// The engine's fixed processing rate (Hz) — shown in the panel.
  double get engineSampleRate => _engine?.sampleRate ?? 48000.0;

  // The device with [uid] from a list, or null (also for a null/empty uid).
  static AudioDevice? _byUid(List<AudioDevice> list, String? uid) {
    if (uid == null || uid.isEmpty) return null;
    for (final d in list) {
      if (d.uid == uid) return d;
    }
    return null;
  }

  // An explicit pick that isn't in the live list (i.e. unplugged).
  static bool _isMissing(List<AudioDevice> list, String? uid) =>
      uid != null && uid.isNotEmpty && _byUid(list, uid) == null;

  // The effective device (an explicit pick, else the system default) from a list.
  AudioDevice? _effective(List<AudioDevice> list, String? uid) {
    if (uid != null && uid.isNotEmpty) return _byUid(list, uid);
    for (final d in list) {
      if (d.isDefault) return d;
    }
    return null;
  }

  /// True when the effective input and output are the SAME Bluetooth device: opening its mic
  /// forces HFP and degrades the output (a hardware limit — the UI warns to pick another mic).
  bool get bluetoothInOutConflict {
    final out = _effective(_outputDevices, _outputDeviceUid);
    final inp = _effective(_inputDevices, _inputDeviceUid);
    if (out == null || inp == null) return false;
    return out.uid == inp.uid && (out.isBluetooth || inp.isBluetooth);
  }

  /// Whether the audible monitor (hear-yourself) is allowed: only when the device actually
  /// RENDERING is headphones (no acoustic path back to the mic). On a built-in/external SPEAKER
  /// the mic re-captures the monitored voice → howling feedback, so the monitor is locked off.
  ///
  /// Judged from what the engine is playing through, not from the selection: while the duplex mic
  /// unit owns the output, playback follows the microphone's endpoint or the system default, so
  /// "the user picked AirPods" can be true while the sound is coming out of the built-in speakers.
  /// The engine hard-mutes the monitor in that case anyway (its `EVENT_OUTPUT_REROUTED`); this
  /// keeps the fader honest about it. Falls back to the selection when the engine reports nothing
  /// or names a device the panel cannot enumerate — never worse than judging the selection alone.
  bool get monitorAllowed => _isHeadphoneLike(_renderingDevice);

  /// The device to judge feedback safety on: what the engine is actually rendering to, else the
  /// effective selection.
  AudioDevice? get _renderingDevice {
    if (_actualOutputUid.isNotEmpty) {
      final d = _byUid(_outputDevices, _actualOutputUid);
      if (d != null) return d;
    }
    return _effective(_outputDevices, _outputDeviceUid);
  }

  /// Last uid the engine reported rendering to. CACHED, not read per access: [monitorAllowed] is
  /// read from widget builds (the MON fader's enabled state), and resolving it natively walks the
  /// device list. Refreshed wherever the routing can have moved — which is also why the ENGINE
  /// owns the hard mute rather than trusting this: a stale value here is bounded by one tick,
  /// and the engine's own reroute check has no such window.
  String _actualOutputUid = '';

  static bool _isHeadphoneLike(AudioDevice? out) {
    if (out == null) return false; // unknown → safe default: off
    final n = out.name.toLowerCase();
    // A device named as a speaker never is, Bluetooth or not: macOS reports
    // only the transport, and a Bluetooth speaker taken for headphones fed the
    // monitor straight back into the mic (it is kept out of the AEC
    // reference, so nothing cancels it).
    if (_speakerNames.any(n.contains)) return false;
    if (out.isBluetooth) return true; // otherwise Bluetooth ≈ headphones
    return n.contains('headphone') ||
        n.contains('earphone') ||
        n.contains('airpod');
  }

  /// Name fragments of loudspeakers: the generic words, and the common
  /// Bluetooth speaker lines whose names never say so (UE BOOM, JBL Flip…).
  /// Only unambiguous ones — a headphone line sharing a speaker brand's name
  /// would lose its monitor.
  static const List<String> _speakerNames = [
    'speaker',
    'soundbar',
    'homepod',
    'sonos',
    'boom', // UE BOOM / MEGABOOM / WONDERBOOM / HYPERBOOM
    'jbl flip',
    'jbl charge',
    'jbl clip',
    'jbl go',
    'jbl xtreme',
    'jbl pulse',
    'partybox',
    'soundlink flex',
    'soundlink micro',
    'soundlink revolve',
    'soundlink color',
    'beosound',
    'echo dot',
  ];

  /// Re-evaluate monitor feedback safety against the device the engine is CURRENTLY rendering to
  /// and push the resulting gain. Cheap; call it from any tick that could have seen the output
  /// move — opening the mic hands the output to the duplex unit, which can reroute playback
  /// without any device set changing, so [refreshDevices] alone would not notice.
  void reconcileMonitorSafety() {
    final changed = _refreshActualOutput();
    _applyMonitorGain();
    // The MON fader's enabled state is derived from this, so a change has to reach the panel.
    if (changed) notifyListeners();
  }

  // Re-read the engine's rendering device into [_actualOutputUid]; true when it moved. Never
  // notifies — callers that already run outside a build own that.
  bool _refreshActualOutput() {
    final actual = _engine?.actualOutputDevice() ?? '';
    if (actual == _actualOutputUid) return false;
    _actualOutputUid = actual;
    return true;
  }

  // The monitor gain actually sent to the engine: the fader value on headphones, forced to 0 on a
  // speaker so a persisted/raised fader can never feed back after switching output.
  void _applyMonitorGain() =>
      _engine?.setMonitorGain(monitorAllowed ? _monitorGain : 0.0);

  static String? _loadedString(Object? v) => v is String ? v : null;

  /// Non-finite is not a preference: `num.clamp` resolves NaN/Infinity through
  /// compareTo and returns the range MAXIMUM, so a corrupt `1e400` would arrive
  /// as 40 dB of input gain or a fully-open monitor rather than the default.
  static double _loadedDouble(Object? v, double fallback) =>
      v is num && v.toDouble().isFinite ? v.toDouble() : fallback;

  /// Load persisted settings and enumerate devices. Safe to call at startup.
  ///
  /// The caller starts this and drops the future, so it resolves at an
  /// arbitrary point in the session. Two consequences shape the body:
  /// adoption is PER FIELD (a value the user already chose is skipped — see
  /// [_userChosen]), and every field is parsed tolerantly, because one
  /// wrong-typed entry in an otherwise valid document must not cost
  /// [_applyAllToEngine] — the only call that pushes persisted devices and
  /// gains to an engine attached before the read resolved. A strict cast here
  /// left the device panel permanently empty with nothing on screen to say
  /// why.
  Future<void> load() async {
    Map<String, dynamic>? doc;
    _loadsInFlight++;
    try {
      doc = await _storage.read(_collection, _docId);
    } catch (_) {
      doc =
          null; // never let a read failure abort startup; fall back to defaults
    } finally {
      _loadsInFlight--;
    }
    // A uid and its last-known display name are ONE choice — adopt or skip
    // them together, or an adopted name would label the user's new device.
    if (!_userChosen.contains(_kOutputUid)) {
      _outputDeviceUid = _loadedString(doc?[_kOutputUid]);
      _outputDeviceName = _loadedString(doc?[_kOutputName]);
    }
    if (!_userChosen.contains(_kInputUid)) {
      _inputDeviceUid = _loadedString(doc?[_kInputUid]);
      _inputDeviceName = _loadedString(doc?[_kInputName]);
    }
    // Clamp every loaded value to the same range the live setters enforce — a
    // tampered / out-of-range stored value must not reach the engine verbatim
    // (e.g. a huge recordGainDb → enormous input gain, monitorGain > 1 → howling
    // feedback) and must keep the mixer fader math valid.
    if (!_userChosen.contains(_kReverbWet)) {
      _monitorReverbWet = _loadedDouble(doc?[_kReverbWet], 0.2).clamp(0.0, 1.0);
    }
    if (!_userChosen.contains(_kRecordGainDb)) {
      _recordGainDb = _loadedDouble(
        doc?[_kRecordGainDb],
        12.0,
      ).clamp(recordGainMinDb, recordGainMaxDb);
    }
    if (!_userChosen.contains(_kMonitorGain)) {
      _monitorGain = _loadedDouble(doc?[_kMonitorGain], 0.0).clamp(0.0, 1.0);
    }
    // If a screen attached the engine before this load() resolved, it adopted
    // constructor defaults — re-push the now-loaded values to it.
    _applyAllToEngine();
    refreshDevices();
    // refreshDevices only notifies when a device SET changed, and by now it
    // usually has not (attachEngine enumerates first). The loaded gains still
    // have to reach the panel, so notify unconditionally — always past an
    // await here, never inside the build that created the service.
    notifyListeners();
    // A choice persisted while the read was parked was held back (see
    // [_persist]); write it now that the untouched fields hold the document.
    if (_persistDeferred && _loadsInFlight == 0) {
      _persistDeferred = false;
      await _persist();
    }
  }

  /// Re-query the available output + input devices (static; no engine required). Only notifies
  /// when a set actually changed, so it's safe to poll on a timer for hot-plug detection.
  void refreshDevices() {
    final outs = _enumerateOutputs();
    final ins = _enumerateInputs();
    final changed =
        _deviceSig(outs) != _deviceSig(_outputDevices) ||
        _deviceSig(ins) != _deviceSig(_inputDevices);
    if (changed) {
      // A persisted device coming back after an unplug: the engine fell back to
      // the system default meanwhile, so switch it back to the user's choice live.
      final outReturned = _returned(_outputDevices, outs, _outputDeviceUid);
      final inReturned = _returned(_inputDevices, ins, _inputDeviceUid);
      _outputDevices = outs;
      _inputDevices = ins;
      if (outReturned) _engine?.setOutputDevice(_outputDeviceUid!);
      if (inReturned) _engine?.setInputDevice(_inputDeviceUid!);
      _syncLastKnownNames();
      // monitorAllowed is derived from the output set, so reconcile the engine's
      // monitor gain whenever it changes — the initial attach can run before the
      // (microtask-deferred) device list is populated and force the gain to 0;
      // without this re-apply the MON fader reads up but the engine stays muted.
      _refreshActualOutput();
      _applyMonitorGain();
      notifyListeners();
    }
  }

  // A change signature over a device set (the fields the panel renders) to skip no-op rebuilds.
  static String _deviceSig(List<AudioDevice> ds) =>
      ds.map((d) => '${d.uid}:${d.isDefault}:${d.rate}:${d.name}').join('|');

  // The persisted device reappeared: absent from the previous list, present in the
  // fresh one. The previous list must be non-empty so the very first enumeration
  // (load/attach, where _applyAllToEngine already pushed the choice) doesn't count.
  static bool _returned(
    List<AudioDevice> before,
    List<AudioDevice> after,
    String? uid,
  ) => before.isNotEmpty && _isMissing(before, uid) && !_isMissing(after, uid);

  // Track device renames: while a persisted device is connected, keep its stored
  // display name current (the name labels the offline pseudo-row in the panel).
  void _syncLastKnownNames() {
    var dirty = false;
    final out = _byUid(_outputDevices, _outputDeviceUid);
    if (out != null && out.name != _outputDeviceName) {
      _outputDeviceName = out.name;
      dirty = true;
    }
    final inp = _byUid(_inputDevices, _inputDeviceUid);
    if (inp != null && inp.name != _inputDeviceName) {
      _inputDeviceName = inp.name;
      dirty = true;
    }
    // Fire-and-forget: a failed name persist only degrades the offline label.
    // [_persist] absorbs the failure itself, so nothing escapes into the root
    // zone as a spurious crash report.
    if (dirty) unawaited(_persist());
  }

  /// Attach the active engine. Applies the persisted devices immediately so a freshly-created
  /// engine adopts the saved choices; refreshes the device lists from the live host.
  void attachEngine(GraphAudioPlaybackService engine) {
    _engine = engine;
    _applyAllToEngine();
    // attachEngine runs inside the ViewModel constructor, which is built during a widget build;
    // defer the device refresh so its notifyListeners() doesn't fire mid-build.
    Future.microtask(refreshDevices);
  }

  /// Pushes the current devices + gains + reverb into the active engine. Used by
  /// both attachEngine and load(), so a song opened BEFORE load() resolved (which
  /// attached the engine with constructor defaults) still gets the persisted
  /// values once they arrive.
  void _applyAllToEngine() {
    final engine = _engine;
    if (engine == null) return;
    final out = _outputDeviceUid;
    if (out != null && out.isNotEmpty) engine.setOutputDevice(out);
    final inp = _inputDeviceUid;
    if (inp != null && inp.isNotEmpty) engine.setInputDevice(inp);
    engine.setMonitorReverb(
      _monitorReverbRoom,
      _monitorReverbDamping,
      _monitorReverbWet,
    );
    engine.setInputGain(_dbToLinear(_recordGainDb));
    _refreshActualOutput();
    _applyMonitorGain();
  }

  /// Detach the active engine (e.g. its screen closed).
  void detachEngine(GraphAudioPlaybackService engine) {
    if (identical(_engine, engine)) _engine = null;
  }

  /// Select an output device by UID (or null = system default). Applies it live to the active
  /// engine and persists it for the next one.
  Future<void> setOutputDevice(String? uid) async {
    // Mark synchronously with the field write, before any await: the race a
    // late load() would otherwise win is closed on the user's side.
    _userChosen.add(_kOutputUid);
    _outputDeviceUid = uid;
    _outputDeviceName = _byUid(_outputDevices, uid)?.name;
    // Drive the engine toward the resolved device even for a null/empty (System Default)
    // selection: passing '' makes the native side fall back to the default output. Without this
    // the engine would keep the previously-selected device while monitorAllowed is recomputed from
    // the OS default — which could un-mute the hear-yourself monitor into a speaker (feedback).
    // set_output_device blocks for the host reply, so the engine has finished ACTING on the
    // request before monitor safety is re-evaluated below. That is not the same as being on the
    // requested device: while the mic is open the duplex unit owns the output and the host only
    // RECORDS the pick for later, and a device that fails to open falls back to the default. So
    // the re-evaluation below reads what the engine says it is rendering to, not this uid.
    _engine?.setOutputDevice(uid ?? '');
    // Re-evaluate monitor safety for the new output (speaker → force the monitor off).
    _refreshActualOutput();
    _applyMonitorGain();
    notifyListeners();
    await _persist();
  }

  /// Select an input device by UID (or null = system default).
  Future<void> setInputDevice(String? uid) async {
    _userChosen.add(_kInputUid);
    _inputDeviceUid = uid;
    _inputDeviceName = _byUid(_inputDevices, uid)?.name;
    // Also drive the engine on a null/empty selection: '' makes the native side fall back to the
    // default input, so a System Default pick actually switches the mic instead of leaving the
    // engine on the previously-selected device until restart.
    _engine?.setInputDevice(uid ?? '');
    notifyListeners();
    await _persist();
  }

  /// Set the monitor reverb amount (0..1) and apply it live.
  Future<void> setMonitorReverb(double wet) async {
    _userChosen.add(_kReverbWet);
    _monitorReverbWet = wet.clamp(0.0, 1.0);
    _engine?.setMonitorReverb(
      _monitorReverbRoom,
      _monitorReverbDamping,
      _monitorReverbWet,
    );
    notifyListeners();
    await _persist();
  }

  /// Set the record/input gain in dB and apply it live (no persist — call [commitAudioGains] on
  /// the fader's release). Use for continuous fader drags.
  void setRecordGainDbLive(double db) {
    _userChosen.add(_kRecordGainDb);
    _recordGainDb = db.clamp(recordGainMinDb, recordGainMaxDb);
    _engine?.setInputGain(_dbToLinear(_recordGainDb));
    notifyListeners();
  }

  /// Set the monitor (hear-yourself) gain (linear 0..1) and apply it live (no persist). Forced to 0
  /// at the engine on a speaker output (see [monitorAllowed]).
  void setMonitorGainLive(double gain) {
    _userChosen.add(_kMonitorGain);
    _monitorGain = gain.clamp(0.0, 1.0);
    _applyMonitorGain();
    notifyListeners();
  }

  /// Persist the record + monitor gains (call on a fader's release).
  Future<void> commitAudioGains() => _persist();

  /// True when the last settings write did NOT reach disk (a full or
  /// read-only disk, revoked permissions). The panel keeps showing what the
  /// user picked — the engine really is on it — but a restart reads the older
  /// document back, so the UI has to say so instead of quietly lying.
  bool get persistFailed => _persistFailed;
  bool _persistFailed = false;

  /// Writes the whole settings document.
  ///
  /// NEVER throws. Every caller is a UI callback typed `void` (a device row's
  /// onSelect, a fader's onEnd), and Dart happily discards the Future those
  /// callbacks return: a rethrown I/O failure became an unhandled async error
  /// in the root zone — a spurious crash report — while the panel went on
  /// showing a value the disk had refused. The outcome belongs in state that
  /// the UI can render, not in a Future no call site can hold.
  Future<void> _persist() async {
    // The document is all seven fields from memory, and while [load] is parked
    // in its read the fields the user has not touched still hold constructor
    // defaults: writing then put those defaults on disk, and load() never
    // wrote again (a read racing the write could even adopt them). Hold the
    // write back; load() issues it once the document's values are in place.
    if (_loadsInFlight > 0) {
      _persistDeferred = true;
      return;
    }
    try {
      await _storage.write(_collection, _docId, {
        _kOutputUid: _outputDeviceUid,
        _kInputUid: _inputDeviceUid,
        _kOutputName: _outputDeviceName,
        _kInputName: _inputDeviceName,
        _kReverbWet: _monitorReverbWet,
        _kRecordGainDb: _recordGainDb,
        _kMonitorGain: _monitorGain,
      });
      _setPersistFailed(false);
    } catch (e) {
      DiagnosticsLog.instance.log(
        'settings',
        'settings persist failed (${e.runtimeType}); the choice is live but '
            'will not survive a restart',
      );
      _setPersistFailed(true);
    }
  }

  void _setPersistFailed(bool failed) {
    if (_persistFailed == failed) return;
    _persistFailed = failed;
    notifyListeners();
  }
}
