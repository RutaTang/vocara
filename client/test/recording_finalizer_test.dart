// The quit-time finalize budget. Both quit paths (the close button's
// _QuitGuard and the Cmd+Q exit request) await finalizeAll before letting the
// app go, so an unbounded wait here is the one failure macOS resolves by
// force-killing the app — which finalizes nothing at all.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/recording_finalizer.dart';

void main() {
  test('a wedged finalizer cannot hold the quit, and does not starve the '
      'rest', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 30),
      totalBudget: const Duration(milliseconds: 500),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    var ran = 0;
    finalizer.register(() => never.future);
    finalizer.register(() async => ran++);

    // The outer guard is the test's stand-in for macOS's own watchdog.
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));

    expect(ran, 1, reason: 'the wedged callback must not starve the next one');
  });

  test('the total budget stops handing out fresh per-finalizer budgets', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 40),
      totalBudget: const Duration(milliseconds: 60),
    );
    final parked = <Completer<void>>[];
    addTearDown(() {
      for (final c in parked) {
        if (!c.isCompleted) c.complete();
      }
    });
    var entered = 0;
    for (var i = 0; i < 5; i++) {
      finalizer.register(() {
        entered++;
        final c = Completer<void>();
        parked.add(c);
        return c.future;
      });
    }

    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));

    // Asserting on how many callbacks were ENTERED (not on elapsed wall
    // clock) keeps this deterministic: five 40 ms budgets would be 200 ms,
    // well past the 60 ms total.
    expect(entered, greaterThanOrEqualTo(1));
    expect(entered, lessThan(5));
  });

  test('the normal path still runs every finalizer to completion', () async {
    final finalizer = RecordingFinalizer.forTesting();
    var ran = 0;
    finalizer.register(() async => ran++);
    finalizer.register(() async {
      ran++;
      throw StateError('one failure never blocks the rest');
    });
    finalizer.register(() async => ran++);

    await finalizer.finalizeAll();

    expect(ran, 3);
  });
}
