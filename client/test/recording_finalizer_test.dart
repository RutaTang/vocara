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

  test(
    'the total budget stops handing out fresh per-finalizer budgets',
    () async {
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
    },
  );

  test(
    'a held save still copying past its budget keeps the quit waiting',
    () async {
      // A Save pooling a large example (a 1 GB video copies for well over the
      // 5 s budget) is working, not wedged: cutting it off lost the Save.
      var copied = 0;
      final finalizer = RecordingFinalizer.forTesting(
        perFinalizerBudget: const Duration(milliseconds: 40),
        totalBudget: const Duration(milliseconds: 60),
        progressCeiling: const Duration(seconds: 5),
        progress: () async => copied,
      );
      var landed = false;
      final save = finalizer.hold(() async {
        for (var i = 0; i < 15; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 20));
          copied += 1 << 20; // a megabyte more on disk
        }
        landed = true;
      });
      await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
      expect(landed, isTrue, reason: 'the quit cut off a save still copying');
      await save;
    },
  );

  test('a save still copying is not cut off when another copy lands', () async {
    // The reading is the staging files in flight, so it DROPS when another
    // import finishes and leaves staging: that is progress, not a stall.
    var copied = 100 << 20; // another import's 100 MB already in staging
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 40),
      totalBudget: const Duration(milliseconds: 60),
      progressCeiling: const Duration(seconds: 5),
      progress: () async => copied,
    );
    var landed = false;
    final save = finalizer.hold(() async {
      for (var i = 0; i < 15; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
        copied += 1 << 20;
        if (i == 3) copied -= 100 << 20; // the other import landed
      }
      landed = true;
    });
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(landed, isTrue, reason: 'the drop was read as a stalled save');
    await save;
  });

  test('a held save that stops progressing is still cut off', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 40),
      totalBudget: const Duration(milliseconds: 60),
      progressCeiling: const Duration(seconds: 5),
      progress: () async => 7, // never moves
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future));
    final sw = Stopwatch()..start();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(sw.elapsedMilliseconds, lessThan(500));
  });

  test('a held save keeps the quit waiting until it settles, and only that '
      'long', () async {
    final finalizer = RecordingFinalizer.forTesting();
    final commit = Completer<int>();
    final saved = finalizer.hold(() => commit.future);
    expect(finalizer.hasActive, isTrue, reason: 'held from the first call');

    var quitDone = false;
    final quit = finalizer.finalizeAll().whenComplete(() => quitDone = true);
    await pumpEventQueue();
    expect(quitDone, isFalse, reason: 'the quit waits for the airborne save');

    commit.complete(7);
    await quit;
    expect(await saved, 7, reason: "the caller still gets the save's result");
    expect(finalizer.hasActive, isFalse);
  });

  test('a failed held save reaches its caller, never the quit', () async {
    final finalizer = RecordingFinalizer.forTesting();
    final commit = Completer<void>();
    final saved = finalizer.hold(() => commit.future);
    final quit = finalizer.finalizeAll();
    commit.completeError(StateError('disk full'));

    await expectLater(saved, throwsStateError);
    await quit; // completes normally
    expect(finalizer.hasActive, isFalse);
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

  test('a reopened practice waits for its landing take, but not for a '
      'wedged one', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 30),
    );
    final take = Completer<void>();
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.landing('p1', () => take.future));
    var landed = false;
    unawaited(finalizer.landed('p1').then((_) => landed = true));
    await Future<void>.delayed(const Duration(milliseconds: 5));
    expect(landed, isFalse, reason: 'its take is still landing');
    take.complete();
    await Future<void>.delayed(Duration.zero);
    expect(landed, isTrue);

    unawaited(finalizer.landing('p1', () => never.future));
    // The outer guard is the test's stand-in for a screen that never opens.
    await finalizer.landed('p1').timeout(const Duration(seconds: 2));
    await finalizer.landed('other').timeout(const Duration(seconds: 2));
  });
}
