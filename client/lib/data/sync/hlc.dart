import 'package:flutter/foundation.dart';

/// A Hybrid Logical Clock timestamp: a physical wall-clock value (ms) plus a
/// logical [counter] that orders events within the same millisecond, plus a
/// stable per-install [node] id as the final tiebreak.
///
/// HLCs are TOTALLY ordered, which is what makes entity-level Last-Writer-Wins
/// deterministic: two devices that have seen the same set of writes resolve any
/// row to the identical winner, so they always converge — no split brain.
@immutable
class Hlc implements Comparable<Hlc> {
  const Hlc({required this.wallMs, required this.counter, required this.node});

  /// The HLC's physical component (epoch milliseconds).
  final int wallMs;

  /// Logical counter, bumped when two events share a [wallMs].
  final int counter;

  /// Stable id of the install that produced this stamp (a ULID minted once).
  final String node;

  /// A fixed-width, lexicographically comparable encoding whose string order is
  /// exactly the tuple order — stored next to each row as the hot-path LWW key
  /// so a conflict is a single `packed < packed` comparison. The counter field
  /// is fixed at 6 digits: [HlcClock] never emits a counter above
  /// [HlcClock.maxCounter] (it spills into the next millisecond instead), so
  /// the width — and therefore the lexicographic ordering — can never break.
  String get packed =>
      '${wallMs.toString().padLeft(13, '0')}-'
      '${counter.toString().padLeft(6, '0')}-'
      '$node';

  /// Parses a [packed] encoding back into an [Hlc]. The node is a ULID (no
  /// dashes), so only the first two dashes are separators.
  factory Hlc.parse(String packed) {
    final i1 = packed.indexOf('-');
    final i2 = packed.indexOf('-', i1 + 1);
    if (i1 < 0 || i2 < 0) {
      throw FormatException('Not a packed HLC: "$packed"');
    }
    return Hlc(
      wallMs: int.parse(packed.substring(0, i1)),
      counter: int.parse(packed.substring(i1 + 1, i2)),
      node: packed.substring(i2 + 1),
    );
  }

  @override
  int compareTo(Hlc other) {
    if (wallMs != other.wallMs) return wallMs.compareTo(other.wallMs);
    if (counter != other.counter) return counter.compareTo(other.counter);
    return node.compareTo(other.node);
  }

  bool operator >(Hlc other) => compareTo(other) > 0;
  bool operator >=(Hlc other) => compareTo(other) >= 0;
  bool operator <(Hlc other) => compareTo(other) < 0;
  bool operator <=(Hlc other) => compareTo(other) <= 0;

  @override
  bool operator ==(Object other) =>
      other is Hlc &&
      other.wallMs == wallMs &&
      other.counter == counter &&
      other.node == node;

  @override
  int get hashCode => Object.hash(wallMs, counter, node);

  @override
  String toString() => 'Hlc($packed)';
}

/// Thrown when an inbound (remote) HLC is implausibly far in the future, beyond
/// the clock's drift bound. The offending record is quarantined rather than
/// merged, so a single device with a bad clock cannot ratchet the whole fleet's
/// clocks forward (which would let ancient future-stamped data win LWW for
/// years).
class HlcDriftException implements Exception {
  HlcDriftException({
    required this.remote,
    required this.nowMs,
    required this.maxDriftMs,
  });

  final Hlc remote;
  final int nowMs;
  final int maxDriftMs;

  @override
  String toString() =>
      'HlcDriftException(remote=${remote.packed} is '
      '${remote.wallMs - nowMs}ms ahead of now, > maxDrift ${maxDriftMs}ms)';
}

/// A device's Hybrid Logical Clock. The SINGLE producer of HLC stamps for local
/// writes ([tick]) and the only place that advances when a remote stamp is seen
/// ([receive], or [mergeVerified] for stamps already gated by
/// [assertPlausible]). Inject [now] in tests for determinism.
class HlcClock {
  HlcClock({
    required this.node,
    int Function()? now,
    this.maxDriftMs = defaultMaxDriftMs,
    Hlc? initial,
  }) : _now = now ?? (() => DateTime.now().millisecondsSinceEpoch),
       _last = initial ?? Hlc(wallMs: 0, counter: 0, node: node) {
    _maxWallEverSeen = _last.wallMs;
  }

