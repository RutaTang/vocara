// The graph engine's sync pipeline policies.
//
// Pinned here: the degraded-commit mask is a full u64 (bit 63 is the overflow
// fold), so counting its bits must terminate on a negative Dart
// reinterpretation.

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';

void main() {
  group('bitCount', () {
    test('counts ordinary masks', () {
      expect(GraphAudioPlaybackService.bitCount(0), 0);
      expect(GraphAudioPlaybackService.bitCount(1), 1);
      expect(GraphAudioPlaybackService.bitCount(0x2d), 4);
    });

    test('terminates on a mask with bit 63 (the degraded overflow fold)', () {
      // ve_commit_degraded_mask is a u64; the Rust side folds staged indices
      // >= 63 into bit 63, which arrives through the Uint64 binding as a
      // NEGATIVE Dart int. An arithmetic shift would loop forever on it.
      expect(GraphAudioPlaybackService.bitCount(1 << 63), 1);
      expect(GraphAudioPlaybackService.bitCount((1 << 63) | 0x3), 3);
      expect(GraphAudioPlaybackService.bitCount(-1), 64);
    });
  });
}
