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
  /// so a conflict is a single `packed < packed` comparison.
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
/// ([receive]). Inject [now] in tests for determinism.
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

  /// Stable per-install node id (final LWW tiebreak).
  final String node;

  /// Maximum tolerated forward drift of a remote (or the local) clock.
  final int maxDriftMs;

  final int Function() _now;
  Hlc _last;
  int _maxWallEverSeen = 0;

  /// The most recent stamp this clock produced or merged.
  Hlc get last => _last;

  /// The highest wall ever observed (kept for forensics / drift diagnostics).
  int get maxWallEverSeen => _maxWallEverSeen;

  /// Stamps a LOCAL write. Standard HLC advance with a bounded-drift clamp: if
  /// the last stamp is more than [maxDriftMs] ahead of wall time (a clock that
  /// was skewed forward and has since been corrected), distrust it and reset to
  /// wall time — otherwise a once-future clock would ratchet forward forever.
  Hlc tick() {
    final now = _now();
    final int wall;
    if (_last.wallMs - now > maxDriftMs) {
      wall = now;
    } else {
      wall = now > _last.wallMs ? now : _last.wallMs;
    }
    final counter = (wall == _last.wallMs) ? _last.counter + 1 : 0;
    _last = Hlc(wallMs: wall, counter: counter, node: node);
    _track(wall);
    return _last;
  }

  /// Merges a [remote] stamp seen on an inbound record, advancing the local
  /// clock per the canonical HLC receive rule so subsequent local stamps stay
  /// causally after anything we have observed. Returns the new local stamp.
  ///
  /// Refuses a remote whose wall is more than [maxDriftMs] in the future
  /// ([HlcDriftException]) so one device's bad clock can't contaminate ours. A
  /// remote in the past is fine (it simply doesn't advance us).
  Hlc receive(Hlc remote) {
    final now = _now();
    if (remote.wallMs - now > maxDriftMs) {
      throw HlcDriftException(
        remote: remote,
        nowMs: now,
        maxDriftMs: maxDriftMs,
      );
    }
    final wall = [
      now,
      _last.wallMs,
      remote.wallMs,
    ].reduce((a, b) => a > b ? a : b);
    final int counter;
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
    _last = Hlc(wallMs: wall, counter: counter, node: node);
    _track(wall);
    return _last;
  }

  void _track(int wall) {
    if (wall > _maxWallEverSeen) _maxWallEverSeen = wall;
  }
}
