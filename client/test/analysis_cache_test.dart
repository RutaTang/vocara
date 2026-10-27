import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/mic_permission.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';
import 'package:vocara/utils/analysis_gate.dart';
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
    bool Function()? wanted,
  }) async {
    specCalls++;
    onSingle?.call(path);
    return specsByPath[path];
  }

  @override
  Future<VoiceMetrics?> computeMetricsFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    bool Function()? wanted,
  }) async {
    metricsCalls++;
    onSingle?.call(path);
    return metricsByPath[path];
  }

  /// Fired as one half alone is computed, before it lands.
  void Function(String path)? onSingle;

  /// How many times each half was computed on its own.
  int specCalls = 0;
  int metricsCalls = 0;

  /// Fired just before each path's pair is returned, so a test can model
  /// something landing mid-loop — a budget eviction of an earlier clip.
  void Function(String path)? onCompute;

  /// How many times the aggregate compute ran, for the pairing tests.
  int bothCalls = 0;

  /// The predicate the last pair compute was asked with.
  bool Function()? lastWanted;

  /// Paths whose compute parks until the test completes them, so an interleaving
  /// (a clip still computing when a second consumer asks for the same path) can
  /// be staged deterministically instead of raced.
  final Map<String, Completer<void>> hold = {};

  /// Derived views served with a path's pair, standing in for the ones the
  /// real service computes in its isolate; absent = none served (the cache
  /// then derives them itself, as for any pair without them).
  final Map<String, Ltas> ltasByPath = {};
  final Map<String, VoiceProfile> profileByPath = {};

  @override
  Future<TakeAnalysisBundle?> computeBothFromFile(
    String path, {
    int fftSize = 2048,
    int hop = 512,
    double maxHz = 5000,
    bool Function()? wanted,
  }) async {
    bothCalls++;
    lastWanted = wanted;
    // Snapshot at CALL time, like a real read of the file: a compute parked on
    // [hold] carries the bytes it started with, so a test can replace a path's
    // content mid-flight and have the parked compute still return the old one.
    final result = (
      spectrogram: specsByPath[path],
      metrics: metricsByPath[path],
      ltas: ltasByPath[path],
      profile: profileByPath[path],
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
    bool Function()? wanted,
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
  AudioTrack? Function(String)? trackById,
  LruBytesBudget? cacheBudget,
  Future<bool> Function()? requestMicPermission,
  void Function(String)? toast,
  DateTime Function()? now,
}) => AnalysisCache(
  spectrograms: spectrograms ?? _FakeSpectrograms(),
  monitor: monitor ?? _FakeMonitor(),
  graphEngine: () => graphEngine,
  pausePlayback: pausePlayback ?? () async {},
  isPlaying: isPlaying ?? () => false,
  isRecording: isRecording ?? () => false,
  selectedTrack: selectedTrack ?? () => null,
  selectedClipId: selectedClipId ?? () => null,
  trackById: trackById ?? (_) => null,
  position: ValueNotifier(Duration.zero),
  seek: (_) async {},
  toast: toast,
  requestMicPermission: requestMicPermission ?? () async => true,
  // A private budget by default so tests never evict each other's entries
  // through the app-wide one.
  cacheBudget: cacheBudget ?? LruBytesBudget(1 << 30),
  now: now ?? DateTime.now,
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
    fftSize: 2048,
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

/// Lets the event loop run (the lane merge is a real isolate) until [done]
/// holds, bounded; the caller's expectations then say what went wrong.
Future<void> _settled(bool Function() done, {int tries = 400}) async {
  for (var i = 0; i < tries && !done(); i++) {
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

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

      // The spectrogram held alone: the byte budget let the metrics half go.
      await cache.ensureSpectrogramFor(track.id);
      cache.debugDropHalf('c1.wav', metrics: true);
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
    test('embeds the ordered clip ids, their timeline starts and files', () {
      final cache = _cache();
      final key = cache.aggregateKeyFor(
        _lane([_clip('c1', 0), _clip('c2', 5000)]),
      );
      expect(key, 'agg:lane#c1@0:c1.wav,c2@5000:c2.wav');
      cache.dispose();
    });

    test('a clip whose bytes arrive after its lane was analyzed is analyzed '
        'again, not left on the missing-audio answer', () async {
      // A synced clip's row lands before its bytes, so it hydrates with an
      // asset:// ref that analyzes to nothing. The next save or reload re-reads
      // it at its pool path, with the same id and start: a key that ignored the
      // path kept serving the lane without it for the rest of the session.
      final missing = AudioClip(
        id: 'c1',
        audioFilePath: 'asset://a1',
        durationMs: 1000,
        startMs: 0,
      );
      final c2 = _clip('c2', 2000);
      var track = _lane([missing, c2]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
      );
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);
      addTearDown(cache.dispose);

      await cache.ensureSelectedSpectrogram();
      // Only c2 analyzed: c1 is an honest gap while its bytes are missing.
      expect(cache.selectedMetrics!.active.take(10), everyElement(0));

      // The bytes landed and a re-read hydrated the clip at a real path.
      track = _lane([missing.copyWith(audioFilePath: 'c1.wav'), c2]);
      await cache.ensureSelectedSpectrogram();
      expect(
        cache.selectedMetrics!.active.take(10).where((a) => a != 0),
        isNotEmpty,
        reason: 'the clip that arrived is part of the lane analysis now',
      );
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

    test(
      'a lane aggregate comes with the LTAS and profile its pair gives',
      () async {
        // Merged (and derived) on a worker isolate now; the dock's getters must
        // still answer exactly what computing from the merged pair gives.
        final track = _lane([_clip('c1', 1000), _clip('c2', 5000)]);
        final cache = _cache(
          spectrograms: _StubSpectrograms(
            metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
            specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
          ),
          selectedTrack: () => track,
        );
        await cache.ensureSelectedSpectrogram();
        final spec = cache.selectedSpectrogram!;
        final metrics = cache.selectedMetrics!;
        final ltas = cache.selectedLtas!;
        final fresh = computeLtas(spec, metrics);
        expect(ltas.db, fresh.db);
        expect(ltas.alphaRatio, fresh.alphaRatio);
        expect(ltas.singersFormantDb, fresh.singersFormantDb);
        expect(ltas.frameCount, fresh.frameCount);
        final profile = cache.selectedProfile!;
        final freshProfile = analyzeVoiceProfile(metrics);
        expect(
          [
            profile.lowMidi,
            profile.highMidi,
            profile.tessLoMidi,
            profile.tessHiMidi,
          ],
          [
            freshProfile.lowMidi,
            freshProfile.highMidi,
            freshProfile.tessLoMidi,
            freshProfile.tessHiMidi,
          ],
        );
        cache.dispose();
      },
    );

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
      cache.evictAnalysis('c1.wav', recompute: true);
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

      cache.evictAnalysis('c1.wav', recompute: true);
      stub.metricsByPath['c1.wav'] = _metrics(10);
      stub.specsByPath['c1.wav'] = _spec(10);
      stub.hold.remove('c1.wav')!.complete();
      await first;
      await pumpEventQueue();

      expect(cache.selectedMetricsUnavailable, isFalse);
      expect(cache.selectedMetrics?.frames, 10);
      cache.dispose();
    });

    // Queued behind the gate, a pair whose screen closed or whose file changed
    // still ran its full-take isolate for no one.
    test('a queued pair compute stops being wanted once the cache closes or '
        'the file changes', () async {
      final track = _lane([_clip('c1', 0)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10)},
      );
      stub.hold['c1.wav'] = Completer<void>();
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);

      final first = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();
      expect(stub.lastWanted!(), isTrue);
      cache.evictAnalysis('c1.wav');
      expect(stub.lastWanted!(), isFalse, reason: 'its file changed');
      stub.hold.remove('c1.wav')!.complete();
      await first;
      await pumpEventQueue();

      stub.hold['c1.wav'] = Completer<void>();
      final second = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();
      expect(stub.lastWanted!(), isTrue);
      cache.dispose();
      expect(stub.lastWanted!(), isFalse, reason: 'its screen closed');
      stub.hold.remove('c1.wav')!.complete();
      await second;
    });

    test('a clip evicted mid-compute because it LEFT the model (delete, '
        'split, merge) is not analyzed again', () async {
      // Redoing the discarded work is for re-analyze, where the same path now
      // holds new bytes. A removed file's redo just re-decoded it into one of
      // the app-wide analysis slots, for a cache entry nothing reads.
      final track = _lane([_clip('c1', 0)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10)},
      );
      stub.hold['c1.wav'] = Completer<void>();
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);

      final first = cache.ensureSelectedSpectrogram();
      await pumpEventQueue();
      cache.evictAnalysis('c1.wav');
      stub.hold.remove('c1.wav')!.complete();
      await first;
      await pumpEventQueue();

      expect(stub.bothCalls, 1);
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

    test('a refused microphone leaves the capture off and the transport '
        'playing, and says why', () async {
      final engine = _FakeGraphEngine();
      final monitor = _FakeMonitor();
      final toasts = <String>[];
      var pauses = 0;
      final cache = _cache(
        graphEngine: engine,
        monitor: monitor,
        isPlaying: () => true,
        pausePlayback: () async => pauses++,
        requestMicPermission: () async => false,
        toast: toasts.add,
      );

      await cache.setLiveMonitor(true);

      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStarts, 0);
      expect(monitor.starts, 0);
      expect(pauses, 0);
      expect(toasts, [MicPermission.deniedMessage]);
      cache.dispose();
    });

    test('a live start cancelled while the permission prompt is up stays '
        'off', () async {
      final engine = _FakeGraphEngine();
      final answer = Completer<bool>();
      final toasts = <String>[];
      final cache = _cache(
        graphEngine: engine,
        requestMicPermission: () => answer.future,
        toast: toasts.add,
      );

      final start = cache.setLiveMonitor(true);
      await cache.setLiveMonitor(false);
      answer.complete(true);
      await start;

      expect(cache.liveMonitor, isFalse);
      expect(engine.captureStarts, 0);
      expect(toasts, isEmpty);
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

  group('a folded-out lane on screen', () {
    // Lanes compute when folded open or toggled. Data the shared byte budget
    // let go (the dock on other long takes) or a clip move re-keyed left a
    // lane empty, saying the take could not be analyzed, until toggled again.
    final single = _lane([_clip('s1', 0)], id: 'S');
    final multi = _lane([_clip('m1', 0), _clip('m2', 2000)], id: 'M');
    final odd = _lane([_clip('x1', 0)], id: 'X'); // nothing to analyze
    _StubSpectrograms stub() => _StubSpectrograms(
      metricsByPath: {
        for (final p in ['s1.wav', 'm1.wav', 'm2.wav']) p: _metrics(10),
      },
      specsByPath: {
        for (final p in ['s1.wav', 'm1.wav', 'm2.wav']) p: _spec(10),
      },
    );
    AudioTrack? lanes(String id) => {'S': single, 'M': multi, 'X': odd}[id];

    test("asks again for a take's analysis the budget let go", () async {
      final cache = _cache(spectrograms: stub(), trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('S');
      await pumpEventQueue();
      expect(cache.metricsFor('S'), isNotNull);
      expect(cache.laneLacksData('S'), isFalse);

      cache.debugDropHalf('s1.wav', metrics: true);
      expect(cache.laneLacksData('S'), isTrue);
      await cache.healLane('S');
      expect(cache.metricsFor('S'), isNotNull);
      expect(cache.laneLacksData('S'), isFalse);
    });

    test('asks again for a lane merge let go', () async {
      final cache = _cache(spectrograms: stub(), trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('M');
      // The merge runs in an isolate: waited for, not a fixed number of
      // event-loop turns (which a loaded machine outran).
      for (var i = 0; i < 300 && cache.metricsFor('M') == null; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      expect(cache.metricsFor('M'), isNotNull);

      cache.evictAnalysis('m1.wav');
      expect(cache.laneLacksData('M'), isTrue);
      await cache.healLane('M');
      expect(cache.metricsFor('M'), isNotNull);
    });

    test('asks for the same take at most once in a while', () async {
      // More analysis on screen than the budget holds: lanes and the dock
      // would evict each other's in turn, one decode after another.
      final s = stub();
      final cache = _cache(spectrograms: s, trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('S');
      await pumpEventQueue();
      cache.debugDropHalf('s1.wav', metrics: true);
      await cache.healLane('S');
      final computes = s.bothCalls + s.metricsCalls;

      cache.debugDropHalf('s1.wav', metrics: true);
      await cache.healLane('S');
      expect(s.bothCalls + s.metricsCalls, computes);
      expect(cache.metricsFor('S'), isNull);
    });

    testWidgets('a heal the interval refused is asked again once it is up', (
      tester,
    ) async {
      // Nothing else may rebuild the lane: on an idle screen it sat empty.
      final cache = _cache(spectrograms: stub(), trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('S');
      await tester.pump();
      cache.debugDropHalf('s1.wav', metrics: true);
      await cache.healLane('S');
      cache.debugDropHalf('s1.wav', metrics: true);
      await cache.healLane('S'); // refused: asked too recently
      var notified = 0;
      cache.addListener(() => notified++);

      await tester.pump(const Duration(seconds: 29));
      expect(notified, 0);
      await tester.pump(const Duration(seconds: 2));
      expect(notified, 1, reason: 'the lanes rebuild and ask again');
    });

    testWidgets('heals far apart each still earn their retry', (tester) async {
      // Counted for the life of the screen, three heals an hour apart used
      // up the retries: the next lane let go just after a heal sat empty on
      // an idle screen for good.
      var clock = DateTime(2026);
      final cache = _cache(
        spectrograms: stub(),
        trackById: lanes,
        now: () => clock,
      );
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('S');
      await tester.pump();
      for (var i = 0; i < 3; i++) {
        clock = clock.add(const Duration(minutes: 20));
        cache.debugDropHalf('s1.wav', metrics: true);
        await cache.healLane('S');
        expect(cache.metricsFor('S'), isNotNull);
      }
      clock = clock.add(const Duration(seconds: 5));
      cache.debugDropHalf('s1.wav', metrics: true);
      await cache.healLane('S'); // refused: asked too recently
      var notified = 0;
      cache.addListener(() => notified++);

      await tester.pump(const Duration(seconds: 26));
      expect(notified, 1, reason: 'the lanes rebuild and ask again');
    });

    test('does not ask for a take that cannot be analyzed, or one folded '
        'shut', () async {
      final cache = _cache(spectrograms: stub(), trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('X');
      await pumpEventQueue();
      expect(cache.metricsFor('X'), isNull);
      expect(cache.laneLacksData('X'), isFalse, reason: 'settled: none');

      expect(cache.laneLacksData('S'), isFalse, reason: 'never folded open');
    });
  });

  group('aggregate eviction', () {
    final laneA = _lane([_clip('a1', 0), _clip('a2', 2000)], id: 'A');
    final laneC = _lane([_clip('c1', 0), _clip('c2', 2000)], id: 'C');
    _StubSpectrograms stub() => _StubSpectrograms(
      metricsByPath: {
        for (final p in ['a1.wav', 'a2.wav', 'c1.wav', 'c2.wav'])
          p: _metrics(10),
      },
      specsByPath: {
        for (final p in ['a1.wav', 'a2.wav', 'c1.wav', 'c2.wav']) p: _spec(10),
      },
    );
    AudioTrack? lanes(String id) => {'A': laneA, 'C': laneC}[id];

    test('removing one lane\'s file keeps the analysis of a lane that never '
        'played it', () async {
      // Deleting a clip or a take, a split, a merge or a re-analyze evicts the
      // files they touched — and with them EVERY lane aggregate, so another
      // take's folded-out pitch lane went blank (no data, no spinner) and was
      // never computed again until the lane was toggled.
      final cache = _cache(spectrograms: stub(), trackById: lanes);
      addTearDown(cache.dispose);
      await cache.ensureMetricsFor('A');
      await cache.ensureMetricsFor('C');

      cache.evictAnalysis('a1.wav');

      expect(cache.metricsFor('A'), isNull, reason: 'A folded the file in');
      expect(cache.metricsFor('C'), isNotNull, reason: 'C never played it');
    });

    test('a lane merge in flight survives an eviction of a file it does not '
        'fold', () async {
      final s = stub();
      s.hold['c2.wav'] = Completer<void>();
      final cache = _cache(spectrograms: s, trackById: lanes);
      addTearDown(cache.dispose);
      await cache.ensureMetricsFor('A');
      final computing = cache.ensureMetricsFor('C');
      await pumpEventQueue();

      cache.evictAnalysis('a1.wav');
      s.hold.remove('c2.wav')!.complete();
      await computing;

      expect(cache.metricsFor('C'), isNotNull);
    });

    test('a shown lane whose merge was disowned mid-flight is merged again, '
        'not left blank with no spinner', () async {
      // Re-analyze evicts each clip while a folded-out lane may still be
      // merging it: the merge is rightly discarded, but nothing asked for the
      // lane again, so it sat empty until it was toggled.
      final s = stub();
      s.hold['c2.wav'] = Completer<void>();
      final cache = _cache(spectrograms: s, trackById: lanes);
      addTearDown(cache.dispose);
      cache.toggleTrackExpanded('C'); // the pitch lane computes the aggregate
      await pumpEventQueue();

      cache.evictAnalysis('c2.wav', recompute: true);
      s.metricsByPath['c2.wav'] = _metrics(20);
      s.hold.remove('c2.wav')!.complete();
      await _settled(() => cache.metricsFor('C') != null);

      expect(cache.metricsLoadingFor('C'), isFalse);
      final merged = cache.metricsFor('C');
      expect(merged, isNotNull, reason: 'the lane is analyzed again');
      // The re-analyzed c2 (20 frames from 2000 ms) now ends the lane span.
      expect(merged!.frames, 40);
    });
  });

  group('lane merge admission', () {
    _StubSpectrograms stub() => _StubSpectrograms(
      metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
      specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
    );

    test('dragging a clip of a lane selected by its header merges the lane '
        'once more for where it lands, not once per frame', () async {
      // The key embeds the clip starts, so every drag tick is a new key, and
      // the inspector asks for each one: every frame started its own merge,
      // copying all the lane's clip analyses into a fresh isolate.
      AudioTrack at(int ms) => _lane([_clip('c1', 0), _clip('c2', ms)]);
      var track = at(2000);
      final cache = _cache(
        spectrograms: stub(),
        selectedTrack: () => track,
        trackById: (id) => id == track.id ? track : null,
      );
      addTearDown(cache.dispose);

      // A real lane merge outlasts many frames; holding the analysis slots
      // for the drag stands in for that, deterministically.
      final busy = Completer<void>();
      final slots = [
        for (var i = 0; i < analysisGate.limit; i++)
          analysisGate.run(() => busy.future),
      ];
      final ticks = [for (var i = 0; i < 20; i++) at(2000 + 100 * i)];
      final asks = <Future<void>>[];
      for (final t in ticks) {
        track = t;
        asks.add(cache.ensureSelectedSpectrogram());
        await Future<void>.delayed(Duration.zero);
      }
      busy.complete();
      await Future.wait(slots);
      await Future.wait(asks);

      var merged = 0;
      for (final t in ticks) {
        track = t;
        if (cache.selectedMetrics != null) merged++;
      }
      expect(cache.selectedMetrics, isNotNull, reason: 'where it landed');
      expect(merged, lessThanOrEqualTo(2), reason: 'the first + one rerun');
    });

    test('a lane merge waits for an analysis slot like any heavy '
        'compute', () async {
      // Its input is every clip's analysis, copied into the isolate: outside
      // the app-wide gate any number of them ran at once.
      final track = _lane([_clip('c1', 0), _clip('c2', 2000)]);
      final cache = _cache(spectrograms: stub(), selectedTrack: () => track);
      addTearDown(cache.dispose);
      final release = Completer<void>();
      final slots = [
        for (var i = 0; i < analysisGate.limit; i++)
          analysisGate.run(() => release.future),
      ];
      addTearDown(() {
        if (!release.isCompleted) release.complete();
      });

      final merging = cache.ensureSelectedSpectrogram();
      await _settled(() => cache.selectedMetrics != null, tries: 40);
      expect(cache.selectedMetrics, isNull, reason: 'no slot was free');
      expect(cache.spectrogramLoading, isTrue);

      release.complete();
      await Future.wait(slots);
      await merging;
      expect(cache.selectedMetrics, isNotNull);
    });

    test('a lane merge still queued when the screen goes away gives its slot '
        'to the next analysis', () async {
      // Admitted after dispose, it copied every clip's analysis into an
      // isolate for a cache with no reader, ahead of the next screen's work.
      final track = _lane([_clip('c1', 0), _clip('c2', 2000)]);
      final cache = _cache(spectrograms: stub(), selectedTrack: () => track);
      final first = Completer<void>(), second = Completer<void>();
      final slots = [
        analysisGate.run(() => first.future),
        analysisGate.run(() => second.future),
      ];
      addTearDown(() {
        for (final c in [first, second]) {
          if (!c.isCompleted) c.complete();
        }
      });
      final merging = cache.ensureSelectedSpectrogram();
      await _settled(() => analysisGate.queued > 0, tries: 40);
      expect(analysisGate.queued, 1, reason: 'the merge waits for a slot');
      cache.dispose();

      var nextRan = false;
      final next = analysisGate.run(() async => nextRan = true);
      first.complete();
      await pumpEventQueue(times: 10);
      expect(nextRan, isTrue, reason: 'the freed slot went to the next one');
      second.complete();
      await Future.wait([...slots, next, merging]);
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

  group('a clip\'s analysis halves', () {
    test('a folded-out lane computes the pair the dock reads, in one decode '
        'with its LTAS and profile', () async {
      // The Pitch lane (metrics) folded out first and the dock opened after:
      // two decodes, and the dock's summary built the LTAS and profile on the
      // UI isolate (~0.4 s for a 10-minute take).
      final track = _lane([_clip('c1', 0)]);
      final metrics = _metrics(10);
      final spec = _spec(10);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': metrics},
        specsByPath: {'c1.wav': spec},
      );
      final ltas = computeLtas(spec, metrics);
      final profile = analyzeVoiceProfile(metrics);
      stub.ltasByPath['c1.wav'] = ltas;
      stub.profileByPath['c1.wav'] = profile;
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        trackById: (id) => id == track.id ? track : null,
      );
      addTearDown(cache.dispose);

      await cache.ensureMetricsFor(track.id);
      await cache.ensureSelectedSpectrogram();
      expect(stub.bothCalls, 1);
      expect(identical(cache.selectedLtas, ltas), isTrue);
      expect(identical(cache.selectedProfile, profile), isTrue);

      // The spectrogram lane asks for the same pair, and joins it.
      await cache.ensureSpectrogramFor(track.id);
      expect(stub.bothCalls, 1);
    });

    test('each half reads as computing on its own', () async {
      // The metric tabs (pitch, vowel, timbre, vibrato, register) said the
      // take could not be analyzed while its metrics computed beside a held
      // spectrogram; read as one flag, the spectrogram views hid what they
      // had behind a spinner instead.
      final track = _lane([_clip('c1', 0)]);
      final stub = _GatedMetricsSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10)},
      );
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        trackById: (id) => id == track.id ? track : null,
      );
      addTearDown(cache.dispose);
      await cache.ensureSelectedSpectrogram();
      cache.debugDropHalf('c1.wav', metrics: true);
      expect(cache.selectedAnalysisIncomplete, isTrue);

      final analyzing = cache.ensureSelectedSpectrogram();
      await Future<void>.delayed(Duration.zero);
      expect(cache.selectedSpectrogram, isNotNull);
      expect(cache.metricsLoading, isTrue);
      expect(cache.spectrogramLoading, isFalse);
      expect(cache.selectedAnalysisIncomplete, isFalse, reason: 'computing');

      stub.metricsGate.complete();
      await analyzing;
      expect(cache.metricsLoading, isFalse);
      expect(cache.selectedMetrics, isNotNull);
      expect(cache.selectedAnalysisIncomplete, isFalse);
    });

    test(
      'a half the budget dropped reads as incomplete until asked again',
      () async {
        final track = _lane([_clip('c1', 0)]);
        final stub = _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        );
        final cache = _cache(
          spectrograms: stub,
          selectedTrack: () => track,
          trackById: (id) => id == track.id ? track : null,
        );
        addTearDown(cache.dispose);
        await cache.ensureSelectedSpectrogram();
        cache.showDockTab(DockTab.spectrogram);
        expect(cache.selectedAnalysisIncomplete, isFalse);
        cache.debugDropHalf('c1.wav', metrics: false);
        expect(cache.selectedSpectrogram, isNull);
        expect(cache.selectedAnalysisIncomplete, isTrue);
        await cache.ensureSelectedSpectrogram();
        expect(cache.selectedSpectrogram, isNotNull);
        expect(cache.selectedAnalysisIncomplete, isFalse);
      },
    );

    test(
      'a lane asks only for the half it shows once the other is in',
      () async {
        // Asking for the pair, a Pitch lane whose spectrogram the budget let go
        // decoded the take again for a spectrogram it never shows — after every
        // clip edit, which refreshes the folded-out lanes.
        final track = _lane([_clip('c1', 0)]);
        final stub = _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        );
        final cache = _cache(
          spectrograms: stub,
          trackById: (id) => id == track.id ? track : null,
        );
        addTearDown(cache.dispose);
        await cache.ensureMetricsFor(track.id);
        expect(stub.bothCalls, 1, reason: 'neither half held: the pair');

        cache.debugDropHalf('c1.wav', metrics: false);
        await cache.ensureMetricsFor(track.id);
        expect((stub.bothCalls, stub.specCalls, stub.metricsCalls), (1, 0, 0));

        await cache.ensureSpectrogramFor(track.id);
        expect((stub.bothCalls, stub.specCalls, stub.metricsCalls), (1, 1, 0));

        cache.debugDropHalf('c1.wav', metrics: true);
        await cache.ensureSpectrogramFor(track.id);
        expect((stub.bothCalls, stub.specCalls, stub.metricsCalls), (1, 1, 0));
        await cache.ensureMetricsFor(track.id);
        expect((stub.bothCalls, stub.specCalls, stub.metricsCalls), (1, 1, 1));
      },
    );

    test('under a budget that cannot hold a take\'s pair, the pair stays '
        'whole: every view is served by one decode', () async {
      // Each half's insert dropped the other. The dock on the spectrogram and
      // the inspector on the metrics each asked again after the other's
      // compute, decoding the take back and forth; bounding that left one of
      // them blank, and closing the dock, a drag or a re-read pushing the
      // shown half out left it so for good.
      final track = _lane([_clip('c1', 0)]);
      final other = _lane([_clip('c2', 0)], id: 'other');
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
      );
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        trackById: (id) => switch (id) {
          'lane' => track,
          'other' => other,
          _ => null,
        },
        cacheBudget: LruBytesBudget(1), // less than any entry
      );
      addTearDown(cache.dispose);
      int decodes() => stub.bothCalls + stub.specCalls + stub.metricsCalls;
      bool served() =>
          cache.selectedSpectrogram != null && cache.selectedMetrics != null;
      bool anyViewAsks() =>
          cache.lacksAnalysis(metrics: true) ||
          cache.selectedAnalysisIncomplete;

      cache.showDockTab(DockTab.pitch);
      await _settled(() => !cache.spectrogramLoading && !cache.metricsLoading);
      expect(served(), isTrue);
      expect(decodes(), 1);
      for (final tab in [DockTab.spectrogram, DockTab.ltas, DockTab.vowel]) {
        cache.showDockTab(tab);
        expect(anyViewAsks(), isFalse, reason: '$tab');
      }
      cache.setDockOpen(false);
      await cache.ensureSelectedSpectrogram(); // a drag's commit, a re-read
      expect(served(), isTrue);
      expect(decodes(), 1, reason: 'no back and forth');

      // Another take's analysis pushes this one out: asked again, not left
      // blank.
      await cache.ensureMetricsFor('other');
      expect(cache.lacksAnalysis(metrics: true), isTrue);
      await cache.healSelectedAnalysis(spectrogram: false, metrics: true);
      expect(cache.selectedMetrics, isNotNull);
    });

    test(
      'a half pushed out while a heal computes is asked for again',
      () async {
        final track = _lane([_clip('c1', 0)]);
        final stub = _StubSpectrograms(
          metricsByPath: {'c1.wav': _metrics(10)},
          specsByPath: {'c1.wav': _spec(10)},
        );
        final cache = _cache(
          spectrograms: stub,
          selectedTrack: () => track,
          trackById: (id) => id == track.id ? track : null,
        );
        addTearDown(cache.dispose);
        await cache.ensureSelectedSpectrogram();
        cache.showDockTab(DockTab.spectrogram);
        cache.debugDropHalf('c1.wav', metrics: false);
        // Pressure while the heal computes: the metrics go meanwhile.
        stub.onSingle = (_) {
          stub.onSingle = null;
          cache.debugDropHalf('c1.wav', metrics: true);
        };
        await cache.healSelectedAnalysis();
        expect(cache.selectedSpectrogram, isNotNull);
        expect(cache.lacksAnalysis(metrics: true), isTrue);
        await cache.healSelectedAnalysis(spectrogram: false, metrics: true);
        expect(cache.selectedMetrics, isNotNull);
      },
    );

    test('a heal that made the take whole leaves the next one free', () async {
      final track = _lane([_clip('c1', 0)]);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': _metrics(10)},
        specsByPath: {'c1.wav': _spec(10)},
      );
      final cache = _cache(
        spectrograms: stub,
        selectedTrack: () => track,
        trackById: (id) => id == track.id ? track : null,
      );
      addTearDown(cache.dispose);
      await cache.ensureSelectedSpectrogram();
      cache.showDockTab(DockTab.spectrogram);
      for (var i = 1; i <= 2; i++) {
        cache.debugDropHalf('c1.wav', metrics: false);
        expect(cache.selectedAnalysisIncomplete, isTrue, reason: 'drop $i');
        await cache.healSelectedAnalysis();
        expect(cache.selectedSpectrogram, isNotNull, reason: 'heal $i');
      }
      expect(stub.specCalls, 2);
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

    test('a clip pair\'s LTAS and profile are served from the analysis '
        'compute, not rebuilt on the UI isolate', () async {
      final track = _lane([_clip('c1', 0)]);
      final metrics = _metrics(10);
      final spec = _spec(10);
      final stub = _StubSpectrograms(
        metricsByPath: {'c1.wav': metrics},
        specsByPath: {'c1.wav': spec},
      );
      final ltas = computeLtas(spec, metrics);
      final profile = analyzeVoiceProfile(metrics);
      stub.ltasByPath['c1.wav'] = ltas;
      stub.profileByPath['c1.wav'] = profile;
      final cache = _cache(spectrograms: stub, selectedTrack: () => track);
      addTearDown(cache.dispose);

      await cache.ensureSelectedSpectrogram();
      expect(stub.bothCalls, 1);
      expect(identical(cache.selectedLtas, ltas), isTrue);
      expect(identical(cache.selectedProfile, profile), isTrue);
      // ...and they go with their source, like any derived memo.
      cache.evictAnalysis('c1.wav');
      expect(cache.derivedEntries, 0);
    });

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

  test('a lane merge asked for after a re-analyze eviction, while the stale '
      'clip compute is still in flight, is not left blank', () async {
    // The merge joined the superseded compute and read its hole; the rerun
    // started only after, and nothing asked for the lane again.
    final laneC = _lane([_clip('c1', 0), _clip('c2', 2000)], id: 'C');
    final s = _StubSpectrograms(
      metricsByPath: {'c1.wav': _metrics(10), 'c2.wav': _metrics(10)},
      specsByPath: {'c1.wav': _spec(10), 'c2.wav': _spec(10)},
    );
    s.hold['c2.wav'] = Completer<void>();
    final cache = _cache(
      spectrograms: s,
      selectedTrack: () => laneC,
      selectedClipId: () => 'c2',
      trackById: (id) => id == 'C' ? laneC : null,
    );
    addTearDown(cache.dispose);
    // The dock analyzes the selected clip c2 (a long file: still computing).
    unawaited(cache.ensureSelectedSpectrogram());
    await pumpEventQueue();
    // Re-analyze lane C: each clip evicted for recompute.
    cache.evictAnalysis('c1.wav', recompute: true);
    cache.evictAnalysis('c2.wav', recompute: true);
    s.metricsByPath['c2.wav'] = _metrics(20);
    // The lane is folded out (or re-analyze's onTrackAudioChanged asks for it)
    // while c2's superseded compute is still running.
    cache.toggleTrackExpanded('C');
    await pumpEventQueue();
    s.hold.remove('c2.wav')!.complete();
    await _settled(() => cache.metricsFor('C') != null);
    expect(cache.metricsFor('C'), isNotNull);
  });
}
