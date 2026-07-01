import 'dart:async';
import 'dart:convert';
import 'dart:ffi' as ffi;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../../domain/models/audio_device.dart';
import '../../domain/models/audio_fx.dart';
import '../ffi/vocara_engine_bindings.dart';
import 'audio_dsp.dart';
import 'playback_engine.dart';

/// Open the native engine framework (embedded + linked via CocoaPods). Top-level so a
/// background decode isolate can load it without capturing `this`.
ffi.DynamicLibrary _openVocaraLibrary() {
  const names = [
    'vocara_engine.framework/vocara_engine',
    'vocara_engine.framework/Versions/A/vocara_engine',
  ];
  for (final n in names) {
    try {
      return ffi.DynamicLibrary.open(n);
    } catch (_) {
      // try next
    }
  }
  return ffi.DynamicLibrary.process();
}

/// Max devices read in one `ve_enum_output_devices` call (generous; real systems have < 16).
const int _kDeviceEnumCap = 32;

/// Read a fixed-size C char array (NUL-terminated, UTF-8) into a Dart string. Device names
/// can be non-ASCII (e.g. a Chinese AirPlay/Continuity device), so decode as UTF-8 — and
/// tolerate a sequence truncated by the fixed buffer.
String _charArrayToString(ffi.Array<ffi.Char> arr, int cap) {
  final bytes = <int>[];
  for (var i = 0; i < cap; i++) {
    final c = arr[i];
    if (c == 0) break;
    bytes.add(c & 0xff);
  }
  return utf8.decode(bytes, allowMalformed: true);
}

/// Enumerate audio devices. Static (no running engine needed) so the global settings panel can
/// list devices even when no song is open — `ve_enum_*_devices` are host queries.
List<AudioDevice> _enumDevices({required bool input}) {
  final bindings = VocaraEngineBindings(_openVocaraLibrary());
  final buf = malloc<VeDeviceInfo>(_kDeviceEnumCap);
  try {
    final total = input
        ? bindings.ve_enum_input_devices(buf, _kDeviceEnumCap)
        : bindings.ve_enum_output_devices(buf, _kDeviceEnumCap);
    final n = total < _kDeviceEnumCap ? total : _kDeviceEnumCap;
    return [
      for (var i = 0; i < n; i++)
        () {
          final info = (buf + i).ref;
          return AudioDevice(
            uid: _charArrayToString(info.uid, 128),
            name: _charArrayToString(info.name, 128),
            rate: info.rate,
            isDefault: info.is_default != 0,
            isInput: input,
            isBluetooth: info.is_bluetooth != 0,
          );
        }(),
    ];
  } catch (_) {
    return const [];
  } finally {
    malloc.free(buf);
  }
}

/// The available output devices (static; no engine required).
List<AudioDevice> enumerateOutputDevices() => _enumDevices(input: false);

/// The available input devices (static; no engine required).
List<AudioDevice> enumerateInputDevices() => _enumDevices(input: true);

/// Decode `path` to device-rate PCM on a background isolate (the CPU-heavy part). Returns the
/// native buffer's address + frame count; the malloc'd buffer lives on the shared process
/// heap, so the main isolate reads/frees it. (0, 0) on failure.
Future<(int, int)> _decodeInIsolate(String path, int rate) {
  return Isolate.run(() {
    final bindings = VocaraEngineBindings(_openVocaraLibrary());
    final cpath = path.toNativeUtf8();
    try {
      final pcm = bindings.ve_decode(cpath.cast(), rate);
      return (pcm.ptr.address, pcm.frames);
    } finally {
      malloc.free(cpath);
    }
  });
}

