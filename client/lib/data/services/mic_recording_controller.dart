import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'diagnostics_log.dart';
import 'graph_audio_playback_service.dart';
import 'mic_permission.dart';
import 'monitor_service.dart';
import 'recording_finalizer.dart';

/// Why a [MicRecordingController.start] attempt failed. The owning view model
/// maps these to its own user-facing messages — toasts/notices stay in the VM.
enum MicRecordingFailure {
  /// Microphone permission was denied (the user must enable it in System
  /// Settings › Privacy & Security › Microphone).
  permissionDenied,

  /// The audio engine has no usable audio device, so nothing can record.
  engineUnavailable,

  /// The engine could not create the WAV file (disk full / permissions);
  /// nothing would be recorded.
  fileCreateFailed,

  /// The recording armed but the microphone never started delivering frames
  /// within the ready timeout (the native input-stream open is asynchronous
  /// and can fail after `startInputRecording` returns success). The take was
  /// discarded — recording ahead against a silent mic would bake a hole into
  /// the front of the take.
  inputNotDelivering,
}

/// Result of a [MicRecordingController.start] attempt. Exactly one shape:
///
///  * started — [path] is non-null; the WAV is being written there.
///  * failed — [failure] is non-null; nothing was armed and any created file
///    was discarded.
///  * invalidated — both null: the start was abandoned mid-await (the owner
///    invalidated it, or the controller was disposed). Nothing was armed, any
///    created file was discarded; callers treat it as a silent no-op.
class MicRecordingStart {
  const MicRecordingStart.started(String this.path) : failure = null;

  const MicRecordingStart.failed(MicRecordingFailure this.failure)
    : path = null;

  const MicRecordingStart.invalidated() : path = null, failure = null;

  /// The WAV path being recorded into, when the recording armed.
  final String? path;

  /// Why setup failed, when it did.
  final MicRecordingFailure? failure;

  /// True when the recording armed and is now in flight.
  bool get started => path != null;
}

/// What [MicRecordingController.stop] hands back: where the take was written
/// and whether the writer finalized it cleanly. `clean == false` means the
/// writer hit an IO error (create/write/finalize) during the take, so the WAV
/// on disk is missing, damaged, or incomplete — the owner decides whether the
/// partial file is worth keeping. `droppedFrames > 0` means the recording ring
/// overflowed (writer thread stalled) and the WAV is missing that many 48 kHz
/// frames as silent holes — the file finalized fine, but it is not gapless,
/// so the owner should tell the user instead of presenting it as clean.
typedef MicRecordingStop = ({String path, bool clean, int droppedFrames});

/// Best-effort delete of the temp dir holding a temp-strategy recording at
/// [path] (see [RecordingPathStrategy.tempDir]: one dir per take, so removing
/// the parent directory removes the take). Null path is a no-op.
Future<void> deleteTempRecordingDir(String? path) async {
  if (path == null) return;
  try {
    final dir = File(path).parent;
    if (await dir.exists()) await dir.delete(recursive: true);
  } catch (_) {}
}

/// Where a take's WAV lives and how the file of a take that must not survive
/// (start failed/invalidated, or the owner aborts) is cleaned up.
class RecordingPathStrategy {
  const RecordingPathStrategy({required this.create, required this.discard});

  /// The ephemeral-take strategy: a fresh `<systemTemp>/<prefix>XXXX/` dir per
  /// take, recording into `<prefix>.wav` inside it; discard deletes the whole
  /// dir (never just the file, so nothing strands the dir).
  RecordingPathStrategy.tempDir(String prefix)
    : create = (() async {
        final dir = await Directory.systemTemp.createTemp(prefix);
        return '${dir.path}/$prefix.wav';
      }),
      discard = deleteTempRecordingDir;

  /// Creates the destination and returns the WAV path to record into.
  final Future<String> Function() create;

  /// Best-effort cleanup of an aborted/failed take at `path`.
  final Future<void> Function(String path) discard;
}

