import 'dart:isolate';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/media_analysis_service.dart';

/// Stands in for the technique view model: what a `wanted` predicate closes
/// over. Its port is not sendable to another isolate, as the view model's
/// repository (a pending write-gate future among it) is not.
class _Owner {
  final port = ReceivePort();
  bool get stillWanted => true;
}

void main() {
  test('a wanted predicate over an unsendable owner does not ride into the '
      'analysis isolate', () async {
    // The isolate hop closed over the gate closure's context, which held
    // the predicate — and with it the whole technique view model, whose
    // repository the isolate message could not carry: every reference
    // example's analysis threw and was marked failed.
    const rate = 16000;
    final tone = Float32List.fromList([
      for (var i = 0; i < 2 * rate; i++)
        0.3 * math.sin(2 * math.pi * 220 * i / rate),
    ]);
    final service = MediaAnalysisService(
      decode: (path, {maxDuration}) async => PcmAudio(tone, rate),
    );
    final owner = _Owner();
    addTearDown(owner.port.close);

    final analysis = await service.analyze(
      'reference.m4a',
      wanted: () => owner.stillWanted,
    );

    expect(analysis, isNotNull);
    expect(analysis!.durationMs, 2000);
  });

  test('a clip deselected during its decode is not analyzed', () async {
    const rate = 16000;
    var wanted = true;
    final service = MediaAnalysisService(
      decode: (path, {maxDuration}) async {
        wanted = false; // deselected while it decoded
        return PcmAudio(Float32List(rate), rate);
      },
    );

    expect(
      await service.analyze('reference.m4a', wanted: () => wanted),
      isNull,
    );
  });
}
