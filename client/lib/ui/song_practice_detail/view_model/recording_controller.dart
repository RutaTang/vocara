import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../../data/services/app_settings_service.dart';
import '../../../data/services/audio_file_service.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/mic_permission.dart';
import '../../../data/services/mic_recording_controller.dart';
import '../../../data/services/recording_finalizer.dart';
import '../../../data/services/waveform_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/song_practice.dart';
import '../../../utils/busy_gate.dart';
import '../../../utils/id.dart';
import 'analysis_cache.dart';
import 'mixer_controller.dart';
import 'playback_controller.dart';

/// Every consumer of the engine's INPUT side for the song detail screen, in
/// one place: take recording (start/stop/finalize) on the shared
/// [MicRecordingController] core, input monitoring (headphone toggle + MIC
/// meter), the 1 s device watch (output-lost reopen, input-lost teardown, the
/// macOS-VPIO input-stall watchdog), the record/input meters and the live
/// recording waveform/clock.
///
/// It borrows the engine (never creates or disposes it) and edits the model
/// only through the host closures — take staging (arm target, empty take,
/// append + select the recorded clip, save) stays view-model logic invoked
/// from here in the recording order.
class RecordingController extends ChangeNotifier {
  RecordingController({
    required GraphAudioPlaybackService? Function() graphEngine,
    required PlaybackController playback,
    required MixerController mixer,
    required AnalysisCache analysis,
    required BusyGate busyGate,
    required AudioFileService audioFiles,
    required WaveformService waveforms,
    required AppSettingsService appSettings,
    required String practiceId,
    required RecordingFinalizer finalizer,
    required SongPractice? Function() practice,
    required String? Function() selectedTrackId,
    required AudioTrack? Function(String) trackById,
    required void Function(String, AudioTrack Function(AudioTrack)) updateTrack,
    required String Function({
      required String path,
      required WaveformResult analysis,
      required int startMs,
    })
    appendVocal,
    required Future<void> Function() createEmptyVocalTake,
    required void Function(String trackId, String? clipId) selectRecordedClip,
    required Future<bool> Function() save,
    required void Function(String) toast,
    required String Function(Object) friendlyError,
  }) : _graphEngine = graphEngine,
       _playback = playback,
       _mixer = mixer,
       _analysis = analysis,
       _busyGate = busyGate,
       _audioFiles = audioFiles,
       _waveforms = waveforms,
       _appSettings = appSettings,
       _practiceId = practiceId,
       _finalizer = finalizer,
       _practiceOf = practice,
       _selectedTrackId = selectedTrackId,
       _trackById = trackById,
       _updateTrack = updateTrack,
       _appendVocal = appendVocal,
       _createEmptyVocalTake = createEmptyVocalTake,
       _selectRecordedClip = selectRecordedClip,
       _save = save,
       _toast = toast,
       _friendlyError = friendlyError {
    _startDeviceWatch();
  }

  /// The native engine, when it's the active backend (only it supports device
  /// I/O + monitoring). Borrowed — the view model owns its lifecycle.
  final GraphAudioPlaybackService? Function() _graphEngine;

  /// Transport + master clock: recording writes [PlaybackController.position]
  /// directly (the playhead follows the record clock during a take) and drives
  /// seek/play/pause and the loop-wrap disable through the SAME engine.
  final PlaybackController _playback;

  /// Resyncing on start is what mutes the punched take in the engine.
  final MixerController _mixer;

  /// Live view interlock (capture is single-owner) + device-watch teardown +
  /// analysis refresh after the take lands.
  final AnalysisCache _analysis;

  /// The screen-wide busy mutex: guards the Stop→Record re-arm race and runs
  /// the stop finalize.
  final BusyGate _busyGate;

  final AudioFileService _audioFiles;
  final WaveformService _waveforms;
  final AppSettingsService _appSettings;
  final String _practiceId;
  final RecordingFinalizer _finalizer;

