import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../domain/models/audio_device.dart';
import 'graph_audio_playback_service.dart';
import 'local_storage_service.dart';

/// Global, persisted app settings — the audio output + input devices. A [ChangeNotifier] so the
/// settings panel rebuilds on change. It brokers between the persisted choice and the
/// currently-active engine: enumeration is static (works with no song open), while applying a
/// choice is forwarded to the attached engine and persisted for the next engine.
class AppSettingsService extends ChangeNotifier {
  AppSettingsService({required LocalStorageService storage})
    : _storage = storage;

  final LocalStorageService _storage;

  // One JSON document under the app-support `settings` collection.
  static const _collection = 'settings';
  static const _docId = 'audio';
  static const _kOutputUid = 'outputDeviceUid';
  static const _kInputUid = 'inputDeviceUid';
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
  double _monitorReverbWet = 0.2;
  double _recordGainDb = 24.0; // mic-input makeup (≈ the engine's default)
  double _monitorGain =
      0.0; // hear-yourself level (linear); 0 = off (e.g. on a speaker)
  List<AudioDevice> _outputDevices = const [];
  List<AudioDevice> _inputDevices = const [];
  GraphAudioPlaybackService? _engine;

  /// The persisted output device UID, or null = system default.
  String? get outputDeviceUid => _outputDeviceUid;

  /// The persisted input device UID, or null = system default.
  String? get inputDeviceUid => _inputDeviceUid;

  /// The currently-enumerated output devices.
  List<AudioDevice> get outputDevices => _outputDevices;

  /// The currently-enumerated input devices.
  List<AudioDevice> get inputDevices => _inputDevices;