/// Decodes any engine-supported media file (wav/mp3/m4a/aac/video/…) to mono
/// samples in [-1, 1] for offline analysis, on a background isolate. The engine
/// hands back interleaved-stereo f32 (`frames * 2`); we average L/R to mono and
/// free the native buffer. Returns null when the file can't be decoded or is
/// empty. Stateless — touches no live engine.
Future<PcmAudio?> decodeMediaToMono(String path, {int rate = 44100}) {
  return Isolate.run(() {
    final bindings = VocaraEngineBindings(_openVocaraLibrary());
    final cpath = path.toNativeUtf8();
    final pcm = bindings.ve_decode(cpath.cast(), rate);
    final ptr = pcm.ptr;
    final frames = pcm.frames;
    try {
      if (frames <= 0 || ptr.address == 0) return null;
      final inter = ptr.asTypedList(frames * 2);
      final mono = Float64List(frames);
      for (var i = 0; i < frames; i++) {
        mono[i] = (inter[i * 2] + inter[i * 2 + 1]) * 0.5;
      }
      return PcmAudio(mono, pcm.rate);
    } finally {
      malloc.free(cpath);
      if (ptr.address != 0) bindings.ve_free_decoded(ptr, frames);
    }
  });
}

/// Native Rust real-time audio engine (`vocara_engine.framework`), behind the
/// [PlaybackEngine] interface.
///
/// **P1 Step 3 (multi-track):** [sync] commits one timeline-aligned source per track; a
/// file's decode runs on a background isolate (no UI freeze) and is cached, so the graph
/// only rebuilds when the file set changes. Volume / mute / the A-B toggle are instant
/// per-track gain changes. The playhead is read from the engine's atomic frame counter.
class GraphAudioPlaybackService implements PlaybackEngine {
  GraphAudioPlaybackService() {
    _bindings = VocaraEngineBindings(_openVocaraLibrary());
    _engine = _bindings.ve_create();
    _available = _engine != ffi.nullptr;
    if (_available) {
      _sampleRate = _bindings.ve_sample_rate(_engine);
      _sourceId = _bindings.ve_source_id(_engine);
    }
  }

  late final VocaraEngineBindings _bindings;
  ffi.Pointer<VeEngine> _engine = ffi.nullptr;
  bool _available = false;
  double _sampleRate = 48000.0;
  int _sourceId = 0;

  final _position = StreamController<Duration>.broadcast();
  final _playing = StreamController<bool>.broadcast();
  Timer? _poll;

  bool _isPlaying = false;
  // The committed graph's topology signature (paths + routing + bus set); a change
  // triggers a rebuild, otherwise sync only pushes live volumes/mutes.
  String _committedSig = '';
  final Map<String, int> _trackIndex = {};
  // Each FX bus id's index into the committed graph (for live bus FX/volume/meter).
  final Map<String, int> _busIndex = {};
  // The shared reverb settings, re-applied after every graph rebuild (a rebuild resets nodes).
  ReverbSettings? _reverb;
  // The master bus gain, re-applied after every graph rebuild (a rebuild resets nodes).
  double _masterGain = 1.0;
  // Serializes sync() calls so a rebuild's async decode can't interleave with another sync.
  Future<void> _syncChain = Future.value();

  /// Whether the native engine started (a device was opened). If false, every call no-ops.
  bool get isAvailable => _available;
  double get sampleRate => _sampleRate;

  /// The single-track probe source node id (unused by the multi-track app path).
  int get sourceId => _sourceId;

  /// Switch the active output device by UID. Returns true on success (the engine falls back to
  /// the default output if the requested device can't be opened). No-op if the engine is down.
  bool setOutputDevice(String uid) {
    if (!_available) return false;
    final cuid = uid.toNativeUtf8();
    try {
      return _bindings.ve_set_output_device(_engine, cuid.cast()) == 0;
    } finally {
      malloc.free(cuid);
    }
  }

  /// The active output device's UID ('' if unavailable).
  String currentOutputDevice() {
    if (!_available) return '';
    const cap = 256;
    final buf = malloc<ffi.Char>(cap);
    try {
      _bindings.ve_current_output_device(_engine, buf, cap);
      return buf.cast<Utf8>().toDartString();
    } finally {
      malloc.free(buf);
    }
  }

  /// Poll + clear pending device events (bitmask; bit 1 = output lost, bit 2 = input lost).
  int pollDeviceEvents() =>
      _available ? _bindings.ve_poll_device_event(_engine) : 0;

