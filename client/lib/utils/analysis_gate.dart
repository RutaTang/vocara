import 'dart:async';

/// App-wide throttle for heavy analysis computes.
///
/// One clip analysis legally peaks at hundreds of MB (10-minute cap, see
/// MediaAnalysisService); nothing used to stop different surfaces — a
/// selected take, a lane aggregate, a technique example — from launching
/// several at once and stacking those peaks. Every heavy compute funnels
/// through [run]; excess callers WAIT (FIFO) instead of stacking memory.
///
/// The limit of 2 bounds the worst legal stack at roughly one gigabyte
/// while keeping one background compute rolling behind the interactive one.
/// Playback staging decodes are deliberately NOT gated: they are
/// latency-critical, bounded by their own budgets, and starving playback
/// behind an analysis queue would be a worse trade.
class AnalysisGate {
  AnalysisGate(this.limit) : assert(limit > 0, 'a gate must admit someone');

  final int limit;
  int _running = 0;
  final List<Completer<void>> _waiters = [];

  /// Number of computes currently inside the gate (for tests/diagnostics).
  int get running => _running;

  /// Number of computes queued behind the gate (for tests/diagnostics).
  int get queued => _waiters.length;

  Future<T> run<T>(Future<T> Function() work) async {
    if (_running < limit) {
      _running++;
    } else {
      // Woken with the slot of the job that finished, never counted free in
      // between: counted free, a caller reaching here before the waiter
      // resumed took it, and the waiter queued again behind everyone after it
      // (a selected take waited out the lane merges queued after it).
      final c = Completer<void>();
      _waiters.add(c);
      await c.future;
    }
    try {
      return await work();
    } finally {
      if (_waiters.isNotEmpty) {
        _waiters.removeAt(0).complete();
      } else {
        _running--;
      }
    }
  }
}

/// The single shared gate every analysis service routes through.
final AnalysisGate analysisGate = AnalysisGate(2);
