import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/monitor_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/ui/song_practice_detail/view_model/analysis_cache.dart';

class _FakeSpectrograms extends Fake implements SpectrogramService {}

class _FakeMonitor extends Fake implements MonitorService {}

AnalysisCache _cache() => AnalysisCache(
  spectrograms: _FakeSpectrograms(),
  monitor: _FakeMonitor(),
  graphEngine: () => null,
  pausePlayback: () async {},
  isPlaying: () => false,
  isRecording: () => false,
  selectedTrack: () => null,
  selectedClipId: () => null,
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