  /// Select the input device by UID (reopens the input stream live if monitoring). Returns true
  /// on success. No-op if the engine is down.
  bool setInputDevice(String uid) {
    if (!_available) return false;
    final cuid = uid.toNativeUtf8();
    try {
      return _bindings.ve_set_input_device(_engine, cuid.cast()) == 0;
    } finally {
      malloc.free(cuid);
    }
  }

  /// The selected input device's UID ('' if unavailable / system default).
  String currentInputDevice() {
    if (!_available) return '';
    const cap = 256;
    final buf = malloc<ffi.Char>(cap);
    try {
      _bindings.ve_current_input_device(_engine, buf, cap);
      return buf.cast<Utf8>().toDartString();
    } finally {
      malloc.free(buf);
    }
  }

  /// Start live-input monitoring (open the mic stream + ramp the monitor in).
  void startMonitor() {
    if (_available) _bindings.ve_start_monitor(_engine);
  }

  /// Stop live-input monitoring (ramp out + close the mic stream).
  void stopMonitor() {
    if (_available) _bindings.ve_stop_monitor(_engine);
  }

  /// Set the live-input monitor gain (linear, 0 = muted).
  void setMonitorGain(double gain) {
    if (_available) _bindings.ve_set_monitor_gain(_engine, gain);
  }

  /// Set the record/input gain (linear): the mic-input fader's makeup gain applied to the captured
  /// voice before recording/analysis.
  void setInputGain(double gain) {
    if (_available) _bindings.ve_set_input_gain(_engine, gain);
  }

  /// Set the monitor reverb (room / damping / wet, each 0..1).
  void setMonitorReverb(double room, double damping, double wet) {
    if (_available) {
      _bindings.ve_set_monitor_reverb(_engine, room, damping, wet);
    }
  }

  /// The current live-input peak level (0..1) for a meter; 0 when not capturing.
  double inputLevel() => _available ? _bindings.ve_input_level(_engine) : 0.0;

  /// Diagnostic: total input frames the mic callback received (0 = mic delivering nothing).
  int inputFramesReceived() =>
      _available ? _bindings.ve_input_frames_received(_engine) : 0;

  /// Record round-trip latency in milliseconds: how far behind the playhead a captured vocal lands
  /// (output IO + acoustic + input IO + AEC block). Shift recorded takes earlier by this so they
  /// sit on the beat the singer heard. 0 until the mic has opened once.
  double recordLatencyMs() => _available
      ? _bindings.ve_record_latency_frames(_engine) / 48000.0 * 1000.0
      : 0.0;

  // Reusable native buffer for pulling captured frames (interleaved stereo).
  static const _kInputPullFrames = 4096;
  ffi.Pointer<ffi.Float>? _inputPullBuf;

  /// Begin capturing input frames for live analysis (opens the mic stream on the selected device).
  void startCapture() {
    if (_available) _bindings.ve_start_capture(_engine);
  }

  /// Stop capturing input frames (closes the mic stream if nothing else needs it).
  void stopCapture() {
    if (_available) _bindings.ve_stop_capture(_engine);
  }

  /// Pull the latest captured frames (interleaved stereo at 48 kHz) — `count*2` floats.
  Float32List pullInputFrames() {
    if (!_available) return Float32List(0);
    final buf = (_inputPullBuf ??= malloc<ffi.Float>(_kInputPullFrames * 2));
    final n = _bindings.ve_input_frames(_engine, buf, _kInputPullFrames);
    return Float32List.fromList(buf.asTypedList(n * 2));
  }

  /// Record the live input (selected device) to a mono 48 kHz WAV at [path].
  void startInputRecording(String path) {
    if (!_available) return;
    final cp = path.toNativeUtf8();
    try {
      _bindings.ve_start_input_recording(_engine, cp.cast());
    } finally {
      malloc.free(cp);
    }
  }

