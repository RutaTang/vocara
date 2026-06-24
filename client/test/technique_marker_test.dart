import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/technique_marker.dart';

TechniqueMarker _marker(String id, int ms) => TechniqueMarker(
  id: id,
  positionMs: ms,
  techniqueId: 'tech-$id',
  techniqueName: 'Twang $id',
  createdAt: DateTime(2026, 6, 20),
  updatedAt: DateTime(2026, 6, 20),
);

void main() {
  group('TechniqueMarker', () {
    test('round-trips through JSON', () {
      final m = _marker('1', 1500);
      final back = TechniqueMarker.fromJson(m.toJson());
      expect(back, m);
      expect(back.positionMs, 1500);
      expect(back.techniqueId, 'tech-1');
      expect(back.techniqueName, 'Twang 1');
    });

    test('copyWith preserves id/createdAt and updates fields', () {
      final m = _marker('1', 1000);
      final moved = m.copyWith(positionMs: 2000, techniqueName: 'Belt');
      expect(moved.id, m.id);
      expect(moved.createdAt, m.createdAt);
      expect(moved.positionMs, 2000);
      expect(moved.techniqueName, 'Belt');
      expect(moved.techniqueId, m.techniqueId);
    });
  });

  group('AudioTrack.techniqueMarkers', () {
    AudioTrack track({List<TechniqueMarker> markers = const []}) => AudioTrack(
      id: 't1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: '/x.wav',
      durationMs: 1000,
      createdAt: DateTime(2026, 6, 20),
      techniqueMarkers: markers,
    );

    test('round-trips markers through JSON', () {
      final t = track(markers: [_marker('1', 500), _marker('2', 1200)]);
      final back = AudioTrack.fromJson(t.toJson());
      expect(back.techniqueMarkers.length, 2);
      expect(back.techniqueMarkers[0].techniqueName, 'Twang 1');
      expect(back.techniqueMarkers[1].positionMs, 1200);
    });

    test('defaults to empty + survives JSON without the key', () {
      expect(track().techniqueMarkers, isEmpty);
      final json = track().toJson()..remove('techniqueMarkers');
      expect(AudioTrack.fromJson(json).techniqueMarkers, isEmpty);
    });

    test('copyWith replaces markers but keeps notes', () {
      final t = track(markers: [_marker('1', 100)]);
      final updated = t.copyWith(techniqueMarkers: [_marker('2', 200)]);
      expect(updated.techniqueMarkers.single.id, '2');
      // copyWith without the arg keeps the existing markers.
      expect(t.copyWith(displayName: 'X').techniqueMarkers.single.id, '1');
    });
  });
}
