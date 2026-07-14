import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/utils/busy_gate.dart';

void main() {
  group('BusyGate.run', () {
    test('runs the action under the busy latch and reports acceptance',
        () async {
      final errors = <Object>[];
      final gate = BusyGate(onError: errors.add);
      var ran = false;

      final accepted = await gate.run(() async {
        ran = true;
        expect(gate.busy, isTrue);
      });

      expect(accepted, isTrue);
      expect(ran, isTrue);
      expect(gate.busy, isFalse);
      expect(errors, isEmpty);
      gate.dispose();
    });

    test('is ATOMIC: a second concurrent run is rejected and its action '
        'never executes, busy stays latched until the FIRST finishes',
        () async {
      final gate = BusyGate(onError: (_) {});
      final firstBlocked = Completer<void>();
      var secondRan = false;

      final first = gate.run(() => firstBlocked.future);
      // The latch is set synchronously before the action's first await.
      expect(gate.busy, isTrue);

      final second = await gate.run(() async => secondRan = true);
      expect(second, isFalse, reason: 'the overlapping run must be rejected');
      expect(secondRan, isFalse);
      expect(
        gate.busy,
        isTrue,
        reason: 'a rejected run must not clear the first run\'s latch',
      );

      firstBlocked.complete();
      expect(await first, isTrue);
      expect(gate.busy, isFalse);
      gate.dispose();
    });

    test('accepts a new run once the previous one finished', () async {
      final gate = BusyGate(onError: (_) {});
      expect(await gate.run(() async {}), isTrue);
      expect(await gate.run(() async {}), isTrue);
      gate.dispose();
    });

    test('a throwing action still counts as ran: errorMessage + onError are '
        'set, busy clears, and the gate reopens', () async {
      final errors = <Object>[];
      final gate = BusyGate(onError: errors.add);

      final accepted = await gate.run(() async => throw StateError('boom'));

      expect(accepted, isTrue);
      expect(gate.busy, isFalse);
      expect(gate.errorMessage, contains('boom'));
      expect(errors, hasLength(1));
      // The error does not wedge the gate shut.
      expect(await gate.run(() async {}), isTrue);
      gate.dispose();
    });
  });
}
