import 'dart:async';
import 'dart:isolate';

import 'package:flutter/foundation.dart';

import '../../../config/app_config.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/mic_permission.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/spectrogram_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/analysis_gate.dart';
import '../../../utils/lru_bytes_cache.dart';
import 'analysis_source.dart';

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

/// All analysis compute + caches for the song detail screen, and their view
/// chrome: per-path spectrogram/metrics/profile/LTAS/register caches with
/// loading/unavailable latches, whole-lane aggregate merging for multi-clip
/// takes, the wide-range EQ display spectrogram, the inline fold-out lanes,
/// the bottom dock (open/tab/height), and live-vs-take switching (owns the
/// live-monitor flag and the 30 ms capture pump feeding the [MonitorService]).
///
/// Implements [AnalysisSource] so the dock/inspector views could read it
/// directly; today the view model stays the instance handed to them (a pure
/// delegating facade over this cache), keeping their [Listenable] untouched.
/// Selection and the playhead come from the injected host closures — this
/// cache owns no model or selection state.
class AnalysisCache extends ChangeNotifier implements AnalysisSource {
  AnalysisCache({
    required SpectrogramService spectrograms,
    required MonitorService monitor,
    required GraphAudioPlaybackService? Function() graphEngine,
    required Future<void> Function() pausePlayback,
    required bool Function() isPlaying,
    required bool Function() isRecording,
    required AudioTrack? Function() selectedTrack,
    required String? Function() selectedClipId,
    required AudioTrack? Function(String) trackById,
    required ValueListenable<Duration> position,
    required Future<void> Function(Duration) seek,
    void Function(String)? toast,
    bool Function(String trackId)? isMovingLive,
    @visibleForTesting
    Future<bool> Function() requestMicPermission = MicPermission.request,
    @visibleForTesting LruBytesBudget? cacheBudget,
    @visibleForTesting DateTime Function() now = DateTime.now,
  }) : _aggAnalysisCache = LruBytesCache.shared(
         cacheBudget ?? analysisCacheBudget,
         _aggBytes,
       ),
       _spectrogramCache = LruBytesCache.shared(
         cacheBudget ?? analysisCacheBudget,
         _specBytes,
       ),
       _metricsCache = LruBytesCache.shared(
         cacheBudget ?? analysisCacheBudget,
         _metricsBytes,
       ),
       _eqSpectroCache = LruBytesCache.shared(
         cacheBudget ?? analysisCacheBudget,
         _specBytes,
       ),
       _spectrograms = spectrograms,
       _monitor = monitor,
       _graphEngine = graphEngine,
       _pausePlayback = pausePlayback,
       _isPlaying = isPlaying,
       _isRecording = isRecording,
       _selectedTrack = selectedTrack,
       _selectedClipId = selectedClipId,
       _trackById = trackById,
       _isMovingLive = isMovingLive ?? _never,
       _position = position,
       _seek = seek,
       _toast = toast,
       _requestMicPermission = requestMicPermission,
       _now = now {
    // Derived results are memoized under the SAME key as the entry they came
    // from, so the budget dropping a source leaves them with no owner. Wiring
    // the hooks here (rather than at construction) is what lets them close
    // over this cache.
    _spectrogramCache.onEvict = _dropDerived;
    _metricsCache.onEvict = _dropDerived;
    // A clip's two halves are one analysis: neither insert evicts the other.
    // Not the EQ display: it follows the clip under the playhead, not the
    // selected one, and pinned with a selected clip's halves it outlived the
    // display on screen.
    _metricsCache.partnerWith(_spectrogramCache);
    _aggAnalysisCache.onEvict = _dropAggDerived;
  }

  final SpectrogramService _spectrograms;
  final MonitorService _monitor;

  /// The native engine when active — the only backend with live input capture.
  final GraphAudioPlaybackService? Function() _graphEngine;

  /// Pauses playback before the live view starts (capture and playback share
  /// the engine's input focus).
  final Future<void> Function() _pausePlayback;
  final bool Function() _isPlaying;

  /// Recording owns the capture — the live view can't start during a take.
  final bool Function() _isRecording;

  // Host selection closures: the analysis subject follows the selection the
  // view model owns.
  final AudioTrack? Function() _selectedTrack;
  final String? Function() _selectedClipId;
  final AudioTrack? Function(String) _trackById;

  /// Whether a clip of the take is being moved live (see
  /// SongPracticeDetailViewModel.isMovingLive). Each tick of the move re-keys
  /// a multi-clip lane's merge, and nothing asks for its analysis meanwhile
  /// ([lacksAnalysis], [laneLacksData]): the lanes, the inspector and the
  /// dock each merged and cached every position the clip passed through.
  /// The commit asks for where it landed.
  final bool Function(String trackId) _isMovingLive;

  static bool _never(String _) => false;

  final ValueListenable<Duration> _position;
  final Future<void> Function(Duration) _seek;
  final void Function(String)? _toast;
  final Future<bool> Function() _requestMicPermission;

  /// Set in [dispose]. Guards async cache computes resolving after the view
  /// model (and this cache) are gone: every compute helper refuses to START
  /// once it is set, and re-checks after its await before touching a cache.
  /// What that buys is not the wasted writes — heavy computes queue through
  /// the app-wide analysis gate's two slots, so a multi-clip aggregate that
  /// kept feeding clips into it after its screen closed made the NEXT
  /// screen's analysis wait behind results no one would read. An
  /// already-admitted compute cannot be recalled (Isolate.run has no
  /// cancellation); the guarantee is that no FURTHER one is admitted.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  // --- Caches ---------------------------------------------------------------

  // The heavy caches are byte-budgeted LRUs: a long session used to keep
  // every take/lane/EQ result ever opened resident. Reads refresh recency,
  // so the on-screen subject is never the eviction victim. Sizes are the
  // typed-data payloads (the dominant term).
  static int _specBytes(Spectrogram s) => s.db.length * 4;
  // Budgeted per frame: the metrics payload plus the score curve its memoized
  // RegisterMap keeps alive beside it — that curve outlives eviction in the
  // plain derived map, so a budget that ignored it would understate what an
  // entry actually costs the process.
  static const int _registerScoreBytesPerFrame = 4;
  static int _metricsBytes(VoiceMetrics m) =>
      m.frames * (VoiceMetrics.bytesPerFrame + _registerScoreBytesPerFrame);
  final LruBytesCache<Spectrogram> _spectrogramCache;
  final LruBytesCache<VoiceMetrics> _metricsCache;
  // Derived voice profile (range/passaggio/dynamics) per take path, memoized so
  // it isn't recomputed on every unrelated rebuild. Their lifetime is the
  // SOURCE entry's — [_dropDerived] is wired as the metrics/spectrogram
  // caches' eviction hook, because the budget can drop a source on any insert
  // and a derived value keyed off a gone source has no owner left to drop it.
  final Map<String, VoiceProfile> _profileCache = {};
  // Long-term average spectrum + register map per take path, memoized likewise.
  final Map<String, Ltas> _ltasCache = {};
  final Map<String, RegisterMap> _registerCache = {};
  final Set<String> _spectrogramLoading = {};
  final Set<String> _metricsLoading = {};
  // The in-flight compute per path, so a second caller JOINS it instead of
  // skipping ahead. Kept in lockstep with the loading sets above (which drive
  // the spinner); these carry the future the joiner awaits. Without them a
  // caller that arrived mid-compute returned instantly with nothing, and the
  // aggregate merge — which refuses to cache a partial lane, correctly — threw
  // the whole lane away with no retry scheduled, leaving the dock blank until
  // the user changed the selection.
  final Map<String, Future<void>> _spectrogramInFlight = {};
  final Map<String, Future<void>> _metricsInFlight = {};

