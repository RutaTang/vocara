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

/// Fake engine bindings: records what each `ve_tracks_commit_staged` committed (as
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

  /// Scripted `ve_tracks_commit_staged` results, consumed front-first (empty = 0,
  /// accepted). A non-zero result models the engine REJECTING the topology
  /// and keeping its old graph; only accepted commits land in [commits].
  final List<int> commitResults = [];

  /// Decoded buffers handed back through `ve_free_decoded`.
  int frees = 0;

  /// Commit, publish and live-value calls in call order.
  final List<String> events = [];

  final List<String> _staging = [];

  @override
  double ve_sample_rate(ffi.Pointer<VeEngine> e) => 48000.0;

  /// Every `ve_set_input_channel` mode, in call order.
  final List<int> inputChannels = [];

  @override
  int ve_set_input_channel(ffi.Pointer<VeEngine> e, int mode) {
    inputChannels.add(mode);
    return 0;
  }

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
  int ve_tracks_commit_staged(ffi.Pointer<VeEngine> e) {
    final result = commitResults.isEmpty ? 0 : commitResults.removeAt(0);
    if (result == 0) commits.add(List.of(_staging));
    events.add('commit');
    return result;
  }

  @override
  int ve_tracks_publish(ffi.Pointer<VeEngine> e) {
    events.add('publish');
    return 0;
  }

  /// Every `ve_track_lane` call as (track, leader), in call order.
  final List<(int, int)> lanes = [];

  @override
  int ve_track_lane(ffi.Pointer<VeEngine> e, int track, int leader) {
    lanes.add((track, leader));
    return 0;
  }

  /// Makes `ve_commit_degraded_mask` throw (something failing after a commit).
  bool throwOnDegradedMask = false;

  @override
  int ve_commit_degraded_mask(ffi.Pointer<VeEngine> e) {
    if (throwOnDegradedMask) throw StateError('the read after the commit');
    return degradedMasks.isEmpty ? 0 : degradedMasks.removeAt(0);
  }

  @override
  int ve_set_track_volume(
    ffi.Pointer<VeEngine> e,
    int index,
    double gain,
    int muted,
  ) {
    events.add('volume');
    return 0;
  }

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
  int ve_set_master(ffi.Pointer<VeEngine> e, double gain) {
    events.add('master');
    return 0;
  }

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

  test('the input-channel choice reaches the engine as its mode', () {
    final fake = _FakeBindings();
    final svc = GraphAudioPlaybackService.withBindings(fake);
    addTearDown(svc.dispose);
    svc.setInputChannel(GraphAudioPlaybackService.inputChannel2);
    svc.setInputChannel(GraphAudioPlaybackService.inputChannel1);
    svc.setInputChannel(GraphAudioPlaybackService.inputChannelAuto);
    expect(fake.inputChannels, [2, 1, 0]);
  });

  test('a rebuild sets its live values on the staged graph before it goes '
      'live', () async {
    // A rebuilt graph starts every node at its build defaults. Published at
    // the commit, the engine rendered those defaults (and every smoother ramped
    // away from them) for the blocks the re-push took to land; staged, the
    // graph goes live only once each value is already set.
    final fake = _FakeBindings()..cachedPaths.add('a.wav');
    final svc = GraphAudioPlaybackService.withBindings(
      fake,
      decode: (path, rate) async => (0x1000, 10),
    );
    addTearDown(svc.dispose);

    await svc.sync(const [TrackPlayback(id: 'a', path: 'a.wav')], const []);
    expect(fake.events, ['commit', 'volume', 'master', 'publish']);

    // A rejected topology stages nothing: the publish that follows it anyway
    // (see the next test) is a no-op, and no live value is pushed.
    fake.events.clear();
    fake.commitResults.add(-1);
    await svc.sync(const [
      TrackPlayback(id: 'a', path: 'a.wav', startMs: 10),
    ], const []);
    expect(fake.events, ['commit', 'publish']);
  });

  test('a commit that fails after staging still goes live', () async {
    // Staged but never published, the old graph kept playing while every
    // setter addressed the staged one: mute, faders and FX went dead until the
    // next topology change.
    final fake = _FakeBindings()
      ..cachedPaths.add('a.wav')
      ..throwOnDegradedMask = true;
    final svc = GraphAudioPlaybackService.withBindings(
      fake,
      decode: (path, rate) async => (0x1000, 10),
    );
    addTearDown(svc.dispose);

    await expectLater(
      svc.sync(const [TrackPlayback(id: 'a', path: 'a.wav')], const []),
      throwsStateError,
    );
    expect(fake.events, ['commit', 'publish']);
  });

  group('one chain per lane', () {
    test(
      'a lane\'s later clips play through its first clip\'s chain',
      () async {
        // A chain per clip ran a compressor per clip, each seeing only its own
        // clip: every clip boundary restarted the envelope cold.
        final fake = _FakeBindings();
        final svc = GraphAudioPlaybackService.withBindings(
          fake,
          decode: (path, rate) async => (0x1000, 10),
        );
        addTearDown(svc.dispose);

        await svc.sync(const [
          TrackPlayback(id: 'c1', path: 'a.wav', laneId: 'take'),
          TrackPlayback(id: 'b1', path: 'b.wav', laneId: 'backing'),
          TrackPlayback(id: 'c2', path: 'c.wav', startMs: 5000, laneId: 'take'),
          TrackPlayback(id: 'c3', path: 'd.wav', startMs: 9000, laneId: 'take'),
        ], const []);
        expect(fake.lanes, [(2, 0), (3, 0)]);
      },
    );

    test(
      'a lane whose first clip cannot stage leads with the next one',
      () async {
        final fake = _FakeBindings();
        final svc = GraphAudioPlaybackService.withBindings(
          fake,
          // a.wav decodes to nothing: that clip is left out of the graph.
          decode: (path, rate) async => path == 'a.wav' ? (0, 0) : (0x1000, 10),
        );
        addTearDown(svc.dispose);

        await svc.sync(const [
          TrackPlayback(id: 'c1', path: 'a.wav', laneId: 'take'),
          TrackPlayback(id: 'c2', path: 'b.wav', startMs: 5000, laneId: 'take'),
          TrackPlayback(id: 'c3', path: 'c.wav', startMs: 9000, laneId: 'take'),
        ], const []);
        expect(fake.commits.single, ['b.wav', 'c.wav']);
        expect(fake.lanes, [(1, 0)]);
      },
    );

    test('moving a clip to another lane rebuilds the graph', () async {
      final fake = _FakeBindings();
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (path, rate) async => (0x1000, 10),
      );
      addTearDown(svc.dispose);

      const a = TrackPlayback(id: 'c1', path: 'a.wav', laneId: 'take1');
      await svc.sync(const [
        a,
        TrackPlayback(id: 'c2', path: 'b.wav', laneId: 'take1'),
      ], const []);
      await svc.sync(const [
        a,
        TrackPlayback(id: 'c2', path: 'b.wav', laneId: 'take2'),
      ], const []);
      expect(fake.commits, hasLength(2));
      expect(fake.lanes, [(1, 0)]);
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
