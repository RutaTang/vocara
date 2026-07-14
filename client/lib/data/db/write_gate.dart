import 'dart:async';

/// The app-wide store write gate: a hand-rolled shared/exclusive async mutex
/// (deliberately no package dependency) serializing ordinary WRITERS against
/// store-wide MAINTENANCE.
///
/// Why it exists: a save's import+commit sequence is not atomic — the asset
/// row/pool file land first (AssetStore.importFile), the clip/media rows that
/// reference them commit later, with awaits in between. A GC pass running in
/// that window (repairLibrary between user actions) observes the asset as
/// unreferenced and tombstones it, or trashes the freshly placed pool file /
/// in-flight `.part`. The gate closes that window structurally:
///
///  * [shared] — every save/import path (the repositories' save cores, and
///    AssetStore.importFile itself as a belt-and-braces for any other caller)
///    runs under the shared side; any number may overlap.
///  * [exclusive] — repairLibrary and the boot sweeps run exclusively: they
///    wait for every in-flight shared section to finish and hold off new ones
///    while they run.
///
/// Reader preference, by design: a NEW shared section only waits for a
/// RUNNING exclusive, never a queued one. That makes nested shared
/// acquisition safe (a save holding the gate while importFile re-enters it
/// can never deadlock behind a queued repair). The cost is theoretical writer
/// starvation under a continuous stream of saves — acceptable because
/// maintenance is rare and user-triggered while saves are short.
///
/// NOT re-entrant on the exclusive side: an exclusive section must never call
/// [exclusive] (or [shared]) again — Persistence keeps repair/sweep internals
/// gate-free for exactly that reason.
class WriteGate {
  /// Number of shared sections currently running.
  int _shared = 0;

  /// Completes when [_shared] drains to zero (recreated per waiter round).
  Completer<void>? _drained;

  /// Serializes exclusive sections among themselves.
  Future<void> _exclusiveChain = Future.value();

  /// Whether an exclusive section is RUNNING right now (not merely queued).
  bool _exclusiveRunning = false;

  /// Completes when the currently running exclusive section finishes.
  Completer<void>? _exclusiveDone;

  /// Runs [op] under the shared side of the gate. Overlaps freely with other
  /// shared sections (including nested ones); waits only while an exclusive
  /// section is actively running.
  Future<T> shared<T>(Future<T> Function() op) async {
    while (_exclusiveRunning) {
      await _exclusiveDone!.future;
    }
    _shared++;
    try {
      return await op();
    } finally {
      _shared--;
      if (_shared == 0) {
        _drained?.complete();
        _drained = null;
      }
    }
  }

  /// Runs [op] exclusively: after every in-flight shared section has
  /// finished, with new shared sections held off until [op] completes.
  /// Exclusive sections are serialized among themselves in call order.
  Future<T> exclusive<T>(Future<T> Function() op) {
    final run = _exclusiveChain.then((_) async {
      // Wait for the shared side to drain. Re-check after every wake-up: a
      // shared section may have slipped in while this one was waking (reader
      // preference admits it), so only a synchronous zero-count observation
      // may proceed.
      while (_shared > 0) {
        _drained ??= Completer<void>();
        await _drained!.future;
      }
      _exclusiveRunning = true;
      _exclusiveDone = Completer<void>();
      try {
        return await op();
      } finally {
        _exclusiveRunning = false;
        _exclusiveDone!.complete();
      }
    });
    // The chain must survive a failed run; callers see the error via [run].
    _exclusiveChain = run.then((_) {}, onError: (_) {});
    return run;
  }
}
