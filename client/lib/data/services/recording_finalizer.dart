import 'dart:async';

import 'package:flutter/foundation.dart';

import 'diagnostics_log.dart';

/// Process-wide registry of in-flight recordings so the app can FINALIZE them
/// (stop + analyze + save the captured take) before the process is torn down
/// on quit. Without this, quitting mid-record loses the take as a 0-length
/// row + a header-broken WAV — the finalize only runs on the normal stop path.
///
/// Quit paths covered (both wired in `main.dart`):
///  * red close button — window_manager's preventClose defers the close to
///    `_QuitGuard.onWindowClose`, which runs [finalizeAll] before
///    `windowManager.destroy()`;
///  * Cmd+Q / app-menu Quit / Dock quit / logout — these route through
///    `NSApplication.terminate:`, which FlutterAppDelegate turns into a
///    cancellable exit request; the `AppLifecycleListener.onExitRequested`
///    guard runs [finalizeAll] before allowing the exit.
/// NOT interceptable: force-quit / `kill -9`, where the process dies without
/// notice. That path is covered differently: the engine keeps the streaming
/// WAV's header valid (periodic in-place patch), and the next boot recovers
/// the staged file as a new take (`Persistence.recoverStagedRecordings`)
/// before the staging sweep.
///
/// A recording view model registers a callback when it starts and removes it
/// when it stops or is disposed. A save in flight holds the quit the same way
/// for exactly its own lifetime ([hold]).
class RecordingFinalizer {
  RecordingFinalizer._()
    : perFinalizerBudget = _defaultPerFinalizer,
      totalBudget = _defaultTotal;

  /// An isolated instance for tests, with budgets small enough to drive the
  /// expiry paths in real time.
  @visibleForTesting
  RecordingFinalizer.forTesting({
    this.perFinalizerBudget = _defaultPerFinalizer,
    this.totalBudget = _defaultTotal,
  });

  /// The single app-wide instance (recordings are bound to the one main window).
  static final RecordingFinalizer instance = RecordingFinalizer._();

  /// How long ONE finalizer may hold the quit. A legitimate finalize is a
  /// hardware stop, one waveform extraction and one database write — a second
  /// or two even for a long take on a busy disk. Past that it is wedged, which
  /// is reachable without any bug: a save retries behind the busy gate for as
  /// long as it takes, because dropping it would be worse.
  static const Duration _defaultPerFinalizer = Duration(seconds: 5);

  /// Ceiling across ALL registered finalizers, so several wedged recordings
  /// cannot serialize their budgets into a quit macOS ends on its own terms.
  static const Duration _defaultTotal = Duration(seconds: 8);

  final Duration perFinalizerBudget;
  final Duration totalBudget;

  final _pending = <Future<void> Function()>{};

  /// The in-flight [finalizeAll] run, shared by concurrent callers.
  Future<void>? _inFlight;

  /// True while at least one recording is in flight.
  bool get hasActive => _pending.isNotEmpty;

  void register(Future<void> Function() finalize) => _pending.add(finalize);

  void unregister(Future<void> Function() finalize) =>
      _pending.remove(finalize);

  /// Runs [work] — a save the user already asked for — while holding the
  /// quit: registered from the synchronous start of [work] until it settles,
  /// so a quit landing mid-save waits for it (under the same budgets as any
  /// finalizer) instead of exiting under it. Without this a Save followed by
  /// Cmd+Q exited before the commit, and the edit was lost with nothing left
  /// to recover. [work]'s outcome is returned to the caller unchanged; the
  /// quit only waits for it.
  Future<T> hold<T>(Future<T> Function() work) async {
    final future = work();
    Future<void> waitForIt() => future.then<void>((_) {}, onError: (_) {});
    register(waitForIt);
    try {
      return await future;
    } finally {
      unregister(waitForIt);
    }
  }

  /// Runs every registered finalizer (best-effort; one failure never blocks the
  /// rest or the quit). Iterates a snapshot, so a finalizer that unregisters
  /// itself mid-run is safe. Concurrent calls (e.g. a close click racing a
  /// Cmd+Q exit request) await the SAME in-flight run instead of double-running
  /// finalizers; once a run completes, a later call starts a fresh one.
  ///
  /// BOUNDED: each finalizer gets [perFinalizerBudget] and the whole run
  /// [totalBudget]. Both quit paths gate the app's exit on this future, and an
  /// exit macOS ends on its own terms is worse than a skipped save — a take
  /// left behind is recovered on the next boot, a force-killed process
  /// finalizes nothing at all.
  Future<void> finalizeAll() =>
      _inFlight ??= _finalizeAll().whenComplete(() => _inFlight = null);

  Future<void> _finalizeAll() async {
    final pending = _pending.toList();
    final spent = Stopwatch()..start();
    for (var i = 0; i < pending.length; i++) {
      final left = totalBudget - spent.elapsed;
      if (left <= Duration.zero) {
        DiagnosticsLog.instance.log(
          'quit',
          'finalize budget spent; ${pending.length - i} not run',
        );
        return;
      }
      final budget = left < perFinalizerBudget ? left : perFinalizerBudget;
      try {
        await pending[i]().timeout(budget);
      } on TimeoutException {
        // Quitting past a wedged finalize loses strictly LESS than being
        // force-killed while still waiting: the streaming WAV's header is
        // patched in place about once a second, so the staged take is
        // recovered on the next boot either way (see the force-quit note
        // above). Future.timeout also absorbs a late failure from the
        // abandoned callback, so no unhandled async error escapes into the
        // exiting process.
        DiagnosticsLog.instance.log(
          'quit',
          'finalizer ${i + 1}/${pending.length} exceeded '
              '${budget.inMilliseconds} ms; left for boot recovery',
        );
      } catch (_) {
        // Best-effort by contract: one failure never blocks the rest.
      }
    }
  }
}
