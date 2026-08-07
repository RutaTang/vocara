import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:file_selector/file_selector.dart' show getSaveLocation;
import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import '../../../data/repositories/song_practice_repository.dart';
import '../../../data/repositories/technique_repository.dart';
import '../../../data/services/app_settings_service.dart';
import '../../../data/services/audio_edit_service.dart';
import '../../../data/services/audio_file_service.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/playback_engine.dart';
import '../../../data/services/playback_engine_factory.dart';
import '../../../data/services/recording_finalizer.dart';
import '../../../data/services/source_separation_service.dart';
import '../../../data/services/spectrogram_service.dart';
import '../../../data/services/waveform_service.dart';
import '../../../data/sync/sync_engine.dart';
import '../../../data/sync/sync_hashes.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/lyric_line.dart';
import '../../../domain/models/practice_note.dart';
import '../../../domain/models/song_practice.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/technique.dart';
import '../../../domain/models/technique_marker.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/busy_gate.dart';
import '../../../utils/command.dart';
import '../../../utils/id.dart';
import '../../../utils/lyrics_format.dart';
import '../../../utils/result.dart';
import 'analysis_cache.dart';
import 'analysis_source.dart';
import 'interpretations_controller.dart';
import 'mixer_controller.dart';
import 'playback_controller.dart';
import 'recording_controller.dart';

// The dock/lane enums live with the analysis cache; re-export them so the
// widgets that name them (bottom dock, lanes, left panel) keep their single
// view-model import.
export 'analysis_cache.dart' show AnalysisLane, DockTab;

/// The two tabs of the right-side panel.
enum RightTab { lyrics, notes, techniques }

/// The lyrics panel's interaction mode.
enum LyricsMode { read, edit, sync }

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

