import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';

void main() {
  group('analysis decode duration cap', () {
    const cap = Duration(minutes: 30);

    test('trips just past the cap, never at it', () {
      final capFrames = cap.inSeconds * 44100;
      expect(exceedsDecodeCap(capFrames, 44100, cap), isFalse);
      expect(exceedsDecodeCap(capFrames + 1, 44100, cap), isTrue);
      expect(exceedsDecodeCap(0, 44100, cap), isFalse);
    });

    test('is a duration bound at the ACTUAL decode rate', () {
      // 31 minutes of 22.05 kHz audio has fewer frames than 30 minutes of
      // 44.1 kHz audio, but must still trip the cap at its own rate.
      final frames = 31 * 60 * 22050;
      expect(exceedsDecodeCap(frames, 22050, cap), isTrue);
      expect(exceedsDecodeCap(frames, 44100, cap), isFalse);
    });

    test('MediaTooLongException carries a user-presentable message', () {
      const e = MediaTooLongException(cap);
      expect('$e', contains('30 minutes'));
      // UI copy rule: sentences are never joined with dashes.
      expect('$e', isNot(contains(' - ')));
      expect('$e', isNot(contains('—')));
    });

    test('the analysis service caps at 30 minutes of decoded audio', () {
      expect(MediaAnalysisService.maxAnalysisDuration, cap);
    });
  });
}
