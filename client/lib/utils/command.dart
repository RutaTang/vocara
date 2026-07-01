import 'package:flutter/foundation.dart';

import 'result.dart';

typedef CommandAction0<T> = Future<Result<T>> Function();
typedef CommandAction1<T, A> = Future<Result<T>> Function(A);

/// A [ChangeNotifier] that runs an async action and exposes its running/result
/// state to the UI, following Flutter's official app-architecture guide. The
/// UI listens to a command to render loading spinners and surface errors.
abstract class Command<T> extends ChangeNotifier {
  Command();

  bool _running = false;
  bool get running => _running;

  Result<T>? _result;
  Result<T>? get result => _result;

  /// Whether the last run finished with an error.
  bool get error => _result is Error;

  /// Whether the last run finished successfully.
  bool get completed => _result is Ok;

  Future<void> _execute(CommandAction0<T> action) async {
    // Guard against re-entrancy while a run is in flight.
    if (_running) return;

    _running = true;
    _result = null;
    notifyListeners();

    try {
      _result = await action();
    } catch (e) {
      // The contract is that actions return a Result and never throw. Be defensive
      // anyway: a thrown action would otherwise leave _result null (neither error
      // nor completed, so the UI shows nothing) and surface as an unhandled async
      // error far from its cause. Convert it into an observable error result.
      _result = Result.error(e is Exception ? e : Exception('$e'));
    } finally {
      _running = false;
      notifyListeners();
    }
  }

  /// Clears the last result (e.g. after the UI has shown the error once).
  void clearResult() {
    _result = null;
    notifyListeners();
  }
}

/// A [Command] that accepts no arguments.
class Command0<T> extends Command<T> {
  Command0(this._action);

  final CommandAction0<T> _action;

  Future<void> execute() => _execute(_action);
}

/// A [Command] that accepts a single argument.
class Command1<T, A> extends Command<T> {
  Command1(this._action);

  final CommandAction1<T, A> _action;

  Future<void> execute(A argument) => _execute(() => _action(argument));
}