  /// 5 minutes. Generous enough to absorb normal cross-device clock skew, tight
  /// enough that a grossly-wrong (e.g. far-future) clock is rejected. A long
  /// OFFLINE gap is unaffected — the wall clock advances normally while offline,
  /// so a clock that was correct months ago arrives with a PAST wall, never a
  /// future one.
  static const int defaultMaxDriftMs = 5 * 60 * 1000;

  /// The highest counter the fixed 6-digit [Hlc.packed] field can carry.
  /// [tick] and [receive] spill into the next millisecond instead of exceeding
  /// it — a 7-digit counter would sort lexicographically BELOW 6-digit ones,
  /// making the string MAX pick the wrong winner under LWW.
  static const int maxCounter = 999999;

  /// Stable per-install node id (final LWW tiebreak).
  final String node;

  /// The stamp a DERIVED write (integrity repair, slot-resolution tombstone,
  /// FK-sweep collision restore) mints from the row state [base] it derives
  /// from: the SAME (wall, counter), with the node extended by
  /// `'!' + deviceNode`. String order makes a prefix extension sort strictly
  /// AFTER the prefix, so the derived stamp out-ranks [base] (it replicates
  /// over every peer still holding base) while sorting strictly BELOW every
  /// stamp that out-ranks base — including a held-wall author edit at
  /// `(base.wall, base.counter + 1)` AND a node-space tie winner at
  /// `(base.wall, base.counter, biggerNode)`.
  ///
  /// Deriving in NODE space, never counter space, is the whole point — it
  /// closes both halves of the derived-stamp lost update:
  ///
  ///  * counter escalation: two chained repairs used to stamp base+1 then
  ///    base+2, and base+2-derived beat a base+1 REAL edit, silently
  ///    reverting it fleet-wide. Here a repair-of-a-repair just extends the
  ///    node again — `(w, c, A!D!D)` — and still loses to every real edit
  ///    above the ORIGINAL base. Bookkeeping can never out-rank user intent,
  ///    no matter how many times it re-derives.
  ///  * the restore collision: a parked update that won its slot by NODE
  ///    only — `(w, c, B)` over prior `(w, c, A)` — used to be beaten by the
  ///    restore's `(w, c + 1, …)` tombstone, so its quarantine replay lost
  ///    LWW forever. `(w, c, A!D)` sorts between A and B: the tombstone
  ///    replicates over the prior state AND the replay still lands.
  ///
  /// The device suffix keeps derived writes from DIFFERENT devices distinct
  /// (two devices deriving different content from the same base must not
  /// mint one stamp), and `'!'` (below the Crockford node alphabet) makes
  /// derived stamps recognizable and reserved on the wire.
  ///
  /// At the chain's ceiling ([maxNodeSegments] segments) the derivation stays
  /// in NODE space rather than bumping the counter. A counter bump leaves node
  /// space entirely, so the derived stamp outranks any same-millisecond honest
  /// write — exactly the tie the extension scheme exists to avoid — and the
  /// wire's node grammar makes the ceiling unreachable without a chain of real
  /// derivations anyway. Advancing the node keeps the stamp strictly above its
  /// base and strictly below the next real edit, which is the whole contract.
  ///
  /// The one exception: the wire grammar also admits a ceiling node whose
  /// characters sit at the TOP of the alphabet (minted ids are Crockford, so
  /// only a hostile stamp reaches it) — at the absolute top of node space no
  /// stamp exists between the base and (wall, counter + 1), so the derivation
  /// must leave node space after all. It does so MINIMALLY, one counter up
  /// (spilling to the next millisecond at [maxCounter], like [tick]) under
  /// [deviceNode]. That stamp can win a node tiebreak against a real edit at
  /// the very same (wall, counter + 1) — accepted: the input requires a
  /// hostile ceiling-length stamp, and refusing to derive would instead
  /// permanently fail every caller (integrity repair, slot resolution) for
  /// the row carrying it. Total by design: derived() never throws.
  static Hlc derived(Hlc base, String deviceNode) {
    final extended = '${base.node}!$deviceNode';
    if (extended.length <= maxNodeLength) {
      return Hlc(wallMs: base.wallMs, counter: base.counter, node: extended);
    }
    final advanced = _advanceNode(base.node);
    if (advanced != null) {
      return Hlc(wallMs: base.wallMs, counter: base.counter, node: advanced);
    }
    // Node space above the base is empty: the minimal strictly-greater stamp
    // lives one counter up. Same spill rule as tick/_advance at the cap.
    var wall = base.wallMs;
    var counter = base.counter + 1;
    if (counter > maxCounter) {
      wall += 1;
      counter = 0;
    }
    return Hlc(wallMs: wall, counter: counter, node: deviceNode);
  }

