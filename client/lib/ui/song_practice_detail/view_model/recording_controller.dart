import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../../data/services/app_settings_service.dart';
import '../../../data/services/audio_edit_service.dart';
import '../../../data/services/audio_file_service.dart';
import '../../../data/services/diagnostics_log.dart';
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
    required Future<String?> Function() createEmptyVocalTake,
    required void Function(String trackId, String? clipId) selectRecordedClip,
    required Future<bool> Function() save,
    required void Function(String) toast,
    required String Function(Object) friendlyError,
    MicRecordingController? mic,
  }) : _graphEngine = graphEngine,
       _micOverride = mic,
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

  /// Creates the empty take REC records into when nothing is armed; returns
  /// its id once its save landed, or null when it did not.
  final Future<String?> Function() _createEmptyVocalTake;

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
  /// Test seam: a fake recorder so controller-level races (the take ABA, the
  /// stop-vs-continuation windows) are testable without the platform channels
  /// the real controller's permission/engine path needs. Mirrors the technique
  /// VM's recorder override. Not annotated here: the view model passes its own
  /// (test-only) override through, so the screen-level flows are testable too.
  final MicRecordingController? _micOverride;

  late final MicRecordingController _mic =
      _micOverride ??
      MicRecordingController(
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
        // The id the take was created with, never a re-read of the selection:
        // a failed save reverts the model and moves the selection to the first
        // lane — usually the backing, which the take would then mute as its
        // punch-in target for the whole recording. Nothing to record into:
        // abort (the save already said so).
        targetId = await _createEmptyVocalTake();
        if (targetId == null) throw const _TakeSetupFailed();
      }
      _takeGeneration++;
      _recordingTargetId = targetId;
      _recordingStartMs = _playback.position.value.inMilliseconds;
      _recordingPreRollMs = 0;
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

  /// True during REC's arm window — after the user tapped Record but before
  /// the permission/file awaits resolve and [isRecording] flips true. The live
  /// view treats it like recording: the capture already belongs to the
  /// incoming take. So does the transport: a Stop here cancels the take (see
  /// [stopRecording]), and the window lasts until a cancelled start unwound.
  bool get isStartingTake => _mic.isStarting || _cancellingStart;

  /// Set when Stop cancels a start still arming, until that start returns:
  /// its take setup (the empty take's save) may still be finishing, and a
  /// fast re-arm's setup interleaving with it would bump the take generation
  /// under the new take and move its start position.
  bool _cancellingStart = false;

  /// Synchronous Stop latch: set at [stopRecording]'s entry (before its pause/
  /// finalize awaits) and cleared when the finalize completes. During those
  /// awaits [isRecording] is already false and the busy gate not yet held, so
  /// without this a fast Stop→REC would arm take #2 interleaved with take #1's
  /// finalize. Checked by [startRecording]'s guard.
  bool _stopping = false;

  /// True from Stop until the stopped take's finalize has committed (see
  /// [_stopping]) — the window where [isRecording] already reads false but the
  /// take is not in the model yet. The host's structural edits refuse it like
  /// a take in flight: one holding the busy gate here makes the finalize wait.
  bool get isStopping => _stopping;

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

  /// Monotonic per-take token, bumped when a take's destination is created.
  /// The start continuation captures it and re-checks after every await: a
  /// plain `isRecording` bool is ABA-vulnerable — take A's continuation can
  /// park across A's stop AND take B's arm, resume, see `isRecording == true`
  /// (B's!), and drive B's transport with A's stale seek/play/pre-roll.
  int _takeGeneration = 0;

  /// Milliseconds of audio recorded BEFORE the backing playback actually
  /// started (mic arm → play), measured from the engine's input-frame counter.
  /// The WAV's content sung along to the backing begins this far into the
  /// file, so take placement subtracts it — without this, everything between
  /// arm and play (graph resync, decode, seek) lands as unbounded skew.
  int _recordingPreRollMs = 0;

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
    // The rule [SongPractice.activeVocalTracks] lists lanes by: a take on
    // screen is armable. Comparing the raw ids left a visible take
    // un-armable whenever the active id, or the take's own, did not resolve.
    final practice = _practiceOf();
    if (practice != null && !practice.listsTake(t)) return null;
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
    _playback.setRecordingPosition(
      Duration(milliseconds: _recordingStartMs + recordingDurationMs),
    );
  }

  // --- Recording ------------------------------------------------------------

  Future<void> startRecording() async {
    // `_stopping || busy` closes the re-arm race: stopRecording clears
    // isRecording FIRST, then awaits the transport pause and only THEN runs
    // its finalize inside the busy gate — `_stopping` covers the whole window
    // (it is set synchronously at stopRecording's entry), `busy` the gated
    // tail. Without them a fast Stop→Record would start a second take whose
    // createEmptyVocalTake/resyncPlayback interleave with the still-running
    // finalize's save (lost update + graph thrash).
    // isStartingTake is the synchronous re-entrancy guard for a fast
    // double-tap (and keeps a cancelled start's setup from interleaving).
    if (_mic.isRecording || isStartingTake || _stopping || _busyGate.busy) {
      return;
    }
    // Captured after the take arms (see below); -1 = never armed. Declared out
    // here so the catch can tell whether the armed take is still OURS — a
    // catch that only checks isRecording is ABA-vulnerable exactly like the
    // happy path (A throws after A's stop and B's arm → stopping "the" take
    // would stop B).
    var takeGen = -1;
    try {
      // The live view and the take can't share the engine capture.
      await _analysis.setLiveMonitor(false);
      final engine = _graphEngine();
      if (engine == null) return;
      final starting = _mic.start(_takeDestination);
      // The arm window is open: show it, so Stop reaches [stopRecording].
      notifyListeners();
      final MicRecordingStart r;
      try {
        r = await starting;
      } finally {
        _cancellingStart = false;
      }
      // Armed, failed or cancelled: the arm window closed either way.
      notifyListeners();
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
          case MicRecordingFailure.inputNotDelivering:
            // The mic armed but never delivered a frame within the ready
            // timeout (async input-stream open failed or stalled). The take
            // was already discarded by the controller.
            _toast(
              'The microphone did not start. Try again, or pick a '
              'different input device in Settings.',
            );
        }
        return;
      }
      // Invalidated mid-await (Stop, route popped / disposed): a silent
      // no-op.
      if (!r.started) return;

      // The take is armed, which means the user can STOP it right now — every
      // await below must re-check, or a continuation resuming after the stop
      // would restart the transport against a finalized take and overwrite the
      // pre-roll the placement already consumed. The generation token (not
      // just the recording bool) closes the ABA: after this take's stop, a
      // NEW take can arm while this continuation is still parked, and
      // `isRecording` alone would read true again for the wrong take.
      takeGen = _takeGeneration;
      bool stopped() =>
          _disposed || !_mic.isRecording || _takeGeneration != takeGen;

      // Mute the take being recorded into (you can't monitor what you're laying
      // down); every other lane still obeys mute/solo. Play freely (no range wrap)
      // from the cursor as a reference.
      await _mixer.resyncPlayback();
      if (stopped()) return;
      _playback.engine.setLoop(false, 0, 0);
      await _playback.engine.seek(Duration(milliseconds: _recordingStartMs));
      if (stopped()) return;
      _playback.setRecordingPosition(Duration(milliseconds: _recordingStartMs));
      await _playback.engine.play();
      if (stopped()) {
        // A stop landed between the check above and play(): its own pause may
        // have run FIRST, in which case this play just restarted a transport
        // the finalize believes is stopped — undo it. ONLY while the transport
        // is still ours: a different generation means a NEW take owns it, and
        // pausing here would kill that take's backing.
        if (!_disposed && _takeGeneration == takeGen) {
          await _playback.engine.pause();
        }
        return;
      }
      // Pre-roll: the recording has been rolling since the arm (WAV offset 0),
      // but the backing only started just now — everything above (resync,
      // decode, seek) sat between the two. recordWrittenFrames counts exactly
      // the frames already in this take's ring, so its value at this moment
      // IS the take's file offset (engine-rate, 48 kHz) — no global-counter
      // subtraction, no ±1-callback ambiguity. The acoustic round-trip is
      // compensated separately (recordLatencyMs). Guarded: a stop that raced
      // the play() above has already read (and reset) the pre-roll, so a
      // late write here would leak into the NEXT take.
      final engineNow = _graphEngine();
      if (!stopped() && engineNow != null) {
        final pre = engineNow.recordWrittenFrames();
        if (pre > 0) _recordingPreRollMs = pre * 1000 ~/ 48000;
      }
    } catch (e) {
      // Setup failed partway (createEmptyVocalTake/save, newRecordingPath I/O,
      // playback). If OUR take armed before the throw, stop it — the WAV stays
      // on disk for the boot sweep — and surface the error so REC isn't left
      // dead-buttoned. Generation-guarded: after this take's stop a NEW take
      // can already be armed, and a bare isRecording check would stop it.
      if (_mic.isRecording && _takeGeneration == takeGen) _mic.stop();
      if (e is! _TakeSetupFailed) _toast(_friendlyError(e));
      notifyListeners();
    }
  }

  Future<void> stopRecording() async {
    // Stops the hardware + pumps and unregisters the quit finalizer; null means
    // no take was in flight. Everything up to the `_stopping` latch below is
    // synchronous, so the latch is armed before any await can yield to a
    // racing REC tap.
    final stopped = _mic.stop();
    if (stopped == null) {
      // Still arming (permission, take setup, the mic's ready handshake):
      // cancel the start, which then arms nothing and drops its file. Ignored
      // before, the take armed and rolled moments after the user's Stop.
      if (_mic.isStarting) {
        _mic.invalidatePendingStart();
        _cancellingStart = true;
        notifyListeners();
      }
      return;
    }
    _stopping = true;
    try {
      // _mic.stop() above just unregistered the armed-window quit finalizer,
      // but the take's row commits only at the end of the finalize tail below
      // — hold the quit gate over it, or a quit in this window exits mid-save
      // and boot recovery re-adopts the WAV at timeline zero, losing the
      // placement, punch-in target and latency alignment.
      // Listed per practice too: a screen reopened on it while the take is
      // still landing waits for the take (see the host's load).
      await _mic.guardStopFinalize(
        () => _finalizer.landing(_practiceId, () => _finishStop(stopped)),
      );
    } finally {
      _stopping = false;
    }
  }

  /// The tail of [stopRecording] (meter reset → pause → gated finalize), split
  /// out so the `_stopping` latch brackets it in one try/finally.
  Future<void> _finishStop(MicRecordingStop stopped) async {
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
    // The take disabled the engine's loop wrap. Re-arm it here rather than
    // relying on the finalize's save: a finalize that drops an unreadable
    // partial take returns before saving, which left the loop button lit over
    // a transport that no longer looped. (The mic stopped above, so this push
    // is no longer held off as a mid-take one.)
    _playback.ensureLoopRange();
    // Placement compensation, two independent terms:
    //  * pre-roll — the WAV has been recording since the arm, but the backing started
    //    `_recordingPreRollMs` later, so the sung content begins that far INTO the file;
    //  * record latency — the first sample sung against the backing is the response to audio
    //    the singer heard ~round-trip ago (output IO + acoustic + input IO + AEC).
    // Both shift the take EARLIER on the timeline. When the shift would land before
    // timeline zero (recording from the top — the common case), the compensation is
    // applied by CUTTING that much off the file's head instead: clamping the position
    // alone would silently discard the whole compensation and leave the take late.
    final latencyMs = _graphEngine()?.recordLatencyMs() ?? 0.0;
    final rawStartMs = (_recordingStartMs - _recordingPreRollMs - latencyMs)
        .round();
    var startMs = rawStartMs;
    if (rawStartMs < 0) {
      startMs = 0;
      try {
        await trimWavHeadInPlace(path, -rawStartMs);
      } catch (_) {
        // Best-effort: a take that can't be trimmed (unparseable partial after
        // an IO error) keeps its head and sits late by the untrimmed amount —
        // strictly better than losing the take.
      }
    }
    final targetId = _recordingTargetId;
    _recordingTargetId = null;
    _recPeaks.clear();
    // The screen can be left during the awaits above (Stop, then Back: the
    // route no longer waits once isRecording is false). The finalize below
    // still has to run, so don't touch the disposed notifier on the way.
    if (!_disposed) recordingPeaks.value = const [];
    notifyListeners();

    // Whether the finalize reached the host's save, whose resync is what
    // makes the punched take audible again (see the finally below).
    var saveRan = false;
    Future<void> finalize() async {
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
      // The writer finalized fine but the recording ring overflowed mid-take
      // (writer thread or disk stalled): the WAV is valid yet missing frames as
      // silent holes. Keep the take — most of it is fine — but never present
      // it as gapless.
      if (recordingClean && stopped.droppedFrames > 0) {
        final droppedMs = stopped.droppedFrames * 1000 ~/ 48000;
        _toast(
          'Recording dropped about $droppedMs ms of audio while the disk '
          'was busy. The take has small gaps.',
        );
      }
      final target = targetId == null ? null : _trackById(targetId);
      final String landedOn;
      if (target == null || target.kind != AudioTrackKind.vocal) {
        // No armed take → the recording becomes a new take at the cursor.
        landedOn = _appendVocal(
          path: path,
          analysis: analysis,
          startMs: startMs,
        );
        _selectRecordedClip(landedOn, null);
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
        landedOn = target.id;
      }
      await _save();
      saveRan = true;
      // Only AFTER the save: it re-reads the clip at its pooled path, which is
      // the key the analysis caches look it up by. Refreshed before it, the
      // dock analyzed the staging path nothing shows — and, computing only on
      // these pushes, then sat on "not available" for the new take while the
      // inspector (when open) analyzed the take a second time.
      _analysis.onTrackAudioChanged(landedOn);
    }

    // The gate is normally free here — every structural action pre-guards on
    // the whole take window (arm → recording → this stop) — but the finalize
    // must NEVER be dropped (the take would be stranded on disk until the next
    // boot, which re-adopts it at 0:00 without its lane or placement), so on
    // the off-chance an action still holds the gate, wait it out and retry.
    // Even once the screen is disposed: the host's save still writes after
    // dispose (it only skips the UI resync), and every gate holder finishes.
    try {
      while (!await _busyGate.run(finalize)) {
        await Future<void>.delayed(const Duration(milliseconds: 50));
      }
    } finally {
      // The take punched into was silent in the engine for the whole
      // recording, and only the save's resync lifted that. A finalize that
      // drops an unreadable partial returns before saving, and one that throws
      // (a file too short to decode) never gets there: without this the lane
      // played silent, its M button unlit, until some later edit saved.
      if (!saveRan && !_disposed) await _mixer.resyncPlayback();
    }
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
  int _lastStreamXruns = 0;
  int _inputStallTicks = 0;

  /// Watch for lost audio devices and self-heal. The playhead is driven by the output stream's
  /// callback, so if that stream dies (a Bluetooth headset flips to HFP, a device is unplugged),
  /// transport would otherwise freeze. On an output-lost event we re-open the current output.
  void _startDeviceWatch() {
    final engine = _graphEngine();
    if (engine == null) return;
    // Per-bit claims: the poll clears only the masked bits, so this watch and
    // the technique page's level tick each consume their own events instead
    // of the fragile whole-process single-poller convention.
    _deviceWatch = Timer.periodic(const Duration(seconds: 1), (_) {
      final events = engine.pollDeviceEvents(
        GraphAudioPlaybackService.eventOutputLost |
            GraphAudioPlaybackService.eventInputLost |
            GraphAudioPlaybackService.eventAecBypassed |
            GraphAudioPlaybackService.eventInputFellBack |
            GraphAudioPlaybackService.eventRecordLimit |
            GraphAudioPlaybackService.eventOutputRerouted,
      );
      // Opening the mic hands the output to the duplex unit, which can move playback off the
      // selected device without any device SET changing — so monitor safety has to be re-derived
      // on the tick, not only when refreshDevices sees a difference.
      _appSettings.reconcileMonitorSafety();
      if (events & 1 != 0) {
        // Output lost: re-open the current output so transport doesn't freeze. Deliberately the
        // REQUESTED device, not the one currently rendering: re-asking for the user's pick
        // re-attaches it if it came back, where re-asking for a fallback would strand them there.
        engine.setOutputDevice(engine.currentOutputDevice());
      }
      if (events & GraphAudioPlaybackService.eventOutputRerouted != 0) {
        // Playback is not on the device the user chose. Most often this is the mic opening: the
        // duplex unit renders through the microphone's endpoint or the system default, so the
        // backing track moves too — it is not only the monitor that is affected, and saying so is
        // the difference between "my headphones went quiet" and a mystery.
        _toast(
          'Playback moved off your selected output device while the '
          'microphone is open. Hear-yourself monitoring is muted until it '
          'returns.',
        );
      }
      if (events & 8 != 0) {
        // The selected microphone could not be opened; capture fell back to
        // the system default. The device label in Settings is now wrong for
        // this stream — silent misattribution is worse than the notice.
        _toast(
          'Your selected microphone is unavailable. '
          'Recording from the system default microphone instead.',
        );
      }
      // Streaming health: log underrun growth (never toast — brief silence
      // already told the user; the log is for diagnosing feeder timing on
      // real hardware, where offline bit-exactness can't reach).
      final xruns = engine.streamXruns();
      if (xruns > _lastStreamXruns) {
        DiagnosticsLog.instance.log(
          'engine',
          'stream underruns grew to $xruns (+${xruns - _lastStreamXruns})',
        );
        _lastStreamXruns = xruns;
      }
      if (events & 4 != 0) {
        // The mic opened WITHOUT echo cancellation (VPIO negotiated
        // mismatched element rates): capture works, but speaker playback can
        // bleed into the take. Silent degradation is worse than the notice.
        _toast(
          'Echo cancellation is unavailable for this microphone. '
          'Use headphones to keep playback out of the recording.',
        );
      }
      if (events & GraphAudioPlaybackService.eventRecordLimit != 0 &&
          isRecording) {
        // The take reached the longest recording a WAV header can describe, so
        // the engine stopped feeding the writer. Finalize now: the file on disk
        // is valid and complete up to the limit, and the recording clock would
        // otherwise keep running over audio nothing is storing.
        _toast(
          'Recording reached the six-hour maximum length. '
          'The take was saved up to that point.',
        );
        unawaited(stopRecording());
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

/// Thrown from the take setup when the empty take REC was to record into
/// could not be saved. Aborts the start silently: the failed save has already
/// told the user.
class _TakeSetupFailed implements Exception {
  const _TakeSetupFailed();
}
