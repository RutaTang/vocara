import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

const _sr = 44100, _hop = 512, _fft = 2048;
const _binHz = _sr / _fft; // ≈ 21.5

VoiceMetrics _vm({Float32List? f0, Float32List? h1h2, int frames = 0}) {
  final n = frames > 0 ? frames : (f0?.length ?? h1h2?.length ?? 0);
  final z = Float32List(n);
  final nan = Float32List(n)..fillRange(0, n, double.nan);
  return VoiceMetrics(
    frames: n,
    hop: _hop,
    fftSize: 2048,
    sampleRate: _sr,
    ring: z,
    tilt: z,
    cpp: z,
    loudness: z,
    f0: f0 ?? nan,
    f1: nan,
    f2: nan,
    f3: nan,
    h1h2: h1h2 ?? nan,
    active: Uint8List(n)..fillRange(0, n, 1),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
  );
}

/// A spectrogram whose every frame has [dbOfBin] per bin.
Spectrogram _spec(int frames, int bins, double Function(int bin) dbOfBin) {
  final db = Float32List(frames * bins);
  for (var f = 0; f < frames; f++) {
    for (var b = 0; b < bins; b++) {
      db[f * bins + b] = dbOfBin(b);
    }
  }
  return Spectrogram(
    frames: frames,
    bins: bins,
    maxHz: 5000,
    sampleRate: _sr,
    fftSize: _fft,
    hop: _hop,
    db: db,
    minDb: -90,
    maxDb: 0,
  );
}

int _argmax(Float32List xs) {
  var bi = 0;
  for (var i = 1; i < xs.length; i++) {
    if (xs[i] > xs[bi]) bi = i;
  }
  return bi;
}