  /// The selected input device (uid + name), or null when following the system default. Used to
  /// point recording / live analysis at the same device as the engine (not the OS default).
  AudioDevice? get selectedInputDevice {
    final uid = _inputDeviceUid;
    if (uid == null || uid.isEmpty) return null;
    for (final d in _inputDevices) {
      if (d.uid == uid) return d;
    }
    return AudioDevice(uid: uid, name: '');
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

  // The effective device (an explicit pick, else the system default) from a list.
  AudioDevice? _effective(List<AudioDevice> list, String? uid) {
    if (uid != null && uid.isNotEmpty) {
      for (final d in list) {
        if (d.uid == uid) return d;
      }
      return null;
    }
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

  /// Whether the audible monitor (hear-yourself) is allowed: only when the effective output is
  /// HEADPHONES (no acoustic path back to the mic). On a built-in/external SPEAKER the mic
  /// re-captures the monitored voice → howling feedback, so the monitor is locked off there.
  bool get monitorAllowed {
    final out = _effective(_outputDevices, _outputDeviceUid);
    if (out == null) return false; // unknown → safe default: off
    if (out.isBluetooth) return true; // Bluetooth ≈ headphones
    final n = out.name.toLowerCase();
    return n.contains('headphone') ||
        n.contains('earphone') ||
        n.contains('airpod');
  }

  // The monitor gain actually sent to the engine: the fader value on headphones, forced to 0 on a
  // speaker so a persisted/raised fader can never feed back after switching output.
  void _applyMonitorGain() =>
      _engine?.setMonitorGain(monitorAllowed ? _monitorGain : 0.0);

  /// Load persisted settings and enumerate devices. Safe to call at startup.
  Future<void> load() async {
    Map<String, dynamic>? doc;
    try {
      doc = await _storage.read(_collection, _docId);
    } catch (_) {
      doc = null; // never let a read failure abort startup; fall back to defaults
    }
    _outputDeviceUid = doc?[_kOutputUid] as String?;
    _inputDeviceUid = doc?[_kInputUid] as String?;
    // Clamp every loaded value to the same range the live setters enforce — a
    // tampered / out-of-range stored value must not reach the engine verbatim
    // (e.g. a huge recordGainDb → enormous input gain, monitorGain > 1 → howling
    // feedback) and must keep the mixer fader math valid.
    _monitorReverbWet =
        ((doc?[_kReverbWet] as num?)?.toDouble() ?? 0.2).clamp(0.0, 1.0);
    _recordGainDb = ((doc?[_kRecordGainDb] as num?)?.toDouble() ?? 24.0)
        .clamp(recordGainMinDb, recordGainMaxDb);
    _monitorGain =
        ((doc?[_kMonitorGain] as num?)?.toDouble() ?? 0.0).clamp(0.0, 1.0);
    // If a screen attached the engine before this load() resolved, it adopted
    // constructor defaults — re-push the now-loaded values to it.
    _applyAllToEngine();
    refreshDevices();
  }

  /// Re-query the available output + input devices (static; no engine required). Only notifies
  /// when a set actually changed, so it's safe to poll on a timer for hot-plug detection.
  void refreshDevices() {
    final outs = enumerateOutputDevices();
    final ins = enumerateInputDevices();
    final changed =
        _deviceSig(outs) != _deviceSig(_outputDevices) ||
        _deviceSig(ins) != _deviceSig(_inputDevices);
    if (changed) {
      _outputDevices = outs;
      _inputDevices = ins;
      // monitorAllowed is derived from the output set, so reconcile the engine's
      // monitor gain whenever it changes — the initial attach can run before the
      // (microtask-deferred) device list is populated and force the gain to 0;
      // without this re-apply the MON fader reads up but the engine stays muted.
      _applyMonitorGain();
      notifyListeners();
    }
  }

  // A change signature over a device set (the fields the panel renders) to skip no-op rebuilds.
  static String _deviceSig(List<AudioDevice> ds) =>
      ds.map((d) => '${d.uid}:${d.isDefault}:${d.rate}:${d.name}').join('|');

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
    _applyMonitorGain();
  }

  /// Detach the active engine (e.g. its screen closed).
  void detachEngine(GraphAudioPlaybackService engine) {
    if (identical(_engine, engine)) _engine = null;
  }

  /// Select an output device by UID (or null = system default). Applies it live to the active
  /// engine and persists it for the next one.
  Future<void> setOutputDevice(String? uid) async {
    _outputDeviceUid = uid;
    // Drive the engine toward the resolved device even for a null/empty (System Default)
    // selection: passing '' makes the native side fall back to the default output. Without this
    // the engine would keep the previously-selected device while monitorAllowed is recomputed from
    // the OS default — which could un-mute the hear-yourself monitor into a speaker (feedback).
    // set_output_device blocks for the host reply, so the engine is on the resulting device before
    // we re-evaluate monitor safety below.
    _engine?.setOutputDevice(uid ?? '');
    // Re-evaluate monitor safety for the new output (speaker → force the monitor off).
    _applyMonitorGain();
    notifyListeners();
    await _persist();
  }

  /// Select an input device by UID (or null = system default).
  Future<void> setInputDevice(String? uid) async {
    _inputDeviceUid = uid;
    // Also drive the engine on a null/empty selection: '' makes the native side fall back to the
    // default input, so a System Default pick actually switches the mic instead of leaving the
    // engine on the previously-selected device until restart.
    _engine?.setInputDevice(uid ?? '');
    notifyListeners();
    await _persist();
  }

  /// Set the monitor reverb amount (0..1) and apply it live.
  Future<void> setMonitorReverb(double wet) async {
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
    _recordGainDb = db.clamp(recordGainMinDb, recordGainMaxDb);
    _engine?.setInputGain(_dbToLinear(_recordGainDb));
    notifyListeners();
  }

  /// Set the monitor (hear-yourself) gain (linear 0..1) and apply it live (no persist). Forced to 0
  /// at the engine on a speaker output (see [monitorAllowed]).
  void setMonitorGainLive(double gain) {
    _monitorGain = gain.clamp(0.0, 1.0);
    _applyMonitorGain();
    notifyListeners();
  }

  /// Persist the record + monitor gains (call on a fader's release).
  Future<void> commitAudioGains() => _persist();

  Future<void> _persist() => _storage.write(_collection, _docId, {
    _kOutputUid: _outputDeviceUid,
    _kInputUid: _inputDeviceUid,
    _kReverbWet: _monitorReverbWet,
    _kRecordGainDb: _recordGainDb,
    _kMonitorGain: _monitorGain,
  });
}