  /// Stop the input recording (finalizes the WAV).
  void stopInputRecording() {
    if (_available) _bindings.ve_stop_input_recording(_engine);
  }

  @override
  Stream<Duration> get position => _position.stream;
  @override
  Stream<bool> get playing => _playing.stream;
  @override
  bool get isPlaying => _isPlaying;
  @override
  Duration get currentPosition => _available
      ? _framesToDuration(_bindings.ve_frame_pos(_engine))
      : Duration.zero;

  @override
  Future<void> sync(List<TrackPlayback> tracks, List<BusPlayback> buses) {
    final next = _syncChain.then((_) => _doSync(tracks, buses));
    _syncChain = next.catchError((_) {});
    return next;
  }

  /// The graph topology that forces a rebuild: each track's file + output routing,
  /// and each bus's id + output routing. Volumes / mutes / FX are excluded (pushed
  /// live), so dragging a fader never rebuilds the graph.
  static String _orderTag(TrackFx? fx) =>
      fx == null ? '' : fx.order.map((k) => k.index).join(',');

  // Send targets (not levels) drive the graph wiring; level changes are live.
  static String _sendsTag(List<TrackSend> sends) =>
      sends.map((s) => s.busId).join(',');

  String _topologySig(List<TrackPlayback> tracks, List<BusPlayback> buses) {
    final b = StringBuffer();
    for (final t in tracks) {
      b
        ..write(t.path)
        // startMs is baked into the engine source at commit (there is no live FFI setter to
        // move a committed track), so a clip/take move must force a graph rebuild.
        ..write('@')
        ..write(t.startMs)
        ..write('>')
        ..write(t.busId ?? '')
        ..write(':')
        ..write(_orderTag(t.fx))
        ..write(';')
        ..write(_sendsTag(t.sends))
        ..write('|');
    }
    b.write('#');
    for (final x in buses) {
      b
        ..write(x.id)
        ..write('>')
        ..write(x.outputBusId ?? '')
        ..write(':')
        ..write(_orderTag(x.fx))
        ..write(';')
        ..write(_sendsTag(x.sends))
        ..write('|');
    }
    return b.toString();
  }

  /// Stages a track/bus's aux sends (target bus index + level dB) before commit.
  void _pushSends(
    int idx,
    List<TrackSend> sends,
    Map<String, int> busIndex, {
    required bool bus,
  }) {
    if (sends.isEmpty) return;
    final n = sends.length;
    final targets = malloc<ffi.Int32>(n);
    final levels = malloc<ffi.Float>(n);
    try {
      for (var i = 0; i < n; i++) {
        targets[i] = busIndex[sends[i].busId] ?? -1;
        levels[i] = sends[i].levelDb;
      }
      if (bus) {
        _bindings.ve_bus_sends(_engine, idx, targets, levels, n);
      } else {
        _bindings.ve_track_sends(_engine, idx, targets, levels, n);
      }
    } finally {
      malloc.free(targets);
      malloc.free(levels);
    }
  }

  /// Pushes a track/bus insert-chain order to the engine (call after add/begin,
  /// before commit). Empty order → engine default (EQ → Comp).
  void _pushChain(int idx, TrackFx? fx, {required bool bus}) {
    final order = fx?.order ?? const <FxKind>[];
    if (order.isEmpty) return;
    final n = order.length;
    final ptr = malloc<ffi.Uint8>(n);
    try {
      for (var i = 0; i < n; i++) {
        ptr[i] = order[i].index;
      }
      if (bus) {
        _bindings.ve_bus_chain(_engine, idx, ptr, n);
      } else {
        _bindings.ve_track_chain(_engine, idx, ptr, n);
      }
    } finally {
      malloc.free(ptr);
    }
  }

