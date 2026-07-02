import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/source_separation_service.dart';

void main() {
  group('FakeSourceSeparationService', () {
    test(
      'produces two distinct stem files and reports progress to 1.0',
      () async {
        final src = File(
          '${Directory.systemTemp.path}/vocara_src_'
          '${DateTime.now().microsecondsSinceEpoch}.wav',
        );
        await src.writeAsBytes(List<int>.filled(64, 7));
        addTearDown(() => src.delete());

        final svc = FakeSourceSeparationService();
        final progress = <double>[];
        final result = await svc.separate(src.path, onProgress: progress.add);

        expect(await File(result.instrumentalPath).exists(), isTrue);
        expect(await File(result.vocalPath).exists(), isTrue);
        expect(result.instrumentalPath, isNot(result.vocalPath));
        // The Fake copies the source verbatim into both stems.
        expect(
          await File(result.instrumentalPath).readAsBytes(),
          hasLength(64),
        );
        expect(progress, isNotEmpty);
        expect(progress.last, 1.0);
      },
    );

    test('isAvailable reflects the configured flag', () async {
      expect(await FakeSourceSeparationService().isAvailable(), isTrue);
      expect(
        await FakeSourceSeparationService(available: false).isAvailable(),
        isFalse,
      );
    });

    test('still yields two files when the source is missing', () async {
      final result = await FakeSourceSeparationService().separate(
        '/no/such/file.wav',
      );
      expect(await File(result.instrumentalPath).exists(), isTrue);
      expect(await File(result.vocalPath).exists(), isTrue);
    });

    test('cancel() is a safe no-op on the fake', () {
      expect(FakeSourceSeparationService().cancel, returnsNormally);
    });
  });
}
