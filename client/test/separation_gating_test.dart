import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/config/dependencies.dart';
import 'package:vocara/config/environment.dart';
import 'package:vocara/data/services/local_storage_service.dart';
import 'package:vocara/data/services/source_separation_service.dart';

void main() {
  group('createSourceSeparationService', () {
    test('prod is wired to the Null engine (dead feature stays dark)', () {
      final svc = createSourceSeparationService(
        AppEnvironment.prod,
        LocalStorageService(),
      );
      expect(svc, isA<NullSourceSeparationService>());
    });

    test('dev and test keep the Python-sidecar prototype', () {
      for (final env in [AppEnvironment.dev, AppEnvironment.test]) {
        expect(
          createSourceSeparationService(env, LocalStorageService()),
          isA<ProcessSourceSeparationService>(),
          reason: '$env should keep the prototype engine',
        );
      }
    });
  });

  group('NullSourceSeparationService', () {
    // The availability probe runs in the detail view model's constructor on
    // every practice open — in prod it must resolve false without spawning
    // any process (isAvailable is a plain `async => false`; nothing else).
    test('reports unavailable', () async {
      expect(await const NullSourceSeparationService().isAvailable(), isFalse);
    });

    test('separate throws UnsupportedError (guard behind the hidden UI)', () {
      expect(
        const NullSourceSeparationService().separate('/no/such/song.wav'),
        throwsUnsupportedError,
      );
    });

    test('cancel is a safe no-op', () {
      expect(const NullSourceSeparationService().cancel, returnsNormally);
    });
  });
}
