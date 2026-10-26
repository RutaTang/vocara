import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/technique_media.dart';

void main() {
  test('offers only the audio the engine can decode', () {
    // Opus and WMA were offered with no decoder for either: every import of
    // one failed after the copy.
    for (final ok in ['a.wav', 'a.MP3', 'a.m4a', 'a.aiff', 'a.flac', 'a.mp4']) {
      expect(TechniqueMedia.isSupportedMedia(ok), isTrue, reason: ok);
    }
    for (final no in ['a.opus', 'a.wma', 'a.pdf']) {
      expect(TechniqueMedia.isSupportedMedia(no), isFalse, reason: no);
    }
  });
}
