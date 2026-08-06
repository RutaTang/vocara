/// A byte-budgeted LRU cache with a plain-Map surface, for analysis results
/// whose per-entry size is dominated by typed-data payloads.
///
/// The per-path analysis caches used to grow without bound: every take,
/// lane aggregate, and EQ spectrogram ever opened in a session stayed
/// resident. Entries here count toward [maxBytes] via [byteOf]; inserting
/// past the budget evicts least-recently-USED entries (reads refresh
/// recency, so the currently displayed subject is always the most recent
/// and never evicted under it). The newest entry is kept even when it
/// alone exceeds the budget — an oversized result is better served once
/// than recomputed forever.
class LruBytesCache<V> {
  LruBytesCache(this.maxBytes, this.byteOf)
    : assert(maxBytes > 0, 'a cache needs a budget');

  final int maxBytes;
  final int Function(V) byteOf;

  // LinkedHashMap iteration order doubles as the recency order: re-insertion
  // on read moves an entry to the back (most recent).
  final Map<String, V> _map = {};
  int _bytes = 0;

  /// Current tracked payload bytes (for tests/diagnostics).
  int get bytes => _bytes;

  int get length => _map.length;
  bool get isEmpty => _map.isEmpty;
  bool get isNotEmpty => _map.isNotEmpty;

  bool containsKey(String key) => _map.containsKey(key);

  V? operator [](String key) {
    final v = _map.remove(key);
    if (v == null) return null;
    _map[key] = v; // refresh recency
    return v;
  }

  void operator []=(String key, V value) {
    final old = _map.remove(key);
    if (old != null) _bytes -= byteOf(old);
    _map[key] = value;
    _bytes += byteOf(value);
    while (_bytes > maxBytes && _map.length > 1) {
      final oldest = _map.keys.first;
      final evicted = _map.remove(oldest) as V;
      _bytes -= byteOf(evicted);
    }
  }

  V? remove(String key) {
    final v = _map.remove(key);
    if (v != null) _bytes -= byteOf(v);
    return v;
  }

  void clear() {
    _map.clear();
    _bytes = 0;
  }
}