  /// [node] replaced by its successor among nodes of the same segment shape,
  /// or null when every character is already at the top of the alphabet. The
  /// whole chain is one odometer: a carry out of the final segment advances
  /// the previous one ('!' separators stay fixed in place). [_nodeAlphabet]
  /// is listed in code-unit order AND the wire grammar admits nothing outside
  /// it, so alphabet order is string order here; within the final segment no
  /// legal node sits between a node and its successor, and once a carry
  /// crosses a segment boundary only a strictly shorter chain that prefixes
  /// the successor can — an ordering the hostile-only ceiling accepts.
  static String? _advanceNode(String node) {
    final chars = node.split('');
    for (var i = chars.length - 1; i >= 0; i--) {
      if (chars[i] == '!') continue; // segment separator: fixed in place
      final next = _nodeAlphabet.indexOf(chars[i]) + 1;
      // Outside the alphabet (never stored past the wire gate, but derived()
      // is total): un-advanceable in place, keep carrying left.
      if (next == 0) continue;
      if (next < _nodeAlphabet.length) {
        chars[i] = _nodeAlphabet[next];
        return chars.join();
      }
      chars[i] = _nodeAlphabet[0]; // carry one character left
    }
    return null; // every position exhausted: node space above is empty
  }

  static const String _nodeAlphabet =
      '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';

  /// One node segment: a device id is a 26-character ULID, and every
  /// derivation appends exactly one more.
  static const int nodeSegmentLength = 26;

  /// Longest derivation CHAIN a node may carry: the minting device plus 13
  /// derived writes. Far beyond any real chain — the integrity fixpoint caps
  /// at 8 passes per run — and bounded so a hostile stamp cannot bloat every
  /// packed key it touches.
  static const int maxNodeSegments = 14;

  /// Longest node the wire accepts (see [SyncRow.checkHlcConsistency]) and
  /// the ceiling [derived] extends under: [maxNodeSegments] segments joined
  /// by '!'.
  static const int maxNodeLength =
      nodeSegmentLength * maxNodeSegments + (maxNodeSegments - 1);

  /// Maximum tolerated forward drift of a remote (or the local) clock.
  final int maxDriftMs;

  final int Function() _now;
  Hlc _last;
  int _maxWallEverSeen = 0;

  /// The most recent stamp this clock produced or merged.
  Hlc get last => _last;

  /// The highest wall ever observed (kept for forensics / drift diagnostics).
  int get maxWallEverSeen => _maxWallEverSeen;

  /// Stamps a LOCAL write. Standard, strictly-monotonic HLC advance: the wall
  /// never moves backward, so every stamp is greater than the previous one.
  ///
  /// We deliberately do NOT reset the wall back to real time on forward drift.
  /// That reset emitted an `Hlc(now, 0)` that compared SMALLER than the device's
  /// own earlier (skewed-forward) stamps, so — under entity-level LWW — the newest
  /// local edit silently lost to an older one, and a just-undeleted row could stay
  /// tombstoned. Monotonicity is the stronger invariant. If a corrected clock
  /// leaves us ahead of real time, the wall simply holds (counter keeps
  /// incrementing) until real time catches up: local edits still win over our own
  /// prior ones, and any stamp too far ahead of a peer is quarantined by that
  /// peer's [receive] drift gate — a visible, self-healing degradation rather than
  /// silent local data loss.
  Hlc tick() {
    final now = _now();
    var wall = now > _last.wallMs ? now : _last.wallMs;
    var counter = (wall == _last.wallMs) ? _last.counter + 1 : 0;
    if (counter > maxCounter) {
      // The 6-digit packed counter is exhausted within this millisecond: spill
      // into the next one. Same hold-the-wall degradation as a forward clock
      // excursion — still strictly monotonic, and real time catches up.
      wall += 1;
      counter = 0;
    }
    _last = Hlc(wallMs: wall, counter: counter, node: node);
    _track(wall);
    return _last;
  }

