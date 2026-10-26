import 'dart:math' as math;

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';

AudioTrack _track(
  String id, {
  bool soloed = false,
  String? busId,
  List<TrackSend> sends = const [],
}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: id,
  audioFilePath: '/$id.wav',
  durationMs: 1000,
  createdAt: DateTime(2026),
  soloed: soloed,
  busId: busId,
  sends: sends,
);

FxBus _bus(
  String id, {
  bool soloed = false,
  String? outputBusId,
  List<TrackSend> sends = const [],
}) => FxBus(
  id: id,
  name: id,
  fx: TrackFx.flat(),
  soloed: soloed,
  outputBusId: outputBusId,
  sends: sends,
);

void main() {
  group('resolveSolo', () {
    test('nothing soloed: every source is in solo', () {
      final solo = resolveSolo(
        [_track('t1'), _track('t2', busId: 'b1')],
        [_bus('b1')],
      );
      expect(solo.anySolo, isFalse);
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.trackInSolo('t2'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
    });

    test('soloing a track to master isolates it, no buses audible', () {
      final solo = resolveSolo(
        [_track('t1', soloed: true), _track('t2', busId: 'b1')],
        [_bus('b1')],
      );
      expect(solo.anySolo, isTrue);
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.trackInSolo('t2'), isFalse);
      // t1 goes straight to master, so its bus chain is empty.
      expect(solo.busInSolo('b1'), isFalse);
    });

    test('soloing a track through a bus keeps that bus audible', () {
      final solo = resolveSolo(
        [_track('t1', soloed: true, busId: 'b1'), _track('t2', busId: 'b2')],
        [_bus('b1'), _bus('b2')],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.trackInSolo('t2'), isFalse);
      expect(solo.busInSolo('b2'), isFalse);
    });

    test('a send turned all the way down does not carry a source into a '
        'soloed bus', () {
      // Counted as a route, soloing the bus un-muted the take's dry path,
      // though the send fed the bus nothing.
      final solo = resolveSolo(
        [
          _track(
            't1',
            sends: const [TrackSend(busId: 'b1', levelDb: kSendOffDb)],
          ),
          _track('t2', sends: const [TrackSend(busId: 'b1', levelDb: -12)]),
        ],
        [_bus('b1', soloed: true)],
      );
      expect(solo.trackInSolo('t1'), isFalse);
      expect(solo.trackInSolo('t2'), isTrue);
    });

    test('soloing a bus keeps the takes feeding it audible', () {
      final solo = resolveSolo(
        [
          _track('t1', busId: 'b1'), // feeds the soloed bus
          _track('t2', busId: 'b2'), // unrelated
          _track('t3'), // straight to master
        ],
        [_bus('b1', soloed: true), _bus('b2')],
      );
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.trackInSolo('t1'), isTrue, reason: 't1 feeds the soloed bus');
      expect(solo.trackInSolo('t2'), isFalse);
      expect(solo.trackInSolo('t3'), isFalse);
      expect(solo.busInSolo('b2'), isFalse);
    });

    test('soloing a downstream bus pulls the whole input chain in', () {
      // t1 -> b1 -> b2 -> master; solo b2.
      final solo = resolveSolo(
        [_track('t1', busId: 'b1')],
        [_bus('b1', outputBusId: 'b2'), _bus('b2', soloed: true)],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.busInSolo('b2'), isTrue);
    });

    test('soloing an upstream bus keeps its output chain to master audible', () {
      // t1 -> b1 -> b2 -> master; solo b1. b2 must stay audible to carry b1 out.
      final solo = resolveSolo(
        [_track('t1', busId: 'b1')],
        [_bus('b1', soloed: true, outputBusId: 'b2'), _bus('b2')],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.busInSolo('b2'), isTrue);
    });

    test('soloing an empty bus is silent but still marks the bus in solo', () {
      final solo = resolveSolo(
        [_track('t1', busId: 'b2')],
        [
          _bus('b1', soloed: true), // nothing feeds it
          _bus('b2'),
        ],
      );
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.trackInSolo('t1'), isFalse);
      expect(solo.busInSolo('b2'), isFalse);
    });

    test('a routing cycle does not hang and resolves both buses', () {
      // b1 -> b2 -> b1 (cycle); t1 -> b1, solo t1.
      final solo = resolveSolo(
        [_track('t1', soloed: true, busId: 'b1')],
        [_bus('b1', outputBusId: 'b2'), _bus('b2', outputBusId: 'b1')],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.busInSolo('b2'), isTrue);
    });

    test('soloing a take keeps its reverb send-return bus audible', () {
      // t1 -> master (dry) + post-fader send to reverb bR -> master. Solo t1:
      // its reverb tail must survive, so bR stays audible.
      final solo = resolveSolo(
        [
          _track('t1', soloed: true, sends: const [TrackSend(busId: 'bR')]),
          _track('t2'),
        ],
        [_bus('bR')],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.busInSolo('bR'), isTrue, reason: "t1's reverb must be heard");
      expect(solo.trackInSolo('t2'), isFalse);
    });

    test('soloing a send-only return bus keeps its feeder take audible', () {
      // bR fed ONLY by t1's send (t1 main -> master). Solo bR: t1 must stay
      // audible so its send actually reaches bR (a muted source zeroes sends).
      final solo = resolveSolo(
        [
          _track('t1', sends: const [TrackSend(busId: 'bR')]),
          _track('t2'),
        ],
        [_bus('bR', soloed: true)],
      );
      expect(solo.busInSolo('bR'), isTrue);
      expect(solo.trackInSolo('t1'), isTrue, reason: 't1 feeds bR via a send');
      expect(solo.trackInSolo('t2'), isFalse);
    });

    test('a non-soloed take that only sends to a soloed bus is dropped', () {
      // t2 sends to bR but bR is not soloed and t1 (soloed) does not feed bR.
      final solo = resolveSolo(
        [
          _track('t1', soloed: true),
          _track('t2', sends: const [TrackSend(busId: 'bR')]),
        ],
        [_bus('bR')],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.trackInSolo('t2'), isFalse);
      expect(solo.busInSolo('bR'), isFalse);
    });

    test('track solo and bus solo combine (union of both isolations)', () {
      // t1 soloed to master; b1 soloed fed by t2.
      final solo = resolveSolo(
        [_track('t1', soloed: true), _track('t2', busId: 'b1'), _track('t3')],
        [_bus('b1', soloed: true)],
      );
      expect(solo.trackInSolo('t1'), isTrue);
      expect(solo.trackInSolo('t2'), isTrue);
      expect(solo.busInSolo('b1'), isTrue);
      expect(solo.trackInSolo('t3'), isFalse);
    });
  });

  group('FxBus.soloed', () {
    test('round-trips through JSON and copyWith toggles it', () {
      final bus = FxBus(
        id: 'b1',
        name: 'Reverb',
        fx: TrackFx.flat(),
        soloed: true,
      );
      expect(FxBus.fromJson(bus.toJson()).soloed, isTrue);
      // Legacy JSON without the field defaults to not-soloed.
      final legacy = Map<String, dynamic>.from(bus.toJson())..remove('soloed');
      expect(FxBus.fromJson(legacy).soloed, isFalse);
      expect(bus.copyWith(soloed: false).soloed, isFalse);
    });
  });

  // Hand-written cases pin the shapes we reasoned about. This one generates
  // routing graphs — including ones a single device cannot build, since a merge
  // can — and checks the two properties the feature actually promises, on every
  // one of them.
  group('resolveSolo invariants over random graphs', () {
    /// Every bus a source reaches, following main outputs and aux sends.
    Set<String> routeOf(
      String? busId,
      List<TrackSend> sends,
      Map<String, FxBus> byId,
    ) {
      final seen = <String>{};
      final queue = <String>[];
      void push(String? id) {
        if (id != null && byId.containsKey(id) && seen.add(id)) queue.add(id);
      }

      push(busId);
      for (final s in sends) {
        push(s.busId);
      }
      while (queue.isNotEmpty) {
        final b = byId[queue.removeLast()]!;
        push(b.outputBusId);
        for (final s in b.sends) {
          push(s.busId);
        }
      }
      return seen;
    }

    for (var seed = 0; seed < 200; seed++) {
      test('seed $seed', () {
        final rnd = math.Random(seed);
        final busCount = 1 + rnd.nextInt(4);
        final ids = [for (var i = 0; i < busCount; i++) 'b$i'];
        String? maybeBus() =>
            rnd.nextInt(3) == 0 ? null : ids[rnd.nextInt(busCount)];
        List<TrackSend> someSends() => [
          for (var i = 0; i < rnd.nextInt(3); i++)
            TrackSend(busId: ids[rnd.nextInt(busCount)]),
        ];

        final buses = [
          for (final id in ids)
            _bus(
              id,
              soloed: rnd.nextInt(5) == 0,
              // Deliberately unconstrained: self-routes and cycles included.
              outputBusId: maybeBus(),
              sends: someSends(),
            ),
        ];
        final tracks = [
          for (var i = 0; i < 1 + rnd.nextInt(4); i++)
            _track(
              't$i',
              soloed: rnd.nextInt(4) == 0,
              busId: maybeBus(),
              sends: someSends(),
            ),
        ];

        final byId = {for (final b in buses) b.id: b};
        final r = resolveSolo(tracks, buses);
        final anySolo =
            tracks.any((t) => t.soloed) || buses.any((b) => b.soloed);
        if (!anySolo) return; // the all-audible short circuit

        for (final t in tracks) {
          if (t.soloed) {
            expect(
              r.trackInSolo(t.id),
              isTrue,
              reason: 'a soloed take must be audible',
            );
          }
          if (!r.trackInSolo(t.id)) continue;
          // The promise that makes solo useful: an audible take is actually
          // HEARD, so every bus carrying it to the master stays audible too.
          for (final busId in routeOf(t.busId, t.sends, byId)) {
            expect(
              r.busInSolo(busId),
              isTrue,
              reason:
                  'take ${t.id} is audible but its route through $busId '
                  'is muted, so nothing comes out',
            );
          }
        }
        for (final b in buses) {
          if (b.soloed) {
            expect(
              r.busInSolo(b.id),
              isTrue,
              reason: 'a soloed bus must be audible',
            );
          }
        }
      });
    }
  });
}