/// The shared mic-recording core: one guarded start → pump → stop lifecycle
/// used by every screen that records the microphone.
///
/// [start] runs the full guarded arm sequence — mic permission, WAV
/// destination creation via a [RecordingPathStrategy], `startInputRecording`
/// on the engine, the 30 ms capture pump feeding a [MonitorService] (live
/// spectrogram/metrics) and the 50 ms level/device-watch timer — registering
/// with [RecordingFinalizer] so an in-flight take is finalized if the app
/// quits mid-record. [stop] tears the hardware/timers down and reports the
/// path plus the writer's clean/dirty flag; [abort] additionally discards the
/// file. Every await in [start] is bracketed by disposal/generation guards
/// ([invalidatePendingStart], [dispose]) so a start pending behind the
/// permission dialog or file IO can never arm a hot mic after its owning
/// session ended.
///
/// The controller does NOT own the engine: [acquireEngine] borrows it per
/// take (the technique detail VM creates its private engine lazily there and
/// applies the Settings input device; the song detail VM will hand over its
/// playback engine). Engine disposal stays with the owner.
///
/// Currently the technique detail VM is the only consumer; the song practice
/// detail VM migrates onto this controller in the NEXT phase. Its extra needs
/// are already representable:
///  * count-in / take setup — VM-side work before calling [start] (nothing
///    here arms hardware until the engine start succeeds);
///  * per-take destination — a custom [RecordingPathStrategy] wrapping its
///    AudioFileService `newRecordingPath`/`deleteAudio`;
///  * meter / live waveform peaks / elapsed clock — [onLevelTick] delivers the
///    raw engine peak once per 50 ms level tick (the song VM's existing record
///    interval), while [level] keeps the plain clamped level for simple meters;
///  * device-loss handling — [onInputDeviceLost] makes the level tick poll and
///    react to input loss (the technique screen's only device-event consumer);
///    the song VM can instead pass null and keep its own device watch, since
///    `pollDeviceEvents` clears on read and must have exactly one poller;
///  * quit finalization — [onQuitFinalize] is registered for exactly the
///    armed window, replacing each VM's constructor-lifetime registration.
class MicRecordingController {
  MicRecordingController({
    required GraphAudioPlaybackService Function() acquireEngine,
    MonitorService? monitor,
    Future<void> Function()? onQuitFinalize,
    this.onInputDeviceLost,
    this.onLevelTick,
    Future<bool> Function() requestPermission = MicPermission.request,
    RecordingFinalizer? finalizer,
    this.readyTimeout = const Duration(seconds: 2),
  }) : _acquireEngine = acquireEngine,
       _monitor = monitor,
       _onQuitFinalize = onQuitFinalize,
       _requestPermission = requestPermission,
       _finalizer = finalizer ?? RecordingFinalizer.instance;

  /// How long [start] waits for the armed mic to actually deliver frames
  /// before giving up (see [MicRecordingFailure.inputNotDelivering]). A warm
  /// stream answers within one callback (~10 ms); a cold VoiceProcessingIO
  /// open takes a few hundred. Injectable so tests can force the timeout
  /// deterministically.
  final Duration readyTimeout;

  /// Live-preview cadence: pull captured frames into the monitor every 30 ms
  /// so the live spectrogram/metrics stay smooth.
  static const _captureIntervalMs = 30;

  /// Meter/device-watch cadence (also the elapsed clock the song detail VM
  /// derives its live waveform peaks from — one [onLevelTick] per tick).
  /// Public so an owner deriving durations from tick counts can alias it
  /// instead of hard-coding a second 50 that could drift.
  static const int levelIntervalMs = 50;

  /// Borrows the engine to record with; called once per [start], after the
  /// permission gate (so a denied/abandoned start never touches — or lazily
  /// creates — an engine). The owner keeps engine ownership and disposal.
  final GraphAudioPlaybackService Function() _acquireEngine;

  /// When provided, recording also feeds this monitor with the live input
  /// (engine capture + 30 ms pump), for real-time analysis views.
  final MonitorService? _monitor;

