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

  group('HlcClock.derived (node-space derivation)', () {
    const device = 'DEVICE00000000000000000000';
    final base = Hlc(wallMs: 1000, counter: 5, node: 'A' * 26);

    test('out-ranks its base (the write replicates) yet sorts below EVERY '
        'stamp that out-ranks the base', () {
      final d = HlcClock.derived(base, device);
      expect(d > base, isTrue, reason: 'must replicate over the base state');
      expect(d.wallMs, base.wallMs);
      expect(d.counter, base.counter, reason: 'never escalates the counter');

      // A held-wall author edit one counter above the base.
      final heldWall = Hlc(wallMs: 1000, counter: 6, node: '0' * 26);
      expect(d < heldWall, isTrue);
      // A node-space tie winner barely above the base — the parked-update
      // shape whose replay must still beat the derived tombstone.
      final nodeWinner = Hlc(wallMs: 1000, counter: 5, node: '${'A' * 25}B');
      expect(nodeWinner > base, isTrue, reason: 'fixture sanity');
      expect(d < nodeWinner, isTrue);
      // Packed order agrees everywhere the tuple order does.
      expect(d.packed.compareTo(base.packed) > 0, isTrue);
      expect(d.packed.compareTo(heldWall.packed) < 0, isTrue);
      expect(d.packed.compareTo(nodeWinner.packed) < 0, isTrue);
    });

    test('a CHAIN of derivations still loses to every real edit above the '
        'original base (no counter escalation, ever)', () {
      final d1 = HlcClock.derived(base, device);
      final d2 = HlcClock.derived(d1, device);
      final d3 = HlcClock.derived(d2, device);
      expect(d1 < d2, isTrue);
      expect(d2 < d3, isTrue, reason: 'each re-derivation replicates');
      final heldWall = Hlc(wallMs: 1000, counter: 6, node: '0' * 26);
      for (final d in [d1, d2, d3]) {
        expect(
          d < heldWall,
          isTrue,
          reason: 'chained bookkeeping must never beat user intent',
        );
      }
    });

    test('keeps devices distinct so derived-vs-derived orders '
        'deterministically', () {
      final a = HlcClock.derived(base, 'DEVICEA0000000000000000000');
      final b = HlcClock.derived(base, 'DEVICEB0000000000000000000');
      expect(a == b, isFalse);
      expect(a.compareTo(b) == 0, isFalse);
    });

    test('round-trips through packed parse like any node', () {
      final d = HlcClock.derived(
        HlcClock.derived(base, device),
        '01J9Z3KQ8V5N7P2R4T6W8XABCD',
      );
      expect(Hlc.parse(d.packed), d);
    });

    test('at the chain ceiling it stays in NODE space instead of bumping the '
        'counter past an honest same-millisecond write', () {
      var d = base;
      while (d.node.length + device.length + 1 <= HlcClock.maxNodeLength) {
        d = HlcClock.derived(d, device);
        expect(d.counter, base.counter);
      }
      final over = HlcClock.derived(d, device);

      // A counter bump would leave node space entirely and outrank every
      // same-millisecond honest write — the exact tie the extension scheme
      // exists to avoid.
      expect(over.counter, base.counter, reason: 'no counter bump');
      expect(over.wallMs, base.wallMs);
      expect(over.node.length <= HlcClock.maxNodeLength, isTrue);
      // Still strictly above its base…
      expect(over.compareTo(d) > 0, isTrue);
      // …and still strictly below a real next edit at the same wall clock.
      final realEdit = Hlc(
        wallMs: base.wallMs,
        counter: base.counter + 1,
        node: device,
      );
      expect(over.compareTo(realEdit) < 0, isTrue);
    });

    test('a further derivation at the ceiling keeps advancing', () {
      var d = base;
      while (d.node.length + device.length + 1 <= HlcClock.maxNodeLength) {
        d = HlcClock.derived(d, device);
      }
      final first = HlcClock.derived(d, device);
      final second = HlcClock.derived(first, device);
      expect(second.compareTo(first) > 0, isTrue);
      expect(second.counter, base.counter);
    });

    // The wire grammar (SyncRow.checkHlcConsistency) admits a node of exactly
    // maxNodeSegments segments whose characters sit anywhere in the node
    // alphabet — including its top character, which minted ids (Crockford
    // ULIDs) never contain. A hostile stamp can therefore hand [derived] a
    // ceiling-length base whose final segment cannot be advanced; deriving
    // from it must still produce a stamp, not throw (a throw here permanently
    // disabled every pull run's end-of-run integrity repair).
    group('hostile ceiling nodes (un-advanceable final segment)', () {
      const top = 'z'; // last character of the node alphabet
      String seg(String c) => c * HlcClock.nodeSegmentLength;
      String chain(String head, String tail, int tailSegments) => [
        seg(head),
        for (var i = 0; i < tailSegments; i++) seg(tail),
      ].join('!');

      test('an exhausted FINAL segment carries into the previous segment, '
          'staying in node space at the same (wall, counter)', () {
        final node = chain('A', top, HlcClock.maxNodeSegments - 1);
        expect(node.length, HlcClock.maxNodeLength, reason: 'fixture sanity');
        final base = Hlc(wallMs: 1000, counter: 5, node: node);

        final d = HlcClock.derived(base, device);
        expect(d > base, isTrue, reason: 'must replicate over the base state');
        expect(d.wallMs, base.wallMs);
        expect(d.counter, base.counter, reason: 'never escalates the counter');
        expect(d.node.length, HlcClock.maxNodeLength);
        // The carry treats the whole chain as one odometer: the exhausted
        // trailing segments reset to the alphabet's first character and the
        // first advanceable character bumps.
        expect(
          d.node,
          chain('A', '0', 13).replaceFirst(seg('A'), '${'A' * 25}B'),
        );
        // Still strictly below a real next edit at the same wall clock.
        final realEdit = Hlc(
          wallMs: base.wallMs,
          counter: base.counter + 1,
          node: device,
        );
        expect(d < realEdit, isTrue);
        expect(Hlc.parse(d.packed), d, reason: 'round-trips like any stamp');
      });

      test('a chain of derivations from an exhausted-final-segment base keeps '
          'advancing below the next real edit', () {
        final base = Hlc(
          wallMs: 1000,
          counter: 5,
          node: chain('A', top, HlcClock.maxNodeSegments - 1),
        );
        final first = HlcClock.derived(base, device);
        final second = HlcClock.derived(first, device);
        expect(first > base, isTrue);
        expect(second > first, isTrue);
        final realEdit = Hlc(wallMs: 1000, counter: 6, node: device);
        expect(second < realEdit, isTrue);
      });

      test('the absolute TOP of node space leaves node space minimally: '
          'counter + 1 under the deriving device node', () {
        // The plan-of-record hostile input: every segment at the alphabet top.
        final base = Hlc(
          wallMs: 1000,
          counter: 5,
          node: chain(top, top, HlcClock.maxNodeSegments - 1),
        );
        final d = HlcClock.derived(base, device);
        expect(d > base, isTrue, reason: 'must replicate over the base state');
        expect(d.wallMs, base.wallMs);
        expect(d.counter, base.counter + 1, reason: 'the minimal escalation');
        expect(d.node, device, reason: 'distinct per deriving device again');
        // Bounded: still below everything two counters up.
        final laterEdit = Hlc(wallMs: 1000, counter: 7, node: '0' * 26);
        expect(d < laterEdit, isTrue);
        expect(Hlc.parse(d.packed), d);
      });

      test('the top-of-node-space spill respects the 6-digit counter cap '
          '(spills to the next millisecond like tick/receive)', () {
        final base = Hlc(
          wallMs: 1000,
          counter: HlcClock.maxCounter,
          node: chain(top, top, HlcClock.maxNodeSegments - 1),
        );
        final d = HlcClock.derived(base, device);
        expect(d > base, isTrue);
        expect(d.wallMs, base.wallMs + 1);
        expect(d.counter, 0);
        expect(
          base.packed.compareTo(d.packed) < 0,
          isTrue,
          reason: 'packed ordering stays monotonic across the spill',
        );
      });

      test('a derivation from the spilled stamp goes back to plain node '
          'extension (the chain restarts from the device segment)', () {
        final base = Hlc(
          wallMs: 1000,
          counter: 5,
          node: chain(top, top, HlcClock.maxNodeSegments - 1),
        );
        final spilled = HlcClock.derived(base, device);
        final next = HlcClock.derived(spilled, device);
        expect(next > spilled, isTrue);
        expect(next.counter, spilled.counter, reason: 'extension, not a bump');
        expect(next.node, '${spilled.node}!$device');
      });
    });
  });

  group('HlcClock.tick (local writes)', () {
    test(
      'counter increments within a millisecond, resets when wall advances',
      () {
        var now = 1000;
        final clock = HlcClock(node: 'N', now: () => now);
        expect(clock.tick(), Hlc(wallMs: 1000, counter: 0, node: 'N'));
        expect(clock.tick(), Hlc(wallMs: 1000, counter: 1, node: 'N'));
        expect(clock.tick(), Hlc(wallMs: 1000, counter: 2, node: 'N'));
        now = 1001;
        expect(clock.tick(), Hlc(wallMs: 1001, counter: 0, node: 'N'));
      },
    );

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

    test('a once-future clock stays strictly monotonic on tick (never smaller)', () {
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
      final merged = clock.receive(
        Hlc(wallMs: trueNow + 1000, counter: 0, node: 'M'),
      );
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

  group('HlcClock.assertPlausible (check-only drift gate)', () {
    test('accepts and rejects on exactly the same boundary as receive, '
        'without mutating any clock state', () {
      const maxDrift = 60000;
      Hlc remote(int wall) => Hlc(wallMs: wall, counter: 0, node: 'M');

      // Two identical clocks: one probed check-only, one driven by receive —
      // the drift verdicts must match on both sides of the boundary.
      final probe = HlcClock(node: 'N', now: () => 1000, maxDriftMs: maxDrift);
      final mutate = HlcClock(node: 'N', now: () => 1000, maxDriftMs: maxDrift);

      // At the boundary: both accept.
      final atEdge = remote(1000 + maxDrift);
      expect(() => probe.assertPlausible(atEdge), returnsNormally);
      expect(() => mutate.receive(atEdge), returnsNormally);
      // One past the boundary: both refuse. (mutate advanced to the edge
      // stamp above, raising maxWallEverSeen — probe the SAME reference by
      // judging a stamp one past drift from that raised reference.)
      expect(
        () => probe.assertPlausible(remote(1000 + maxDrift + 1)),
        throwsA(isA<HlcDriftException>()),
      );
      expect(
        () => mutate.receive(remote(1000 + maxDrift + maxDrift + 1)),
        throwsA(isA<HlcDriftException>()),
      );

      // The check-only clock is bit-for-bit where it started: neither the
      // accepted nor the refused probe moved last or maxWallEverSeen.
      expect(probe.last, Hlc(wallMs: 0, counter: 0, node: 'N'));
      expect(probe.maxWallEverSeen, 0);
      // A subsequent tick behaves as if the probes never happened.
      expect(probe.tick(), Hlc(wallMs: 1000, counter: 0, node: 'N'));
    });

    test('judges against max(now, maxWallEverSeen) exactly like receive '
        '(a slow local clock still accepts honest remotes)', () {
      const trueNow = 10 * 60 * 1000;
      // Local clock 10 minutes behind real time; the persisted last stamp
      // anchors maxWallEverSeen near real time (same setup as the receive
      // test above).
      final clock = HlcClock(
        node: 'N',
        now: () => 1000,
        initial: Hlc(wallMs: trueNow, counter: 0, node: 'N'),
      );
      final honest = Hlc(wallMs: trueNow + 1000, counter: 0, node: 'M');
      expect(() => clock.assertPlausible(honest), returnsNormally);
      // And the plausible-but-unreceived stamp changed nothing.
      expect(clock.last.wallMs, trueNow);
      expect(clock.maxWallEverSeen, trueNow);
      // receive agrees with the check-only verdict.
      expect(() => clock.receive(honest), returnsNormally);
    });
  });

  group('HlcClock.mergeVerified (post-commit advance, no drift re-check)', () {
    test('advances exactly like receive for a plausible stamp — same result, '
        'same clock state, same behavior afterwards', () {
      final viaMerge = HlcClock(node: 'N', now: () => 1000);
      final viaReceive = HlcClock(node: 'N', now: () => 1000);
      viaMerge.tick();
      viaReceive.tick();
      final remote = Hlc(wallMs: 1005, counter: 3, node: 'M');
      expect(viaMerge.mergeVerified(remote), viaReceive.receive(remote));
      expect(viaMerge.last, viaReceive.last);
      expect(viaMerge.maxWallEverSeen, viaReceive.maxWallEverSeen);
      // The two clocks keep agreeing: the shared advance left identical state.
      expect(viaMerge.tick(), viaReceive.tick());
      final again = Hlc(wallMs: 1005, counter: 9, node: 'M');
      expect(viaMerge.mergeVerified(again), viaReceive.receive(again));
    });

    test('accepts a stamp receive would reject for drift, advances '
        'monotonically past it, and later ticks stay uncorrupted', () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now, maxDriftMs: 60000);
      final before = clock.tick(); // 1000/0
      // Far beyond the drift bound: receive refuses (and mutates nothing)...
      final vetted = Hlc(wallMs: 1000 + 60000 + 5000, counter: 2, node: 'M');
      expect(() => clock.receive(vetted), throwsA(isA<HlcDriftException>()));
      expect(clock.last, before);
      // ...but mergeVerified — for a stamp the caller already gated — merges
      // per the canonical receive rule.
      final merged = clock.mergeVerified(vetted);
      expect(merged > before, isTrue);
      expect(merged > vetted, isTrue, reason: 'strictly after the remote');
      expect(merged.wallMs, vetted.wallMs);
      expect(merged.counter, vetted.counter + 1);
      expect(clock.maxWallEverSeen, vetted.wallMs);
      // Later ticks are strictly monotonic on top of the merged stamp (the
      // held-wall degradation, exactly as after any future-ward receive).
      final t1 = clock.tick();
      expect(t1 > merged, isTrue);
      expect(t1.packed.length, merged.packed.length);
      now = 2000;
      final t2 = clock.tick();
      expect(t2 > t1, isTrue);
    });

    test('a stamp vetted by assertPlausible still merges after the wall clock '
        'rolls back past the drift bound (the post-commit case)', () {
      var now = 1000;
      final clock = HlcClock(node: 'N', now: () => now, maxDriftMs: 60000);
      final stamp = Hlc(wallMs: 1000 + 60000, counter: 0, node: 'M');
      // The gate passes at the boundary...
      expect(() => clock.assertPlausible(stamp), returnsNormally);
      // ...then the wall clock rolls BACK before the advance runs.
      // assertPlausible (check-only) never raised maxWallEverSeen, so the
      // drift reference shrank with it: re-running the check would now
      // reject the very stamp the gate admitted.
      now = 500;
      expect(() => clock.receive(stamp), throwsA(isA<HlcDriftException>()));
      final merged = clock.mergeVerified(stamp);
      expect(merged > stamp, isTrue);
      expect(clock.last, merged);
    });
  });

  group('counter cap (fixed 6-digit packed field)', () {
    test('tick spills into the next millisecond at counter 999999', () {
      // Hold the wall (now < last.wallMs) so every tick lands in the same
      // millisecond and drives the counter toward the cap.
      final clock = HlcClock(
        node: 'N',
        now: () => 1000,
        initial: Hlc(wallMs: 5000, counter: HlcClock.maxCounter - 1, node: 'N'),
      );
      final atCap = clock.tick();
      expect(atCap, Hlc(wallMs: 5000, counter: HlcClock.maxCounter, node: 'N'));

      // The next tick would be counter 1_000_000 — 7 digits, which sorts
      // lexicographically BELOW '999999'. It must spill to wallMs+1 instead.
      final spilled = clock.tick();
      expect(spilled, Hlc(wallMs: 5001, counter: 0, node: 'N'));
      expect(spilled > atCap, isTrue);
      expect(spilled.packed.length, atCap.packed.length); // width never grows
      expect(
        atCap.packed.compareTo(spilled.packed) < 0,
        isTrue,
        reason: 'packed ordering stays monotonic across the spill',
      );

      // And keeps ticking normally after the spill.
      final next = clock.tick();
      expect(next, Hlc(wallMs: 5001, counter: 1, node: 'N'));
      expect(spilled.packed.compareTo(next.packed) < 0, isTrue);
    });

    test('receive spills instead of overflowing the 6-digit counter', () {
      final local = Hlc(wallMs: 1000, counter: HlcClock.maxCounter, node: 'N');
      final clock = HlcClock(node: 'N', now: () => 1000, initial: local);
      // Equal walls take max(counter)+1, which would overflow the field.
      final merged = clock.receive(
        Hlc(wallMs: 1000, counter: HlcClock.maxCounter, node: 'M'),
      );
      expect(merged, Hlc(wallMs: 1001, counter: 0, node: 'N'));
      expect(merged > local, isTrue);
      expect(
        local.packed.compareTo(merged.packed) < 0,
        isTrue,
        reason: 'packed ordering stays monotonic across the spill',
      );
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
