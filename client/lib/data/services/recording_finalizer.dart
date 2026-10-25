import 'dart:async';

import 'package:flutter/foundation.dart';

import '../db/asset_store.dart';
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
      totalBudget = _defaultTotal,
      progressCeiling = _defaultProgressCeiling,
      _progress = AssetStore.bytesBeingCopied;

  /// An isolated instance for tests, with budgets small enough to drive the
  /// expiry paths in real time.
  @visibleForTesting
  RecordingFinalizer.forTesting({
    this.perFinalizerBudget = _defaultPerFinalizer,
    this.totalBudget = _defaultTotal,
    this.progressCeiling = _defaultProgressCeiling,
    Future<int> Function()? progress,
  }) : _progress = progress ?? (() async => 0);

  /// The single app-wide instance (recordings are bound to the one main window).
  static final RecordingFinalizer instance = RecordingFinalizer._();

  /// How long the quit waits on its finalizers, which run together, with
  /// nothing being copied. A legitimate finalize is a hardware stop, one
  /// waveform extraction and one database write — a second or two even for a
  /// long take on a busy disk. Past that it is wedged, which
  /// is reachable without any bug: a save retries behind the busy gate for as
  /// long as it takes, because dropping it would be worse.
  static const Duration _defaultPerFinalizer = Duration(seconds: 5);

  /// Ceiling on that wait since the quit began, across both quit paths' runs
  /// (see [_quitClock]), so a quit macOS would end on its own terms never
  /// happens without bytes still moving.
  static const Duration _defaultTotal = Duration(seconds: 8);

  /// How long a quit keeps waiting, all told, on a finalizer that is still
  /// making progress past its budget (see [finalizeAll]): a save pooling a large
  /// file — a 1 GB technique video copies for well over 5 s — is working, not
  /// wedged, and cutting it off lost the user's Save outright.
  static const Duration _defaultProgressCeiling = Duration(seconds: 60);

  final Duration perFinalizerBudget;
  final Duration totalBudget;
  final Duration progressCeiling;

  /// A reading that moves while finalizers do real work (the bytes the
  /// imports in flight have copied so far): the quit's evidence that a
  /// finalizer past its budget is still working.
  final Future<int> Function() _progress;

  final _pending = <Future<void> Function()>{};

  /// The in-flight [finalizeAll] run, shared by concurrent callers.
  Future<void>? _inFlight;

  /// True while at least one recording is in flight.
  bool get hasActive => _pending.isNotEmpty;

  /// Lists [finalize] for the next quit. [startsWork] marks one that begins
  /// work no quit run has budgeted yet — an armed take's stop-and-save — as
  /// opposed to a wait on work already under way (a save's [hold], a stop's
  /// tail): a quit run that finds one registered since its last call gives
  /// it a budget of its own (see [_finalizeAll]).
  void register(Future<void> Function() finalize, {bool startsWork = false}) {
    _pending.add(finalize);
    _invoked.remove(finalize);
    if (startsWork) {
      _startsWork.add(finalize);
    } else {
      _startsWork.remove(finalize);
    }
  }

  void unregister(Future<void> Function() finalize) {
    _pending.remove(finalize);
    _invoked.remove(finalize);
    _startsWork.remove(finalize);
  }

  /// The registered finalizers marked `startsWork` (see [register]).
  final _startsWork = <Future<void> Function()>{};

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

  /// Stopped takes still landing, per practice id (see [landing]).
  final _landing = <String, Set<Future<void>>>{};

  /// Runs [work] — the post-stop finalize of a take of [practiceId] (its
  /// analysis, then the save that commits it) — listed for [landed] until it
  /// settles. The finalize outlives the screen that stopped the take (Stop,
  /// then Back), and a screen reopened on the practice meanwhile read it
  /// without the take.
  Future<void> landing(String practiceId, Future<void> Function() work) async {
    final tail = work();
    final tails = _landing.putIfAbsent(practiceId, () => {})..add(tail);
    try {
      await tail;
    } finally {
      tails.remove(tail);
      if (tails.isEmpty) _landing.remove(practiceId);
    }
  }

  /// Whether anything of [practiceId] is landing right now (see [landing]).
  bool isLanding(String practiceId) => _landing.containsKey(practiceId);

  /// Completes once every take of [practiceId] landing now has settled,
  /// whatever its outcome (the finalize reports its own) — or after
  /// [perFinalizerBudget], like a quit's wait: a wedged finalize then costs
  /// the reopened screen only the take's visibility, never the screen.
  Future<void> landed(String practiceId) {
    final tails = _landing[practiceId];
    if (tails == null) return Future.value();
    return Future.wait([
      for (final tail in tails) tail.then<void>((_) {}, onError: (_) {}),
    ]).timeout(perFinalizerBudget, onTimeout: () => const []);
  }

  /// Completes once nothing of [practiceId] is landing: every tail landing
  /// now, and any listed while this waits, has settled. No budget — what a
  /// reopened screen waits on before it takes edits, since its edits merge
  /// row by row against what it read, and a save landing underneath them
  /// would win or lose against them outright.
  Future<void> settled(String practiceId) async {
    for (
      var tails = _landing[practiceId];
      tails != null;
      tails = _landing[practiceId]
    ) {
      await Future.wait([
        for (final tail in List.of(tails))
          tail.then<void>((_) {}, onError: (_) {}),
      ]);
    }
  }

  /// Runs every registered finalizer (best-effort; one failure never blocks the
  /// rest or the quit). Iterates a snapshot, so a finalizer that unregisters
  /// itself mid-run is safe. Concurrent calls (e.g. a close click racing a
  /// Cmd+Q exit request) await the SAME in-flight run instead of double-running
  /// finalizers; once a run completes, a later call starts a fresh one.
  ///
  /// The finalizers run TOGETHER, and the quit waits for all of them at once.
  /// One after another, a save queued behind a long one (a mute behind a large
  /// import) found the budget spent and was never waited on. And a copy that
  /// kept the quit waiting could belong to a save it was not waiting on yet.
  ///
  /// BOUNDED: the quit waits [perFinalizerBudget], never past [totalBudget]
  /// since the quit began, and on past that only while bytes are still being
  /// copied ([_progress] moved during the last window), never past
  /// [progressCeiling]. Both quit paths gate the app's exit on this future, and
  /// an exit macOS ends on its own terms is worse than a skipped save — a take
  /// left behind is recovered on the next boot, a force-killed process
  /// finalizes nothing at all.
  Future<void> finalizeAll() =>
      _inFlight ??= _finalizeAll().whenComplete(() => _inFlight = null);

  /// When the quit began, kept across runs while one cut a finalizer off: the
  /// close button runs [finalizeAll], and the destroy() after it is an exit
  /// request that runs it again for whatever is still pending — timed afresh,
  /// a wedged save held the quit for twice every limit. A run whose finalizers
  /// all settled clears it, and so does a later run finding it older than any
  /// quit lasts (one the app survived: a destroy() that threw).
  Stopwatch? _quitClock;

  /// The finalizers this quit has called since each was registered. Every
  /// run calls every pending finalizer again — each is safe to: a take's
  /// stops only a take still recording, a stop's tail and a save's hold wait
  /// afresh, so a run sees work registered under the same tear-off since
  /// (a take re-armed on the same screen, a second stop's tail). But only a
  /// `startsWork` one not in here gets a budget of its own; the rest are on
  /// the quit's clock, or a take's own stop-and-save — registering its tail
  /// and holds as it goes — would earn the exit request's run a fresh budget
  /// every time.
  final _invoked = <Future<void> Function()>{};

  Future<void> _finalizeAll() async {
    final pending = _pending.toList();
    if (pending.isEmpty) return;
    if ((_quitClock?.elapsed ?? Duration.zero) >
        progressCeiling + perFinalizerBudget) {
      _quitClock = null;
      _invoked.clear();
    }
    final clock = _quitClock ??= Stopwatch()..start();
    final fresh = pending.any(
      (f) => _startsWork.contains(f) && !_invoked.contains(f),
    );
    var window = fresh
        ? perFinalizerBudget
        : _shorter(perFinalizerBudget, totalBudget - clock.elapsed);
    if (window <= Duration.zero) {
      DiagnosticsLog.instance.log(
        'quit',
        'finalize budget spent; ${pending.length} still running',
      );
      return;
    }
    // Read before the finalizers start: the wait must attach to them at once,
    // or one failing straight away escapes as an unhandled error.
    var mark = await _progress();
    _invoked.addAll(pending);
    var running = pending.length;
    final all = Future.wait([
      for (final finalize in pending)
        Future.sync(finalize)
            // Best-effort by contract: one failure never blocks the rest.
            .then<void>((_) {}, onError: (_) {})
            .whenComplete(() => running--),
    ]);
    while (true) {
      try {
        await all.timeout(window);
        _quitClock = null;
        _invoked.clear();
        return;
      } on TimeoutException {
        // Moved, not risen: the reading is the staging files in flight, so it
        // DROPS when one of two concurrent copies lands while the other is
        // still copying.
        final now = await _progress();
        window = _shorter(perFinalizerBudget, progressCeiling - clock.elapsed);
        if (now == mark || window <= Duration.zero) break;
        mark = now;
        DiagnosticsLog.instance.log(
          'quit',
          'still copying past the budget; waiting on',
        );
      }
    }
    // Quitting past a wedged finalize loses strictly LESS than being
    // force-killed while still waiting: the streaming WAV's header is patched
    // in place about once a second, so the staged take is recovered on the
    // next boot either way (see the force-quit note above). The finalizers'
    // own failures are absorbed above, so none escapes into the exiting
    // process.
    DiagnosticsLog.instance.log(
      'quit',
      '$running of ${pending.length} finalizers still running after '
          '${clock.elapsed.inMilliseconds} ms; left for boot recovery',
    );
  }

  static Duration _shorter(Duration a, Duration b) => a < b ? a : b;
}
