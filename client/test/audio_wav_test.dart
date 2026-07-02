import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';

void main() {
  test('writeWavMono/readWavMono round-trips samples and sample rate', () {
    const sr = 44100;
    final samples = Float64List.fromList([
      for (var i = 0; i < 256; i++) 0.7 * (i / 255 * 2 - 1),
    ]);
    final pcm = readWavMono(writeWavMono(samples, sr));
    expect(pcm, isNotNull);
    expect(pcm!.sampleRate, sr);
    expect(pcm.samples.length, samples.length);
    for (var i = 0; i < samples.length; i++) {
      expect(pcm.samples[i], closeTo(samples[i], 2e-4));
    }
  });

  test(
    'concatenating WAV parts preserves total length and boundary samples',
    () {
      // Mirrors AudioEditService.concatWav: split into two halves, glue back.
      const sr = 16000;
      final a = Float64List.fromList(List.filled(1000, 0.3));
      final b = Float64List.fromList(List.filled(500, -0.4));
      final pa = readWavMono(writeWavMono(a, sr))!;
      final pb = readWavMono(writeWavMono(b, sr))!;
      final merged = Float64List(pa.samples.length + pb.samples.length)
        ..setRange(0, pa.samples.length, pa.samples)
        ..setRange(
          pa.samples.length,
          pa.samples.length + pb.samples.length,
          pb.samples,
        );
      final round = readWavMono(writeWavMono(merged, sr))!;
      expect(round.samples.length, 1500);
      expect(round.samples.first, closeTo(0.3, 2e-4));
      expect(round.samples.last, closeTo(-0.4, 2e-4));
    },
  );
}
