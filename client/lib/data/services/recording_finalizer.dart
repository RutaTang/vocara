import 'dart:async';

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
/// when it stops or is disposed.
class RecordingFinalizer {
  RecordingFinalizer._();

  /// The single app-wide instance (recordings are bound to the one main window).
  static final RecordingFinalizer instance = RecordingFinalizer._();

  final _pending = <Future<void> Function()>{};

  /// The in-flight [finalizeAll] run, shared by concurrent callers.
  Future<void>? _inFlight;

  /// True while at least one recording is in flight.
  bool get hasActive => _pending.isNotEmpty;

  void register(Future<void> Function() finalize) => _pending.add(finalize);

  void unregister(Future<void> Function() finalize) =>
      _pending.remove(finalize);

  /// Runs every registered finalizer (best-effort; one failure never blocks the
  /// rest or the quit). Iterates a snapshot, so a finalizer that unregisters
  /// itself mid-run is safe. Concurrent calls (e.g. a close click racing a
  /// Cmd+Q exit request) await the SAME in-flight run instead of double-running
  /// finalizers; once a run completes, a later call starts a fresh one.
  Future<void> finalizeAll() =>
      _inFlight ??= _finalizeAll().whenComplete(() => _inFlight = null);

  Future<void> _finalizeAll() async {
    for (final finalize in _pending.toList()) {
      try {
        await finalize();
      } catch (_) {}
    }
  }
}
