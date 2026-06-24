import 'dart:async';

/// Process-wide registry of in-flight recordings so the app can FINALIZE them
/// (stop + analyze + save the captured take) before the OS tears the process
/// down on quit (Cmd+Q / red close button). Without this, quitting mid-record
/// loses the take as a 0-length row + a header-broken WAV — the finalize only
/// runs on the normal stop path.
///
/// A recording view model registers a callback when it starts and removes it
/// when it stops or is disposed; [finalizeAll] is invoked from the window-close
/// handler before `windowManager.destroy()`.
class RecordingFinalizer {
  RecordingFinalizer._();

  /// The single app-wide instance (recordings are bound to the one main window).
  static final RecordingFinalizer instance = RecordingFinalizer._();

  final _pending = <Future<void> Function()>{};

  /// True while at least one recording is in flight.
  bool get hasActive => _pending.isNotEmpty;

  void register(Future<void> Function() finalize) => _pending.add(finalize);

  void unregister(Future<void> Function() finalize) =>
      _pending.remove(finalize);

  /// Runs every registered finalizer (best-effort; one failure never blocks the
  /// rest or the quit). Iterates a snapshot, so a finalizer that unregisters
  /// itself mid-run is safe.
  Future<void> finalizeAll() async {
    for (final finalize in _pending.toList()) {
      try {
        await finalize();
      } catch (_) {}
    }
  }
}
