import 'dart:math';
import 'dart:typed_data';

import 'package:fftea/fftea.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';

void main() {
  test('StreamingSpectrogramBuilder equals computeSpectrogram exactly, over '
      'awkward chunk boundaries', () {
    // The builder must be COLUMN-EXACT with the batch path (the same frames
    // and column math) — including the trailing zero-padded partial frame
    // and the empty-input case.
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

  test("the analysis FFT is fftea's realFft bit for bit", () {
    Uint64List bits(Float64x2List a) =>
        a.buffer.asUint64List(a.offsetInBytes, a.length * 2);
    final rng = Random(17);
    for (final n in [16, 32, 64, 256, 512, 1024, 2048, 4096, 8192]) {
      final fft = FFT(n);
      for (var t = 0; t < 30; t++) {
        final x = Float64List(n);
        for (var i = 0; i < n; i++) {
          // Signed zeros (a Hann window's ends, digital silence), denormal-
          // scale and full-scale values, and an all-zero frame.
          x[i] = t == 0
              ? 0.0
              : switch (rng.nextInt(12)) {
                  0 => 0.0,
                  1 => -0.0,
                  2 => 1e-310 * (rng.nextDouble() - 0.5),
                  _ => rng.nextDouble() * 2 - 1,
                };
        }
        expect(
          bits(radix2RealFft(x)),
          bits(fft.realFft(x)),
          reason: 'n=$n trial $t',
        );
      }
    }
  });

  test("the spectrogram builders are fftea's STFT bit for bit", () {
    // What computeSpectrogram was: fftea's STFT over the samples, the first
    // maxBin magnitudes of each frame in dB, no frame for empty input.
    Float32List ffteaColumns(
      Float32List samples,
      int sr,
      double maxHz,
      int fftSize,
      int hop,
    ) {
      final maxBin = min(fftSize ~/ 2 + 1, (maxHz * fftSize / sr).ceil() + 1);
      final out = <double>[];
      if (samples.isNotEmpty) {
        STFT(fftSize, Window.hanning(fftSize)).run(samples, (freq) {
          final mags = freq.discardConjugates().magnitudes();
          for (var b = 0; b < maxBin; b++) {
            out.add(20.0 * (log(mags[b] + 1e-9) / 2.302585092994046));
          }
        }, hop);
      }
      return Float32List.fromList(out);
    }

    Uint32List bits(Float32List a) =>
        a.buffer.asUint32List(a.offsetInBytes, a.length);
    final rng = Random(23);
    for (final (sr, fftSize, hop) in [
      (8000, 2048, 512),
      (22050, 2048, 512),
      (44100, 2048, 512),
      (48000, 2048, 512),
      (48000, 1024, 256),
      (44100, 1000, 300), // fftea's own FFT, not radix-2
    ]) {
      for (final n in [0, 1, 300, fftSize, fftSize + 1, 9001, sr]) {
        final samples = Float32List(n);
        for (var i = 0; i < n; i++) {
          samples[i] =
              0.4 * sin(2 * pi * 220 * i / sr) +
              0.05 * (rng.nextDouble() - 0.5);
        }
        for (final maxHz in [5000.0, 18000.0]) {
          final want = ffteaColumns(samples, sr, maxHz, fftSize, hop);
          final why = 'sr=$sr fft=$fftSize n=$n maxHz=$maxHz';
          final batch = computeSpectrogram(
            samples,
            sr,
            fftSize: fftSize,
            hop: hop,
            maxHz: maxHz,
          );
          expect(bits(batch.db), bits(want), reason: why);
          final b = StreamingSpectrogramBuilder(
            sr,
            fftSize: fftSize,
            hop: hop,
            maxHz: maxHz,
          );
          for (var at = 0; at < n; at += 777) {
            b.add(Float32List.sublistView(samples, at, min(n, at + 777)));
          }
          expect(bits(b.finish().db), bits(want), reason: '$why (streamed)');
        }
      }
    }
  });

  test("the live analyzers' columns are fftea's transform bit for bit", () {
    // What StreamingStft and LiveVoiceAnalyzer computed: every complete
    // window at a multiple of hop (no padded tail), Hann-windowed through
    // fftea's realFft, the first maxBin magnitudes in dB.
    List<Float32List> ffteaColumns(
      Float32List samples,
      int sr,
      double maxHz,
      int fftSize,
      int hop,
    ) {
      final maxBin = min(fftSize ~/ 2 + 1, (maxHz * fftSize / sr).ceil() + 1);
      final window = Window.hanning(fftSize);
      final fft = FFT(fftSize);
      final frame = Float64List(fftSize);
      final out = <Float32List>[];
      for (var at = 0; at + fftSize <= samples.length; at += hop) {
        for (var i = 0; i < fftSize; i++) {
          frame[i] = samples[at + i] * window[i];
        }
        final mags = fft.realFft(frame).discardConjugates().magnitudes();
        out.add(
          Float32List.fromList([
            for (var b = 0; b < maxBin; b++)
              20.0 * (log(mags[b] + 1e-9) / 2.302585092994046),
          ]),
        );
      }
      return out;
    }

    Uint32List bits(Float32List a) =>
        a.buffer.asUint32List(a.offsetInBytes, a.length);
    final rng = Random(29);
    for (final (sr, fftSize, hop) in [
      (16000, 2048, 512),
      (44100, 2048, 512),
      (48000, 2048, 512),
      (48000, 1024, 256),
      (44100, 1000, 300), // fftea's own FFT, not radix-2
    ]) {
      final n = sr ~/ 2 + 123;
      final samples = Float32List(n);
      for (var i = 0; i < n; i++) {
        samples[i] =
            0.4 * sin(2 * pi * 220 * i / sr) + 0.05 * (rng.nextDouble() - 0.5);
      }
      for (final maxHz in [5000.0, 18000.0]) {
        final want = ffteaColumns(samples, sr, maxHz, fftSize, hop);
        final why = 'sr=$sr fft=$fftSize maxHz=$maxHz';
        final stft = StreamingStft(
          sampleRate: sr,
          fftSize: fftSize,
          hop: hop,
          maxHz: maxHz,
        );
        final live = LiveVoiceAnalyzer(
          sampleRate: sr,
          fftSize: fftSize,
          hop: hop,
          maxHz: maxHz,
        );
        final stftCols = <Float32List>[], liveCols = <Float32List>[];
        for (var at = 0; at < n; at += 777) {
          final chunk = Float32List.sublistView(samples, at, min(n, at + 777));
          stft.addSamples(chunk, stftCols.add);
          live.addSamples(chunk, (f) => liveCols.add(f.column));
        }
        expect(stftCols.length, want.length, reason: why);
        expect(liveCols.length, want.length, reason: why);
        for (var c = 0; c < want.length; c++) {
          expect(bits(stftCols[c]), bits(want[c]), reason: '$why col $c');
          expect(
            bits(liveCols[c]),
            bits(want[c]),
            reason: '$why col $c (live)',
          );
        }
      }
    }
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

  test('computeVoiceMetricsAndSpectrogram equals the two separate passes '
      'bit for bit (its spectrogram is the metrics pass\'s own FFT)', () {
    Uint32List bits(Float32List a) =>
        a.buffer.asUint32List(a.offsetInBytes, a.length);
    final rng = Random(5);
    for (final sr in [8000, 16000, 22050, 44100, 48000]) {
      // Sub-frame, exact-frame, hop-ragged and longer lengths: the trailing
      // zero-padded frame and the empty input included.
      for (final n in [0, 300, 2048, 2049, 9001, sr]) {
        final samples = Float32List(n);
        for (var i = 0; i < n; i++) {
          samples[i] =
              0.4 * sin(2 * pi * 220 * i / sr) +
              0.05 * (rng.nextDouble() - 0.5);
        }
        final both = computeVoiceMetricsAndSpectrogram(samples, sr);
        final spec = computeSpectrogram(samples, sr);
        final metrics = computeVoiceMetrics(samples, sr);
        final s = both.spectrogram;
        final why = 'sr=$sr n=$n';
        expect(s.frames, spec.frames, reason: why);
        expect(s.bins, spec.bins, reason: why);
        expect(s.maxHz, spec.maxHz, reason: why);
        expect(bits(s.db), bits(spec.db), reason: why);
        expect(s.minDb, spec.minDb, reason: why);
        expect(s.maxDb, spec.maxDb, reason: why);
        expect(both.metrics.frames, metrics.frames, reason: why);
        expect(bits(both.metrics.f0), bits(metrics.f0), reason: why);
        expect(bits(both.metrics.cpp), bits(metrics.cpp), reason: why);
        expect(bits(both.metrics.f1), bits(metrics.f1), reason: why);
        expect(both.metrics.active, metrics.active, reason: why);
      }
    }
  });

  test(
    'a low-rate source caps maxHz at the highest bin both builders emit',
    () {
      // At 8 kHz the bins stop at Nyquist (4 kHz); a maxHz left at 5 kHz
      // stretched the 0–4 kHz image over a 0–5 kHz axis, grid and formant
      // overlay. Rates whose bins reach 5 kHz keep it unchanged.
      for (final sr in [8000, 11025, 44100, 48000]) {
        final samples = Float32List(sr ~/ 4);
        for (var i = 0; i < samples.length; i++) {
          samples[i] = sin(2 * pi * 1000 * i / sr) * 0.5;
        }
        final batch = computeSpectrogram(samples, sr);
        final b = StreamingSpectrogramBuilder(sr)..add(samples);
        final streamed = b.finish();
        final top = batch.freqOfBin(batch.bins - 1);
        final want = sr < 10000 ? top : 5000.0;
        expect(batch.maxHz, want, reason: '$sr Hz batch');
        expect(streamed.maxHz, want, reason: '$sr Hz streamed');
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
