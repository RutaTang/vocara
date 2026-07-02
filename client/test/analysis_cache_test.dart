import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';

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
}

class _FakeMonitor extends Fake implements MonitorService {}

AnalysisCache _cache({
  SpectrogramService? spectrograms,
  AudioTrack? Function()? selectedTrack,
  String? Function()? selectedClipId,
}) => AnalysisCache(
  spectrograms: spectrograms ?? _FakeSpectrograms(),
  monitor: _FakeMonitor(),
  graphEngine: () => null,
  pausePlayback: () async {},
  isPlaying: () => false,
  isRecording: () => false,
  selectedTrack: selectedTrack ?? () => null,
  selectedClipId: selectedClipId ?? () => null,
  trackById: (_) => null,
  position: ValueNotifier(Duration.zero),
  seek: (_) async {},
);

AudioTrack _lane(List<AudioClip> clips) => AudioTrack(
  id: 'lane',
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

Spectrogram _spec(int frames) => Spectrogram(
  frames: frames,
  bins: 2,
  maxHz: 5000,
  sampleRate: _sr,
  fftSize: 2048,
  hop: _hop,
  db: Float32List(frames * 2),
  minDb: -60,
  maxDb: 0,
);

void main() {
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
}