  // Host closures (model + selection stay owned by the view model).
  final SongPractice? Function() _practiceOf;
  final String? Function() _selectedTrackId;
  final AudioTrack? Function(String) _trackById;
  final void Function(String, AudioTrack Function(AudioTrack)) _updateTrack;
  final String Function({
    required String path,
    required WaveformResult analysis,
    required int startMs,
  })
  _appendVocal;
  final Future<void> Function() _createEmptyVocalTake;

  /// Selects the lane (and, when non-null, the clip) the recording landed on.
  /// A null clip id leaves the host's clip selection untouched (new-take case).
  final void Function(String trackId, String? clipId) _selectRecordedClip;
  final Future<bool> Function() _save;
  final void Function(String) _toast;
  final String Function(Object) _friendlyError;

  /// Set in [dispose]. Guards timer ticks and post-await continuations racing
  /// the tear-down (the route can pop mid-start).
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  // --- Shared mic-recording core -------------------------------------------

  /// The shared guarded start → pump → stop lifecycle. monitor stays null: the
  /// song screen never feeds the MonitorService while recording (the live view
  /// is forced off before a take). onInputDeviceLost stays null: pollDeviceEvents
  /// CLEARS ON READ, and this controller's own 1 s device watch must remain the
  /// process's ONLY poller — a second poller would steal the output-lost/
  /// input-lost bits and device loss would never heal.
  late final MicRecordingController _mic = MicRecordingController(
    // Only called after startRecording's own null check passed, and the VM
    // holds one engine for its whole lifetime — the cast stays valid.
    acquireEngine: () => _graphEngine()!,
    monitor: null,
    onInputDeviceLost: null,
    // Registered for exactly the armed window (replaces the old VM-lifetime
    // registration): finalize the in-flight take if the app quits mid-record.
    onQuitFinalize: () async {
      if (isRecording) await stopRecording();
    },
    onLevelTick: _onLevelTick,
    finalizer: _finalizer,
  );

  /// Where a take records to, and the take setup that must run in ORDER —
  /// after the permission gate, exactly like the pre-controller flow: resolve
  /// the armed target (creating an empty take when nothing is armed), freeze
  /// it, stamp the start position, clear the live peaks, then create the WAV
  /// destination. A permission denial therefore never creates an empty take.
  late final RecordingPathStrategy _takeDestination = RecordingPathStrategy(
    create: () async {
      if (_appSettings.bluetoothInOutConflict) {
        _toast(
          'Recording from the Bluetooth headset lowers playback quality. '
          'pick a different mic in Settings.',
        );
      }
      // Record onto the armed take (the selected vocal lane) at the playhead, so
      // the live waveform draws on that lane. If nothing recordable is armed,
      // create an empty take to record into — there's no separate REC lane.
      var targetId = recordTargetId;
      if (targetId == null) {
        await _createEmptyVocalTake();
        targetId = _selectedTrackId();
      }
      _recordingTargetId = targetId;
      _recordingStartMs = _playback.position.value.inMilliseconds;
      _recPeaks.clear();
      recordingPeaks.value = const [];
      return _audioFiles.newRecordingPath(practiceId: _practiceId);
    },
    // Failed/invalidated starts drop the created WAV; an armed take's file is
    // never discarded here (it is kept for the boot sweep on hard kills).
    discard: _audioFiles.deleteAudio,
  );

  /// True while a take is armed (WAV being written, pumps running).
  bool get isRecording => _mic.isRecording;

  /// Live input level (0..1) while recording. Drives the meter only.
  /// NOTE: currently no widget on this screen reads it (kept for parity;
  /// flagged for deletion in the cleanup stage).
  final ValueNotifier<double> recordLevel = ValueNotifier(0);

  /// Live input peak (0..1) while input monitoring is on. Drives the monitor meter.
  final ValueNotifier<double> inputLevel = ValueNotifier(0);

  /// One tick per level interval — the live waveform resolution and the
  /// recording clock derive from it, so it must equal the mic controller's
  /// cadence (aliased, so it can't drift).
  static const int _recordIntervalMs = MicRecordingController.levelIntervalMs;