  /// Registered with [RecordingFinalizer] for exactly the armed window: runs
  /// the owner's stop-and-save path when the app quits mid-record.
  final Future<void> Function()? _onQuitFinalize;

  /// Called from the level tick when the engine reports the input device was
  /// lost (poll bit 2) — the owner should stop the take gracefully (finalizing
  /// KEEPS what was captured) and surface its own notice. The tick observing
  /// the loss skips its level update. When null the controller does not poll
  /// device events at all (they clear on read, so an owner with its own
  /// device watch must remain the only poller).
  final void Function()? onInputDeviceLost;

  /// Called once per level tick with the raw engine input peak (linear 0..1,
  /// unclamped), for owners that derive their own meter scale, live waveform
  /// peaks, or elapsed time. [level] is updated first.
  final void Function(double rawPeak)? onLevelTick;

  /// Injectable for tests; defaults to the real macOS permission round-trip.
  final Future<bool> Function() _requestPermission;

  /// The quit-finalize registry an armed take registers with; injectable so
  /// an owner (and its tests) can pass a private registry, defaulting to the
  /// app-wide [RecordingFinalizer.instance].
  final RecordingFinalizer _finalizer;

  /// Live input level (0..1, clamped) while recording, for a meter; reset to
  /// 0 on stop. Disposed with the controller.
  final ValueNotifier<double> level = ValueNotifier(0);

  /// The engine of the current (or latest) take, kept only so teardown can
  /// close a racing arm's native WAV handle. Never created or disposed here.
  GraphAudioPlaybackService? _engine;

  Timer? _captureTimer;
  Timer? _levelTimer;
  String? _path;
  RecordingPathStrategy? _destination;
  bool _isRecording = false;
  bool _starting = false;
  bool _feedingMonitor = false;
  bool _disposed = false;

  /// Bumped whenever a pending start must be abandoned; each [start] snapshots
  /// it before its awaits and arms nothing on a mismatch.
  int _generation = 0;

  /// True while a take is armed (WAV being written, pumps running).
  bool get isRecording => _isRecording;

  /// True during a start's pending window — set synchronously when [start] is
  /// called (so a fast double-tap can be guarded before the permission/IO
  /// awaits resolve) and cleared when it arms, fails, or is invalidated.
  bool get isStarting => _starting;

  /// True while the live input is being fed into the monitor (i.e. recording,
  /// with a [MonitorService] attached) — drives "live view" UI states.
  bool get feedingMonitor => _feedingMonitor;

  /// Abandons any start still pending behind its permission / destination
  /// awaits: bumps the generation the pending start snapshotted (it will arm
  /// nothing and discard its file when it resumes) and clears [isStarting]
  /// immediately, so the owner's UI guards unblock without waiting for the
  /// abandoned awaits (which can pend on the system permission dialog).
  void invalidatePendingStart() {
    _generation++;
    _starting = false;
  }

