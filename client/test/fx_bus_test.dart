import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/song_practice.dart';

void main() {
  group('FxBus', () {
    test('round-trips through JSON with its chain + routing', () {
      final bus = FxBus(
        id: 'bus1',
        name: 'Lead Vox',
        colorValue: 0xFF112233,
        volume: 0.8,
        muted: true,
        outputBusId: 'bus2',
        fx: TrackFx.flat().copyWith(eqOn: true, sendDb: -9),
      );
      final back = FxBus.fromJson(bus.toJson());
      expect(back.id, 'bus1');
      expect(back.name, 'Lead Vox');
      expect(back.colorValue, 0xFF112233);
      expect(back.volume, 0.8);
      expect(back.muted, isTrue);
      expect(back.outputBusId, 'bus2');
      expect(back.fx.eqOn, isTrue);
      expect(back.fx.sendDb, -9);
    });
  });

  group('AudioTrack.busId (output routing)', () {
    test('round-trips; copyWith can set + clear it', () {
      final t = AudioTrack(
        id: 't1',
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        audioFilePath: '/x.wav',
        durationMs: 1000,
        createdAt: DateTime(2026, 6, 19),
        busId: 'bus1',
        pan: -0.5,
      );
      expect(t.busId, 'bus1');
      expect(t.pan, -0.5);
      final back = AudioTrack.fromJson(t.toJson());
      expect(back.busId, 'bus1');
      expect(back.pan, -0.5);
      // Legacy JSON without pan → centered.
      final noPan = Map<String, dynamic>.from(t.toJson())..remove('pan');
      expect(AudioTrack.fromJson(noPan).pan, 0.0);

      // Legacy JSON without busId → null (no migration needed).
      final legacy = Map<String, dynamic>.from(t.toJson())..remove('busId');
      expect(AudioTrack.fromJson(legacy).busId, isNull);

      expect(t.copyWith(busId: 'bus2').busId, 'bus2');
      expect(t.copyWith(clearBus: true).busId, isNull);
    });
  });

  group('Aux sends', () {
    test('TrackSend round-trips on tracks and buses', () {
      final t = AudioTrack(
        id: 't1',
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        audioFilePath: '/x.wav',
        durationMs: 1000,
        createdAt: DateTime(2026, 6, 19),
        sends: const [
          TrackSend(busId: 'b1', levelDb: -12),
          TrackSend(busId: 'b2', levelDb: 0),
        ],
      );
      final back = AudioTrack.fromJson(t.toJson());
      expect(back.sends.length, 2);
      expect(back.sends[0].busId, 'b1');
      expect(back.sends[0].levelDb, -12);
      // Legacy JSON without sends → empty.
      final legacy = Map<String, dynamic>.from(t.toJson())..remove('sends');
      expect(AudioTrack.fromJson(legacy).sends, isEmpty);

      final bus = FxBus(
        id: 'b1',
        name: 'Reverb',
        fx: TrackFx.flat(),
        sends: const [TrackSend(busId: 'b2', levelDb: -6)],
      );
      final backBus = FxBus.fromJson(bus.toJson());
      expect(backBus.sends.single.busId, 'b2');
      expect(backBus.sends.single.levelDb, -6);
    });
  });

  group('SongPractice.fxBuses', () {
    test('round-trips; legacy JSON has no buses', () {
      final p = SongPractice(
        id: 'p1',
        title: 'Song',
        createdAt: DateTime(2026, 6, 19),
        updatedAt: DateTime(2026, 6, 19),
        fxBuses: [
          FxBus(id: 'b1', name: 'Bus 1', fx: TrackFx.flat()),
          FxBus(id: 'b2', name: 'Bus 2', fx: TrackFx.flat(), outputBusId: 'b1'),
        ],
      );
      final back = SongPractice.fromJson(p.toJson());
      expect(back.fxBuses.length, 2);
      expect(back.fxBuses[1].outputBusId, 'b1');
      expect(back.busById('b2')?.name, 'Bus 2');

      final legacy = Map<String, dynamic>.from(p.toJson())..remove('fxBuses');
      expect(SongPractice.fromJson(legacy).fxBuses, isEmpty);
    });
  });
}
