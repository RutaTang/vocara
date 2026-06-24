import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/app_settings_service.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/media_analysis_service.dart';
import '../../../data/services/mic_permission.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/recording_finalizer.dart';
import '../../../data/services/technique_media_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/technique.dart';
import '../../../domain/models/technique_analysis.dart';
import '../../../domain/models/technique_media.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/command.dart';
import '../../../utils/id.dart';
import '../../../utils/result.dart';
import '../../song_practice_detail/view_model/analysis_source.dart';

/// Which clip the analysis card is showing: the technique's reference example
/// ("ground truth") or the user's just-recorded practice take.
enum TechniqueAnalysisSubject { reference, practice }

/// A seek request the analysis pushes to the inline player. Unlike a plain
/// [ValueNotifier], it re-notifies even when the new target equals the previous
/// one — a repeat scrub (scrubbing back to a time the player has since played
/// past) must still move the player, not be swallowed by an `==` guard.
class _SeekSignal extends ChangeNotifier implements ValueListenable<Duration> {
  Duration _value = Duration.zero;

  @override
  Duration get value => _value;

  void request(Duration to) {
    _value = to;
    notifyListeners();
  }
}

/// ViewModel for a single technique's detail page: view/edit its text fields,
/// manage its audio/video example clips, compute the reference "ground truth"
/// analysis from a selected example, and run an ephemeral practice recording to
/// compare against it.
///
/// Media edits made while editing are STAGED (added/removed/renamed/recorded in
/// a draft) and only persisted when the form is saved — the same buffered model
/// as the text fields, so nothing touches the stored technique until the user
/// explicitly saves. Cancelling discards the draft (and any files copied for it).
///
/// Implements [AnalysisSource] so the same bottom-dock analysis views (spectro-
/// gram / pitch / timbre / vibrato / register / range / dynamics) render here,
/// switching between the reference and practice subjects — and showing the live
/// mic in real time while a recording is in progress.
class TechniqueDetailViewModel extends ChangeNotifier
    implements AnalysisSource {
  TechniqueDetailViewModel({
    required TechniqueRepository repository,
    required TechniqueMediaService mediaService,
    required this.techniqueId,
    AppSettingsService? appSettings,
    MediaAnalysisService analysisService = const MediaAnalysisService(),
  }) : _repository = repository,
       _mediaService = mediaService,
       _appSettings = appSettings,
       _analysis = analysisService {
    load = Command0(_load)..execute();
    // Finalize an in-flight example/practice recording if the app is quit
    // mid-record (no-op otherwise); unregistered in dispose.
    RecordingFinalizer.instance.register(_quitFinalize);
  }

  // Registered with RecordingFinalizer (saves the captured example/practice take
  // on app quit). No-op unless a recording is in flight. (A method tear-off is a
  // stable reference, so register/unregister match.)
  Future<void> _quitFinalize() async {
    if (isRecordingExample) {
      await stopExampleRecording();
    } else if (isRecordingPractice) {
      await stopPractice();
    }
  }

  final TechniqueRepository _repository;
  final TechniqueMediaService _mediaService;
  final MediaAnalysisService _analysis;
  final AppSettingsService? _appSettings;
  final String techniqueId;

  /// Loads the technique (runs once on creation).
  late final Command0<Technique> load;

  Technique? _technique;
  Technique? get technique => _technique;

  /// True while example files are being copied into the sandbox.
  bool _importingMedia = false;
  bool get importingMedia => _importingMedia;

  Future<Result<Technique>> _load() async {
    final result = await _repository.getTechnique(techniqueId);
    if (result is Ok<Technique>) {
      _technique = result.value;
      notifyListeners();
    }
    return result;
  }

  // --- Staged media draft (committed on save) ------------------------------

  /// Example clips added (recorded or imported) during the current edit but not
  /// yet persisted. Their files already live in the sandbox; they are committed
  /// on save and their files are deleted if the edit is cancelled.
  final List<TechniqueMedia> _draftAdds = [];

  /// Ids of already-persisted examples staged for removal; their files are
  /// deleted only once the technique-without-them persists.
  final Set<String> _draftRemoves = {};

  /// Staged caption changes (media id -> new label), applied on save.
  final Map<String, String> _draftLabels = {};

  /// True when the media draft differs from what's persisted, so Cancel knows to
  /// warn about unsaved example changes.
  bool get hasMediaDraft =>
      _draftAdds.isNotEmpty ||
      _draftRemoves.isNotEmpty ||
      _draftLabels.isNotEmpty;

  /// The example list as the user currently sees it: persisted media (minus
  /// staged removals, with staged captions) followed by staged additions. In the
  /// read view the draft is empty, so this is just the technique's media.
  List<TechniqueMedia> get effectiveMedia {
    final base = _technique?.media ?? const <TechniqueMedia>[];
    final out = <TechniqueMedia>[];
    for (final m in base) {
      if (_draftRemoves.contains(m.id)) continue;
      final label = _draftLabels[m.id];
      out.add(label == null ? m : m.copyWith(label: label));
    }
    for (final m in _draftAdds) {
      final label = _draftLabels[m.id];
      out.add(label == null ? m : m.copyWith(label: label));
    }
    return out;
  }

  /// Resets the draft when an edit session opens (defensive: there shouldn't be
  /// a stale one).
  void beginMediaDraft() {
    _draftAdds.clear();
    _draftRemoves.clear();
    _draftLabels.clear();
  }

  /// Drops the media draft and deletes any files copied for staged additions
  /// (they were never committed). Called when an edit is cancelled.
  Future<void> discardMediaDraft() async {
    // Releasing edit must release the mic: stop and discard any in-flight
    // example recording (otherwise the mic + capture/level timers run on until
    // the route is popped).
    if (_isRecording) {
      _endRecordingHardware();
      _recordingExample = false;
      _starting = false;
      await _deleteTempDir(_exampleTempPath);
      _exampleTempPath = null;
    }
    final orphans = List<TechniqueMedia>.from(_draftAdds);
    _draftAdds.clear();
    _draftRemoves.clear();
    _draftLabels.clear();
    for (final m in orphans) {
      _refCache.remove(m.path);
      _refFailed.remove(m.path);
      _loadingPaths.remove(m.path);
      await _mediaService.deleteMedia(m.path);
    }
    _fixSelectionAfterMediaChange();
    notifyListeners();
  }

  // --- Reference analysis (ground truth) -----------------------------------

  /// All analyzable (audio) examples, in display order. Video clips aren't
  /// analyzed.
  List<TechniqueMedia> get audioExamples => [
    for (final m in effectiveMedia)
      if (!m.isVideo) m,
  ];

  /// Path of the example whose analysis is shown. Null falls back to the first
  /// audio example.
  String? _selectedExamplePath;

  /// The example clip used as the reference "ground truth": the selected audio
  /// example, or the first one. Null when the technique has no audio example.
  TechniqueMedia? get referenceMedia {
    final audio = audioExamples;
    if (audio.isEmpty) return null;
    for (final m in audio) {
      if (m.path == _selectedExamplePath) return m;
    }
    return audio.first;
  }

  /// Whether the selected example's inline analysis is collapsed. Only the
  /// selected example can be expanded (accordion), so this one flag suffices.
  bool _analysisCollapsed = false;

  /// The example whose inline analysis is currently expanded, or null when
  /// collapsed.
  String? get expandedExamplePath =>
      _analysisCollapsed ? null : referenceMedia?.path;

  /// Toggles a card's inline analysis: collapse/expand the selected one, or
  /// select-and-expand a different one.
  void toggleExampleExpanded(String path) {
    if (referenceMedia?.path == path) {
      _analysisCollapsed = !_analysisCollapsed;
      notifyListeners();
    } else {
      _analysisCollapsed = false;
      selectExample(path);
    }
  }

  /// Switches which example's analysis is shown (and which drives the playhead).
  /// Drops any practice take, since it was compared against the old reference.
  void selectExample(String path) {
    if (referenceMedia?.path == path) return;
    _selectedExamplePath = path;
    _resetToReferenceSubject();
    notifyListeners();
    unawaited(ensureReferenceAnalysis());
  }

  /// A new/different reference example became selected: drop any practice take (it was
  /// recorded against the OLD reference, so pairing it with the new one in the gap table
  /// would mislead) and point the analysis views at the reference.
  void _resetToReferenceSubject() {
    _practice = null;
    _practiceFailed = false;
    _subject = TechniqueAnalysisSubject.reference;
    _syncPositionToSubject();
  }

  /// Cached analysis per example path, so switching back to an example is
  /// instant and a changed reference recomputes while an unchanged one doesn't.
  final Map<String, TechniqueAnalysis> _refCache = {};

  /// Paths whose analysis was attempted but produced nothing (undecodable file).
  final Set<String> _refFailed = {};

  /// Paths whose analysis is currently in flight.
  final Set<String> _loadingPaths = {};

  TechniqueAnalysis? get _selectedReference {
    final path = referenceMedia?.path;
    return path == null ? null : _refCache[path];
  }

  /// The reference analysis of [referenceMedia], or null until computed.
  TechniqueAnalysis? get referenceAnalysis => _selectedReference;

  bool get referenceAnalysisLoading =>
      _loadingPaths.contains(referenceMedia?.path);

  /// True when analysis of the selected clip was attempted but produced nothing.
  bool get referenceAnalysisFailed {
    final path = referenceMedia?.path;
    return path != null && _refFailed.contains(path);
  }

  /// Computes the reference analysis from [referenceMedia] if it hasn't been
  /// done for the current clip. Safe to call repeatedly (e.g. from build).
  Future<void> ensureReferenceAnalysis() async {
    final media = referenceMedia;
    if (media == null) return;
    final path = media.path;
    if (_refCache.containsKey(path) ||
        _refFailed.contains(path) ||
        _loadingPaths.contains(path)) {
      return;
    }
    _loadingPaths.add(path);
    notifyListeners();

    // analyze() can THROW (native decode / isolate-spawn failure), not just
    // return null. Without try/finally the path would stay in _loadingPaths
    // forever — the spinner sticks AND the contains-guard above blocks every
    // retry. Always clear the loading flag and latch failure.
    try {
      final result = await _analysis.analyze(path);
      if (result != null) {
        _refCache[path] = result;
      } else {
        _refFailed.add(path);
      }
    } catch (_) {
      _refFailed.add(path);
    } finally {
      _loadingPaths.remove(path);
    }
    // Place the playhead if this is still the shown reference.
    if (referenceMedia?.path == path &&
        _subject == TechniqueAnalysisSubject.reference) {
      _syncPositionToSubject();
    }
    notifyListeners();
  }

  // --- Analysis subject (reference vs practice) ----------------------------

  TechniqueAnalysisSubject _subject = TechniqueAnalysisSubject.reference;
  TechniqueAnalysisSubject get analysisSubject => _subject;

  /// Switches the analysis card between the reference and the practice take.
  void showAnalysisSubject(TechniqueAnalysisSubject subject) {
    if (_subject == subject) return;
    _subject = subject;
    _syncPositionToSubject();
    notifyListeners();
  }

  TechniqueAnalysis? get _activeAnalysis =>
      _subject == TechniqueAnalysisSubject.practice
      ? _practice
      : _selectedReference;

  // --- Mic recording (shared core) -----------------------------------------

  GraphAudioPlaybackService? _engine;
  Timer? _levelTimer;
  Timer? _captureTimer;
  bool _isRecording = false;
  bool _recordingExample = false;
  // Synchronous in-flight guard: set true the instant a start is requested (the
  // engine/permission setup awaits, so _isRecording isn't true yet — without
  // this a fast double-tap would start two recordings and leak the first).
  bool _starting = false;

  /// True during the example/practice record START window — after the user tapped
  /// Record but before the permission + temp-dir awaits resolve and _isRecording
  /// flips true. Cancel must treat this as dirty so the pending start isn't left
  /// to arm a hot mic in the read view after teardown.
  bool get starting => _starting;

  // One-shot user notice from an async recording event (e.g. the input device
  // was disconnected mid-record). The view peeks it, surfaces it as a toast,
  // then clears it.
  String? _recordingNotice;
  String? get recordingNotice => _recordingNotice;
  void clearRecordingNotice() => _recordingNotice = null;

  /// True while recording the ephemeral practice take.
  bool get isRecordingPractice => _isRecording && !_recordingExample;

  /// True while recording a clip to save as a persisted example.
  bool get isRecordingExample => _isRecording && _recordingExample;

  /// Live input level (0..1) while recording, for the meter (shared by both
  /// the practice and the example recorder).
  final ValueNotifier<double> recordLevel = ValueNotifier(0);

  /// Requests mic permission, ensures the engine, opens a temp WAV under a
  /// [prefix] temp dir, starts the level-meter timer, and feeds the live input
  /// into the monitor so the analysis views show the voice in real time while
  /// recording. Returns the temp path, or an error message (path null).
  Future<({String? path, String? error})> _beginRecording(String prefix) async {
    if (!await MicPermission.request()) {
      return (
        path: null,
        error:
            'Microphone access is off. Enable it in System Settings › '
            'Privacy & Security › Microphone.',
      );
    }
    final engine = _engine ??= GraphAudioPlaybackService();
    if (!engine.isAvailable) {
      return (
        path: null,
        error:
            'The audio engine is unavailable. Reconnect an audio device and '
            'try again.',
      );
    }
    // Record from the input device the user chose in Settings (this VM owns its
    // own engine, so without this it would capture from the OS default and
    // silently contradict the global selection).
    final inUid = _appSettings?.inputDeviceUid;
    if (inUid != null && inUid.isNotEmpty) engine.setInputDevice(inUid);
    final dir = await Directory.systemTemp.createTemp(prefix);
    final path = '${dir.path}/$prefix.wav';
    engine.startInputRecording(path);
    _isRecording = true;
    // Live preview: the engine captures the same input it's recording (cpal),
    // so we can drive the live spectrogram / tuner / metrics off it.
    engine.startCapture();
    _monitor.start();
    _liveMonitor = true;
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
    _levelTimer = Timer.periodic(const Duration(milliseconds: 50), (_) {
      // This timer is the only device-event consumer on the technique screen
      // (poll clears on read), so handle input loss here: a mic unplug must stop
      // the recording gracefully — finalizing KEEPS what was captured — instead
      // of letting capture die silently.
      if (engine.pollDeviceEvents() & 2 != 0) {
        _recordingNotice =
            'Recording stopped: the input device was disconnected.';
        if (_recordingExample) {
          unawaited(stopExampleRecording());
        } else {
          unawaited(stopPractice());
        }
        return;
      }
      recordLevel.value = engine.inputLevel().clamp(0.0, 1.0);
    });
    return (path: path, error: null);
  }

  /// Stops the mic hardware, live capture and meter (without persisting or
  /// analyzing).
  void _endRecordingHardware() {
    _levelTimer?.cancel();
    _levelTimer = null;
    _captureTimer?.cancel();
    _captureTimer = null;
    recordLevel.value = 0;
    final engine = _engine;
    engine?.stopInputRecording();
    if (_liveMonitor) {
      engine?.stopCapture();
      _monitor.stop();
      _liveMonitor = false;
    }
    _isRecording = false;
  }

  /// Best-effort delete of a temp recording dir at [path].
  Future<void> _deleteTempDir(String? path) async {
    if (path == null) return;
    try {
      final dir = File(path).parent;
      if (await dir.exists()) await dir.delete(recursive: true);
    } catch (_) {}
  }

  // --- Ephemeral practice recording ----------------------------------------

  String? _practiceTempPath;
  TechniqueAnalysis? _practice;

  /// The analysis of the latest practice recording (ephemeral), or null.
  TechniqueAnalysis? get practiceAnalysis => _practice;

  bool _practiceLoading = false;
  bool get practiceAnalysisLoading => _practiceLoading;

  bool _practiceFailed = false;
  bool get practiceAnalysisFailed => _practiceFailed;

  /// Whether a practice take can be recorded right now.
  bool get canRecordPractice =>
      !_isRecording && !_practiceLoading && !_savingExample && !_starting;

  /// Starts an ephemeral practice recording. Returns an error to show, or null.
  Future<String?> startPractice() async {
    if (!canRecordPractice) return null;
    _starting = true;
    try {
      final r = await _beginRecording('vocara_practice');
      if (r.error != null) return r.error;
      _practiceTempPath = r.path;
      _recordingExample = false;
      _practiceFailed = false;
      notifyListeners();
      return null;
    } finally {
      _starting = false;
    }
  }

  /// Stops the practice recording, analyzes it, and shows it on the card. The
  /// recording file is deleted afterwards (never persisted).
  Future<void> stopPractice() async {
    if (!isRecordingPractice) return;
    _endRecordingHardware();
    final path = _practiceTempPath;
    if (path == null) {
      notifyListeners();
      return;
    }
    _practiceLoading = true;
    _practiceFailed = false;
    _subject = TechniqueAnalysisSubject.practice;
    notifyListeners();

    // analyze() can THROW (native decode / isolate failure). Without try/finally
    // _practiceLoading would stay true forever — canRecordPractice is then stuck
    // false, blocking EVERY future practice recording, and the temp dir leaks.
    try {
      final result = await _analysis.analyze(path);
      _practice = result;
      _practiceFailed = result == null;
      if (result == null) _subject = TechniqueAnalysisSubject.reference;
    } catch (_) {
      _practice = null;
      _practiceFailed = true;
      _subject = TechniqueAnalysisSubject.reference;
    } finally {
      await _deleteTempDir(path);
      _practiceTempPath = null;
      _practiceLoading = false;
      _syncPositionToSubject();
      notifyListeners();
    }
  }

  /// Discards the current practice take (and its analysis).
  void clearPractice() {
    if (_practice == null && !_practiceFailed) return;
    _practice = null;
    _practiceFailed = false;
    if (_subject == TechniqueAnalysisSubject.practice) {
      _subject = TechniqueAnalysisSubject.reference;
    }
    _syncPositionToSubject();
    notifyListeners();
  }

  // --- Record a persisted example clip --------------------------------------

  String? _exampleTempPath;
  bool _savingExample = false;

  /// True while the just-recorded example clip is being imported into the draft.
  bool get savingExample => _savingExample;

  /// Whether an example clip can be recorded right now.
  bool get canRecordExample =>
      !_isRecording && !_savingExample && !_importingMedia && !_starting;

  /// Records a clip to add as an audio example (staged in the draft) — so a
  /// singer can demo the technique themselves and re-record until happy, seeing
  /// each take's analysis. The example is only persisted when the form is saved.
  /// Returns an error to surface, or null.
  Future<String?> startExampleRecording() async {
    if (!canRecordExample) return null;
    _starting = true;
    try {
      final r = await _beginRecording('vocara_example');
      if (r.error != null) return r.error;
      _exampleTempPath = r.path;
      _recordingExample = true;
      notifyListeners();
      return null;
    } finally {
      _starting = false;
    }
  }

  /// Stops the example recording and imports it into the sandbox as a STAGED
  /// example (added to the draft, not persisted until save), then deletes the
  /// temp. The just-recorded example becomes the selected reference so its
  /// analysis shows immediately.
  Future<void> stopExampleRecording() async {
    if (!isRecordingExample) return;
    _endRecordingHardware();
    _recordingExample = false;
    final path = _exampleTempPath;
    if (path == null) {
      notifyListeners();
      return;
    }
    _savingExample = true;
    notifyListeners();

    TechniqueMedia? added;
    try {
      final dest = await _mediaService.importMedia(
        techniqueId: techniqueId,
        sourcePath: path,
      );
      added = TechniqueMedia(
        id: newId(),
        path: dest,
        kind: TechniqueMedia.kindForPath(dest),
        addedAt: DateTime.now(),
      );
    } on Exception {
      // Import failed; nothing staged.
    }
    // Keep _exampleTempPath set until the file is actually gone, so a dispose
    // mid-save still cleans it up.
    await _deleteTempDir(path);
    _exampleTempPath = null;
    if (added != null) {
      _draftAdds.add(added);
      _selectedExamplePath = added.path;
      // The just-recorded example becomes the reference; drop a stale practice take that
      // was compared against the previous reference (matches selectExample).
      _resetToReferenceSubject();
      // Expand the just-recorded example so its analysis shows immediately.
      _analysisCollapsed = false;
    }
    _savingExample = false;
    notifyListeners();
  }

  // --- AnalysisSource ------------------------------------------------------

  final MonitorService _monitor = MonitorService();

  /// True while a recording is in progress, switching the analysis views to the
  /// live mic feed.
  bool _liveMonitor = false;

  @override
  final ValueNotifier<Duration> position = ValueNotifier(Duration.zero);

  /// Pushed when the user scrubs the analysis (spectrogram), so the analyzed
  /// example's inline player seeks to match. Kept separate from [position] so a
  /// playback-driven position update doesn't loop back and re-seek the player.
  final _SeekSignal _scrub = _SeekSignal();
  ValueListenable<Duration> get analysisScrub => _scrub;

  /// Which analysis dimension (Spectrogram / Power / …) is shown. Held here so it
  /// persists when the panel moves between cards or in/out of the live slot.
  int _analysisDim = 0;
  int get analysisDim => _analysisDim;
  void setAnalysisDim(int index) {
    if (_analysisDim == index) return;
    _analysisDim = index;
    notifyListeners();
  }

  @override
  bool get liveMonitor => _liveMonitor;

  @override
  MonitorService get monitor => _monitor;

  @override
  Future<void> seek(Duration to) async {
    final maxMs = _activeAnalysis?.durationMs ?? 0;
    final clamped = Duration(milliseconds: to.inMilliseconds.clamp(0, maxMs));
    position.value = clamped;
    // The inline player plays the reference clip; move it to match a scrub. A
    // practice take has no player to move.
    if (_subject == TechniqueAnalysisSubject.reference) {
      _scrub.request(clamped);
    }
  }

  /// Moves the analysis playhead to follow the example clip's audio player while
  /// it plays — only when the reference is shown (the inline player plays the
  /// reference clip), and not while recording (the live feed owns the views).
  void setPlayheadFromPlayback(Duration to) {
    if (_isRecording) return;
    if (_subject != TechniqueAnalysisSubject.reference) return;
    final maxMs = _selectedReference?.durationMs ?? 0;
    if (maxMs <= 0) return;
    position.value = Duration(milliseconds: to.inMilliseconds.clamp(0, maxMs));
  }

  /// A representative time on [a]: the loudest clearly-voiced frame — so the
  /// playhead-pinned panels (Power / Vowel / Timbre) open on a real sung moment
  /// instead of the (usually silent) clip onset. Middle of the clip if none.
  Duration _representativeTime(TechniqueAnalysis a) {
    final m = a.metrics;
    var bestFr = -1;
    var best = -1e30;
    for (var fr = 0; fr < m.frames; fr++) {
      if (m.active[fr] == 1 && m.f0[fr].isFinite && m.loudness[fr] > best) {
        best = m.loudness[fr];
        bestFr = fr;
      }
    }
    if (bestFr < 0) return Duration(milliseconds: a.durationMs ~/ 2);
    return Duration(milliseconds: (m.timeOfFrame(bestFr) * 1000).round());
  }

  /// Moves the playhead to a representative moment of the active subject.
  void _syncPositionToSubject() {
    // Reached from analysis continuations (ensureReferenceAnalysis / stopPractice /
    // _resetToReferenceSubject) that await an Isolate.run; the route can pop and
    // dispose the VM during that await, after which `position` is disposed. Writing
    // a disposed ValueNotifier throws — bail once disposed.
    if (_disposed) return;
    final a = _activeAnalysis;
    position.value = a == null ? Duration.zero : _representativeTime(a);
  }

  @override
  AudioTrack? get selectedTrack {
    final a = _activeAnalysis;
    if (a == null) return null;
    // A non-empty audioFilePath makes the factory build a clip that carries
    // durationMs (the time axis for the timbre / register / pitch / dynamics
    // panels). No view reads the path itself.
    return AudioTrack(
      id: 'technique-analysis',
      kind: AudioTrackKind.vocal,
      displayName: _subject == TechniqueAnalysisSubject.practice
          ? 'Your practice'
          : 'Reference',
      createdAt: DateTime.fromMillisecondsSinceEpoch(0),
      audioFilePath: 'technique-analysis.wav',
      durationMs: a.durationMs,
    );
  }

  @override
  VoiceMetrics? get selectedMetrics => _activeAnalysis?.metrics;

  @override
  Spectrogram? get selectedSpectrogram => _activeAnalysis?.spectrogram;

  @override
  VoiceProfile? get selectedProfile => _activeAnalysis?.profile;

  @override
  Ltas? get selectedLtas => _activeAnalysis?.ltas;

  @override
  RegisterMap? get selectedRegister => _activeAnalysis?.register;

  @override
  bool get spectrogramLoading => _subject == TechniqueAnalysisSubject.practice
      ? _practiceLoading
      : _loadingPaths.contains(referenceMedia?.path);

  /// Set in [dispose]; guards async callbacks (reference analysis, recording
  /// finalize, draft cleanup) that resume after the view model is gone from
  /// notifying a disposed notifier.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    RecordingFinalizer.instance.unregister(_quitFinalize);
    _levelTimer?.cancel();
    _captureTimer?.cancel();
    if (_isRecording) _engine?.stopInputRecording();
    // Clean up any abandoned recording (Stop never pressed before leaving).
    unawaited(_deleteTempDir(_practiceTempPath));
    unawaited(_deleteTempDir(_exampleTempPath));
    // Delete files copied for staged additions that were never committed.
    for (final m in _draftAdds) {
      unawaited(_mediaService.deleteMedia(m.path));
    }
    unawaited(_engine?.dispose());
    recordLevel.dispose();
    position.dispose();
    _scrub.dispose();
    _monitor.dispose();
    super.dispose();
  }

  /// Saves edited text fields plus the staged media draft. Returns whether it
  /// persisted.
  Future<bool> save(Technique edited) async {
    // Fold the staged media into the technique being saved.
    final media = effectiveMedia;
    // Files of examples staged for removal — delete only after the technique
    // without them persists.
    final removedPaths = <String>[
      for (final m in (_technique?.media ?? const <TechniqueMedia>[]))
        if (_draftRemoves.contains(m.id)) m.path,
    ];
    final ok = await _commit(edited.copyWith(media: media));
    if (ok) {
      for (final path in removedPaths) {
        await _mediaService.deleteMedia(path);
      }
      // The staged additions are now part of the persisted technique; clear the
      // draft so their files aren't treated as orphans.
      _draftAdds.clear();
      _draftRemoves.clear();
      _draftLabels.clear();
    }
    return ok;
  }

  /// Deletes this technique and its example files. Returns whether it persisted
  /// (the caller navigates back on success).
  Future<bool> delete() async {
    final result = await _repository.deleteTechnique(techniqueId);
    if (result is Ok<void>) {
      await _mediaService.deleteTechniqueMedia(techniqueId);
      return true;
    }
    return false;
  }

  /// Imports [sourcePaths] as example clips, copying each into the sandbox and
  /// staging it in the draft (persisted on save). Unsupported files are skipped.
  /// Reentrancy-guarded.
  Future<void> addMedia(List<String> sourcePaths) async {
    if (_importingMedia) return;
    if (_technique == null) return;

    final supported = sourcePaths
        .where(TechniqueMedia.isSupportedMedia)
        .toList();
    if (supported.isEmpty) {
      if (sourcePaths.isNotEmpty) {
        _recordingNotice = 'No supported audio or video files in the drop.';
        notifyListeners();
      }
      return;
    }

    _importingMedia = true;
    notifyListeners();

    final now = DateTime.now();
    final added = <TechniqueMedia>[];
    // Unsupported files are already excluded; count both for one summary notice.
    var skipped = sourcePaths.length - supported.length;
    for (final source in supported) {
      try {
        final dest = await _mediaService.importMedia(
          techniqueId: techniqueId,
          sourcePath: source,
        );
        added.add(
          TechniqueMedia(
            id: newId(),
            path: dest,
            kind: TechniqueMedia.kindForPath(dest),
            addedAt: now,
          ),
        );
      } on Exception {
        skipped++; // a file that failed to copy; the rest still import
      }
    }
    if (skipped > 0) {
      _recordingNotice = skipped == 1
          ? "1 file couldn't be imported."
          : "$skipped files couldn't be imported.";
    }

    _draftAdds.addAll(added);
    _importingMedia = false;
    // Select + expand the first newly added audio example so its analysis shows.
    for (final m in added) {
      if (!m.isVideo) {
        _selectedExamplePath = m.path;
        // The selected reference changed; drop a stale practice take (see selectExample).
        _resetToReferenceSubject();
        _analysisCollapsed = false;
        break;
      }
    }
    notifyListeners();
  }

  /// Removes one example clip. A staged (not-yet-saved) addition is dropped and
  /// its file deleted now; a persisted example is staged for removal (its file
  /// is deleted on save).
  Future<void> removeMedia(String mediaId) async {
    final draftIndex = _draftAdds.indexWhere((m) => m.id == mediaId);
    if (draftIndex >= 0) {
      final m = _draftAdds.removeAt(draftIndex);
      _draftLabels.remove(mediaId);
      _refCache.remove(m.path);
      _refFailed.remove(m.path);
      _loadingPaths.remove(m.path);
      _fixSelectionAfterMediaChange();
      notifyListeners();
      await _mediaService.deleteMedia(m.path);
      return;
    }
    if ((_technique?.media ?? const []).any((m) => m.id == mediaId)) {
      _draftRemoves.add(mediaId);
      _draftLabels.remove(mediaId);
      _fixSelectionAfterMediaChange();
      notifyListeners();
    }
  }

  /// Updates the caption on one example clip (staged; applied on save).
  Future<void> renameMedia(String mediaId, String label) async {
    _draftLabels[mediaId] = label.trim();
    notifyListeners();
  }

  /// Re-selects a valid example after the current one was removed.
  void _fixSelectionAfterMediaChange() {
    final audio = audioExamples;
    final before = _selectedExamplePath;
    if (audio.isEmpty) {
      _selectedExamplePath = null;
    } else if (!audio.any((m) => m.path == _selectedExamplePath)) {
      _selectedExamplePath = audio.first.path;
    }
    if (_selectedExamplePath != before) {
      // The reference example changed — drop the practice take, matching selectExample.
      _resetToReferenceSubject();
    }
  }

  /// Applies [next] optimistically, persists it, reverts on failure. Returns
  /// whether the save persisted (callers gate file deletion on this).
  Future<bool> _commit(Technique next) async {
    final previous = _technique;
    _technique = next;
    notifyListeners();

    final result = await _repository.saveTechnique(next);
    final ok = result is Ok<Technique>;
    if (ok) {
      _technique = result.value;
    } else {
      _technique = previous;
    }
    notifyListeners();
    return ok;
  }
}