  int _recordingStartMs = 0;
  int get recordingStartMs => _recordingStartMs;

  /// The vocal take the in-flight recording is punching into (frozen at start);
  /// null when REC is instead creating a brand-new take.
  String? _recordingTargetId;

  /// Live, growing waveform peaks of the in-progress recording (one per
  /// [_recordIntervalMs]). Drives the recording lane without rebuilding the
  /// whole detail tree.
  /// Backing store for [recordingPeaks]: appended in place (amortised O(1) per tick)
  /// and surfaced as a zero-copy [UnmodifiableListView], so the notifier still fires a
  /// fresh reference each tick without the per-tick full-list copy that made a long
  /// take O(n^2).
  final List<double> _recPeaks = <double>[];
  final ValueNotifier<List<double>> recordingPeaks = ValueNotifier(const []);
  int get recordingDurationMs =>
      recordingPeaks.value.length * _recordIntervalMs;

  /// The take REC will record into: the selected lane if it's a recordable vocal
  /// take in the active interpretation, else null (→ REC creates a new take).
  /// Drives the "armed" record-dot; frozen into [_recordingTargetId] while a
  /// recording is in flight.
  String? get recordTargetId {
    if (_mic.isRecording) return _recordingTargetId;
    final id = _selectedTrackId();
    if (id == null) return null;
    final t = _trackById(id);
    if (t == null || t.kind != AudioTrackKind.vocal) return null;
    if (t.versionId != _practiceOf()?.activeVersionId) return null;
    return id;
  }

  /// Map a linear input peak (0..1) to a perceptual meter fill (0..1) on a dB scale
  /// (−50 dBFS → empty, 0 dBFS → full), so normal vocal levels read clearly instead of as a
  /// tiny linear sliver.
  static double _meterFromPeak(double peak) {
    if (peak <= 1e-6) return 0;
    final db = 20 * (math.log(peak) / math.ln10);
    return ((db + 50) / 50).clamp(0.0, 1.0);
  }

  /// Drives the meters + live waveform + recording clock off the engine's raw
  /// input peak, once per level tick while a take is armed. Ignores
  /// [MicRecordingController.level] (linear-clamped) — these meters are
  /// dB-mapped.
  void _onLevelTick(double peak) {
    // Meter: dB-perceptual (reads clearly). Live waveform: LINEAR, so it matches the final take
    // waveform (rendered from the WAV in linear amplitude) instead of looking fuller than it.
    recordLevel.value = _meterFromPeak(peak);
    // Also drive the mixer's MIC meter while recording (it shares this notifier with monitoring).
    inputLevel.value = _meterFromPeak(peak);
    _recPeaks.add(peak);
    recordingPeaks.value = UnmodifiableListView(_recPeaks);
    _playback.position.value = Duration(
      milliseconds: _recordingStartMs + recordingDurationMs,
    );
  }

  // --- Recording ------------------------------------------------------------

