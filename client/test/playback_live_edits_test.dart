// Live mixer edits against the graph engine's sync jobs, over fake bindings.
//
//  * A live edit made while a sync job waits (on the chain, or on an isolate
//    decode) survives that job's push of its older snapshot.
//  * A lane's chain-level settings (pan, FX, sends) reach the engine once per
//    lane, not once per clip: every clip of a lane plays through one chain.

// The binding overrides keep the generated C parameter names (snake_case).
// ignore_for_file: non_constant_identifier_names

import 'dart:async';
import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/ffi/vocara_engine_bindings.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/playback_engine.dart';
import 'package:vocara/domain/models/audio_fx.dart';

/// Keeps each graph's last (gain, muted) per track index: a value set while a
/// graph is staged lands on it, and it replaces the live one at publish.
class _Fake extends Fake implements VocaraEngineBindings {
  final Set<String> cachedPaths = {};
  final List<String> _staging = [];
  Map<int, (double, int)> live = {};
  Map<int, (double, int)> staged = {};
  bool isStaged = false;

  /// The track index of every chain-level setter call (pan, sends, insert
  /// FX), in call order.
  final List<int> chainCalls = [];

  @override
  double ve_sample_rate(ffi.Pointer<VeEngine> e) => 48000.0;
  @override
  int ve_buses_begin(ffi.Pointer<VeEngine> e) => 0;
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
    isStaged = true;
    staged = {};
    return 0;
  }

  @override
  int ve_tracks_publish(ffi.Pointer<VeEngine> e) {
    if (isStaged) {
      live = staged;
      isStaged = false;
    }
    return 0;
  }

  @override
  int ve_track_lane(ffi.Pointer<VeEngine> e, int track, int leader) => 0;
  @override
  int ve_commit_degraded_mask(ffi.Pointer<VeEngine> e) => 0;
  @override
  int ve_set_track_volume(
    ffi.Pointer<VeEngine> e,
    int index,
    double gain,
    int muted,
  ) {
    (isStaged ? staged : live)[index] = (gain, muted);
    return 0;
  }

  @override
  int ve_track_chain(
    ffi.Pointer<VeEngine> e,
    int track,
    ffi.Pointer<ffi.Uint8> tags,
    int n,
  ) => 0;
  @override
  int ve_set_master(ffi.Pointer<VeEngine> e, double gain) => 0;
  @override
  void ve_free_decoded(ffi.Pointer<ffi.Float> ptr, int frames) {}
  @override
  void ve_destroy(ffi.Pointer<VeEngine> e) {}

  /// Every other track setter (pan, sends, each insert's parameters) addresses
  /// the chain: count it by its track index.
  @override
  dynamic noSuchMethod(Invocation i) {
    final name = i.memberName.toString();
    if (name.startsWith('Symbol("ve_set_') && !name.contains('bus')) {
      chainCalls.add(i.positionalArguments[1] as int);
      return 0;
    }
    return super.noSuchMethod(i);
  }
}