  Future<void> _doSync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
  ) async {
    if (!_available) return;
    final sig = _topologySig(tracks, buses);
    if (sig == _committedSig) {
      // No structural change → push live volumes/mutes for tracks + buses only.
      for (final t in tracks) {
        final idx = _trackIndex[t.id];
        if (idx != null) {
          _bindings.ve_set_track_volume(
            _engine,
            idx,
            t.volume,
            t.muted ? 1 : 0,
          );
        }
      }
      for (var i = 0; i < buses.length; i++) {
        _bindings.ve_set_bus_volume(
          _engine,
          i,
          buses[i].volume,
          buses[i].muted ? 1 : 0,
        );
      }
      return;
    }

    // Topology changed → rebuild the graph. Stage BUSES FIRST so a track can route
    // into one by index. Build the new index maps off to the side so the old ones
    // stay valid during the (async) track decodes.
    final newBusIndex = <String, int>{
      for (var i = 0; i < buses.length; i++) buses[i].id: i,
    };
    _bindings.ve_buses_begin(_engine);
    for (var i = 0; i < buses.length; i++) {
      final b = buses[i];
      final out = b.outputBusId != null
          ? (newBusIndex[b.outputBusId] ?? -1)
          : -1;
      _bindings.ve_buses_add(_engine, b.volume, b.muted ? 1 : 0, out);
      _pushChain(i, b.fx, bus: true);
      _pushSends(i, b.sends, newBusIndex, bus: true);
    }

    _bindings.ve_tracks_begin(_engine);
    final newIndex = <String, int>{};
    for (final t in tracks) {
      final busIdx = t.busId != null ? (newBusIndex[t.busId] ?? -1) : -1;
      final idx = await _stageTrack(t, busIdx);
      if (idx >= 0) {
        newIndex[t.id] = idx;
        _pushSends(idx, t.sends, newBusIndex, bus: false);
      }
      // A track that fails to stage (idx < 0, e.g. a gone/corrupt file) is simply
      // left out of the graph this round; the commit still succeeds for the rest and
      // the skipped track restages on the next topology change (see _committedSig below).
    }
    // A non-zero return means the engine REJECTED this topology (e.g. a routing
    // cycle) and kept the OLD graph. Don't record it as committed or swap the
    // index maps — otherwise an identical re-sync short-circuits on _committedSig
    // and never retries, and the cached indices diverge from the live graph.
    // Leave the prior committed state intact so a corrected topology rebuilds.
    if (_bindings.ve_tracks_commit(_engine) != 0) return;
    _trackIndex
      ..clear()
      ..addAll(newIndex);
    _busIndex
      ..clear()
      ..addAll(newBusIndex);
    // Record the committed signature even when a track was skipped (e.g. a gone or
    // corrupt file). Forcing the empty sentinel here used to guarantee a retry, but
    // because the signature excludes live volume/mute, EVERY fader/mute/FX tweak then
    // missed the fast path and rebuilt the whole graph — re-running the dead clip's
    // isolate decode and re-staging everything on each tick (audible glitch + constant
    // re-decode). Recording the real `sig` keeps unrelated live changes on the fast
    // path; a skipped track is restaged on the next real topology change (a structural
    // edit, backing toggle or reopen — all of which change `sig`), which is also when a
    // transiently-unreadable file would have become readable again.
    _committedSig = sig;

    // The rebuild reset every node to defaults — re-apply persisted effects + levels.
    for (final t in tracks) {
      final idx = newIndex[t.id];
      if (idx == null) continue;
      _bindings.ve_set_pan(_engine, idx, t.pan);
      if (t.fx != null) _applyFxToIndex(idx, t.fx!);
    }
    for (var i = 0; i < buses.length; i++) {
      _applyBusFx(i, buses[i].fx);
      _bindings.ve_set_bus_volume(
        _engine,
        i,
        buses[i].volume,
        buses[i].muted ? 1 : 0,
      );
      _bindings.ve_set_bus_pan(_engine, i, buses[i].pan);
    }
    final r = _reverb;
    if (r != null) _applyReverb(r);
    _bindings.ve_set_master(_engine, _masterGain);
  }

  /// Stage one track for the pending commit, routed to [busIdx] (-1 = master).
  /// Tries the engine's decoded-PCM cache first (instant; the native side is the single
  /// source of truth for what's cached and never decodes on this thread — a miss returns
  /// -1). On a miss (first stage, or evicted at an earlier commit) the file is decoded on
  /// an isolate and staged as raw samples, which re-caches it under [t.path].
  Future<int> _stageTrack(TrackPlayback t, int busIdx) async {
    final muted = t.muted ? 1 : 0;
    final vocal = t.isVocal ? 1 : 0;
    final cachedPath = t.path.toNativeUtf8();
    try {
      final idx = _bindings.ve_tracks_add(
        _engine,
        cachedPath.cast(),
        t.startMs,
        t.volume,
        muted,
        vocal,
        busIdx,
      );
      if (idx >= 0) {
        _pushChain(idx, t.fx, bus: false);
        return idx;
      }
    } finally {
      malloc.free(cachedPath);
    }
    final (addr, frames) = await _decodeInIsolate(t.path, _sampleRate.round());
    if (addr == 0 || frames == 0) return -1;
    final ptr = ffi.Pointer<ffi.Float>.fromAddress(addr);
    final cpath = t.path.toNativeUtf8();
    try {
      final idx = _bindings.ve_tracks_add_pcm(
        _engine,
        cpath.cast(),
        ptr,
        frames,
        t.startMs,
        t.volume,
        muted,
        vocal,
        busIdx,
      );
      if (idx >= 0) _pushChain(idx, t.fx, bus: false);
      return idx;
    } finally {
      malloc.free(cpath);
      _bindings.ve_free_decoded(ptr, frames);
    }
  }

  @override
  Future<void> setVolume(String id, double volume, {bool muted = false}) async {
    if (!_available) return;
    final idx = _trackIndex[id];
    if (idx != null) {
      _bindings.ve_set_track_volume(_engine, idx, volume, muted ? 1 : 0);
    }
  }

  @override
  void setTrackPan(String id, double pan) {
    if (!_available) return;
    final idx = _trackIndex[id];
    if (idx != null) _bindings.ve_set_pan(_engine, idx, pan);
  }

  @override
  Future<void> play() async {
    if (!_available) return;
    _bindings.ve_play(_engine);
    _isPlaying = true;
    _playing.add(true);
    _startPoll();
  }

  @override
  Future<void> pause() async {
    if (!_available) return;
    _bindings.ve_pause(_engine);
    _isPlaying = false;
    _playing.add(false);
    // Stop the 60 Hz position poll: while paused the playhead doesn't advance, so
    // polling just burns FFI calls + GC every 16 ms. play() restarts it (and
    // seek() emits the playhead directly), so this matches seek()'s "poll only runs
    // while playing" contract — which was previously false.
    _poll?.cancel();
    _poll = null;
  }

  @override
  Future<void> seek(Duration position) async {
    if (!_available) return;
    _bindings.ve_seek(_engine, _durationToFrames(position));
    // Emit the new position immediately. The position poll only runs while
    // playing, so without this a seek (e.g. clicking the ruler) before the
    // first playback would move the engine but never update the playhead.
    _position.add(position);
  }

  @override
  void setLoop(bool enabled, int startMs, int endMs) {
    if (!_available) return;
    _bindings.ve_set_loop(
      _engine,
      enabled ? 1 : 0,
      _msToFrames(startMs),
      _msToFrames(endMs),
    );
  }

  /// Decode + play a single file immediately (used by the standalone probe).
  bool loadAndPlayFile(String path) {
    if (!_available) return false;
    final cpath = path.toNativeUtf8();
    try {
      if (_bindings.ve_load_file(_engine, cpath.cast()) != 0) return false;
    } finally {
      malloc.free(cpath);
    }
    _bindings.ve_seek(_engine, 0);
    _bindings.ve_play(_engine);
    _isPlaying = true;
    _playing.add(true);
    _startPoll();
    return true;
  }

  @override
  bool get supportsEffects => true;

  @override
  void setTrackFx(String trackId, TrackFx fx) {
    if (!_available) return;
    final idx = _trackIndex[trackId];
    if (idx != null) _applyFxToIndex(idx, fx);
  }

  @override
  void setBusFx(String busId, TrackFx fx) {
    if (!_available) return;
    final idx = _busIndex[busId];
    if (idx != null) _applyBusFx(idx, fx);
  }

  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {
    if (!_available) return;
    final idx = _busIndex[busId];
    if (idx != null) {
      _bindings.ve_set_bus_volume(_engine, idx, volume, muted ? 1 : 0);
    }
  }

  @override
  void setBusPan(String busId, double pan) {
    if (!_available) return;
    final idx = _busIndex[busId];
    if (idx != null) _bindings.ve_set_bus_pan(_engine, idx, pan);
  }

  @override
  double busGainReductionDb(String busId) {
    if (!_available) return 0;
    final idx = _busIndex[busId];
    return idx != null ? _bindings.ve_bus_gr(_engine, idx) : 0;
  }

  @override
  void setTrackSendLevel(String trackId, int slot, double levelDb) {
    if (!_available) return;
    final idx = _trackIndex[trackId];
    if (idx != null) {
      _bindings.ve_set_send_level(_engine, idx, slot, levelDb);
    }
  }

  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {
    if (!_available) return;
    final idx = _busIndex[busId];
    if (idx != null) {
      _bindings.ve_set_bus_send_level(_engine, idx, slot, levelDb);
    }
  }

  @override
  void setReverb(ReverbSettings reverb) {
    _reverb = reverb;
    if (_available) _applyReverb(reverb);
  }

  @override
  void setMasterVolume(double gain) {
    _masterGain = gain;
    if (_available) _bindings.ve_set_master(_engine, gain);
  }

  void _applyFxToIndex(int index, TrackFx fx) {
    for (var i = 0; i < fx.eq.length; i++) {
      final b = fx.eq[i];
      _bindings.ve_set_eq_band(
        _engine,
        index,
        i,
        b.freq,
        b.q,
        b.gainDb,
        (fx.eqOn && b.enabled) ? 1 : 0,
      );
    }
    final c = fx.comp;
    _bindings.ve_set_comp(
      _engine,
      index,
      c.thresholdDb,
      c.ratio,
      c.kneeDb,
      c.attackMs,
      c.releaseMs,
      c.makeupDb,
      fx.compOn ? 0 : 1, // bypass when the compressor is off
    );
    _bindings.ve_set_send(_engine, index, fx.sendDb);
    final de = fx.deEsser;
    _bindings.ve_set_deesser(
      _engine,
      index,
      de.freq,
      de.thresholdDb,
      de.rangeDb,
      fx.deEsserOn ? 0 : 1,
    );
    final g = fx.gate;
    _bindings.ve_set_gate(
      _engine,
      index,
      g.thresholdDb,
      g.rangeDb,
      g.attackMs,
      g.releaseMs,
      fx.gateOn ? 0 : 1,
    );
    final dl = fx.delay;
    _bindings.ve_set_delay(
      _engine,
      index,
      dl.timeMs,
      dl.feedback,
      dl.mix,
      fx.delayOn ? 0 : 1,
    );
    final sa = fx.saturation;
    _bindings.ve_set_saturation(
      _engine,
      index,
      sa.driveDb,
      sa.mix,
      fx.saturationOn ? 0 : 1,
    );
    final db = fx.doubler;
    _bindings.ve_set_doubler(
      _engine,
      index,
      db.delayMs,
      db.depthMs,
      db.rateHz,
      db.mix,
      fx.doublerOn ? 0 : 1,
    );
    final lm = fx.limiter;
    _bindings.ve_set_limiter(
      _engine,
      index,
      lm.ceilingDb,
      lm.releaseMs,
      fx.limiterOn ? 0 : 1,
    );
    final rv = fx.reverb;
    _bindings.ve_set_reverb_insert(
      _engine,
      index,
      rv.roomSize,
      rv.damping,
      fx.reverbOn ? rv.wet : 0.0,
      rv.width,
    );
  }

  /// Push an FX bus's effect chain to its committed bus index (mirrors
  /// [_applyFxToIndex] but addresses bus nodes).
  void _applyBusFx(int index, TrackFx fx) {
    for (var i = 0; i < fx.eq.length; i++) {
      final b = fx.eq[i];
      _bindings.ve_set_bus_eq_band(
        _engine,
        index,
        i,
        b.freq,
        b.q,
        b.gainDb,
        (fx.eqOn && b.enabled) ? 1 : 0,
      );
    }
    final c = fx.comp;
    _bindings.ve_set_bus_comp(
      _engine,
      index,
      c.thresholdDb,
      c.ratio,
      c.kneeDb,
      c.attackMs,
      c.releaseMs,
      c.makeupDb,
      fx.compOn ? 0 : 1,
    );
    _bindings.ve_set_bus_send(_engine, index, fx.sendDb);
    final de = fx.deEsser;
    _bindings.ve_set_bus_deesser(
      _engine,
      index,
      de.freq,
      de.thresholdDb,
      de.rangeDb,
      fx.deEsserOn ? 0 : 1,
    );
    final g = fx.gate;
    _bindings.ve_set_bus_gate(
      _engine,
      index,
      g.thresholdDb,
      g.rangeDb,
      g.attackMs,
      g.releaseMs,
      fx.gateOn ? 0 : 1,
    );
    final dl = fx.delay;
    _bindings.ve_set_bus_delay(
      _engine,
      index,
      dl.timeMs,
      dl.feedback,
      dl.mix,
      fx.delayOn ? 0 : 1,
    );
    final sa = fx.saturation;
    _bindings.ve_set_bus_saturation(
      _engine,
      index,
      sa.driveDb,
      sa.mix,
      fx.saturationOn ? 0 : 1,
    );
    final db = fx.doubler;
    _bindings.ve_set_bus_doubler(
      _engine,
      index,
      db.delayMs,
      db.depthMs,
      db.rateHz,
      db.mix,
      fx.doublerOn ? 0 : 1,
    );
    final lm = fx.limiter;
    _bindings.ve_set_bus_limiter(
      _engine,
      index,
      lm.ceilingDb,
      lm.releaseMs,
      fx.limiterOn ? 0 : 1,
    );
    final rv = fx.reverb;
    _bindings.ve_set_bus_reverb_insert(
      _engine,
      index,
      rv.roomSize,
      rv.damping,
      fx.reverbOn ? rv.wet : 0.0,
      rv.width,
    );
  }

  void _applyReverb(ReverbSettings r) {
    // dry = 0: the bus is a wet-only send return.
    _bindings.ve_set_reverb(
      _engine,
      r.roomSize,
      r.damping,
      r.wet,
      0.0,
      r.width,
    );
  }

  @override
  double trackGainReductionDb(String trackId) {
    if (!_available) return 0;
    final idx = _trackIndex[trackId];
    return idx != null ? _bindings.ve_track_gr(_engine, idx) : 0;
  }

  // --- Internals -----------------------------------------------------------

  void _startPoll() {
    _poll?.cancel();
    _poll = Timer.periodic(const Duration(milliseconds: 16), (_) {
      if (!_available) return;
      _position.add(_framesToDuration(_bindings.ve_frame_pos(_engine)));
    });
  }

  int _durationToFrames(Duration d) =>
      (d.inMicroseconds * _sampleRate / 1e6).round();
  int _msToFrames(int ms) => (ms * _sampleRate / 1000).round();
  Duration _framesToDuration(int frames) =>
      Duration(microseconds: (frames / _sampleRate * 1e6).round());

  @override
  Future<void> dispose() async {
    _poll?.cancel();
    if (_available) {
      _bindings.ve_destroy(_engine);
      _engine = ffi.nullptr;
      _available = false;
    }
    if (_inputPullBuf != null) {
      malloc.free(_inputPullBuf!);
      _inputPullBuf = null;
    }
    _trackIndex.clear();
    _busIndex.clear();
    _committedSig = '';
    await _position.close();
    await _playing.close();
  }
}
