// An analysis queued behind the gate can stop being wanted before its turn
// (its screen left, its file changed): it is skipped instead of run for no one.
// The predicate closes over a view model, which must not ride into the
// isolate.

import 'dart:io';
import 'dart:isolate';
import 'dart:math' as math;

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/spectrogram_service.dart';

/// What a `wanted` predicate closes over: a port, unsendable like a view
/// model's repository.
class _Owner {
  final port = ReceivePort();
  bool stillWanted = true;
}

void main() {
  late Directory dir;
  late String path;
  final owner = _Owner();

  setUpAll(() async {
    dir = await Directory.systemTemp.createTemp('spec_wanted');
    path = '${dir.path}/take.wav';
    const rate = 16000;
    await File(path).writeAsBytes(
      writeWavMono([
        for (var i = 0; i < rate; i++)
          0.3 * math.sin(2 * math.pi * 220 * i / rate),
      ], rate),
    );
  });
  tearDownAll(() async {
    owner.port.close();
    await dir.delete(recursive: true);
  });

  const service = SpectrogramService();

  test(
    'a wanted analysis runs, its predicate kept out of the isolate',
    () async {
      owner.stillWanted = true;
      bool wanted() => owner.stillWanted;
      expect(await service.computeFromFile(path, wanted: wanted), isNotNull);
      expect(
        await service.computeMetricsFromFile(path, wanted: wanted),
        isNotNull,
      );
      expect(
        await service.computeBothFromFile(path, wanted: wanted),
        isNotNull,
      );
    },
  );

  test('an unwanted one is skipped', () async {
    owner.stillWanted = false;
    bool wanted() => owner.stillWanted;
    expect(await service.computeFromFile(path, wanted: wanted), isNull);
    expect(await service.computeMetricsFromFile(path, wanted: wanted), isNull);
    expect(await service.computeBothFromFile(path, wanted: wanted), isNull);
  });
}
