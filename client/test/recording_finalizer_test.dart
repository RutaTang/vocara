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

  test('a stuck progress reading holds neither the saves nor a quit they '
      'finished in time', () async {
    // Read first, the reading held every save back for its whole limit, and
    // the quit after they were done.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(seconds: 1),
      totalBudget: const Duration(seconds: 3),
      progress: () => Completer<int>().future,
    );
    final clock = Stopwatch()..start();
    Duration? started;
    finalizer.register(() async => started = clock.elapsed);
    await finalizer.finalizeAll().timeout(const Duration(seconds: 5));
    expect(started, lessThan(const Duration(milliseconds: 500)));
    expect(clock.elapsed, lessThan(RecordingFinalizer.readingLimit));
  });

  test('a quit whose finalizers all settled leaves the next one its full '
      'budget', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 100),
      totalBudget: const Duration(milliseconds: 150),
    );
    finalizer.register(() async {});
    await finalizer.finalizeAll();
    await Future<void>.delayed(const Duration(milliseconds: 200));
    var landed = false;
    final save = finalizer.hold(() async {
      await Future<void>.delayed(const Duration(milliseconds: 20));
      landed = true;
    });
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(landed, isTrue, reason: 'the earlier quit spent its budget');
    await save;
  });

  test('a take armed while a cut-off quit waited is still finalized by the '
      'exit request', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 50),
      totalBudget: const Duration(milliseconds: 80),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future));
    final close = finalizer.finalizeAll(); // the close button's run
    await Future<void>.delayed(const Duration(milliseconds: 10));
    var stopped = 0;
    finalizer.register(() async {
      stopped++;
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }, startsWork: true);
    await close.timeout(const Duration(seconds: 2));
    await Future<void>.delayed(const Duration(milliseconds: 40)); // spent
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(stopped, 1, reason: "the exit request's run skipped the new take");
  });

  test('a take re-armed on the same screen during the wait is finalized '
      'too', () async {
    // The mic controller registers the SAME tear-off for every take: reusing
    // the first call's future, the exit request's run never stopped take 2.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 50),
      totalBudget: const Duration(milliseconds: 80),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future));
    var takes = 0;
    var stops = 0;
    late Future<void> Function() finalizeOnQuit;
    Future<void> stopTake() async {
      finalizer.unregister(finalizeOnQuit);
      stops++;
    }

    finalizeOnQuit = () async {
      if (takes > stops) await stopTake();
    };
    void arm() {
      takes++;
      finalizer.register(finalizeOnQuit, startsWork: true);
    }

    arm();
    final close = finalizer.finalizeAll();
    await Future<void>.delayed(const Duration(milliseconds: 10));
    arm(); // REC again while the close button's run waits
    await close.timeout(const Duration(seconds: 2));
    await Future<void>.delayed(const Duration(milliseconds: 40)); // spent
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(stops, 2, reason: 'take 2 was never stopped');
  });

  test('a take armed during the wait and stopped by the user keeps its own '
      'budget', () async {
    // Its stop-and-save carries on under waits not marked startsWork; with
    // the take's own finalizer gone, the exit request's run found the budget
    // spent and exited mid-save.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 100),
      totalBudget: const Duration(milliseconds: 60),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future)); // a wedged save
    final close = finalizer.finalizeAll();
    await Future<void>.delayed(const Duration(milliseconds: 10));
    late Future<void> Function() finalizeOnQuit;
    finalizeOnQuit = () async {};
    finalizer.register(finalizeOnQuit, startsWork: true); // REC
    await Future<void>.delayed(const Duration(milliseconds: 30));
    // The user's Stop: the take's finalizer goes, its save holds the quit.
    finalizer.unregister(finalizeOnQuit);
    var saved = false;
    unawaited(
      finalizer.hold(
        () => Future<void>.delayed(
          const Duration(milliseconds: 50),
          () => saved = true,
        ),
      ),
    );
    await close.timeout(const Duration(seconds: 2));
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(saved, isTrue, reason: "take 2's save at exit");
  });

  test(
    'a take stopped during the wait with nothing to save earns no budget',
    () async {
      // A practice take holds nothing once stopped: counted as uncounted work
      // anyway, it gave the exit request's run a full wait on nothing.
      final finalizer = RecordingFinalizer.forTesting(
        perFinalizerBudget: const Duration(milliseconds: 100),
        totalBudget: const Duration(milliseconds: 60),
      );
      final never = Completer<void>();
      addTearDown(() {
        if (!never.isCompleted) never.complete();
      });
      unawaited(finalizer.hold(() => never.future)); // a wedged save
      final close = finalizer.finalizeAll();
      await Future<void>.delayed(const Duration(milliseconds: 10));
      Future<void> practiceTake() async {}
      finalizer.register(practiceTake, startsWork: true); // REC
      finalizer.unregister(practiceTake); // Stop: nothing to save
      await close.timeout(const Duration(seconds: 2));
      final sw = Stopwatch()..start();
      await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
      expect(
        sw.elapsedMilliseconds,
        lessThan(50),
        reason: 'the budget is spent',
      );
    },
  );

  test("a stop's tail added during the wait is waited on by the next "
      'run', () async {
    // Reusing the first call's future, the exit request's run waited only on
    // the tails that existed then, and exited mid-save of the later one.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 200),
      totalBudget: const Duration(milliseconds: 600),
    );
    // A stable tear-off waiting on a growing set, like the mic controller's.
    final tails = <Future<void>>[];
    Future<void> holdForStops() => Future.wait(List.of(tails));
    final first = Completer<void>(); // take 1's save, slow
    tails.add(first.future);
    finalizer.register(holdForStops);
    final close = finalizer.finalizeAll();
    await Future<void>.delayed(const Duration(milliseconds: 10));
    var saved = false;
    tails.add(
      Future<void>.delayed(
        const Duration(milliseconds: 300),
        () => saved = true,
      ),
    );
    await close.timeout(const Duration(seconds: 2)); // cut off at 200 ms
    first.complete();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(saved, isTrue, reason: "the exit request's run missed the tail");
  });

  test("a take's own stop-and-save earns the quit no second budget", () async {
    // Stopping a take registers its tail and its save's hold as it goes;
    // counted as new work, they gave the exit request's run a full budget and
    // the quit ran past its total with nothing moving.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 200),
      totalBudget: const Duration(milliseconds: 300),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    late Future<void> Function() finalizeOnQuit;
    finalizeOnQuit = () async {
      finalizer.unregister(finalizeOnQuit);
      unawaited(finalizer.hold(() => never.future)); // the stalled save
      await never.future;
    };
    finalizer.register(finalizeOnQuit, startsWork: true);
    final sw = Stopwatch()..start();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    // A second full budget would be 400 ms.
    expect(sw.elapsedMilliseconds, lessThan(370));
  });

  test('a quit the app survived leaves the next one a fresh budget', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 30),
      totalBudget: const Duration(milliseconds: 40),
      progressCeiling: const Duration(milliseconds: 60),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    finalizer.register(() => never.future);
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    await Future<void>.delayed(const Duration(milliseconds: 150));
    var ran = false;
    finalizer.register(() async => ran = true);
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(ran, isTrue, reason: 'the old quit left the budget spent for good');
  });

  test('several wedged finalizers cost the quit one budget, not one '
      'each', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 100),
      totalBudget: const Duration(seconds: 5),
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

    final sw = Stopwatch()..start();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));

    expect(entered, 5, reason: 'every finalizer runs');
    // Five budgets one after another would be 500 ms.
    expect(sw.elapsedMilliseconds, lessThan(400));
  });

  test(
    'a save queued behind a long copy lands before the quit exits',
    () async {
      // One after another, the copy used up the total budget and the mute
      // queued behind it was never waited on: it needs only milliseconds, but
      // only once the copy is done.
      var copied = 0;
      final finalizer = RecordingFinalizer.forTesting(
        perFinalizerBudget: const Duration(milliseconds: 40),
        totalBudget: const Duration(milliseconds: 60),
        progressCeiling: const Duration(seconds: 5),
        progress: () async => copied,
      );
      final copy = finalizer.hold(() async {
        for (var i = 0; i < 10; i++) {
          await Future<void>.delayed(const Duration(milliseconds: 20));
          copied += 1 << 20;
        }
      });
      var muted = false;
      final mute = finalizer.hold(() async {
        await copy;
        await Future<void>.delayed(const Duration(milliseconds: 10)); // write
        muted = true;
      });
      await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
      expect(muted, isTrue, reason: 'the quit exited under the queued save');
      await mute;
    },
  );

  test('a copy keeps the quit waiting for the save it belongs to, then a '
      'wedged one is cut off', () async {
    // The copy is evidence for the save it belongs to, which the quit waits
    // on alongside the wedged one; once it lands nothing moves, and the
    // wedged save is cut off one budget later.
    var copied = 0;
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 40),
      totalBudget: const Duration(milliseconds: 60),
      progressCeiling: const Duration(seconds: 5),
      progress: () async => copied,
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future));
    var landed = false;
    final copy = finalizer.hold(() async {
      for (var i = 0; i < 10; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
        copied += 1 << 20;
      }
      landed = true;
    });
    final sw = Stopwatch()..start();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(landed, isTrue, reason: 'the copying save was not waited on');
    expect(sw.elapsedMilliseconds, lessThan(1000), reason: 'the wedge is cut');
    await copy;
  });

  test('the exit request after a close-button quit does not wait it all over '
      'again', () async {
    // The close button runs the quit, then destroy() raises an exit request
    // that runs it again for whatever is still pending.
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 100),
      totalBudget: const Duration(milliseconds: 150),
    );
    final never = Completer<void>();
    addTearDown(() {
      if (!never.isCompleted) never.complete();
    });
    unawaited(finalizer.hold(() => never.future));
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    await Future<void>.delayed(const Duration(milliseconds: 60));
    final sw = Stopwatch()..start();
    await finalizer.finalizeAll().timeout(const Duration(seconds: 2));
    expect(
      sw.elapsedMilliseconds,
      lessThan(50),
      reason: 'budget already spent',
    );
  });

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

  test('settled waits out a landing past the budget, and one listed while '
      'it waits', () async {
    final finalizer = RecordingFinalizer.forTesting(
      perFinalizerBudget: const Duration(milliseconds: 20),
    );
    final first = Completer<void>();
    final second = Completer<void>();
    unawaited(finalizer.landing('p', () => first.future));
    var done = false;
    final settled = finalizer.settled('p').whenComplete(() => done = true);
    await Future<void>.delayed(const Duration(milliseconds: 60));
    expect(done, isFalse, reason: 'no budget caps it');

    // landing() hands the finalize's own error back to its caller.
    final secondLanding = finalizer.landing('p', () => second.future);
    first.complete();
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(done, isFalse, reason: 'the take listed meanwhile still lands');

    final secondFails = expectLater(secondLanding, throwsStateError);
    second.completeError(StateError('a failed finalize still settles'));
    await settled;
    await secondFails;
    expect(finalizer.isLanding('p'), isFalse);
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

  // The quit's progress reading looks at an export's destination, which can sit
  // on a network share that stopped answering: the quit waited on it, and so
  // did every save behind it.
  for (final (name, reading) in <(String, Future<int> Function())>[
    ('never answers', () => Completer<int>().future),
    ('throws', () async => throw StateError('gone')),
  ]) {
    test(
      'a progress reading that $name neither holds nor stops the quit',
      () async {
        final finalizer = RecordingFinalizer.forTesting(
          perFinalizerBudget: const Duration(milliseconds: 50),
          totalBudget: const Duration(milliseconds: 80),
          progress: reading,
        );
        var saved = false;
        finalizer.register(() async => saved = true, startsWork: true);
        final clock = Stopwatch()..start();
        await finalizer.finalizeAll();
        expect(saved, isTrue);
        expect(
          clock.elapsed,
          lessThan(RecordingFinalizer.readingLimit * 3),
          reason: 'one bounded reading, then the finalizers',
        );
      },
    );
  }
}
