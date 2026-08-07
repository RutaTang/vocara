/// A byte-budgeted LRU cache with a plain-Map surface, for analysis results
/// whose per-entry size is dominated by typed-data payloads.
///
/// The per-path analysis caches used to grow without bound: every take,
/// lane aggregate, and EQ spectrogram ever opened in a session stayed
/// resident. Entries here count toward a budget via [byteOf]; inserting
/// past it evicts least-recently-USED entries (reads refresh recency, so the
/// currently displayed subject is always the most recent and never evicted
/// under it). The newest entry is kept even when it alone exceeds the budget
/// — an oversized result is better served once than recomputed forever.
///
/// A cache either owns its budget (the plain constructor) or shares one with
/// its siblings ([LruBytesCache.shared]). Sharing is what makes the process's
/// analysis footprint ONE number instead of the sum of several independent
/// ceilings, and it lets the screen the user left pay for the one they are
/// on: eviction always takes the globally least-recently-used entry, whichever
/// cache holds it.
library;

/// A byte budget several [LruBytesCache]es charge against together.
///
/// It owns the ceiling, the running total, and the single recency clock that
/// makes "least recently used" comparable ACROSS its members — without one,
/// each cache could only rank its own entries and the budget would have to
/// pick a victim cache arbitrarily.
class LruBytesBudget {
  LruBytesBudget(this.maxBytes) : assert(maxBytes > 0, 'a budget must be > 0');

  final int maxBytes;

  final List<LruBytesCache<Object?>> _members = [];
  int _bytes = 0;
  int _clock = 0;

  /// Current tracked payload bytes across every member (tests/diagnostics).
  int get bytes => _bytes;

  /// Members currently charging against this budget (tests/diagnostics).
  int get memberCount => _members.length;

  int _tick() => ++_clock;

  void _join(LruBytesCache<Object?> cache) => _members.add(cache);

  void _leave(LruBytesCache<Object?> cache) {
    if (_members.remove(cache)) _bytes -= cache.bytes;
  }

  /// Evict globally-oldest entries until the total fits, keeping at least one
  /// entry alive so a lone oversized result is still served (see the class
  /// doc). Called after any member's insert.
  void _trim() {
    while (_bytes > maxBytes) {
      LruBytesCache<Object?>? victim;
      var oldest = 0;
      for (final m in _members) {
        final tick = m._oldestTick;
        if (tick == null) continue;
        if (victim == null || tick < oldest) {
          victim = m;
          oldest = tick;
        }
      }
      // Nothing left to give: every member is empty, or the only entries left
      // are the single ones their caches keep by contract.
      if (victim == null) return;
      if (_members.fold<int>(0, (n, m) => n + m.length) <= 1) return;
      victim._evictOldest();
    }
  }
}

/// One entry plus the recency stamp its budget ranks it by.
class _Entry<V> {
  _Entry(this.value, this.tick);

  final V value;
  int tick;
}

class LruBytesCache<V> {
  /// A cache with a budget of its own.
  LruBytesCache(int maxBytes, this.byteOf)
    : _budget = LruBytesBudget(maxBytes) {
    _budget._join(this as LruBytesCache<Object?>);
  }

  /// A cache charging against a budget it SHARES with its siblings. Call
  /// [dispose] when the owner goes away: a shared budget outlives any one
  /// screen, so a cache that never leaves keeps that screen's whole payload
  /// resident for the life of the process.
  LruBytesCache.shared(LruBytesBudget budget, this.byteOf) : _budget = budget {
    _budget._join(this as LruBytesCache<Object?>);
  }

  final LruBytesBudget _budget;
  final int Function(V) byteOf;

  /// Called with the key of every entry this cache drops — eviction, [remove]
  /// and [clear] alike. Anything memoized FROM an entry is keyed off the same
  /// key and has no owner once the entry is gone, so this is where those
  /// derivations are dropped. Assigned by the owner after construction (a
  /// hook usually closes over `this`).
  void Function(String key)? onEvict;

  /// The budget this cache charges against, so an owner can report the shared
  /// total (tests/diagnostics).
  LruBytesBudget get budget => _budget;

  // LinkedHashMap iteration order doubles as this cache's own recency order:
  // re-insertion on read moves an entry to the back (most recent).
  final Map<String, _Entry<V>> _map = {};
  int _bytes = 0;

  /// Current tracked payload bytes in THIS cache (for tests/diagnostics); see
  /// [budget] for the shared total.
  int get bytes => _bytes;

  int get length => _map.length;
  bool get isEmpty => _map.isEmpty;
  bool get isNotEmpty => _map.isNotEmpty;

  bool containsKey(String key) => _map.containsKey(key);

  /// The recency stamp of this cache's oldest entry, or null when empty. Only
  /// true because [operator []] re-inserts on read, which also moves the entry
  /// to the back of the iteration order.
  int? get _oldestTick => _map.isEmpty ? null : _map.values.first.tick;

  void _evictOldest() {
    if (_map.isEmpty) return;
    final key = _map.keys.first;
    final e = _map.remove(key)!;
    _bytes -= byteOf(e.value);
    _budget._bytes -= byteOf(e.value);
    onEvict?.call(key);
  }

  V? operator [](String key) {
    final e = _map.remove(key);
    if (e == null) return null;
    e.tick = _budget._tick();
    _map[key] = e; // refresh recency
    return e.value;
  }

  void operator []=(String key, V value) {
    final old = _map.remove(key);
    if (old != null) {
      _bytes -= byteOf(old.value);
      _budget._bytes -= byteOf(old.value);
    }
    _map[key] = _Entry(value, _budget._tick());
    _bytes += byteOf(value);
    _budget._bytes += byteOf(value);
    _budget._trim();
  }

  V? remove(String key) {
    final e = _map.remove(key);
    if (e == null) return null;
    _bytes -= byteOf(e.value);
    _budget._bytes -= byteOf(e.value);
    onEvict?.call(key);
    return e.value;
  }

  void clear() {
    final keys = _map.keys.toList();
    _map.clear();
    _budget._bytes -= _bytes;
    _bytes = 0;
    final hook = onEvict;
    if (hook != null) keys.forEach(hook);
  }

  /// Drops everything and leaves the shared budget. Idempotent.
  void dispose() {
    // Leave FIRST: the budget discounts what this cache still holds, so the
    // total stays honest whether or not the map was already emptied.
    _budget._leave(this as LruBytesCache<Object?>);
    _map.clear();
    _bytes = 0;
  }
}

/// The app-wide analysis budget every heavy result cache charges against —
/// the practice screen's per-clip spectrograms and metrics, its lane
/// aggregates and EQ display spectrograms, and the technique screen's
/// reference analyses.
///
/// ONE number for what analysis can hold, rather than a per-cache ceiling
/// each: the old independent budgets summed to more than half a gigabyte that
/// no single screen could ever use, while nothing stopped two screens' worth
/// of results from being resident at once. 384 MB comfortably holds a working
/// practice (a lane aggregate plus its clips, ~10 min at 214 bins is ~44 MB
/// of spectrogram each) and lets whatever the user left be the natural
/// eviction victim for what they are on.
final LruBytesBudget analysisCacheBudget = LruBytesBudget(384 * 1000 * 1000);
