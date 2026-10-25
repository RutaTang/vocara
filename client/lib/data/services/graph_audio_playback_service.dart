import 'dart:async';
import 'dart:convert';
import 'dart:ffi' as ffi;
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart' show visibleForTesting;

import '../../domain/models/audio_device.dart';
import '../../domain/models/audio_fx.dart';
import '../ffi/vocara_engine_bindings.dart';
import 'audio_dsp.dart';
import 'diagnostics_log.dart';
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

/// Serializes graph syncs: two rebuilds must never interleave over one engine,
/// because a rebuild's file decodes are async and the second would stage into
/// the index maps the first is still swapping.
///
/// It also has to accept a job ENQUEUED FROM INSIDE a running job — that is how
/// a degraded commit queues its one rebuild. That works because [add] captures
/// and replaces the tail SYNCHRONOUSLY, before the job it wraps ever runs: an
/// inner `add` therefore chains onto the outer job's own future and lands
/// after it, instead of awaiting a link that is waiting on the caller.
class SyncChain {
  Future<void> _tail = Future<void>.value();

  /// Runs [job] once everything already queued has settled. The returned
  /// future carries [job]'s own outcome; the chain absorbs failures so one
  /// failed sync never blocks the ones behind it.
  Future<void> add(Future<void> Function() job) {
    final next = _tail.then((_) => job());
    _tail = next.catchError((_) {});
    return next;
  }
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

/// The input channel count of the device with [uid] ('' = the system default input); 0 when the
/// device is absent or unreadable. Static (no engine required), like the enumeration: the settings
/// panel offers the input-channel choice only for a device with 2+ inputs.
int inputDeviceChannelCount(String uid) {
  try {
    final bindings = VocaraEngineBindings(_openVocaraLibrary());
    final cuid = uid.toNativeUtf8();
    try {
      return bindings.ve_input_device_channels(cuid.cast());
    } finally {
      malloc.free(cuid);
    }
  } catch (_) {
    return 0;
  }
}

/// Hard duration cap for the PLAYBACK/import decode path. Committed tracks live in memory
/// as full-length engine-rate stereo f32 (~1.4 GB/hour resident, with a ~2x transient while
/// decoding), so an arbitrary-length import is an OOM, not a feature. The cap is enforced
/// natively BEFORE the track allocates (see `ve_decode`'s `max_frames`), and is deliberately
/// far above any song-practice use so only runaway inputs (a DJ set, a podcast rip) hit it.
const Duration maxPlaybackDuration = Duration(minutes: 60);

/// ABSOLUTE source-frame memory budgets handed to `ve_decode` — the bound the
/// native allocation actually obeys (duration and rate ceilings alone still
/// multiply to gigabytes inside both limits; one stereo frame is 8 bytes in
/// the decode buffer).
/// Playback staging: 60 minutes at a 96 kHz source ≈ 346 M frames ≈ 2.8 GB —
/// whole-track residency is the playback design's documented cost, and this
/// bounds what a single import can demand of it (a 60-minute 192 kHz file
/// would have doubled it).
const int maxPlaybackSrcFrames = 346000000;

/// Analysis: ~33 M frames ≈ 264 MB — a 10-minute take at up to ~55 kHz
/// source, shorter at higher rates; the analysis targets vocal takes
/// recorded at 48 kHz, so this refuses only exotic hi-rate media.
const int maxAnalysisSrcFrames = 33000000;

/// Decode `path` to device-rate PCM on a background isolate (the CPU-heavy part). Returns the
/// native buffer's address + frame count; the malloc'd buffer lives on the shared process
/// heap, so the main isolate reads/frees it. (0, 0) on failure. Throws
/// [MediaTooLongException] when the file exceeds [maxPlaybackDuration] — refused natively
/// before the full track allocates.
Future<(int, int)> _decodeInIsolate(String path, int rate) {
  return Isolate.run(() {
    final bindings = VocaraEngineBindings(_openVocaraLibrary());
    final cpath = path.toNativeUtf8();
    final status = malloc<ffi.Int>();
    try {
      final maxFrames = maxPlaybackDuration.inMilliseconds * rate ~/ 1000;
      final pcm = bindings.ve_decode(
        cpath.cast(),
        rate,
        maxFrames,
        maxPlaybackSrcFrames,
        status,
      );
      final refused = decodeStatusToException(
        status.value,
        maxPlaybackDuration,
      );
      if (refused != null) throw refused;
      return (pcm.ptr.address, pcm.frames);
    } finally {
      malloc.free(cpath);
      malloc.free(status);
    }
  });
}

/// `ve_decode` statuses (see the C header's VE_DECODE_* constants). Mirrored
/// here because ffigen only binds functions/structs. The taxonomy matters for
/// diagnosis: every refusal used to surface as "too long", so a short 384 kHz
/// file and a failing disk both read as an over-duration file.
const int veDecodeTooLong = -2;
const int veDecodeTooBig = -3;
const int veDecodeRateUnsupported = -4;
const int veDecodeIo = -5;

/// Thrown when media is refused by a MEMORY budget or the supported-rate
/// envelope rather than the duration cap — a short but extremely dense file
/// (e.g. 384 kHz), which "too long" would misdiagnose. The message is
/// user-presentable.
class MediaUnsupportedException implements Exception {
  const MediaUnsupportedException(this.message);

