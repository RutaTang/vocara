import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import '../../../data/repositories/song_practice_repository.dart';
import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/app_settings_service.dart';
import '../../../data/services/audio_edit_service.dart';
import '../../../data/services/audio_file_service.dart';
import '../../../data/services/recording_finalizer.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/mic_permission.dart';
import '../../../data/services/playback_engine.dart';
import '../../../data/services/playback_engine_factory.dart';
import '../../../domain/models/audio_fx.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/source_separation_service.dart';
import '../../../data/services/spectrogram_service.dart';
import '../../../data/services/waveform_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/lyric_line.dart';
import '../../../domain/models/practice_note.dart';
import '../../../domain/models/song_practice.dart';
import '../../../domain/models/take_version.dart';
import '../../../domain/models/technique.dart';
import '../../../domain/models/technique_marker.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/command.dart';
import '../../../utils/id.dart';
import 'analysis_source.dart';
import '../../../utils/lyrics_format.dart';
import '../../../utils/result.dart';

/// The tabs of the collapsible bottom dock — one analysis feature each, plus
/// the mixer. The spectral ones can also show the live mic.
enum DockTab {
  analysisSummary,
  spectrogram,
  power,
  ltas,
  vowel,
  pitch,
  vibrato,
  timbre,
  registers,
  range,
  dynamics,
  mixer,
}

/// The inline analysis sub-lanes a vocal take can fold out (Logic-style).
enum AnalysisLane { pitch, loudness, spectrogram, power, notes, technique }

/// The two tabs of the right-side panel.
enum RightTab { lyrics, notes, techniques }

/// The lyrics panel's interaction mode.
enum LyricsMode { read, edit, sync }

/// Whether a track is heard given its mute/solo and whether any track is
/// soloed. Mute *always* silences a track; solo only isolates which tracks can
/// be heard — so muting a soloed track still silences it (matches user intent
/// and avoids "M does nothing while a solo is active").
@visibleForTesting
bool computeAudible({
  required bool soloed,
  required bool muted,
  required bool anySolo,
}) => (anySolo ? soloed : true) && !muted;

/// A note/technique range drag shorter than this (ms) collapses to a point — so
/// a stray wobble while tapping doesn't create a tiny range.
const int kMinMarkerRangeMs = 120;

/// Orders [startMs]/[endMs] and drops a null or too-short span to a point
/// marker (endMs null). Returns the normalized (start, end?), with start never
/// negative. Pure — unit-tested.
@visibleForTesting
(int, int?) normalizeMarkerRange(int startMs, int? endMs) {
  if (endMs == null) return (startMs < 0 ? 0 : startMs, null);
  var s = startMs < endMs ? startMs : endMs;
  final e = startMs < endMs ? endMs : startMs;
  if (s < 0) s = 0;
  if (e - s < kMinMarkerRangeMs) return (s, null);
  return (s, e);
}

/// Clamps a note's range so its span doesn't extend past [boundaryMs] (e.g. a
/// split point or a deleted clip's edge), collapsing to a point when the clamped
/// span is shorter than the minimum. A point note is returned unchanged.
@visibleForTesting
PracticeNote clampNoteEnd(PracticeNote n, int boundaryMs) {
  final end = n.endMs;
  if (end == null || end <= boundaryMs) return n;
  return boundaryMs - n.positionMs < kMinMarkerRangeMs
      ? n.copyWith(clearEnd: true)
      : n.copyWith(endMs: boundaryMs);
}

/// Like [clampNoteEnd], for a technique marker.
@visibleForTesting
TechniqueMarker clampMarkerEnd(TechniqueMarker m, int boundaryMs) {
  final end = m.endMs;
  if (end == null || end <= boundaryMs) return m;
  return boundaryMs - m.positionMs < kMinMarkerRangeMs
      ? m.copyWith(clearEnd: true)
      : m.copyWith(endMs: boundaryMs);
}

/// The resolved solo state of the whole mix: which tracks and buses are part of
/// the current solo. When [anySolo] is false (the `.all()` case) every source is
/// "in solo" and only its own mute matters. Built by [resolveSolo].
@visibleForTesting
class SoloResolution {
  const SoloResolution(this._tracks, this._buses) : anySolo = true;
  const SoloResolution.all()
    : _tracks = const {},
      _buses = const {},
      anySolo = false;

  final Set<String> _tracks;
  final Set<String> _buses;
  final bool anySolo;

  bool trackInSolo(String id) => !anySolo || _tracks.contains(id);
  bool busInSolo(String id) => !anySolo || _buses.contains(id);
}

/// Resolves the whole mix's solo state in one pass. Solo is a ViewModel concept
/// the engine only ever sees as per-source mutes: with nothing soloed everything
/// (un-muted) plays; once any track *or* bus is soloed, only the soloed sources
/// and the buses that route them to the master stay audible. Crucially, soloing
/// a bus keeps the takes feeding it audible — otherwise the bus would have no
/// input and the solo would be silent.
///
/// Routing follows BOTH the main output (busId → outputBusId) and post-fader aux
/// sends (track/bus `sends`). A reverb / return bus reached only by a send is
/// part of the path, so soloing a take keeps its reverb tail and soloing a
/// send-fed return keeps its feeders audible (a muted source zeroes its sends
/// too, so the return would otherwise go silent). Pure — so it's unit-tested.
@visibleForTesting
SoloResolution resolveSolo(List<AudioTrack> tracks, List<FxBus> buses) {
  final soloedBusIds = {
    for (final b in buses)
      if (b.soloed) b.id,
  };
  final anySolo = tracks.any((t) => t.soloed) || soloedBusIds.isNotEmpty;
  if (!anySolo) return const SoloResolution.all();

  final busById = {for (final b in buses) b.id: b};
  // Every bus a signal reaches from its main output [startBusId] and [sends],
  // following bus outputs and further aux sends onward to the master. A visited
  // set guards routing cycles.
  Set<String> reachableBuses(String? startBusId, List<TrackSend> sends) {
    final result = <String>{};
    final queue = <String>[];
    void visit(String? id) {
      if (id != null && busById.containsKey(id) && result.add(id)) {
        queue.add(id);
      }
    }

    visit(startBusId);
    for (final s in sends) {
      visit(s.busId);
    }
    while (queue.isNotEmpty) {
      final b = busById[queue.removeLast()]!;
      visit(b.outputBusId);
      for (final s in b.sends) {
        visit(s.busId);
      }
    }
    return result;
  }

  final audibleTracks = <String>{};
  final audibleBuses = <String>{};
  for (final t in tracks) {
    final reach = reachableBuses(t.busId, t.sends);
    final feedsSoloedBus = reach.any(soloedBusIds.contains);
    // Reuse the pure track rule, treating "feeds a soloed bus" as soloed.
    if (computeAudible(
      soloed: t.soloed || feedsSoloedBus,
      muted: false,
      anySolo: true,
    )) {
      audibleTracks.add(t.id);
      audibleBuses.addAll(reach); // carry this source (dry + sends) onward
    }
  }
  // A soloed bus is heard itself plus everything it routes into, even with no
  // source feeding it yet.
  for (final id in soloedBusIds) {
    final b = busById[id];
    audibleBuses.add(id);
    if (b != null) audibleBuses.addAll(reachableBuses(b.outputBusId, b.sends));
  }
  return SoloResolution(audibleTracks, audibleBuses);
}

