import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/technique_media.dart';

void main() {
  test('offers the audio media_kit plays, decodable by the engine or not', () {
    // Examples play through media_kit: dropping Opus and WMA (the engine
    // has no decoder for either) refused files that play, when only their
    // analysis fails.
    for (final ok in [
      'a.wav',
      'a.MP3',
      'a.m4a',
      'a.aiff',
      'a.caf',
      'a.flac',
      // Ogg audio the song screen takes too (the engine decodes it).
      'a.oga',
      'a.opus',
      'a.wma',
      'a.mp4',
    ]) {
      expect(TechniqueMedia.isSupportedMedia(ok), isTrue, reason: ok);
    }
    expect(TechniqueMedia.isSupportedMedia('a.pdf'), isFalse);
  });
}
