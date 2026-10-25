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
  /// [enumerateOutputs] / [enumerateInputs] / [inputChannelCount] are injectable for tests; they
  /// default to the live CoreAudio queries.
  AppSettingsService({
    required LocalStorageService storage,
    List<AudioDevice> Function()? enumerateOutputs,
    List<AudioDevice> Function()? enumerateInputs,
    int Function(String uid)? inputChannelCount,
  }) : _storage = storage,
       _enumerateOutputs = enumerateOutputs ?? enumerateOutputDevices,
       _enumerateInputs = enumerateInputs ?? enumerateInputDevices,
       _channelsOf = inputChannelCount ?? inputDeviceChannelCount;

  final LocalStorageService _storage;
  final List<AudioDevice> Function() _enumerateOutputs;
  final List<AudioDevice> Function() _enumerateInputs;
  final int Function(String uid) _channelsOf;

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
  static const _kInputChannels = 'inputChannelByUid';
  static const _kHeadphoneOutputs = 'headphoneOutputUids';

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

  /// The input-channel choice (a [GraphAudioPlaybackService] `inputChannel*` mode) per uid of the
  /// device capturing (see [_inputKey]). A device with no entry is on Auto. Per device because it
  /// describes how THAT device is wired: input 2 of one interface is not input 2 of the next.
  final Map<String, int> _inputChannelByUid = {};

  /// The uids whose choice the user made this session (see [_userChosen]): [load] adopts the
  /// stored choice of every other device.
  final Set<String> _inputChannelsChosen = {};

  int _inputChannelCount = 0;

  /// The outputs the user has said only headphones play from (see [setOutputIsHeadphones]), by
  /// uid: what no name can tell, as with wired headphones on the jack or an audio interface.
  final Set<String> _headphoneOutputs = {};

  /// The uids whose mark the user set or cleared this session: [load] adopts only the others'.
  final Set<String> _headphoneMarksChosen = {};

  /// The persisted keys the user has already chosen this session. [load] is
  /// fire-and-forget (the provider graph starts it and drops the future), so
  /// it can resolve at ANY moment — including after the singer has moved a
  /// fader. A field the user set is never adopted from the document:
  /// otherwise the change is reverted on screen, re-pushed to the engine, and
  /// then written back as the old value by the next [_persist], which
  /// serializes every field at once. Keyed by the same `_kXxx` constants
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

  /// The device the mic captures from, as the key of its input-channel choice: the chosen one
  /// while it is connected, else the system default — what the engine records from when the
  /// selection is System Default, or when the chosen device is unplugged. Keyed by the selection
  /// instead, a choice made for one interface followed the default onto the next.
  String get _inputKey => _keyIn(_inputDevices);

  // [_inputKey] against [devices].
  String _keyIn(List<AudioDevice> devices) {
    final uid = _inputDeviceUid;
    if (uid != null && uid.isNotEmpty && !_isMissing(devices, uid)) {
      return uid;
    }
    final fallback = _defaultIn(devices);
    return fallback.isNotEmpty ? fallback : uid ?? '';
  }

  // The choice for the device that [key] names.
  int _channelFor(String key) =>
      _inputChannelByUid[key] ?? GraphAudioPlaybackService.inputChannelAuto;

  /// The input-channel choice of the device capturing NOW, read off a fresh enumeration of the
  /// inputs without adopting it: the list is polled only while Settings is open, and the device
  /// poll owns adopting it (with its switches back to a returned device).
  int _channelCapturingNow() => _channelFor(_keyIn(_enumerateInputs()));

  /// Before a take: point the attached engine at the input-channel choice of the device capturing
  /// now (an interface plugged in, or a default that moved, since the screen opened). Only that —
  /// a full [refreshDevices] there also switched a returned device back synchronously, re-opening
  /// the mic or blocking on the output host right as the take armed.
  ///
  /// With the mic already open (monitoring), the device capturing is the one the stream is bound
  /// to: it retries the chosen device only when it opens, so a mic open on the fallback stays
  /// there after the chosen one returns — and keyed on the returned one, the fallback recorded
  /// under its choice.
  void syncInputChannel() {
    final engine = _engine;
    if (engine == null) return;
    final open = GraphAudioPlaybackService.inputStateRunning(
      engine.inputStreamState(),
    );
    final String key;
    if (!open) {
      key = _keyIn(_enumerateInputs());
    } else {
      final bound = engine.actualInputDevice();
      key = bound.isNotEmpty ? bound : _defaultIn(_enumerateInputs());
    }
    engine.setInputChannel(_channelFor(key));
  }

  // The system default input in [devices] ('' when none is marked).
  static String _defaultIn(List<AudioDevice> devices) {
    for (final d in devices) {
      if (d.isDefault) return d.uid;
    }
    return '';
  }

  /// Which input(s) of the current input device the engine records and monitors: a
  /// [GraphAudioPlaybackService] `inputChannel*` mode, Auto unless chosen for this device.
  int get inputChannel => _channelFor(_inputKey);

  /// The name of the device the input-channel choice applies to: the one capturing (see
  /// [_inputKey]), which is the system default while the chosen device is unplugged — the panel
  /// names it, or a choice made under the offline row landed on another interface unseen.
  String get inputChannelDeviceName =>
      _byUid(_inputDevices, _inputKey)?.name ?? '';

  /// The current input device's channel count (0 = unknown) — the panel offers the input-channel
  /// choice only from 2. Read from the device itself, so it is known with no song open and no mic
  /// running; refreshed with the device list.
  int get inputChannelCount => _inputChannelCount;

  // Re-read the current input's channel count; true when it changed. Never notifies.
  bool _refreshInputChannelCount() {
    final count = _channelsOf(_inputKey);
    if (count == _inputChannelCount) return false;
    _inputChannelCount = count;
    return true;
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
  bool get monitorAllowed {
    final d = _renderingDevice;
    return _markedHeadphones(d) || _isHeadphoneLike(d);
  }

  /// The device the engine reports rendering to when it is surely headphones, so the mic hears
  /// none of the backing; '' otherwise. The engine's echo canceller then passes the mic through
  /// (only while that device still renders) instead of adapting to a reference the mic never
  /// hears, which pulled an in-key voice under. Stricter than [monitorAllowed], twice over:
  ///  * only the device the engine REPORTS counts — never the selection, and never a device the
  ///    lists do not know;
  ///  * only a PRODUCT named as earbuds counts, not any Bluetooth device and not a headphone
  ///    PORT ("External Headphones" is the jack, whatever is plugged in) — or an output the user
  ///    marked as headphones-only ([setOutputIsHeadphones]). A speaker taken for headphones here
  ///    would leave its whole echo in every take.
  String get echoFreeOutput {
    final d = _knownRendering;
    return d != null && (_markedHeadphones(d) || _isSurelyHeadphones(d))
        ? d.uid
        : '';
  }

  /// The output a headphones mark would apply to: the device the engine renders to, else the
  /// selection. Null when neither is known.
  AudioDevice? get outputToMark => _renderingDevice;

  /// Whether [outputToMark] can be marked as headphones: see [_canCarryMark]; and a device its
  /// name already tells as earbuds needs no mark.
  bool get outputMarkable {
    final d = _renderingDevice;
    return d != null && _canCarryMark(d) && !_isSurelyHeadphones(d);
  }

  /// Whether the user has marked [outputToMark] as headphones.
  bool get outputMarkedHeadphones => _markedHeadphones(_renderingDevice);

  /// A mark counts only where the panel can show it, so one nobody could see to clear never
  /// stays in force: a device since renamed as a speaker, or newly on [_speakerNames].
  bool _markedHeadphones(AudioDevice? d) =>
      d != null && _headphoneOutputs.contains(d.uid) && _canCarryMark(d);

  /// Whether [d] can carry a headphones mark at all. Decided by its TRANSPORT (see
  /// [AudioTransport.canCarryHeadphonesMark]): its name is translated by the system, so a name
  /// rule let a German Mac's built-in speakers ("MacBook Pro-Lautsprecher") or its one device for
  /// speakers and jack ("Integrierte Ausgabe") be marked — and after the headphones came out,
  /// every take recorded the whole backing and the monitor fed the speakers. A device named as a
  /// speaker is refused on top.
  static bool _canCarryMark(AudioDevice d) =>
      d.transport.canCarryHeadphonesMark && !_namedSpeaker(d);

  static bool _namedSpeaker(AudioDevice d) {
    final n = d.name.toLowerCase();
    return _speakerNames.any(n.contains);
  }

  /// The device to judge feedback safety on: what the engine is actually rendering to, else the
  /// effective selection. Null (judged unsafe) while the device list cannot be read at all: the
  /// selection is then no stand-in for what renders — on System Default it is the device the
  /// default has just LEFT, and the engine's own mute does not cover a default that moves.
  AudioDevice? get _renderingDevice {
    if (_actualOutputUid.isNotEmpty) {
      final d = _knownRendering;
      if (d != null) return d;
      if (_unlistedUnreadable) return null;
    }
    return _effective(_outputDevices, _outputDeviceUid);
  }

  /// The device the engine reports rendering to, from the lists or, when they have never seen it
  /// (a speaker turned on since this page enumerated, which the system default then moved to),
  /// from the fresh read [_refreshActualOutput] took. Null when neither knows it.
  AudioDevice? get _knownRendering {
    final uid = _actualOutputUid;
    if (uid.isEmpty) return null;
    final listed = _byUid(_outputDevices, uid);
    if (listed != null) return listed;
    final unlisted = _unlistedOutput;
    return unlisted != null && unlisted.uid == uid ? unlisted : null;
  }

  /// The rendering device when the lists do not know it, read afresh WITHOUT adopting that read:
  /// adopting it would be [refreshDevices], whose switches (a persisted device back, the input
  /// channel) on the 1 s monitor tick reopened the mic in the middle of a take.
  AudioDevice? _unlistedOutput;

  /// The fresh read for an unlisted rendering device came back empty: the list could not be read
  /// (the engine renders to SOMETHING), so nothing is known about that device.
  bool _unlistedUnreadable = false;

  /// Last uid the engine reported rendering to. CACHED, not read per access: [monitorAllowed] is
  /// read from widget builds (the MON fader's enabled state), and resolving it natively walks the
  /// device list. Refreshed wherever the routing can have moved — which is also why the ENGINE
  /// owns the hard mute rather than trusting this: a stale value here is bounded by one tick,
  /// and the engine's own reroute check has no such window.
  String _actualOutputUid = '';

  static bool _isSurelyHeadphones(AudioDevice? out) {
    if (out == null || _namedSpeaker(out)) return false;
    final n = out.name.toLowerCase();
    return n.contains('airpod') ||
        n.contains('earpod') ||
        n.contains('earbud') ||
        n.contains('buds');
  }

  static bool _isHeadphoneLike(AudioDevice? out) {
    // Unknown → the safe default: off.
    if (out == null || _namedSpeaker(out)) return false;
    final n = out.name.toLowerCase();
    // Only a name that says so. Not "any Bluetooth device": Bluetooth is speakers as often as
    // headphones, a speaker missing from [_speakerNames] fed the monitor straight back into the
    // mic (it is kept out of the AEC reference, so nothing cancels it), and on macOS the
    // transport was never even read until the headphones mark needed it. Anything else is the
    // user's to mark ([setOutputIsHeadphones]).
    return _isSurelyHeadphones(out) ||
        n.contains('headphone') ||
        n.contains('earphone');
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
    _applyOutputRoute();
    // The MON fader's enabled state is derived from this, so a change has to reach the panel.
    if (changed) notifyListeners();
  }

  // Re-read the engine's rendering device into [_actualOutputUid]; true when what it is judged
  // as changed: the device moved, or a fresh read of one the lists do not know came out
  // differently from the last (found it, or could read the list again). Never notifies —
  // callers that already run outside a build own that.
  bool _refreshActualOutput() {
    final actual = _engine?.actualOutputDevice() ?? '';
    final known =
        actual.isEmpty ||
        _byUid(_outputDevices, actual) != null ||
        _unlistedOutput?.uid == actual;
    final unlisted = _unlistedOutput;
    var unreadable = false;
    if (!known) {
      final fresh = _enumerateOutputs();
      _unlistedOutput = _byUid(fresh, actual);
      unreadable = fresh.isEmpty;
    }
    final rejudged =
        !identical(unlisted, _unlistedOutput) ||
        unreadable != _unlistedUnreadable;
    _unlistedUnreadable = unreadable;
    if (actual == _actualOutputUid) return rejudged;
    _actualOutputUid = actual;
    return true;
  }

  // The monitor gain actually sent to the engine: the fader value on headphones, forced to 0 on a
  // speaker so a persisted/raised fader can never feed back after switching output.
  void _applyMonitorGain() =>
      _engine?.setMonitorGain(monitorAllowed ? _monitorGain : 0.0);

  // Everything the engine derives from the rendering device: the monitor's feedback safety and
  // the echo canceller's route ([echoFreeOutput]).
  void _applyOutputRoute() {
    _applyMonitorGain();
    // Named only on a change (and on every attach): the engine re-checks it against what renders
    // at each of its own polls, so repeating it each tick adds nothing. Nor is it withdrawn while
    // the engine reports nothing rendering (the mic opening): the engine applies it only while
    // that very device renders, so keeping it is safe, and withdrawing it left the canceller
    // adapting on the earbuds once the unit bound, until the next tick named them again.
    final held = _echoFreePushed;
    if (_actualOutputUid.isEmpty && held != null && _stillEchoFree(held)) {
      return;
    }
    final echoFree = echoFreeOutput;
    if (echoFree != _echoFreePushed) {
      _echoFreePushed = echoFree;
      _engine?.setEchoFree(echoFree);
    }
  }

  // The echo-free output last named to the attached engine; null = not named yet.
  String? _echoFreePushed;

  /// Whether the output [uid] last named echo-free would still be judged so if it rendered: a
  /// name held through a mic open must not outlive the mark it came from.
  bool _stillEchoFree(String uid) {
    if (uid.isEmpty) return true;
    final d =
        _byUid(_outputDevices, uid) ??
        (_unlistedOutput?.uid == uid ? _unlistedOutput : null);
    return d != null && (_markedHeadphones(d) || _isSurelyHeadphones(d));
  }

  static String? _loadedString(Object? v) => v is String ? v : null;

  /// The stored per-device input-channel choices, keeping only well-formed entries (a known mode
  /// under a string uid): one corrupt entry costs that device's choice, not the others'.
  static Map<String, int> _loadedChannels(Object? v) {
    if (v is! Map) return const {};
    return {
      for (final MapEntry(:key, :value) in v.entries)
        if (key is String &&
            value is int &&
            value >= GraphAudioPlaybackService.inputChannelAuto &&
            value <= GraphAudioPlaybackService.inputChannel2)
          key: value,
    };
  }

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
    _loadedChannels(doc?[_kInputChannels]).forEach((uid, mode) {
      if (!_inputChannelsChosen.contains(uid)) _inputChannelByUid[uid] = mode;
    });
    final marks = doc?[_kHeadphoneOutputs];
    if (marks is List) {
      for (final uid in marks.whereType<String>()) {
        if (!_headphoneMarksChosen.contains(uid)) _headphoneOutputs.add(uid);
      }
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
    if (!changed) {
      // Polled with the lists: a device whose input format changed can change whether the
      // panel offers the input-channel choice without any device set changing.
      if (_refreshInputChannelCount()) notifyListeners();
      return;
    }
    final capturingBefore = _inputKey;
    // A persisted device coming back after an unplug: the engine fell back to
    // the system default meanwhile, so switch it back to the user's choice live.
    final outReturned = _returned(_outputDevices, outs, _outputDeviceUid);
    final inReturned = _returned(_inputDevices, ins, _inputDeviceUid);
    _outputDevices = outs;
    _inputDevices = ins;
    if (outReturned) _engine?.setOutputDevice(_outputDeviceUid!);
    if (inReturned) _engine?.setInputDevice(_inputDeviceUid!);
    // The mic moved under the same selection (the system default changed, or the chosen
    // device left or came back): it runs on ITS input-channel choice.
    if (_inputKey != capturingBefore) _engine?.setInputChannel(inputChannel);
    _refreshInputChannelCount();
    _syncLastKnownNames();
    // monitorAllowed is derived from the output set, so reconcile the engine's
    // monitor gain whenever it changes — the initial attach can run before the
    // (microtask-deferred) device list is populated and force the gain to 0;
    // without this re-apply the MON fader reads up but the engine stays muted.
    _refreshActualOutput();
    _applyOutputRoute();
    notifyListeners();
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
    _echoFreePushed = null;
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
    engine.setInputChannel(inputChannel);
    _refreshActualOutput();
    _applyOutputRoute();
  }

  /// Detach the active engine (e.g. its screen closed).
  void detachEngine(GraphAudioPlaybackService engine) {
    if (!identical(_engine, engine)) return;
    _engine = null;
    // What that engine rendered to is not what anything renders to now. Kept, it had Settings
    // (opened from Home, after the song page let go) offer or hide the headphones mark for the
    // last song's output instead of the user's choice. No notify: this runs from a disposing
    // page, and whatever reads these next builds afresh.
    _actualOutputUid = '';
    _unlistedOutput = null;
    _unlistedUnreadable = false;
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
    _applyOutputRoute();
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
    // The choice is per device: the new input runs on ITS choice (Auto unless made).
    _engine?.setInputChannel(inputChannel);
    _refreshInputChannelCount();
    notifyListeners();
    await _persist();
  }

  /// Points [engine], one this service does not track (the technique page owns its own), at the
  /// user's input choice: the device, which of its inputs to record, and the record gain. A new
  /// engine starts on Auto and the default gain, so without them a singer who picked Input 1 to
  /// keep a guitar on input 2 out got it blended into those takes, at a level they had not set.
  ///
  /// The channel choice is the device capturing NOW's (see [_channelCapturingNow]): read off the
  /// polled list, an interface plugged in since launch read as missing, and its takes ran on the
  /// built-in mic's choice.
  void applyInputTo(GraphAudioPlaybackService engine) {
    final uid = _inputDeviceUid;
    if (uid != null && uid.isNotEmpty) engine.setInputDevice(uid);
    engine.setInputChannel(_channelCapturingNow());
    engine.setInputGain(_dbToLinear(_recordGainDb));
  }

  /// Choose which input(s) of the current input device the engine records and monitors (a
  /// [GraphAudioPlaybackService] `inputChannel*` mode). Applies live and persists for this device.
  Future<void> setInputChannel(int mode) async {
    final key = _inputKey;
    _inputChannelsChosen.add(key);
    _inputChannelByUid[key] = mode.clamp(
      GraphAudioPlaybackService.inputChannelAuto,
      GraphAudioPlaybackService.inputChannel2,
    );
    _engine?.setInputChannel(inputChannel);
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

  /// Mark the output [uid] (the [outputToMark] the panel showed — not whatever renders by the
  /// time the tap lands) as one only headphones play from, or clear the mark. Takes on a marked
  /// output skip echo cancelling, which pulled held in-key notes under on an echo the mic never
  /// heard, and the monitor is allowed on it. A speaker playing from a marked output would leave
  /// its whole echo in every take, so the mark is the user's alone to make.
  Future<void> setOutputIsHeadphones(String uid, bool on) async {
    if (uid.isEmpty) return;
    _headphoneMarksChosen.add(uid);
    if (on) {
      _headphoneOutputs.add(uid);
    } else {
      _headphoneOutputs.remove(uid);
    }
    _applyOutputRoute();
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
    // The document is every field from memory, and while [load] is parked
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
        _kInputChannels: Map.of(_inputChannelByUid),
        _kHeadphoneOutputs: _headphoneOutputs.toList(),
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
