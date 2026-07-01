import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/sync/hlc.dart';

void main() {
  group('Hlc packing + ordering', () {
    test('packed is lexicographically ordered like compareTo', () {
      final a = Hlc(wallMs: 1000, counter: 0, node: 'AAAA');
      final b = Hlc(wallMs: 1000, counter: 1, node: 'AAAA');
      final c = Hlc(wallMs: 1001, counter: 0, node: 'AAAA');
      final d = Hlc(wallMs: 1000, counter: 0, node: 'BBBB');
      expect(a < b, isTrue);
      expect(b < c, isTrue);
      expect(a < d, isTrue); // node breaks the tie
      expect(a.packed.compareTo(b.packed) < 0, isTrue);
      expect(b.packed.compareTo(c.packed) < 0, isTrue);
      expect(a.packed.compareTo(d.packed) < 0, isTrue);
    });

    test('packed round-trips through parse (node may be a ULID)', () {
      final h = Hlc(
        wallMs: 1717000000000,
        counter: 42,
        node: '01J9Z3KQ8V5N7P2R4T6W8XABCD',
      );
      final back = Hlc.parse(h.packed);
      expect(back, h);
      expect(back.wallMs, h.wallMs);
      expect(back.counter, 42);
      expect(back.node, h.node);
    });

    test('packed is fixed width so string sort never breaks on magnitude', () {
      final small = Hlc(wallMs: 9, counter: 0, node: 'N').packed;
      final big = Hlc(wallMs: 1000000000000, counter: 0, node: 'N').packed;
      expect(small.compareTo(big) < 0, isTrue);
    });
  });

  group('HlcClock.tick (local writes)', () {
    test('counter increments within a millisecond, resets when wall advances',
        () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now);
      expect(clock.tick(), Hlc(wallMs: 1000, counter: 0, node: 'N'));
      expect(clock.tick(), Hlc(wallMs: 1000, counter: 1, node: 'N'));
      expect(clock.tick(), Hlc(wallMs: 1000, counter: 2, node: 'N'));
      now = 1001;
      expect(clock.tick(), Hlc(wallMs: 1001, counter: 0, node: 'N'));
    });

    test('stays monotonic when the wall clock goes backwards', () {
      var now = 5000;
      final clock = HlcClock(node: 'N', now: () => now);
      final a = clock.tick();
      now = 4000; // clock jumped back
      final b = clock.tick();
      expect(b > a, isTrue);
      expect(b.wallMs, 5000); // held at the last seen wall
      expect(b.counter, 1);
    });

    test('a once-future clock stays strictly monotonic on tick (never smaller)',
        () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now, maxDriftMs: 60000);
      // Force the internal clock into the future via a remote merge (30s ahead).
      clock.receive(Hlc(wallMs: 1000 + 30000, counter: 0, node: 'M'));
      expect(clock.last.wallMs, 31000);
      // Wall is still ~1000; tick holds at the last seen wall (monotonic).
      now = 1001;
      expect(clock.tick().wallMs, 31000);

      // Even when the last stamp is far ahead of wall (a clock corrected after a
      // large forward skew), tick must NOT emit a stamp that compares LESS than its
      // own previous one — that would silently drop the device's newest edit under
      // LWW. It holds at the future wall and bumps the counter until real time
      // catches up, instead of resetting backward.
      final skewed = Hlc(wallMs: 1002 + 60000 + 5, counter: 9, node: 'N');
      final clock2 = HlcClock(
        node: 'N',
        now: () => 1002,
        maxDriftMs: 60000,
        initial: skewed,
      );
      final t = clock2.tick();
      expect(t > skewed, isTrue, reason: 'strictly monotonic, never backward');
      expect(t.wallMs, skewed.wallMs);
      expect(t.counter, 10);
    });

    test('a slow local clock still accepts correctly-stamped remotes', () {
      // The device operated with a correct clock before (its persisted last stamp is
      // near real time), then its clock went 10 min slow. maxWallEverSeen (seeded
      // from that persisted stamp) keeps the drift gate anchored to real time, so an
      // honest peer near true-now is accepted instead of quarantined.
      const trueNow = 10 * 60 * 1000; // 600000
      final clock = HlcClock(
        node: 'N',
        now: () => 1000, // local clock 10 min behind real time
        initial: Hlc(wallMs: trueNow, counter: 0, node: 'N'),
      );
      final merged =
          clock.receive(Hlc(wallMs: trueNow + 1000, counter: 0, node: 'M'));
      expect(merged.wallMs, trueNow + 1000);
    });
  });

  group('HlcClock.receive (remote merge)', () {
    test('adopts a remote ahead within drift and advances past it', () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now);
      clock.tick(); // local at 1000/0
      final merged = clock.receive(Hlc(wallMs: 1005, counter: 3, node: 'M'));
      expect(merged.wallMs, 1005);
      expect(merged.counter, 4); // remote.counter + 1 (wall == remote.wall)
      expect(merged.node, 'N'); // the local clock keeps its own node
    });

    test('remote in the past does not move us backwards', () {
      var now = 2000;
      final clock = HlcClock(node: 'N', now: () => now);
      final local = clock.tick(); // 2000/0
      final merged = clock.receive(Hlc(wallMs: 500, counter: 9, node: 'M'));
      expect(merged.wallMs, 2000);
      expect(merged >= local, isTrue);
    });

    test('ties between equal local+remote walls take max counter + 1', () {
      var now = 1000;
      final clock = HlcClock(
        node: 'N',
        now: () => now,
        initial: Hlc(wallMs: 1000, counter: 2, node: 'N'),
      );
      final merged = clock.receive(Hlc(wallMs: 1000, counter: 7, node: 'M'));
      expect(merged.wallMs, 1000);
      expect(merged.counter, 8); // max(2,7)+1
    });

    test('refuses (quarantines) a remote implausibly far in the future', () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now, maxDriftMs: 60000);
      expect(
        () => clock.receive(Hlc(wallMs: 1000 + 60001, counter: 0, node: 'BAD')),
        throwsA(isA<HlcDriftException>()),
      );
      // The bad clock did not contaminate us.
      expect(clock.last.wallMs, lessThanOrEqualTo(1000));
    });
  });

  test('two clocks converge to a deterministic total order', () {
    // Cross-merge a few stamps and assert the relation is a strict total order.
    final a = HlcClock(node: 'AAA', now: () => 100);
    final b = HlcClock(node: 'BBB', now: () => 100);
    final s1 = a.tick();
    final s2 = b.receive(s1);
    final s3 = a.receive(s2);
    expect(s1 < s2, isTrue);
    expect(s2 < s3, isTrue);
    expect(s1 < s3, isTrue);
  });
}