void main() {
  group('LTAS', () {
    const bins = 233; // ~5000 Hz at 21.5 Hz/bin
    test('peaks at the dominant bin; alpha ratio bright vs dark', () {
      final peak3k = (3000 / _binHz).round();
      final peak300 = (300 / _binHz).round();
      final m = _vm(frames: 10);
      final bright = computeLtas(
        _spec(10, bins, (b) => b == peak3k ? 0.0 : -60.0),
        m,
      );
      final dark = computeLtas(
        _spec(10, bins, (b) => b == peak300 ? 0.0 : -60.0),
        m,
      );

      expect(_argmax(bright.db), peak3k);
      expect(bright.alphaRatio, greaterThan(0)); // energy mostly >1 kHz
      expect(dark.alphaRatio, lessThan(0)); // energy mostly <1 kHz
      // Singer's-formant prominence: a 3 kHz peak reads far higher than 300 Hz.
      expect(bright.singersFormantDb, greaterThan(dark.singersFormantDb + 30));
      expect(bright.hasData, isTrue);
    });

    test('averages in power (a loud bin dominates the mean)', () {
      // Half the frames have a +0 dB bin, half −60 dB at the same bin. The
      // power mean is dominated by the loud frames, so the LTAS sits well above
      // the −30 dB linear-dB midpoint.
      final peak = (3000 / _binHz).round();
      final db = Float32List(10 * bins)..fillRange(0, 10 * bins, -90);
      for (var f = 0; f < 10; f++) {
        db[f * bins + peak] = f < 5 ? 0.0 : -60.0;
      }
      final s = Spectrogram(
        frames: 10,
        bins: bins,
        maxHz: 5000,
        sampleRate: _sr,
        fftSize: _fft,
        hop: _hop,
        db: db,
        minDb: -90,
        maxDb: 0,
      );
      final ltas = computeLtas(s, _vm(frames: 10));
      // mean power = (5·1 + 5·1e-6)/10 ≈ 0.5 → 10·log10(0.5) ≈ −3 dB ≫ −30.
      expect(ltas.db[peak], greaterThan(-6));
    });
  });

  group('Vibrato', () {
    const frameRate = _sr / _hop; // ≈ 86.13

    Float32List vibratoF0(double rate, double extentCents, double seconds) {
      final n = (frameRate * seconds).round();
      final f0 = Float32List(n);
      for (var i = 0; i < n; i++) {
        final t = i / frameRate;
        final cents = extentCents * sin(2 * pi * rate * t);
        f0[i] = 220 * pow(2, cents / 1200).toDouble();
      }
      return f0;
    }

    test('measures a 6 Hz ±50¢ vibrato', () {
      final m = _vm(f0: vibratoF0(6, 50, 1.5));
      final v = analyzeVibrato(m, 0, m.frames - 1);
      expect(v.present, isTrue);
      expect(v.rateHz, inInclusiveRange(5, 7));
      expect(v.extentCents, inInclusiveRange(40, 60));
      expect(v.regularity, greaterThan(0.7));
    });

    test('a straight tone has no vibrato', () {
      final f0 = Float32List((frameRate * 1.5).round())..fillRange(0, 0, 0);
      f0.fillRange(0, f0.length, 220);
      final v = analyzeVibrato(_vm(f0: f0), 0, f0.length - 1);
      expect(v.present, isFalse);
    });

    test('a too-short note reports nothing', () {
      final v = analyzeVibrato(_vm(f0: vibratoF0(6, 50, 0.2)), 0, 100);
      expect(v.present, isFalse);
    });

    test('a scoop / slide is not reported as vibrato', () {
      // Pitch sliding up 150¢ over 1.5 s, no oscillation.
      final n = (frameRate * 1.5).round();
      final f0 = Float32List(n);
      for (var i = 0; i < n; i++) {
        f0[i] = 220 * pow(2, 150 * (i / (n - 1)) / 1200).toDouble();
      }
      expect(analyzeVibrato(_vm(f0: f0), 0, n - 1).present, isFalse);
    });

    test(
      'vibrato riding on a drift reports the true extent (drift removed)',
      () {
        final n = (frameRate * 1.6).round();
        final f0 = Float32List(n);
        for (var i = 0; i < n; i++) {
          final cents =
              40 * sin(2 * pi * 6 * (i / frameRate)) + 150 * (i / (n - 1));
          f0[i] = 220 * pow(2, cents / 1200).toDouble();
        }
        final v = analyzeVibrato(_vm(f0: f0), 0, n - 1);
        expect(v.present, isTrue);
        expect(v.extentCents, inInclusiveRange(30, 55)); // ~40, not inflated
        expect(v.rateHz, inInclusiveRange(5, 7));
      },
    );

    test('the live analyzer measures the trailing window', () {
      // 3 s of straight tone then a 6 Hz ±50¢ vibrato: the trailing-window live
      // analyzer should lock onto the recent oscillation, not the silence/flat.
      final flat = Float32List((frameRate * 3).round())..fillRange(0, 0, 0);
      flat.fillRange(0, flat.length, 220);
      final vib = vibratoF0(6, 50, 2);
      final hist = <double>[...flat, ...vib];
      final v = analyzeVibratoLive(hist, frameRate);
      expect(v.present, isTrue);
      expect(v.rateHz, inInclusiveRange(5, 7));
      expect(v.extentCents, inInclusiveRange(40, 60));
    });
  });

  group('RegisterMap', () {
    test('classifies chest (low H1−H2) vs head (high H1−H2)', () {
      const n = 100;
      final f0 = Float32List(n)..fillRange(0, n, 220);
      final h = Float32List(n);
      for (var i = 0; i < n; i++) {
        h[i] = i < 50 ? -5.0 : 12.0; // chest then head
      }
      final r = analyzeRegister(_vm(f0: f0, h1h2: h));
      expect(r.hasData, isTrue);
      expect(r.chestPct, greaterThan(40));
      expect(r.headPct, greaterThan(40));
      expect(r.score[10], lessThan(0.4)); // chest end
      expect(r.score[90], greaterThan(0.6)); // head end
    });

    test('no voiced H1−H2 → no data', () {
      final f0 = Float32List(40)..fillRange(0, 40, 220);
      final r = analyzeRegister(_vm(f0: f0)); // h1h2 all NaN
      expect(r.hasData, isFalse);
    });
  });
}
