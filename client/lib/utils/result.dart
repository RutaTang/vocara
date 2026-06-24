/// A lightweight result type used across the data and UI layers, following
/// Flutter's official app-architecture guide. Repositories and services return
/// [Result] instead of throwing, so callers must handle both branches.
sealed class Result<T> {
  const Result();

  /// A successful result carrying a [value].
  const factory Result.ok(T value) = Ok._;

  /// A failed result carrying an [error].
  const factory Result.error(Exception error) = Error._;
}

/// A successful [Result].
final class Ok<T> extends Result<T> {
  const Ok._(this.value);

  final T value;

  @override
  String toString() => 'Result<$T>.ok($value)';
}

/// A failed [Result].
final class Error<T> extends Result<T> {
  const Error._(this.error);

  final Exception error;

  @override
  String toString() => 'Result<$T>.error($error)';
}
