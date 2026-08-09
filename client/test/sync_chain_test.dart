// The playback engine's sync serializer.
//
// Graph rebuilds decode files asynchronously, so two of them running at once
// would stage into the index maps the other is swapping. SyncChain is the
// single-file queue that prevents it — and, crucially, the queue a running
// sync can push its OWN follow-up onto: a commit that degraded a streaming
// track to silence queues exactly one rebuild from inside _doSync, and that
// must land after the commit instead of deadlocking on it.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/graph_audio_playback_service.dart';

void main() {
  test('jobs run one at a time, in the order they were queued', () async {
    final chain = SyncChain();
    final log = <String>[];
    Future<void> job(String name) async {
      log.add('$name:start');
      await Future<void>.delayed(Duration.zero);
      log.add('$name:end');
    }

    final a = chain.add(() => job('a'));
    final b = chain.add(() => job('b'));
    await Future.wait([a, b]);

    expect(log, ['a:start', 'a:end', 'b:start', 'b:end']);
  });

  test('a job may queue another from inside itself', () async {
    // The degraded-commit retry: the running sync withholds the committed
    // signature and queues the rebuild that will re-probe the file. Nothing
    // else ever calls sync for it — play, seek and fader moves all bypass the
    // graph rebuild — so if this deadlocked or was dropped, the track would
    // stay silent for the rest of the session.
    final chain = SyncChain();
    final log = <String>[];
    var retried = false;

    Future<void> commit() async {
      log.add('commit:start');
      if (!retried) {
        retried = true;
        // Fire-and-forget from INSIDE the running job.
        unawaited(chain.add(() async => log.add('rebuild')));
      }
      await Future<void>.delayed(Duration.zero);
      log.add('commit:end');
    }

    await chain.add(commit);
    // Let the queued rebuild run.
    await chain.add(() async {});

    expect(log, ['commit:start', 'commit:end', 'rebuild']);
  });

  test(
    'a failing job reports to its caller without poisoning the queue',
    () async {
      final chain = SyncChain();
      final ran = <String>[];

      final failed = chain.add(() async {
        ran.add('boom');
        throw StateError('commit rejected');
      });
      final after = chain.add(() async => ran.add('after'));

      await expectLater(failed, throwsStateError);
      await after;
      expect(ran, ['boom', 'after']);
    },
  );
}
