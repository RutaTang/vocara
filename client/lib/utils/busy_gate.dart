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
  BusyGate({required this.onError});

  /// Called when [run]'s action throws, so the owner can surface a friendly
  /// message (toast) its own way. [errorMessage] keeps the raw error text.
  final void Function(Object error) onError;

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
  Future<void> run(Future<void> Function() action) async {
    _busy = true;
    _errorMessage = null;
    notifyListeners();
    try {
      await action();
    } catch (e) {
      _errorMessage = e.toString();
      onError(e);
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
