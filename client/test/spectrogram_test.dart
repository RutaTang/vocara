import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';

void main() {
  test('StreamingSpectrogramBuilder equals computeSpectrogram exactly, over '
      'awkward chunk boundaries', () {
    // The builder must be COLUMN-EXACT with the batch path (same fftea
    // pipeline; stream+flush share run's implementation) — including the
    // trailing zero-padded partial frame and the empty-input case.
    final n = 10000; // not a multiple of fftSize or hop
    final samples = Float32List(n);
    for (var i = 0; i < n; i++) {
      samples[i] = (sin(i / 9.0) * 0.4 + sin(i / 51.0) * 0.2).toDouble();
    }
    final batch = computeSpectrogram(samples, 44100);

    for (final chunks in [
      [n], // one shot
      [1, 999, 2048, n - 1 - 999 - 2048], // ragged
      List.filled(n ~/ 500, 500) + [n % 500], // uniform + remainder
    ]) {
      final b = StreamingSpectrogramBuilder(44100);
      var at = 0;
      for (final c in chunks) {
        if (c == 0) continue;
        b.add(Float32List.sublistView(samples, at, at + c));
        at += c;
      }
      expect(at, n, reason: 'fixture sanity');
      final streamed = b.finish();
      expect(streamed.frames, batch.frames);
      expect(streamed.bins, batch.bins);
      expect(streamed.db, batch.db, reason: 'columns must be bit-identical');
      expect(streamed.minDb, batch.minDb);
      expect(streamed.maxDb, batch.maxDb);
    }

    // Empty input: zero frames on both paths, never a lone padded frame.
    expect(StreamingSpectrogramBuilder(44100).finish().frames, 0);
    expect(computeSpectrogram(Float32List(0), 44100).frames, 0);
  });

  test(
    'computeSpectrogram concentrates energy at the input tone frequencies',
    () {
      const sr = 44100;
      final n = sr; // 1 second
      final samples = Float32List(n);
      const tones = [500.0, 1500.0, 3000.0];
      for (var i = 0; i < n; i++) {
        var v = 0.0;
        for (final f in tones) {
          v += sin(2 * pi * f * i / sr);
        }
        samples[i] = v / tones.length * 0.8;
      }

      final spec = computeSpectrogram(
        samples,
        sr,
        fftSize: 2048,
        hop: 512,
        maxHz: 5000,
      );
      expect(spec.frames, greaterThan(10));
      expect(spec.bins, greaterThan(200));

      // Average dB per bin across all frames.
      final avg = Float64List(spec.bins);
      for (var f = 0; f < spec.frames; f++) {
        final base = f * spec.bins;
        for (var b = 0; b < spec.bins; b++) {
          avg[b] += spec.db[base + b];
        }
      }
      for (var b = 0; b < spec.bins; b++) {
        avg[b] /= spec.frames;
      }

      int binOf(double hz) => (hz / spec.binHz).round();
      final control = avg[binOf(4500)]; // a quiet region with no tone
      for (final f in tones) {
        expect(
          avg[binOf(f)],
          greaterThan(control + 20),
          reason: 'tone $f Hz should dominate the ~4500 Hz control bin',
        );
      }
    },
  );

  test('StreamingStft emits columns with energy at the tone frequency', () {
    const sr = 44100;
    final stft = StreamingStft(
      fftSize: 2048,
      hop: 512,
      sampleRate: sr,
      maxHz: 5000,
    );
    final cols = <Float32List>[];
    const tone = 2000.0;
    var phase = 0.0;
    for (var chunk = 0; chunk < 11; chunk++) {
      final s = Float32List(4096);
      for (var i = 0; i < 4096; i++) {
        s[i] = sin(phase);
        phase += 2 * pi * tone / sr;
      }
      stft.addSamples(s, cols.add);
    }
    expect(cols.length, greaterThan(10));
    int binOf(double hz) => (hz / stft.binHz).round();
    final col = cols.last;
    expect(col[binOf(tone)], greaterThan(col[binOf(4500)] + 20));
  });

  test('readWavMono round-trips 16-bit PCM', () {
    final bytes = _wav(const [0, 16384, -16384, 32767, -32768, 0], 44100);
    final pcm = readWavMono(bytes);
    expect(pcm, isNotNull);
    expect(pcm!.sampleRate, 44100);
    expect(pcm.samples.length, 6);
    expect(pcm.samples[3], closeTo(32767 / 32768, 1e-4));
  });
}

Uint8List _wav(List<int> samples, int sr) {
  final dataSize = samples.length * 2;
  final b = BytesBuilder();
  void s(String x) => b.add(x.codeUnits);
  void u32(int v) =>
      b.add((ByteData(4)..setUint32(0, v, Endian.little)).buffer.asUint8List());
  void u16(int v) =>
      b.add((ByteData(2)..setUint16(0, v, Endian.little)).buffer.asUint8List());
  s('RIFF');
  u32(36 + dataSize);
  s('WAVE');
  s('fmt ');
  u32(16);
  u16(1);
  u16(1);
  u32(sr);
  u32(sr * 2);
  u16(2);
  u16(16);
  s('data');
  u32(dataSize);
  final pcm = ByteData(dataSize);
  for (var i = 0; i < samples.length; i++) {
    pcm.setInt16(i * 2, samples[i], Endian.little);
  }
  b.add(pcm.buffer.asUint8List());
  return b.toBytes();
}