  /// Requests mic permission, creates the WAV via [destination], starts the
  /// engine's input recording, then arms the capture/level pumps and the quit
  /// finalizer. Returns started / failed / invalidated (see
  /// [MicRecordingStart]); on anything but started, no hardware or timers are
  /// left armed and any created file is discarded.
  Future<MicRecordingStart> start(RecordingPathStrategy destination) async {
    // Re-entrancy: one take at a time. The owner's canRecord guards normally
    // prevent this; a start slipping through anyway is a silent no-op.
    if (_disposed || _isRecording || _starting) {
      return const MicRecordingStart.invalidated();
    }
    _starting = true;
    // Snapshot the generation: an invalidate/dispose during the permission or
    // destination awaits below abandons this start — arming the mic then would
    // leave it hot with no stop affordance in the owner's UI.
    final generation = _generation;
    try {
      if (!await _requestPermission()) {
        return const MicRecordingStart.failed(
          MicRecordingFailure.permissionDenied,
        );
      }
      // The owner may have been disposed during the permission await (it could
      // not tear down a not-yet-started recording), or invalidated this start
      // (its edit session was cancelled/saved). Bail before touching — or
      // lazily creating — the engine.
      if (_disposed || _generation != generation) {
        return const MicRecordingStart.invalidated();
      }
      final engine = _acquireEngine();
      _engine = engine;
      if (!engine.isAvailable) {
        return const MicRecordingStart.failed(
          MicRecordingFailure.engineUnavailable,
        );
      }
      final path = await destination.create();
      // Same guard after the destination await: a dispose/invalidate in this
      // window can't stop a recording that hasn't started. Don't arm
      // hardware/timers; drop the just-created destination.
      if (_disposed || _generation != generation) {
        _discardQuietly(destination, path);
        return const MicRecordingStart.invalidated();
      }
      // A false return means the writer could not create the WAV (disk full /
      // permissions) and nothing would be recorded: fail loudly instead of
      // "recording" into nowhere, and arm no timers or state.
      if (!engine.startInputRecording(path)) {
        _discardQuietly(destination, path);
        return const MicRecordingStart.failed(
          MicRecordingFailure.fileCreateFailed,
        );
      }
      // Ready handshake. startInputRecording == true proves only that the WAV
      // exists — the native input-stream open is deliberately asynchronous (a
      // blocking open froze the UI), so the mic may still be opening, or may
      // fail to open, right now. Wait (bounded) for BOTH halves of the proof:
      //  * recorded progress — recordWrittenFrames counts only frames that
      //    actually LANDED in this take's recording ring (reset at the arm);
      //    a nonzero value is unforgeable (pre-arm and dying-stream callbacks
      //    read the disarmed flag and never write this ring);
      //  * ordering — inputAppliedSeq >= the arm's intent sequence proves the
      //    host has CONVERGED to a desired state at least as new as this arm
      //    (want = on, since recording holds it on). Without it, a warm
      //    stream's frames could land before the host processed an older
      //    queued teardown; the host's desired-state cell makes that teardown
      //    a no-op, and this watermark is the observable receipt that the
      //    convergence actually happened.
      // When the stream is already open (monitoring), both resolve within one
      // callback (~10 ms); a cold VoiceProcessingIO open takes a few hundred.
      final armSeq = engine.inputIntentSeq();
      var ready = false;
      final readyClock = Stopwatch()..start();
      while (readyClock.elapsed < readyTimeout) {
        if (_disposed || _generation != generation) {
          engine.stopInputRecording();
          _discardQuietly(destination, path);
          return const MicRecordingStart.invalidated();
        }
        if (engine.recordWrittenFrames() > 0 &&
            engine.inputAppliedSeq() >= armSeq) {
          ready = true;
          break;
        }
        await Future<void>.delayed(const Duration(milliseconds: 25));
      }
      if (!ready) {
        engine.stopInputRecording();
        _discardQuietly(destination, path);
        return const MicRecordingStart.failed(
          MicRecordingFailure.inputNotDelivering,
        );
      }
      _isRecording = true;
      _path = path;
      _destination = destination;
      final monitor = _monitor;
      if (monitor != null) {
        // Live preview: the engine captures the same input it's recording
        // (cpal), so the live spectrogram / tuner / metrics run off it.
        engine.startCapture();
        monitor.start();
        _feedingMonitor = true;
        _captureTimer = Timer.periodic(
          const Duration(milliseconds: _captureIntervalMs),
          (_) {
            if (_disposed) return;
            final frames = engine.pullInputFrames(); // interleaved stereo
            if (frames.isEmpty) return;
            final n = frames.length ~/ 2;
            final mono = Float64List(n);
            for (var i = 0; i < n; i++) {
              mono[i] = (frames[2 * i] + frames[2 * i + 1]) * 0.5;
            }
            monitor.feedSamples(mono);
          },
        );
      }
      _levelTimer = Timer.periodic(
        const Duration(milliseconds: levelIntervalMs),
        (_) {
          if (_disposed) return;
          // Input loss must stop the take gracefully — finalizing KEEPS what
          // was captured — instead of letting capture die silently. Poll only
          // when the owner handles it here (see [onInputDeviceLost]).
          final onLost = onInputDeviceLost;
          if (onLost != null && engine.pollDeviceEvents() & 2 != 0) {
            onLost();
            return;
          }
          final peak = engine.inputLevel();
          level.value = peak.clamp(0.0, 1.0);
          onLevelTick?.call(peak);
        },
      );
      // Finalize this take if the app quits mid-record; unregistered on stop.
      _finalizer.register(_finalizeOnQuit);
      return MicRecordingStart.started(path);
    } finally {
      // Only this start's own outcome clears the flag: after an invalidate a
      // NEWER start may already be pending and own it.
      if (_generation == generation) _starting = false;
    }
  }

