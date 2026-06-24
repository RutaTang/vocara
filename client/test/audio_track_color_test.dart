import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';

AudioTrack _track({int? colorValue}) => AudioTrack(
      id: 't1',
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: '/tmp/t1.wav',
      durationMs: 1000,
      createdAt: DateTime(2026, 1, 1),
      colorValue: colorValue,
    );

void main() {
  group('AudioTrack color', () {
    test('copyWith sets a custom color', () {
      final t = _track().copyWith(colorValue: 0xFF5B8DEF);
      expect(t.colorValue, 0xFF5B8DEF);
    });

    test('copyWith with no color args keeps the existing color', () {
      final t = _track(colorValue: 0xFFE3604E).copyWith(displayName: 'X');
      expect(t.colorValue, 0xFFE3604E);
      expect(t.displayName, 'X');
    });

    test('copyWith(clearColor: true) resets to the default (null)', () {
      final t = _track(colorValue: 0xFFE3604E).copyWith(clearColor: true);
      expect(t.colorValue, isNull);
    });

    test('colorValue round-trips through JSON', () {
      final json = _track(colorValue: 0xFF6FB562).toJson();
      expect(AudioTrack.fromJson(json).colorValue, 0xFF6FB562);
      // A track with no custom color stays null across JSON.
      expect(AudioTrack.fromJson(_track().toJson()).colorValue, isNull);
    });
  });
}
