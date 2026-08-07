import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';
import 'package:vocara/data/services/media_analysis_service.dart';

void main() {
  group('analysis decode duration cap', () {
    const cap = Duration(minutes: 10);

    test('trips just past the cap, never at it', () {
      final capFrames = cap.inSeconds * 44100;
      expect(exceedsDecodeCap(capFrames, 44100, cap), isFalse);
      expect(exceedsDecodeCap(capFrames + 1, 44100, cap), isTrue);
      expect(exceedsDecodeCap(0, 44100, cap), isFalse);
    });

    test('is a duration bound at the ACTUAL decode rate', () {
      // 11 minutes of 22.05 kHz audio has fewer frames than 10 minutes of
      // 44.1 kHz audio, but must still trip the cap at its own rate.
      final frames = 11 * 60 * 22050;
      expect(exceedsDecodeCap(frames, 22050, cap), isTrue);
      expect(exceedsDecodeCap(frames, 44100, cap), isFalse);
    });

    test('MediaTooLongException carries a user-presentable message', () {
      const e = MediaTooLongException(cap);
      expect('$e', contains('10 minutes'));
      // UI copy rule: sentences are never joined with dashes.
      expect('$e', isNot(contains(' - ')));
      expect('$e', isNot(contains('—')));
    });

    test('the analysis service caps at 10 minutes of decoded audio', () {
      expect(MediaAnalysisService.maxAnalysisDuration, cap);
    });

    test('refusal statuses map to DISTINCT user-presentable diagnoses', () {
      // Every refusal used to collapse into "too long": a short 384 kHz file
      // and a failing disk both read as an over-duration file.
      expect(
        decodeStatusToException(veDecodeTooLong, cap),
        isA<MediaTooLongException>(),
      );
      final tooBig = decodeStatusToException(veDecodeTooBig, cap);
      expect(tooBig, isA<MediaUnsupportedException>());
      expect('$tooBig', isNot(contains('too long')));
      final rate = decodeStatusToException(veDecodeRateUnsupported, cap);
      expect(rate, isA<MediaUnsupportedException>());
      expect('$rate', contains('sample rate'));
      final io = decodeStatusToException(veDecodeIo, cap);
      expect(io, isA<MediaUnsupportedException>());
      expect('$io', contains('read'));
      expect(decodeStatusToException(0, cap), isNull);
      expect(decodeStatusToException(-1, cap), isNull);
      // UI copy rule: sentences are never joined with dashes.
      for (final e in [tooBig, rate, io]) {
        expect('$e', isNot(contains(' - ')));
        expect('$e', isNot(contains('—')));
      }
    });
  });
}