/// ViewModel for the multi-track detail view. Owns the (stateful) playback and
/// recording services for this view's lifetime and disposes them on tear-down;
/// reads the stateless file/waveform services. The shared playhead position
/// and live record level are exposed as [ValueNotifier]s so only those widgets
/// rebuild on every tick.
class SongPracticeDetailViewModel extends ChangeNotifier
    implements AnalysisSource {
  SongPracticeDetailViewModel({
    required SongPracticeRepository repository,
    required TechniqueRepository techniqueRepository,
    required AudioFileService audioFileService,
    required WaveformService waveformService,
    required SpectrogramService spectrogramService,
    required SourceSeparationService separationService,
    required AppSettingsService appSettings,
    required String practiceId,
  }) : _repository = repository,
       _techniqueRepository = techniqueRepository,
       _audioFiles = audioFileService,
       _waveforms = waveformService,
       _spectrograms = spectrogramService,
       _separation = separationService,
       _appSettings = appSettings,
       _practiceId = practiceId {
    // Adopt the persisted output-device choice and let the global settings panel drive this
    // engine live. The native engine is the only one with device support.
    if (_playback is GraphAudioPlaybackService) {
      _appSettings.attachEngine(_playback);
    }
    _startDeviceWatch();
    // Finalize an in-flight take if the app is quit mid-record (the callback is a
    // no-op when not recording). Unregistered in dispose.
    RecordingFinalizer.instance.register(_quitFinalize);
    _playingSub = _playback.playing.listen((playing) {
      _isPlaying = playing;
      notifyListeners();
    });
    _positionSub = _playback.position.listen((d) {
      // While recording, the playhead follows the recording clock instead.
      if (!_isRecording) {
        position.value = d;
        _maybeFollowCursor(d.inMilliseconds);
      }
    });
    load = Command0(_load)..execute();
    // Probe the separation engine once so the UI can hide "Separate" when no
    // engine is present (e.g. the dev sidecar isn't installed).
    _separation.isAvailable().then((available) {
      _separationAvailable = available;
      notifyListeners();
    });
  }

  final SongPracticeRepository _repository;
  final TechniqueRepository _techniqueRepository;
  final AudioFileService _audioFiles;
  final WaveformService _waveforms;
  final SpectrogramService _spectrograms;
  final SourceSeparationService _separation;
  final AppSettingsService _appSettings;

  /// The global audio settings (devices, record/monitor gains) — used by the mixer's mic strip.
  AppSettingsService get appSettings => _appSettings;
  final String _practiceId;
  String get practiceId => _practiceId;

  final PlaybackEngine _playback = createPlaybackEngine();
  late final AudioEditService _edits = AudioEditService(files: _audioFiles);
  final MonitorService _monitor = MonitorService();
  @override
  MonitorService get monitor => _monitor;

  late final Command0<SongPractice> load;

  /// Master-clock playhead position. Drives the playhead overlay only.
  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  /// Live input level (0..1) while recording. Drives the meter only.
  final ValueNotifier<double> recordLevel = ValueNotifier(0);

  /// Live input peak (0..1) while input monitoring is on. Drives the monitor meter.
  final ValueNotifier<double> inputLevel = ValueNotifier(0);

  /// One-shot user feedback (errors, guards, confirmations), shown by the view
  /// as a transient toast.
  final StreamController<String> _toasts = StreamController<String>.broadcast();
  Stream<String> get toasts => _toasts.stream;

  void _toast(String message) {
    if (!_toasts.isClosed) _toasts.add(message);
  }

  /// Set in [dispose]. Guards fire-and-forget async callbacks that can resume
  /// after the view model is gone (the separation probe resolving, a save
  /// finishing while the route is popped mid-record) from touching a disposed
  /// notifier or value notifier.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  String _friendlyError(Object e) =>
      e is FormatException ? e.message.toString() : 'Something went wrong.';

  StreamSubscription<Duration>? _positionSub;
  StreamSubscription<bool>? _playingSub;
  // Polls the engine input level/meter while recording; replaces the old record stream.
  Timer? _recLevelTimer;
  // Pulls captured frames from the engine into the live-analysis monitor.
  Timer? _captureTimer;

  SongPractice? _practice;
  SongPractice? get practice => _practice;

  bool _isPlaying = false;
  bool get isPlaying => _isPlaying;

  bool _isRecording = false;
  bool get isRecording => _isRecording;

  // Synchronous re-entrancy guard: set the instant a start is requested, before
  // the permission/engine awaits (so _isRecording isn't true yet). Without it a
  // fast double-tap starts two recordings — two empty takes + a leaked timer.
  bool _startingRecording = false;

  // Registered with RecordingFinalizer so an in-flight take is saved if the app
  // is quit mid-record. No-op unless actually recording. (A method tear-off is a
  // stable reference, so register/unregister match.)
  Future<void> _quitFinalize() async {
    if (_isRecording) await stopRecording();
  }

  bool _inputMonitorOn = false;
  // Synchronous re-entrancy guard for toggleInputMonitor (its ON branch awaits a permission
  // round-trip, during which a second tap must not arm monitoring twice and orphan a Timer).
  bool _togglingMonitor = false;

  /// Whether the singer hears themselves through the engine (live input monitoring).
  bool get inputMonitorEnabled => _inputMonitorOn;
  Timer? _inputLevelTimer;
  // Peak-hold state for the monitor meter so brief/low vocal peaks stay visible (decays smoothly).
  double _heldInputLevel = 0;
  Timer? _deviceWatch;
  // Input-stall watchdog: macOS VPIO raises no device-loss event, so the device-watch also
  // treats a non-advancing input frame counter (while capturing) as a lost input.
  int _lastInputFrames = 0;
  int _inputStallTicks = 0;

  /// The native engine, when it's the active backend (only it supports device I/O + monitoring).
  GraphAudioPlaybackService? get _graphEngine {
    final p = _playback;
    return p is GraphAudioPlaybackService ? p : null;
  }

  /// Watch for lost audio devices and self-heal. The playhead is driven by the output stream's
  /// callback, so if that stream dies (a Bluetooth headset flips to HFP, a device is unplugged),
  /// transport would otherwise freeze. On an output-lost event we re-open the current output.
  void _startDeviceWatch() {
    final engine = _graphEngine;
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
      } else if (_isRecording || _inputMonitorOn || _liveMonitor) {
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
    final wasRecording = _isRecording;
    final wasMonitoring = _inputMonitorOn;
    final wasLive = _liveMonitor;
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
    if (wasLive) unawaited(setLiveMonitor(false));
  }

  /// Map a linear input peak (0..1) to a perceptual meter fill (0..1) on a dB scale
  /// (−50 dBFS → empty, 0 dBFS → full), so normal vocal levels read clearly instead of as a
  /// tiny linear sliver.
  static double _meterFromPeak(double peak) {
    if (peak <= 1e-6) return 0;
    final db = 20 * (math.log(peak) / math.ln10);
    return ((db + 50) / 50).clamp(0.0, 1.0);
  }

  /// Toggle live input monitoring — open/close the mic stream and ramp the monitor in/out, and
  /// poll the input level for the meter. No-op on engines without monitoring.
  Future<void> toggleInputMonitor() async {
    final engine = _graphEngine;
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
          // While a take is recording, the record-level timer owns the shared
          // inputLevel meter (it writes the instantaneous peak at the same 50ms
          // cadence). Yield so the two timers don't fight and flicker the mixer
          // MIC meter; this timer keeps running and reclaims the meter the moment
          // recording stops (stopRecording only clears it when monitoring is off).
          if (_isRecording) return;
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

  bool _busy = false;
  bool get busy => _busy;

  /// 0..1 while a stem separation runs; null when idle. Drives a progress bar —
  /// separation can take minutes, unlike the ~1s import busy spinner.
  double? _separationProgress;
  double? get separationProgress => _separationProgress;
  bool get isSeparating => _separationProgress != null;

  /// Whether the separation engine is usable; gates the "Separate" action.
  bool _separationAvailable = false;
  bool get separationAvailable => _separationAvailable;

  String? _errorMessage;
  String? get errorMessage => _errorMessage;

  String? _recordingPath;

  static const int _recordIntervalMs = 50;
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

  String? _selectedTrackId;
  String? get selectedTrackId => _selectedTrackId;

  /// The selected clip within the selected lane (for per-clip actions like
  /// delete). Null when the lane is selected via its header rather than a clip.
  String? _selectedClipId;
  String? get selectedClipId => _selectedClipId;

  void selectTrack(String trackId) {
    if (_selectedTrackId == trackId &&
        _selectedClipId == null &&
        _selectedBusId == null) {
      return;
    }
    _selectedTrackId = trackId;
    _selectedClipId = null;
    _selectedBusId = null;
    notifyListeners();
    // The dock and the left inspector both read the selected take's analysis.
    if ((_dockOpen && _dockShowsTake) || _inspectorOpen) {
      ensureSelectedSpectrogram();
    }
  }

  /// Selects a specific clip (and its lane), so per-clip actions target it.
  void selectClip(String trackId, String clipId) {
    if (_selectedTrackId == trackId &&
        _selectedClipId == clipId &&
        _selectedBusId == null) {
      return;
    }
    _selectedTrackId = trackId;
    _selectedClipId = clipId;
    _selectedBusId = null;
    notifyListeners();
    if ((_dockOpen && _dockShowsTake) || _inspectorOpen) {
      ensureSelectedSpectrogram();
    }
  }

  /// The take REC will record into: the selected lane if it's a recordable vocal
  /// take in the active interpretation, else null (→ REC creates a new take).
  /// Drives the "armed" record-dot; frozen into [_recordingTargetId] while a
  /// recording is in flight.
  String? get recordTargetId {
    if (_isRecording) return _recordingTargetId;
    final id = _selectedTrackId;
    if (id == null) return null;
    final t = _trackById(id);
    if (t == null || t.kind != AudioTrackKind.vocal) return null;
    if (t.versionId != _practice?.activeVersionId) return null;
    return id;
  }

  // --- Bottom dock (Voiceprint + Mixer) ----------------------------------

  // Defaults closed; the dart-define is a dev hook for previewing the dock.
  bool _dockOpen = const bool.fromEnvironment('VOICEPRINT_DEMO');
  bool get dockOpen => _dockOpen;

  DockTab _dockTab = DockTab.analysisSummary;
  DockTab get dockTab => _dockTab;

  /// Every analysis tab (all but Mixer) reads the selected take's
  /// spectrogram/metrics, so opening one computes them.
  bool get _dockShowsTake => _dockTab != DockTab.mixer;

  /// The spectral/pitch/timbre tabs can render the live mic; range/dynamics
  /// need a whole recording and the mixer isn't a view of the source.
  bool _tabSupportsLive(DockTab t) =>
      t == DockTab.spectrogram ||
      t == DockTab.power ||
      t == DockTab.ltas ||
      t == DockTab.vowel ||
      t == DockTab.pitch ||
      t == DockTab.vibrato ||
      t == DockTab.timbre ||
      t == DockTab.registers;

  /// Whether the active tab can show the live mic (drives the Take/Live toggle).
  bool get activeTabSupportsLive => _tabSupportsLive(_dockTab);

  static const double _minDockHeight = 160;
  static const double _maxDockHeight = 760;
  double _dockHeight = 300;
  double get dockHeight => _dockHeight;

  /// Resizes the dock by dragging its top edge (clamped to a sane range).
  void setDockHeight(double height) {
    final clamped = height.clamp(_minDockHeight, _maxDockHeight);
    if (clamped == _dockHeight) return;
    _dockHeight = clamped;
    notifyListeners();
  }

  // --- Backing source (accompaniment ⇄ reference) -------------------------

  bool _playReference = false;

  /// When true the reference (original song) is the audible backing instead of
  /// the accompaniment — a seamless A/B; the playhead is unaffected.
  bool get playReference => _playReference;

  /// The backing track currently selected for playback and display.
  AudioTrack? get activeBacking =>
      _playReference ? _practice?.reference : _practice?.accompaniment;

  void setPlayReference(bool value) {
    if (_playReference == value) return;
    _playReference = value;
    _applyAudibility();
    notifyListeners();
  }

  void toggleBacking() => setPlayReference(!_playReference);

  // --- Interpretations (vocal-take versions) ------------------------------

  /// All interpretations of this song, in creation order.
  List<TakeVersion> get interpretations => _practice?.versions ?? const [];

  /// The interpretation currently shown and played.
  TakeVersion? get activeInterpretation => _practice?.activeVersion;

  /// Vocal takes belonging to the active interpretation — the only takes shown
  /// in the lane list and loaded into playback.
  List<AudioTrack> get vocalTakes => _practice?.activeVocalTracks ?? const [];

  /// Switches which interpretation is shown/played; the other interpretations'
  /// takes stop playing. Because the loaded track set changes, this is a pause →
  /// reload → resume (playback continues from the same spot if it was playing).
  Future<void> setActiveInterpretation(String versionId) async {
    final practice = _practice;
    if (practice == null || practice.activeVersionId == versionId) return;
    if (_isRecording) {
      _toast('Stop recording to switch interpretation.');
      return;
    }
    if (!practice.versions.any((v) => v.id == versionId)) return;
    final wasPlaying = _playback.isPlaying;
    await _playback.pause();
    _practice = practice.copyWith(activeVersionId: versionId);
    _ensureSelection();
    await _save();
    if (_dockOpen && _dockShowsTake) {
      unawaited(ensureSelectedSpectrogram());
    }
    if (wasPlaying) await _playback.play();
  }

  /// Creates a new (empty) interpretation and switches to it, ready to record a
  /// fresh emotional/technical reading of the song.
  Future<void> createInterpretation([String name = '']) async {
    final practice = _practice;
    if (practice == null) return;
    if (_isRecording) {
      _toast('Stop recording to add an interpretation.');
      return;
    }
    final trimmed = name.trim();
    final version = TakeVersion(
      id: newId(),
      name: trimmed.isEmpty
          ? 'Interpretation ${practice.versions.length + 1}'
          : trimmed,
      createdAt: DateTime.now(),
    );
    await _playback.pause();
    _practice = practice.copyWith(
      versions: [...practice.versions, version],
      activeVersionId: version.id,
    );
    _ensureSelection();
    await _save();
  }

  Future<void> renameInterpretation(String versionId, String name) async {
    final practice = _practice;
    final trimmed = name.trim();
    if (practice == null || trimmed.isEmpty) return;
    _practice = practice.copyWith(
      versions: [
        for (final v in practice.versions)
          if (v.id == versionId) v.copyWith(name: trimmed) else v,
      ],
    );
    await _save();
  }

  /// Deletes an interpretation and every take in it (audio files included).
  /// Refuses to remove the last one — a practice always keeps one.
  Future<void> deleteInterpretation(String versionId) async {
    final practice = _practice;
    if (practice == null) return;
    if (_isRecording) {
      _toast('Stop recording to delete an interpretation.');
      return;
    }
    if (_busy || isSeparating) return; // no concurrent structural edit
    if (practice.versions.length <= 1) {
      _toast('Keep at least one interpretation.');
      return;
    }
    await _runBusy(() async {
      await _playback.pause();
      // Re-read the LIVE practice after the pause await so a take recorded into a
      // DIFFERENT interpretation meanwhile isn't dropped by a stale-snapshot
      // rebuild; re-check the guard too (it may have changed during the await).
      final cur = _practice;
      if (cur == null || cur.versions.length <= 1) return;
      final doomed = cur.vocalTracks
          .where((t) => t.versionId == versionId)
          .toList();
      final remaining = cur.versions.where((v) => v.id != versionId).toList();
      if (remaining.isEmpty) return; // the target was the only one left
      _practice = cur.copyWith(
        versions: remaining,
        activeVersionId: cur.activeVersionId == versionId
            ? remaining.first.id
            : cur.activeVersionId,
        vocalTracks: cur.vocalTracks
            .where((t) => t.versionId != versionId)
            .toList(),
      );
      _ensureSelection();
      // Persist the dropped state BEFORE deleting files, and ONLY delete when the
      // save succeeded — a failed save reverts the model back to referencing
      // these takes, so their audio must survive (deleteAudio already refuses
      // pooled files; this gate also covers any not-yet-pooled working file).
      if (await _save()) {
        for (final t in doomed) {
          _expandedTracks.remove(t.id);
          _trackLanes.remove(t.id);
          // Delete EVERY clip's file, not just the primary — a multi-clip take
          // would otherwise orphan its trailing clips' audio.
          for (final c in t.clips) {
            _evictAnalysis(c.audioFilePath);
            await _audioFiles.deleteAudio(c.audioFilePath);
          }
        }
      }
    });
  }

  // --- Lyrics --------------------------------------------------------------

  List<LyricLine> get lyrics => _practice?.lyrics ?? const [];
  bool get hasLyrics => lyrics.isNotEmpty;
  bool get lyricsSynced => lyrics.any((l) => l.isSynced);

  bool _lyricsOpen = true;
  bool get lyricsOpen => _lyricsOpen;
  void toggleLyricsPanel() {
    _lyricsOpen = !_lyricsOpen;
    notifyListeners();
  }

  // --- Left inspector (selected-take summary) ------------------------------

  bool _inspectorOpen = true;
  bool get inspectorOpen => _inspectorOpen;
  void toggleInspectorPanel() {
    _inspectorOpen = !_inspectorOpen;
    notifyListeners();
    // The inspector shows analysis, so opening it must trigger the compute the
    // dock would otherwise be responsible for.
    if (_inspectorOpen) ensureSelectedSpectrogram();
  }

  RightTab _rightTab = RightTab.lyrics;
  RightTab get rightTab => _rightTab;
  void setRightTab(RightTab tab) {
    if (_rightTab == tab) return;
    _rightTab = tab;
    notifyListeners();
  }

  LyricsMode _lyricsMode = LyricsMode.read;
  LyricsMode get lyricsMode => _lyricsMode;
  void setLyricsMode(LyricsMode mode) {
    if (_lyricsMode == mode) return;
    _lyricsMode = mode;
    if (mode == LyricsMode.sync) _syncIndex = _firstUnsyncedIndex();
    notifyListeners();
  }

  bool _lyricsFocus = false;
  bool get lyricsFocus => _lyricsFocus;
  void setLyricsFocus(bool on) {
    if (_lyricsFocus == on) return;
    _lyricsFocus = on;
    notifyListeners();
  }

  /// Index of the line highlighted at [positionMs] (the synced line with the
  /// LATEST time at or before the playhead), or -1 if none. Picking by max time
  /// rather than highest index keeps the karaoke highlight correct even if the
  /// user stamped lines out of time order (the times need not be monotonic with
  /// the line order).
  int currentLyricIndex(int positionMs) {
    var idx = -1;
    var best = -1;
    for (var i = 0; i < lyrics.length; i++) {
      final t = lyrics[i].timeMs;
      if (t != null && t <= positionMs && t >= best) {
        best = t;
        idx = i;
      }
    }
    return idx;
  }

  void seekToLyric(LyricLine line) {
    final t = line.timeMs;
    if (t != null) unawaited(seek(Duration(milliseconds: t)));
  }

  /// Replaces lyrics from edited plain text, keeping existing line times by
  /// MATCHING TEXT (not position): inserting, deleting, or reordering a line in
  /// the editor must not pin an old timestamp onto different words — or onto a
  /// blank line. Each old synced time is consumed at most once, in order, when
  /// its trimmed text matches a new line.
  Future<void> setLyricsText(String text) async {
    final parsed = parsePlainLyrics(text);
    final byText = <String, List<int>>{};
    for (final l in lyrics) {
      final t = l.timeMs;
      if (t == null) continue;
      (byText[l.text.trim()] ??= <int>[]).add(t);
    }
    final next = <LyricLine>[];
    for (final line in parsed) {
      final key = line.text.trim();
      final q = byText[key];
      if (key.isNotEmpty && q != null && q.isNotEmpty) {
        next.add(line.copyWith(timeMs: q.removeAt(0)));
      } else {
        next.add(line); // parsed plain line carries no time
      }
    }
    await _setLyrics(next);
  }

  /// Imports pasted/dropped lyrics — `.lrc` if it has time tags, else plain.
  Future<void> importLyrics(String content) async {
    final lines = looksLikeLrc(content)
        ? parseLrc(content)
        : parsePlainLyrics(content);
    if (lines.every((l) => l.isBlank)) {
      _toast("That file didn't contain any lyrics.");
      return;
    }
    await _setLyrics(lines);
  }

  /// Imports lyrics from a DROPPED file: validates the type + size, reads, and
  /// replaces the lyrics — with feedback on every failure path so the drop never
  /// silently does nothing, OOMs on a huge file, or overwrites the lyrics with
  /// the contents of an unrelated file.
  Future<void> importLyricsFromFile(String path) async {
    if (_isRecording || _busy) {
      _toast('Finish the current action before importing lyrics.');
      return;
    }
    const exts = {'.lrc', '.txt', '.text'};
    final dot = path.lastIndexOf('.');
    final ext = dot < 0 ? '' : path.substring(dot).toLowerCase();
    if (!exts.contains(ext)) {
      _toast('Drop a .lrc or .txt lyrics file.');
      return;
    }
    try {
      final file = File(path);
      if (await file.length() > 512 * 1024) {
        _toast('That file is too large to be lyrics.');
        return;
      }
      await importLyrics(await file.readAsString());
    } catch (_) {
      _toast("Couldn't read that file as lyrics.");
    }
  }

  /// Applies text saved from the editor: parsed as `.lrc` if it carries time
  /// tags (so pasting LRC just works), otherwise as plain text (keeping times).
  Future<void> applyEditedLyrics(String text) async {
    if (looksLikeLrc(text)) {
      await importLyrics(text);
    } else {
      await setLyricsText(text);
    }
  }

  String lyricsAsLrc() => toLrc(lyrics);
  String lyricsAsText() => lyricsToPlainText(lyrics);

  // Tap-to-time sync.
  int _syncIndex = 0;
  int get syncIndex => _syncIndex;

  int _firstUnsyncedIndex() {
    for (var i = 0; i < lyrics.length; i++) {
      if (!lyrics[i].isBlank && !lyrics[i].isSynced) return i;
    }
    return 0;
  }

  /// Stamps the next line at the playhead, then advances past blank lines.
  Future<void> stampNextLyric() async {
    if (_syncIndex < 0 || _syncIndex >= lyrics.length) return;
    await stampLyricAt(_syncIndex);
    var next = _syncIndex + 1;
    while (next < lyrics.length && lyrics[next].isBlank) {
      next++;
    }
    _syncIndex = next;
    notifyListeners();
  }

  /// Stamps a specific line at the current playhead.
  Future<void> stampLyricAt(int index) async {
    if (index < 0 || index >= lyrics.length) return;
    final at = position.value.inMilliseconds;
    _syncIndex = index;
    await _setLyrics([
      for (var i = 0; i < lyrics.length; i++)
        if (i == index) lyrics[i].copyWith(timeMs: at) else lyrics[i],
    ]);
  }

  Future<void> clearLyricTimes() async {
    _syncIndex = 0;
    await _setLyrics([for (final l in lyrics) l.copyWith(clearTime: true)]);
  }

  /// Persists a lyrics change without re-syncing the audio engine (lyrics don't
  /// affect playback), so tap-to-time while playing doesn't glitch the audio.
  Future<void> _setLyrics(List<LyricLine> lines) async {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(lyrics: lines);
    final result = await _repository.savePractice(_practice!);
    if (result is Ok<SongPractice>) _practice = result.value;
    notifyListeners();
  }

  final Map<String, Spectrogram> _spectrogramCache = {};
  final Map<String, VoiceMetrics> _metricsCache = {};
  // Derived voice profile (range/passaggio/dynamics) per take path, memoized so
  // it isn't recomputed on every unrelated rebuild.
  final Map<String, VoiceProfile> _profileCache = {};
  // Long-term average spectrum + register map per take path, memoized likewise.
  final Map<String, Ltas> _ltasCache = {};
  final Map<String, RegisterMap> _registerCache = {};
  final Set<String> _spectrogramLoading = {};
  final Set<String> _metricsLoading = {};
  // Paths whose compute finished but produced nothing (non-WAV imports, missing
  // or truncated files). Latched so a null result is never retried — otherwise
  // the always-on inspector would re-launch the read + isolate every rebuild.
  final Set<String> _spectrogramUnavailable = {};
  final Set<String> _metricsUnavailable = {};
  // Whole-lane aggregate analysis for multi-clip takes: each clip's per-path
  // result merged in timeline order, keyed by [aggregateKeyFor] (which embeds
  // the ordered clip ids, so the key self-invalidates when the clip set
  // changes). Only multi-clip takes ever populate these.
  final Map<String, Spectrogram> _aggSpectrogramCache = {};
  final Map<String, VoiceMetrics> _aggMetricsCache = {};
  final Map<String, VoiceProfile> _aggProfileCache = {};
  final Map<String, Ltas> _aggLtasCache = {};
  final Map<String, RegisterMap> _aggRegisterCache = {};
  final Set<String> _aggLoading = {};
  final Set<String> _aggUnavailable = {};
  // Full-range (to ~18 kHz) display spectrogram per clip file, for the FX EQ's
  // input/output spectrum analyzer. Kept separate from the 5 kHz analysis
  // spectrogram, which is tuned for voice metrics rather than a wide display.
  static const double _eqDisplayMaxHz = 18000;
  final Map<String, Spectrogram> _eqSpectroCache = {};
  final Set<String> _eqSpectroLoading = {};
  final Set<String> _eqSpectroUnavailable = {};
  // Inline analysis sub-lanes (view state, not persisted): which tracks are
  // folded open, and which lanes each shows.
  final Set<String> _expandedTracks = {};
  final Map<String, Set<AnalysisLane>> _trackLanes = {};

  void toggleDock() => setDockOpen(!_dockOpen);

  void setDockOpen(bool open) {
    if (_dockOpen == open) return;
    _dockOpen = open;
    notifyListeners();
    if (open && _dockShowsTake) {
      ensureSelectedSpectrogram();
    } else if (!open && _liveMonitor) {
      unawaited(setLiveMonitor(false));
    }
  }

  /// Opens the dock on [tab] (switching if needed). Switching to a tab that
  /// can't show the live mic stops it.
  void showDockTab(DockTab tab) {
    _dockTab = tab;
    _dockOpen = true;
    notifyListeners();
    if (_dockShowsTake) {
      ensureSelectedSpectrogram();
    }
    if (!activeTabSupportsLive && _liveMonitor) {
      unawaited(setLiveMonitor(false));
    }
  }

  // --- Live monitor (real-time spectrogram) ------------------------------

  bool _liveMonitor = false;
  @override
  bool get liveMonitor => _liveMonitor;

  /// Starts/stops feeding the engine's live input into the spectrogram analyzer. Can't run while
  /// recording (the capture is busy with the take); pauses playback when starting.
  Future<void> setLiveMonitor(bool on) async {
    if (_liveMonitor == on) return;
    final engine = _graphEngine;
    if (on) {
      if (_isRecording || engine == null) return;
      if (_isPlaying) await _playback.pause();
      engine.startCapture();
      _monitor.start();
      _liveMonitor = true;
      // Two starts can race across the pause() await above; cancel any prior
      // timer so the first isn't orphaned (it would keep pulling input forever).
      _captureTimer?.cancel();
      _captureTimer = Timer.periodic(const Duration(milliseconds: 30), (_) {
        final frames = engine.pullInputFrames(); // interleaved stereo
        if (frames.isEmpty) return;
        final n = frames.length ~/ 2;
        final mono = Float64List(n);
        for (var i = 0; i < n; i++) {
          mono[i] = (frames[2 * i] + frames[2 * i + 1]) * 0.5;
        }
        _monitor.feedSamples(mono);
      });
    } else {
      _captureTimer?.cancel();
      _captureTimer = null;
      engine?.stopCapture();
      _monitor.stop();
      _liveMonitor = false;
    }
    notifyListeners();
  }

  @override
  AudioTrack? get selectedTrack {
    final id = _selectedTrackId;
    return id == null ? null : _trackById(id);
  }

  /// Cache key for a take's whole-lane aggregate analysis. Embeds the ordered
  /// clip ids (with an `agg:` prefix that no file path uses) so the key
  /// self-invalidates whenever the clip set changes.
  String aggregateKeyFor(AudioTrack track) =>
      'agg:${track.id}#${track.clips.map((c) => c.id).join(',')}';

  /// The subject the inspector analyzes for the current selection: a single
  /// selected clip (analyze that one file), or — when the lane is selected via
  /// its header — the whole take, which is just the one clip for a single-clip
  /// take and the aggregate across all clips for a multi-clip take. Null for no
  /// selection, a backing track, or an empty take. [aggregate] picks which cache
  /// family [key] lives in.
  ({String key, bool aggregate})? get _analysisSubject {
    final track = selectedTrack;
    if (track == null || track.isBacking || track.clips.isEmpty) return null;
    final clipId = _selectedClipId;
    if (clipId != null) {
      final clip = track.clipById(clipId);
      if (clip != null) return (key: clip.audioFilePath, aggregate: false);
    }
    if (track.clips.length == 1) {
      return (key: track.clips.first.audioFilePath, aggregate: false);
    }
    return (key: aggregateKeyFor(track), aggregate: true);
  }

  /// The computed spectrogram for the analysis subject, if ready.
  @override
  Spectrogram? get selectedSpectrogram {
    final s = _analysisSubject;
    if (s == null) return null;
    return s.aggregate ? _aggSpectrogramCache[s.key] : _spectrogramCache[s.key];
  }

  /// The computed voice-timbre metrics (ring/tilt/CPP/formants) for the analysis
  /// subject, if ready. Computed alongside [selectedSpectrogram].
  @override
  VoiceMetrics? get selectedMetrics {
    final s = _analysisSubject;
    if (s == null) return null;
    return s.aggregate ? _aggMetricsCache[s.key] : _metricsCache[s.key];
  }

  /// True when the subject's metrics compute already ran and produced nothing
  /// (e.g. a non-WAV import). The inspector uses this to stop retrying.
  bool get selectedMetricsUnavailable {
    final s = _analysisSubject;
    if (s == null) return false;
    return s.aggregate
        ? _aggUnavailable.contains(s.key)
        : _metricsUnavailable.contains(s.key);
  }

  /// The voice profile (range / passaggio / dynamics) for the analysis subject,
  /// derived from [selectedMetrics] and memoized.
  @override
  VoiceProfile? get selectedProfile {
    final s = _analysisSubject;
    if (s == null) return null;
    final m = s.aggregate ? _aggMetricsCache[s.key] : _metricsCache[s.key];
    if (m == null) return null;
    final cache = s.aggregate ? _aggProfileCache : _profileCache;
    return cache[s.key] ??= analyzeVoiceProfile(m);
  }

  /// The long-term average spectrum for the analysis subject (needs both the
  /// spectrogram and metrics); memoized.
  @override
  Ltas? get selectedLtas {
    final s = _analysisSubject;
    if (s == null) return null;
    final spec = s.aggregate
        ? _aggSpectrogramCache[s.key]
        : _spectrogramCache[s.key];
    if (spec == null) return null;
    final m = s.aggregate ? _aggMetricsCache[s.key] : _metricsCache[s.key];
    final cache = s.aggregate ? _aggLtasCache : _ltasCache;
    return cache[s.key] ??= computeLtas(spec, m);
  }

  /// The register map (per-frame chest↔head) for the analysis subject; memoized.
  @override
  RegisterMap? get selectedRegister {
    final s = _analysisSubject;
    if (s == null) return null;
    final m = s.aggregate ? _aggMetricsCache[s.key] : _metricsCache[s.key];
    if (m == null) return null;
    final cache = s.aggregate ? _aggRegisterCache : _registerCache;
    return cache[s.key] ??= analyzeRegister(m);
  }

  /// Voice metrics for an arbitrary track (its inline lanes read this).
  VoiceMetrics? metricsFor(String trackId) {
    final t = _trackById(trackId);
    return t == null ? null : _metricsCache[t.audioFilePath];
  }

  bool metricsLoadingFor(String trackId) {
    final t = _trackById(trackId);
    return t != null && _metricsLoading.contains(t.audioFilePath);
  }

  /// Computed spectrogram for an arbitrary track (its inline lanes read this).
  Spectrogram? spectrogramFor(String trackId) {
    final t = _trackById(trackId);
    return t == null ? null : _spectrogramCache[t.audioFilePath];
  }

  bool spectrogramLoadingFor(String trackId) {
    final t = _trackById(trackId);
    return t != null && _spectrogramLoading.contains(t.audioFilePath);
  }

  // --- Inline analysis sub-lanes (Logic-style fold-out) -------------------

  bool isTrackExpanded(String trackId) => _expandedTracks.contains(trackId);
  Set<AnalysisLane> lanesFor(String trackId) =>
      _trackLanes[trackId] ?? const {};
  bool isLaneOn(String trackId, AnalysisLane lane) =>
      _trackLanes[trackId]?.contains(lane) ?? false;

  /// Folds a track's analysis sub-area open/closed. First open defaults to the
  /// pitch lane. View state — not persisted.
  void toggleTrackExpanded(String trackId) {
    if (_expandedTracks.remove(trackId)) {
      notifyListeners();
      return;
    }
    _expandedTracks.add(trackId);
    _trackLanes.putIfAbsent(trackId, () => {AnalysisLane.pitch});
    _ensureLaneData(trackId);
    notifyListeners();
  }

  /// Turns a single sub-lane on/off for [trackId], computing its data on demand.
  void toggleLane(String trackId, AnalysisLane lane) {
    final set = _trackLanes.putIfAbsent(trackId, () => <AnalysisLane>{});
    if (!set.remove(lane)) set.add(lane);
    _ensureLaneData(trackId);
    notifyListeners();
  }

  /// Computes whatever the currently-shown lanes need (metrics / spectrogram).
  void _ensureLaneData(String trackId) {
    final lanes = _trackLanes[trackId] ?? const {};
    if (lanes.contains(AnalysisLane.pitch) ||
        lanes.contains(AnalysisLane.loudness) ||
        lanes.contains(AnalysisLane.power)) {
      unawaited(ensureMetricsFor(trackId));
    }
    if (lanes.contains(AnalysisLane.spectrogram) ||
        lanes.contains(AnalysisLane.power)) {
      unawaited(ensureSpectrogramFor(trackId));
    }
  }

  /// Computes + caches voice metrics for any track's audio (lazy, in an isolate;
  /// WAV-only). Shares [_metricsCache] with the Voiceprint dock.
  Future<void> ensureMetricsFor(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    await _ensureMetricsForPath(track.audioFilePath);
  }

  /// Computes + caches voice metrics for one audio file (lazy, in an isolate;
  /// WAV-only). Keyed by [path] so a clip's result is shared by the per-clip
  /// inspector, the aggregate merge, and the dock.
  Future<void> _ensureMetricsForPath(String path) async {
    if (path.isEmpty ||
        _metricsCache.containsKey(path) ||
        _metricsLoading.contains(path) ||
        _metricsUnavailable.contains(path)) {
      return;
    }
    _metricsLoading.add(path);
    notifyListeners();
    try {
      final m = await _spectrograms.computeMetricsFromFile(path);
      if (m != null) {
        _metricsCache[path] = m;
      } else {
        _metricsUnavailable.add(path);
      }
    } catch (_) {
      // Compute failed; latch so it isn't retried. The lane shows unavailable.
      _metricsUnavailable.add(path);
    } finally {
      _metricsLoading.remove(path);
      notifyListeners();
    }
  }

  /// Computes + caches the spectrogram for any track's audio (lazy).
  Future<void> ensureSpectrogramFor(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    await _ensureSpectrogramForPath(track.audioFilePath);
  }

  /// Computes + caches the spectrogram for one audio file (lazy). Keyed by
  /// [path], like [_ensureMetricsForPath].
  Future<void> _ensureSpectrogramForPath(String path) async {
    if (path.isEmpty ||
        _spectrogramCache.containsKey(path) ||
        _spectrogramLoading.contains(path) ||
        _spectrogramUnavailable.contains(path)) {
      return;
    }
    _spectrogramLoading.add(path);
    notifyListeners();
    try {
      final spec = await _spectrograms.computeFromFile(path);
      if (spec != null) {
        _spectrogramCache[path] = spec;
      } else {
        _spectrogramUnavailable.add(path);
      }
    } catch (_) {
      // Compute failed; latch so it isn't retried. The lane shows unavailable.
      _spectrogramUnavailable.add(path);
    } finally {
      _spectrogramLoading.remove(path);
      notifyListeners();
    }
  }

  /// Computes spectrogram + metrics for one clip's file together.
  Future<void> _ensureClipAnalysis(String path) => Future.wait([
    _ensureSpectrogramForPath(path),
    _ensureMetricsForPath(path),
  ]);

  /// The pre-EQ input spectrum column at the playhead for [trackId]'s currently
  /// sounding clip, plus its bin width (Hz) — drives the FX EQ analyzer. Null
  /// when no clip of the take is under the playhead, or its display spectrogram
  /// isn't computed yet (kicking off that compute on the way).
  ({Float32List col, double binHz})? eqInputColumn(String trackId) {
    final track = _trackById(trackId);
    if (track == null) return null;
    final posMs = position.value.inMilliseconds;
    AudioClip? clip;
    for (final c in track.clips) {
      if (posMs >= c.startMs && posMs < c.endMs) {
        clip = c;
        break;
      }
    }
    if (clip == null) return null;
    final spec = _eqSpectroCache[clip.audioFilePath];
    if (spec == null) {
      unawaited(_ensureEqSpectrogramForPath(clip.audioFilePath));
      return null;
    }
    return (
      col: spec.columnAtTime((posMs - clip.startMs) / 1000),
      binHz: spec.binHz,
    );
  }

  /// Computes + caches the wide-range display spectrogram for one clip file.
  Future<void> _ensureEqSpectrogramForPath(String path) async {
    if (path.isEmpty ||
        _eqSpectroCache.containsKey(path) ||
        _eqSpectroLoading.contains(path) ||
        _eqSpectroUnavailable.contains(path)) {
      return;
    }
    _eqSpectroLoading.add(path);
    try {
      final spec = await _spectrograms.computeFromFile(
        path,
        maxHz: _eqDisplayMaxHz,
      );
      if (spec != null) {
        _eqSpectroCache[path] = spec;
      } else {
        _eqSpectroUnavailable.add(path);
      }
    } catch (_) {
      _eqSpectroUnavailable.add(path);
    } finally {
      _eqSpectroLoading.remove(path);
      notifyListeners();
    }
  }

  /// Computes the whole-lane aggregate for a multi-clip [track]: analyzes each
  /// clip's file (reusing the per-path caches, so selecting a clip later is
  /// already warm), then merges the per-clip results in timeline order. Clips
  /// that fail or aren't WAV are simply skipped; the aggregate is built from
  /// whatever analyzed.
  Future<void> _ensureAggregateAnalysis(AudioTrack track) async {
    final key = aggregateKeyFor(track);
    if (_aggMetricsCache.containsKey(key) ||
        _aggLoading.contains(key) ||
        _aggUnavailable.contains(key)) {
      return;
    }
    _aggLoading.add(key);
    notifyListeners();
    try {
      final clips = [...track.clips]
        ..sort((a, b) => a.startMs.compareTo(b.startMs));
      await Future.wait([
        for (final c in clips) _ensureClipAnalysis(c.audioFilePath),
      ]);
      final metrics = <VoiceMetrics>[];
      final specs = <Spectrogram>[];
      for (final c in clips) {
        final m = _metricsCache[c.audioFilePath];
        final spec = _spectrogramCache[c.audioFilePath];
        // Fold a clip in only when BOTH analyses are present and non-empty, kept
        // as a pair — an empty/degenerate clip yields spec.frames=1 but
        // metrics.frames=0, which would desync the two merged framings (the
        // formant overlay then normalizes against the wrong frame count).
        if (m != null && spec != null && m.frames > 0 && spec.frames > 0) {
          metrics.add(m);
          specs.add(spec);
        }
      }
      final mergedMetrics = mergeVoiceMetrics(metrics);
      final mergedSpec = mergeSpectrograms(specs);
      if (mergedMetrics != null) _aggMetricsCache[key] = mergedMetrics;
      if (mergedSpec != null) _aggSpectrogramCache[key] = mergedSpec;
      if (mergedMetrics == null && mergedSpec == null) {
        _aggUnavailable.add(key);
      }
    } catch (_) {
      _aggUnavailable.add(key);
    } finally {
      _aggLoading.remove(key);
      notifyListeners();
    }
  }

  /// Drops the cached spectrogram + voice metrics for an audio [path] that no
  /// longer exists (split/merge/re-analyze replace a take's file).
  void _evictAnalysis(String path) {
    _spectrogramCache.remove(path);
    _metricsCache.remove(path);
    _profileCache.remove(path);
    _ltasCache.remove(path);
    _registerCache.remove(path);
    // Clear the "unavailable" latches so the replacement file can re-analyze.
    _spectrogramUnavailable.remove(path);
    _metricsUnavailable.remove(path);
    _eqSpectroCache.remove(path);
    _eqSpectroUnavailable.remove(path);
    // Any whole-lane aggregate may have folded this path in — drop them all
    // (cheap to recompute, and only multi-clip takes ever have them).
    _evictAggregates();
  }

  /// Drops every cached whole-lane aggregate. Aggregate keys embed the clip ids
  /// so a changed clip set self-invalidates, but a clip's *file* can change with
  /// its id intact (re-analyze), so any file mutation clears them outright.
  void _evictAggregates() {
    if (_aggMetricsCache.isEmpty &&
        _aggSpectrogramCache.isEmpty &&
        _aggUnavailable.isEmpty) {
      return;
    }
    _aggSpectrogramCache.clear();
    _aggMetricsCache.clear();
    _aggProfileCache.clear();
    _aggLtasCache.clear();
    _aggRegisterCache.clear();
    _aggUnavailable.clear();
  }

  /// After a track's audio file changes (re-analyze/split/merge), refresh any
  /// open Voiceprint dock and inline sub-lanes that point at it.
  void _onTrackAudioChanged(String trackId) {
    if (_dockOpen && _dockShowsTake) {
      unawaited(ensureSelectedSpectrogram());
    }
    if (_expandedTracks.contains(trackId)) {
      _ensureLaneData(trackId);
    }
  }

  @override
  bool get spectrogramLoading {
    final s = _analysisSubject;
    if (s == null) return false;
    return s.aggregate
        ? _aggLoading.contains(s.key)
        : _spectrogramLoading.contains(s.key);
  }

  /// Computes (and caches) the spectrogram + metrics for the current analysis
  /// subject: a single selected clip, or — for a header selection — the one clip
  /// of a single-clip take or the aggregate of a multi-clip take. The dock,
  /// inspector and inline lanes share the per-path caches, so a clip analyzed
  /// here is reused everywhere (no double work).
  Future<void> ensureSelectedSpectrogram() async {
    final track = selectedTrack;
    // Backing tracks (original/accompaniment) carry no voice analysis — don't
    // spend a file read + isolate computing metrics the inspector never shows.
    if (track == null || track.isBacking || track.clips.isEmpty) return;
    final clipId = _selectedClipId;
    if (clipId != null) {
      final clip = track.clipById(clipId);
      if (clip != null) {
        await _ensureClipAnalysis(clip.audioFilePath);
        return;
      }
    }
    if (track.clips.length == 1) {
      await _ensureClipAnalysis(track.clips.first.audioFilePath);
      return;
    }
    await _ensureAggregateAnalysis(track);
  }

  /// All notes across every track, with their owning track, sorted by time.
  List<({String trackId, String trackName, PracticeNote note})> get allNotes {
    final practice = _practice;
    if (practice == null) return const [];
    final out = <({String trackId, String trackName, PracticeNote note})>[];
    void collect(AudioTrack t) {
      for (final n in t.notes) {
        out.add((trackId: t.id, trackName: t.displayName, note: n));
      }
    }

    final acc = practice.accompaniment;
    if (acc != null) collect(acc);
    for (final v in practice.activeVocalTracks) {
      collect(v);
    }
    out.sort((a, b) => a.note.positionMs.compareTo(b.note.positionMs));
    return out;
  }

  // --- Loading -------------------------------------------------------------

  Future<Result<SongPractice>> _load() async {
    final result = await _repository.getPractice(_practiceId);
    if (result is Ok<SongPractice>) {
      _practice = result.value;
      _ensureSelection();
      _ensureTimeline();
      _ensureLoopRange();
      // Render the workspace as soon as the project DATA is ready (tracks + cached waveforms), so
      // there is no blank loading screen; the audio engine sync below continues in the background.
      notifyListeners();
      if (_dockOpen && _dockShowsTake) {
        unawaited(ensureSelectedSpectrogram());
      }
      // Dev hook: show the Live voiceprint layout (without starting the mic).
      if (const bool.fromEnvironment('LIVE_DEMO')) _liveMonitor = true;
      // Dev hook for screenshots: open a specific dock tab by name.
      const dockTabName = String.fromEnvironment('DOCK_TAB');
      if (dockTabName.isNotEmpty) {
        for (final t in DockTab.values) {
          if (t.name == dockTabName) _dockTab = t;
        }
        // Keep the two dev hooks consistent: a non-live tab can't be live.
        if (!_tabSupportsLive(_dockTab)) _liveMonitor = false;
      }
      // Dev hook for screenshots: fold out every vocal take with all lanes.
      if (const bool.fromEnvironment('EXPAND_PITCH')) {
        for (final t in result.value.vocalTracks) {
          _expandedTracks.add(t.id);
          _trackLanes[t.id] = AnalysisLane.values.toSet();
          _ensureLaneData(t.id);
        }
      }
      await _resyncPlayback();
      notifyListeners();
    }
    return result;
  }

  // --- Importing audio -----------------------------------------------------

  /// Imports [sourcePath] as the accompaniment (replacing any existing one).
  Future<void> addAccompaniment(String sourcePath) async {
    if (_practice == null) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    await _runBusy(() async {
      final dest = await _audioFiles.importAudio(
        practiceId: _practiceId,
        sourcePath: sourcePath,
      );
      final analysis = await _waveforms.analyze(dest);
      final track = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.accompaniment,
        displayName: p.basenameWithoutExtension(sourcePath),
        audioFilePath: dest,
        durationMs: analysis.durationMs,
        waveformPeaks: analysis.peaks,
        createdAt: DateTime.now(),
      );
      // Re-read the LIVE practice after the import/analyze awaits: a take
      // recorded/imported during them must survive, so rebuild from the current
      // model rather than a stale pre-await snapshot (lost-update). The
      // accompaniment slot is single-writer (guarded above), so the replaced
      // backing + kept original are read from the fresh model too.
      final cur = _practice;
      if (cur == null) return;
      final old = cur.accompaniment;
      final staleSource = cur.separationSource;
      // A manual import replaces any separation-derived backing — drop the kept
      // original so "Re-separate" can't later target a stale source.
      _practice = cur.copyWith(
        accompaniment: track,
        clearSeparationSource: true,
      );
      if (old != null) await _audioFiles.deleteAudio(old.audioFilePath);
      if (staleSource != null) {
        await _audioFiles.deleteAudio(staleSource);
      }
      await _save();
    });
  }

  /// Imports [sourcePath] as the reference track / original song (replacing any
  /// existing one).
  Future<void> addReference(String sourcePath) async {
    if (_practice == null) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    await _runBusy(() async {
      final dest = await _audioFiles.importAudio(
        practiceId: _practiceId,
        sourcePath: sourcePath,
      );
      final analysis = await _waveforms.analyze(dest);
      final track = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.reference,
        displayName: p.basenameWithoutExtension(sourcePath),
        audioFilePath: dest,
        durationMs: analysis.durationMs,
        waveformPeaks: analysis.peaks,
        createdAt: DateTime.now(),
      );
      // Re-read the LIVE practice after the awaits (see addAccompaniment) so a
      // concurrently-committed take isn't lost to a stale-snapshot rebuild.
      final cur = _practice;
      if (cur == null) return;
      final old = cur.reference;
      final staleSource = cur.separationSource;
      _practice = cur.copyWith(reference: track, clearSeparationSource: true);
      if (old != null) await _audioFiles.deleteAudio(old.audioFilePath);
      if (staleSource != null) {
        await _audioFiles.deleteAudio(staleSource);
      }
      await _save();
    });
  }

  /// Imports [sourcePath] as a new vocal take.
  Future<void> addVocalTake(String sourcePath) async {
    final practice = _practice;
    if (practice == null) return;
    await _runBusy(() async {
      final dest = await _audioFiles.importAudio(
        practiceId: _practiceId,
        sourcePath: sourcePath,
      );
      final analysis = await _waveforms.analyze(dest);
      _appendVocal(
        path: dest,
        displayName: p.basenameWithoutExtension(sourcePath),
        analysis: analysis,
      );
      await _save();
    });
  }

  /// Imports [sourcePath] and adds it as a new CLIP on the existing vocal take [trackId] at
  /// the playhead — the drag-drop equivalent of punching a recording onto an armed take.
  /// Existing clips are left untouched (Logic / Cubase: clips never auto-merge).
  Future<void> addClipToTake(String trackId, String sourcePath) async {
    final practice = _practice;
    if (practice == null) return;
    final target = _trackById(trackId);
    if (target == null || target.kind != AudioTrackKind.vocal) return;
    await _runBusy(() async {
      final dest = await _audioFiles.importAudio(
        practiceId: _practiceId,
        sourcePath: sourcePath,
      );
      final analysis = await _waveforms.analyze(dest);
      final clip = AudioClip(
        id: newId(),
        audioFilePath: dest,
        durationMs: analysis.durationMs,
        startMs: math.max(0, position.value.inMilliseconds),
        waveformPeaks: analysis.peaks,
      );
      _updateTrack(target.id, (t) => t.copyWith(clips: [...t.clips, clip]));
      _selectedTrackId = target.id;
      _selectedClipId = clip.id;
      _onTrackAudioChanged(target.id);
      await _save();
    });
  }

  /// Whether there's already a separated backing present (so the action reads as
  /// "re-extract"). False once the produced accompaniment is deleted — then it's
  /// a fresh extraction again.
  bool get canReseparate =>
      _practice?.separationSource != null && _practice?.accompaniment != null;

  /// Separates the original song and puts the **instrumental** in the
  /// Accompaniment slot (to sing over). The **Original is kept** in the
  /// Reference slot untouched; the isolated vocal is discarded for now. The
  /// active backing switches to the instrumental.
  Future<void> separateBacking(String trackId) async {
    final practice = _practice;
    final source = _trackById(trackId);
    if (practice == null || source == null || !source.isBacking) return;
    if (isSeparating || _busy) return; // no concurrent / re-entrant runs
    if (!_separationAvailable) {
      _toast(
        'Separation engine unavailable. Install audio-separator and run a '
        'non-sandboxed debug build (see assets/separation/README.md).',
      );
      return;
    }
    if (_isRecording) {
      _toast('Stop recording to separate the backing.');
      return;
    }
    // The original to separate + keep: prefer the Reference slot (where the
    // original lives), else the right-clicked track.
    final original = practice.reference ?? source;
    final originalPath = original.audioFilePath;
    final baseName = original.displayName.replaceAll(
      RegExp(r'\s*\(Instrumental\)$'),
      '',
    );

    _separationProgress = 0;
    notifyListeners();
    await _runBusy(() async {
      // The old backing/reference to clean up after a successful swap. Captured from
      // the LIVE practice at commit time (below), not the pre-await snapshot.
      AudioTrack? oldAccompaniment;
      AudioTrack? oldReference;
      try {
        final SeparationResult stems;
        try {
          stems = await _separation.separate(
            originalPath,
            onProgress: (v) {
              _separationProgress = v;
              notifyListeners();
            },
          );
        } on SeparationCancelled {
          return; // user aborted before any file work — nothing to undo
        }

        // Only the instrumental is used; the vocal stem is ignored for now.
        final instDest = await _audioFiles.importAudio(
          practiceId: _practiceId,
          sourcePath: stems.instrumentalPath,
        );
        var saved = false;
        try {
          final instAnalysis = await _waveforms.analyze(instDest);
          final instrumental = AudioTrack(
            id: newId(),
            kind: AudioTrackKind.accompaniment,
            displayName: '$baseName (Instrumental)',
            audioFilePath: instDest,
            durationMs: instAnalysis.durationMs,
            waveformPeaks: instAnalysis.peaks,
            createdAt: DateTime.now(),
          );
          // Keep the original as the Reference track. If it isn't already the
          // reference (e.g. it was dropped into the accompaniment slot), recreate
          // it with the reference kind so the Original stays available.
          final referenceTrack = original.isReference
              ? original
              : AudioTrack(
                  id: original.id,
                  kind: AudioTrackKind.reference,
                  displayName: original.displayName,
                  audioFilePath: original.audioFilePath,
                  durationMs: original.durationMs,
                  startMs: original.startMs,
                  waveformPeaks: original.waveformPeaks,
                  notes: original.notes,
                  techniqueMarkers: original.techniqueMarkers,
                  colorValue: original.colorValue,
                  volume: original.volume,
                  createdAt: original.createdAt,
                );

          // Re-read the LIVE practice: notes / lyrics / interpretations may have been
          // edited (and persisted) during the minutes-long separation. Build on the
          // current state so those edits are not clobbered by the pre-await snapshot
          // (mirrors addAccompaniment / addReference).
          final cur = _practice;
          if (cur == null) {
            await _audioFiles.deleteAudio(instDest);
            return;
          }
          oldAccompaniment = cur.accompaniment;
          oldReference = cur.reference;
          _playReference = false; // sing over the instrumental
          _practice = cur.copyWith(
            accompaniment: instrumental,
            reference: referenceTrack,
            separationSource: originalPath,
          );
          // Persist BEFORE deleting any old audio, so a failed save can't leave
          // the JSON pointing at files already gone.
          saved = await _save();
          if (!saved) {
            _practice = cur; // revert in-memory to the un-persisted state
            _toast('Could not save the separated track.');
          }
        } catch (_) {
          // Roll back the copy on any failure before it goes live.
          await _audioFiles.deleteAudio(instDest);
          rethrow;
        }

        if (!saved) {
          await _audioFiles.deleteAudio(instDest);
          return;
        }

        // The instrumental is live + persisted. Delete any prior backing file
        // that's no longer referenced — never the kept original or new
        // instrumental. Errors here must NOT roll back the live track.
        for (final old in [oldAccompaniment, oldReference]) {
          if (old != null &&
              old.audioFilePath != originalPath &&
              old.audioFilePath != instDest) {
            _evictAnalysis(old.audioFilePath);
            try {
              await _audioFiles.deleteAudio(old.audioFilePath);
            } catch (_) {
              /* best-effort: a lingering old file is harmless */
            }
          }
        }
      } finally {
        _separationProgress = null;
        notifyListeners();
      }
    });
  }

  /// Aborts an in-flight separation (best-effort).
  void cancelSeparation() {
    if (isSeparating) _separation.cancel();
  }

  // --- Recording -----------------------------------------------------------

  Future<void> startRecording() async {
    // `|| _busy` closes the re-arm race: stopRecording clears _isRecording BEFORE
    // its finalize runs inside _runBusy (which holds _busy), so without this a fast
    // Stop→Record would start a second take whose createEmptyVocalTake/_resyncPlayback
    // interleave with the still-running finalize's _save (lost update + graph thrash).
    if (_isRecording || _startingRecording || _busy) return;
    _startingRecording = true;
    try {
      await _beginRecording();
    } catch (e) {
      // Setup failed partway (createEmptyVocalTake/_save, newRecordingPath I/O,
      // engine start, playback). Tear down anything started and surface it — the
      // finally re-enables REC instead of leaving _startingRecording stuck true
      // (which would dead-button recording for the rest of the session).
      _recLevelTimer?.cancel();
      _recLevelTimer = null;
      _graphEngine?.stopInputRecording();
      _isRecording = false;
      _toast(_friendlyError(e));
      notifyListeners();
    } finally {
      _startingRecording = false;
    }
  }

  Future<void> _beginRecording() async {
    if (_liveMonitor) await setLiveMonitor(false);
    final engine = _graphEngine;
    if (engine == null) return;
    if (!await MicPermission.request()) {
      _toast(
        'Microphone access is off. Enable it in System Settings › '
        'Privacy & Security › Microphone.',
      );
      return;
    }
    // The route can be popped during the permission dialog or the take-setup
    // awaits below — canPop is true while _isRecording is still false — which
    // disposes the VM. Bail before touching the model or opening the native WAV
    // handle so neither runs on a disposed VM (mirrors toggleInputMonitor's guard).
    if (_disposed) return;
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
      await createEmptyVocalTake();
      targetId = _selectedTrackId;
    }
    _recordingTargetId = targetId;
    _recordingStartMs = position.value.inMilliseconds;
    _recPeaks.clear();
    recordingPeaks.value = const [];
    _recordingPath = await _audioFiles.newRecordingPath(
      practiceId: _practiceId,
    );
    // Popped during createEmptyVocalTake/_save or newRecordingPath — don't open a
    // native WAV handle that nothing will ever close, nor arm the level timer that
    // would write disposed ValueNotifiers every tick.
    if (_disposed) return;
    // Record from the engine's selected input device (cpal). macOS prompts for mic access on the
    // first capture; if denied, the take is silent.
    engine.startInputRecording(_recordingPath!);
    _isRecording = true;
    notifyListeners();

    // Mute the take being recorded into (you can't monitor what you're laying
    // down); every other lane still obeys mute/solo. Play freely (no range wrap)
    // from the cursor as a reference.
    await _resyncPlayback();
    if (_disposed) return;
    _playback.setLoop(false, 0, 0);
    await _playback.seek(Duration(milliseconds: _recordingStartMs));
    if (_disposed) return;
    position.value = Duration(milliseconds: _recordingStartMs);
    await _playback.play();
    // Popped during the resync/seek/play awaits above — dispose() already stopped
    // recording and disposed the notifiers/playback; don't arm a timer that would
    // write disposed ValueNotifiers and poll a disposed engine every tick.
    if (_disposed) return;

    // Drive the meter + live waveform off the engine's input peak.
    _recLevelTimer = Timer.periodic(const Duration(milliseconds: _recordIntervalMs), (
      _,
    ) {
      final peak = engine.inputLevel(); // linear 0..1
      // Meter: dB-perceptual (reads clearly). Live waveform: LINEAR, so it matches the final take
      // waveform (rendered from the WAV in linear amplitude) instead of looking fuller than it.
      recordLevel.value = _meterFromPeak(peak);
      // Also drive the mixer's MIC meter while recording (it shares this notifier with monitoring).
      inputLevel.value = _meterFromPeak(peak);
      _recPeaks.add(peak);
      recordingPeaks.value = UnmodifiableListView(_recPeaks);
      position.value = Duration(
        milliseconds: _recordingStartMs + recordingDurationMs,
      );
    });
  }

  Future<void> stopRecording() async {
    if (!_isRecording) return;
    _recLevelTimer?.cancel();
    _recLevelTimer = null;
    recordLevel.value = 0;
    // The recording loop was driving the shared MIC meter; unless monitoring is
    // also on (its own timer keeps updating it), nothing else will, so clear it
    // — otherwise the meter sticks at the last captured peak.
    if (!_inputMonitorOn) {
      _heldInputLevel = 0;
      inputLevel.value = 0;
    }
    _graphEngine?.stopInputRecording();
    await _playback.pause();
    final path = _recordingPath;
    // Record-latency compensation: the first captured sample is the vocal sung in response to the
    // backing the singer heard ~round-trip ago (output IO + acoustic + input IO + AEC), so the take
    // belongs that much EARLIER on the timeline — otherwise it plays back behind the beat.
    final latencyMs = _graphEngine?.recordLatencyMs() ?? 0.0;
    final startMs = (_recordingStartMs - latencyMs).round().clamp(
      0,
      _recordingStartMs,
    );
    final targetId = _recordingTargetId;
    _isRecording = false;
    _recordingTargetId = null;
    _recPeaks.clear();
    recordingPeaks.value = const [];
    notifyListeners();
    if (path == null) return;

    await _runBusy(() async {
      final analysis = await _waveforms.analyze(path);
      final target = targetId == null ? null : _trackById(targetId);
      if (target == null || target.kind != AudioTrackKind.vocal) {
        // No armed take → the recording becomes a new take at the cursor.
        _selectedTrackId = _appendVocal(
          path: path,
          displayName: 'Take ${_nextTakeNumber()}',
          analysis: analysis,
          startMs: startMs,
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
        // recording, leaving _selectedTrackId stale would pair this clip id with
        // the wrong lane and the inspector would analyze the wrong subject.
        _selectedTrackId = target.id;
        _selectedClipId = clip.id;
        _onTrackAudioChanged(target.id);
      }
      await _save();
    });
  }

  /// Creates an empty vocal take (a lane with no clip yet) in the active
  /// interpretation and selects it, so the next REC records into it.
  Future<void> createEmptyVocalTake() async {
    final practice = _practice;
    if (practice == null) return;
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take ${_nextTakeNumber()}',
      durationMs: 0,
      createdAt: DateTime.now(),
      versionId: practice.activeVersionId,
    );
    _practice = practice.copyWith(
      vocalTracks: [...practice.vocalTracks, track],
    );
    _selectedTrackId = track.id;
    await _save();
  }

  /// Deletes a single clip from a lane (Logic / Cubase style), keeping the lane
  /// and its other clips. Notes anchored inside the clip's span are dropped. Use
  /// [deleteTrack] to remove a whole lane.
  Future<void> deleteClip(String trackId, String clipId) async {
    final track = _trackById(trackId);
    final clip = track?.clipById(clipId);
    if (track == null || clip == null) return;
    final remaining = track.clips.where((c) => c.id != clipId).toList();
    // Drop markers anchored inside the deleted clip; for a range that starts
    // before the clip, clamp its end to the clip edge so it doesn't overhang the
    // gap left behind.
    final keptNotes = track.notes
        .where((n) => n.positionMs < clip.startMs || n.positionMs >= clip.endMs)
        .map(
          (n) =>
              n.positionMs < clip.startMs ? clampNoteEnd(n, clip.startMs) : n,
        )
        .toList();
    final keptMarkers = track.techniqueMarkers
        .where((m) => m.positionMs < clip.startMs || m.positionMs >= clip.endMs)
        .map(
          (m) =>
              m.positionMs < clip.startMs ? clampMarkerEnd(m, clip.startMs) : m,
        )
        .toList();
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        clips: remaining,
        notes: keptNotes,
        techniqueMarkers: keptMarkers,
      ),
    );
    if (_selectedClipId == clipId) _selectedClipId = null;
    _evictAnalysis(clip.audioFilePath);
    await _audioFiles.deleteAudio(clip.audioFilePath);
    _onTrackAudioChanged(trackId);
    await _save();
  }

  String _appendVocal({
    required String path,
    required String displayName,
    required WaveformResult analysis,
    int startMs = 0,
  }) {
    final practice = _practice;
    if (practice == null) return '';
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: displayName,
      audioFilePath: path,
      durationMs: analysis.durationMs,
      startMs: startMs,
      waveformPeaks: analysis.peaks,
      createdAt: DateTime.now(),
      versionId: practice.activeVersionId,
    );
    _practice = practice.copyWith(
      vocalTracks: [...practice.vocalTracks, track],
    );
    return track.id;
  }

  /// One past the highest existing "Take N" label in the active interpretation,
  /// so a new take never reuses a number freed by a delete.
  int _nextTakeNumber() {
    final re = RegExp(r'^Take (\d+)$');
    var max = 0;
    for (final t in vocalTakes) {
      final m = re.firstMatch(t.displayName);
      if (m != null) {
        final n = int.parse(m.group(1)!);
        if (n > max) max = n;
      }
    }
    return max + 1;
  }

  // --- Transport -----------------------------------------------------------

  Future<void> togglePlay() async {
    if (_playback.isPlaying) {
      await _playback.pause();
    } else {
      await _playback.play();
    }
  }

  @override
  Future<void> seek(Duration to) => _playback.seek(to);

  /// Stops playback and returns the playhead to the start of the play range.
  Future<void> stop() async {
    await _playback.pause();
    await _playback.seek(Duration(milliseconds: _loopStartMs));
  }

  Future<void> seekToFraction(double fraction) {
    final durationMs = _practice?.timelineDurationMs ?? 0;
    final clamped = fraction < 0 ? 0.0 : (fraction > 1 ? 1.0 : fraction);
    return seek(Duration(milliseconds: (clamped * durationMs).round()));
  }

  // --- Loop / cycle region ------------------------------------------------

  bool _loopEnabled = false;
  bool get loopEnabled => _loopEnabled;

  int _loopStartMs = 0;
  int _loopEndMs = 0;
  int get loopStartMs => _loopStartMs;
  int get loopEndMs => _loopEndMs;
  bool get hasLoop => _loopEndMs > _loopStartMs;

  static const int _minLoopMs = 150;

  /// Defaults the timeline length to fit the content (plus a little tail) on
  /// first open, and grows it if the content has outgrown it — but never
  /// shrinks it, so moving a clip can't rescale the timeline.
  void _ensureTimeline() {
    final practice = _practice;
    if (practice == null) return;
    final content = practice.contentEndMs;
    var dur = practice.timelineDurationMs;
    if (dur <= 0) {
      dur = content <= 0
          ? 20000
          : content + (content * 15 ~/ 100).clamp(2000, 30000);
    } else if (dur < content) {
      dur = content;
    }
    if (dur != practice.timelineDurationMs) {
      _practice = practice.copyWith(timelineDurationMs: dur);
    }
  }

  // The timeline extent (the full arrangement, persisted) is the "fit all" view.
  // The zoom narrows the *visible* span — anchored at the start — down to a
  // 1-second window, so the grid can refine to one line per second. The view
  // span is transient (not persisted); it resets to fit-all on reopen.
  static const int _viewMinMs = 1000; // most zoomed in: a 1-second window
  int _viewSpanMs = 0; // 0 = fit all (the whole extent)

  int get _extentMs {
    final c = _practice?.timelineDurationMs ?? 0;
    return c < _viewMinMs ? _viewMinMs : c;
  }

  /// The visible timeline span in ms — between a 1-second window and fit-all.
  int get viewSpanMs {
    final ext = _extentMs;
    final span = _viewSpanMs <= 0 ? ext : _viewSpanMs;
    return span.clamp(_viewMinMs, ext);
  }

  /// Current zoom as a slider value in [0,1]: 0 = fully zoomed out (fit all),
  /// 1 = fully zoomed in (1-second window).
  double get timelineZoom {
    final lo = _viewMinMs, hi = _extentMs;
    if (hi <= lo) return 0.0;
    return (1.0 - math.log(viewSpanMs / lo) / math.log(hi / lo)).clamp(
      0.0,
      1.0,
    );
  }

  /// Sets the zoom from the slider live (transient view state).
  void setTimelineZoomLive(double value) {
    final lo = _viewMinMs, hi = _extentMs;
    if (hi <= lo) return;
    final t = value.clamp(0.0, 1.0);
    final span = (lo * math.pow(hi / lo, 1 - t)).round().clamp(lo, hi);
    if (span == viewSpanMs) return;
    _viewSpanMs = span;
    _recenterViewOnPlayhead();
    notifyListeners();
  }

  Future<void> commitTimelineZoom() async {}

  /// Nudges the zoom by [factor] (>1 zooms out / widens, <1 zooms in), for the
  /// slider's end icons.
  Future<void> zoomTimeline(double factor) async {
    final span = (viewSpanMs * factor).round().clamp(_viewMinMs, _extentMs);
    if (span == viewSpanMs) return;
    _viewSpanMs = span;
    _recenterViewOnPlayhead();
    notifyListeners();
  }

  // --- Viewport pan / follow -----------------------------------------------

  int _viewStartMs = 0; // left edge of the visible window (ms)

  /// The left edge of the visible window, clamped so it never scrolls past the
  /// extent. 0 when fit-all.
  int get viewStartMs {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    return _viewStartMs.clamp(0, maxStart);
  }

  bool _followCursor = true;

  /// Whether the timeline auto-scrolls to keep the playhead in view (on by
  /// default; a manual scroll turns it off).
  bool get followCursor => _followCursor;
  void toggleFollowCursor() {
    _followCursor = !_followCursor;
    if (_followCursor) _recenterViewOnPlayhead();
    notifyListeners();
  }

  /// Pans the visible window by [deltaMs] (horizontal scroll / drag). Turns off
  /// follow so a manual scroll sticks.
  void panView(int deltaMs) {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    if (maxStart <= 0) return;
    final next = (viewStartMs + deltaMs).clamp(0, maxStart);
    if (next == _viewStartMs) {
      if (_followCursor) {
        _followCursor = false;
        notifyListeners();
      }
      return;
    }
    _followCursor = false;
    _viewStartMs = next;
    notifyListeners();
  }

  void _recenterViewOnPlayhead() {
    final maxStart = (_extentMs - viewSpanMs).clamp(0, _extentMs);
    _viewStartMs = (position.value.inMilliseconds - viewSpanMs ~/ 2).clamp(
      0,
      maxStart,
    );
  }

  void _maybeFollowCursor(int playheadMs) {
    if (!_followCursor) return;
    final span = viewSpanMs;
    final maxStart = (_extentMs - span).clamp(0, _extentMs);
    if (maxStart <= 0) return;
    final start = viewStartMs;
    // Page-scroll: while the playhead is comfortably inside the window it moves
    // smoothly across a *static* view (no stutter); only when it nears the right
    // edge (or falls behind the view) do we jump forward by a page.
    if (playheadMs >= start && playheadMs < start + (span * 88 ~/ 100)) return;
    final next = (playheadMs - span ~/ 10).clamp(0, maxStart);
    if (next == _viewStartMs) return;
    _viewStartMs = next;
    notifyListeners();
  }

  /// Defaults the play region to the content length (the longest take) on first
  /// load, and keeps it within the (possibly longer) timeline afterwards.
  void _ensureLoopRange() {
    final timeline = _practice?.timelineDurationMs ?? 0;
    // Clamp the loop to real audio (content), not the padded timeline, and pull
    // it BACK when content shrinks — e.g. deleting the longest take must not
    // leave loopEnd cycling through a now-silent tail.
    final max = _loopMaxMs;
    if (_loopEndMs <= 0 || _loopEndMs > max) _loopEndMs = max;
    if (_loopStartMs < 0) _loopStartMs = 0;
    if (_loopStartMs > _loopEndMs - _minLoopMs) {
      _loopStartMs = (_loopEndMs - _minLoopMs).clamp(0, timeline);
    }
    _playback.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
  }

  /// When the loop is active, bring the playhead inside [loopStart, loopEnd).
  /// The native engine only WRAPS at loopEnd; it never snaps a playhead that sits
  /// BEFORE loopStart up into the window, so without this enabling a loop while
  /// the playhead is to its left plays a long out-of-region pass before wrapping.
  void _snapPlayheadIntoLoop() {
    if (!_loopEnabled || !hasLoop) return;
    final posMs = position.value.inMilliseconds;
    if (posMs < _loopStartMs || posMs >= _loopEndMs) {
      position.value = Duration(milliseconds: _loopStartMs);
      unawaited(_playback.seek(Duration(milliseconds: _loopStartMs)));
    }
  }

  /// The furthest a loop edge may go: the end of real audio, NOT the padded
  /// timeline — looping into the ~15% silent tail just plays dead air each cycle.
  int get _loopMaxMs {
    final content = _practice?.contentEndMs ?? 0;
    return content > 0 ? content : (_practice?.timelineDurationMs ?? 0);
  }

  /// Drags the left edge of the loop region (from the ruler handle).
  void setLoopStart(int ms) {
    // Guard the upper bound: for sub-_minLoopMs content (a tiny clip) the naive
    // `_loopEndMs - _minLoopMs` goes negative, and num.clamp asserts (debug) or
    // returns a negative upperLimit (release) when upper < lower.
    final hi = math.max(0, _loopEndMs - _minLoopMs);
    _loopStartMs = ms.clamp(0, hi);
    _playback.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  /// Drags the right edge of the loop region.
  void setLoopEnd(int ms) {
    // Guard the lower bound symmetrically: for sub-_minLoopMs content
    // `_loopStartMs + _minLoopMs` can exceed _loopMaxMs (lower > upper).
    final lo = math.min(_loopStartMs + _minLoopMs, _loopMaxMs);
    _loopEndMs = ms.clamp(lo, _loopMaxMs);
    _playback.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  void toggleLoop() {
    _loopEnabled = !_loopEnabled;
    _playback.setLoop(_loopEnabled, _loopStartMs, _loopEndMs);
    _snapPlayheadIntoLoop();
    notifyListeners();
  }

  // --- Track mutations -----------------------------------------------------

  Future<void> deleteTrack(String trackId) async {
    final practice = _practice;
    if (practice == null) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    if (practice.accompaniment?.id == trackId) {
      for (final c in practice.accompaniment!.clips) {
        _evictAnalysis(c.audioFilePath);
        await _audioFiles.deleteAudio(c.audioFilePath);
      }
      final cur = _practice;
      if (cur == null) return;
      _practice = cur.copyWith(clearAccompaniment: true);
    } else if (practice.reference?.id == trackId) {
      for (final c in practice.reference!.clips) {
        _evictAnalysis(c.audioFilePath);
        await _audioFiles.deleteAudio(c.audioFilePath);
      }
      final cur = _practice;
      if (cur == null) return;
      _practice = cur.copyWith(clearReference: true);
    } else {
      for (final t in practice.vocalTracks) {
        if (t.id == trackId) {
          // Delete EVERY clip's file — a multi-clip take would otherwise orphan
          // its trailing clips' audio.
          for (final c in t.clips) {
            _evictAnalysis(c.audioFilePath);
            await _audioFiles.deleteAudio(c.audioFilePath);
          }
          break;
        }
      }
      // Rebuild from the LIVE practice so a take recorded/imported during the
      // deleteAudio awaits isn't dropped by a stale-snapshot rebuild.
      final cur = _practice;
      if (cur == null) return;
      _practice = cur.copyWith(
        vocalTracks: cur.vocalTracks.where((t) => t.id != trackId).toList(),
      );
    }
    _expandedTracks.remove(trackId);
    _trackLanes.remove(trackId);
    await _save();
  }

  // --- Clip editing (drag-move, reorder, split, merge, …) -----------------

  /// Moves a clip along the timeline live (model only) while dragging — call on
  /// every drag tick with the new absolute [startMs]. Persist once on release
  /// with [commitTrackMove].
  void moveTrackStartLive(String trackId, int startMs) {
    final track = _trackById(trackId);
    if (track == null) return;
    // Keep the clip within the (fixed) timeline so dragging never resizes it.
    final timeline = _practice?.timelineDurationMs ?? 0;
    final maxStart = (timeline - track.durationMs).clamp(0, timeline);
    final clamped = startMs.clamp(0, maxStart);
    if (track.startMs == clamped) return;
    _updateTrack(trackId, (t) => t.copyWith(startMs: clamped));
    notifyListeners();
  }

  /// Persists a finished drag-move and re-aligns offset-aware playback.
  Future<void> commitTrackMove() => _save();

  /// Moves a single clip along the timeline live (model only) while dragging.
  /// Persist once on release with [commitTrackMove].
  void moveClipStartLive(String trackId, String clipId, int startMs) {
    final track = _trackById(trackId);
    final clip = track?.clipById(clipId);
    if (track == null || clip == null) return;
    final timeline = _practice?.timelineDurationMs ?? 0;
    final maxStart = (timeline - clip.durationMs).clamp(0, timeline);
    final clamped = startMs.clamp(0, maxStart);
    if (clip.startMs == clamped) return;
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        clips: [
          for (final c in t.clips)
            if (c.id == clipId) c.copyWith(startMs: clamped) else c,
        ],
      ),
    );
    notifyListeners();
  }

  /// Moves the dragged vocal take into the lane currently held by [targetId]
  /// (drag-and-drop reorder) — "move to a different track" in our one-clip-per-
  /// lane model.
  Future<void> moveVocalTrackToLaneOf(String draggedId, String targetId) async {
    final practice = _practice;
    if (practice == null || draggedId == targetId) return;
    final list = [...practice.vocalTracks];
    final from = list.indexWhere((t) => t.id == draggedId);
    final to = list.indexWhere((t) => t.id == targetId);
    if (from < 0 || to < 0) return;
    final moved = list.removeAt(from);
    list.insert(to, moved);
    _practice = practice.copyWith(vocalTracks: list);
    await _save();
  }

  Future<void> renameTrack(String trackId, String name) async {
    final trimmed = name.trim();
    if (trimmed.isEmpty) return;
    _updateTrack(trackId, (t) => t.copyWith(displayName: trimmed));
    await _save();
  }

  /// Sets a track's custom lane tint (ARGB). Pass null to reset to the default.
  Future<void> setTrackColor(String trackId, int? colorValue) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(colorValue: colorValue, clearColor: colorValue == null),
    );
    await _save();
  }

  /// Copies a vocal take (audio + notes) into a new lane right below it.
  Future<void> duplicateTrack(String trackId) async {
    final src = _trackById(trackId);
    if (_practice == null || src == null || src.isBacking) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    await _runBusy(() async {
      // Copy EVERY clip's file (not just the primary) to a fresh path and give it
      // a fresh id, so the duplicate is fully independent: a later delete of the
      // source (or the copy) must never touch the other's audio, and reused clip
      // PKs would collide on insert and roll the whole save back.
      final newClips = <AudioClip>[];
      for (final c in src.clips) {
        final dest = await _edits.duplicate(
          practiceId: _practiceId,
          sourcePath: c.audioFilePath,
        );
        newClips.add(
          AudioClip(
            id: newId(),
            audioFilePath: dest,
            durationMs: c.durationMs,
            startMs: c.startMs,
            waveformPeaks: c.waveformPeaks,
          ),
        );
      }
      final copy = src.copyWith(
        id: newId(),
        displayName: '${src.displayName} copy',
        createdAt: DateTime.now(),
        clips: newClips,
        // Fresh note/marker ids too: unlike split/merge the SOURCE take survives,
        // so its child rows stay LIVE holding these ids — copying them verbatim
        // onto the new track id would collide on the notes/markers primary key and
        // roll the whole save back (only clip ids were regenerated before).
        notes: src.notes.map((n) => n.copyWith(id: newId())).toList(),
        techniqueMarkers: src.techniqueMarkers
            .map((m) => m.copyWith(id: newId()))
            .toList(),
      );
      // Re-read the LIVE practice after the file copies so a take recorded/
      // imported meanwhile isn't dropped by a stale-snapshot rebuild.
      final cur = _practice;
      if (cur == null) return;
      final list = [...cur.vocalTracks];
      final idx = list.indexWhere((t) => t.id == trackId);
      if (idx < 0) return; // source removed during the copy
      list.insert(idx + 1, copy);
      _practice = cur.copyWith(vocalTracks: list);
      if (await _save()) _toast('Take duplicated.');
    });
  }

  /// Splits a vocal take at the playhead into two takes; the second is offset to
  /// the cut. Notes follow whichever side they fall on.
  Future<void> splitTrackAtPlayhead(String trackId) async {
    final practice = _practice;
    final src = _trackById(trackId);
    if (practice == null || src == null || src.isBacking) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    // Byte-splitting one file can't represent a multi-clip lane — refuse rather
    // than silently drop the trailing clips (delete/move a clip from its menu).
    if (src.clips.length > 1) {
      _toast(
        'This take has multiple clips. Delete or move a clip from its menu '
        'instead of splitting the lane.',
      );
      return;
    }
    final atMs = position.value.inMilliseconds;
    if (atMs <= src.startMs + _minLoopMs || atMs >= src.endMs - _minLoopMs) {
      _toast(
        'Move the playhead onto this take to split it. The take spans '
        'only its highlighted region.',
      );
      return;
    }
    await _runBusy(() async {
      final parts = await _edits.splitWav(
        practiceId: _practiceId,
        sourcePath: src.audioFilePath,
        splitFileMs: atMs - src.startMs,
      );
      final firstAnalysis = await _waveforms.analyze(parts.firstPath);
      final secondAnalysis = await _waveforms.analyze(parts.secondPath);
      final first = src.copyWith(
        audioFilePath: parts.firstPath,
        durationMs: firstAnalysis.durationMs,
        waveformPeaks: firstAnalysis.peaks,
        // Markers stay with the half their start falls in; a range straddling
        // the cut is clamped so it doesn't dangle past the shortened take.
        notes: src.notes
            .where((n) => n.positionMs < atMs)
            .map((n) => clampNoteEnd(n, atMs))
            .toList(),
        techniqueMarkers: src.techniqueMarkers
            .where((m) => m.positionMs < atMs)
            .map((m) => clampMarkerEnd(m, atMs))
            .toList(),
      );
      final second = src.copyWith(
        id: newId(),
        // A FRESH clip id (not the legacy single-clip copyWith path, which keeps the
        // source clip's id) — otherwise the new take and the surviving `first` take would
        // share one clip primary key and the save transaction would roll back on the
        // duplicate INSERT, silently losing the split (and the original file is already
        // deleted by then).
        clips: [
          AudioClip(
            id: newId(),
            audioFilePath: parts.secondPath,
            durationMs: secondAnalysis.durationMs,
            startMs: atMs,
            waveformPeaks: secondAnalysis.peaks,
          ),
        ],
        // Relocating notes/markers onto a NEW track id needs fresh ids: the source track
        // is tombstoned (its child rows soft-deleted) but still holds these ids, so reusing
        // them would collide on insert and roll the whole save back.
        notes: src.notes
            .where((n) => n.positionMs >= atMs)
            .map((n) => n.copyWith(id: newId()))
            .toList(),
        techniqueMarkers: src.techniqueMarkers
            .where((m) => m.positionMs >= atMs)
            .map((m) => m.copyWith(id: newId()))
            .toList(),
        displayName: '${src.displayName} (2)',
        createdAt: DateTime.now(),
      );
      // Re-read the LIVE practice after the split/analyze awaits so a take
      // recorded/imported meanwhile isn't dropped by a stale-snapshot rebuild.
      final cur = _practice;
      if (cur == null) return;
      final list = [...cur.vocalTracks];
      final idx = list.indexWhere((t) => t.id == trackId);
      if (idx < 0) return; // source removed during the split
      list[idx] = first;
      list.insert(idx + 1, second);
      _practice = cur.copyWith(vocalTracks: list);
      _evictAnalysis(src.audioFilePath);
      await _audioFiles.deleteAudio(src.audioFilePath);
      await _save();
      _toast('Take split.');
    });
    // The surviving take keeps its id but now points at a new file.
    _onTrackAudioChanged(trackId);
  }

  /// Merges a vocal take with the take in the lane below it: their audio is
  /// concatenated (inverse of split) and the result sits at the upper clip's
  /// position. The lower take's notes shift to follow the joined audio.
  Future<void> mergeTrackDown(String trackId) async {
    final practice = _practice;
    if (practice == null) return;
    if (_busy || isSeparating) return; // no concurrent structural edit
    // Pick the partner from the VISIBLE (active-interpretation) order — the same list the
    // UI's "Merge down" affordance is gated on. Using the next take in the FULL vocalTracks
    // list could grab (and then delete) a take belonging to a DIFFERENT interpretation.
    final visible = practice.activeVocalTracks;
    final vIdx = visible.indexWhere((t) => t.id == trackId);
    if (vIdx < 0 || vIdx + 1 >= visible.length) {
      _toast('No take below to merge with.');
      return;
    }
    final top = visible[vIdx];
    final bottom = visible[vIdx + 1];
    // Byte-concatenating two primary files can't represent multi-clip lanes —
    // refuse rather than orphan the trailing clips' audio.
    if (top.clips.length > 1 || bottom.clips.length > 1) {
      _toast(
        'These takes have multiple clips and can\'t be merged into one lane.',
      );
      return;
    }
    await _runBusy(() async {
      final mergedPath = await _edits.concatWav(
        practiceId: _practiceId,
        firstPath: top.audioFilePath,
        secondPath: bottom.audioFilePath,
      );
      final analysis = await _waveforms.analyze(mergedPath);
      // Shift the bottom take's markers onto the merged timeline; range markers
      // shift their end by the same delta so the span is preserved.
      final delta = top.startMs + top.durationMs - bottom.startMs;
      // Fresh ids for the bottom take's notes/markers: the bottom track is tombstoned but
      // its child rows keep these ids, so reusing them on the merged (top) track would
      // collide on insert and roll the save back.
      final notes = [
        ...top.notes,
        for (final n in bottom.notes)
          n.copyWith(
            id: newId(),
            positionMs: n.positionMs + delta,
            endMs: n.endMs == null ? null : n.endMs! + delta,
          ),
      ]..sort((a, b) => a.positionMs.compareTo(b.positionMs));
      final markers = [
        ...top.techniqueMarkers,
        for (final m in bottom.techniqueMarkers)
          m.copyWith(
            id: newId(),
            positionMs: m.positionMs + delta,
            endMs: m.endMs == null ? null : m.endMs! + delta,
          ),
      ]..sort((a, b) => a.positionMs.compareTo(b.positionMs));
      final merged = top.copyWith(
        audioFilePath: mergedPath,
        durationMs: analysis.durationMs,
        waveformPeaks: analysis.peaks,
        notes: notes,
        techniqueMarkers: markers,
      );
      // Re-read the LIVE practice and re-find the indices after the concat/
      // analyze awaits (they shift if a take was added meanwhile) so the merge
      // never rebuilds from a stale snapshot and drops a concurrent take.
      final cur = _practice;
      if (cur == null) return;
      final newList = [...cur.vocalTracks];
      final idx = newList.indexWhere((t) => t.id == top.id);
      final bottomIdx = newList.indexWhere((t) => t.id == bottom.id);
      if (idx < 0 || bottomIdx < 0) return;
      newList[idx] = merged;
      newList.removeAt(bottomIdx);
      _practice = cur.copyWith(vocalTracks: newList);
      _expandedTracks.remove(bottom.id); // the absorbed take is gone
      _trackLanes.remove(bottom.id);
      _evictAnalysis(top.audioFilePath);
      _evictAnalysis(bottom.audioFilePath);
      await _audioFiles.deleteAudio(top.audioFilePath);
      await _audioFiles.deleteAudio(bottom.audioFilePath);
      await _save();
      _toast('Takes merged.');
    });
    // The merged take keeps the top take's id but now points at a new file.
    _onTrackAudioChanged(trackId);
  }

  /// Recomputes the waveform (and invalidates the cached spectrogram) for a
  /// track — useful after its audio file changed underneath.
  Future<void> reanalyzeTrack(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    await _runBusy(() async {
      final analysis = await _waveforms.analyze(track.audioFilePath);
      _updateTrack(
        trackId,
        (t) => t.copyWith(
          durationMs: analysis.durationMs,
          waveformPeaks: analysis.peaks,
        ),
      );
      _evictAnalysis(track.audioFilePath);
      await _save();
    });
    _onTrackAudioChanged(trackId);
  }

  Future<void> toggleMute(String trackId) async {
    _updateTrack(trackId, (t) => t.copyWith(muted: !t.muted));
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  Future<void> toggleSolo(String trackId) async {
    _updateTrack(trackId, (t) => t.copyWith(soloed: !t.soloed));
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  /// True if a track should be heard, given the resolved [solo] state. Only the
  /// *active* backing (accompaniment or reference, per [playReference]) is ever
  /// audible; the other backing is always silent. While recording, the take
  /// being punched into is silenced (you can't monitor what you're laying down)
  /// but every other lane still obeys mute/solo. Otherwise mute always silences
  /// and solo isolates.
  bool _audibleTrack(AudioTrack t, SoloResolution solo) {
    if (t.isAccompaniment && _playReference) return false;
    if (t.isReference && !_playReference) return false;
    if (_isRecording && t.id == _recordingTargetId) return false;
    if (t.muted) return false;
    return solo.trackInSolo(t.id);
  }

  /// True if a bus passes audio given the resolved [solo] state — muted always
  /// silences; otherwise the bus is heard only when it's part of the solo
  /// (soloed itself, fed by a soloed source, or carrying a soloed bus onward).
  bool _audibleBus(FxBus b, SoloResolution solo) =>
      !b.muted && solo.busInSolo(b.id);

  /// The mix's current solo state — see [resolveSolo].
  SoloResolution _resolveSolo() => resolveSolo(_allTracks, fxBuses);

  /// Pushes the current mute/solo state of every track *and* bus to the audio
  /// engine immediately, so M/S respond on click without waiting for the save
  /// round-trip.
  void _applyAudibility() {
    final solo = _resolveSolo();
    for (final t in _allTracks) {
      final muted = !_audibleTrack(t, solo);
      // The engine plays one source per clip — push the lane's level to each.
      for (final c in t.clips) {
        unawaited(_playback.setVolume(c.id, t.volume, muted: muted));
      }
    }
    for (final b in fxBuses) {
      _playback.setBusVolume(b.id, b.volume, muted: !_audibleBus(b, solo));
    }
  }

  /// Applies a volume change live (model + audio) without persisting — call on
  /// every slider tick. Persist once with [commitTrackVolume] on release.
  void setTrackVolumeLive(String trackId, double volume) {
    _updateTrack(trackId, (t) => t.copyWith(volume: volume));
    final track = _trackById(trackId);
    if (track != null) {
      final muted = !_audibleTrack(track, _resolveSolo());
      for (final c in track.clips) {
        unawaited(_playback.setVolume(c.id, track.volume, muted: muted));
      }
    }
    notifyListeners();
  }

  List<AudioTrack> get _allTracks {
    final practice = _practice;
    if (practice == null) return const [];
    return [
      ?practice.accompaniment,
      ?practice.reference,
      // Only the active interpretation's takes are loaded into playback/mixing.
      ...practice.activeVocalTracks,
    ];
  }

  /// Notes pinned to the currently selected track, sorted by time.
  List<PracticeNote> get selectedTrackNotes {
    final track = selectedTrack;
    if (track == null) return const [];
    return [...track.notes]
      ..sort((a, b) => a.positionMs.compareTo(b.positionMs));
  }

  /// The selected take's technique markers, sorted by position — for the right
  /// panel's Techniques tab.
  List<TechniqueMarker> get selectedTrackTechniques {
    final track = selectedTrack;
    if (track == null) return const [];
    return [...track.techniqueMarkers]
      ..sort((a, b) => a.positionMs.compareTo(b.positionMs));
  }

  Future<void> commitTrackVolume() => _save();

  /// Applies a pan change live (model + audio) without persisting — call on every
  /// knob tick. Persist once with [commitTrackVolume] on release.
  void setTrackPanLive(String trackId, double pan) {
    final p = pan.clamp(-1.0, 1.0);
    _updateTrack(trackId, (t) => t.copyWith(pan: p));
    final track = _trackById(trackId);
    if (track != null) {
      for (final c in track.clips) {
        _playback.setTrackPan(c.id, p);
      }
    }
    notifyListeners();
  }

  // --- Effects (EQ / compressor / shared reverb) ---------------------------

  /// Whether the active playback engine can apply effects (only the native engine can).
  bool get supportsEffects => _playback.supportsEffects;

  /// The shared reverb settings for this practice (the default until edited).
  ReverbSettings get reverbSettings =>
      _practice?.reverb ?? const ReverbSettings();

  /// The selected vocal take's effect chain (null for backing or an unconfigured take).
  TrackFx? get selectedTrackFx => selectedTrack?.fx;

  /// Whether the FX detail (vs Analysis) is shown in the inspector.
  bool _inspectorShowFx = false;
  bool get inspectorShowFx => _inspectorShowFx;
  void setInspectorShowFx(bool show) {
    if (_inspectorShowFx == show) return;
    _inspectorShowFx = show;
    notifyListeners();
  }

  /// Live FX edit for a track (model + engine) without persisting — call on every drag
  /// tick; persist once with [commitFx] on release.
  void setTrackFxLive(String trackId, TrackFx fx) {
    _updateTrack(trackId, (t) => t.copyWith(fx: fx));
    // The lane's FX applies to each of its clip sources in the engine.
    final track = _trackById(trackId);
    if (track != null) {
      for (final c in track.clips) {
        _playback.setTrackFx(c.id, fx);
      }
    }
    notifyListeners();
  }

  /// Live shared-reverb edit (model + engine) without persisting.
  void setReverbLive(ReverbSettings reverb) {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(reverb: reverb);
    _playback.setReverb(reverb);
    notifyListeners();
  }

  Future<void> commitFx() => _save();

  /// Reorders a vocal take's insert chain (the audible plugin order). Persists +
  /// rebuilds the engine graph (the chain order is graph wiring).
  Future<void> reorderTrackInserts(String trackId, List<FxKind> order) async {
    final track = _trackById(trackId);
    if (track == null) return;
    final fx = (track.fx ?? TrackFx.flat()).copyWith(order: order);
    _updateTrack(trackId, (t) => t.copyWith(fx: fx));
    await _save();
    await _resyncPlayback();
  }

  /// Reorders an FX bus's insert chain. Persists + rebuilds the graph.
  Future<void> reorderBusInserts(String busId, List<FxKind> order) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(fx: b.fx.copyWith(order: order)) else b,
      ],
    );
    await _save();
    await _resyncPlayback();
  }

  // --- Aux sends (post-fader, to FX buses) --------------------------------

  /// Adds a post-fader send from a take to [busId] (max [kMaxSends], no dupes).
  Future<void> addTrackSend(String trackId, String busId) async {
    final track = _trackById(trackId);
    if (track == null ||
        track.sends.length >= kMaxSends ||
        track.sends.any((s) => s.busId == busId)) {
      return;
    }
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        sends: [
          ...t.sends,
          TrackSend(busId: busId),
        ],
      ),
    );
    await _save();
    await _resyncPlayback();
  }

  Future<void> removeTrackSend(String trackId, int slot) async {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    final next = [...track.sends]..removeAt(slot);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    await _save();
    await _resyncPlayback();
  }

  Future<void> setTrackSendTarget(
    String trackId,
    int slot,
    String busId,
  ) async {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    final next = [...track.sends];
    next[slot] = next[slot].copyWith(busId: busId);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    await _save();
    await _resyncPlayback();
  }

  /// Live send-level edit (model + engine) without persisting.
  void setTrackSendLevelLive(String trackId, int slot, double levelDb) {
    final track = _trackById(trackId);
    if (track == null || slot < 0 || slot >= track.sends.length) return;
    final next = [...track.sends];
    next[slot] = next[slot].copyWith(levelDb: levelDb);
    _updateTrack(trackId, (t) => t.copyWith(sends: next));
    for (final c in track.clips) {
      _playback.setTrackSendLevel(c.id, slot, levelDb);
    }
    notifyListeners();
  }

  List<FxBus> _withBus(String busId, FxBus Function(FxBus) update) => [
    for (final b in fxBuses)
      if (b.id == busId) update(b) else b,
  ];

  Future<void> addBusSend(String busId, String targetBusId) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        busId == targetBusId ||
        bus.sends.length >= kMaxSends ||
        bus.sends.any((s) => s.busId == targetBusId)) {
      return;
    }
    if (_busRouteWouldCycle(busId, targetBusId)) {
      _toast("Can't send there — it would create a feedback loop.");
      return;
    }
    _practice = practice.copyWith(
      fxBuses: _withBus(
        busId,
        (b) => b.copyWith(
          sends: [
            ...b.sends,
            TrackSend(busId: targetBusId),
          ],
        ),
      ),
    );
    await _save();
    await _resyncPlayback();
  }

  Future<void> removeBusSend(String busId, int slot) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    _practice = practice.copyWith(
      fxBuses: _withBus(
        busId,
        (b) => b.copyWith(sends: [...b.sends]..removeAt(slot)),
      ),
    );
    await _save();
    await _resyncPlayback();
  }

  Future<void> setBusSendTarget(
    String busId,
    int slot,
    String targetBusId,
  ) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        busId == targetBusId ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    if (_busRouteWouldCycle(busId, targetBusId)) {
      _toast("Can't send there — it would create a feedback loop.");
      return;
    }
    _practice = practice.copyWith(
      fxBuses: _withBus(busId, (b) {
        final next = [...b.sends];
        next[slot] = next[slot].copyWith(busId: targetBusId);
        return b.copyWith(sends: next);
      }),
    );
    await _save();
    await _resyncPlayback();
  }

  void setBusSendLevelLive(String busId, int slot, double levelDb) {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null ||
        bus == null ||
        slot < 0 ||
        slot >= bus.sends.length) {
      return;
    }
    _practice = practice.copyWith(
      fxBuses: _withBus(busId, (b) {
        final next = [...b.sends];
        next[slot] = next[slot].copyWith(levelDb: levelDb);
        return b.copyWith(sends: next);
      }),
    );
    _playback.setBusSendLevel(busId, slot, levelDb);
    notifyListeners();
  }

  // --- Master bus ----------------------------------------------------------

  /// The master bus output gain (linear, 1.0 = unity) for the whole mix.
  double get masterVolume => _practice?.masterVolume ?? 1.0;

  /// Live master-volume edit (model + engine) without persisting — call on every
  /// fader tick; persist once with [commitMasterVolume] on release.
  void setMasterVolumeLive(double gain) {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(masterVolume: gain);
    _playback.setMasterVolume(gain);
    notifyListeners();
  }

  Future<void> commitMasterVolume() => _save();

  // --- FX buses (DAW group / aux tracks) ----------------------------------

  List<FxBus> get fxBuses => _practice?.fxBuses ?? const [];

  /// The bus whose strip is selected for FX editing in the inspector. Null when a
  /// take (or nothing) is selected.
  String? _selectedBusId;
  String? get selectedBusId => _selectedBusId;
  FxBus? get selectedBus => _practice?.busById(_selectedBusId);

  /// Selecting a bus takes over the inspector FX panel; selecting a track clears it.
  void selectBus(String? busId) {
    if (_selectedBusId == busId) return;
    _selectedBusId = busId;
    // Bus and track selection are mutually exclusive (the inspector owns one or
    // the other) — selecting a bus must clear the track/clip selection, or two
    // lanes highlight at once. createBus/duplicateBus already do this.
    if (busId != null) {
      _selectedTrackId = null;
      _selectedClipId = null;
    }
    notifyListeners();
  }

  /// Creates a new (transparent) FX bus and selects it. Returns its id.
  Future<String> createBus([String name = '']) async {
    final practice = _practice;
    if (practice == null) return '';
    final trimmed = name.trim();
    final bus = FxBus(
      id: newId(),
      name: trimmed.isEmpty ? 'Bus ${practice.fxBuses.length + 1}' : trimmed,
      fx: TrackFx.flat(),
    );
    _practice = practice.copyWith(fxBuses: [...practice.fxBuses, bus]);
    _selectedBusId = bus.id;
    _selectedTrackId = null;
    _selectedClipId = null;
    await _save();
    await _resyncPlayback();
    return bus.id;
  }

  Future<void> renameBus(String busId, String name) async {
    final practice = _practice;
    final trimmed = name.trim();
    if (practice == null || trimmed.isEmpty) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(name: trimmed) else b,
      ],
    );
    await _save();
  }

  /// Deletes a bus: un-routes any take feeding it and any bus whose output points
  /// at it (both fall back to the master), then removes it. Triggers a rebuild.
  Future<void> deleteBus(String busId) async {
    final practice = _practice;
    if (practice == null) return;
    final tracks = [
      for (final t in practice.vocalTracks)
        t.copyWith(
          clearBus: t.busId == busId,
          sends: t.sends.where((s) => s.busId != busId).toList(),
        ),
    ];
    final buses = [
      for (final b in practice.fxBuses)
        if (b.id != busId)
          b.copyWith(
            clearOutput: b.outputBusId == busId,
            sends: b.sends.where((s) => s.busId != busId).toList(),
          ),
    ];
    if (_selectedBusId == busId) _selectedBusId = null;
    _practice = practice.copyWith(vocalTracks: tracks, fxBuses: buses);
    await _save();
    await _resyncPlayback();
  }

  /// Routes a vocal take's output to [busId] (null = master). Triggers a rebuild.
  Future<void> setTrackBus(String trackId, String? busId) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(busId: busId, clearBus: busId == null),
    );
    await _save();
    await _resyncPlayback();
  }

  /// Whether routing [fromBusId] into [toBusId] (as its output or an aux send)
  /// would close a feedback loop — i.e. [toBusId] can already reach [fromBusId]
  /// through bus outputs and aux sends. The engine REJECTS a cyclic graph and
  /// silently keeps the old one, so the routing UI must refuse the edit instead.
  bool _busRouteWouldCycle(String fromBusId, String? toBusId) {
    final practice = _practice;
    if (toBusId == null || practice == null) return false;
    if (toBusId == fromBusId) return true;
    final byId = {for (final b in practice.fxBuses) b.id: b};
    final seen = <String>{};
    final queue = <String>[toBusId];
    while (queue.isNotEmpty) {
      final id = queue.removeLast();
      if (id == fromBusId) return true;
      if (!seen.add(id)) continue;
      final b = byId[id];
      if (b == null) continue;
      if (b.outputBusId != null) queue.add(b.outputBusId!);
      for (final s in b.sends) {
        queue.add(s.busId);
      }
    }
    return false;
  }

  /// Routes a bus's output to another bus, or to the master ([outputBusId] null).
  /// Self-routing and feedback loops are refused. Rebuilds.
  Future<void> setBusOutput(String busId, String? outputBusId) async {
    final practice = _practice;
    if (practice == null || busId == outputBusId) return;
    if (_busRouteWouldCycle(busId, outputBusId)) {
      _toast("Can't route there — it would create a feedback loop.");
      return;
    }
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId)
            b.copyWith(
              outputBusId: outputBusId,
              clearOutput: outputBusId == null,
            )
          else
            b,
      ],
    );
    await _save();
    await _resyncPlayback();
  }

  /// Live bus FX edit (model + engine) without persisting — persist with [commitFx].
  void setBusFxLive(String busId, TrackFx fx) {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(fx: fx) else b,
      ],
    );
    _playback.setBusFx(busId, fx);
    notifyListeners();
  }

  /// Live bus fader edit (model + engine) without persisting.
  void setBusVolumeLive(String busId, double volume) {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    final updated = bus.copyWith(volume: volume);
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) updated else b,
      ],
    );
    _playback.setBusVolume(
      busId,
      volume,
      muted: !_audibleBus(updated, _resolveSolo()),
    );
    notifyListeners();
  }

  /// Live bus pan edit (model + engine) without persisting.
  void setBusPanLive(String busId, double pan) {
    final practice = _practice;
    if (practice == null) return;
    final p = pan.clamp(-1.0, 1.0);
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(pan: p) else b,
      ],
    );
    _playback.setBusPan(busId, p);
    notifyListeners();
  }

  Future<void> toggleBusMute(String busId) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(muted: !b.muted) else b,
      ],
    );
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  /// Toggles a bus's solo. Soloing a bus isolates it (and the takes feeding it)
  /// across the whole mix; see `_resolveSolo`.
  Future<void> toggleBusSolo(String busId) async {
    final practice = _practice;
    final bus = practice?.busById(busId);
    if (practice == null || bus == null) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(soloed: !b.soloed) else b,
      ],
    );
    _applyAudibility();
    notifyListeners();
    await _save();
  }

  /// Sets a bus's lane tint (ARGB). Purely visual; persisting it goes through
  /// the shared save path (which resyncs the graph, harmlessly) like
  /// [setTrackColor].
  Future<void> setBusColor(String busId, int value) async {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(
      fxBuses: [
        for (final b in practice.fxBuses)
          if (b.id == busId) b.copyWith(colorValue: value) else b,
      ],
    );
    notifyListeners();
    await _save();
  }

  /// Duplicates a bus (its chain, color, level and routing) and inserts the copy
  /// right after it, then selects it. The copy is never born soloed.
  Future<void> duplicateBus(String busId) async {
    final practice = _practice;
    final src = practice?.busById(busId);
    if (practice == null || src == null) return;
    final copy = FxBus(
      id: newId(),
      name: '${src.name} copy',
      fx: src.fx,
      colorValue: src.colorValue,
      volume: src.volume,
      muted: src.muted,
      pan: src.pan,
      outputBusId: src.outputBusId,
      sends: src.sends,
    );
    final list = [...practice.fxBuses];
    final idx = list.indexWhere((b) => b.id == busId);
    list.insert(idx < 0 ? list.length : idx + 1, copy);
    _practice = practice.copyWith(fxBuses: list);
    _selectedBusId = copy.id;
    _selectedTrackId = null;
    await _save();
    await _resyncPlayback();
  }

  /// The bus compressor's current gain reduction (dB, >= 0) — for a meter.
  double busGainReductionDb(String busId) =>
      _playback.busGainReductionDb(busId);

  /// Every bus id that flows into [busId] (itself plus any bus that transitively
  /// routes its output into it) — so a take feeding an upstream bus still counts
  /// toward this bus's composite waveform.
  Set<String> _busFeeders(String busId) {
    final result = <String>{busId};
    var changed = true;
    while (changed) {
      changed = false;
      for (final b in fxBuses) {
        if (!result.contains(b.id) &&
            b.outputBusId != null &&
            result.contains(b.outputBusId)) {
          result.add(b.id);
          changed = true;
        }
      }
    }
    return result;
  }

  /// A composite waveform for an FX bus lane: the max-envelope of every audible
  /// take routed (directly or via an upstream bus) into [busId], scaled by each
  /// take's volume. Mirrors [masterWaveformPeaks] but filtered to the bus.
  List<double> busWaveformPeaks(String busId) {
    final tl = _practice?.timelineDurationMs ?? 0;
    if (tl <= 0) return const [];
    const n = 1000;
    final out = List<double>.filled(n, 0.0);
    final feeders = _busFeeders(busId);
    final solo = _resolveSolo();

    void fold(List<double> peaks, int startMs, int durationMs, double vol) {
      if (peaks.isEmpty || durationMs <= 0 || vol <= 0) return;
      for (var i = 0; i < peaks.length; i++) {
        final tms = startMs + (i + 0.5) / peaks.length * durationMs;
        final b = (tms / tl * n).floor().clamp(0, n - 1);
        final v = peaks[i] * vol;
        if (v > out[b]) out[b] = v;
      }
    }

    for (final t in _allTracks) {
      if (t.isBacking || !t.hasClip) continue;
      if (t.busId == null || !feeders.contains(t.busId)) continue;
      if (!_audibleTrack(t, solo)) continue;
      for (final c in t.clips) {
        fold(c.waveformPeaks, c.startMs, c.durationMs, t.volume);
      }
    }
    return out;
  }

  /// A composite "master" waveform for the master lane: the loudest audible
  /// track envelope (scaled by each track's volume and the master gain) at every
  /// point on the timeline, plus the live take while recording. It approximates
  /// the downmix for display and recomputes reactively as tracks/levels change.
  List<double> get masterWaveformPeaks {
    final tl = _practice?.timelineDurationMs ?? 0;
    if (tl <= 0) return const [];
    const n = 1000;
    final out = List<double>.filled(n, 0.0);
    final solo = _resolveSolo();

    void fold(List<double> peaks, int startMs, int durationMs, double vol) {
      if (peaks.isEmpty || durationMs <= 0 || vol <= 0) return;
      for (var i = 0; i < peaks.length; i++) {
        final tms = startMs + (i + 0.5) / peaks.length * durationMs;
        final b = (tms / tl * n).floor().clamp(0, n - 1);
        final v = peaks[i] * vol;
        if (v > out[b]) out[b] = v;
      }
    }

    for (final t in _allTracks) {
      if (!t.hasClip || !_audibleTrack(t, solo)) continue;
      // Fold every clip of the take (not just the primary), so trailing punched-in
      // clips of a multi-clip take show in the master envelope — matching busWaveformPeaks.
      for (final c in t.clips) {
        fold(c.waveformPeaks, c.startMs, c.durationMs, t.volume);
      }
    }
    // The take being laid down isn't a track yet — fold its live envelope in.
    if (_isRecording) {
      fold(recordingPeaks.value, _recordingStartMs, recordingDurationMs, 1.0);
    }

    final m = masterVolume;
    if (m != 1.0) {
      for (var b = 0; b < n; b++) {
        out[b] *= m;
      }
    }
    return out;
  }

  /// The vocal compressor's current gain reduction (dB) for a track — for a live meter.
  double gainReductionDb(String trackId) {
    // The FX meter reads the lane's primary clip source.
    final clip = _trackById(trackId)?.primaryClip;
    return clip == null ? 0 : _playback.trackGainReductionDb(clip.id);
  }

  AudioTrack? _trackById(String trackId) {
    final practice = _practice;
    if (practice == null) return null;
    if (practice.accompaniment?.id == trackId) return practice.accompaniment;
    if (practice.reference?.id == trackId) return practice.reference;
    for (final t in practice.vocalTracks) {
      if (t.id == trackId) return t;
    }
    return null;
  }

  // --- Notes ---------------------------------------------------------------

  /// Adds a note at [startMs] (a point), or across [startMs]..[endMs] when
  /// [endMs] is given and the span is long enough.
  Future<void> addNote(
    String trackId,
    int startMs,
    int? endMs,
    String content,
  ) async {
    final now = DateTime.now();
    final (s, e) = normalizeMarkerRange(startMs, endMs);
    final note = PracticeNote(
      id: newId(),
      positionMs: s,
      endMs: e,
      content: content,
      createdAt: now,
      updatedAt: now,
    );
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        notes: [...t.notes, note]
          ..sort((a, b) => a.positionMs.compareTo(b.positionMs)),
      ),
    );
    await _save();
  }

  Future<void> addNoteAtPlayhead(String trackId, String content) =>
      addNote(trackId, position.value.inMilliseconds, null, content);

  Future<void> updateNote(String trackId, PracticeNote note) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        notes: [
          for (final n in t.notes)
            if (n.id == note.id) note else n,
        ],
      ),
    );
    await _save();
  }

  Future<void> deleteNote(String trackId, String noteId) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(notes: t.notes.where((n) => n.id != noteId).toList()),
    );
    await _save();
  }

  // --- Technique markers ---------------------------------------------------

  /// The user's technique library, for the "add technique" picker. Returns an
  /// empty list if it fails to load.
  Future<List<Technique>> availableTechniques() async {
    final result = await _techniqueRepository.listTechniques();
    return result is Ok<List<Technique>> ? result.value : const [];
  }

  /// Adds a technique marker at [startMs] (a point), or across
  /// [startMs]..[endMs] when [endMs] is given and the span is long enough.
  Future<void> addTechnique(
    String trackId,
    String techniqueId,
    String techniqueName,
    int startMs,
    int? endMs,
  ) async {
    final now = DateTime.now();
    final (s, e) = normalizeMarkerRange(startMs, endMs);
    final marker = TechniqueMarker(
      id: newId(),
      positionMs: s,
      endMs: e,
      techniqueId: techniqueId,
      techniqueName: techniqueName,
      createdAt: now,
      updatedAt: now,
    );
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        techniqueMarkers: [...t.techniqueMarkers, marker]
          ..sort((a, b) => a.positionMs.compareTo(b.positionMs)),
      ),
    );
    await _save();
  }

  Future<void> addTechniqueAtPlayhead(
    String trackId,
    String techniqueId,
    String techniqueName,
  ) => addTechnique(
    trackId,
    techniqueId,
    techniqueName,
    position.value.inMilliseconds,
    null,
  );

  Future<void> deleteTechniqueMarker(String trackId, String markerId) async {
    _updateTrack(
      trackId,
      (t) => t.copyWith(
        techniqueMarkers: t.techniqueMarkers
            .where((m) => m.id != markerId)
            .toList(),
      ),
    );
    await _save();
  }

  // --- Internals -----------------------------------------------------------

  void _updateTrack(String trackId, AudioTrack Function(AudioTrack) update) {
    final practice = _practice;
    if (practice == null) return;
    if (practice.accompaniment?.id == trackId) {
      _practice = practice.copyWith(
        accompaniment: update(practice.accompaniment!),
      );
    } else if (practice.reference?.id == trackId) {
      _practice = practice.copyWith(reference: update(practice.reference!));
    } else {
      _practice = practice.copyWith(
        vocalTracks: [
          for (final t in practice.vocalTracks)
            if (t.id == trackId) update(t) else t,
        ],
      );
    }
  }

  /// Persists [_practice]. Returns whether the write succeeded — callers that
  /// then delete files must gate the deletion on a successful save so a failed
  /// write can't leave the JSON referencing audio that's already gone.
  Future<bool> _save() async {
    if (_practice == null) return false;
    _ensureTimeline();
    final result = await _repository.savePractice(_practice!);
    final ok = result is Ok<SongPractice>;
    // If the route was popped mid-save (e.g. a second back while finalizing a
    // recording), the write above still committed — the take is safe — but the
    // playback/selection resync below must not touch now-disposed notifiers.
    if (_disposed) return ok;
    if (ok) {
      _practice = result.value;
    } else {
      // Revert the optimistic in-memory model to what's actually on disk, so the
      // UI never keeps showing edits a failed write didn't persist.
      final reloaded = await _repository.getPractice(_practice!.id);
      if (reloaded is Ok<SongPractice>) _practice = reloaded.value;
      _toast("Couldn't save your changes.");
    }
    _ensureSelection();
    _ensureLoopRange();
    await _resyncPlayback();
    notifyListeners();
    return ok;
  }

  void _ensureSelection() {
    final practice = _practice;
    if (practice == null) return;
    // A selected bus owns the inspector; track and bus selection are mutually
    // exclusive, so don't force a track back in (that would highlight two lanes
    // at once after creating/duplicating a bus).
    if (_selectedBusId != null) return;
    final ids = <String>[
      if (practice.accompaniment != null) practice.accompaniment!.id,
      if (practice.reference != null) practice.reference!.id,
      ...practice.activeVocalTracks.map((t) => t.id),
    ];
    if (_selectedTrackId == null || !ids.contains(_selectedTrackId)) {
      _selectedTrackId = ids.isEmpty ? null : ids.first;
    }
  }

  Future<void> _resyncPlayback() async {
    final practice = _practice;
    if (practice == null) return;
    final allTracks = _allTracks;

    final solo = _resolveSolo();
    // Flatten lanes to one engine source per clip (id = clip id); the lane's
    // volume / mute / FX apply to all of its clips. Empty takes contribute none.
    final tracks = <TrackPlayback>[];
    for (final t in allTracks) {
      final muted = !_audibleTrack(t, solo);
      for (final c in t.clips) {
        if (c.audioFilePath.isEmpty) continue;
        tracks.add(
          TrackPlayback(
            id: c.id,
            path: c.audioFilePath,
            startMs: c.startMs,
            durationMs: c.durationMs,
            volume: t.volume,
            muted: muted,
            pan: t.pan,
            isVocal: !t.isBacking,
            fx: t.fx,
            busId: t.isBacking ? null : t.busId,
            sends: t.isBacking ? const [] : t.sends,
          ),
        );
      }
    }
    final buses = [
      for (final b in practice.fxBuses)
        BusPlayback(
          id: b.id,
          fx: b.fx,
          volume: b.volume,
          muted: !_audibleBus(b, solo),
          pan: b.pan,
          outputBusId: b.outputBusId,
          sends: b.sends,
        ),
    ];
    _playback.setReverb(reverbSettings);
    _playback.setMasterVolume(masterVolume);
    await _playback.sync(tracks, buses);
  }

  Future<void> _runBusy(Future<void> Function() action) async {
    _busy = true;
    _errorMessage = null;
    notifyListeners();
    try {
      await action();
    } catch (e) {
      _errorMessage = e.toString();
      _toast(_friendlyError(e));
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    RecordingFinalizer.instance.unregister(_quitFinalize);
    // Close any in-flight recording's native WAV handle so it isn't leaked or
    // left truncated (the take itself should be finalized before dispose via the
    // screen's PopScope; this is the safety net).
    if (_isRecording) {
      _graphEngine?.stopInputRecording();
      _isRecording = false;
    }
    // Cancel an in-flight separation so its Python/torch/ffmpeg subprocess is
    // killed rather than orphaned when the user leaves mid-separation.
    if (isSeparating) _separation.cancel();
    _positionSub?.cancel();
    _playingSub?.cancel();
    _recLevelTimer?.cancel();
    _captureTimer?.cancel();
    _inputLevelTimer?.cancel();
    _deviceWatch?.cancel();
    position.dispose();
    recordLevel.dispose();
    inputLevel.dispose();
    recordingPeaks.dispose();
    unawaited(_toasts.close());
    if (_playback is GraphAudioPlaybackService) {
      _appSettings.detachEngine(_playback);
    }
    unawaited(_playback.dispose());
    _monitor.dispose();
    super.dispose();
  }
}