  Future<void> startRecording() async {
    // `|| busy` closes the re-arm race: stopRecording clears isRecording BEFORE
    // its finalize runs inside the busy gate (which holds busy), so without this a fast
    // Stop→Record would start a second take whose createEmptyVocalTake/resyncPlayback
    // interleave with the still-running finalize's save (lost update + graph thrash).
    // isStarting is the synchronous re-entrancy guard for a fast double-tap.
    if (_mic.isRecording || _mic.isStarting || _busyGate.busy) return;
    try {
      // The live view and the take can't share the engine capture.
      await _analysis.setLiveMonitor(false);
      final engine = _graphEngine();
      if (engine == null) return;
      final r = await _mic.start(_takeDestination);
      final failure = r.failure;
      if (failure != null) {
        switch (failure) {
          case MicRecordingFailure.permissionDenied:
            _toast(
              'Microphone access is off. Enable it in System Settings › '
              'Privacy & Security › Microphone.',
            );
          case MicRecordingFailure.fileCreateFailed:
            // The writer could not create the WAV (disk full / permissions) and
            // nothing would be recorded — fail loudly instead of "recording"
            // into nowhere.
            _toast(
              "Couldn't create the recording file. Check your free disk "
              'space.',
            );
          case MicRecordingFailure.engineUnavailable:
            // No usable audio device (the take setup was skipped entirely).
            _toast('No audio device is available to record with.');
        }
        return;
      }
      // Invalidated mid-await (route popped / disposed): a silent no-op.
      if (!r.started) return;
      notifyListeners();

      // Mute the take being recorded into (you can't monitor what you're laying
      // down); every other lane still obeys mute/solo. Play freely (no range wrap)
      // from the cursor as a reference.
      await _mixer.resyncPlayback();
      if (_disposed) return;
      _playback.engine.setLoop(false, 0, 0);
      await _playback.engine.seek(Duration(milliseconds: _recordingStartMs));
      if (_disposed) return;
      _playback.position.value = Duration(milliseconds: _recordingStartMs);
      await _playback.engine.play();
    } catch (e) {
      // Setup failed partway (createEmptyVocalTake/save, newRecordingPath I/O,
      // playback). If the take armed before the throw, stop it — the WAV stays
      // on disk for the boot sweep — and surface the error so REC isn't left
      // dead-buttoned.
      if (_mic.isRecording) _mic.stop();
      _toast(_friendlyError(e));
      notifyListeners();
    }
  }

  Future<void> stopRecording() async {
    // Stops the hardware + pumps and unregisters the quit finalizer; null means
    // no take was in flight (incl. a still-pending start).
    final stopped = _mic.stop();
    if (stopped == null) return;
    recordLevel.value = 0;
    // The recording loop was driving the shared MIC meter; unless monitoring is
    // also on (its own timer keeps updating it), nothing else will, so clear it
    // — otherwise the meter sticks at the last captured peak.
    if (!_inputMonitorOn) {
      _heldInputLevel = 0;
      inputLevel.value = 0;
    }
    // clean == false: the writer hit an IO error (create/write/finalize) during the take, so the
    // WAV on disk is missing, damaged, or incomplete; handled below once analyzed.
    final recordingClean = stopped.clean;
    final path = stopped.path;
    await _playback.engine.pause();
    // Record-latency compensation: the first captured sample is the vocal sung in response to the
    // backing the singer heard ~round-trip ago (output IO + acoustic + input IO + AEC), so the take
    // belongs that much EARLIER on the timeline — otherwise it plays back behind the beat.
    final latencyMs = _graphEngine()?.recordLatencyMs() ?? 0.0;
    final startMs = (_recordingStartMs - latencyMs).round().clamp(
      0,
      _recordingStartMs,
    );
    final targetId = _recordingTargetId;
    _recordingTargetId = null;
    _recPeaks.clear();
    recordingPeaks.value = const [];
    notifyListeners();

    await _busyGate.run(() async {
      WaveformResult analysis;
      if (recordingClean) {
        analysis = await _waveforms.analyze(path);
      } else {
        // The engine reported an IO error during the take (disk full, write or
        // finalize failure). Be conservative: keep the take only if the partial
        // WAV is still present and readable; otherwise delete it so no take row
        // points at unusable audio.
        WaveformResult? partial;
        try {
          if (await File(path).exists()) {
            partial = await _waveforms.analyze(path);
          }
        } catch (_) {
          partial = null;
        }
        if (partial == null) {
          try {
            await _audioFiles.deleteAudio(path);
          } catch (_) {
            // Best-effort cleanup of the unreadable partial file.
          }
          _toast('Recording failed. The take could not be saved.');
          return;
        }
        _toast('Recording hit a disk error. The take may be incomplete.');
        analysis = partial;
      }
      final target = targetId == null ? null : _trackById(targetId);
      if (target == null || target.kind != AudioTrackKind.vocal) {
        // No armed take → the recording becomes a new take at the cursor.
        _selectRecordedClip(
          _appendVocal(path: path, analysis: analysis, startMs: startMs),
          null,
        );
      } else {
        // Drop the recording onto the armed take as its own clip at the cursor —
        // a separate clip, never merged into existing audio (Logic / Cubase).
        final clip = AudioClip(
          id: newId(),
          audioFilePath: path,
          durationMs: analysis.durationMs,
          startMs: startMs,
          waveformPeaks: analysis.peaks,
        );
        _updateTrack(target.id, (t) => t.copyWith(clips: [...t.clips, clip]));
        // Select BOTH the lane and the clip (matching addClipToTake): selection
        // isn't gated during a take, so if the user tapped a different lane while
        // recording, leaving the lane selection stale would pair this clip id with
        // the wrong lane and the inspector would analyze the wrong subject.
        _selectRecordedClip(target.id, clip.id);
        _analysis.onTrackAudioChanged(target.id);
      }
      await _save();
    });
  }

