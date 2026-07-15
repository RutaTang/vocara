// AppEnvironment.resolve: the flavor string arrives in whatever casing the
// build pipeline uses — `flutter build macos --flavor prod` injects the Xcode
// SCHEME name ("Prod"). The v0.1.0 release shipped as dev because resolution
// was case-sensitive; these tests pin the case-insensitive contract.
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/config/environment.dart';

void main() {
  test('scheme-cased flavors resolve to their environments', () {
    expect(AppEnvironment.resolve('Prod'), AppEnvironment.prod);
    expect(AppEnvironment.resolve('Test'), AppEnvironment.test);
    expect(AppEnvironment.resolve('Dev'), AppEnvironment.dev);
  });

  test('lowercase flavor arguments resolve identically', () {
    expect(AppEnvironment.resolve('prod'), AppEnvironment.prod);
    expect(AppEnvironment.resolve('test'), AppEnvironment.test);
    expect(AppEnvironment.resolve('dev'), AppEnvironment.dev);
  });

  test('absent or unknown flavor falls back to dev, never prod', () {
    expect(AppEnvironment.resolve(null), AppEnvironment.dev);
    expect(AppEnvironment.resolve(''), AppEnvironment.dev);
    expect(AppEnvironment.resolve('staging'), AppEnvironment.dev);
  });

  test('prod hides the badge and uses the plain window title', () {
    expect(AppEnvironment.prod.badge, isEmpty);
    expect(AppEnvironment.prod.windowTitle, 'Vocara');
    expect(AppEnvironment.dev.badge, 'DEV');
    expect(AppEnvironment.test.badge, 'TEST');
  });
}