  // Drop an in-flight entry. A plain `map.remove(path)` inside an async body
  // reads as an unawaited Future (that IS the stored value), so the discard is
  // spelled out here instead of suppressed at each call site.
  static void _forgetInFlight(Map<String, Future<void>> m, String path) {
    m.remove(path);
  }

  /// Waits out [path]'s compute in [inFlight], then whatever replaced it as
  /// it ended. A compute evicted mid-flight lands empty and, for a
  /// re-analyze, starts its rerun as it ends (see [_recomputeOnStale]): a
  /// caller that joined it read the hole the dropped compute left, and a lane
  /// merge asked for in that window was left blank with no spinner — nothing
  /// asked again.
  static Future<void> _joinInFlight(
    Map<String, Future<void>> inFlight,
    String path,
  ) async {
    for (var p = inFlight[path]; p != null; p = inFlight[path]) {
      await p;
    }
  }

  /// Bumped by [evictAnalysis] per path (the lane merges folding that path
  /// are disowned through [_aggDisowned]). A compute captures the generation
  /// it started under and, if it changed by the time the result lands, that
  /// result describes bytes the eviction has already declared stale: it must
  /// not be cached, and must not latch "unavailable" over the new file.
  /// Re-analyze is exactly this window — it evicts each clip while the dock
  /// may still be computing it — and without the check the stale result was
  /// written back under a key nothing invalidates again, so the take showed
  /// the OLD audio's analysis (or a permanent "unavailable") for the rest of
  /// the session.
  final Map<String, int> _pathGeneration = {};

  /// Paths whose latest [evictAnalysis] asked for a stale in-flight compute to
  /// be REDONE (re-analyze: the same path now holds new bytes). Every other
  /// eviction is for a file that left the model — a deleted take, a split or
  /// merged-away clip — where redoing it only re-decoded a dead file into one
  /// of the app-wide analysis slots.
  final Set<String> _recomputeOnStale = {};
  int _generationOf(String path) => _pathGeneration[path] ?? 0;

  /// Whether an analysis of [path] begun at [generation] is still wanted once
  /// it reaches the gate: this cache is still open and the file unchanged.
  /// Asked before the isolate starts, so a song left with several lanes open
  /// doesn't run their queued analyses for no one.
  bool _wanted(String path, int generation) =>
      !_disposed && generation == _generationOf(path);
  void _bumpGeneration(String path) =>
      _pathGeneration[path] = _generationOf(path) + 1;
  // Paths whose compute finished but produced nothing (non-WAV imports, missing
  // or truncated files). Latched so a null result is never retried — otherwise
  // the always-on inspector would re-launch the read + isolate every rebuild.
  final Set<String> _spectrogramUnavailable = {};
  final Set<String> _metricsUnavailable = {};
  // Whole-lane aggregate analysis for multi-clip takes: each clip's per-path
  // result merged in timeline order, keyed by [aggregateKeyFor] (which embeds
  // the ordered clip ids, so the key self-invalidates when the clip set
  // changes). Only multi-clip takes ever populate these.
  //
  // Spectrogram and metrics share ONE entry so eviction can never keep half a
  // subject: a merged spectrogram costs ~856 B per frame against the metrics'
  // 41, so any split budget drops the spectrogram first and leaves behind a
  // half that still answers `containsKey` — which the ready latch reads as
  // "done", so the missing half was never recomputed and the view stayed
  // empty without even a loading state. [LruBytesCache] keeps a lone
  // oversized entry — for a pair that means both halves stay, never one.
  static int _aggBytes(TakeAnalysis a) {
    final s = a.spectrogram;
    final m = a.metrics;
    return (s == null ? 0 : _specBytes(s)) + (m == null ? 0 : _metricsBytes(m));
  }

  final LruBytesCache<TakeAnalysis> _aggAnalysisCache;
  final Map<String, VoiceProfile> _aggProfileCache = {};
  final Map<String, Ltas> _aggLtasCache = {};
  final Map<String, RegisterMap> _aggRegisterCache = {};
  final Set<String> _aggLoading = {};
  final Set<String> _aggUnavailable = {};

  /// Lane merges in flight (keys in [_aggLoading]) that an [evictAnalysis]
  /// of a file they fold has disowned: their result describes bytes already
  /// declared stale, so it is neither cached nor latched, and the lane is
  /// merged again if a view still shows it (see [_showsLane]).
  final Set<String> _aggDisowned = {};

  /// The lane merge in flight per lane (track id → its key). A lane's key
  /// moves with every tick of a clip drag (it embeds the clip starts), and
  /// the inspector asks for each new one: every frame started a merge of its
  /// own, copying all the lane's clip analyses into a fresh isolate. One runs
  /// per lane; an ask for a newer key waits in [_aggQueued] (loading
  /// meanwhile) and is answered by ONE rerun for the lane as it is when the
  /// running merge lands.
  final Map<String, String> _aggInFlight = {};
  final Map<String, ({String key, Completer<void> done})> _aggQueued = {};

  /// Everything memoized FROM a per-path analysis: the profile and register
  /// map derive from the metrics, the LTAS from the spectrogram and metrics.
  void _dropDerived(String key) {
    _profileCache.remove(key);
    _ltasCache.remove(key);
    _registerCache.remove(key);
  }

  /// [_dropDerived] for the whole-lane aggregate family.
  void _dropAggDerived(String key) {
    _aggProfileCache.remove(key);
    _aggLtasCache.remove(key);
    _aggRegisterCache.remove(key);
  }

  /// Bytes this screen's caches currently charge to the shared analysis
  /// budget, for tests pinning that a disposed screen stops charging it.
  @visibleForTesting
  int get budgetBytes => _spectrogramCache.budget.bytes;

  /// Drops one half of a clip's analysis, as the shared byte budget does under
  /// pressure (the halves are separate entries).
  @visibleForTesting
  void debugDropHalf(String path, {required bool metrics}) {
    if (metrics) {
      _metricsCache.remove(path);
    } else {
      _spectrogramCache.remove(path);
    }
  }

  /// Memoized derived results currently held, for tests pinning that derived
  /// state never outlives its source entry.
  @visibleForTesting
  int get derivedEntries =>
      _profileCache.length +
      _ltasCache.length +
      _registerCache.length +
      _aggProfileCache.length +
      _aggLtasCache.length +
      _aggRegisterCache.length;
  // Full-range (to ~18 kHz) display spectrogram per clip file, for the FX EQ's
  // input/output spectrum analyzer. Kept separate from the 5 kHz analysis
  // spectrogram, which is tuned for voice metrics rather than a wide display.
  static const double _eqDisplayMaxHz = 18000;
  final LruBytesCache<Spectrogram> _eqSpectroCache;
  final Set<String> _eqSpectroLoading = {};
  final Set<String> _eqSpectroUnavailable = {};
  // Inline analysis sub-lanes (view state, not persisted): which tracks are
  // folded open, and which lanes each shows.
  final Set<String> _expandedTracks = {};
  final Map<String, Set<AnalysisLane>> _trackLanes = {};

  // --- Bottom dock (Voiceprint + Mixer) ----------------------------------

  // Defaults closed; the dart-define is a dev hook for previewing the dock.
  bool _dockOpen = AppConfig.voiceprintDemo;
  bool get dockOpen => _dockOpen;

  DockTab _dockTab = DockTab.analysisSummary;
  DockTab get dockTab => _dockTab;

  /// Every analysis tab (all but Mixer) reads the selected take's
  /// spectrogram/metrics, so opening one computes them. The view model's
  /// selection triggers and the interpretation switch gate on this too.
  bool get dockShowsTake => _dockTab != DockTab.mixer;

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

  void toggleDock() => setDockOpen(!_dockOpen);