  /// Fire-and-forget discard of an aborted start's file. A discard strategy
  /// can throw (deleteAudio permission/IO errors), and these calls are
  /// unawaited — log the failure instead of letting it escape into the root
  /// zone as an unhandled async error.
  static void _discardQuietly(RecordingPathStrategy destination, String path) {
    unawaited(
      destination.discard(path).catchError((Object e) {
        DiagnosticsLog.instance.log('mic', 'discard failed for $path: $e');
      }),
    );
  }

  /// Stops the mic hardware, live capture/monitor feed and meter (without
  /// touching the file). Returns the take's path and whether the writer
  /// finalized it cleanly; null when no take is in flight.
  MicRecordingStop? stop() {
    if (!_isRecording) return null;
    _levelTimer?.cancel();
    _levelTimer = null;
    _captureTimer?.cancel();
    _captureTimer = null;
    level.value = 0;
    final engine = _engine;
    final clean = engine?.stopInputRecording() ?? true;
    // Read AFTER stop: the counter is stable once the recording is disarmed,
    // and any overflow during the final drain is included.
    final dropped = engine?.recordDroppedFrames() ?? 0;
    if (_feedingMonitor) {
      engine?.stopCapture();
      _monitor?.stop();
      _feedingMonitor = false;
    }
    _isRecording = false;
    _finalizer.unregister(_finalizeOnQuit);
    final path = _path!;
    _path = null;
    _destination = null;
    return (path: path, clean: clean, droppedFrames: dropped);
  }

  /// Stops AND discards an in-flight take (its file is deleted via the path
  /// strategy) — for owners tearing down the session the take belonged to
  /// (e.g. cancelling the edit an example was being recorded for). No-op when
  /// not recording.
  Future<void> abort() async {
    if (!_isRecording) return;
    final destination = _destination;
    final stopped = stop();
    if (stopped != null && destination != null) {
      await destination.discard(stopped.path);
    }
  }

  /// Registered with [RecordingFinalizer] while a take is armed. (A method
  /// tear-off is a stable reference, so register/unregister match.)
  Future<void> _finalizeOnQuit() async {
    final finalize = _onQuitFinalize;
    if (finalize != null) await finalize();
  }

  /// Tears everything down: invalidates a pending start, cancels the pumps,
  /// closes a racing arm's native WAV handle, and disposes [level]. The take
  /// FILE is not discarded — the owner keeps its path (from [start]) and
  /// applies its own cleanup policy (the technique VM deletes its temp dirs;
  /// the song VM keeps the take for the boot sweep).
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    // Stop a recording that is running OR still starting (its start awaits
    // haven't resolved). The generation/_disposed guards in [start] keep it
    // from arming after this, so stopping here is belt-and-suspenders against
    // a hot mic.
    final active = _isRecording || _starting;
    invalidatePendingStart();
    _finalizer.unregister(_finalizeOnQuit);
    _levelTimer?.cancel();
    _levelTimer = null;
    _captureTimer?.cancel();
    _captureTimer = null;
    if (active) {
      _engine?.stopInputRecording();
      if (_feedingMonitor) _engine?.stopCapture();
    }
    // Deliberately no _monitor.stop(): the owner is mid-dispose and about to
    // dispose the monitor itself; stop()'s notifyListeners must not fire into
    // a half-torn-down widget tree.
    _isRecording = false;
    _feedingMonitor = false;
    _path = null;
    _destination = null;
    level.dispose();
  }
}
