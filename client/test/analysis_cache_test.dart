import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/utils/lru_bytes_cache.dart';

class _FakeSpectrograms extends Fake implements SpectrogramService {}

/// Serves canned per-path analysis, so the cache's compute paths run for real
/// (per-clip caches, aggregate merge) without files or isolates.
class _StubSpectrograms extends Fake implements SpectrogramService {
  _StubSpectrograms({required this.metricsByPath, required this.specsByPath});

  final Map<String, VoiceMetrics> metricsByPath;
  final Map<String, Spectrogram> specsByPath;

  @override
  Future<Spectrogram?> computeFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async => specsByPath[path];

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async => metricsByPath[path];

  /// Fired just before each path's pair is returned, so a test can model
  /// something landing mid-loop — a budget eviction of an earlier clip.
  void Function(String path)? onCompute;

  /// How many times the aggregate compute ran, for the pairing tests.
  int bothCalls = 0;

  /// Paths whose compute parks until the test completes them, so an interleaving
  /// (a clip still computing when a second consumer asks for the same path) can
  /// be staged deterministically instead of raced.
  final Map<String, Completer<void>> hold = {};

  @override
  Future<TakeAnalysis?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
  }) async {
    bothCalls++;
    // Snapshot at CALL time, like a real read of the file: a compute parked on
    // [hold] carries the bytes it started with, so a test can replace a path's
    // content mid-flight and have the parked compute still return the old one.
    final result = (
      spectrogram: specsByPath[path],
      metrics: metricsByPath[path],
    );
    await hold[path]?.future;
    onCompute?.call(path);
    return result;
  }
}

/// [_StubSpectrograms] whose per-path METRICS compute parks on [metricsGate]:
/// the window in which a subject's spectrogram is already cached but its
/// metrics are not.
class _GatedMetricsSpectrograms extends _StubSpectrograms {
  _GatedMetricsSpectrograms({
    required super.metricsByPath,
    required super.specsByPath,
  });

  final Completer<void> metricsGate = Completer<void>();

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
  }) async {
    await metricsGate.future;
    return super.computeMetricsFromFile(path, fftSize: fftSize, hop: hop);
  }
}

class _FakeMonitor extends Fake implements MonitorService {
  int starts = 0;
  int stops = 0;

  @override
  void start() => starts++;

  @override
  void stop() => stops++;
}

/// Capture-side fake of the native engine: counts start/stopCapture so the
/// live-monitor interlock tests can assert the pump never armed.
class _FakeGraphEngine extends Fake implements GraphAudioPlaybackService {
  int captureStarts = 0;
  int captureStops = 0;

  @override
  void startCapture() => captureStarts++;

  @override
  void stopCapture() => captureStops++;

  @override
  Float32List pullInputFrames() => Float32List(0);
}

AnalysisCache _cache({
  SpectrogramService? spectrograms,
  AudioTrack? Function()? selectedTrack,
  String? Function()? selectedClipId,
  GraphAudioPlaybackService? graphEngine,
  MonitorService? monitor,
  Future<void> Function()? pausePlayback,
  bool Function()? isPlaying,
  bool Function()? isRecording,
  LruBytesBudget? cacheBudget,
}) => AnalysisCache(
  spectrograms: spectrograms ?? _FakeSpectrograms(),
  monitor: monitor ?? _FakeMonitor(),
  graphEngine: () => graphEngine,
  pausePlayback: pausePlayback ?? () async {},
  isPlaying: isPlaying ?? () => false,
  isRecording: isRecording ?? () => false,
  selectedTrack: selectedTrack ?? () => null,
  selectedClipId: selectedClipId ?? () => null,
  trackById: (_) => null,
  position: ValueNotifier(Duration.zero),
  seek: (_) async {},
  // A private budget by default so tests never evict each other's entries
  // through the app-wide one.
  cacheBudget: cacheBudget ?? LruBytesBudget(1 << 30),
);

AudioTrack _lane(List<AudioClip> clips, {String id = 'lane'}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: 'Lane',
  createdAt: DateTime(2026),
  clips: clips,
);

AudioClip _clip(String id, int startMs) => AudioClip(
  id: id,
  audioFilePath: '$id.wav',
  durationMs: 1000,
  startMs: startMs,
);

// Test framing: hop=100 @ 1 kHz → one frame per 100 ms, so a 1000 ms clip is
// exactly 10 frames and offsets land on whole frames (matches the merge tests).
const int _hop = 100;
const int _sr = 1000;