  /// Judges [remote] against the drift rule WITHOUT advancing the clock:
  /// throws [HlcDriftException] exactly when [receive] would, and mutates
  /// nothing. This exists so the [SyncApplier] can GATE a record before its
  /// savepoint while deferring the state-changing [receive] until the record
  /// has settled AND its batch's outer transaction has committed — before
  /// the split, a record that failed (or was quarantined) inside its
  /// savepoint had already advanced the in-memory clock, so a rolled-back
  /// record still left a side effect behind; the same holds for a whole
  /// batch whose outer commit fails. Both methods share [_checkDrift], so
  /// the two rules can never diverge.
  void assertPlausible(Hlc remote) => _checkDrift(remote, _now());

  /// Merges a [remote] stamp seen on an inbound record, advancing the local
  /// clock per the canonical HLC receive rule so subsequent local stamps stay
  /// causally after anything we have observed. Returns the new local stamp.
  ///
  /// Refuses a remote whose wall is more than [maxDriftMs] in the future
  /// ([HlcDriftException]) so one device's bad clock can't contaminate ours. A
  /// remote in the past is fine (it simply doesn't advance us). The check-only
  /// form of the same rule is [assertPlausible]; the advance WITHOUT the
  /// drift check is [mergeVerified].
  Hlc receive(Hlc remote) {
    final now = _now();
    _checkDrift(remote, now);
    return _advance(remote, now);
  }

  /// Merges a [remote] stamp that ALREADY passed [assertPlausible] earlier in
  /// the same batch: the exact advance [receive] performs, with NO drift
  /// re-check. Never call this on a stamp that has not been gated.
  ///
  /// This exists for the [SyncApplier]'s deferred post-commit advance. Each
  /// record is gated with [assertPlausible] before its savepoint, but the
  /// state-changing merge runs only after the batch's outer transaction has
  /// committed — and between the two, the wall clock can roll BACKWARD.
  /// [assertPlausible] never raises [maxWallEverSeen] (it is check-only by
  /// contract), so the drift reference `max(now, maxWallEverSeen)` can
  /// SHRINK across that window, and re-running the check in [receive] would
  /// then throw for a stamp whose data and change token are already durable:
  /// the run reports failure for a committed page. Clamping the remote stamp
  /// instead was considered and rejected — it would corrupt causal order.
  /// Both methods share [_advance], so the two merges can never diverge.
  Hlc mergeVerified(Hlc remote) => _advance(remote, _now());

  /// The shared advance rule behind [receive] and [mergeVerified]: adopt the
  /// max wall of (now, ours, remote) and pick the counter that keeps the new
  /// stamp strictly after both our last stamp and the remote one.
  Hlc _advance(Hlc remote, int now) {
    var wall = [
      now,
      _last.wallMs,
      remote.wallMs,
    ].reduce((a, b) => a > b ? a : b);
    int counter;
    if (wall == _last.wallMs && wall == remote.wallMs) {
      counter =
          (_last.counter > remote.counter ? _last.counter : remote.counter) + 1;
    } else if (wall == _last.wallMs) {
      counter = _last.counter + 1;
    } else if (wall == remote.wallMs) {
      counter = remote.counter + 1;
    } else {
      counter = 0;
    }
    if (counter > maxCounter) {
      // Keep the packed encoding's fixed 6-digit counter: spill to the next
      // millisecond (still strictly after both _last and remote).
      wall += 1;
      counter = 0;
    }
    _last = Hlc(wallMs: wall, counter: counter, node: node);
    _track(wall);
    return _last;
  }

  /// The shared drift rule behind [receive] and [assertPlausible]. Judge
  /// "implausibly far in the future" against the best estimate of true time
  /// we have: the later of our local clock and the highest wall we have ever
  /// seen from an honest peer. Gating on the local clock alone meant that if
  /// OUR clock ran slow by more than [maxDriftMs], every correctly-stamped
  /// remote looked far-future and ALL inbound sync was quarantined. Using
  /// max(now, seen) keeps the gate working through a slow local clock while
  /// still rejecting a genuinely far-future remote clock.
  void _checkDrift(Hlc remote, int now) {
    final reference = now > _maxWallEverSeen ? now : _maxWallEverSeen;
    if (remote.wallMs - reference > maxDriftMs) {
      throw HlcDriftException(
        remote: remote,
        nowMs: reference,
        maxDriftMs: maxDriftMs,
      );
    }
  }

  void _track(int wall) {
    if (wall > _maxWallEverSeen) _maxWallEverSeen = wall;
  }
}
