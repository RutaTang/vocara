import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/ui/song_practice_detail/view_model/mixer_controller.dart';

void main() {
  group('computeAudible', () {
    test('no solo: only mute decides', () {
      expect(computeAudible(soloed: false, muted: false, anySolo: false), true);
      expect(computeAudible(soloed: false, muted: true, anySolo: false), false);
    });

    test('solo active: only soloed tracks are heard', () {
      expect(computeAudible(soloed: true, muted: false, anySolo: true), true);
      expect(computeAudible(soloed: false, muted: false, anySolo: true), false);
    });

    test('mute still wins over solo (the bug fix)', () {
      // A soloed-but-muted track must be silent — previously mute was ignored
      // whenever any track was soloed, which made "M" appear to do nothing.
      expect(computeAudible(soloed: true, muted: true, anySolo: true), false);
    });
  });
}