  final String message;

  @override
  String toString() => message;
}

/// Maps a refusing `ve_decode` status to its typed exception; returns null
/// for OK and for the generic failure (callers treat that as "not decodable"
/// without a special message).
Exception? decodeStatusToException(int status, Duration durationCap) {
  switch (status) {
    case veDecodeTooLong:
      return MediaTooLongException(durationCap);
    case veDecodeTooBig:
      return const MediaUnsupportedException(
        'This file is too dense to process. Sample rates this high are not '
        'supported.',
      );
    case veDecodeRateUnsupported:
      return const MediaUnsupportedException(
        'This file uses an unsupported sample rate.',
      );
    case veDecodeIo:
      return const MediaUnsupportedException(
        'The file could not be read. Check the disk and try again.',
      );
    default:
      return null;
  }
}

/// The DECLARED duration of a media file, probed from the container without
/// decoding a single packet — the import gate's cheap pre-check. Null when the
/// container declares no length (the decode-time budget still bounds it later)
/// or the file can't be probed (the import will fail on its own terms).
/// Stateless FFI; cheap enough for the UI thread (header read only).
Duration? probeMediaDuration(String path) {
  final bindings = VocaraEngineBindings(_openVocaraLibrary());
  final cpath = path.toNativeUtf8();
  try {
    final ms = bindings.ve_probe_duration_ms(cpath.cast());
    return ms >= 0 ? Duration(milliseconds: ms) : null;
  } finally {
    malloc.free(cpath);
  }
}

/// Thrown when media is longer than the decode path's duration cap
/// ([maxPlaybackDuration] for playback/import, the caller's `maxDuration` for
/// analysis). The message is user-presentable.
class MediaTooLongException implements Exception {
  const MediaTooLongException(this.maxDuration);

  /// The cap the media exceeded.
  final Duration maxDuration;

