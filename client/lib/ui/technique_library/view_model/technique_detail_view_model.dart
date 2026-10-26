import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/app_settings_service.dart';
import '../../../data/services/diagnostics_log.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/media_analysis_service.dart';
import '../../../data/services/mic_recording_controller.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/recording_finalizer.dart';
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

/// A bare "it happened" signal for the example players (see [micArmed]).
class _Signal extends ChangeNotifier {
  void fire() => notifyListeners();
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
    RecordingFinalizer? finalizer,
  }) : _repository = repository,
       _mediaService = mediaService,
       _appSettings = appSettings,
       _analysis = analysisService,
       _recorderOverride = recorder,
       _finalizer = finalizer ?? RecordingFinalizer.instance {
    load = Command0(_load)..execute();
    // Freshness reloads when sync applies remote changes to THIS technique.
    _remoteSub = remoteApplied?.listen(_onRemoteApplied);
  }

  /// Registered with [RecordingFinalizer] by the recording controller while a
  /// take is in flight (saves the captured example/practice take on app quit).
  /// Visible for testing only: an injected test recorder bypasses the
  /// finalizer registry, so tests drive the quit path by calling this.
  @visibleForTesting
  Future<void> quitFinalize() async {
    if (isRecordingExample) {
      // The same backstop as [stopExampleRecording]: an airborne save owns
      // the take.
      if (_saving) return;
      // Stopping only STAGES the clip into the in-memory media draft, which
      // dies with the exiting process — and the imported file, referenced by
      // no row, would be swept as an orphan at the next boot. Commit the
      // captured clip itself so the recording survives the quit (mirrors the
      // song screen's finalizer, which persists its take). Held for the quit
      // from the stop on, like [save]'s: the stop unregisters this finalizer,
      // so a close-button run cut off mid-stage or mid-commit left the exit
      // request's run nothing to wait on, and the clip was lost.
      final stopped = _recorder.stop();
      await _recorder.guardStopFinalize(() async {
        final staged = await _stageStoppedExample(stopped);
        if (staged != null) await _persistExampleAtQuit(staged);
      });
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

  /// The quit registry an armed take and an in-flight [save] hold the app's
  /// exit on (the app-wide instance unless a test injects its own).
  final RecordingFinalizer _finalizer;

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

  /// Whether an edit session may open now. Not while a practice take is
  /// recording or its start is still pending: the edit view has no practice
  /// recorder (so no Stop for the take), [save] refuses while the mic is hot
  /// (every Save would fail), and Cancel's [discardMediaDraft] would abort the
  /// take with no prompt. The user stops the take first. The screen disables
  /// Edit on this and also checks it at tap time, since a tap can land
  /// between a start going pending and the rebuild that announces it.
  bool get canEdit => !isRecordingPractice && !_recorder.isStarting;

  /// Open an edit session. Pairs with [endEdit]; the screen calls this when it
  /// seeds the form's controllers, so the flag's lifetime is exactly theirs.
  void beginEdit() {
    _editing = true;
    // A change missed before this session stays owed (replayed once it and any
    // hold end): cleared here, one missed while an abandoned import still ran
    // was dropped by an Edit reopened before the copy ended.
    // The editor's example card has no Reference/Practice switch and only the
    // reference drives (and is driven by) the inline player, so a practice
    // subject left over from the read view would show the take's analysis
    // under the example with no way back. The take itself is kept for the
    // read view's switch; a take still being analyzed lands without touching
    // the subject (see [stopPractice]).
    showAnalysisSubject(TechniqueAnalysisSubject.reference);
  }

  /// Close the edit session (cancel, successful save, or the screen going
  /// away). Idempotent. Replays a reload the session had to skip.
  void endEdit() {
    if (!_editing) return;
    _editing = false;
    _replayMissedRemote();
  }

  /// Replays a reload skipped while a hold was on (see [_holdsEditorState]),
  /// once nothing pins the read view any more. Outside an edit session the
  /// hold is a practice take: stopping it used to replay nothing, so a remote
  /// change landing mid-take left the read view stale with nothing pending
  /// to correct it — and a later Edit seeded the form from that stale text.
  /// An import or an example save that outlived its edit session ended (or
  /// failed): tell the screen, and replay a remote change skipped while it
  /// ran. endEdit's replay found it still busy and skipped again, and nothing
  /// else replayed it: the read view kept the old text, and the next edit,
  /// seeded from it against the stale base, wrote it over the remote change.
  void _busyEnded() {
    notifyListeners();
    _replayMissedRemote();
  }

  void _replayMissedRemote() {
    if (_editing || !_missedRemote) return;
    _missedRemote = false;
    unawaited(_reloadFromRemote());
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
    final referenceBefore = referenceMedia?.id;
    _technique = result.value.aggregate;
    _baseHashes = result.value.baseHashes;
    _fixSelectionAfterMediaChange(referenceBefore);
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
    // The failure must reach the screen too: it watches only this view model,
    // and the command notifies just its own listeners, so without this the
    // spinner (keyed on load.running) is never rebuilt away. The rebuild runs
    // on the next frame, after the command has cleared `running`.
    notifyListeners();
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
  ///
  /// A staged addition is listed from the draft for as long as the draft holds
  /// it, even when [_technique] already carries the same clip: [_commit]'s
  /// optimistic aggregate includes every staged addition (and so does the
  /// just-committed one, until [save] clears the draft), and listing both
  /// showed each staged clip twice — with a second audio player — for the
  /// whole commit. Keeping the DRAFT copy (not the aggregate's) leaves the
  /// list exactly as it was before the save: same order, and the sandbox path
  /// the selection still points at until [save] remaps it. A failed save
  /// reverts the aggregate, so nothing else is needed to restore the list.
  List<TechniqueMedia> get effectiveMedia {
    final base = _technique?.media ?? const <TechniqueMedia>[];
    final stagedIds = {for (final m in _draftAdds) m.id};
    final out = <TechniqueMedia>[];
    for (final m in base) {
      if (_draftRemoves.contains(m.id) || stagedIds.contains(m.id)) continue;
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
    final referenceBefore = referenceMedia?.id;
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
    _fixSelectionAfterMediaChange(referenceBefore);
    notifyListeners();
  }

  // --- Reference analysis (ground truth) -----------------------------------

  /// All analyzable (audio) examples, in display order. Video clips aren't
  /// analyzed.
  List<TechniqueMedia> get audioExamples => [
    for (final m in effectiveMedia)
      if (!m.isVideo) m,
  ];

  /// Id of the example whose analysis is shown. Null falls back to the first
  /// audio example.
  ///
  /// An id, not a path: two examples with identical bytes resolve to the SAME
  /// pooled file once saved, and a path then named both of them — both cards
  /// opened with the analysis and a playhead-linked player, and the second
  /// could never be selected on its own. The analysis caches stay keyed by
  /// path: an analysis is a property of the file's bytes, shared correctly.
  String? _selectedExampleId;

  /// The example clip used as the reference "ground truth": the selected audio
  /// example, or the first one. Null when the technique has no audio example.
  TechniqueMedia? get referenceMedia {
    final audio = audioExamples;
    if (audio.isEmpty) return null;
    for (final m in audio) {
      if (m.id == _selectedExampleId) return m;
    }
    return audio.first;
  }

  /// Whether the selected example's inline analysis is collapsed. Only the
  /// selected example can be expanded (accordion), so this one flag suffices.
  bool _analysisCollapsed = false;

  /// The id of the example whose inline analysis is currently expanded, or
  /// null when collapsed.
  String? get expandedExampleId =>
      _analysisCollapsed ? null : referenceMedia?.id;

  /// Toggles a card's inline analysis (by media id): collapse/expand the
  /// selected one, or select-and-expand a different one. While a practice
  /// take records — or its start is still pending, when it arms into this
  /// same card — its Stop lives in the selected card: collapsing that card
  /// hid it (until re-expanded), so the card stays open.
  void toggleExampleExpanded(String mediaId) {
    if (referenceMedia?.id == mediaId) {
      if ((isRecordingPractice || _recorder.isStarting) &&
          !_analysisCollapsed) {
        return;
      }
      _analysisCollapsed = !_analysisCollapsed;
      notifyListeners();
    } else {
      _analysisCollapsed = false;
      selectExample(mediaId);
    }
  }

  /// Switches which example's analysis is shown (and which drives the playhead).
  /// Drops any practice take, since it was compared against the old reference.
  /// Refused while one records: it is compared against the example it armed
  /// under, so switching mid-take only threw it away at Stop.
  void selectExample(String mediaId) {
    if (referenceMedia?.id == mediaId) return;
    if (isRecordingPractice) {
      _recordingNotice = 'Stop the practice take to switch examples.';
      notifyListeners();
      return;
    }
    _selectedExampleId = mediaId;
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

  /// In-flight analyses whose clip a commit moved to a new (pool) path: the
  /// old path -> the path the result lands under (see
  /// [_remapMediaAfterCommit]).
  final Map<String, String> _inFlightMoves = {};

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

    // Where the result lands: [path], unless a commit moved the clip to its
    // pool path while this was in flight (see [_remapMediaAfterCommit]).
    var landed = path;
    var skipped = false;
    // analyze() can THROW (native decode / isolate-spawn failure), not just
    // return null. Without try/finally the path would stay in _loadingPaths
    // forever — the spinner sticks AND the contains-guard above blocks every
    // retry. Always clear the loading flag and latch failure.
    try {
      final result = await _analysis.analyze(
        path,
        // Asked when the app-wide analysis gate admits the call: one queued
        // behind other computes while the user clicked on to another example
        // (or left the screen) is dropped before its decode, instead of
        // delaying the analyses still wanted behind a full pass for nothing.
        wanted: () {
          skipped =
              _disposed ||
              referenceMedia?.path != (_inFlightMoves[path] ?? path);
          return !skipped;
        },
      );
      // The screen can be gone by now (analysis outlives a quick back-out);
      // its cache has left the shared analysis budget and nothing will read
      // these latches again. The cache drops the write on its own, so this is
      // about not doing pointless work, not about correctness.
      if (_disposed) return;
      landed = _inFlightMoves[path] ?? path;
      if (skipped) {
        // Nothing to latch: selecting the example again analyzes it.
      } else if (result != null) {
        _refCache[landed] = result;
      } else {
        _refFailed.add(landed);
      }
    } catch (e) {
      landed = _inFlightMoves[path] ?? path;
      _refFailed.add(landed);
      // Keep the cause when it is user-presentable (the decode-length cap),
      // so the failure hint can say WHY instead of the generic copy.
      if (e is MediaTooLongException || e is MediaUnsupportedException) {
        _refFailedMessage[landed] = e.toString();
      }
    } finally {
      _loadingPaths.remove(path);
      final moved = _inFlightMoves.remove(path);
      if (moved != null) _loadingPaths.remove(moved);
    }
    // Place the playhead if this is still the shown reference.
    if (referenceMedia?.path == landed &&
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
          // Record from the input the user chose in Settings, device and channel
          // (this VM owns its own engine, so without this it would capture from
          // the OS default and silently contradict the global selection). Read
          // per take, so a choice changed between takes applies to the next.
          // No-op when the engine is unavailable.
          _appSettings?.applyInputTo(engine);
          return engine;
        },
        monitor: _monitor,
        onQuitFinalize: quitFinalize,
        onInputDeviceLost: _onInputDeviceLost,
        onDeviceNotice: _onDeviceNotice,
        onRecordLimitReached: _onRecordLimitReached,
        finalizer: _finalizer,
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

  /// Runs the recorder's start, announcing both edges of its pending window.
  /// [MicRecordingController.isStarting] flips synchronously at the call and
  /// can then pend behind the permission prompt and the mic open, but the
  /// screen only sees what this view model notifies: without these, Save and
  /// the example cards looked live for the whole window (a Save tap toasted
  /// "Couldn't save", a collapsed card armed the take with its Stop hidden),
  /// and a failed start left them looking busy until something else rebuilt.
  Future<MicRecordingStart> _startRecorder(
    RecordingPathStrategy destination,
  ) async {
    final pending = _recorder.start(destination);
    notifyListeners();
    final r = await pending;
    if (!r.started) notifyListeners();
    return r;
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
  /// practice take is cleared, so a take whose reference changed while it was
  /// recording or being analyzed knows it is stale — recorded against a
  /// now-replaced reference — and is discarded instead of pairing mismatched
  /// clips.
  int _practiceGeneration = 0;

  /// The [_practiceGeneration] the take now recording was ARMED under: the
  /// reference a take is recorded against is the one selected when capture
  /// starts, so the snapshot is taken then, not at Stop (see [stopPractice]).
  int _practiceTakeGeneration = 0;

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
    final r = await _startRecorder(
      RecordingPathStrategy.tempDir('vocara_practice'),
    );
    final failure = r.failure;
    if (failure != null) return _startFailureMessage(failure);
    // Not started with no failure: the start was invalidated mid-await
    // (dispose or a generation bump). Nothing armed; leave state untouched.
    if (!r.started) return null;
    _practiceTempPath = r.path;
    _practiceTakeGeneration = _practiceGeneration;
    _recordingExample = false;
    _practiceFailed = false;
    _micArmed.fire();
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
    // The take was the hold keeping remote reloads off the read view; a quit
    // has no view left to refresh.
    if (!skipAnalysis) _replayMissedRemote();
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
    // The take was recorded against the reference selected when it ARMED. A
    // reference change since then (the example cards stay live while
    // recording, so opening another one mid-take is one tap) already dropped
    // the previous take and pointed the views at the new reference; analyzing
    // this one would pair it in the gap table with a reference it was never
    // recorded against. A snapshot taken HERE accepted exactly that, while
    // the same tap landing just after Stop discarded the take — the verdict
    // hung on which side of Stop the tap fell. Discard it, and say why: the
    // user just pressed Stop and expects a result.
    final generation = _practiceTakeGeneration;
    if (_practiceGeneration != generation) {
      _practiceTempPath = null;
      await deleteTempRecordingDir(path);
      _recordingNotice =
          'Practice take discarded: the reference example changed while '
          'recording.';
      notifyListeners();
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
    // The same generation guards the analysis: if it changes during analyze()
    // (the user selected/added/recorded a different reference, or cleared the
    // take), the result is discarded rather than shown against the wrong
    // reference.
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
    // the take the user just discarded. Not while a NEW take is recording: no
    // analysis can be in flight then (a take only arms with none loading),
    // and the bump would void that take at its Stop although its reference
    // never changed (see [stopPractice]).
    if (!isRecordingPractice) _practiceGeneration++;
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
    final r = await _startRecorder(
      RecordingPathStrategy.tempDir('vocara_example'),
    );
    final failure = r.failure;
    if (failure != null) return _startFailureMessage(failure);
    // Not started with no failure: the start was invalidated mid-await
    // (dispose or a generation bump). Nothing armed; leave state untouched.
    if (!r.started) return null;
    _exampleTempPath = r.path;
    _recordingExample = true;
    _micArmed.fire();
    notifyListeners();
    return null;
  }

  /// Stops the example recording and imports it into the sandbox as a STAGED
  /// example (added to the draft, not persisted until save), then deletes the
  /// temp. The just-recorded example becomes the selected reference so its
  /// analysis shows immediately. Returns the staged clip, or null when
  /// nothing was staged (not recording, the take failed, or the draft was
  /// abandoned mid-import).
  ///
  /// Not quit-guarded past the stop, deliberately: the clip only lands in the
  /// in-memory draft, which a quit discards like any unsaved edit, so holding
  /// the exit for the import would persist nothing. A Save while the take
  /// records runs this same tail and its commit under the quit gate instead
  /// (see [save]).
  Future<TechniqueMedia?> stopExampleRecording() async {
    if (!isRecordingExample) return null;
    // Backstop for the in-flight-save invariant (the screen's controls are
    // disabled then): stopping here would stage a clip into the draft the
    // airborne commit already snapshotted, silently dropping it when the save
    // lands. The recording keeps running; save() itself aborts it on success.
    if (_saving) return null;
    return _stageStoppedExample(_recorder.stop());
  }

  /// The tail of [stopExampleRecording] after the hardware [stopped]: import
  /// the take into the draft (see there). Split out so [save] can run it,
  /// together with its commit, inside one quit-gate bracket.
  Future<TechniqueMedia?> _stageStoppedExample(
    MicRecordingStop? stopped,
  ) async {
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
      return null;
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
        _busyEnded();
        return null;
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
        // Captioned now: the file is named by a generated id here and by its
        // content hash once pooled, so the uncaptioned fallback (the file's
        // basename) would show the user a hash.
        label: _nextRecordingLabel(),
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
      _busyEnded();
      return null;
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
      _savingExample = false;
      _busyEnded();
      return null;
    }
    // A dirty stop already validated the partial take: reuse its analysis
    // so the reference view doesn't decode the clip a second time.
    if (partial != null) _refCache[added.path] = partial;
    _draftAdds.add(added);
    _selectedExampleId = added.id;
    // The just-recorded example becomes the reference; drop a stale practice take that
    // was compared against the previous reference (matches selectExample).
    _resetToReferenceSubject();
    // Expand the just-recorded example so its analysis shows immediately.
    _analysisCollapsed = false;
    _savingExample = false;
    notifyListeners();
    return added;
  }

  /// The caption a just-recorded example is staged under: "Recording N", one
  /// past the highest such caption the technique already shows, so takes
  /// recorded across edit sessions keep counting up.
  String _nextRecordingLabel() {
    var highest = 0;
    for (final m in effectiveMedia) {
      final n = int.tryParse(
        _recordingLabel.firstMatch(m.label.trim())?.group(1) ?? '',
      );
      if (n != null && n > highest) highest = n;
    }
    return 'Recording ${highest + 1}';
  }

  /// "Recording N" with a bounded N, so an absurd caption typed by hand can't
  /// overflow the numbering (the same guard "Take N" numbering carries).
  static final RegExp _recordingLabel = RegExp(r'^Recording (\d{1,9})$');

  /// Quit-time commit of the example [staged] by [stopExampleRecording]: the
  /// persisted technique plus the captured clip, and nothing else — the rest
  /// of the unsaved draft (removals, captions, earlier additions) stays
  /// uncommitted, exactly as it does when the app quits with no recording in
  /// flight. Only the take the finalizer just stopped must not die with the
  /// process.
  Future<void> _persistExampleAtQuit(TechniqueMedia staged) async {
    final current = _technique;
    if (current == null) return;
    // The commit resolves the clip's sandbox path to a pool path; remap the
    // analysis caches like [save] does, so the state stays coherent if the
    // exit is ever abandoned after the finalizer ran.
    final oldPathById = {
      for (final m in current.media) m.id: m.path,
      staged.id: staged.path,
    };
    final referenceBefore = referenceMedia?.id;
    final ok = await _commit(
      current.copyWith(media: [...current.media, staged]),
    );
    if (!ok) {
      // The clip stays staged; the finalizer has no UI to retry from, so the
      // loss risk is at least recorded.
      DiagnosticsLog.instance.log(
        'technique',
        'quit-time example commit failed',
      );
      return;
    }
    _draftAdds.removeWhere((m) => m.id == staged.id);
    _remapMediaAfterCommit(oldPathById, referenceBefore);
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

  /// Fired when a take arms (practice or example), so every example player
  /// pauses. They play through media_kit, not the engine, so the engine's
  /// echo canceller has no reference to subtract: a clip left playing on
  /// speakers was recorded into the very take it is compared against.
  final _Signal _micArmed = _Signal();
  Listenable get micArmed => _micArmed;

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

  /// The halves come from one compute here.
  @override
  bool get metricsLoading => spectrogramLoading;

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
    _micArmed.dispose();
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
  /// persisted. An example take still recording is finalized BY the save —
  /// stopped, staged and committed with the rest — so Save keeps the take and
  /// releases the mic.
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
    // the invariant backstop for a same-frame click. A recording EXAMPLE take
    // is the exception: this save stops and stages it itself, before taking
    // that snapshot. (A practice take never reaches an edit session; see
    // [canEdit].)
    final finalizeTake = isRecordingExample;
    if (_importingMedia ||
        savingExample ||
        (_recorder.isRecording && !finalizeTake)) {
      return false;
    }
    // Held across the take's import (when there is one), the commit AND its
    // cleanup below, not just the repository write: until the successful
    // commit's draft is cleared, a Cancel → discardMediaDraft would delete
    // staged files whose rows just persisted. Holding it from the import on
    // also keeps Cancel and the form frozen while the take is staged — the
    // screen built [edited] from the form before calling, and a Cancel →
    // Discard landing mid-import used to tear the form down under a save
    // that then committed anyway.
    _saving = true;
    try {
      // Held against quit like the take path below: the user pressed Save,
      // and a Cmd+Q landing during the pool import or the commit must wait
      // for it rather than exit with the edit (and its staged files, swept
      // as orphans at the next boot) silently lost.
      if (!finalizeTake) {
        return await _finalizer.hold(() => _commitEdit(edited));
      }
      // stop() unregisters the armed-window quit finalizer, yet the take is
      // safe only once the commit below lands: until then it is a temp WAV no
      // boot recovery scans plus an in-memory draft entry. Hold the quit gate
      // over the import AND the commit (as the song flow does over its
      // finalize tail), entered synchronously after the stop so no event-loop
      // turn separates the two registrations.
      final stopped = _recorder.stop();
      var ok = false;
      await _recorder.guardStopFinalize(() async {
        await _stageStoppedExample(stopped);
        ok = await _commitEdit(edited);
      });
      return ok;
    } finally {
      // Clear BEFORE the notify so the rebuild that shows the outcome also
      // re-enables Save/Cancel (and a throwing repository can't strand the UI
      // saving forever).
      _saving = false;
      notifyListeners();
    }
  }

  /// The commit half of [save] (which holds [_saving] around it): fold the
  /// draft into [edited], persist it, and on success clean up after the
  /// draft.
  Future<bool> _commitEdit(Technique edited) async {
    // Fold the staged media into the technique being saved.
    final media = effectiveMedia;
    // A successful commit re-reads the aggregate with every media file
    // resolved through the asset pool, so a clip staged from the sandbox
    // comes back under a DIFFERENT path. Remember each clip's pre-commit
    // path by id (ids are stable across the commit) for the remap below.
    final oldPathById = {for (final m in media) m.id: m.path};
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
      final referenceBefore = referenceMedia?.id;
      _draftAdds.clear();
      _draftRemoves.clear();
      _draftLabels.clear();
      // The analysis caches key on paths the commit just rewrote to pool
      // paths; carry them onto the persisted aggregate so the shown example's
      // analysis isn't recomputed (or its failure verdict forgotten).
      _remapMediaAfterCommit(oldPathById, referenceBefore);
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
            // The user's own file name: the sandbox copy is named by a
            // generated id and the pooled file by its content hash, so the
            // uncaptioned fallback (the file's basename) showed a hash.
            label: p.basenameWithoutExtension(source),
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
      // The screen last saw importingMedia true (its spinner, and Save/add/
      // Record disabled) and the user may already be in a NEW edit session:
      // tell it the import is over, or that busy state sticks there.
      _busyEnded();
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
        _selectedExampleId = m.id;
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
    final referenceBefore = referenceMedia?.id;
    final draftIndex = _draftAdds.indexWhere((m) => m.id == mediaId);
    if (draftIndex >= 0) {
      final m = _draftAdds.removeAt(draftIndex);
      _draftLabels.remove(mediaId);
      _refCache.remove(m.path);
      _refFailed.remove(m.path);
      _refFailedMessage.remove(m.path);
      _loadingPaths.remove(m.path);
      _fixSelectionAfterMediaChange(referenceBefore);
      notifyListeners();
      await _mediaService.deleteMedia(m.path);
      return;
    }
    if ((_technique?.media ?? const []).any((m) => m.id == mediaId)) {
      _draftRemoves.add(mediaId);
      _draftLabels.remove(mediaId);
      _fixSelectionAfterMediaChange(referenceBefore);
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

  /// Carries the path-keyed state (reference-analysis cache and failure
  /// latches) across a successful commit. The commit re-reads the aggregate
  /// with every media file resolved through the asset pool, so a clip staged
  /// from the sandbox comes back under a different path while its id stays
  /// stable; without the remap the just-computed analysis is orphaned under
  /// the old path. (The selection is keyed by id, so it needs no carrying.)
  /// [oldPathById] maps each committed clip's id to its pre-commit path;
  /// [referenceBefore] is the reference shown before the commit (see
  /// [_fixSelectionAfterMediaChange]).
  void _remapMediaAfterCommit(
    Map<String, String> oldPathById,
    String? referenceBefore,
  ) {
    for (final m in _technique?.media ?? const <TechniqueMedia>[]) {
      final old = oldPathById[m.id];
      if (old == null || old == m.path) continue;
      // An analysis still in flight under the old path lands under the new
      // one (see [ensureReferenceAnalysis]), and the new path reads as
      // loading meanwhile: keyed on the old path alone, the saved example
      // looked unanalyzed, the view started a second full pass over the same
      // bytes, and the first parked a dead entry in the shared cache.
      if (_loadingPaths.contains(old) && !_loadingPaths.contains(m.path)) {
        _loadingPaths.add(m.path);
        _inFlightMoves[old] = m.path;
      }
      final cached = _refCache.remove(old);
      if (cached != null) _refCache[m.path] = cached;
      // The pool copy is byte-identical to the staged file, so a latched
      // failure verdict transfers — recomputing it is doomed.
      if (_refFailed.remove(old)) _refFailed.add(m.path);
      final message = _refFailedMessage.remove(old);
      if (message != null) _refFailedMessage[m.path] = message;
    }
    // Re-normalize like every other media-mutating path (e.g. the selected
    // example was staged for removal by this very save).
    _fixSelectionAfterMediaChange(referenceBefore);
  }

  /// Re-selects a valid example after the current one was removed.
  /// [referenceBefore] is [referenceMedia]'s id as it was before the media
  /// changed.
  ///
  /// The practice take is dropped only when the reference ITSELF moved. This
  /// used to compare the raw selection instead, and an example nobody had
  /// tapped — the first one, the reference by fallback, which the page opens
  /// on — went from null to its own id here: read as a reference change, it
  /// dropped the take on Edit then Cancel, on a text-only Save and on every
  /// remote change to the technique, and a remote change replayed at Stop
  /// discarded the take being analyzed without a word.
  void _fixSelectionAfterMediaChange(String? referenceBefore) {
    final audio = audioExamples;
    if (audio.isEmpty) {
      _selectedExampleId = null;
    } else if (!audio.any((m) => m.id == _selectedExampleId)) {
      _selectedExampleId = audio.first.id;
    }
    if (referenceMedia?.id != referenceBefore) {
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
      // Committed, but its re-read failed: what was sent stands in for the
      // store, which may hold rows a concurrent writer kept, and pool paths
      // where this has its clips' sandbox ones. A reload is owed, as for a
      // remote change skipped meanwhile — replayed once nothing holds the
      // editor (a save ends its edit session).
      if (!result.value.reread) _missedRemote = true;
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
