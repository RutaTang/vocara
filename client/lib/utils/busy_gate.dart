import 'package:flutter/foundation.dart';

/// A screen's single busy/error latch for long-running actions.
///
/// [busy] is deliberately ONE flag per view model, not one per action:
///  * it renders as the one transport spinner and is polled by the
///    integration tests as their settle condition;
///  * it is the cross-action structural-edit MUTEX — actions guard on
///    `busy` before touching the model, so an import can never interleave
///    with a recording finalize (lost-update races).
/// Owning view models must therefore create exactly one instance and inject
/// it into every controller that runs guarded work; a second instance would
/// silently split the mutex.
class BusyGate extends ChangeNotifier {
  BusyGate({required this.onError, this.around});

  /// Called when [run]'s action throws, so the owner can surface a friendly
  /// message (toast) its own way. [errorMessage] keeps the raw error text.
  final void Function(Object error) onError;

  /// Wraps every [run]'s action unless the run opts out: how the owner keeps
  /// track of ALL its guarded work at once, whichever controller started it.
  /// The song practice screen lists each one as landing on its practice, so a
  /// screen reopened on it meanwhile waits for it before taking edits.
  final Future<void> Function(Future<void> Function() action)? around;

  bool _busy = false;
  bool get busy => _busy;

  String? _errorMessage;
  String? get errorMessage => _errorMessage;

  /// Set in [dispose]. Guards the finally-notify of an action that resolves
  /// after the owning view model is gone (e.g. a save finishing while the
  /// route is popped mid-record).
  bool _disposed = false;

  @override
  void notifyListeners() {
    if (_disposed) return;
    super.notifyListeners();
  }

  /// Runs [action] under the busy flag: sets [busy], clears [errorMessage],
  /// notifies; a throw lands in [errorMessage] + [onError]; [busy] clears in
  /// finally with a second notify.
  ///
  /// ATOMIC: when an action already holds the gate, the call does nothing and
  /// returns false — the mutex must hold even for a caller whose own pre-guard
  /// raced the latch (two overlapping runs would let the first finisher clear
  /// [busy] while the second still mutates, re-opening the gate to a third).
  /// Returns true when [action] ran (even if it threw — the error is surfaced
  /// through [errorMessage] + [onError], not the return value). No caller may
  /// nest a [run] inside another run's action: the inner call would be
  /// rejected, silently skipping its work. [around] wraps the action unless
  /// [wrapped] is false.
  Future<bool> run(
    Future<void> Function() action, {
    bool wrapped = true,
  }) async {
    if (_busy) return false;
    _busy = true;
    _errorMessage = null;
    notifyListeners();
    try {
      final around = this.around;
      await (wrapped && around != null ? around(action) : action());
    } catch (e) {
      _errorMessage = e.toString();
      onError(e);
    } finally {
      _busy = false;
      notifyListeners();
    }
    return true;
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
