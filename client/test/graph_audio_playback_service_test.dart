// The graph engine's sync pipeline policies, driven over fake FFI bindings.
//
// Two contracts pinned here:
//  * A track whose file decode is REFUSED (gone/unreadable/over-cap) is left
//    out of this round's graph; the commit still runs for the rest, and the
//    skipped track restages on the next topology change. Programming errors
//    are not absorbed by that skip.
//  * The degraded-commit mask is a full u64 (bit 63 is the overflow fold), so
//    counting its bits must terminate on a negative Dart reinterpretation.
//  * The one rebuild a degraded commit queues reflects the model as it stands
//    when the retry RUNS: a sync enqueued during the degraded job wins over
//    the degraded job's own captured topology.

// The binding overrides keep the generated C parameter names (snake_case).
// ignore_for_file: non_constant_identifier_names

import 'dart:async';
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

  /// Scripted `ve_tracks_commit` results, consumed front-first (empty = 0,
  /// accepted). A non-zero result models the engine REJECTING the topology
  /// and keeping its old graph; only accepted commits land in [commits].
  final List<int> commitResults = [];

  /// Decoded buffers handed back through `ve_free_decoded`.
  int frees = 0;

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
    final result = commitResults.isEmpty ? 0 : commitResults.removeAt(0);
    if (result == 0) commits.add(List.of(_staging));
    return result;
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
  void ve_free_decoded(ffi.Pointer<ffi.Float> ptr, int frames) => frees++;

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

  test(
    'closing the engine mid-rebuild stops decoding the remaining tracks',
    () async {
      // The sync checked availability only on entry, so after the page closed
      // during the first decode every remaining track was still decoded in
      // full (seconds of CPU, up to gigabytes of PCM) for a null engine.
      final fake = _FakeBindings();
      final decoded = <String>[];
      final pending = <Completer<(int, int)>>[];
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (path, rate) {
          decoded.add(path);
          final c = Completer<(int, int)>();
          pending.add(c);
          return c.future;
        },
      );
      final sync = svc.sync(const [
        TrackPlayback(id: 'a', path: 'a.mp3'),
        TrackPlayback(id: 'b', path: 'b.mp3'),
        TrackPlayback(id: 'c', path: 'c.mp3'),
      ], const []);
      await pumpEventQueue();
      expect(decoded, ['a.mp3']);

      await svc.dispose(); // the user leaves the page mid-load
      pending.single.complete((0x1000, 10));
      await sync;

      expect(decoded, ['a.mp3']);
      expect(fake.frees, 1, reason: 'the decode in flight is still freed');
      expect(fake.commits, isEmpty);
    },
  );

  group('degraded-commit retry', () {
    const take = TrackPlayback(id: 't', path: 'take.wav');
    const backing = TrackPlayback(id: 'b', path: 'backing.wav');

    test('rebuilds from the model current when it runs, not the degraded '
        'job\'s captured one', () async {
      final fake = _FakeBindings();
      fake.cachedPaths.addAll({'take.wav', 'backing.wav'});
      // Only the first commit (the degraded job's) reports a degraded track.
      fake.degradedMasks.add(1);
      final svc = GraphAudioPlaybackService.withBindings(fake);
      addTearDown(svc.dispose);

      // The degraded job stages take + backing; while it is in flight the
      // user deletes the take, so the newer model lands on the chain BETWEEN
      // the degraded job and the retry it queues.
      final degradedJob = svc.sync(const [take, backing], const []);
      final newerSync = svc.sync(const [backing], const []);
      await degradedJob;
      await newerSync;
      // Drain the chain: an identical re-sync serializes after the retry.
      await svc.sync(const [backing], const []);

      // The retry must never re-commit the stale topology over the newer
      // commit — the deleted take would be audible again while the mixer
      // shows it gone.
      expect(fake.commits.first, ['take.wav', 'backing.wav']);
      for (final commit in fake.commits.skip(1)) {
        expect(commit, ['backing.wav']);
      }
    });

    test('a rejected commit in between does not cost a later degraded commit '
        'its retry', () async {
      // The retry used to be gated on a flag the degraded commit set and only
      // a later SUCCESSFUL commit cleared. A rejected commit (the engine kept
      // its old graph) returns early, so with the queued sync and the retry
      // both rejected the flag stayed set, and the next degraded commit was
      // recorded as committed with its track silent for the session.
      final fake = _FakeBindings();
      fake.cachedPaths.addAll({'take.wav', 'backing.wav'});
      const moved = TrackPlayback(id: 't', path: 'take.wav', startMs: 500);
      final svc = GraphAudioPlaybackService.withBindings(fake);
      addTearDown(svc.dispose);

      // The first accepted commit degrades; the sync queued behind it and the
      // retry it queues are both rejected; the next accepted commit — an
      // unrelated edit — degrades too.
      fake.degradedMasks.addAll([1, 1]);
      fake.commitResults.addAll([0, 1, 1]);
      final degradedJob = svc.sync(const [take, backing], const []);
      final rejected = svc.sync(const [backing], const []);
      await degradedJob;
      await rejected;
      await svc.sync(const [moved, backing], const []); // after the retry
      await svc.sync(const [moved, backing], const []); // drain ITS retry

      expect(fake.commits, [
        ['take.wav', 'backing.wav'],
        ['take.wav', 'backing.wav'],
        ['take.wav', 'backing.wav'], // the second degraded commit's retry
      ]);
    });

    test('with no newer sync in between, still re-commits the degraded model '
        'once from a fresh probe', () async {
      final fake = _FakeBindings();
      fake.cachedPaths.addAll({'take.wav', 'backing.wav'});
      fake.degradedMasks.add(1);
      final svc = GraphAudioPlaybackService.withBindings(fake);
      addTearDown(svc.dispose);

      await svc.sync(const [take, backing], const []);
      // Drain the chain (an identical sync serializes after the retry, and
      // fast-paths once the retry's healthy commit records the signature).
      await svc.sync(const [take, backing], const []);

      expect(fake.commits, [
        ['take.wav', 'backing.wav'],
        ['take.wav', 'backing.wav'],
      ]);
    });
  });
}
