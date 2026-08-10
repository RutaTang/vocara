// The graph engine's sync pipeline policies, driven over fake FFI bindings.
//
// Two contracts pinned here:
//  * A track whose file decode is REFUSED (gone/unreadable/over-cap) is left
//    out of this round's graph; the commit still runs for the rest, and the
//    skipped track restages on the next topology change. Programming errors
//    are not absorbed by that skip.
//  * The degraded-commit mask is a full u64 (bit 63 is the overflow fold), so
//    counting its bits must terminate on a negative Dart reinterpretation.

// The binding overrides keep the generated C parameter names (snake_case).
// ignore_for_file: non_constant_identifier_names

import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/ffi/vocara_engine_bindings.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/playback_engine.dart';

/// Fake engine bindings: records what each `ve_tracks_commit` committed (as
/// the staged file paths, in staging order) and feeds scripted degraded
/// masks. Anything the sync pipeline calls beyond these stubs throws via
/// [Fake], so an unexpected FFI call fails the test instead of hiding.
class _FakeBindings extends Fake implements VocaraEngineBindings {
  /// Paths `ve_tracks_add` treats as PCM-cache hits (default: all miss).
  final Set<String> cachedPaths = {};

  /// The staged paths of every successful commit, in commit order.
  final List<List<String>> commits = [];

  /// Scripted `ve_commit_degraded_mask` results, consumed front-first
  /// (empty = 0, the healthy answer).
  final List<int> degradedMasks = [];

  final List<String> _staging = [];

  @override
  double ve_sample_rate(ffi.Pointer<VeEngine> e) => 48000.0;

  @override
  int ve_buses_begin(ffi.Pointer<VeEngine> e) => 0;

  @override
  int ve_buses_add(
    ffi.Pointer<VeEngine> e,
    double gain,
    int muted,
    int output,
  ) => 0;

  @override
  int ve_tracks_begin(ffi.Pointer<VeEngine> e) {
    _staging.clear();
    return 0;
  }

  @override
  int ve_tracks_add(
    ffi.Pointer<VeEngine> e,
    ffi.Pointer<ffi.Char> path,
    int start_ms,
    double gain,
    int muted,
    int is_vocal,
    int bus,
  ) {
    final p = path.cast<Utf8>().toDartString();
    if (!cachedPaths.contains(p)) return -1;
    _staging.add(p);
    return _staging.length - 1;
  }

  @override
  int ve_tracks_add_pcm(
    ffi.Pointer<VeEngine> e,
    ffi.Pointer<ffi.Char> path,
    ffi.Pointer<ffi.Float> pcm,
    int frames,
    int start_ms,
    double gain,
    int muted,
    int is_vocal,
    int bus,
  ) {
    _staging.add(path.cast<Utf8>().toDartString());
    return _staging.length - 1;
  }

  @override
  int ve_tracks_commit(ffi.Pointer<VeEngine> e) {
    commits.add(List.of(_staging));
    return 0;
  }

  @override
  int ve_commit_degraded_mask(ffi.Pointer<VeEngine> e) =>
      degradedMasks.isEmpty ? 0 : degradedMasks.removeAt(0);

  @override
  int ve_set_track_volume(
    ffi.Pointer<VeEngine> e,
    int index,
    double gain,
    int muted,
  ) => 0;

  @override
  int ve_set_bus_volume(
    ffi.Pointer<VeEngine> e,
    int bus,
    double gain,
    int muted,
  ) => 0;

  @override
  int ve_set_pan(ffi.Pointer<VeEngine> e, int index, double pan) => 0;

  @override
  int ve_set_bus_pan(ffi.Pointer<VeEngine> e, int bus, double pan) => 0;

  @override
  int ve_set_master(ffi.Pointer<VeEngine> e, double gain) => 0;

  @override
  void ve_free_decoded(ffi.Pointer<ffi.Float> ptr, int frames) {}

  @override
  void ve_destroy(ffi.Pointer<VeEngine> e) {}
}

void main() {
  group('bitCount', () {
    test('counts ordinary masks', () {
      expect(GraphAudioPlaybackService.bitCount(0), 0);
      expect(GraphAudioPlaybackService.bitCount(1), 1);
      expect(GraphAudioPlaybackService.bitCount(0x2d), 4);
    });

    test('terminates on a mask with bit 63 (the degraded overflow fold)', () {
      // ve_commit_degraded_mask is a u64; the Rust side folds staged indices
      // >= 63 into bit 63, which arrives through the Uint64 binding as a
      // NEGATIVE Dart int. An arithmetic shift would loop forever on it.
      expect(GraphAudioPlaybackService.bitCount(1 << 63), 1);
      expect(GraphAudioPlaybackService.bitCount((1 << 63) | 0x3), 3);
      expect(GraphAudioPlaybackService.bitCount(-1), 64);
    });
  });

  group('per-track staging skip', () {
    test('a refused decode skips just that track; the commit proceeds and the '
        'track restages on the next topology change', () async {
      final fake = _FakeBindings();
      var goneReadable = false;
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (path, rate) async {
          if (path == 'gone.wav' && !goneReadable) {
            throw const MediaUnsupportedException(
              'The file could not be read.',
            );
          }
          if (path == 'long.wav') {
            throw const MediaTooLongException(Duration(minutes: 60));
          }
          return (0x1000, 100);
        },
      );
      addTearDown(svc.dispose);

      const gone = TrackPlayback(id: 'g', path: 'gone.wav');
      const long = TrackPlayback(id: 'l', path: 'long.wav');
      const good = TrackPlayback(id: 'ok', path: 'good.wav');

      // Both refused tracks are left out; the healthy one still commits.
      await svc.sync(const [gone, long, good], const []);
      expect(fake.commits, [
        ['good.wav'],
      ]);

      // The signature was recorded despite the skips: an identical re-sync
      // takes the live-params fast path instead of rebuilding (which would
      // re-run the dead file's decode on every fader move).
      await svc.sync(const [gone, long, good], const []);
      expect(fake.commits, hasLength(1));

      // The file becomes readable again: the next TOPOLOGY change (a moved
      // clip) restages the previously skipped track, as promised.
      goneReadable = true;
      const moved = TrackPlayback(id: 'ok', path: 'good.wav', startMs: 500);
      await svc.sync(const [gone, long, moved], const []);
      expect(fake.commits, hasLength(2));
      expect(fake.commits.last, ['gone.wav', 'good.wav']);
    });

    test('a programming error during staging still aborts the sync', () async {
      final fake = _FakeBindings();
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (path, rate) async => throw StateError('bug in staging'),
      );
      addTearDown(svc.dispose);

      const t = TrackPlayback(id: 't', path: 't.wav');
      await expectLater(svc.sync(const [t], const []), throwsStateError);
      expect(fake.commits, isEmpty);
    });
  });
}