/// ViewModel for the multi-track detail view: the composition root of the
/// screen's controllers and the single owner of the practice model, selection,
/// persistence and the audio-engine/monitor lifecycle.
///
/// The feature areas live in five controllers this VM constructs and wires
/// with closures onto itself — [playback] (transport/loop/zoom/playhead),
/// [recording] (take capture/input monitoring/device watch), [mixer]
/// (everything the engine hears), [versions] (interpretations) and [analysis]
/// (compute caches + dock + live view). Their notifications are chained into
/// this VM's, so every `ListenableBuilder(listenable: vm)` keeps rebuilding
/// unchanged, and thin delegates preserve the VM's public surface while the
/// widgets migrate. What remains here besides composition: audio imports,
/// source separation, track/clip STRUCTURE edits, notes, technique markers,
/// lyrics, toasts, and the shared [BusyGate].
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
    Stream<RemoteApplied>? remoteApplied,
    PlaybackEngine? playbackEngine,
    MonitorService? monitor,
    AudioEditService? editService,
    RecordingFinalizer? finalizer,
    Future<String?> Function(String suggestedName)? saveLocationPicker,
    @visibleForTesting Duration? Function(String path)? probeDuration,
  }) : _repository = repository,
       _techniqueRepository = techniqueRepository,
       _audioFiles = audioFileService,
       _waveforms = waveformService,
       _spectrograms = spectrogramService,
       _separation = separationService,
       _appSettings = appSettings,
       _practiceId = practiceId,
       _playback = playbackEngine ?? createPlaybackEngine(),
       _pickSaveLocation = saveLocationPicker ?? _systemSaveLocation,
       _monitor = monitor ?? MonitorService(),
       _edits = editService ?? AudioEditService(files: audioFileService),
       // Injectable because the real probe is a direct FFI call into the
       // engine dylib, which plain unit tests cannot load.
       _probeDuration = probeDuration ?? probeMediaDuration,
       _finalizer = finalizer ?? RecordingFinalizer.instance {
    // Adopt the persisted output-device choice and let the global settings panel drive this
    // engine live. The native engine is the only one with device support.
    if (_playback is GraphAudioPlaybackService) {
      _appSettings.attachEngine(_playback);
    }

    // --- Controller wiring. Each closure names the invariant it serves. ---
    _busyGate = BusyGate(onError: (e) => _toast(_friendlyError(e)));
    playback = PlaybackController(
      engine: _playback,
      practice: () => _practice,
      // The engine's position stream must keep yielding to the record clock
      // while a take is in flight (the recording tick writes the playhead).
      isRecording: () => recording.isRecording,
    );
    mixer = MixerController(
      engine: _playback,
      practice: () => _practice,
      setPractice: (next) => _practice = next,
      trackById: _trackById,
      allTracks: () => _allTracks,
      updateTrack: _updateTrack,
      save: _save,
      toast: _toast,
      selectBusAfterMutation: _selectBusAfterMutation,
      clearBusSelectionIf: _clearBusSelectionIf,
      // The take being punched into must be silent in the engine while its
      // live envelope folds into the master display waveform.
      isRecording: () => recording.isRecording,
      recordingTargetId: () => recording.recordTargetId,
      recordingPeaks: () => recording.recordingPeaks.value,
      recordingStartMs: () => recording.recordingStartMs,
      recordingDurationMs: () => recording.recordingDurationMs,
      position: playback.position,
    );
    analysis = AnalysisCache(
      spectrograms: _spectrograms,
      monitor: _monitor,
      graphEngine: () => _graphEngine,
      // The live view pauses the transport before taking over the capture.
      pausePlayback: _playback.pause,
      isPlaying: () => playback.isPlaying,
      // Recording owns the capture — the live view can't start during a take,
      // INCLUDING the arm window before isRecording flips true (a live start
      // slipping in there would pull input frames from under the take).
      isRecording: () => recording.isRecording || recording.isStartingTake,
      selectedTrack: () => selectedTrack,
      selectedClipId: () => _selectedClipId,
      trackById: _trackById,
      position: playback.position,
      seek: playback.seek,
    );
    recording = RecordingController(
      graphEngine: () => _graphEngine,
      playback: playback,
      mixer: mixer,
      analysis: analysis,
      // The ONE busy gate: start guard (Stop→Record re-arm race) + finalize.
      busyGate: _busyGate,
      audioFiles: _audioFiles,
      waveforms: _waveforms,
      appSettings: _appSettings,
      practiceId: _practiceId,
      finalizer: _finalizer,
      practice: () => _practice,
      selectedTrackId: () => _selectedTrackId,
      trackById: _trackById,
      updateTrack: _updateTrack,
      // Take naming stays here (numbering reads the active interpretation).
      appendVocal:
          ({
            required String path,
            required WaveformResult analysis,
            required int startMs,
          }) => _appendVocal(
            path: path,
            displayName: 'Take ${_nextTakeNumber()}',
            analysis: analysis,
            startMs: startMs,
          ),
      createEmptyVocalTake: createEmptyVocalTake,
      selectRecordedClip: _selectRecordedClip,
      save: _save,
      toast: _toast,
      friendlyError: _friendlyError,
    );
    versions = InterpretationsController(
      engine: _playback,
      analysis: analysis,
      busyGate: _busyGate,
      audioFiles: _audioFiles,
      practice: () => _practice,
      setPractice: (next) => _practice = next,
      save: _save,
      ensureSelection: _ensureSelection,
      toast: _toast,
      isRecording: () => recording.isRecording,
      isSeparating: () => isSeparating,
    );
    // Forward every controller's notifications through this VM so all existing
    // `ListenableBuilder(listenable: vm)` / `Listenable.merge([vm, …])`
    // consumers keep rebuilding unchanged.
    for (final Listenable c in [
      _busyGate,
      playback,
      mixer,
      analysis,
      recording,
      versions,
    ]) {
      c.addListener(notifyListeners);
    }

    load = Command0(_load)..execute();
    // Freshness reloads when sync applies remote changes to THIS practice.
    _remoteSub = remoteApplied?.listen(_onRemoteApplied);
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

  /// Resolves where an export is written (the platform save panel by default);
  /// injected so tests can drive [exportTrackAudio] without the plugin.
  final Future<String?> Function(String suggestedName) _pickSaveLocation;

  /// The production picker: the macOS save panel via file_selector. Returns
  /// null when the user cancels.
  static Future<String?> _systemSaveLocation(String suggestedName) async {
    final location = await getSaveLocation(suggestedName: suggestedName);
    return location?.path;
  }

  /// The global audio settings (devices, record/monitor gains) — used by the mixer's mic strip.
  AppSettingsService get appSettings => _appSettings;
  final String _practiceId;
  String get practiceId => _practiceId;

  /// The playback engine — owned here (created, attached to settings,
  /// disposed); controllers only borrow it.
  final PlaybackEngine _playback;
  final AudioEditService _edits;

  /// Probes a media file's declared duration for the import admission gate
  /// (see [_admitImportDuration]); the default is the engine's container probe.
  final Duration? Function(String path) _probeDuration;
  final MonitorService _monitor;
  final RecordingFinalizer _finalizer;
  @override
  MonitorService get monitor => _monitor;

  // The five feature controllers (constructed + wired in the constructor).
  late final BusyGate _busyGate;
  late final PlaybackController playback;
  late final MixerController mixer;
  late final AnalysisCache analysis;
  late final RecordingController recording;

  /// Interpretations. (Named `versions` — `interpretations` is the list getter.)
  late final InterpretationsController versions;

  late final Command0<SongPractice> load;

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

  String _friendlyError(Object e) => switch (e) {
    FormatException(:final message) => message,
    // Carry user-presentable copy (duration cap / density / rate / IO).
    MediaTooLongException() => e.toString(),
    MediaUnsupportedException() => e.toString(),
    _ => 'Something went wrong.',
  };

  SongPractice? _practice;
  SongPractice? get practice => _practice;

  /// Per-row content hashes of the persisted practice as THIS view model last
  /// loaded or saved it — the BASE side of the repository's three-way save
  /// (see [Loaded]). Captured by [_load], handed to every save, and replaced
  /// by the fresh map each successful save (and the failed-save reload)
  /// returns, so a stale in-memory aggregate can never revert or tombstone
  /// rows that changed underneath it (e.g. applied by sync).
  Map<String, String> _baseHashes = const {};

  // --- Sync freshness ---------------------------------------------------------

  StreamSubscription<RemoteApplied>? _remoteSub;

  /// True while a save is writing/re-reading; a remote-applied reload must not
  /// interleave its own load with it (the save already returns the fresh
  /// aggregate + base).
  bool _saving = false;

  /// Bumped whenever a save or a newer remote reload starts, so an older
  /// in-flight reload's result is discarded instead of clobbering fresher
  /// state (the classic stale-read-completes-last race).
  int _reloadEpoch = 0;

  /// Freshness-only reload when sync applied remote rows belonging to THIS
  /// practice. Heavily guarded: skipped while anything is in flight (busy
  /// gate, recording, separating, saving) — skipping is always SAFE because
  /// every save three-way merges against [_baseHashes]; this reload only
  /// makes the remote edit visible sooner and refreshes the base so the next
  /// save resolves against it.
  Future<void> _onRemoteApplied(RemoteApplied event) async {
    if (_disposed || _practice == null || load.running) return;
    if (!event.practiceIds.contains(_practiceId)) return;
    if (busy || isRecording || isSeparating || _saving) return;
    final epoch = ++_reloadEpoch;
    final loaded = await _repository.loadPractice(_practiceId);
    if (_disposed || epoch != _reloadEpoch) return;
    if (busy || isRecording || isSeparating || _saving) return;
    if (loaded is! Ok<Loaded<SongPractice>>) {
      // Deleted remotely (or unreadable): keep showing the current state. A
      // later save aborts safely on the tombstoned parent; navigation-on-
      // remote-delete is deliberately out of scope here.
      return;
    }
    _practice = loaded.value.aggregate;
    _baseHashes = loaded.value.baseHashes;
    _ensureSelection();
    playback.ensureLoopRange();
    await mixer.resyncPlayback();
    notifyListeners();
  }

  /// The one busy/error latch (spinner + structural-edit mutex).
  bool get busy => _busyGate.busy;
  String? get errorMessage => _busyGate.errorMessage;

  /// 0..1 while a stem separation runs; null when idle. Drives a progress bar —
  /// separation can take minutes, unlike the ~1s import busy spinner.
  double? _separationProgress;
  double? get separationProgress => _separationProgress;
  bool get isSeparating => _separationProgress != null;

  /// Whether the separation engine is usable; gates the "Separate" action.
  bool _separationAvailable = false;
  bool get separationAvailable => _separationAvailable;

  /// The native engine, when it's the active backend (only it supports device I/O + monitoring).
  GraphAudioPlaybackService? get _graphEngine {
    final p = _playback;
    return p is GraphAudioPlaybackService ? p : null;
  }

  // --- Selection -------------------------------------------------------------

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
    if ((analysis.dockOpen && analysis.dockShowsTake) || _inspectorOpen) {
      analysis.ensureSelectedSpectrogram();
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
    if ((analysis.dockOpen && analysis.dockShowsTake) || _inspectorOpen) {
      analysis.ensureSelectedSpectrogram();
    }
  }

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

  // --- Selection hooks for the controllers -----------------------------------

  /// Selects the lane a finished recording landed on and, when [clipId] is
  /// non-null, the punched-in clip itself. A null [clipId] (new-take case)
  /// leaves the clip selection untouched, matching the pre-controller flow.
  void _selectRecordedClip(String trackId, String? clipId) {
    _selectedTrackId = trackId;
    if (clipId != null) _selectedClipId = clipId;
  }

  /// Selects a just-created/duplicated bus. Bus and track selection are
  /// mutually exclusive (the inspector owns one or the other) — the track/clip
  /// selection is cleared so two lanes never highlight at once.
  void _selectBusAfterMutation(String busId) {
    _selectedBusId = busId;
    _selectedTrackId = null;
    _selectedClipId = null;
  }

  /// Clears the bus selection when [busId] (just deleted) was selected.
  void _clearBusSelectionIf(String busId) {
    if (_selectedBusId == busId) _selectedBusId = null;
  }

  @override
  AudioTrack? get selectedTrack {
    final id = _selectedTrackId;
    return id == null ? null : _trackById(id);
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
    // The reference lane too: _updateTrack routes note updates to it, so
    // reference notes (e.g. arriving via sync) must show up here — mirroring
    // SongPractice.noteCount / activeNoteCount.
    final ref = practice.reference;
    if (ref != null) collect(ref);
    for (final v in practice.activeVocalTracks) {
      collect(v);
    }
    out.sort((a, b) => a.note.positionMs.compareTo(b.note.positionMs));
    return out;
  }

  // --- Panels (left inspector / right tabs) ----------------------------------

  bool _lyricsOpen = true;
  bool get lyricsOpen => _lyricsOpen;
  void toggleLyricsPanel() {
    _lyricsOpen = !_lyricsOpen;
    notifyListeners();
  }

  bool _inspectorOpen = true;
  bool get inspectorOpen => _inspectorOpen;
  void toggleInspectorPanel() {
    _inspectorOpen = !_inspectorOpen;
    notifyListeners();
    // The inspector shows analysis, so opening it must trigger the compute the
    // dock would otherwise be responsible for.
    if (_inspectorOpen) analysis.ensureSelectedSpectrogram();
  }

  RightTab _rightTab = RightTab.lyrics;
  RightTab get rightTab => _rightTab;
  void setRightTab(RightTab tab) {
    if (_rightTab == tab) return;
    _rightTab = tab;
    notifyListeners();
  }

  // --- Lyrics --------------------------------------------------------------

  List<LyricLine> get lyrics => _practice?.lyrics ?? const [];
  bool get hasLyrics => lyrics.isNotEmpty;

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
    if (isRecording || busy) {
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
  /// Runs on the same save chain as [_save] (one in-flight write at a time).
  Future<void> _setLyrics(List<LyricLine> lines) async {
    final practice = _practice;
    if (practice == null) return;
    _practice = practice.copyWith(lyrics: lines);
    await _enqueueSave(_writeLyrics);
  }

  /// The lyrics-only persist body (see [_setLyrics]); queued via [_enqueueSave].
  Future<void> _writeLyrics() async {
    final sent = _practice;
    if (sent == null) return;
    // Same save bracket as [_doSave]: supersede in-flight remote reloads and
    // block new ones until the fresh base is captured.
    _reloadEpoch++;
    _saving = true;
    try {
      final result = await _repository.savePracticeWithBase(
        sent,
        baseHashes: _baseHashes,
      );
      if (_disposed) return;
      if (result is Ok<Loaded<SongPractice>>) {
        _baseHashes = result.value.baseHashes;
        // Keep a model mutated during the write (see [_doSave]); the queued
        // save behind this one persists it against the fresh base above.
        if (identical(_practice, sent)) {
          _practice = result.value.aggregate;
        }
      } else {
        // Mirror [_doSave]'s failure path: revert the optimistic lyrics to
        // what's actually on disk, so the screen never keeps showing lines a
        // failed write already lost (they'd silently vanish on restart). The
        // identical() guard protects a NEWER queued model from the revert,
        // exactly as on the success path.
        final reloaded = await _repository.loadPractice(sent.id);
        if (reloaded is Ok<Loaded<SongPractice>> &&
            identical(_practice, sent)) {
          _practice = reloaded.value.aggregate;
          _baseHashes = reloaded.value.baseHashes;
        }
        _toast("Couldn't save your changes.");
      }
    } finally {
      _saving = false;
    }
    notifyListeners();
  }

  // --- Loading -------------------------------------------------------------

  Future<Result<SongPractice>> _load() async {
    // Load through the base-hash path: the hashes captured here are what every
    // later save three-way merges against (see [_baseHashes]).
    final loaded = await _repository.loadPractice(_practiceId);
    if (loaded is! Ok<Loaded<SongPractice>>) {
      return Result.error((loaded as Error<Loaded<SongPractice>>).error);
    }
    final practice = loaded.value.aggregate;
    _practice = practice;
    _baseHashes = loaded.value.baseHashes;
    _ensureSelection();
    _ensureTimeline();
    playback.ensureLoopRange();
    // Render the workspace as soon as the project DATA is ready (tracks + cached waveforms), so
    // there is no blank loading screen; the audio engine sync below continues in the background.
    notifyListeners();
    if (analysis.dockOpen && analysis.dockShowsTake) {
      unawaited(analysis.ensureSelectedSpectrogram());
    }
    // Compile-time demo/screenshot hooks (see AppConfig).
    analysis.applyDemoHooks(practice.vocalTracks);
    await mixer.resyncPlayback();
    notifyListeners();
    return Result.ok(practice);
  }

  // --- Importing audio -----------------------------------------------------

  /// Import admission gate: refuses a file whose DECLARED duration exceeds the
  /// playback cap BEFORE anything happens — the decode-time budget alone would
  /// only reject the file at playback staging, after it was already copied,
  /// waveform-analyzed and persisted as an unplayable clip. Containers that
  /// declare no length (probe returns null) are admitted; the native decode
  /// budget still bounds them later. Returns true when the import may proceed.
  bool _admitImportDuration(String sourcePath) {
    Duration? declared;
    try {
      declared = _probeDuration(sourcePath);
    } catch (_) {
      // A probe that can't run (no engine dylib in plain unit tests, an
      // unreadable file) must admit: the native decode budget still bounds
      // the file at staging, this gate only improves WHERE the refusal lands.
      declared = null;
    }
    if (declared != null && declared > maxPlaybackDuration) {
      _toast(
        'That file is ${declared.inMinutes} minutes long. The limit is '
        '${maxPlaybackDuration.inMinutes} minutes.',
      );
      return false;
    }
    return true;
  }

  /// Imports [sourcePath] into the sandbox, TRANSCODES compressed media to an
  /// engine-streamable WAV once (the industry import discipline — decode at
  /// import, never in full at every open; see ensureStreamableWav), and runs
  /// [body] with the playable copy. [body] returns whether the import was
  /// COMMITTED (its save landed): on false — a failed save reports through
  /// _doSave's revert+toast, not an exception — and on any throw, every file
  /// this call created is deleted before returning. A failed import must not
  /// strand bytes in the official audio directory: if the leftover happens to
  /// be a canonical mono WAV, boot recovery would resurrect it as a phantom
  /// take pointing at a practice whose save never happened.
  Future<void> _withImportedAudio(
    String sourcePath,
    Future<bool> Function(String dest) body,
  ) async {
    final imported = await _audioFiles.importAudio(
      practiceId: _practiceId,
      sourcePath: sourcePath,
    );
    var dest = imported;
    var committed = false;
    try {
      dest = await _edits.ensureStreamableWav(
        practiceId: _practiceId,
        path: imported,
      );
      if (dest != imported) {
        // The compressed original is now redundant: the WAV is the track.
        await _audioFiles.deleteAudio(imported);
      }
      committed = await body(dest);
    } finally {
      if (!committed) {
        try {
          await _audioFiles.deleteAudio(dest);
          if (dest != imported) await _audioFiles.deleteAudio(imported);
        } catch (_) {
          // Best-effort: the orphan sweep remains the backstop.
        }
      }
    }
  }

  /// Imports [sourcePath] as the accompaniment (replacing any existing one).
  Future<void> addAccompaniment(String sourcePath) async {
    if (_practice == null) return;
    // No concurrent structural edit. isRecording is part of EVERY structural
    // pre-guard: the gate must be free when the stop finalize needs it, or the
    // finalize would be rejected and the take stranded until the boot sweep.
    if (busy || isSeparating || isRecording) return;
    if (!_admitImportDuration(sourcePath)) return;
    await _busyGate.run(() async {
      await _withImportedAudio(sourcePath, (dest) async {
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
        if (cur == null) return false;
        final old = cur.accompaniment;
        final staleSource = cur.separationSource;
        // A manual import replaces any separation-derived backing — drop the kept
        // original so "Re-separate" can't later target a stale source.
        _practice = cur.copyWith(
          accompaniment: track,
          clearSeparationSource: true,
        );
        // Persist-before-delete: the replaced files fall only after the save
        // landed. Deleting them first meant a failed save reverted the model
        // to rows whose audio was already gone.
        if (!await _save()) return false;
        if (old != null) {
          for (final c in old.clips) {
            await _audioFiles.deleteAudio(c.audioFilePath);
          }
        }
        if (staleSource != null) {
          await _audioFiles.deleteAudio(staleSource);
        }
        return true;
      });
    });
  }

  /// Imports [sourcePath] as the reference track / original song (replacing any
  /// existing one).
  Future<void> addReference(String sourcePath) async {
    if (_practice == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    if (!_admitImportDuration(sourcePath)) return;
    await _busyGate.run(() async {
      await _withImportedAudio(sourcePath, (dest) async {
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
        if (cur == null) return false;
        final old = cur.reference;
        final staleSource = cur.separationSource;
        _practice = cur.copyWith(reference: track, clearSeparationSource: true);
        // Persist-before-delete (see addAccompaniment).
        if (!await _save()) return false;
        if (old != null) {
          for (final c in old.clips) {
            await _audioFiles.deleteAudio(c.audioFilePath);
          }
        }
        if (staleSource != null) {
          await _audioFiles.deleteAudio(staleSource);
        }
        return true;
      });
    });
  }

  /// Imports [sourcePath] as a new vocal take.
  Future<void> addVocalTake(String sourcePath) async {
    final practice = _practice;
    if (practice == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    if (!_admitImportDuration(sourcePath)) return;
    await _busyGate.run(() async {
      await _withImportedAudio(sourcePath, (dest) async {
        final analysis = await _waveforms.analyze(dest);
        _appendVocal(
          path: dest,
          displayName: p.basenameWithoutExtension(sourcePath),
          analysis: analysis,
        );
        return _save();
      });
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
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    if (!_admitImportDuration(sourcePath)) return;
    await _busyGate.run(() async {
      await _withImportedAudio(sourcePath, (dest) async {
        final waveform = await _waveforms.analyze(dest);
        final clip = AudioClip(
          id: newId(),
          audioFilePath: dest,
          durationMs: waveform.durationMs,
          startMs: math.max(0, position.value.inMilliseconds),
          waveformPeaks: waveform.peaks,
        );
        _updateTrack(target.id, (t) => t.copyWith(clips: [...t.clips, clip]));
        _selectedTrackId = target.id;
        _selectedClipId = clip.id;
        analysis.onTrackAudioChanged(target.id);
        return _save();
      });
    });
  }

  // --- Source separation -----------------------------------------------------

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
    if (isSeparating || busy) return; // no concurrent / re-entrant runs
    // Residual race only: the menu item is hidden while unavailable (the
    // availability probe resolves before any menu can be shown), so this
    // guard fires only if a click slipped in before the probe landed.
    if (!_separationAvailable) {
      _toast('Source separation is not available in this version.');
      return;
    }
    if (isRecording) {
      _toast('Stop recording to separate the backing.');
      return;
    }
    // The original to separate + keep: prefer the Reference slot (where the
    // original lives), else the right-clicked track. Backings are single-clip
    // lanes, so the primary clip's file IS the original recording.
    final original = practice.reference ?? source;
    final originalPath = original.primaryClip?.audioFilePath ?? '';
    final baseName = original.displayName.replaceAll(
      RegExp(r'\s*\(Instrumental\)$'),
      '',
    );

    _separationProgress = 0;
    notifyListeners();
    await _busyGate.run(() async {
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
                  clips: original.clips,
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
          final prevPlayRef = mixer.playReference;
          mixer.setPlayReferenceSilently(false); // sing over the instrumental
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
            // Restore the pre-separation backing flag.
            mixer.setPlayReferenceSilently(prevPlayRef);
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

        // The instrumental is live + persisted. Delete any prior backing files
        // that are no longer referenced — never the kept original or new
        // instrumental. Errors here must NOT roll back the live track.
        for (final old in [oldAccompaniment, oldReference]) {
          if (old == null) continue;
          for (final c in old.clips) {
            if (c.audioFilePath == originalPath ||
                c.audioFilePath == instDest) {
              continue;
            }
            analysis.evictAnalysis(c.audioFilePath);
            try {
              await _audioFiles.deleteAudio(c.audioFilePath);
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

  // --- Exporting audio -------------------------------------------------------
  // The escape hatch that guarantees audio is never trapped in the app's
  // private container: any track's audio can be copied out to a user-chosen
  // location, byte-for-byte.

  /// Exports a track's audio to a user-chosen location. Single-clip lanes
  /// only (the file IS the lane); a multi-clip lane is refused with a pointer
  /// to the per-clip export ([exportClipAudio]) — v1 does not render a lane's
  /// clips + gaps into one file.
  Future<void> exportTrackAudio(String trackId) async {
    final practice = _practice;
    final track = _trackById(trackId);
    if (practice == null || track == null) return;
    if (track.clips.isEmpty) {
      _toast('This take has no audio to export yet.');
      return;
    }
    if (track.clips.length > 1) {
      _toast(
        'This take has multiple clips. Export each clip from its own '
        'right-click menu.',
      );
      return;
    }
    final clip = track.primaryClip!;
    await _exportFile(
      clip.audioFilePath,
      suggestedName: _exportName(
        practice.title,
        track.displayName,
        clip.audioFilePath,
      ),
    );
  }

  /// Exports one clip's audio file — the per-clip path for multi-clip lanes.
  Future<void> exportClipAudio(String trackId, String clipId) async {
    final practice = _practice;
    final track = _trackById(trackId);
    final clip = track?.clipById(clipId);
    if (practice == null || track == null || clip == null) return;
    // Number the clip when the lane holds several, so exporting two clips of
    // the same take doesn't suggest the same file name twice.
    final index = track.clips.indexWhere((c) => c.id == clipId);
    final suffix = track.clips.length > 1 ? ' (clip ${index + 1})' : '';
    await _exportFile(
      clip.audioFilePath,
      suggestedName: _exportName(
        practice.title,
        '${track.displayName}$suffix',
        clip.audioFilePath,
      ),
    );
  }

  /// `"{practice title} - {track name}{source extension}"`, with path-hostile
  /// characters flattened so the suggestion is a valid file name. The source
  /// extension is kept (an imported mp3 exports as .mp3; recordings are .wav).
  static String _exportName(String title, String trackName, String sourcePath) {
    final ext = p.extension(sourcePath);
    final base = '$title - $trackName'.replaceAll(RegExp(r'[/\\:]'), '-');
    return '$base${ext.isEmpty ? '.wav' : ext}';
  }

  /// Copies [sourcePath] to a location picked via [_pickSaveLocation]. A null
  /// pick (user cancelled) exits silently; everything else toasts an outcome.
  Future<void> _exportFile(
    String sourcePath, {
    required String suggestedName,
  }) async {
    final source = File(sourcePath);
    if (sourcePath.isEmpty || !await source.exists()) {
      _toast('The audio file for this track is missing.');
      return;
    }
    final String? destPath;
    try {
      destPath = await _pickSaveLocation(suggestedName);
    } catch (_) {
      _toast('Could not open the save dialog.');
      return;
    }
    if (destPath == null) return; // user cancelled
    try {
      // Overwrite is intentional: the save panel already confirmed replacing.
      await source.copy(destPath);
    } catch (_) {
      _toast('Could not export the audio to that location.');
      return;
    }
    _toast('Exported to $destPath');
  }

  // --- Takes -----------------------------------------------------------------

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
    for (final t in versions.vocalTakes) {
      final m = re.firstMatch(t.displayName);
      if (m != null) {
        final n = int.parse(m.group(1)!);
        if (n > max) max = n;
      }
    }
    return max + 1;
  }

  // --- Track mutations -----------------------------------------------------

  Future<void> deleteTrack(String trackId) async {
    final practice = _practice;
    if (practice == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    await _busyGate.run(() async {
      // No awaits happened since the guard, so the model is still live here.
      final cur = _practice;
      if (cur == null) return;
      final List<AudioClip> doomed;
      if (cur.accompaniment?.id == trackId) {
        doomed = cur.accompaniment!.clips;
        _practice = cur.copyWith(clearAccompaniment: true);
      } else if (cur.reference?.id == trackId) {
        doomed = cur.reference!.clips;
        _practice = cur.copyWith(clearReference: true);
      } else {
        // Delete EVERY clip's file — a multi-clip take would otherwise orphan
        // its trailing clips' audio.
        doomed = [
          for (final t in cur.vocalTracks)
            if (t.id == trackId) ...t.clips,
        ];
        _practice = cur.copyWith(
          vocalTracks: cur.vocalTracks.where((t) => t.id != trackId).toList(),
        );
      }
      // Persist BEFORE deleting any audio, and ONLY delete when the save
      // succeeded — a failed save reverts the model back to referencing these
      // clips, so their files must survive (mirrors separateBacking and
      // InterpretationsController.delete).
      if (await _save()) {
        analysis.dropLaneState(trackId);
        for (final c in doomed) {
          analysis.evictAnalysis(c.audioFilePath);
          await _audioFiles.deleteAudio(c.audioFilePath);
        }
      }
    });
  }

  /// Deletes a single clip from a lane (Logic / Cubase style), keeping the lane
  /// and its other clips. Notes anchored inside the clip's span are dropped. Use
  /// [deleteTrack] to remove a whole lane.
  Future<void> deleteClip(String trackId, String clipId) async {
    final track = _trackById(trackId);
    final clip = track?.clipById(clipId);
    if (track == null || clip == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    await _busyGate.run(() async {
      final remaining = track.clips.where((c) => c.id != clipId).toList();
      // Drop markers anchored inside the deleted clip; for a range that starts
      // before the clip, clamp its end to the clip edge so it doesn't overhang the
      // gap left behind.
      final keptNotes = track.notes
          .where(
            (n) => n.positionMs < clip.startMs || n.positionMs >= clip.endMs,
          )
          .map(
            (n) =>
                n.positionMs < clip.startMs ? clampNoteEnd(n, clip.startMs) : n,
          )
          .toList();
      final keptMarkers = track.techniqueMarkers
          .where(
            (m) => m.positionMs < clip.startMs || m.positionMs >= clip.endMs,
          )
          .map(
            (m) => m.positionMs < clip.startMs
                ? clampMarkerEnd(m, clip.startMs)
                : m,
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
      // Persist BEFORE deleting the audio, and ONLY delete when the save
      // succeeded — a failed save reverts the model back to referencing this
      // clip, so its file must survive.
      if (await _save()) {
        analysis.evictAnalysis(clip.audioFilePath);
        await _audioFiles.deleteAudio(clip.audioFilePath);
      }
      analysis.onTrackAudioChanged(trackId);
    });
  }

  // --- Clip editing (drag-move, reorder, split, merge, …) -----------------

  /// Persists a finished drag-move and re-aligns offset-aware playback. A move
  /// changes the lane-relative offsets baked into a multi-clip take's aggregate
  /// analysis (its cache key embeds clip positions, so the old entry is simply
  /// stale) — recompute whatever analysis is on screen for the new layout. The
  /// ensure calls are cheap no-ops for single-clip takes and cached subjects.
  Future<void> commitTrackMove() async {
    await _save();
    if ((analysis.dockOpen && analysis.dockShowsTake) || _inspectorOpen) {
      unawaited(analysis.ensureSelectedSpectrogram());
    }
    analysis.refreshExpandedLanes();
  }

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
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    await _busyGate.run(() async {
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
      Future<void> discardCopies() async {
        for (final c in newClips) {
          await _audioFiles.deleteAudio(c.audioFilePath);
        }
      }

      final cur = _practice;
      if (cur == null) return discardCopies();
      final list = [...cur.vocalTracks];
      final idx = list.indexWhere((t) => t.id == trackId);
      if (idx < 0) return discardCopies(); // source removed during the copy
      list.insert(idx + 1, copy);
      _practice = cur.copyWith(vocalTracks: list);
      if (await _save()) {
        _toast('Take duplicated.');
      } else {
        // The save reverted the model; the copied files would sit orphaned
        // in edits/ until the boot sweep otherwise.
        await discardCopies();
      }
    });
  }

  /// Splits a vocal take at the playhead into two takes; the second is offset to
  /// the cut. Notes follow whichever side they fall on.
  Future<void> splitTrackAtPlayhead(String trackId) async {
    final practice = _practice;
    final src = _trackById(trackId);
    if (practice == null || src == null || src.isBacking) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    // Byte-splitting is a ONE-FILE operation: a multi-clip lane has several
    // independent files, and its lane span (startMs/durationMs) covers gaps no
    // file byte maps to — refuse rather than silently drop the other clips
    // (delete/move a clip from its menu instead).
    if (src.clips.length > 1) {
      _toast(
        'This take has multiple clips. Delete or move a clip from its menu '
        'instead of splitting the lane.',
      );
      return;
    }
    // Single-clip lane: the clip IS the lane (same span), so the cut position
    // maps 1:1 into the clip's file.
    final clip = src.primaryClip;
    final atMs = position.value.inMilliseconds;
    if (clip == null ||
        atMs <= clip.startMs + PlaybackController.minLoopMs ||
        atMs >= clip.endMs - PlaybackController.minLoopMs) {
      _toast(
        'Move the playhead onto this take to split it. The take spans '
        'only its highlighted region.',
      );
      return;
    }
    await _busyGate.run(() async {
      final parts = await _edits.splitWav(
        practiceId: _practiceId,
        sourcePath: clip.audioFilePath,
        splitFileMs: atMs - clip.startMs,
      );
      final firstAnalysis = await _waveforms.analyze(parts.firstPath);
      final secondAnalysis = await _waveforms.analyze(parts.secondPath);
      final first = src.copyWith(
        // The surviving take keeps its clip id (same row, new file + length).
        clips: [
          clip.copyWith(
            audioFilePath: parts.firstPath,
            durationMs: firstAnalysis.durationMs,
            waveformPeaks: firstAnalysis.peaks,
          ),
        ],
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
        // A FRESH clip id — otherwise the new take and the surviving `first`
        // take would share one clip primary key and the save transaction would
        // roll back on the duplicate INSERT, silently losing the split (and the
        // original file is already deleted by then).
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
      // Persist BEFORE deleting any old audio, so a failed save can't leave
      // the model pointing at a file that's already gone.
      if (await _save()) {
        analysis.evictAnalysis(clip.audioFilePath);
        await _audioFiles.deleteAudio(clip.audioFilePath);
        _toast('Take split.');
      } else {
        // The failed save reverted the model to the original one-clip take,
        // so the split halves are orphans — clean them up instead.
        try {
          await _audioFiles.deleteAudio(parts.firstPath);
          await _audioFiles.deleteAudio(parts.secondPath);
        } catch (_) {
          /* best-effort: a lingering working file is harmless */
        }
      }
    });
    // The surviving take keeps its id but now points at a new file.
    analysis.onTrackAudioChanged(trackId);
  }

  /// Merges a vocal take with the take in the lane below it: their audio is
  /// concatenated (inverse of split) and the result sits at the upper clip's
  /// position. The lower take's notes shift to follow the joined audio.
  Future<void> mergeTrackDown(String trackId) async {
    final practice = _practice;
    if (practice == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
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
    // Byte-concatenation joins exactly TWO files into one clip: a multi-clip
    // lane has several independent files (and its lane span covers gaps no file
    // byte maps to) — refuse rather than orphan the other clips' audio.
    if (top.clips.length > 1 || bottom.clips.length > 1) {
      _toast(
        'These takes have multiple clips and can\'t be merged into one lane.',
      );
      return;
    }
    // Single-clip lanes: each lane's clip IS the lane (same span).
    final topClip = top.primaryClip;
    final bottomClip = bottom.primaryClip;
    if (topClip == null || bottomClip == null) {
      _toast('An empty take has no audio to merge.');
      return;
    }
    await _busyGate.run(() async {
      final mergedPath = await _edits.concatWav(
        practiceId: _practiceId,
        firstPath: topClip.audioFilePath,
        secondPath: bottomClip.audioFilePath,
      );
      final analysisResult = await _waveforms.analyze(mergedPath);
      // Shift the bottom take's markers onto the merged timeline; range markers
      // shift their end by the same delta so the span is preserved.
      final delta = topClip.endMs - bottomClip.startMs;
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
        // The merged take keeps the top clip's id (same row, new file+length).
        clips: [
          topClip.copyWith(
            audioFilePath: mergedPath,
            durationMs: analysisResult.durationMs,
            waveformPeaks: analysisResult.peaks,
          ),
        ],
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
      // Persist BEFORE deleting any old audio, so a failed save can't leave
      // the model pointing at files that are already gone.
      if (await _save()) {
        analysis.dropLaneState(bottom.id); // the absorbed take is gone
        analysis.evictAnalysis(topClip.audioFilePath);
        analysis.evictAnalysis(bottomClip.audioFilePath);
        await _audioFiles.deleteAudio(topClip.audioFilePath);
        await _audioFiles.deleteAudio(bottomClip.audioFilePath);
        _toast('Takes merged.');
      } else {
        // The failed save reverted the model to the two original takes, so
        // the merged output is an orphan — clean it up instead.
        try {
          await _audioFiles.deleteAudio(mergedPath);
        } catch (_) {
          /* best-effort: a lingering working file is harmless */
        }
      }
    });
    // The merged take keeps the top take's id but now points at a new file.
    analysis.onTrackAudioChanged(trackId);
  }

  /// Recomputes the waveform (and invalidates the cached spectrogram) for
  /// every clip on a lane — useful after its audio files changed underneath.
  Future<void> reanalyzeTrack(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    if (busy || isSeparating || isRecording) {
      return;
    } // no concurrent structural edit
    await _busyGate.run(() async {
      // Analyze EVERY clip's file (a lane can hold several independent files),
      // then fold the results back in BY CLIP ID against the live model — a
      // clip added/removed during the analyze awaits must survive untouched.
      final updatedById = <String, AudioClip>{};
      for (final c in track.clips) {
        final waveform = await _waveforms.analyze(c.audioFilePath);
        updatedById[c.id] = c.copyWith(
          durationMs: waveform.durationMs,
          waveformPeaks: waveform.peaks,
        );
        analysis.evictAnalysis(c.audioFilePath);
      }
      _updateTrack(
        trackId,
        (t) => t.copyWith(
          clips: [for (final c in t.clips) updatedById[c.id] ?? c],
        ),
      );
      await _save();
    });
    analysis.onTrackAudioChanged(trackId);
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

  /// The tail of the save chain (see [_save]): the completion of the latest
  /// queued persist, with its error swallowed (each op surfaces its own).
  Future<void> _saveTail = Future.value();

  /// Serializes every persist of [_practice] (gated or not): two in-flight
  /// [SongPracticeRepository.savePracticeWithBase] calls would otherwise share
  /// one [_baseHashes] snapshot (two rapid interpretation clicks, a lyric
  /// stamp racing a gated save), and whichever landed second would three-way
  /// merge against a base that no longer describes the store. Each op starts
  /// only after the previous one fully completed, then reads the LIVE
  /// [_practice]/[_baseHashes]. Independent of [BusyGate]: ops never wait on
  /// the gate, so a gated save chaining after an ungated one cannot deadlock.
  Future<T> _enqueueSave<T>(Future<T> Function() op) {
    final next = _saveTail.then((_) => op());
    _saveTail = next.then<void>((_) {}, onError: (_) {});
    return next;
  }

  /// Persists [_practice]. Returns whether the write succeeded — callers that
  /// then delete files must gate the deletion on a successful save so a failed
  /// write can't leave the JSON referencing audio that's already gone.
  Future<bool> _save() => _enqueueSave(_doSave);

  Future<bool> _doSave() async {
    if (_practice == null) return false;
    _ensureTimeline();
    // A starting save supersedes any in-flight remote-applied reload (its read
    // predates this write) and blocks new ones until the fresh base is in.
    _reloadEpoch++;
    _saving = true;
    try {
      // Three-way save against the base captured at load / the previous save,
      // so a stale aggregate can't revert or tombstone concurrent (synced-in)
      // changes; the returned fresh base replaces ours for the next save.
      final sent = _practice!;
      final result = await _repository.savePracticeWithBase(
        sent,
        baseHashes: _baseHashes,
      );
      final ok = result is Ok<Loaded<SongPractice>>;
      // If the route was popped mid-save (e.g. a second back while finalizing a
      // recording), the write above still committed — the take is safe — but the
      // playback/selection resync below must not touch now-disposed notifiers.
      if (_disposed) return ok;
      if (ok) {
        _baseHashes = result.value.baseHashes;
        // Adopt the re-read aggregate only if no caller mutated the model
        // while the write was in flight: a save queued behind this one already
        // carries the newer model and will persist it against the fresh base
        // above — overwriting [_practice] here would silently drop that edit.
        if (identical(_practice, sent)) {
          _practice = result.value.aggregate;
        }
      } else {
        // Revert the optimistic in-memory model to what's actually on disk, so the
        // UI never keeps showing edits a failed write didn't persist. The base
        // hashes are refreshed with it (they must always describe _practice).
        // The same identical() guard as the success path applies: a save queued
        // behind this one carries a NEWER model — reverting over it would make
        // that save persist pre-edit state (a silent lost update). Let the
        // queued save retry the combined state; if it also fails, its own
        // revert (then identical) restores disk state consistently.
        final reloaded = await _repository.loadPractice(sent.id);
        if (reloaded is Ok<Loaded<SongPractice>> &&
            identical(_practice, sent)) {
          _practice = reloaded.value.aggregate;
          _baseHashes = reloaded.value.baseHashes;
        }
        _toast("Couldn't save your changes.");
      }
      _ensureSelection();
      playback.ensureLoopRange();
      await mixer.resyncPlayback();
      notifyListeners();
      return ok;
    } finally {
      _saving = false;
    }
  }

  // --- Retained cross-cutting delegates -------------------------------------
  // The widgets reach feature state directly (vm.playback / vm.mixer /
  // vm.analysis / vm.recording / vm.versions); every delegate below is kept
  // deliberately, with its reason, not as a transition shim.

  // AnalysisSource contract: this VM is the instance handed — typed as
  // AnalysisSource — to every dock/inspector analysis view (shared with the
  // technique screen's source), so it must implement the full interface. The
  // compute + caches live in [analysis]; playhead/seek live in [playback].
  @override
  ValueNotifier<Duration> get position => playback.position;
  @override
  Future<void> seek(Duration to) => playback.seek(to);
  @override
  int get analysisStartMs => analysis.analysisStartMs;
  @override
  int get analysisDurationMs => analysis.analysisDurationMs;
  @override
  VoiceMetrics? get selectedMetrics => analysis.selectedMetrics;
  @override
  Spectrogram? get selectedSpectrogram => analysis.selectedSpectrogram;
  @override
  VoiceProfile? get selectedProfile => analysis.selectedProfile;
  @override
  Ltas? get selectedLtas => analysis.selectedLtas;
  @override
  RegisterMap? get selectedRegister => analysis.selectedRegister;
  @override
  bool get spectrogramLoading => analysis.spectrogramLoading;
  @override
  bool get liveMonitor => analysis.liveMonitor;

  // Cross-cutting recording guard: read by the transport bar, the screen's
  // PopScope/back path, the lyrics import, and the separation/interpretation
  // guards above — a genuinely VM-level condition, not a mixer/playback one.
  bool get isRecording => recording.isRecording;

  // Cross-cutting stop: invoked from both the transport bar and the screen's
  // back/PopScope path (finalize the in-flight take before leaving the route).
  Future<void> stopRecording() => recording.stopRecording();

  // The one shared "persist pending live FX/level edits" commit — called from
  // four widget files (fx_panel, mixer_strip, left_panel, screen).
  Future<void> commitFx() => mixer.commitFx();

  @override
  void dispose() {
    _disposed = true;
    unawaited(_remoteSub?.cancel());
    // ORDER MATTERS. Recording first: it closes an in-flight take's WAV
    // handle, unregisters the quit finalizer and kills the device-watch/meter
    // timers that poll the engine over FFI — all before the engine goes away.
    recording.dispose();
    // Cancel an in-flight separation so its Python/torch/ffmpeg subprocess is
    // killed rather than orphaned when the user leaves mid-separation.
    if (isSeparating) _separation.cancel();
    analysis.dispose(); // stops the live-capture pump
    versions.dispose();
    mixer.dispose();
    playback.dispose(); // cancels the position/playing subs, frees position
    _busyGate.dispose();
    unawaited(_toasts.close());
    if (_playback is GraphAudioPlaybackService) {
      _appSettings.detachEngine(_playback);
    }
    unawaited(_playback.dispose());
    _monitor.dispose();
    super.dispose();
  }
}