void main() {
  group('a live edit made while a sync waits', () {
    test('an A/B backing flip during the page-open decode stays', () async {
      // The mixer's A/B flip is live setVolume calls only; no save and no
      // resync follow it, so a lost flip stayed wrong for the session.
      final fake = _Fake();
      final decodes = <Completer<(int, int)>>[];
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (p, r) {
          final c = Completer<(int, int)>();
          decodes.add(c);
          return c.future;
        },
      );
      addTearDown(svc.dispose);
      final load = svc.sync(const [
        TrackPlayback(id: 'acc', path: 'acc.mp3'),
        TrackPlayback(id: 'ref', path: 'ref.mp3', muted: true),
      ], const []);
      await pumpEventQueue();
      await svc.setVolume('acc', 1.0, muted: true);
      await svc.setVolume('ref', 1.0, muted: false);
      decodes[0].complete((0x1000, 10));
      await pumpEventQueue();
      decodes[1].complete((0x1000, 10));
      await load;
      expect(fake.live, {0: (1.0, 1), 1: (1.0, 0)});
    });

    test('a fader moved during a rebuild keeps its value', () async {
      final fake = _Fake()..cachedPaths.add('take.wav');
      final decode = Completer<(int, int)>();
      final svc = GraphAudioPlaybackService.withBindings(
        fake,
        decode: (path, rate) => decode.future,
      );
      addTearDown(svc.dispose);
      const take = TrackPlayback(id: 't', path: 'take.wav', isVocal: true);
      await svc.sync(const [take], const []);

      final rebuild = svc.sync(const [
        take,
        TrackPlayback(id: 'b', path: 'ref.m4a'),
      ], const []);
      await pumpEventQueue();
      await svc.setVolume('t', 0.2);
      expect(fake.live[0], (0.2, 0), reason: 'heard at once on the old graph');
      decode.complete((0x1000, 10));
      await rebuild;
      expect(fake.live[0], (0.2, 0));
    });

    test(
      'an edit queued behind a rebuild survives the fast path too',
      () async {
        final fake = _Fake()..cachedPaths.add('take.wav');
        final decode = Completer<(int, int)>();
        final svc = GraphAudioPlaybackService.withBindings(
          fake,
          decode: (path, rate) => decode.future,
        );
        addTearDown(svc.dispose);
        const take = TrackPlayback(id: 't', path: 'take.wav');
        const b = TrackPlayback(id: 'b', path: 'ref.m4a');
        final rebuild = svc.sync(const [take, b], const []);
        // Same topology: this one takes the fast path once the rebuild lands.
        final fast = svc.sync(const [take, b], const []);
        await pumpEventQueue();
        await svc.setVolume('t', 0.5, muted: true);
        decode.complete((0x1000, 10));
        await rebuild;
        await fast;
        expect(fake.live[0], (0.5, 1));
      },
    );

    test('a newer sync wins over an edit its model already carries', () async {
      // A failed save reverts the model and resyncs: that snapshot is newer
      // than the edit it reverts, so the edit must not be replayed over it.
      final fake = _Fake()..cachedPaths.add('take.wav');
      final svc = GraphAudioPlaybackService.withBindings(fake);
      addTearDown(svc.dispose);
      const take = TrackPlayback(id: 't', path: 'take.wav');
      await svc.sync(const [take], const []);
      await svc.setVolume('t', 0.2);
      await svc.sync(const [take], const []);
      expect(fake.live[0], (1.0, 0));
    });
  });

  test('a lane whose first clip left the model still hears its FX', () async {
    // The first clip of a take deleted, its save still in flight: the mixer
    // sends the lane's FX and pan to the remaining clips, which the playing
    // graph still runs through the deleted clip's chain. Skipping followers
    // dropped every one of them until the resync committed.
    final fake = _Fake();
    final svc = GraphAudioPlaybackService.withBindings(
      fake,
      decode: (p, r) async => (0x1000, 10),
    );
    addTearDown(svc.dispose);
    final clips = [
      for (var i = 0; i < 3; i++)
        TrackPlayback(
          id: 'c$i',
          path: 'c$i.wav',
          startMs: i * 1000,
          laneId: 'take',
          fx: TrackFx.flat(),
        ),
    ];
    await svc.sync(clips, const []);
    final perSync = fake.chainCalls.length;
    fake.chainCalls.clear();
    final fx = TrackFx.flat().copyWith(eqOn: false);
    for (final c in clips.skip(1)) {
      svc.setTrackFx(c.id, fx);
      svc.setTrackPan(c.id, 0.3);
    }
    expect(fake.chainCalls, hasLength(perSync), reason: 'once, not per clip');
  });

  test('an FX drag on a many-clip lane sets the lane chain once', () async {
    final fake = _Fake();
    final svc = GraphAudioPlaybackService.withBindings(
      fake,
      decode: (p, r) async => (0x1000, 10),
    );
    addTearDown(svc.dispose);
    final clips = [
      for (var i = 0; i < 20; i++)
        TrackPlayback(
          id: 'c$i',
          path: 'c$i.wav',
          startMs: i * 1000,
          laneId: 'take',
          fx: TrackFx.flat(),
        ),
    ];
    await svc.sync(clips, const []);
    final perSync = fake.chainCalls.length;
    expect(fake.chainCalls.toSet(), {0}, reason: 'only the leader is set');

    fake.chainCalls.clear();
    // The mixer pushes the lane's value (one object per tick) to each clip.
    for (var tick = 0; tick < 2; tick++) {
      final fx = TrackFx.flat().copyWith(eqOn: tick.isEven);
      for (final c in clips) {
        svc.setTrackFx(c.id, fx);
        svc.setTrackPan(c.id, 0.3 + tick * 0.1);
      }
    }
    expect(fake.chainCalls.length, 2 * perSync);
  });
}
