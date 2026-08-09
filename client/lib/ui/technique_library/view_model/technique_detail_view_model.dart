import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/app_settings_service.dart';
import '../../../data/services/diagnostics_log.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/media_analysis_service.dart';
import '../../../data/services/mic_recording_controller.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/technique_media_service.dart';
import '../../../data/sync/sync_engine.dart';
import '../../../data/sync/sync_hashes.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/technique.dart';
import '../../../domain/models/technique_analysis.dart';
import '../../../domain/models/technique_media.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/command.dart';
import '../../../utils/id.dart';
import '../../../utils/lru_bytes_cache.dart';
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
    Stream<RemoteApplied>? remoteApplied,
    AppSettingsService? appSettings,
    MediaAnalysisService analysisService = const MediaAnalysisService(),
    @visibleForTesting MicRecordingController? recorder,
  }) : _repository = repository,
       _mediaService = mediaService,
       _appSettings = appSettings,
       _analysis = analysisService,
       _recorderOverride = recorder {
    load = Command0(_load)..execute();
    // Freshness reloads when sync applies remote changes to THIS technique.
    _remoteSub = remoteApplied?.listen(_onRemoteApplied);
  }

  // Registered with RecordingFinalizer by the recording controller while a
  // take is in flight (saves the captured example/practice take on app quit).
  Future<void> _quitFinalize() async {
    if (isRecordingExample) {
      await stopExampleRecording();
    } else if (isRecordingPractice) {
      // A practice take is ephemeral: the dying process would never show the
      // analysis result, so skip the full DSP pass and only clean up — quit
      // must not block on a discarded compute.
      await stopPractice(skipAnalysis: true);
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

  /// Per-row content hashes of the persisted technique as THIS view model last
  /// loaded or saved it — the BASE side of the repository's three-way save
  /// (see [Loaded]). Captured by [_load], handed to every save, and replaced
  /// by the fresh map each successful save returns, so a stale in-memory
  /// technique can never revert or tombstone rows that changed underneath it
  /// (e.g. applied by sync).
  Map<String, String> _baseHashes = const {};

  // --- Sync freshness ---------------------------------------------------------

  StreamSubscription<RemoteApplied>? _remoteSub;

  /// True while [save] is writing (the commit plus its file cleanup); a
  /// remote-applied reload must not interleave with it (the save returns the
  /// fresh aggregate + base itself).
  bool _saving = false;

  /// True while [save] runs. The screen disables the WHOLE edit surface while
  /// this holds: Save (a second commit would race the first on the same
  /// [_baseHashes]), Cancel (a discard would delete staged media files the
  /// airborne commit is persisting rows for), the text fields and the media
  /// add/remove/rename/record controls (the commit snapshotted the form and
  /// the draft when it started, so edits made during the await would be
  /// silently thrown away when the save lands and edit mode closes). The
  /// media mutation methods also early-return on this flag — the UI disabling
  /// is the affordance, the VM guard is the invariant.
  bool get saving => _saving;

  /// Bumped when a save or a newer remote reload starts so an older in-flight
  /// reload's result is discarded (stale-read-completes-last race).
  int _reloadEpoch = 0;

  /// True for the lifetime of the editor's form — from [beginEdit] until the
  /// controllers are torn down. The reload must not advance past the snapshot
  /// those controllers were seeded from; see [_onRemoteApplied].
  bool _editing = false;

  /// A remote change arrived while the reload was skipped, so it has to be
  /// replayed once the edit session ends or the read view keeps showing
  /// pre-remote content with nothing pending to correct it.
  bool _missedRemote = false;

  /// Open an edit session. Pairs with [endEdit]; the screen calls this when it
  /// seeds the form's controllers, so the flag's lifetime is exactly theirs.
  void beginEdit() {
    _editing = true;
    _missedRemote = false;
  }

  /// Close the edit session (cancel, successful save, or the screen going
  /// away). Idempotent. Replays a reload the session had to skip.
  void endEdit() {
    if (!_editing) return;
    _editing = false;
    if (_missedRemote) {
      _missedRemote = false;
      unawaited(_reloadFromRemote());
    }
  }

  /// Freshness-only reload when sync applied remote rows of THIS technique.
  /// Skipped whenever an edit session holds state this reload would talk over
  /// (a media draft, an import, a recording, a save in flight) — skipping is
  /// always SAFE: the next save three-way merges against [_baseHashes], so a
  /// missed reload can never revert the synced-in change.
  ///
  /// An open EDIT SESSION skips it too, and that one is not merely safe but
  /// REQUIRED. The screen seeds its text controllers once, at [beginEdit], and
  /// never re-seeds them; a save then submits all eight fields from those
  /// controllers. Advancing [_baseHashes] to the remote row while the form
  /// still holds pre-remote text makes `desired != base` even though the user
  /// typed nothing, which is exactly the condition the repository's
  /// keep-theirs branch requires to be FALSE — so a later save (a media-only
  /// one, most plausibly) rewrites the stale text over the remote change under
  /// a fresh stamp, and wins downstream too. Re-seeding the controllers
  /// instead would fight the caret and discard typed text.
  Future<void> _onRemoteApplied(RemoteApplied event) async {
    if (!event.techniqueIds.contains(techniqueId)) return;
    await _reloadFromRemote();
  }

  /// Whether a reload would talk over state the user is holding. Records the
  /// skip in [_missedRemote] so [endEdit] can replay it.
  bool _holdsEditorState() {
    final holds =
        _saving ||
        _editing ||
        importingMedia ||
        savingExample ||
        hasMediaDraft ||
        _recorder.isRecording;
    if (holds) _missedRemote = true;
    return holds;
  }

  /// The reload body: shared by the sync event and by [endEdit]'s replay.
  Future<void> _reloadFromRemote() async {
    if (_disposed || _technique == null || load.running) return;
    if (_holdsEditorState()) return;
    final epoch = ++_reloadEpoch;
    final result = await _repository.loadTechnique(techniqueId);
    if (_disposed || epoch != _reloadEpoch) return;
    if (_holdsEditorState()) return;
    if (result is! Ok<Loaded<Technique>>) return; // e.g. deleted remotely
    _technique = result.value.aggregate;
    _baseHashes = result.value.baseHashes;
    _fixSelectionAfterMediaChange();
    notifyListeners();
  }

  /// True while example files are being copied into the sandbox.
  bool _importingMedia = false;
  bool get importingMedia => _importingMedia;

  Future<Result<Technique>> _load() async {
    // Load through the base-hash path: the hashes captured here are what every
    // later save three-way merges against (see [_baseHashes]).
    final result = await _repository.loadTechnique(techniqueId);
    if (result is Ok<Loaded<Technique>>) {
      _technique = result.value.aggregate;
      _baseHashes = result.value.baseHashes;
      notifyListeners();
      return Result.ok(result.value.aggregate);
    }
    return Result.error((result as Error<Loaded<Technique>>).error);
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

  /// Bumped whenever the media draft is reset/discarded, so an in-flight import
  /// (addMedia / stopExampleRecording) that resolves after the draft was abandoned
  /// deletes its just-copied file instead of appending an orphan clip to the
  /// cleared draft (and mutating selection behind the read view).
  int _draftGeneration = 0;

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
    // Never during an in-flight save: the commit is persisting rows for the
    // exact files this would delete (the screen disables Cancel while saving,
    // but this method is callable directly). The save's own success path
    // clears the draft; its failure path leaves the edit session — and this
    // draft — alive for the user to retry or cancel.
    if (_saving) return;
    // Invalidate any in-flight import: a clip that finishes copying after this
    // must be treated as an orphan (deleted), not appended to the now-cleared
    // draft (which would strand its file and show a phantom clip in the read view).
    _draftGeneration++;
    // Also invalidate a PENDING recording start (still awaiting permission or
    // the temp dir: isRecording is false, so the teardown below can't see it).
    // Without this, cancelling during that window would arm a hot mic in the
    // read view with no stop affordance.
    _recorder.invalidatePendingStart();
    // Releasing edit must release the mic: stop and discard any in-flight
    // example recording (otherwise the mic + capture/level timers run on until
    // the route is popped).
    if (_recorder.isRecording) {
      await _recorder.abort();
      _recordingExample = false;
      _exampleTempPath = null;
    }
    final orphans = List<TechniqueMedia>.from(_draftAdds);
    _draftAdds.clear();
    _draftRemoves.clear();
    _draftLabels.clear();
    for (final m in orphans) {
      _refCache.remove(m.path);
      _refFailed.remove(m.path);
      _refFailedMessage.remove(m.path);
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
    _practiceFailedMessage = null;
    // Invalidate any in-flight stopPractice() analysis: its take was recorded
    // against the reference we're replacing.
    _practiceGeneration++;
    _subject = TechniqueAnalysisSubject.reference;
    _syncPositionToSubject();
  }

  /// Cached analysis per example path, so switching back to an example is
  /// instant and a changed reference recomputes while an unchanged one doesn't.
  ///
  /// Byte-budgeted against the app-wide analysis budget, and disposed with
  /// this view model: each entry carries a whole spectrogram (a 10-minute
  /// reference is ~44 MB of it), so an unbounded map pinned every example the
  /// user opened for as long as the screen lived, on top of whatever the
  /// practice screen was holding.
  final LruBytesCache<TechniqueAnalysis> _refCache = LruBytesCache.shared(
    analysisCacheBudget,
    _analysisBytes,
  );

  /// Resident payload of one reference analysis: the two typed-data members
  /// dominate; the LTAS, profile and register map are bounded summaries.
  static int _analysisBytes(TechniqueAnalysis a) =>
      a.spectrogram.db.length * 4 +
      a.metrics.frames * VoiceMetrics.bytesPerFrame;

  /// Paths whose analysis was attempted but produced nothing (undecodable file).
  final Set<String> _refFailed = {};

  /// Per-path cause for a [_refFailed] entry when the exception carries a
  /// user-presentable message (today: the decode-length cap). The failure
  /// hint shows it instead of the generic "couldn't analyze" copy.
  final Map<String, String> _refFailedMessage = {};

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

  /// A user-presentable cause for the current reference's failed analysis,
  /// or null when only the generic copy applies.
  String? get referenceAnalysisFailureMessage {
    final path = referenceMedia?.path;
    return path == null ? null : _refFailedMessage[path];
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
      // The screen can be gone by now (analysis outlives a quick back-out);
      // its cache has left the shared analysis budget and nothing will read
      // these latches again. The cache drops the write on its own, so this is
      // about not doing pointless work, not about correctness.
      if (_disposed) return;
      if (result != null) {
        _refCache[path] = result;
      } else {
        _refFailed.add(path);
      }
    } catch (e) {
      _refFailed.add(path);
      // Keep the cause when it is user-presentable (the decode-length cap),
      // so the failure hint can say WHY instead of the generic copy.
      if (e is MediaTooLongException || e is MediaUnsupportedException) {
        _refFailedMessage[path] = e.toString();
      }
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

  // --- Mic recording (shared controller) -----------------------------------

  GraphAudioPlaybackService? _engine;
  bool _recordingExample = false;

  /// Test seam: when the constructor received a recorder, [_recorder] uses it
  /// instead of building the real controller (whose start path talks to the
  /// native engine and the OS permission dialog).
  final MicRecordingController? _recorderOverride;

  /// The shared mic-recording core (permission → temp WAV → engine → capture/
  /// level pumps → quit finalizer). The engine stays owned by this VM: created
  /// lazily on the first take, disposed in [dispose]; the controller borrows
  /// it per take.
  late final MicRecordingController _recorder =
      _recorderOverride ??
      MicRecordingController(
        acquireEngine: () {
          final engine = _engine ??= GraphAudioPlaybackService();
          // Record from the input device the user chose in Settings (this VM owns
          // its own engine, so without this it would capture from the OS default
          // and silently contradict the global selection). No-op when the engine
          // is unavailable.
          final inUid = _appSettings?.inputDeviceUid;
          if (inUid != null && inUid.isNotEmpty) engine.setInputDevice(inUid);
          return engine;
        },
        monitor: _monitor,
        onQuitFinalize: _quitFinalize,
        onInputDeviceLost: _onInputDeviceLost,
        onDeviceNotice: _onDeviceNotice,
        onRecordLimitReached: _onRecordLimitReached,
      );

  /// True during the example/practice record START window — after the user tapped
  /// Record but before the permission + temp-dir awaits resolve and isRecording
  /// flips true. Cancel must treat this as dirty so the pending start isn't left
  /// to arm a hot mic in the read view after teardown.
  bool get starting => _recorder.isStarting;

  // One-shot user notice from an async recording event (e.g. the input device
  // was disconnected mid-record). The view peeks it, surfaces it as a toast,
  // then clears it.
  String? _recordingNotice;
  String? get recordingNotice => _recordingNotice;
  void clearRecordingNotice() => _recordingNotice = null;

  /// True while recording the ephemeral practice take.
  bool get isRecordingPractice => _recorder.isRecording && !_recordingExample;

  /// True while recording a clip to save as a persisted example.
  bool get isRecordingExample => _recorder.isRecording && _recordingExample;

  /// Live input level (0..1) while recording, for the meter (shared by both
  /// the practice and the example recorder).
  ValueListenable<double> get recordLevel => _recorder.level;

  /// The mic vanished mid-record (unplugged, Bluetooth profile flip): stop the
  /// recording gracefully — finalizing KEEPS what was captured — instead of
  /// letting capture die silently. This VM is the only device-event consumer
  /// on the technique screen, so the controller polls while recording.
  void _onInputDeviceLost() {
    _recordingNotice = 'Recording stopped: the input device was disconnected.';
    if (_recordingExample) {
      unawaited(stopExampleRecording());
    } else {
      unawaited(stopPractice());
    }
  }

  /// The take reached the longest recording a WAV header can describe: stop it
  /// so everything captured up to the limit is saved, instead of leaving a take
  /// that looks like it is still recording while nothing more is stored.
  void _onRecordLimitReached() {
    _recordingNotice =
        'Recording stopped at the six-hour maximum length. '
        'Everything up to that point was saved.';
    if (_recordingExample) {
      unawaited(stopExampleRecording());
    } else {
      unawaited(stopPractice());
    }
  }

  /// A non-fatal input degradation (echo cancellation unavailable, or the
  /// selected mic fell back to the system default): recording continues, but
  /// the user must know — both were silently dropped before.
  void _onDeviceNotice(String message) {
    _recordingNotice = message;
    notifyListeners();
  }

  /// The message the screen toasts for a failed recording start.
  String _startFailureMessage(MicRecordingFailure failure) => switch (failure) {
    MicRecordingFailure.permissionDenied =>
      'Microphone access is off. Enable it in System Settings › '
          'Privacy & Security › Microphone.',
    MicRecordingFailure.engineUnavailable =>
      'The audio engine is unavailable. Reconnect an audio device and '
          'try again.',
    MicRecordingFailure.fileCreateFailed =>
      "Couldn't create the recording file. Check your free disk space.",
    MicRecordingFailure.inputNotDelivering =>
      'The microphone did not start. Try again, or pick a different '
          'input device in Settings.',
  };

  /// The notice for a take that finalized CLEANLY but whose recording ring
  /// overflowed mid-take (writer thread or disk stalled): the WAV is valid yet
  /// missing [droppedFrames] frames as silent holes. The clip is kept — most
  /// of it is fine — but it must never be presented as gapless. Mirrors the
  /// song recording flow's dropped-frames toast.
  static String _droppedFramesMessage(int droppedFrames) {
    // The recording ring runs at 48 kHz, so frames convert to milliseconds
    // directly (the same math as the song flow's toast).
    final droppedMs = droppedFrames * 1000 ~/ 48000;
    return 'Recording dropped about $droppedMs ms of audio while the disk '
        'was busy. The clip has small gaps.';
  }

  // --- Ephemeral practice recording ----------------------------------------

  String? _practiceTempPath;
  TechniqueAnalysis? _practice;

  /// Bumped whenever the reference changes (_resetToReferenceSubject) or the
  /// practice take is cleared, so a stopPractice() analysis that resolves late
  /// knows its take is stale — recorded against a now-replaced reference — and
  /// discards its result instead of pairing mismatched clips.
  int _practiceGeneration = 0;

  /// The analysis of the latest practice recording (ephemeral), or null.
  TechniqueAnalysis? get practiceAnalysis => _practice;

  bool _practiceLoading = false;
  bool get practiceAnalysisLoading => _practiceLoading;

  bool _practiceFailed = false;
  bool get practiceAnalysisFailed => _practiceFailed;

  /// A user-presentable cause for the failed practice analysis (today: the
  /// decode-length cap), or null when only the generic copy applies. Mirrors
  /// [referenceAnalysisFailureMessage] for the practice subject.
  String? _practiceFailedMessage;
  String? get practiceAnalysisFailureMessage => _practiceFailedMessage;

  /// Whether a practice take can be recorded right now. `!_saving` keeps the
  /// mic from arming while a save is committing (the edit surface is frozen
  /// then; see [save]).
  bool get canRecordPractice =>
      !_recorder.isRecording &&
      !_practiceLoading &&
      !_savingExample &&
      !_saving &&
      !_recorder.isStarting;

  /// Starts an ephemeral practice recording. Returns an error to show, or null.
  Future<String?> startPractice() async {
    if (!canRecordPractice) return null;
    final r = await _recorder.start(
      RecordingPathStrategy.tempDir('vocara_practice'),
    );
    final failure = r.failure;
    if (failure != null) return _startFailureMessage(failure);
    // Not started with no failure: the start was invalidated mid-await
    // (dispose or a generation bump). Nothing armed; leave state untouched.
    if (!r.started) return null;
    _practiceTempPath = r.path;
    _recordingExample = false;
    _practiceFailed = false;
    notifyListeners();
    return null;
  }

  /// Stops the practice recording, analyzes it, and shows it on the card. The
  /// recording file is deleted afterwards (never persisted).
  ///
  /// [skipAnalysis] is the quit-finalize path: stop the hardware and clean up
  /// the temp dir WITHOUT the analysis pass — the process is exiting, so its
  /// result would never be shown (an ephemeral take persists nothing either).
  Future<void> stopPractice({bool skipAnalysis = false}) async {
    if (!isRecordingPractice) return;
    final stopped = _recorder.stop();
    // False = the writer hit an IO error during the take, so the WAV on disk
    // is missing, damaged, or incomplete; handled conservatively below.
    final recordingClean = stopped?.clean ?? true;
    // > 0 = the recording ring overflowed mid-take: the WAV finalized fine but
    // is time-compressed (frames missing as silent holes) — never present it
    // as clean.
    final droppedFrames = stopped?.droppedFrames ?? 0;
    final path = _practiceTempPath;
    if (path == null) {
      notifyListeners();
      return;
    }
    if (skipAnalysis) {
      // Only the cleanup from the finally below matters at quit.
      _practiceTempPath = null;
      await deleteTempRecordingDir(path);
      return;
    }
    if (recordingClean && droppedFrames > 0) {
      // The take is kept and analyzed anyway; the notice only stops it from
      // masquerading as gapless. A dirty stop surfaces its own notice below.
      _recordingNotice = _droppedFramesMessage(droppedFrames);
    }
    _practiceLoading = true;
    _practiceFailed = false;
    _subject = TechniqueAnalysisSubject.practice;
    // Snapshot the reference generation: if it changes during analyze() (the user
    // selected/added/recorded a different reference, or cleared the take), this
    // take was recorded against a now-replaced reference. Discarding its result
    // avoids showing/comparing a take against the wrong reference.
    final generation = _practiceGeneration;
    notifyListeners();

    // analyze() can THROW (native decode / isolate failure). Without try/finally
    // _practiceLoading would stay true forever — canRecordPractice is then stuck
    // false, blocking EVERY future practice recording, and the temp dir leaks.
    try {
      TechniqueAnalysis? result;
      if (recordingClean) {
        result = await _analysis.analyze(path);
      } else if (await File(path).exists()) {
        // Dirty stop: analyze the partial WAV only if it's still on disk; a
        // missing or undecodable file falls out as a failed take below.
        result = await _analysis.analyze(path);
      }
      if (!recordingClean) {
        _recordingNotice = result == null
            ? 'Recording failed. The take could not be saved.'
            : 'Recording hit a disk error. The take may be incomplete.';
      }
      if (_practiceGeneration == generation) {
        _practice = result;
        _practiceFailed = result == null;
        _practiceFailedMessage = null;
        if (result == null) _subject = TechniqueAnalysisSubject.reference;
      }
      // else: the reference changed mid-analysis; _resetToReferenceSubject already
      // dropped this take — keep it discarded (the temp file is still deleted below).
    } catch (e) {
      if (!recordingClean) {
        _recordingNotice = 'Recording failed. The take could not be saved.';
      }
      if (_practiceGeneration == generation) {
        _practice = null;
        _practiceFailed = true;
        // Keep the cause when it is user-presentable (the decode-length
        // cap), mirroring the reference-analysis failure hint.
        if (e is MediaTooLongException || e is MediaUnsupportedException) {
          _practiceFailedMessage = e.toString();
        }
        _subject = TechniqueAnalysisSubject.reference;
      }
    } finally {
      await deleteTempRecordingDir(path);
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
    _practiceFailedMessage = null;
    // Invalidate any in-flight stopPractice() analysis so it doesn't resurrect
    // the take the user just discarded.
    _practiceGeneration++;
    if (_subject == TechniqueAnalysisSubject.practice) {
      _subject = TechniqueAnalysisSubject.reference;
    }
    _syncPositionToSubject();
    notifyListeners();
  }

  // --- Record a persisted example clip --------------------------------------

  String? _exampleTempPath;
  bool _savingExample = false;

  /// True while the just-recorded example clip is being imported into the draft
  /// OR while a recording start is pending (permission/temp-dir awaits). The
  /// screen's saveBlocked reads this, so both windows disable Save: saving
  /// during a pending start would close the edit and let the mic arm in the
  /// read view with no stop affordance.
  bool get savingExample => _savingExample || _recorder.isStarting;

  /// Whether an example clip can be recorded right now. `!_saving` is part of
  /// the in-flight-save invariant: a take started during the commit would
  /// stage its clip into a draft the save has already snapshotted, so the
  /// clip would be silently dropped when the save lands (see [save]).
  bool get canRecordExample =>
      !_recorder.isRecording &&
      !_savingExample &&
      !_importingMedia &&
      !_saving &&
      !_recorder.isStarting;

  /// Records a clip to add as an audio example (staged in the draft) — so a
  /// singer can demo the technique themselves and re-record until happy, seeing
  /// each take's analysis. The example is only persisted when the form is saved.
  /// Returns an error to surface, or null.
  Future<String?> startExampleRecording() async {
    if (!canRecordExample) return null;
    final r = await _recorder.start(
      RecordingPathStrategy.tempDir('vocara_example'),
    );
    final failure = r.failure;
    if (failure != null) return _startFailureMessage(failure);
    // Not started with no failure: the start was invalidated mid-await
    // (dispose or a generation bump). Nothing armed; leave state untouched.
    if (!r.started) return null;
    _exampleTempPath = r.path;
    _recordingExample = true;
    notifyListeners();
    return null;
  }

  /// Stops the example recording and imports it into the sandbox as a STAGED
  /// example (added to the draft, not persisted until save), then deletes the
  /// temp. The just-recorded example becomes the selected reference so its
  /// analysis shows immediately.
  Future<void> stopExampleRecording() async {
    if (!isRecordingExample) return;
    // Backstop for the in-flight-save invariant (the screen's controls are
    // disabled then): stopping here would stage a clip into the draft the
    // airborne commit already snapshotted, silently dropping it when the save
    // lands. The recording keeps running; save() itself aborts it on success.
    if (_saving) return;
    final stopped = _recorder.stop();
    // False = the writer hit an IO error during the take, so the WAV on disk
    // is missing, damaged, or incomplete; handled conservatively below.
    final recordingClean = stopped?.clean ?? true;
    // > 0 = the recording ring overflowed mid-take: the WAV finalized fine but
    // is time-compressed (frames missing as silent holes) — never present it
    // as clean.
    final droppedFrames = stopped?.droppedFrames ?? 0;
    _recordingExample = false;
    final path = _exampleTempPath;
    if (path == null) {
      notifyListeners();
      return;
    }
    if (recordingClean && droppedFrames > 0) {
      // The clip is still staged below; the notice only stops it from
      // masquerading as gapless. A dirty stop surfaces its own notice instead.
      _recordingNotice = _droppedFramesMessage(droppedFrames);
    }
    _savingExample = true;
    // Snapshot the draft generation: a Cancel/discard (or dispose) while the clip
    // is being copied means it must be deleted, not staged into the cleared draft.
    final generation = _draftGeneration;
    notifyListeners();

    // Dirty stop: stage the clip only if the partial WAV is still present and
    // decodable, so no example points at unusable audio. The partial analysis
    // seeds the reference cache after the import below.
    TechniqueAnalysis? partial;
    if (!recordingClean) {
      try {
        if (await File(path).exists()) {
          partial = await _analysis.analyze(path);
        }
      } catch (_) {
        partial = null;
      }
      if (partial == null) {
        await deleteTempRecordingDir(path);
        _exampleTempPath = null;
        _recordingNotice = 'Recording failed. The example could not be saved.';
        _savingExample = false;
        notifyListeners();
        return;
      }
      _recordingNotice =
          'Recording hit a disk error. The example may be incomplete.';
    }

    final TechniqueMedia added;
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
      // Import failed (copy error, disk full): the temp WAV is the ONLY copy
      // of the take, so it must NOT be deleted — keep it on disk for recovery
      // and surface the failure. Clearing _exampleTempPath keeps dispose()'s
      // abandoned-recording cleanup away from the kept file.
      _exampleTempPath = null;
      _recordingNotice =
          "Couldn't save the example recording. The take was kept in a "
          'temporary folder.';
      _savingExample = false;
      notifyListeners();
      return;
    }
    // Import succeeded: the clip lives in the sandbox now, the temp is a
    // duplicate. Keep _exampleTempPath set until the file is actually gone, so
    // a dispose mid-save still cleans it up.
    await deleteTempRecordingDir(path);
    _exampleTempPath = null;
    if (_draftGeneration != generation || _disposed) {
      // Draft abandoned (Cancel) or VM disposed mid-save; the copied clip is an
      // orphan — delete it rather than staging it behind the read view.
      _deleteMediaQuietly(added.path);
    } else {
      // A dirty stop already validated the partial take: reuse its analysis
      // so the reference view doesn't decode the clip a second time.
      if (partial != null) _refCache[added.path] = partial;
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

  /// True while a recording is in progress (the controller feeds the monitor
  /// then), switching the analysis views to the live mic feed.
  @override
  bool get liveMonitor => _recorder.feedingMonitor;

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
    if (_recorder.isRecording) return;
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

  /// The subject axis (see [AnalysisSource.analysisStartMs]): the synthetic
  /// [selectedTrack] wraps exactly one clip placed at 0, so the subject span
  /// is trivially the track span — the clip's own 0..durationMs.
  @override
  int get analysisStartMs => 0;

  /// The length of the subject span; see [analysisStartMs].
  @override
  int get analysisDurationMs => _activeAnalysis?.durationMs ?? 0;

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
    unawaited(_remoteSub?.cancel());
    // Tears down a recording that is running OR still starting (a pending
    // start is invalidated and arms nothing), unregisters the quit finalizer,
    // and disposes the level notifier.
    _recorder.dispose();
    // The analysis budget is app-wide, so a cache that never leaves it keeps
    // this technique's references resident for the life of the process.
    _refCache.dispose();
    // Clean up any abandoned recording (Stop never pressed before leaving).
    unawaited(deleteTempRecordingDir(_practiceTempPath));
    unawaited(deleteTempRecordingDir(_exampleTempPath));
    // Delete files copied for staged additions that were never committed —
    // but NEVER while a save is airborne: the commit is persisting rows for
    // exactly these files, and deleting them here (the route can pop during
    // the repository await) would leave committed rows pointing at nothing.
    // The save's own completion path clears the draft it owns; if it fails
    // after this dispose, the files leak as orphans — strictly better than
    // dangling rows.
    if (!_saving) {
      for (final m in _draftAdds) {
        _deleteMediaQuietly(m.path);
      }
    }
    unawaited(_engine?.dispose());
    position.dispose();
    _scrub.dispose();
    _monitor.dispose();
    super.dispose();
  }

  /// Fire-and-forget deletion of an orphaned media copy. These calls run from
  /// dispose and abandoned-draft paths where nothing can await them; an IO
  /// failure (permissions, the storage root itself being torn down) must be
  /// logged, never escape into the root zone as an unhandled async error.
  void _deleteMediaQuietly(String path) {
    unawaited(
      _mediaService.deleteMedia(path).catchError((Object e) {
        // No path in the log: DiagnosticsLog's contract forbids media paths
        // (they name user content). The error TYPE is enough to diagnose.
        DiagnosticsLog.instance.log(
          'technique',
          'draft media delete failed: ${e.runtimeType}',
        );
      }),
    );
  }

  /// Saves edited text fields plus the staged media draft. Returns whether it
  /// persisted.
  Future<bool> save(Technique edited) async {
    // Re-entrancy guard (the UI also disables Save, this is the backstop):
    // a second commit racing the first would run against the same, now-stale
    // [_baseHashes] — the three-way save would see the first commit's rows as
    // concurrent edits and could double-apply or mis-merge. Report "did not
    // persist"; the button re-enables when the in-flight save resolves.
    if (_saving) return false;
    // The REVERSE races of the in-flight-save freeze: a save entering while a
    // media import / example save / recording start is mid-await would
    // snapshot effectiveMedia BEFORE that operation lands — its result would
    // then be silently thrown away when this save clears the draft and the
    // screen leaves edit mode. The UI disables Save for all of these; this is
    // the invariant backstop for a same-frame click.
    if (_importingMedia || savingExample || _recorder.isRecording) {
      return false;
    }
    // Held across the commit AND its cleanup below, not just the repository
    // write: until the successful commit's draft is cleared, a Cancel →
    // discardMediaDraft would delete staged files whose rows just persisted.
    _saving = true;
    try {
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
        // A successful save ends the edit session (the screen leaves edit mode):
        // invalidate a recording start still pending behind its awaits, and stop
        // an example recording that armed while the commit was in flight — in the
        // read view a live example mic would have no stop affordance.
        _recorder.invalidatePendingStart();
        if (_recorder.isRecording && _recordingExample) {
          await _recorder.abort();
          _recordingExample = false;
          _exampleTempPath = null;
        }
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
    } finally {
      // Clear BEFORE the notify so the rebuild that shows the outcome also
      // re-enables Save/Cancel (and a throwing repository can't strand the UI
      // saving forever).
      _saving = false;
      notifyListeners();
    }
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
    // In-flight-save invariant (the UI also disables the add zone; this is
    // the backstop): the commit already snapshotted effectiveMedia, so a clip
    // staged now would be thrown away when the save lands and the screen
    // leaves edit mode — silent loss plus an orphaned copied file.
    if (_saving) return;

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
    // Snapshot the draft generation: if the draft is discarded (Cancel) or the VM
    // disposed while these copies are in flight, they must be deleted, not folded
    // into the cleared draft.
    final generation = _draftGeneration;
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

    // The draft was abandoned (Cancel) or the VM disposed while importing: the
    // copies are orphans — delete them and don't touch the (cleared) draft.
    if (_draftGeneration != generation || _disposed) {
      _importingMedia = false;
      for (final m in added) {
        _deleteMediaQuietly(m.path);
      }
      return;
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
    // In-flight-save invariant (backstop behind the disabled UI): a removal
    // staged now is invisible to the airborne commit and gets cleared with
    // the draft when the save lands — the user's removal silently un-happens.
    // Worse, removing a staged ADDITION would delete a file the commit is
    // persisting a row for.
    if (_saving) return;
    final draftIndex = _draftAdds.indexWhere((m) => m.id == mediaId);
    if (draftIndex >= 0) {
      final m = _draftAdds.removeAt(draftIndex);
      _draftLabels.remove(mediaId);
      _refCache.remove(m.path);
      _refFailed.remove(m.path);
      _refFailedMessage.remove(m.path);
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
    // In-flight-save invariant (backstop behind the disabled UI): the commit
    // already snapshotted the labels, so a caption staged now vanishes when
    // the successful save clears the draft.
    if (_saving) return;
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
    // A starting save supersedes any in-flight remote-applied reload (its
    // read predates this write) and blocks new ones until the fresh base is
    // captured below. [save] already holds _saving here, so this optimistic
    // notify also disables the header's Save/Cancel.
    _reloadEpoch++;
    final previous = _technique;
    _technique = next;
    notifyListeners();
    // Three-way save against the base captured at load / the previous save,
    // so a stale technique can't revert or tombstone concurrent (synced-in)
    // changes; the returned fresh base replaces ours for the next save.
    final result = await _repository.saveTechniqueWithBase(
      next,
      baseHashes: _baseHashes,
    );
    final ok = result is Ok<Loaded<Technique>>;
    if (ok) {
      _technique = result.value.aggregate;
      _baseHashes = result.value.baseHashes;
    } else {
      // Revert to the previous aggregate; _baseHashes stays as-is — it still
      // describes the last state this session loaded or saved, which is what
      // the reverted model matches.
      _technique = previous;
    }
    notifyListeners();
    return ok;
  }
}