  // --- Input monitoring -----------------------------------------------------

  bool _inputMonitorOn = false;
  // Synchronous re-entrancy guard for toggleInputMonitor (its ON branch awaits a permission
  // round-trip, during which a second tap must not arm monitoring twice and orphan a Timer).
  bool _togglingMonitor = false;

  /// Whether the singer hears themselves through the engine (live input monitoring).
  bool get inputMonitorEnabled => _inputMonitorOn;
  Timer? _inputLevelTimer;
  // Peak-hold state for the monitor meter so brief/low vocal peaks stay visible (decays smoothly).
  double _heldInputLevel = 0;

  /// Toggle live input monitoring — open/close the mic stream and ramp the monitor in/out, and
  /// poll the input level for the meter. No-op on engines without monitoring.
  Future<void> toggleInputMonitor() async {
    final engine = _graphEngine();
    if (engine == null) return;
    // Synchronous re-entrancy guard: the ON branch awaits a permission round-trip, so a
    // fast double-tap could otherwise arm monitoring twice and orphan a level Timer.
    if (_togglingMonitor) return;
    _togglingMonitor = true;
    try {
      if (_inputMonitorOn) {
        engine.stopMonitor();
        _inputMonitorOn = false;
        _inputLevelTimer?.cancel();
        _inputLevelTimer = null;
        _heldInputLevel = 0;
        inputLevel.value = 0;
      } else {
        if (!await MicPermission.request()) {
          _toast(
            'Microphone access is off. Enable it in System Settings › '
            'Privacy & Security › Microphone.',
          );
          return;
        }
        // The view could have been disposed during the permission await.
        if (_disposed) return;
        engine.startMonitor();
        _inputMonitorOn = true;
        _heldInputLevel = 0;
        // Cancel any prior timer before assigning so a leftover never leaks.
        _inputLevelTimer?.cancel();
        _inputLevelTimer = Timer.periodic(const Duration(milliseconds: 50), (
          _,
        ) {
          // While a take is recording, the record-level tick owns the shared
          // inputLevel meter (it writes the instantaneous peak at the same 50ms
          // cadence). Yield so the two timers don't fight and flicker the mixer
          // MIC meter; this timer keeps running and reclaims the meter the moment
          // recording stops (stopRecording only clears it when monitoring is off).
          if (isRecording) return;
          final meter = _meterFromPeak(engine.inputLevel());
          // Peak-hold with decay: hold the recent peak so brief vocal hits stay visible,
          // then ease back instead of snapping to black between syllables.
          _heldInputLevel = math.max(meter, _heldInputLevel - 0.05);
          inputLevel.value = _heldInputLevel;
        });
      }
      notifyListeners();
    } finally {
      _togglingMonitor = false;
    }
  }

  // --- Device watch -----------------------------------------------------------

  Timer? _deviceWatch;
  // Input-stall watchdog: macOS VPIO raises no device-loss event, so the device-watch also
  // treats a non-advancing input frame counter (while capturing) as a lost input.
  int _lastInputFrames = 0;
  int _inputStallTicks = 0;

