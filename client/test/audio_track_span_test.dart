import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';

AudioClip _clip(String id, {required int startMs, required int durationMs}) =>
    AudioClip(
      id: id,
      audioFilePath: '/tmp/$id.wav',
      durationMs: durationMs,
      startMs: startMs,
    );

AudioTrack _lane(List<AudioClip> clips) => AudioTrack(
  id: 'lane',
  kind: AudioTrackKind.vocal,
  displayName: 'Take 1',
  createdAt: DateTime(2026),
  clips: clips,
);

void main() {
  group('AudioTrack lane-span time model', () {
    test('a single-clip lane spans exactly its clip', () {
      final track = _lane([_clip('c1', startMs: 1500, durationMs: 4000)]);
      expect(track.startMs, 1500);
      expect(track.endMs, 5500);
      expect(track.durationMs, 4000);
      expect(track.primaryClip!.id, 'c1');
    });

    test('the single-clip factory convenience matches the clip fields too', () {
      final track = AudioTrack(
        id: 't',
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        createdAt: DateTime(2026),
        audioFilePath: '/tmp/t.wav',
        startMs: 250,
        durationMs: 1000,
      );
      expect(track.startMs, 250);
      expect(track.endMs, 1250);
      expect(track.durationMs, 1000);
    });

    test('an empty lane reads 0/0/0 and has no primary clip', () {
      final track = _lane(const []);
      expect(track.startMs, 0);
      expect(track.endMs, 0);
      expect(track.durationMs, 0);
      expect(track.primaryClip, isNull);
      expect(track.hasClip, isFalse);
      expect(track.isEmptyTake, isTrue);
    });

    test('a multi-clip lane spans min start .. max end, gap included', () {
      final track = _lane([
        _clip('c1', startMs: 1000, durationMs: 2000), // 1000..3000
        _clip('c2', startMs: 5000, durationMs: 3000), // 5000..8000
      ]);
      expect(track.startMs, 1000);
      expect(track.endMs, 8000);
      expect(track.durationMs, 7000); // includes the 3000..5000 gap
    });

    test('the span does not depend on the clips\' list order', () {
      // A punch-in recorded BEFORE the first clip lands later in the list;
      // primaryClip stays the first LIST entry, but the span is min..max.
      final track = _lane([
        _clip('c1', startMs: 5000, durationMs: 3000), // 5000..8000
        _clip('c2', startMs: 1000, durationMs: 2000), // 1000..3000
      ]);
      expect(track.primaryClip!.id, 'c1');
      expect(track.startMs, 1000);
      expect(track.endMs, 8000);
      expect(track.durationMs, 7000);
    });

    test('overlapping clips span the union of their extents', () {
      final track = _lane([
        _clip('c1', startMs: 0, durationMs: 4000), // 0..4000
        _clip('c2', startMs: 2000, durationMs: 1000), // 2000..3000 (inside c1)
      ]);
      expect(track.startMs, 0);
      expect(track.endMs, 4000);
      expect(track.durationMs, 4000);
    });

    test('copyWith(clips:) recomputes the span', () {
      final track = _lane([_clip('c1', startMs: 1000, durationMs: 2000)]);
      final grown = track.copyWith(
        clips: [...track.clips, _clip('c2', startMs: 6000, durationMs: 500)],
      );
      expect(grown.startMs, 1000);
      expect(grown.endMs, 6500);
      expect(grown.durationMs, 5500);
      // The original is untouched (immutable model).
      expect(track.durationMs, 2000);
    });

    test('the span survives a JSON round-trip', () {
      final track = _lane([
        _clip('c1', startMs: 5000, durationMs: 3000),
        _clip('c2', startMs: 1000, durationMs: 2000),
      ]);
      final back = AudioTrack.fromJson(track.toJson());
      expect(back.startMs, 1000);
      expect(back.endMs, 8000);
      expect(back.durationMs, 7000);
    });
  });
}