VoiceMetrics _metrics(int frames) {
  Float32List flat(double v) => Float32List(frames)..fillRange(0, frames, v);
  return VoiceMetrics(
    frames: frames,
    hop: _hop,
    sampleRate: _sr,
    ring: flat(0),
    tilt: flat(0),
    cpp: flat(0),
    loudness: flat(0),
    // Voiced (finite F0) + active so the aggregate merge folds every frame in.
    f0: flat(200),
    f1: flat(double.nan),
    f2: flat(double.nan),
    f3: flat(double.nan),
    h1h2: flat(double.nan),
    active: Uint8List(frames)..fillRange(0, frames, 1),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
  );
}

Spectrogram _spec(int frames, {int bins = 2}) => Spectrogram(
  frames: frames,
  bins: bins,
  maxHz: 5000,
  sampleRate: _sr,
  fftSize: 2048,
  hop: _hop,
  db: Float32List(frames * bins),
  minDb: -60,
  maxDb: 0,
);

void main() {
  test(
    'an LTAS read before the metrics landed is rebuilt once they do',
    () async {
      // The LTAS tab renders as soon as the spectrogram is in (the inline lane
      // may have computed it alone), and computeLtas drops its voiced-frame
      // gate while metrics are missing. That ungated result was memoized and
      // never replaced, so silence frames stayed averaged in for the session.
      final metrics = _metrics(20)..active.fillRange(10, 20, 0); // 10 voiced
      final stub = _GatedMetricsSpectrograms(
        metricsByPath: {'c1.wav': metrics},
        specsByPath: {'c1.wav': _spec(20)},
      );
      final track = _lane([_clip('c1', 0)]);
      final cache = AnalysisCache(
        spectrograms: stub,
        monitor: _FakeMonitor(),
        graphEngine: () => null,
        pausePlayback: () async {},
        isPlaying: () => false,
        isRecording: () => false,
        selectedTrack: () => track,
        selectedClipId: () => null,
        trackById: (id) => id == track.id ? track : null,
        position: ValueNotifier(Duration.zero),
        seek: (_) async {},
        cacheBudget: LruBytesBudget(1 << 30),
      );
      addTearDown(cache.dispose);

      await cache.ensureSpectrogramFor(track.id); // spectrogram only
      final analyzing = cache.ensureSelectedSpectrogram(); // metrics pending
      await Future<void>.delayed(Duration.zero);
      expect(cache.selectedLtas!.frameCount, 20, reason: 'ungated meanwhile');

      stub.metricsGate.complete();
      await analyzing;
      expect(cache.selectedMetrics, isNotNull);
      expect(cache.selectedLtas!.frameCount, 10, reason: 'voiced-gated now');
    },
  );

  group('aggregateKeyFor', () {
    test('embeds the ordered clip ids and their timeline starts', () {
      final cache = _cache();
      final key = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 5000)]),
      );
      expect(key, 'agg:lane#c1@0,c2@5000');
      cache.dispose();
    });

    test('self-invalidates when a clip moves (offsets are baked in)', () {
      final cache = _cache();
      final before = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 5000)]),
      );
      final after = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 6000)]),
      );
      expect(before, isNot(after));
      cache.dispose();
    });

    test('self-invalidates when the clip set or order changes', () {
      final cache = _cache();
      final two = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 5000)]),
      );
      final three = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 5000), _clip('c3', 8000)]),
      );
      final swapped = cache.aggregateKeyFor(
        _lane([_clip('c2', 5000), _clip('c1', 0)]),
      );
      expect(two, isNot(three));
      expect(two, isNot(swapped));
      cache.dispose();
    });

    test('the agg: prefix keeps aggregate keys disjoint from file paths', () {
      final cache = _cache();
      expect(
        cache.aggregateKeyFor(_lane([_clip('c1', 0)])),
        startsWith('agg:'),
      );
      cache.dispose();
    });
  });

  group('analysis subject axis', () {
    test(
      'single-clip lane: the axis is the clip span == the lane span',
      () async {
        final m1 = _metrics(10);
        final track = _lane([_clip('c1', 2000)]);
        final cache = _cache(
          spectrograms: _StubSpectrograms(
            metricsByPath: {'c1.wav': m1},
            specsByPath: {'c1.wav': _spec(10)},
          ),
          selectedTrack: () => track,
        );

        expect(cache.analysisStartMs, 2000);
        expect(cache.analysisDurationMs, 1000);
        // For a single-clip lane the one clip IS the lane span, so the axis is
        // provably unchanged from the old lane-span mapping.
        expect(cache.analysisStartMs, track.startMs);
        expect(cache.analysisDurationMs, track.durationMs);

        await cache.ensureSelectedSpectrogram();
        expect(cache.selectedMetrics, same(m1));
        cache.dispose();
      },
    );

    test('multi-clip lane, no clip selected: the axis is the lane span and '
        'selectedMetrics is the lane-spanning aggregate', () async {
      final m1 = _metrics(10);
      final m2 = _metrics(10);
      final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
      final cache = _cache(
        spectrograms: _StubSpectrograms(
          metricsByPath: {'c1.wav': m1, 'c2.wav': m2},
          specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
        ),
        selectedTrack: () => track,
      );

      expect(cache.analysisStartMs, track.startMs); // 1000
      expect(cache.analysisDurationMs, track.durationMs); // 5000

      await cache.ensureSelectedSpectrogram();
      final merged = cache.selectedMetrics;
      expect(merged, isNotNull);
      expect(identical(merged, m1), isFalse);
      expect(identical(merged, m2), isFalse);
      // The aggregate spans the whole lane (5000 ms → 50 frames at 100 ms),
      // matching the lane-span axis the getters report.
      expect(merged!.frames, 50);
      cache.dispose();
    });

    test('a clip evicted while a LATER clip computes is not baked into the '
        'lane aggregate as silence', () async {
      // The per-clip caches are byte-budgeted, so analyzing clip N can evict
      // clip 1 before the merge reads it back — and the merge treats a
      // missing part as SILENCE, under a key only a clip-set change can
      // invalidate. A budget that fits ONE clip's pair (10 frames: 410 B of
      // metrics + 80 B of spectrogram) makes c2's analysis push c1 out at
      // exactly that point.
      final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
      );
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        cacheBudget: LruBytesBudget(490),
      );

      await cache.ensureSelectedSpectrogram();

      // c1's frames survived the eviction because the loop held them, so the
      // lane still spans both clips (5000 ms -> 50 frames at 100 ms/frame).
      final merged = cache.selectedMetrics;
      expect(merged, isNotNull);
      expect(merged!.frames, 50);
      // And c1's own frames carry real analysis rather than the merge's
      // silence fill.
      expect(merged.active.take(10).where((a) => a != 0), isNotEmpty);
      cache.dispose();
    });

    test('a lane whose clip was already computing still settles, instead of '
        'staying blank until the selection changes', () async {
      // Select a clip (its compute starts), then click the lane header before
      // it lands. The aggregate loop reached that clip mid-compute; skipping
      // ahead of it produced a lane the merge refused to cache — correctly, it
      // was incomplete — with nothing scheduled to ever try again, so the dock
      // sat on "analysis is available for recorded takes" forever.
      final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
      );
      // c1 is the SLOW one: it is still in flight when the aggregate loop
      // asks for it, and only lands after c2 has finished.
      stub.hold['c1.wav'] = Completer<void>();
      String? clipId = 'c1';
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        selectedClipId: () => clipId,
      );

      final clipCompute = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();
      // Lane header clicked: selection widens to the whole lane.
      clipId = null;
      final laneCompute = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();
      expect(cache.selectedMetrics, isNull, reason: 'c1 is still computing');

      stub.hold['c1.wav']!.complete();
      await clipCompute;
      await laneCompute;

      final merged = cache.selectedMetrics;
      expect(merged, isNotNull, reason: 'the lane must not settle blank');
      expect(merged!.frames, 50);
      // The joiner reused c1's work rather than launching a second decode.
      expect(stub.bothCalls, 2);
      cache.dispose();
    });

    test('a clip evicted MID-COMPUTE is re-analyzed, not answered with the '
        'pre-eviction result', () async {
      // Re-analyze exists for a file that changed underneath, and it evicts
      // each clip while the dock may still be computing it. That compute read
      // the OLD bytes: writing its result back leaves the take showing the
      // superseded analysis under a key nothing invalidates again.
      final track = _lane([_clip('c1', 0)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10)},
      );
      stub.hold['c1.wav'] = Completer<void>();
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);

      final first = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();

      // The file is replaced on disk and the take re-analyzed.
      cache.evictAnalysis('c1.wav');
      stub.metricsByPath['c1.wav'] = _metrics(20);
      stub.specsByPath['c1.wav'] = _spec(20);
      stub.hold.remove('c1.wav')!.complete();
      await first;
      await pumpEventQueue();

      expect(
        cache.selectedMetrics?.frames,
        20,
        reason: 'the evicted analysis must not be served as the new one',
      );
      expect(stub.bothCalls, 2, reason: 'the discarded work must be redone');
      cache.dispose();
    });

    test('an eviction mid-compute clears a stale "unavailable" latch instead '
        'of re-latching it over the new file', () async {
      // Same window, failure side: the pre-eviction read found nothing (the
      // file was mid-write), and re-latching after the evict made the take
      // permanently "no analysis" even though the replacement is fine.
      final track = _lane([_clip('c1', 0)]);
      final stub = _StubSpectrograms(metricsByPath: {}, specsByPath: {});
      stub.hold['c1.wav'] = Completer<void>();
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);

      final first = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();

      cache.evictAnalysis('c1.wav');
      stub.metricsByPath['c1.wav'] = _metrics(10);
      stub.specsByPath['c1.wav'] = _spec(10);
      stub.hold.remove('c1.wav')!.complete();
      await first;
      await pumpEventQueue();

      expect(cache.selectedMetricsUnavailable, isFalse);
      expect(cache.selectedMetrics?.frames, 10);
      cache.dispose();
    });

    test('specific clip of a multi-clip lane selected: the axis is that '
        "clip's span and selectedMetrics is the clip-local object", () async {
      final m1 = _metrics(10);
      final m2 = _metrics(10);
      final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
      String? clipId = 'c2';
      final cache = _cache(
        spectrograms: _StubSpectrograms(
          metricsByPath: {'c1.wav': m1, 'c2.wav': m2},
          specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
        ),
        selectedTrack: () => track,
        selectedClipId: () => clipId,
      );

      // Clip-local subject: the axis follows the clip, not the lane.
      expect(cache.analysisStartMs, 5000);
      expect(cache.analysisDurationMs, 1000);
      await cache.ensureSelectedSpectrogram();
      expect(cache.selectedMetrics, same(m2));

      // Deselecting the clip flips both the served object and the axis back
      // to the lane-aggregate subject together.
      clipId = null;
      expect(cache.analysisStartMs, 1000);
      expect(cache.analysisDurationMs, 5000);
      expect(identical(cache.selectedMetrics, m2), isFalse);
      cache.dispose();
    });

    test(
      'a stale clip id falls back to the aggregate subject, axis included',
      () {
        final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
        final cache = _cache(
          selectedTrack: () => track,
          selectedClipId: () => 'gone',
        );
        expect(cache.analysisStartMs, track.startMs);
        expect(cache.analysisDurationMs, track.durationMs);
        cache.dispose();
      },
    );

    test('no subject (nothing selected) reads as a 0/0 axis', () {
      final cache = _cache();
      expect(cache.analysisStartMs, 0);
      expect(cache.analysisDurationMs, 0);
      cache.dispose();
    });
  });

  group('bottom dock chrome', () {
    test('starts closed on the summary tab and toggles open', () {
      final cache = _cache();
      expect(cache.dockOpen, isFalse);
      expect(cache.dockTab, DockTab.analysisSummary);
      cache.toggleDock();
      expect(cache.dockOpen, isTrue);
      cache.toggleDock();
      expect(cache.dockOpen, isFalse);
      cache.dispose();
    });

    test('showDockTab opens the dock on that tab', () {
      final cache = _cache();
      cache.showDockTab(DockTab.pitch);
      expect(cache.dockOpen, isTrue);
      expect(cache.dockTab, DockTab.pitch);
      cache.dispose();
    });

    test('every tab but the mixer analyzes the selected take', () {
      final cache = _cache();
      cache.showDockTab(DockTab.mixer);
      expect(cache.dockShowsTake, isFalse);
      cache.showDockTab(DockTab.spectrogram);
      expect(cache.dockShowsTake, isTrue);
      cache.dispose();
    });

    test('only the spectral/pitch/timbre family supports the live mic', () {
      final cache = _cache();
      const live = {
        DockTab.spectrogram,
        DockTab.power,
        DockTab.ltas,
        DockTab.vowel,
        DockTab.pitch,
        DockTab.vibrato,
        DockTab.timbre,
        DockTab.registers,
      };
      for (final tab in DockTab.values) {
        cache.showDockTab(tab);
        expect(
          cache.activeTabSupportsLive,
          live.contains(tab),
          reason: 'tab $tab',
        );
      }
      cache.dispose();
    });

    test('setDockHeight clamps to the sane resize range', () {
      final cache = _cache();
      cache.setDockHeight(40);
      expect(cache.dockHeight, 160);
      cache.setDockHeight(5000);
      expect(cache.dockHeight, 760);
      cache.setDockHeight(300);
      expect(cache.dockHeight, 300);
      cache.dispose();
    });
  });

  group('live monitor vs recording interlock', () {
    test('a normal live start arms the capture once', () async {
      final engine = _FakeGraphEngine();
      final monitor = _FakeMonitor();
      final cache = _cache(graphEngine: engine, monitor: monitor);

      await cache.setLiveMonitor(true);
      expect(cache.liveMonitor, isTrue);
      expect(engine.captureStarts, 1);
      expect(monitor.starts, 1);

      await cache.setLiveMonitor(false);
      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStops, 1);
      cache.dispose();
    });

    test('a live start parked on the pause await ABORTS when a recording '
        'started meanwhile (the take owns the capture)', () async {
      final engine = _FakeGraphEngine();
      final monitor = _FakeMonitor();
      final pauseParked = Completer<void>();
      var recording = false;
      final cache = _cache(
        graphEngine: engine,
        monitor: monitor,
        isPlaying: () => true,
        pausePlayback: () => pauseParked.future,
        isRecording: () => recording,
      );

      final start = cache.setLiveMonitor(true); // parks on the pause await
      recording = true; // REC arms during the pause
      pauseParked.complete();
      await start;

      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStarts, 0, reason: 'no pump under the take');
      expect(monitor.starts, 0);
      cache.dispose();
    });

    test('setLiveMonitor(false) cancels a live start parked on its pause '
        'await (startRecording\'s force-off must not no-op)', () async {
      final engine = _FakeGraphEngine();
      final monitor = _FakeMonitor();
      final pauseParked = Completer<void>();
      final cache = _cache(
        graphEngine: engine,
        monitor: monitor,
        isPlaying: () => true,
        pausePlayback: () => pauseParked.future,
      );

      final start = cache.setLiveMonitor(true); // parks on the pause await
      // startRecording's interlock call: _liveMonitor is still false here, so
      // pre-fix this returned early and the parked start armed DURING the take.
      await cache.setLiveMonitor(false);
      pauseParked.complete();
      await start;

      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStarts, 0);
      expect(monitor.starts, 0);
      cache.dispose();
    });

    test('a live start cannot begin while recording', () async {
      final engine = _FakeGraphEngine();
      final cache = _cache(graphEngine: engine, isRecording: () => true);
      await cache.setLiveMonitor(true);
      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStarts, 0);
      cache.dispose();
    });
  });

  group('aggregate pair cache', () {
    test('the aggregate is evicted as a PAIR, never leaving a half that '
        'answers the ready latch', () async {
      // A merged spectrogram costs ~20x its metrics per frame, so split
      // budgets always dropped the spectrogram first — and the ready latch
      // keyed off metrics alone, so the surviving half made the guard report
      // "done" and the missing half was never recomputed: no data, no
      // loading state, forever.
      final laneA = _lane([_clip('a1', 0), _clip('a2', 2000)], id: 'A');
      final laneB = _lane([_clip('b1', 0), _clip('b2', 2000)], id: 'B');
      var track = laneA;
      final stub = _StubSpectrograms(
        metricsByPath: {
          for (final p in ['a1.wav', 'a2.wav', 'b1.wav', 'b2.wav'])
            p: _metrics(10),
        },
        specsByPath: {
          for (final p in ['a1.wav', 'a2.wav', 'b1.wav', 'b2.wav'])
            p: _spec(10),
        },
      );
      // Budget for roughly one lane's pair, so caching B evicts A.
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        cacheBudget: LruBytesBudget(4000),
      );

      await cache.ensureSelectedSpectrogram();
      expect(cache.selectedMetrics, isNotNull);
      expect(cache.selectedSpectrogram, isNotNull);

      track = laneB;
      await cache.ensureSelectedSpectrogram();
      // Back to A: whatever eviction did, it did to BOTH halves — so the
      // guard sees a miss and recomputes, rather than serving a half.
      track = laneA;
      await cache.ensureSelectedSpectrogram();
      expect(cache.selectedMetrics, isNotNull);
      expect(
        cache.selectedSpectrogram,
        isNotNull,
        reason: 'a recomputed aggregate is whole',
      );
      cache.dispose();
    });

    test('a refused spectrogram merge still serves the metrics, and is a '
        'settled answer rather than an endless recompute', () async {
      // The spectrogram merge refuses lane spans past its cell budget where
      // the metrics merge succeeds. Half a pair is the honest result there;
      // what must not happen is the compute running again on every read.
      // A lane exactly at the span budget (300000 frames) at the production
      // spectrogram width (214 bins): 300000 x 214 cells is past the cell
      // budget while 300000 frames of metrics curves is not.
      final track = _lane([_clip('c1', 0), _clip('c2', 29999000)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
        specsByPath: {
          'c1.wav': _spec(10, bins: 214),
          'c2.wav': _spec(10, bins: 214),
        },
      );
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);

      await cache.ensureSelectedSpectrogram();
      final callsAfterFirst = stub.bothCalls;
      expect(cache.selectedMetrics, isNotNull, reason: 'metrics still serve');
      expect(cache.selectedSpectrogram, isNull, reason: 'the merge refused it');

      await cache.ensureSelectedSpectrogram();
      expect(
        stub.bothCalls,
        callsAfterFirst,
        reason: 'a settled pair is not recomputed on the next read',
      );
      cache.dispose();
    });
  });

  group('shared analysis budget', () {
    test(
      'derived results never outlive the source entry they came from',
      () async {
        // profile / LTAS / register are memoized under the SAME key as the
        // analysis they derive from, in plain maps. A budget eviction can drop
        // that source on any insert, and the derived value keyed off it then
        // had no owner left to drop it.
        final track = _lane([_clip('c1', 0)]);
        final stub = _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        );
        final cache = _cache(spectrograms: stub, selectedTrack: () => track);

        await cache.ensureSelectedSpectrogram();
        // Materialize the derived memos.
        cache.selectedProfile;
        cache.selectedLtas;
        cache.selectedRegister;
        expect(cache.derivedEntries, greaterThan(0));

        cache.evictAnalysis('c1.wav');

        expect(
          cache.derivedEntries,
          0,
          reason: 'the derived memos go with their source',
        );
        cache.dispose();
      },
    );

    test('a disposed screen stops charging the shared budget', () async {
      // The budget outlives any one screen, so a cache that does not leave it
      // keeps that screen's whole payload resident for the process's life.
      final budget = LruBytesBudget(1 << 30);
      final track = _lane([_clip('c1', 0)]);
      final cache = _cache(
        spectrograms: _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        ),
        selectedTrack: () => track,
        cacheBudget: budget,
      );

      await cache.ensureSelectedSpectrogram();
      expect(budget.bytes, greaterThan(0));

      cache.dispose();

      expect(budget.bytes, 0);
      expect(budget.memberCount, 0);
    });
  });

  group('dispose abandons in-flight analysis', () {
    test('a disposed cache admits no further clip computes', () async {
      // Heavy computes queue through the app-wide analysis gate's two slots,
      // so an aggregate that keeps feeding clips in after its screen closed
      // makes the NEXT screen wait behind results no one will read.
      final track = _lane([
        _clip('c1', 0),
        _clip('c2', 2000),
        _clip('c3', 4000),
      ]);
      late final AnalysisCache cache;
      final stub = _StubSpectrograms(
        metricsByPath: {
          for (final p in ['c1.wav', 'c2.wav', 'c3.wav']) p: _metrics(10),
        },
        specsByPath: {
          for (final p in ['c1.wav', 'c2.wav', 'c3.wav']) p: _spec(10),
        },
      );
      cache = _cache(spectrograms: stub, selectedTrack: () => track);
      // Dispose the moment the FIRST clip's compute is admitted.
      stub.onCompute = (path) {
        if (path == 'c1.wav') cache.dispose();
      };

      await cache.ensureSelectedSpectrogram();

      expect(
        stub.bothCalls,
        1,
        reason: 'clips after the dispose must never be admitted',
      );
    });

    test(
      'a compute already in flight does not write into a disposed cache',
      () async {
        final track = _lane([_clip('c1', 0)]);
        late final AnalysisCache cache;
        final stub = _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        );
        cache = _cache(spectrograms: stub, selectedTrack: () => track);
        stub.onCompute = (_) => cache.dispose();

        await cache.ensureSelectedSpectrogram();

        expect(
          cache.budgetBytes,
          0,
          reason: 'nothing was cached after dispose',
        );
      },
    );
  });
}