  @override
  String toString() =>
      'This file is too long. '
      'The limit is ${maxDuration.inMinutes} minutes.';
}

/// Whether [frames] mono frames at [rate] Hz exceed the [maxDuration] cap.
/// Extracted from the decode isolate for testability (the decode itself needs
/// the native engine).
@visibleForTesting
bool exceedsDecodeCap(int frames, int rate, Duration maxDuration) =>
    frames > maxDuration.inMilliseconds * rate ~/ 1000;

/// Decodes any engine-supported media file (wav/mp3/m4a/aac/video/…) to mono
/// samples in [-1, 1] for offline analysis, on a background isolate. The engine
/// hands back interleaved-stereo f32 (`frames * 2`); we average L/R to mono and
/// free the native buffer. Returns null when the file can't be decoded or is
/// empty. Stateless — touches no live engine.
///
/// [maxDuration] bounds the decoded length for ANALYSIS callers: the whole
/// file materializes as a Float32List here and is copied AGAIN when the
/// result crosses back to the calling isolate (and typically a third time
/// into the analysis isolate), so an arbitrary-length input (a technique
/// reference can be any media) would blow memory — an hour of mono at
/// 44.1 kHz is ~635 MB per copy. The cap is enforced NATIVELY, before the
/// full-track f32 buffer allocates (`ve_decode`'s `max_frames` refuses a
/// declared-too-long container up front and bails mid-decode otherwise), and
/// exceeding it throws [MediaTooLongException]. Null (the default) falls back
/// to [maxPlaybackDuration] — nothing decodes truly unbounded anymore.
Future<PcmAudio?> decodeMediaToMono(
  String path, {
  int rate = 44100,
  Duration? maxDuration,
}) {
  return Isolate.run(() {
    final bindings = VocaraEngineBindings(_openVocaraLibrary());
    final cpath = path.toNativeUtf8();
    final status = malloc<ffi.Int>();
    final cap = maxDuration ?? maxPlaybackDuration;
    final maxFrames = cap.inMilliseconds * rate ~/ 1000;
    // The MONO entry point: the downmix happens inside the native buffer, so
    // an interleaved stereo track and its mono reduction are never alive at
    // the same time (which cost 12 bytes per frame at peak, on top of
    // everything the analysis itself holds).
    final pcm = bindings.ve_decode_mono(
      cpath.cast(),
      rate,
      maxFrames,
      maxAnalysisSrcFrames,
      status,
    );
    final refused = decodeStatusToException(status.value, cap);
    malloc.free(status);
    final ptr = pcm.ptr;
    final frames = pcm.frames;
    try {
      if (refused != null) throw refused;
      if (frames <= 0 || ptr.address == 0) return null;
      // Belt-and-braces re-check against the ACTUAL decode rate the engine
      // reports, so the cap stays a true duration bound even if the native
      // budget (computed at the REQUESTED rate) and the file's rate disagree.
      if (exceedsDecodeCap(frames, pcm.rate, cap)) {
        throw MediaTooLongException(cap);
      }
      return PcmAudio(Float32List.fromList(ptr.asTypedList(frames)), pcm.rate);
    } finally {
      malloc.free(cpath);
      if (ptr.address != 0) bindings.ve_free_decoded_mono(ptr, frames);
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
  GraphAudioPlaybackService() : _decode = _decodeInIsolate {
    _bindings = VocaraEngineBindings(_openVocaraLibrary());
    _engine = _bindings.ve_create();
    _available = _engine != ffi.nullptr;
    if (_available) {
      _sampleRate = _bindings.ve_sample_rate(_engine);
    }
  }

  /// Test-only: drive the sync pipeline against injected [bindings] and an
  /// injected staging [decode], so its skip/degraded/retry policies are
  /// exercisable without the native engine or real media files. The engine
  /// pointer is a dummy the fake bindings must never dereference.
  @visibleForTesting
  GraphAudioPlaybackService.withBindings(
    VocaraEngineBindings bindings, {
    Future<(int, int)> Function(String path, int rate)? decode,
  }) : _decode = decode ?? _decodeInIsolate {
    _bindings = bindings;
    _engine = ffi.Pointer<VeEngine>.fromAddress(0x1);
    _available = true;
    _sampleRate = _bindings.ve_sample_rate(_engine);
  }

  late final VocaraEngineBindings _bindings;

  /// The staging decode ([_decodeInIsolate] in production; injectable in tests).
  final Future<(int, int)> Function(String path, int rate) _decode;
  ffi.Pointer<VeEngine> _engine = ffi.nullptr;
  bool _available = false;
  double _sampleRate = 48000.0;

  final _position = StreamController<Duration>.broadcast();
  final _playing = StreamController<bool>.broadcast();
  Timer? _poll;

  bool _isPlaying = false;
  // The committed graph's topology signature (paths + routing + bus set); a change
  // triggers a rebuild, otherwise sync only pushes live volumes/mutes.
  String _committedSig = '';

  /// Population count over all 64 bits of [mask]. The degraded mask crosses
  /// the FFI as a Uint64, so bit 63 lands in Dart's sign bit: the shift must
  /// be logical (`>>>`) — an arithmetic `>>` keeps a negative value negative
  /// forever and the loop would never terminate.
  @visibleForTesting
  static int bitCount(int mask) {
    var n = 0;
    for (var m = mask; m != 0; m >>>= 1) {
      n += m & 1;
    }
    return n;
  }

  final Map<String, int> _trackIndex = {};
  // Each committed track's chain owner: its own index, or for a clip that plays
  // through an earlier clip's chain (see [TrackPlayback.laneId]) that leader's.
  // Pan, FX and sends address the chain whichever clip they are sent for.
  final Map<String, int> _chainOwner = {};
  // What each chain last received per setter since the last full push
  // ([_pushLiveParams]). The mixer sends a lane's value to every one of its
  // clips, all onto one chain: a copy per clip filled the engine's command ring
  // on a many-clip lane, and each FX drag tick then reached the audio a few
  // blocks late, holding every other live edit up behind it. Deduplicated per
  // chain rather than by skipping followers, so the lane is still reached when
  // its leader clip has left the model (a delete still saving) but not yet the
  // graph that plays.
  final Map<Object, Object> _chainSent = {};
  // Each FX bus id's index into the committed graph (for live bus FX/volume/meter).
  final Map<String, int> _busIndex = {};
  // Each live setter's newest value, stamped with [_liveSeq]. A sync job pushes
  // the model it was handed, which is older than any edit made while the job
  // waited on the chain or on an isolate decode: the edit was heard at once on
  // the old graph, then the job's push put the old value back. An A/B backing
  // flip during the page-open decode stayed wrong for the session, since
  // nothing saves or resyncs after it. Each job re-applies the edits newer than
  // its snapshot right after its own push ([_replayLiveEdits]).
  int _liveSeq = 0;
  final Map<Object, ({int seq, void Function() apply})> _liveEdits = {};
  // The [_liveSeq] the model in [_latestTracks] / [_latestBuses] carries.
  int _latestAsOf = 0;
  // The shared reverb settings, re-applied after every graph rebuild (a rebuild resets nodes).
  ReverbSettings? _reverb;
  // The master bus gain, re-applied after every graph rebuild (a rebuild resets nodes).
  double _masterGain = 1.0;
  // Serializes sync() calls so a rebuild's async decode can't interleave with another sync.
  final SyncChain _syncChain = SyncChain();

  // The model most recently handed to [sync] — what the degraded retry
  // rebuilds from. A sync enqueued while the degraded job runs lands on the
  // chain BEFORE the retry, so the retry must read the model when IT runs;
  // replaying the degraded job's own captured lists would re-commit a
  // topology that newer sync already replaced.
  List<TrackPlayback> _latestTracks = const [];
  List<BusPlayback> _latestBuses = const [];

  /// Whether the native engine started (a device was opened). If false, every call no-ops.
  bool get isAvailable => _available;
  double get sampleRate => _sampleRate;

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

  /// Reads a NUL-terminated UID the engine wrote into a caller-owned buffer.
  ///
  /// Decodes with `allowMalformed` for the same reason [_charArrayToString]
  /// does: the engine truncates to the buffer at a BYTE boundary, and a device
  /// uid is not guaranteed ASCII (an aggregate device carries its user-chosen
  /// name). A cut through a multi-byte sequence would otherwise throw a
  /// FormatException — out of a one-second device-watch timer, where it lands
  /// as an unhandled async error rather than a wrong label.
  String _readUid(
    int Function(ffi.Pointer<VeEngine>, ffi.Pointer<ffi.Char>, int) read,
  ) {
    if (!_available) return '';
    const cap = 256;
    final buf = malloc<ffi.Char>(cap);
    try {
      read(_engine, buf, cap);
      final bytes = <int>[];
      for (var i = 0; i < cap; i++) {
        final c = buf[i];
        if (c == 0) break;
        bytes.add(c & 0xff);
      }
      return utf8.decode(bytes, allowMalformed: true);
    } finally {
      malloc.free(buf);
    }
  }

  /// The active output device's UID ('' if unavailable).
  String currentOutputDevice() => _readUid(_bindings.ve_current_output_device);

  /// The UID the engine is ACTUALLY rendering to ('' = nothing open). Differs
  /// from [currentOutputDevice], which reports what was REQUESTED: they
  /// diverge when the requested device could not be opened, and for as long as
  /// the duplex mic unit owns the output, where playback follows the
  /// microphone's endpoint or the system default and the app-selected device
  /// is not consulted at all. Feedback safety must be judged from this one.
  String actualOutputDevice() => _readUid(_bindings.ve_actual_output_device);

  /// Device-event bits for [pollDeviceEvents] masks.
  static const int eventOutputLost = 1;
  static const int eventInputLost = 2;
  static const int eventAecBypassed = 4;
  static const int eventInputFellBack = 8;
  static const int eventRecordLimit = 16;
  static const int eventOutputRerouted = 32;

  /// Poll + clear the pending device events selected by [mask] (bit 1 =
  /// output lost, bit 2 = input lost, bit 4 = mic opened without echo
  /// cancellation, bit 8 = selected input unavailable — fell back to the
  /// system default, bit 16 = the take reached the longest recording a WAV
  /// header can describe, so capture past it is refused and the owner must
  /// finalize, bit 32 = playback is not on the selected output device and the
  /// engine has muted the monitor for as long as that holds). Only the claimed
  /// bits are cleared; bits outside the mask stay pending for their own
  /// consumer, so the song page's 1 s watch and the technique page's 50 ms
  /// level tick no longer steal each other's events.
  int pollDeviceEvents(int mask) =>
      _available ? _bindings.ve_poll_device_event(_engine, mask) : 0;

  /// Total streaming-source underruns (frames a disk-streamed track rendered
  /// silent because its ring was empty). 0 is the healthy steady state; the
  /// device watch logs growth — the field instrument for feeder health on
  /// real hardware.
  int streamXruns() => _available ? _bindings.ve_stream_xruns(_engine) : 0;

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
  String currentInputDevice() => _readUid(_bindings.ve_current_input_device);

  /// The UID the OPEN input stream is ACTUALLY bound to ('' = input closed,
  /// or bound to the system default). Selection is fire-and-forget inside
  /// the engine, so this — not [currentInputDevice] — is what the take
  /// really records from.
  String actualInputDevice() => _readUid(_bindings.ve_actual_input_device);

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

  /// Tell the echo canceller whether playback is on a route with no acoustic path back to the mic
  /// (headphones): it then passes the mic through untouched (see [AppSettingsService.echoFree]).
  void setEchoFree(bool on) {
    if (_available) _bindings.ve_set_echo_free(_engine, on ? 1 : 0);
  }

  /// Set the record/input gain (linear): the mic-input fader's makeup gain applied to the captured
  /// voice before recording/analysis.
  void setInputGain(double gain) {
    if (_available) _bindings.ve_set_input_gain(_engine, gain);
  }

  /// [setInputChannel] modes (`ve_set_input_channel`): Auto, the input carrying the mic (a mic on
  /// either input of a 2-input interface records at full level), or one input alone.
  static const int inputChannelAuto = 0;
  static const int inputChannel1 = 1;
  static const int inputChannel2 = 2;

  /// Choose which input(s) of a multi-channel mic become the mono signal the monitor, the echo
  /// canceller, the meter and the recording all hear. Input 2 on a device without one is input 1.
  void setInputChannel(int mode) {
    if (_available) _bindings.ve_set_input_channel(_engine, mode);
  }

  /// Set the monitor reverb (room / damping / wet, each 0..1).
  void setMonitorReverb(double room, double damping, double wet) {
    if (_available) {
      _bindings.ve_set_monitor_reverb(_engine, room, damping, wet);
    }
  }

  /// The current live-input peak level (0..1) for a meter; 0 when not capturing.
  double inputLevel() => _available ? _bindings.ve_input_level(_engine) : 0.0;

  /// Diagnostic + pre-roll clock: total ENGINE-RATE (48 kHz) frames the mic
  /// callback has emitted (0 = mic delivering nothing). Engine-rate so callers
  /// can divide by 48000 for time regardless of the device's native rate.
  int inputFramesReceived() =>
      _available ? _bindings.ve_input_frames_received(_engine) : 0;

  /// The host-thread-authoritative input-stream state word:
  /// `(generation << 1) | running`. Running is set only after a successful
  /// stream start and cleared BEFORE any teardown joins the old callback; the
  /// generation bumps on every start. The mic ready handshake snapshots it,
  /// verifies [inputFramesReceived] advanced, then snapshots it AGAIN — an
  /// unchanged word with bit 0 set proves the SAME stream delivered those
  /// frames (a teardown clears the bit, a restart changes the generation), so
  /// a dying stream's final callback can't fake a new stream coming up.
  int inputStreamState() =>
      _available ? _bindings.ve_input_stream_state(_engine) : 0;

  /// Bit 0 of [inputStreamState]: whether an input stream is currently up.
  static bool inputStateRunning(int state) => state & 1 == 1;

  /// The newest published input-intent sequence. Read right after arming a
  /// recording, it is the arm's watermark for [inputAppliedSeq].
  int inputIntentSeq() =>
      _available ? _bindings.ve_input_intent_seq(_engine) : 0;

  /// The host's last SUCCESSFUL convergence, packed as
  /// `(intent_seq << 24) | input_generation`. Published only when the desired
  /// state was actually reached (a failed stream open leaves it untouched),
  /// and paired with the generation so the ready check can demand that
  /// progress, the running bit, and the applied watermark all come from the
  /// SAME stream instance — see [appliedSeqOf]/[appliedGenOf].
  int inputAppliedState() =>
      _available ? _bindings.ve_input_applied_state(_engine) : 0;

  /// The intent-sequence half of [inputAppliedState].
  static int appliedSeqOf(int applied) => applied >> 24;

  /// The generation half of [inputAppliedState] (24 bits, matching the
  /// generation carried in [inputStreamState]'s high bits).
  static int appliedGenOf(int applied) => applied & 0xFFFFFF;

  /// The generation half of [inputStreamState], truncated to the same 24 bits
  /// the applied pairing carries.
  static int stateGenOf(int state) => (state >> 1) & 0xFFFFFF;

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
    final buf = _inputPullBuf ??= malloc<ffi.Float>(_kInputPullFrames * 2);
    final n = _bindings.ve_input_frames(_engine, buf, _kInputPullFrames);
    return Float32List.fromList(buf.asTypedList(n * 2));
  }

  /// Record the live input (selected device) to a mono 48 kHz WAV at [path].
  /// Returns false when the engine could not create the file (disk full /
  /// permissions), in which case nothing will be recorded.
  bool startInputRecording(String path) {
    if (!_available) return false;
    final cp = path.toNativeUtf8();
    try {
      return _bindings.ve_start_input_recording(_engine, cp.cast()) == 0;
    } finally {
      malloc.free(cp);
    }
  }

  /// Stop the input recording (finalizes the WAV). Returns false when an IO
  /// error occurred during the recording, meaning the WAV on disk is missing,
  /// damaged, or incomplete.
  bool stopInputRecording() {
    if (!_available) return false;
    return _bindings.ve_stop_input_recording(_engine) == 0;
  }

  /// Frames the recording ring dropped during the current/latest take (reset
  /// at each start). Nonzero means the WAV on disk is missing that many frames
  /// — the writer thread stalled past the ring's headroom — so the take has
  /// silent holes and must not be presented as gapless.
  int recordDroppedFrames() =>
      _available ? _bindings.ve_record_dropped_frames(_engine) : 0;

  /// Frames written into the recording ring for the CURRENT take (engine-rate,
  /// reset at each start) — the per-take recorded-progress ack. Nonzero PROVES
  /// audio is in this take's ring: a callback that entered before the arm read
  /// the disarmed flag and never pushed, so unlike [inputFramesReceived] this
  /// can never count frames that are not in the WAV. The mic ready handshake
  /// waits for it to leave zero; read at play time it IS the take's file
  /// offset (the pre-roll).
  int recordWrittenFrames() =>
      _available ? _bindings.ve_record_written_frames(_engine) : 0;

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
    _latestTracks = tracks;
    _latestBuses = buses;
    // Every live edit so far is in this model (the mixer updates its model
    // before it calls a setter); later ones are replayed over it.
    final asOf = _latestAsOf = _liveSeq;
    return _syncChain.add(() => _doSync(tracks, buses, asOf));
  }

  /// Applies a live edit now and records it under [key] (one entry per
  /// parameter), for the sync jobs whose snapshot predates it.
  void _live(Object key, void Function() apply) {
    // Re-inserted, so the log replays in the order the edits were made: two
    // clips of a lane share a chain, and the newer value must land last.
    _liveEdits.remove(key);
    _liveEdits[key] = (seq: ++_liveSeq, apply: apply);
    apply();
  }

  /// Re-applies the live edits newer than a job's snapshot [asOf], after the
  /// job pushed that snapshot. The older ones are in it and in every later
  /// job's (jobs run in the order they were queued), so they are dropped.
  void _replayLiveEdits(int asOf) {
    _liveEdits.removeWhere((_, e) => e.seq <= asOf);
    for (final e in _liveEdits.values) {
      e.apply();
    }
  }

  /// Queues the single rebuild a degraded commit needs.
  ///
  /// Called from inside the running [_doSync], so it must APPEND to the sync
  /// chain and never await: [sync] already replaced `_syncChain`'s tail with
  /// the in-flight link before `_doSync` started, so this lands after it —
  /// and after any sync enqueued while that job ran. The retry therefore
  /// reads [_latestTracks]/[_latestBuses] at RUN time instead of taking the
  /// degraded job's lists: when a newer model has already committed, the
  /// retry fast-paths on its signature instead of resurrecting the stale
  /// topology over it. A failing retry is swallowed like any other chain
  /// failure — the withheld signature means the next real sync rebuilds
  /// anyway. Disposal makes the queued run a no-op (`_available` is false
  /// by then).
  void _scheduleDegradedRebuild() {
    unawaited(
      _syncChain
          .add(
            () =>
                _doSync(_latestTracks, _latestBuses, _latestAsOf, retry: true),
          )
          .catchError((_) {}),
    );
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
        // The lane a clip shares a chain with is wiring too.
        ..write('~')
        ..write(t.laneId ?? '')
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

  /// [retry] marks the one rebuild a degraded commit queues (see
  /// [_scheduleDegradedRebuild]): it never queues another, so a file that
  /// stays unreadable settles instead of rebuilding the graph (and re-running
  /// its isolate decodes) on every sync for the rest of the session. A
  /// parameter rather than shared state: a flag set by the degraded commit
  /// and cleared only on a later successful one stayed set when a rejected
  /// commit returned early in between, and the NEXT degraded commit then
  /// never got its retry.
  /// [asOf] is the [_liveSeq] the model carries (see [_replayLiveEdits]).
  Future<void> _doSync(
    List<TrackPlayback> tracks,
    List<BusPlayback> buses,
    int asOf, {
    bool retry = false,
  }) async {
    if (!_available) return;
    final sig = _topologySig(tracks, buses);
    if (sig == _committedSig) {
      // No structural change → re-push every live parameter. Pushing only volume/mute here was
      // a silent divergence: the signature deliberately excludes pan, FX settings and send
      // levels (a knob tick must not rebuild the graph), and each of those has its own live
      // setter that the mixer calls on every tick — so this branch is the ONLY thing that
      // carries them when the model changed by some other route. A failed save reverting the
      // aggregate, and a remote sync applying someone else's edit, both land exactly here: the
      // UI showed the new value while the engine kept playing the old one until the next
      // structural edit or reopen.
      _pushLiveParams(tracks, buses);
      _replayLiveEdits(asOf);
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
    final newOwner = <String, int>{};
    // Each lane's first clip that staged: its chain is the one every later clip
    // of the lane plays through (see [TrackPlayback.laneId]).
    final laneLeader = <String, int>{};
    for (final t in tracks) {
      final busIdx = t.busId != null ? (newBusIndex[t.busId] ?? -1) : -1;
      // A track that fails to stage (idx < 0, e.g. a gone/corrupt file) is simply
      // left out of the graph this round; the commit still succeeds for the rest and
      // the skipped track restages on the next topology change (see _committedSig below).
      // The decode refusals (unreadable file, unsupported rate, over-cap media)
      // surface as typed exceptions from the isolate and mean exactly "skip this
      // track"; anything else is a programming error and must escape.
      int idx;
      try {
        idx = await _stageTrack(t, busIdx);
      } on MediaUnsupportedException catch (e) {
        DiagnosticsLog.instance.log('engine', 'skip unstageable track: $e');
        idx = -1;
      } on MediaTooLongException catch (e) {
        DiagnosticsLog.instance.log('engine', 'skip unstageable track: $e');
        idx = -1;
      }
      // Disposed (the page closed) while this track decoded: the rest of the
      // rebuild is for an engine that no longer exists. Without this every
      // remaining track was still decoded to PCM — seconds of CPU and up to
      // gigabytes per hour of audio — only to be refused by a null engine.
      if (!_available) return;
      if (idx >= 0) {
        newIndex[t.id] = idx;
        newOwner[t.id] = idx;
        _pushSends(idx, t.sends, newBusIndex, bus: false);
        final lane = t.laneId;
        if (lane != null) {
          final leader = laneLeader[lane];
          if (leader == null) {
            laneLeader[lane] = idx;
          } else if (_bindings.ve_track_lane(_engine, idx, leader) == 0) {
            newOwner[t.id] = leader;
          }
        }
      }
    }
    // A non-zero return means the engine REJECTED this topology (e.g. a routing
    // cycle) and kept the OLD graph. Don't record it as committed or swap the
    // index maps — otherwise an identical re-sync short-circuits on _committedSig
    // and never retries, and the cached indices diverge from the live graph.
    // Leave the prior committed state intact so a corrected topology rebuilds.
    final committed = _bindings.ve_tracks_commit_staged(_engine);
    // Published whatever happens from here on: a commit the engine staged and nothing
    // published (a throw below, or an engine-side failure after the stage that still
    // returned non-zero) left the old graph playing while every setter addressed the
    // staged one — mute, faders and FX went dead until the next topology change. A
    // publish with nothing staged (a rejected topology) is a no-op.
    try {
      if (committed != 0) return;
      _trackIndex
        ..clear()
        ..addAll(newIndex);
      _chainOwner
        ..clear()
        ..addAll(newOwner);
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
      //
      // A DEGRADED track is different from a skipped one and must not be treated
      // the same way. Staging probes a streamable WAV and then hands the engine
      // an empty buffer, trusting the commit to reopen the file; when that
      // second open fails the track is IN the graph, holds an index, has gain
      // and meters — and renders silence forever, while the commit returns
      // success and the track contributes no underruns. Recording the signature
      // over that would make it permanent for the session, so the signature is
      // withheld and the rebuild is QUEUED HERE. Withholding alone was not
      // enough: the only caller syncs on structural mix edits, and nothing in
      // play / seek / a fader move ever calls it, so a "retry on the next sync"
      // could wait for the rest of the session while the take sat silent.
      // The retry re-probes on a fresh open: if the file came back
      // non-canonical it is decoded and staged as PCM instead (it plays), and if
      // it is truly gone the stage MISSES and the track is left out — neither of
      // which can degrade again, so one retry is enough and there is no rebuild
      // loop ([retry] bounds it either way).
      final degraded = _bindings.ve_commit_degraded_mask(_engine);
      if (degraded != 0 && !retry) {
        DiagnosticsLog.instance.log(
          'engine',
          'commit degraded ${bitCount(degraded)} streaming track(s) to silence; '
              'rebuilding once from a fresh probe',
        );
        _committedSig = '';
        // Appended to the sync chain rather than awaited: this call IS the
        // chain's current link, so awaiting its successor here would deadlock.
        _scheduleDegradedRebuild();
      } else {
        _committedSig = sig;
      }

      // The rebuild reset every node to defaults — re-apply persisted effects + levels. The
      // new graph is still staged, so these land on it before it renders anything (its first
      // block already plays the mix as set, instead of the build defaults ramping into it),
      // and only then does it go live.
      _pushLiveParams(tracks, buses);
      _replayLiveEdits(asOf);
    } finally {
      _bindings.ve_tracks_publish(_engine);
    }
  }

  /// Pushes every parameter the engine can change WITHOUT rebuilding the graph: track and bus
  /// volume, mute, pan, insert-FX settings, aux send levels, the shared reverb and the master
  /// gain. Idempotent, and deliberately the single definition of "what a live parameter is" —
  /// both the post-commit re-apply (a rebuild resets nodes to defaults) and the
  /// no-topology-change fast path call it, so the two can never drift apart again.
  ///
  /// Indices come from the committed maps, so a track that failed to stage is simply skipped.
  void _pushLiveParams(List<TrackPlayback> tracks, List<BusPlayback> buses) {
    _chainSent.clear();
    for (final t in tracks) {
      final idx = _trackIndex[t.id];
      if (idx == null) continue;
      _bindings.ve_set_track_volume(_engine, idx, t.volume, t.muted ? 1 : 0);
      // The rest is the lane chain's, set through its leader (always in the
      // model a graph was built from).
      if (_chainOwner[t.id] != idx) continue;
      _bindings.ve_set_pan(_engine, idx, t.pan);
      final fx = t.fx;
      if (fx != null) _applyFxToIndex(idx, fx);
      for (var s = 0; s < t.sends.length; s++) {
        _bindings.ve_set_send_level(_engine, idx, s, t.sends[s].levelDb);
      }
    }
    for (final b in buses) {
      final idx = _busIndex[b.id];
      if (idx == null) continue;
      _bindings.ve_set_bus_volume(_engine, idx, b.volume, b.muted ? 1 : 0);
      _bindings.ve_set_bus_pan(_engine, idx, b.pan);
      _applyBusFx(idx, b.fx);
      for (var s = 0; s < b.sends.length; s++) {
        _bindings.ve_set_bus_send_level(_engine, idx, s, b.sends[s].levelDb);
      }
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
    final (addr, frames) = await _decode(t.path, _sampleRate.round());
    if (addr == 0 || frames == 0) return -1;
    final ptr = ffi.Pointer<ffi.Float>.fromAddress(addr);
    if (!_available) {
      // Disposed during the decode: nothing will take the buffer.
      _bindings.ve_free_decoded(ptr, frames);
      return -1;
    }
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
    _live(('volume', id), () {
      final idx = _trackIndex[id];
      if (idx != null) {
        _bindings.ve_set_track_volume(_engine, idx, volume, muted ? 1 : 0);
      }
    });
  }

  /// [trackId]'s committed index, when [value] is new for the chain it plays
  /// through under [kind] (see [_chainSent]); null otherwise.
  int? _chainIndex(String trackId, Object kind, Object value) {
    final idx = _trackIndex[trackId];
    final owner = _chainOwner[trackId];
    if (idx == null || owner == null) return null;
    final key = (kind, owner);
    // Identity for FX (the mixer hands every clip of a lane the same object),
    // equality for levels.
    final sent = _chainSent[key];
    if (sent != null && (identical(sent, value) || sent == value)) return null;
    _chainSent[key] = value;
    return idx;
  }

  @override
  void setTrackPan(String id, double pan) {
    if (!_available) return;
    _live(('pan', id), () {
      final idx = _chainIndex(id, 'pan', pan);
      if (idx != null) _bindings.ve_set_pan(_engine, idx, pan);
    });
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

  @override
  bool get supportsEffects => true;

  @override
  void setTrackFx(String trackId, TrackFx fx) {
    if (!_available) return;
    _live(('fx', trackId), () {
      final idx = _chainIndex(trackId, 'fx', fx);
      if (idx != null) _applyFxToIndex(idx, fx);
    });
  }

  @override
  void setBusFx(String busId, TrackFx fx) {
    if (!_available) return;
    _live(('busFx', busId), () {
      final idx = _busIndex[busId];
      if (idx != null) _applyBusFx(idx, fx);
    });
  }

  @override
  void setBusVolume(String busId, double volume, {bool muted = false}) {
    if (!_available) return;
    _live(('busVolume', busId), () {
      final idx = _busIndex[busId];
      if (idx != null) {
        _bindings.ve_set_bus_volume(_engine, idx, volume, muted ? 1 : 0);
      }
    });
  }

  @override
  void setBusPan(String busId, double pan) {
    if (!_available) return;
    _live(('busPan', busId), () {
      final idx = _busIndex[busId];
      if (idx != null) _bindings.ve_set_bus_pan(_engine, idx, pan);
    });
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
    _live(('send', trackId, slot), () {
      final idx = _chainIndex(trackId, ('send', slot), levelDb);
      if (idx != null) {
        _bindings.ve_set_send_level(_engine, idx, slot, levelDb);
      }
    });
  }

  @override
  void setBusSendLevel(String busId, int slot, double levelDb) {
    if (!_available) return;
    _live(('busSend', busId, slot), () {
      final idx = _busIndex[busId];
      if (idx != null) {
        _bindings.ve_set_bus_send_level(_engine, idx, slot, levelDb);
      }
    });
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
    _chainOwner.clear();
    _chainSent.clear();
    _busIndex.clear();
    _liveEdits.clear();
    _committedSig = '';
    await _position.close();
    await _playing.close();
  }
}