  void setDockOpen(bool open) {
    if (_dockOpen == open) return;
    _dockOpen = open;
    notifyListeners();
    if (open && dockShowsTake) {
      unawaited(_ensureDockAnalysis());
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
    if (dockShowsTake) {
      unawaited(_ensureDockAnalysis());
    }
    if (!activeTabSupportsLive && _liveMonitor) {
      unawaited(setLiveMonitor(false));
    }
  }

  // --- Live monitor (real-time spectrogram) ------------------------------

  bool _liveMonitor = false;
  @override
  bool get liveMonitor => _liveMonitor;

  // Pulls captured frames from the engine into the live-analysis monitor.
  // Recording never uses this pump (its capture belongs to the take).
  Timer? _captureTimer;

  /// Intent latch for a live start parked on its pause await: set
  /// synchronously at [setLiveMonitor]\(true)'s entry, cleared when the start
  /// arms, aborts, or ANY [setLiveMonitor]\(false) arrives. Without it,
  /// startRecording's `setLiveMonitor(false)` would no-op on the
  /// `_liveMonitor == on` early-return while the start is still parked, and
  /// the capture pump would arm DURING the take (a later Live-off would then
  /// stopCapture underneath the recording).
  bool _liveStartPending = false;

  /// Starts/stops feeding the engine's live input into the spectrogram analyzer. Can't run while
  /// recording — including REC's arm window, see the host's isRecording wiring — because the
  /// engine capture is single-owner and belongs to the take; pauses playback when starting.
  Future<void> setLiveMonitor(bool on) async {
    // Cancel a start parked on its pause await BEFORE the early-return below:
    // while parked, _liveMonitor is still false, so an OFF request would
    // otherwise be swallowed and the abandoned start would arm anyway.
    if (!on) _liveStartPending = false;
    if (_liveMonitor == on) return;
    final engine = _graphEngine();
    if (on) {
      if (_isRecording() || engine == null) return;
      _liveStartPending = true;
      // Without the grant the capture is fed silence: ask first, as Monitor
      // and Record do, so a refusal says why the view stays flat (and leaves
      // the transport playing).
      if (!await _requestMicPermission()) {
        if (_liveStartPending && !_disposed) {
          _toast?.call(MicPermission.deniedMessage);
        }
        _liveStartPending = false;
        return;
      }
      if (_disposed || !_liveStartPending || _isRecording()) {
        _liveStartPending = false;
        return;
      }
      if (_isPlaying()) await _pausePlayback();
      // Re-check after EVERY await: a recording start (which owns the
      // capture) or a cancel/dispose during the pause aborts this start —
      // nothing was armed yet, so there is nothing to undo.
      if (_disposed || !_liveStartPending || _isRecording()) {
        _liveStartPending = false;
        return;
      }
      _liveStartPending = false;
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
        final mono = Float32List(n);
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

  // --- AnalysisSource -------------------------------------------------------

  @override
  AudioTrack? get selectedTrack => _selectedTrack();

  @override
  MonitorService get monitor => _monitor;

  @override
  ValueListenable<Duration> get position => _position;

  @override
  Future<void> seek(Duration to) => _seek(to);

  /// Cache key for a take's whole-lane aggregate analysis. Embeds the ordered
  /// clip ids, their timeline starts AND their files (with an `agg:` prefix
  /// that no file path uses) so the key self-invalidates whenever the clip set
  /// changes, a clip moves — the merged analysis bakes each clip's
  /// lane-relative offset into its frame placement — or a clip is re-read at
  /// another path: a synced clip whose bytes had not arrived carries an
  /// `asset://` ref that analyzes to nothing, and once a re-read hydrates it
  /// at its pool path a path-blind key kept serving the lane without it.
  String aggregateKeyFor(AudioTrack track) =>
      'agg:${track.id}#${track.clips.map((c) => '${c.id}@${c.startMs}:${c.audioFilePath}').join(',')}';

  /// The subject the inspector analyzes for the current selection: a single
  /// selected clip (analyze that one file), or — when the lane is selected via
  /// its header — the whole take, which is just the one clip for a single-clip
  /// take and the aggregate across all clips for a multi-clip take. Null for no
  /// selection, a backing track, or an empty take. [aggregate] picks which cache
  /// family [key] lives in; [startMs]/[durationMs] are the absolute-timeline
  /// span the subject's analysis covers (the clip span for a clip subject, the
  /// lane span for the aggregate — equal for a single-clip lane, whose one
  /// clip IS the lane span). Carried here so the subject axis can never
  /// disagree with which cache [selectedMetrics]/[selectedSpectrogram] serve.
  ({String key, bool aggregate, int startMs, int durationMs})?
  get _analysisSubject {
    final track = selectedTrack;
    if (track == null || track.isBacking || track.clips.isEmpty) return null;
    final clipId = _selectedClipId();
    if (clipId != null) {
      final clip = track.clipById(clipId);
      if (clip != null) {
        return (
          key: clip.audioFilePath,
          aggregate: false,
          startMs: clip.startMs,
          durationMs: clip.durationMs,
        );
      }
    }
    if (track.clips.length == 1) {
      final clip = track.clips.first;
      return (
        key: clip.audioFilePath,
        aggregate: false,
        startMs: clip.startMs,
        durationMs: clip.durationMs,
      );
    }
    return (
      key: aggregateKeyFor(track),
      aggregate: true,
      startMs: track.startMs,
      durationMs: track.durationMs,
    );
  }

  /// The absolute-timeline span of the current analysis subject (see
  /// [AnalysisSource.analysisStartMs]): whatever [selectedMetrics] /
  /// [selectedSpectrogram] return maps over exactly this span. 0 while there
  /// is no subject; unread in live mode (the live views never map a take).
  @override
  int get analysisStartMs => _analysisSubject?.startMs ?? 0;

  /// The length (ms) of the analysis-subject span; see [analysisStartMs].
  @override
  int get analysisDurationMs => _analysisSubject?.durationMs ?? 0;

  /// The computed spectrogram for the analysis subject, if ready.
  @override
  Spectrogram? get selectedSpectrogram {
    final s = _analysisSubject;
    if (s == null) return null;
    return s.aggregate
        ? _aggAnalysisCache[s.key]?.spectrogram
        : _spectrogramCache[s.key];
  }

  /// The computed voice-timbre metrics (ring/tilt/CPP/formants) for the analysis
  /// subject, if ready. Computed alongside [selectedSpectrogram].
  @override
  VoiceMetrics? get selectedMetrics {
    final s = _analysisSubject;
    if (s == null) return null;
    return s.aggregate
        ? _aggAnalysisCache[s.key]?.metrics
        : _metricsCache[s.key];
  }

  /// The halves of a clip's analysis the dock's tab cannot show without. The
  /// spectrogram views overlay formants when the metrics are there, and the
  /// summary its LTAS, but they show without them.
  ({bool spectrogram, bool metrics}) get _dockNeeds => switch (_dockTab) {
    DockTab.mixer => (spectrogram: false, metrics: false),
    DockTab.spectrogram || DockTab.power => (spectrogram: true, metrics: false),
    DockTab.ltas => (spectrogram: true, metrics: true),
    _ => (spectrogram: false, metrics: true),
  };

  /// Whether the analysis subject lacks a part a view reads ([spectrogram],
  /// [metrics]) that it can still get and nothing is computing it. For a
  /// clip, a half the byte budget dropped: the halves are separate entries,
  /// and nothing asked for them again — the spectrogram views said the take
  /// could not be analyzed. For a lane aggregate, the merge (its parts are
  /// one entry), unless asking is blocked (see [_healBlocked]).
  bool lacksAnalysis({bool spectrogram = false, bool metrics = false}) {
    final s = _analysisSubject;
    if (s == null || s.key == _healBlocked) return false;
    if (s.aggregate) {
      final moving = _selectedTrack()?.id;
      if (moving != null && _isMovingLive(moving)) return false;
      return (spectrogram || metrics) &&
          !_aggLoading.contains(s.key) &&
          !_aggAnalysisCache.containsKey(s.key) &&
          !_aggUnavailable.contains(s.key);
    }
    if (_spectrogramLoading.contains(s.key) ||
        _metricsLoading.contains(s.key)) {
      return false;
    }
    return (spectrogram && !_spectrogramSettled(s.key)) ||
        (metrics && !_metricsSettled(s.key));
  }

  /// [lacksAnalysis] for what the dock's tab shows (see [healSelectedAnalysis],
  /// which the dock asks).
  bool get selectedAnalysisIncomplete => lacksAnalysis(
    spectrogram: _dockNeeds.spectrogram,
    metrics: _dockNeeds.metrics,
  );

  bool _spectrogramSettled(String path) =>
      _spectrogramCache.containsKey(path) ||
      _spectrogramUnavailable.contains(path);

  bool _metricsSettled(String path) =>
      _metricsCache.containsKey(path) || _metricsUnavailable.contains(path);

  /// The lane aggregate a heal merged and could not cache, which is not asked
  /// for again until a merge caches it or a file it folds changes: the
  /// inspector asks on every rebuild, and merged the lane again each time.
  /// A clip needs no such bound: a compute's insert never drops the clip's
  /// other half (see [LruBytesCache.partnerWith]), so a heal leaves what it
  /// asked for.
  String? _healBlocked;

  /// Asks again for what [lacksAnalysis] says the subject lacks: by default
  /// what the dock's tab shows, or the parts named (see [_ensureHalves]).
  Future<void> healSelectedAnalysis({bool? spectrogram, bool? metrics}) async {
    final wantsSpec = spectrogram ?? _dockNeeds.spectrogram;
    final wantsMetrics = metrics ?? _dockNeeds.metrics;
    if (!lacksAnalysis(spectrogram: wantsSpec, metrics: wantsMetrics)) return;
    final s = _analysisSubject!;
    if (s.aggregate) {
      final track = selectedTrack;
      if (track != null) await _ensureAggregateAnalysis(track);
      if (!_disposed && !_aggAnalysisCache.containsKey(s.key)) {
        _healBlocked = s.key;
      }
      return;
    }
    await _ensureHalves(s.key, spectrogram: wantsSpec, metrics: wantsMetrics);
  }

  /// Lifts [_healBlocked] once the aggregate [key] is cached or known not to
  /// exist.
  void _forgiveHeal(String key) {
    if (key != _healBlocked) return;
    if (_aggAnalysisCache.containsKey(key) || _aggUnavailable.contains(key)) {
      _healBlocked = null;
    }
  }

  /// What the dock's tab shows, as it opens on it (see [_ensureHalves]).
  Future<void> _ensureDockAnalysis() async {
    final s = _analysisSubject;
    if (s == null) return;
    if (s.aggregate) return ensureSelectedSpectrogram();
    final needs = _dockNeeds;
    await _ensureHalves(
      s.key,
      spectrogram: needs.spectrogram,
      metrics: needs.metrics,
    );
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
    final m = s.aggregate
        ? _aggAnalysisCache[s.key]?.metrics
        : _metricsCache[s.key];
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
    // ONE lookup for the pair: the memo is keyed by subject alone, so it must
    // never be built from a spectrogram and a metrics half that did not come
    // from the same compute — computeLtas silently drops its voiced-frame gate
    // when the metrics are missing.
    final agg = s.aggregate ? _aggAnalysisCache[s.key] : null;
    final spec = s.aggregate ? agg?.spectrogram : _spectrogramCache[s.key];
    if (spec == null) return null;
    final m = s.aggregate ? agg?.metrics : _metricsCache[s.key];
    final cache = s.aggregate ? _aggLtasCache : _ltasCache;
    return cache[s.key] ??= computeLtas(spec, m);
  }

  /// The register map (per-frame chest↔head) for the analysis subject; memoized.
  @override
  RegisterMap? get selectedRegister {
    final s = _analysisSubject;
    if (s == null) return null;
    final m = s.aggregate
        ? _aggAnalysisCache[s.key]?.metrics
        : _metricsCache[s.key];
    if (m == null) return null;
    final cache = s.aggregate ? _aggRegisterCache : _registerCache;
    return cache[s.key] ??= analyzeRegister(m);
  }

  @override
  bool get spectrogramLoading {
    final s = _analysisSubject;
    if (s == null) return false;
    return s.aggregate
        ? _aggLoading.contains(s.key) || _mergeAwaitsMove(s.key)
        : _spectrogramLoading.contains(s.key);
  }

  @override
  bool get metricsLoading {
    final s = _analysisSubject;
    if (s == null) return false;
    return s.aggregate
        ? _aggLoading.contains(s.key) || _mergeAwaitsMove(s.key)
        : _metricsLoading.contains(s.key);
  }

  /// Whether the selected take's merge [key] is unasked only because one of
  /// its clips is being dragged (see [lacksAnalysis]): each tick re-keys it,
  /// and it is computed once the move lands. Its views wait for it — read as
  /// settled, they said the take could not be analyzed for the whole drag.
  bool _mergeAwaitsMove(String key) {
    final moving = _selectedTrack()?.id;
    return moving != null &&
        _isMovingLive(moving) &&
        !_aggAnalysisCache.containsKey(key) &&
        !_aggUnavailable.contains(key);
  }

  // --- Per-lane analysis ----------------------------------------------------

  /// Voice metrics for an arbitrary lane (its inline sub-lanes read this): the
  /// clip's own analysis for a single-clip lane, or the whole-lane aggregate —
  /// aligned to the lane span, gaps as NaN frames — for a multi-clip lane.
  VoiceMetrics? metricsFor(String trackId) {
    final t = _trackById(trackId);
    if (t == null) return null;
    if (t.clips.length > 1) {
      return _aggAnalysisCache[aggregateKeyFor(t)]?.metrics;
    }
    final clip = t.primaryClip;
    return clip == null ? null : _metricsCache[clip.audioFilePath];
  }

  bool metricsLoadingFor(String trackId) {
    final t = _trackById(trackId);
    if (t == null) return false;
    if (t.clips.length > 1) return _aggLoading.contains(aggregateKeyFor(t));
    final clip = t.primaryClip;
    return clip != null && _metricsLoading.contains(clip.audioFilePath);
  }

  /// Computed spectrogram for an arbitrary lane (its inline sub-lanes read
  /// this); single vs multi-clip resolution mirrors [metricsFor].
  Spectrogram? spectrogramFor(String trackId) {
    final t = _trackById(trackId);
    if (t == null) return null;
    if (t.clips.length > 1) {
      return _aggAnalysisCache[aggregateKeyFor(t)]?.spectrogram;
    }
    final clip = t.primaryClip;
    return clip == null ? null : _spectrogramCache[clip.audioFilePath];
  }

  bool spectrogramLoadingFor(String trackId) {
    final t = _trackById(trackId);
    if (t == null) return false;
    if (t.clips.length > 1) return _aggLoading.contains(aggregateKeyFor(t));
    final clip = t.primaryClip;
    return clip != null && _spectrogramLoading.contains(clip.audioFilePath);
  }

  // --- Inline analysis sub-lanes (Logic-style fold-out) -------------------

  bool isTrackExpanded(String trackId) => _expandedTracks.contains(trackId);
  Set<AnalysisLane> lanesFor(String trackId) =>
      _trackLanes[trackId] ?? const {};

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

  /// Drops the fold-out state of a lane that no longer exists (deleted track,
  /// merged-away take, removed interpretation).
  void dropLaneState(String trackId) {
    _expandedTracks.remove(trackId);
    _trackLanes.remove(trackId);
  }

  /// Recomputes whatever every folded-open lane shows — a clip move changed the
  /// lane-relative offsets aggregate analysis bakes in. Cheap no-ops for
  /// single-clip takes and cached subjects.
  void refreshExpandedLanes() {
    for (final id in _expandedTracks) {
      _ensureLaneData(id);
    }
  }

  /// What the folded-open lanes of [trackId] read: metrics (pitch, loudness,
  /// power) and the spectrogram (spectrogram, power).
  ({bool metrics, bool spectrogram}) _laneNeeds(String trackId) {
    final lanes = _trackLanes[trackId] ?? const {};
    return (
      metrics:
          lanes.contains(AnalysisLane.pitch) ||
          lanes.contains(AnalysisLane.loudness) ||
          lanes.contains(AnalysisLane.power),
      spectrogram:
          lanes.contains(AnalysisLane.spectrogram) ||
          lanes.contains(AnalysisLane.power),
    );
  }

  /// Whether the folded-open lanes of [trackId] lack data they can still get,
  /// with nothing computing it: the byte budget let it go (every lane, the
  /// dock and the inspector share it), or a clip moved and re-keyed the lane.
  /// Lanes are computed only when folded open or a lane is toggled, so they
  /// sat empty, saying the take could not be analyzed, until the user
  /// toggled the lane again. A lane on screen asks [healLane], as the dock
  /// asks [healSelectedAnalysis].
  bool laneLacksData(String trackId) {
    if (!_expandedTracks.contains(trackId) || _isMovingLive(trackId)) {
      return false;
    }
    final t = _trackById(trackId);
    if (t == null) return false;
    final needs = _laneNeeds(trackId);
    if (!needs.metrics && !needs.spectrogram) return false;
    if (t.clips.length > 1) {
      final key = aggregateKeyFor(t);
      return !_aggLoading.contains(key) &&
          !_aggAnalysisCache.containsKey(key) &&
          !_aggUnavailable.contains(key) &&
          !_laneHealBlocked.contains(key);
    }
    final path = t.primaryClip?.audioFilePath;
    if (path == null ||
        _metricsLoading.contains(path) ||
        _spectrogramLoading.contains(path)) {
      return false;
    }
    return (needs.metrics && !_metricsSettled(path)) ||
        (needs.spectrogram && !_spectrogramSettled(path));
  }

  /// Lane aggregates a heal merged and could not cache: not asked for again,
  /// or every rebuild of the lane merged it once more (see [_healBlocked]).
  final Set<String> _laneHealBlocked = {};

  /// When each lane subject (a clip's path, or a lane aggregate's key) was
  /// last asked again by [healLane].
  final Map<String, DateTime> _laneHealedAt = {};

  /// A rebuild of the lanes once a heal the interval refused may run.
  Timer? _laneHealRetry;

  /// How many times in a row [healLane] asked for each subject, each within
  /// [_laneHealQuiet] of the one before, and when it last did; and how many
  /// of those earn a retry of their own (see [healLane]). A run of heals the
  /// quiet ends starts over: counted for the life of the screen, three heals
  /// an hour apart left the next lane the budget let go empty for good.
  final Map<String, ({int count, DateTime at})> _laneHeals = {};
  static const _laneHealRetries = 3;
  static const _laneHealQuiet = Duration(minutes: 2);

  final DateTime Function() _now;

  /// How soon [healLane] asks again for the same subject. More analysis on
  /// screen than the byte budget holds would otherwise have the lanes and the
  /// dock evict each other's in turn, one decode after another, for good.
  static const _laneHealInterval = Duration(seconds: 30);

  /// Asks again for what [laneLacksData] says the lanes of [trackId] lack.
  Future<void> healLane(String trackId) async {
    if (!laneLacksData(trackId)) return;
    final t = _trackById(trackId)!;
    final subject = t.clips.length > 1
        ? aggregateKeyFor(t)
        : t.primaryClip!.audioFilePath;
    final now = _now();
    _laneHealedAt.removeWhere(
      (_, at) => now.difference(at) >= _laneHealInterval,
    );
    _laneHeals.removeWhere((_, h) => now.difference(h.at) >= _laneHealQuiet);
    final last = _laneHealedAt[subject];
    if (last != null) {
      // Asked again once the interval is up: nothing else may rebuild the
      // lane, and it sat empty on an idle screen. Not for a subject already
      // healed a few times: more analysis on screen than the budget holds
      // has every view evict another's in turn, and an idle screen then
      // decoded something every interval for as long as it stayed open.
      if ((_laneHeals[subject]?.count ?? 0) < _laneHealRetries) {
        _laneHealRetry ??= Timer(_laneHealInterval - now.difference(last), () {
          _laneHealRetry = null;
          notifyListeners();
        });
      }
      return;
    }
    _laneHealedAt[subject] = now;
    _laneHeals[subject] = (
      count: (_laneHeals[subject]?.count ?? 0) + 1,
      at: now,
    );
    if (t.clips.length > 1) {
      final key = subject;
      // Only lanes as they are now: a key from an earlier layout can never
      // be asked for again.
      final current = {
        for (final id in _expandedTracks)
          if (_trackById(id) case final lane? when lane.clips.length > 1)
            aggregateKeyFor(lane),
      };
      _laneHealBlocked.retainWhere(current.contains);
      await _ensureAggregateAnalysis(t);
      if (!_disposed &&
          !_aggAnalysisCache.containsKey(key) &&
          !_aggUnavailable.contains(key)) {
        _laneHealBlocked.add(key);
      }
      return;
    }
    final needs = _laneNeeds(trackId);
    await _ensureHalves(
      t.primaryClip!.audioFilePath,
      spectrogram: needs.spectrogram,
      metrics: needs.metrics,
    );
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

  // --- Compute (lazy, isolate-backed) ---------------------------------------

  /// Computes + caches voice metrics for a lane (lazy, in an isolate;
  /// WAV-only): the clip's file for a single-clip lane, or the whole-lane
  /// aggregate for a multi-clip lane. Shares the caches with the Voiceprint
  /// dock.
  Future<void> ensureMetricsFor(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    if (track.clips.length > 1) return _ensureAggregateAnalysis(track);
    final clip = track.primaryClip;
    if (clip != null) {
      await _ensureHalves(
        clip.audioFilePath,
        spectrogram: false,
        metrics: true,
      );
    }
  }

  /// The halves of a clip's analysis a view shows ([spectrogram], [metrics]).
  ///
  /// With neither half held or computing, both come from the one decode the
  /// dock would ask for, with the LTAS and voice profile derived in the
  /// analysis isolate — asked for one at a time (the Pitch lane folded out
  /// first, the dock opened after), the two took two decodes and the dock's
  /// summary built the LTAS and profile on this isolate, ~0.4 s for a
  /// 10-minute take. Otherwise only the halves asked for: the other one may be
  /// a half the byte budget dropped, which a lane that never shows it asked
  /// for again after every clip edit, and which a tab switch computed at the
  /// cost of the half the new tab shows.
  Future<void> _ensureHalves(
    String path, {
    required bool spectrogram,
    required bool metrics,
  }) async {
    final spectrogramAbsent =
        !_spectrogramCache.containsKey(path) &&
        !_spectrogramLoading.contains(path) &&
        !_spectrogramUnavailable.contains(path);
    final metricsAbsent =
        !_metricsCache.containsKey(path) &&
        !_metricsLoading.contains(path) &&
        !_metricsUnavailable.contains(path);
    if (spectrogramAbsent && metricsAbsent) {
      await _ensureClipAnalysis(path);
      return;
    }
    await Future.wait([
      if (spectrogram) _ensureSpectrogramForPath(path),
      if (metrics) _ensureMetricsForPath(path),
    ]);
  }

  /// Computes + caches voice metrics for one audio file (lazy, in an isolate;
  /// WAV-only). Keyed by [path] so a clip's result is shared by the per-clip
  /// inspector, the aggregate merge, and the dock.
  Future<void> _ensureMetricsForPath(String path) async {
    if (_disposed ||
        path.isEmpty ||
        _metricsCache.containsKey(path) ||
        _metricsUnavailable.contains(path)) {
      return;
    }
    // Already computing (possibly as half of a paired clip compute): wait for
    // it. Returning here as if there were nothing pending is what let the
    // aggregate merge see a hole that no one would ever fill.
    if (_metricsInFlight.containsKey(path)) {
      return _joinInFlight(_metricsInFlight, path);
    }
    final done = Completer<void>();
    final generation = _generationOf(path);
    _metricsInFlight[path] = done.future;
    _metricsLoading.add(path);
    notifyListeners();
    var stale = false;
    try {
      final m = await _spectrograms.computeMetricsFromFile(
        path,
        wanted: () => _wanted(path, generation),
      );
      // See [_disposed]: no reader is left for this cache.
      if (_disposed) return;
      // See [_pathGeneration]: evicted mid-compute, so this describes bytes
      // that are already gone.
      if (generation != _generationOf(path)) {
        stale = true;
        return;
      }
      if (m != null) {
        _metricsCache[path] = m;
        // An LTAS read while these were computing was built ungated (see
        // [selectedLtas]); drop it so the next read applies the voiced gate.
        _ltasCache.remove(path);
      } else {
        _metricsUnavailable.add(path);
      }
    } catch (_) {
      // Compute failed; latch so it isn't retried. The lane shows unavailable.
      if (generation == _generationOf(path)) _metricsUnavailable.add(path);
    } finally {
      // Removed in the SAME place the loading flag is, so a failed or disposed
      // compute can never leave a future nobody will complete.
      _metricsLoading.remove(path);
      _forgetInFlight(_metricsInFlight, path);
      done.complete();
      notifyListeners();
      // Redo the discarded work against the current file, now that the
      // in-flight slot is free (a joiner of the dropped compute got nothing)
      // — when the eviction asked for it; see [_recomputeOnStale].
      if (stale && _recomputeOnStale.contains(path)) {
        unawaited(_ensureMetricsForPath(path));
      }
    }
  }

  /// Computes + caches the spectrogram for a lane (lazy); single vs multi-clip
  /// resolution mirrors [ensureMetricsFor].
  Future<void> ensureSpectrogramFor(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    if (track.clips.length > 1) return _ensureAggregateAnalysis(track);
    final clip = track.primaryClip;
    if (clip != null) {
      await _ensureHalves(
        clip.audioFilePath,
        spectrogram: true,
        metrics: false,
      );
    }
  }

  /// Computes + caches the spectrogram for one audio file (lazy). Keyed by
  /// [path], like [_ensureMetricsForPath].
  Future<void> _ensureSpectrogramForPath(String path) async {
    if (_disposed ||
        path.isEmpty ||
        _spectrogramCache.containsKey(path) ||
        _spectrogramUnavailable.contains(path)) {
      return;
    }
    // Join an in-flight compute rather than skipping it — see
    // [_ensureMetricsForPath].
    if (_spectrogramInFlight.containsKey(path)) {
      return _joinInFlight(_spectrogramInFlight, path);
    }
    final done = Completer<void>();
    final generation = _generationOf(path);
    _spectrogramInFlight[path] = done.future;
    _spectrogramLoading.add(path);
    notifyListeners();
    var stale = false;
    try {
      final spec = await _spectrograms.computeFromFile(
        path,
        wanted: () => _wanted(path, generation),
      );
      if (_disposed) return;
      // See [_pathGeneration]: evicted mid-compute.
      if (generation != _generationOf(path)) {
        stale = true;
        return;
      }
      if (spec != null) {
        _spectrogramCache[path] = spec;
      } else {
        _spectrogramUnavailable.add(path);
      }
    } catch (_) {
      // Compute failed; latch so it isn't retried. The lane shows unavailable.
      if (generation == _generationOf(path)) _spectrogramUnavailable.add(path);
    } finally {
      _spectrogramLoading.remove(path);
      _forgetInFlight(_spectrogramInFlight, path);
      done.complete();
      notifyListeners();
      if (stale && _recomputeOnStale.contains(path)) {
        unawaited(_ensureSpectrogramForPath(path));
      }
    }
  }

  /// Computes spectrogram + metrics for one clip's file together. When BOTH
  /// are missing (the common case: a take was just selected), they come from
  /// ONE decode in ONE isolate — the old shape ran two concurrent isolates
  /// that each read and decoded the same file, doubling peak memory for the
  /// whole compute. Falls back to the individual paths when only one side is
  /// missing (its twin stays cached).
  /// Returns the clip's pair so a caller can HOLD it: the per-path caches are
  /// byte-budgeted, so a later clip's insert can evict this one before the
  /// caller reads it back.
  Future<TakeAnalysis> _ensureClipAnalysis(String path) async {
    if (_disposed || path.isEmpty) return (spectrogram: null, metrics: null);
    final needSpec =
        !_spectrogramCache.containsKey(path) &&
        !_spectrogramLoading.contains(path) &&
        !_spectrogramUnavailable.contains(path);
    final needMetrics =
        !_metricsCache.containsKey(path) &&
        !_metricsLoading.contains(path) &&
        !_metricsUnavailable.contains(path);
    if (!needSpec || !needMetrics) {
      await Future.wait([
        _ensureSpectrogramForPath(path),
        _ensureMetricsForPath(path),
      ]);
      return _clipAnalysis(path);
    }
    // ONE compute, registered under BOTH paths' in-flight maps: whichever half
    // a later caller asks for, it joins this work instead of starting its own
    // or giving up.
    final done = Completer<void>();
    final generation = _generationOf(path);
    _spectrogramInFlight[path] = done.future;
    _metricsInFlight[path] = done.future;
    _spectrogramLoading.add(path);
    _metricsLoading.add(path);
    notifyListeners();
    var stale = false;
    try {
      final both = await _spectrograms.computeBothFromFile(
        path,
        wanted: () => _wanted(path, generation),
      );
      if (_disposed) return _clipAnalysis(path);
      // See [_pathGeneration]: evicted mid-compute, so this pair describes
      // bytes the eviction already declared stale.
      if (generation != _generationOf(path)) {
        stale = true;
        return (spectrogram: null, metrics: null);
      }
      final spec = both?.spectrogram;
      final m = both?.metrics;
      if (spec != null) {
        _spectrogramCache[path] = spec;
      } else {
        _spectrogramUnavailable.add(path);
      }
      if (m != null) {
        _metricsCache[path] = m;
        _ltasCache.remove(path); // see _ensureMetricsForPath's identical drop
      } else {
        _metricsUnavailable.add(path);
      }
      // The pair's LTAS and voice profile were derived in the analysis
      // isolate (see TakeAnalysisBundle), so the getters below find them
      // memoized instead of building them on this isolate. Seeded only while
      // every source is still resident — an insert above can evict — so a
      // derived memo never outlives what it was derived from.
      if (both?.ltas case final ltas?
          when _spectrogramCache.containsKey(path) &&
              _metricsCache.containsKey(path)) {
        _ltasCache[path] = ltas;
      }
      if (both?.profile case final profile?
          when _metricsCache.containsKey(path)) {
        _profileCache[path] = profile;
      }
    } catch (_) {
      // Compute failed; latch both so neither is retried (matching the
      // individual paths' latch discipline).
      if (generation == _generationOf(path)) {
        _spectrogramUnavailable.add(path);
        _metricsUnavailable.add(path);
      }
    } finally {
      _spectrogramLoading.remove(path);
      _metricsLoading.remove(path);
      _forgetInFlight(_spectrogramInFlight, path);
      _forgetInFlight(_metricsInFlight, path);
      done.complete();
      notifyListeners();
      if (stale && _recomputeOnStale.contains(path)) {
        unawaited(_ensureClipAnalysis(path));
      }
    }
    return _clipAnalysis(path);
  }

  /// Reads a clip's cached pair, refreshing its recency — a part just handed
  /// to the aggregate merge is the newest, not the next eviction victim.
  TakeAnalysis _clipAnalysis(String path) =>
      (spectrogram: _spectrogramCache[path], metrics: _metricsCache[path]);

  /// The pre-EQ input spectrum column at the playhead for [trackId]'s currently
  /// sounding clip, plus its bin width (Hz) — drives the FX EQ analyzer. Null
  /// when no clip of the take is under the playhead, or its display spectrogram
  /// isn't computed yet (kicking off that compute on the way).
  ({Float32List col, double binHz})? eqInputColumn(String trackId) {
    final track = _trackById(trackId);
    if (track == null) return null;
    final posMs = _position.value.inMilliseconds;
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
    if (_disposed ||
        path.isEmpty ||
        _eqSpectroCache.containsKey(path) ||
        _eqSpectroLoading.contains(path) ||
        _eqSpectroUnavailable.contains(path)) {
      return;
    }
    final generation = _generationOf(path);
    _eqSpectroLoading.add(path);
    try {
      final spec = await _spectrograms.computeFromFile(
        path,
        maxHz: _eqDisplayMaxHz,
        wanted: () => _wanted(path, generation),
      );
      // See [_pathGeneration]. No retry is scheduled here: [eqInputColumn]
      // re-asks on the next frame it is painted, which starts a fresh compute
      // once the loading flag clears below.
      if (_disposed || generation != _generationOf(path)) return;
      if (spec != null) {
        _eqSpectroCache[path] = spec;
      } else {
        _eqSpectroUnavailable.add(path);
      }
    } catch (_) {
      if (generation == _generationOf(path)) _eqSpectroUnavailable.add(path);
    } finally {
      _eqSpectroLoading.remove(path);
      notifyListeners();
    }
  }

  /// Computes the whole-lane aggregate for a multi-clip [track]: analyzes each
  /// clip's file (reusing the per-path caches, so selecting a clip later is
  /// already warm), then merges the per-clip results at their lane-relative
  /// offsets — so the aggregate spans exactly [AudioTrack.startMs]..[endMs],
  /// with gaps between clips as silence. Clips that fail or aren't WAV are
  /// simply skipped; the aggregate is built from whatever analyzed.
  Future<void> _ensureAggregateAnalysis(AudioTrack track) async {
    final key = aggregateKeyFor(track);
    if (_disposed ||
        _aggAnalysisCache.containsKey(key) ||
        _aggLoading.contains(key) ||
        _aggUnavailable.contains(key)) {
      return;
    }
    if (_aggInFlight.containsKey(track.id)) {
      final queued = _aggQueued[track.id];
      if (queued != null) _aggLoading.remove(queued.key);
      final done = queued?.done ?? Completer<void>();
      _aggQueued[track.id] = (key: key, done: done);
      _aggLoading.add(key);
      notifyListeners();
      return done.future;
    }
    _aggInFlight[track.id] = key;
    _aggLoading.add(key);
    notifyListeners();
    try {
      // Sequential on purpose: each clip's analysis holds its file bytes plus
      // a Float64 PCM in an isolate, so N clips in a Future.wait meant N
      // concurrent full-file copies with no upper bound. One at a time keeps
      // the peak at a single clip; the per-path caches make later clips warm
      // for every other consumer either way.
      //
      // Each clip's parts are HELD as the loop produces them. The per-path
      // caches are byte-budgeted, so a later clip's insert can evict an
      // earlier one before the merge below reads it — and a merge treats a
      // missing part as SILENCE, which would bake a lane analysis quietly
      // missing a clip under a key only a clip-set change can invalidate.
      final parts = <String, TakeAnalysis>{};
      for (final c in track.clips) {
        parts[c.audioFilePath] = await _ensureClipAnalysis(c.audioFilePath);
        // Between clips is the only place this loop can be abandoned: every
        // further clip claims an analysis-gate slot, and the merge below
        // allocates a lane-length result, for a cache with no reader left.
        if (_disposed) return;
      }
      // Offsets are relative to the lane's span start (min over ALL clips, even
      // ones whose analysis failed), so the merged frames line up with the lane
      // axis (track.startMs/durationMs) every consumer maps against.
      final laneStartMs = track.startMs;
      final metrics = <({int offsetMs, VoiceMetrics metrics})>[];
      final specs = <({int offsetMs, Spectrogram spectrogram})>[];
      var complete = true;
      for (final c in track.clips) {
        final part = parts[c.audioFilePath];
        // The cached re-read is belt-and-braces now that [_ensureClipAnalysis]
        // JOINS an in-flight compute instead of returning ahead of it: the
        // part is already filled. It still covers a byte-budget eviction
        // between the loop and this merge.
        final m = part?.metrics ?? _metricsCache[c.audioFilePath];
        final spec = part?.spectrogram ?? _spectrogramCache[c.audioFilePath];
        // Fold a clip in only when BOTH analyses are present and non-empty, kept
        // as a pair — an empty/degenerate clip yields spec.frames=1 but
        // metrics.frames=0, which would desync the two merged framings (LTAS
        // gates spectrogram frames by the metrics' frame index).
        if (m != null && spec != null && m.frames > 0 && spec.frames > 0) {
          final offsetMs = c.startMs - laneStartMs;
          metrics.add((offsetMs: offsetMs, metrics: m));
          specs.add((offsetMs: offsetMs, spectrogram: spec));
        } else if (!_spectrogramUnavailable.contains(c.audioFilePath) &&
            !_metricsUnavailable.contains(c.audioFilePath)) {
          // A clip that could not analyze at all is a permanent, honest gap;
          // one merely absent here is a transient miss the merge must not
          // freeze into the lane result.
          complete = false;
        }
      }
      // Off the UI isolate: the merges and the views derived from them are
      // lane-length work (a 10-minute lane's LTAS and profile alone ran
      // ~250 ms in the dock's getters).
      final lane = await _mergeLaneInGate(metrics, specs);
      if (_disposed) return;
      final mergedMetrics = lane.metrics;
      final mergedSpec = lane.spectrogram;
      // A lane whose parts did not all land is not cached: leaving the key
      // absent makes the next ensure recompute against now-warm per-clip
      // caches, where a cached partial would be permanent (the key only
      // changes with the clip set).
      // See [_aggDisowned]: an eviction landed while the clips were being
      // analyzed, so this merge folds bytes that are already stale.
      if (complete && !_aggDisowned.contains(key)) {
        // Stored as ONE entry: both merges describe the same clips at the same
        // offsets, every derived view (LTAS, profile, register) is only sound
        // against that exact pair, and the guard above takes a cached entry to
        // be the whole answer. A null spectrogram beside live metrics is a
        // SETTLED answer, not a miss — mergeSpectrograms refuses lane spans
        // past its cell budget where the metrics merge still succeeds, and the
        // pitch and timbre views remain worth showing on their own.
        if (mergedMetrics != null || mergedSpec != null) {
          _aggAnalysisCache[key] = (
            spectrogram: mergedSpec,
            metrics: mergedMetrics,
          );
          // Seeded only while the pair is held: the eviction hook drops these
          // memos with it, and an entry the byte budget refused at once must
          // not leave them behind.
          if (_aggAnalysisCache.containsKey(key)) {
            if (lane.ltas case final ltas?) _aggLtasCache[key] = ltas;
            if (lane.profile case final profile?) {
              _aggProfileCache[key] = profile;
            }
          }
        } else {
          _aggUnavailable.add(key);
        }
      }
    } catch (_) {
      if (!_aggDisowned.contains(key)) _aggUnavailable.add(key);
    } finally {
      _aggLoading.remove(key);
      _aggInFlight.remove(track.id);
      final disowned = _aggDisowned.remove(key);
      // A queued ask never started: its key leaves with it, and the rerun
      // below answers it for wherever the lane is now.
      final queued = _aggQueued.remove(track.id);
      if (queued != null) {
        _aggLoading.remove(queued.key);
        _aggDisowned.remove(queued.key);
      }
      _forgiveHeal(key);
      notifyListeners();
      // Merge the lane as it is now for a queued ask, and for a disowned
      // merge a view still shows: it left the lane with no result and no
      // spinner, and nothing else asks again (the folded-out lanes compute
      // only on these pushes).
      var rerun = Future<void>.value();
      if (!_disposed &&
          (queued != null || (disowned && _showsLane(track.id)))) {
        final live = _trackById(track.id);
        if (live != null && live.clips.length > 1) {
          rerun = _ensureAggregateAnalysis(live);
        }
      }
      unawaited(
        queued == null ? rerun : rerun.whenComplete(queued.done.complete),
      );
    }
  }

  /// Whether a view still shows [trackId]'s whole-lane analysis: its
  /// folded-out sub-lanes, or the dock and inspector with the lane itself
  /// (not one of its clips) as the subject.
  bool _showsLane(String trackId) =>
      _expandedTracks.contains(trackId) ||
      (selectedTrack?.id == trackId && (_analysisSubject?.aggregate ?? false));

  // --- Eviction / refresh ----------------------------------------------------

  /// Drops the cached spectrogram + voice metrics for an audio [path] whose
  /// file left the model (delete/split/merge) or, with [recompute], whose
  /// CONTENT was replaced under the same path (re-analyze) — only then is a
  /// compute still in flight for it redone against the new bytes.
  void evictAnalysis(String path, {bool recompute = false}) {
    // Disown any compute still in flight for this path: it read the file
    // BEFORE this eviction, so its result (or its "unavailable" latch) must
    // not land on top of the replacement. See [_pathGeneration].
    _bumpGeneration(path);
    if (recompute) {
      _recomputeOnStale.add(path);
    } else {
      _recomputeOnStale.remove(path);
    }
    // remove fires the eviction hook, which drops the derived memos with it.
    _spectrogramCache.remove(path);
    _metricsCache.remove(path);
    // Clear the "unavailable" latches so the replacement file can re-analyze.
    _spectrogramUnavailable.remove(path);
    _metricsUnavailable.remove(path);
    _eqSpectroCache.remove(path);
    _eqSpectroUnavailable.remove(path);
    _evictAggregatesFolding(path);
  }

  /// Drops the whole-lane aggregates that folded [path] in. Aggregate keys
  /// embed the clip files so a changed clip set self-invalidates, but a
  /// file's *content* can change at the same path (re-analyze), so a file
  /// mutation drops the lanes playing it — and ONLY those: clearing every
  /// lane blanked other takes' folded-out lanes, which nothing asked for
  /// again.
  void _evictAggregatesFolding(String path) {
    // Disown the merges in flight over it too (see [_aggDisowned]).
    for (final key in _aggLoading) {
      if (_aggregateFolds(key, path)) _aggDisowned.add(key);
    }
    // remove fires the eviction hook, which drops the derived memos with it.
    for (final key in [..._aggAnalysisCache.keys]) {
      if (_aggregateFolds(key, path)) _aggAnalysisCache.remove(key);
    }
    _aggUnavailable.removeWhere((key) => _aggregateFolds(key, path));
    if (_healBlocked case final key? when _aggregateFolds(key, path)) {
      _healBlocked = null;
    }
  }

  /// Whether the aggregate [key] (see [aggregateKeyFor]) folds the clip file
  /// [path] in: each clip ends `:<path>`, comma-separated.
  static bool _aggregateFolds(String key, String path) =>
      key.endsWith(':$path') || key.contains(':$path,');

  /// After a track's audio file changes (re-analyze/split/merge), refresh any
  /// open Voiceprint dock and inline sub-lanes that point at it.
  void onTrackAudioChanged(String trackId) {
    if (_dockOpen && dockShowsTake) {
      unawaited(ensureSelectedSpectrogram());
    }
    if (_expandedTracks.contains(trackId)) {
      _ensureLaneData(trackId);
    }
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
    final clipId = _selectedClipId();
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

  // --- Dev/demo hooks --------------------------------------------------------

  /// Applies the compile-time demo toggles after the practice loads (see
  /// [AppConfig]): the Live voiceprint layout, a specific dock tab by name,
  /// and folding out every vocal take with all lanes for screenshots.
  void applyDemoHooks(List<AudioTrack> vocalTracks) {
    // Dev hook: show the Live voiceprint layout (without starting the mic).
    if (AppConfig.liveDemo) _liveMonitor = true;
    // Dev hook for screenshots: open a specific dock tab by name.
    if (AppConfig.dockTab.isNotEmpty) {
      for (final t in DockTab.values) {
        if (t.name == AppConfig.dockTab) _dockTab = t;
      }
      // Keep the two dev hooks consistent: a non-live tab can't be live.
      if (!_tabSupportsLive(_dockTab)) _liveMonitor = false;
    }
    // Dev hook for screenshots: fold out every vocal take with all lanes.
    if (AppConfig.expandPitch) {
      for (final t in vocalTracks) {
        _expandedTracks.add(t.id);
        _trackLanes[t.id] = AnalysisLane.values.toSet();
        _ensureLaneData(t.id);
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _captureTimer?.cancel();
    _laneHealRetry?.cancel();
    // Stop the engine's capture if the live view was on; the owner disposes
    // the engine right after, this just closes the pump's source first.
    if (_liveMonitor) _graphEngine()?.stopCapture();
    // The shared budget outlives this screen, so a cache that does not LEAVE
    // it keeps this practice's whole analysis payload resident for the life
    // of the process.
    _spectrogramCache.dispose();
    _metricsCache.dispose();
    _aggAnalysisCache.dispose();
    _eqSpectroCache.dispose();
    // Deliberately no _monitor.stop()/dispose(): the view model owns the
    // MonitorService and disposes it after the controllers; stop()'s
    // notifyListeners must not fire into a half-torn-down widget tree.
    super.dispose();
  }
}

/// A lane's merged analysis plus the LTAS and voice profile the dock derives
/// from it (see [_mergeLane]).
typedef _MergedLane = ({
  VoiceMetrics? metrics,
  Spectrogram? spectrogram,
  Ltas? ltas,
  VoiceProfile? profile,
});

/// Runs [_mergeLane] on a worker isolate, holding one of the app-wide
/// analysis slots like every other heavy compute: its input is every clip's
/// analysis, copied into the isolate. Top-level, so the isolate's closure
/// captures the two lists and nothing of the cache.
Future<_MergedLane> _mergeLaneInGate(
  List<({int offsetMs, VoiceMetrics metrics})> metrics,
  List<({int offsetMs, Spectrogram spectrogram})> specs,
) => analysisGate.run(() => Isolate.run(() => _mergeLane(metrics, specs)));

/// A lane's merged analysis plus the LTAS and voice profile the dock derives
/// from it, exactly as its getters would ([AnalysisCache.selectedLtas] needs
/// the spectrogram, [AnalysisCache.selectedProfile] the metrics). Pure, so it
/// runs on a worker isolate.
_MergedLane _mergeLane(
  List<({int offsetMs, VoiceMetrics metrics})> metrics,
  List<({int offsetMs, Spectrogram spectrogram})> specs,
) {
  final m = mergeVoiceMetrics(metrics);
  final spec = mergeSpectrograms(specs);
  return (
    metrics: m,
    spectrogram: spec,
    ltas: spec == null ? null : computeLtas(spec, m),
    profile: m == null ? null : analyzeVoiceProfile(m),
  );
}
