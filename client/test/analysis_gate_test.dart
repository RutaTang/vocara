import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/utils/analysis_gate.dart';
import 'package:vocara/utils/lru_bytes_cache.dart';

void main() {
  group('AnalysisGate', () {
    test('admits up to the limit and queues the rest FIFO', () async {
      final gate = AnalysisGate(2);
      final release = <Completer<void>>[];
      final done = <int>[];
      Future<void> job(int n) => gate.run(() async {
        final c = Completer<void>();
        release.add(c);
        await c.future;
        done.add(n);
      });

      final f1 = job(1);
      final f2 = job(2);
      final f3 = job(3);
      await Future<void>.delayed(Duration.zero);
      expect(gate.running, 2, reason: 'two admitted');
      expect(gate.queued, 1, reason: 'the third waits');
      expect(release, hasLength(2), reason: 'job 3 must not have STARTED');

      release[0].complete();
      await f1;
      await Future<void>.delayed(Duration.zero);
      expect(gate.running, 2, reason: 'job 3 admitted after a slot freed');
      expect(release, hasLength(3));

      release[1].complete();
      release[2].complete();
      await Future.wait([f2, f3]);
      expect(done, [1, 2, 3]);
      expect(gate.running, 0);
    });

    test('a throwing job frees its slot', () async {
      final gate = AnalysisGate(1);
      await expectLater(
        gate.run(() async => throw StateError('boom')),
        throwsStateError,
      );
      // The gate must not be wedged.
      final v = await gate.run(() async => 42);
      expect(v, 42);
    });
  });

  group('LruBytesCache', () {
    test('evicts least-recently-USED past the byte budget', () {
      final cache = LruBytesCache<List<int>>(10, (v) => v.length);
      cache['a'] = List.filled(4, 0);
      cache['b'] = List.filled(4, 0);
      // Touch 'a' so 'b' is the eviction victim.
      expect(cache['a'], isNotNull);
      cache['c'] = List.filled(4, 0); // 12 > 10 -> evict LRU ('b')
      expect(cache.containsKey('a'), isTrue);
      expect(cache.containsKey('b'), isFalse);
      expect(cache.containsKey('c'), isTrue);
      expect(cache.bytes, 8);
    });

    test('replacing a key retallies bytes instead of double-counting', () {
      final cache = LruBytesCache<List<int>>(10, (v) => v.length);
      cache['a'] = List.filled(6, 0);
      cache['a'] = List.filled(2, 0);
      expect(cache.bytes, 2);
      expect(cache.length, 1);
    });

    test('keeps the newest entry even when it alone exceeds the budget', () {
      final cache = LruBytesCache<List<int>>(10, (v) => v.length);
      cache['big'] = List.filled(50, 0);
      expect(
        cache.containsKey('big'),
        isTrue,
        reason: 'better served once than recomputed forever',
      );
      cache['next'] = List.filled(4, 0);
      expect(
        cache.containsKey('big'),
        isFalse,
        reason: 'evicted when superseded',
      );
      expect(cache.containsKey('next'), isTrue);
    });

    test('remove and clear keep the byte tally honest', () {
      final cache = LruBytesCache<List<int>>(100, (v) => v.length);
      cache['a'] = List.filled(6, 0);
      cache['b'] = List.filled(4, 0);
      cache.remove('a');
      expect(cache.bytes, 4);
      cache.clear();
      expect(cache.bytes, 0);
      expect(cache.isEmpty, isTrue);
    });

    test('a write after dispose cannot charge the shared budget', () {
      // The owner of a shared cache disposes it when its screen goes away,
      // but its in-flight analysis can land afterwards. A disposed cache is
      // no longer a member of the budget, so anything charged then could
      // never be evicted — it would permanently shrink what the live screens
      // may hold, one abandoned analysis at a time.
      final budget = LruBytesBudget(100);
      final live = LruBytesCache<List<int>>.shared(budget, (v) => v.length);
      final leaving = LruBytesCache<List<int>>.shared(budget, (v) => v.length);
      live['kept'] = List.filled(10, 0);
      leaving['a'] = List.filled(20, 0);
      expect(budget.bytes, 30);

      leaving.dispose();
      expect(
        budget.bytes,
        10,
        reason: 'the disposed cache gave its bytes back',
      );

      leaving['late-analysis'] = List.filled(40, 0);
      expect(budget.bytes, 10, reason: 'phantom bytes nothing could evict');
      expect(leaving.isDisposed, isTrue);
      expect(leaving['late-analysis'], isNull);
      // The live cache still owns the whole budget it started with.
      live['more'] = List.filled(80, 0);
      expect(live.containsKey('more'), isTrue);
      expect(live.containsKey('kept'), isTrue);
      expect(budget.bytes, 90);
    });
  });
}
