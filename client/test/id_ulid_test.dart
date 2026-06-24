import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/utils/id.dart';

void main() {
  group('newId (ULID)', () {
    final crockford = RegExp(r'^[0-9A-HJKMNP-TV-Z]{26}$');

    test('is 26 chars of Crockford base32 (no I, L, O, U)', () {
      for (var i = 0; i < 200; i++) {
        final id = newId();
        expect(id.length, 26, reason: id);
        expect(crockford.hasMatch(id), isTrue, reason: id);
      }
    });

    test('is unique across many rapid calls', () {
      final seen = <String>{};
      for (var i = 0; i < 5000; i++) {
        expect(seen.add(newId()), isTrue, reason: 'duplicate id');
      }
    });

    test('is strictly monotonic (lexicographic) within a burst', () {
      // Many ids minted in the same/adjacent ms must still sort by creation.
      var prev = newId();
      for (var i = 0; i < 5000; i++) {
        final next = newId();
        expect(next.compareTo(prev), greaterThan(0),
            reason: 'not increasing: $prev -> $next');
        prev = next;
      }
    });

    test('lexicographic order matches time order across millis', () async {
      final a = newId();
      await Future<void>.delayed(const Duration(milliseconds: 3));
      final b = newId();
      expect(b.compareTo(a), greaterThan(0));
      // The 10-char timestamp prefix is non-decreasing with wall time.
      expect(b.substring(0, 10).compareTo(a.substring(0, 10)),
          greaterThanOrEqualTo(0));
    });
  });
}
