// The app-wide store write gate: shared sections (saves/imports) overlap
// freely, exclusive sections (repairLibrary, boot sweeps) wait for the shared
// side to drain and hold off new shared sections while they run. The gate is
// reader-preferring by design so NESTED shared acquisition (a save holding
// the gate while importFile re-enters it) can never deadlock behind a queued
// exclusive — the exact shape a repairLibrary racing an in-flight save takes.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/write_gate.dart';

void main() {
  test('exclusive waits for every in-flight shared section', () async {
    final gate = WriteGate();
    final log = <String>[];
    final release = Completer<void>();

    final s = gate.shared(() async {
      log.add('shared-start');
      await release.future;
      log.add('shared-end');
    });
    final x = gate.exclusive(() async => log.add('exclusive'));

    // Give the exclusive every chance to (incorrectly) jump the queue.
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(log, ['shared-start']);

    release.complete();
    await s;
    await x;
    expect(log, ['shared-start', 'shared-end', 'exclusive']);
  });

  test('shared sections overlap freely', () async {
    final gate = WriteGate();
    final aStarted = Completer<void>();
    final bStarted = Completer<void>();
    // Each section finishes only once the OTHER one has started: completes
    // only if the two genuinely overlap.
    await Future.wait([
      gate.shared(() async {
        aStarted.complete();
        await bStarted.future;
      }),
      gate.shared(() async {
        bStarted.complete();
        await aStarted.future;
      }),
    ]).timeout(const Duration(seconds: 5));
  });

  test(
    'nested shared acquisition cannot deadlock behind a queued exclusive',
    () async {
      final gate = WriteGate();
      final log = <String>[];

      final outer = gate.shared(() async {
        log.add('outer-start');
        // Let the exclusive queue up mid-section, then re-enter the gate the
        // way importFile does inside a repository save.
        await Future<void>.delayed(const Duration(milliseconds: 10));
        await gate.shared(() async => log.add('nested'));
        log.add('outer-end');
      });
      final excl = gate.exclusive(() async => log.add('exclusive'));

      await Future.wait([outer, excl]).timeout(const Duration(seconds: 5));
      expect(log, ['outer-start', 'nested', 'outer-end', 'exclusive']);
    },
  );

  test('a shared section starting during a running exclusive waits', () async {
    final gate = WriteGate();
    final log = <String>[];
    final release = Completer<void>();

    final x = gate.exclusive(() async {
      log.add('exclusive-start');
      await release.future;
      log.add('exclusive-end');
    });
    // Let the exclusive actually start before requesting the shared side.
    await Future<void>.delayed(const Duration(milliseconds: 5));
    final s = gate.shared(() async => log.add('shared'));
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(log, ['exclusive-start']);

    release.complete();
    await x;
    await s;
    expect(log, ['exclusive-start', 'exclusive-end', 'shared']);
  });

  test('exclusive sections are serialized in call order', () async {
    final gate = WriteGate();
    final log = <String>[];
    final release = Completer<void>();

    final x1 = gate.exclusive(() async {
      log.add('x1-start');
      await release.future;
      log.add('x1-end');
    });
    final x2 = gate.exclusive(() async => log.add('x2'));

    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(log, ['x1-start']);

    release.complete();
    await x1;
    await x2;
    expect(log, ['x1-start', 'x1-end', 'x2']);
  });

  test('errors propagate and never wedge the gate', () async {
    final gate = WriteGate();
    await expectLater(
      gate.shared(() async => throw StateError('shared boom')),
      throwsStateError,
    );
    await expectLater(
      gate.exclusive(() async => throw StateError('exclusive boom')),
      throwsStateError,
    );
    // Both sides still work after the failures.
    expect(await gate.shared(() async => 1), 1);
    expect(await gate.exclusive(() async => 2), 2);
  });
}