  /// Watch for lost audio devices and self-heal. The playhead is driven by the output stream's
  /// callback, so if that stream dies (a Bluetooth headset flips to HFP, a device is unplugged),
  /// transport would otherwise freeze. On an output-lost event we re-open the current output.
  void _startDeviceWatch() {
    final engine = _graphEngine();
    if (engine == null) return;
    // This is the ONLY consumer of pollDeviceEvents (it clears on read), so both
    // the output- and input-lost bits must be handled here.
    _deviceWatch = Timer.periodic(const Duration(seconds: 1), (_) {
      final events = engine.pollDeviceEvents();
      if (events & 1 != 0) {
        // Output lost: re-open the current output so transport doesn't freeze.
        engine.setOutputDevice(engine.currentOutputDevice());
      }
      if (events & 2 != 0) {
        _onInputDeviceLost();
      } else if (isRecording || _inputMonitorOn || _analysis.liveMonitor) {
        // No explicit input-loss event, but capture is supposed to be live (recording,
        // input monitoring, OR the live-spectrogram view — all pull the same input frames).
        // macOS VPIO raises no device-loss event, so a mic unplugged mid-capture freezes
        // silently; detect it by the engine's input frame counter not advancing for ~2s and
        // recover through the same graceful path.
        final frames = engine.inputFramesReceived();
        if (frames != _lastInputFrames) {
          _lastInputFrames = frames;
          _inputStallTicks = 0;
        } else if (++_inputStallTicks >= 2) {
          _inputStallTicks = 0;
          _onInputDeviceLost();
        }
      } else {
        // Keep the baseline synced while idle so arming starts from the right count.
        _lastInputFrames = engine.inputFramesReceived();
        _inputStallTicks = 0;
      }
    });
  }

  /// The input device vanished mid-session (mic unplugged, a Bluetooth headset
  /// flipped profile). Like a pro DAW, never let capture die silently: stop
  /// recording gracefully — finalizing KEEPS the take captured so far — and drop
  /// monitoring, each with a clear notice. We do NOT auto-switch the mic mid-take
  /// (that would splice two different inputs into one take).
  void _onInputDeviceLost() {
    // Recording, input monitoring and the live view can be active together
    // (starting a take does NOT stop monitoring), so tear down EVERY active input
    // consumer — a single else-if would leave the others' streams open and freeze
    // the shared MIC meter at its last peak (the dead device's input level never
    // updates again) until a later watchdog cycle. One notice.
    final wasRecording = isRecording;
    final wasMonitoring = _inputMonitorOn;
    final wasLive = _analysis.liveMonitor;
    if (!wasRecording && !wasMonitoring && !wasLive) return;
    _toast(
      wasRecording
          ? 'Recording stopped: the input device was disconnected.'
          : wasMonitoring
          ? 'Input monitoring stopped: the input device was disconnected.'
          : 'Live view stopped: the input device was disconnected.',
    );
    // stopRecording first: while _inputMonitorOn is still true it intentionally
    // leaves the meter for monitoring to own; toggleInputMonitor's OFF branch then
    // closes the monitor stream AND resets the meter to 0, so it can't pin on the
    // stale peak of the now-dead device.
    if (wasRecording) unawaited(stopRecording());
    if (wasMonitoring) unawaited(toggleInputMonitor());
    if (wasLive) unawaited(_analysis.setLiveMonitor(false));
  }

  /// MUST run before the view model disposes the engine: the device watch and
  /// meter timers poll it over FFI, and [_mic]'s dispose closes a racing arm's
  /// native WAV handle (and unregisters the quit finalizer).
  @override
  void dispose() {
    _disposed = true;
    _deviceWatch?.cancel();
    _inputLevelTimer?.cancel();
    _mic.dispose();
    recordLevel.dispose();
    inputLevel.dispose();
    recordingPeaks.dispose();
    super.dispose();
  }
}
