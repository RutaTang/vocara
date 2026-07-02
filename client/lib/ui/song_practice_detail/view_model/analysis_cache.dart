import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../../config/app_config.dart';
import '../../../data/services/graph_audio_playback_service.dart';
import '../../../data/services/monitor_service.dart';
import '../../../data/services/spectrogram_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';
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
  }) : _spectrograms = spectrograms,
       _monitor = monitor,
       _graphEngine = graphEngine,
       _pausePlayback = pausePlayback,
       _isPlaying = isPlaying,
       _isRecording = isRecording,
       _selectedTrack = selectedTrack,
       _selectedClipId = selectedClipId,
       _trackById = trackById,
       _position = position,
       _seek = seek;

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

  final ValueListenable<Duration> _position;
  final Future<void> Function(Duration) _seek;

  /// Set in [dispose]. Guards async cache computes resolving after the view
  /// model (and this cache) are gone.
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  // --- Caches ---------------------------------------------------------------

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
    if (dockShowsTake) {
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

  // Pulls captured frames from the engine into the live-analysis monitor.
  // Recording never uses this pump (its capture belongs to the take).
  Timer? _captureTimer;

  /// Starts/stops feeding the engine's live input into the spectrogram analyzer. Can't run while
  /// recording (the capture is busy with the take); pauses playback when starting.
  Future<void> setLiveMonitor(bool on) async {
    if (_liveMonitor == on) return;
    final engine = _graphEngine();
    if (on) {
      if (_isRecording() || engine == null) return;
      if (_isPlaying()) await _pausePlayback();
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
  /// clip ids AND their timeline starts (with an `agg:` prefix that no file
  /// path uses) so the key self-invalidates whenever the clip set changes or a
  /// clip moves — the merged analysis bakes each clip's lane-relative offset
  /// into its frame placement.
  String aggregateKeyFor(AudioTrack track) =>
      'agg:${track.id}#${track.clips.map((c) => '${c.id}@${c.startMs}').join(',')}';

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

  @override
  bool get spectrogramLoading {
    final s = _analysisSubject;
    if (s == null) return false;
    return s.aggregate
        ? _aggLoading.contains(s.key)
        : _spectrogramLoading.contains(s.key);
  }

  // --- Per-lane analysis ----------------------------------------------------

  /// Voice metrics for an arbitrary lane (its inline sub-lanes read this): the
  /// clip's own analysis for a single-clip lane, or the whole-lane aggregate —
  /// aligned to the lane span, gaps as NaN frames — for a multi-clip lane.
  VoiceMetrics? metricsFor(String trackId) {
    final t = _trackById(trackId);
    if (t == null) return null;
    if (t.clips.length > 1) return _aggMetricsCache[aggregateKeyFor(t)];
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
    if (t.clips.length > 1) return _aggSpectrogramCache[aggregateKeyFor(t)];
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
    if (clip != null) await _ensureMetricsForPath(clip.audioFilePath);
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

  /// Computes + caches the spectrogram for a lane (lazy); single vs multi-clip
  /// resolution mirrors [ensureMetricsFor].
  Future<void> ensureSpectrogramFor(String trackId) async {
    final track = _trackById(trackId);
    if (track == null) return;
    if (track.clips.length > 1) return _ensureAggregateAnalysis(track);
    final clip = track.primaryClip;
    if (clip != null) await _ensureSpectrogramForPath(clip.audioFilePath);
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
  /// already warm), then merges the per-clip results at their lane-relative
  /// offsets — so the aggregate spans exactly [AudioTrack.startMs]..[endMs],
  /// with gaps between clips as silence. Clips that fail or aren't WAV are
  /// simply skipped; the aggregate is built from whatever analyzed.
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
      await Future.wait([
        for (final c in track.clips) _ensureClipAnalysis(c.audioFilePath),
      ]);
      // Offsets are relative to the lane's span start (min over ALL clips, even
      // ones whose analysis failed), so the merged frames line up with the lane
      // axis (track.startMs/durationMs) every consumer maps against.
      final laneStartMs = track.startMs;
      final metrics = <({int offsetMs, VoiceMetrics metrics})>[];
      final specs = <({int offsetMs, Spectrogram spectrogram})>[];
      for (final c in track.clips) {
        final m = _metricsCache[c.audioFilePath];
        final spec = _spectrogramCache[c.audioFilePath];
        // Fold a clip in only when BOTH analyses are present and non-empty, kept
        // as a pair — an empty/degenerate clip yields spec.frames=1 but
        // metrics.frames=0, which would desync the two merged framings (the
        // formant overlay then normalizes against the wrong frame count).
        if (m != null && spec != null && m.frames > 0 && spec.frames > 0) {
          final offsetMs = c.startMs - laneStartMs;
          metrics.add((offsetMs: offsetMs, metrics: m));
          specs.add((offsetMs: offsetMs, spectrogram: spec));
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

  // --- Eviction / refresh ----------------------------------------------------

  /// Drops the cached spectrogram + voice metrics for an audio [path] that no
  /// longer exists (split/merge/re-analyze replace a take's file).
  void evictAnalysis(String path) {
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
    // Stop the engine's capture if the live view was on; the owner disposes
    // the engine right after, this just closes the pump's source first.
    if (_liveMonitor) _graphEngine()?.stopCapture();
    // Deliberately no _monitor.stop()/dispose(): the view model owns the
    // MonitorService and disposes it after the controllers; stop()'s
    // notifyListeners must not fire into a half-torn-down widget tree.
    super.dispose();
  }
}
