// Octave errors in the YIN pitch tracker on two common sung timbres: a breathy
// voice, whose period dips land in the gap between YIN's threshold and the
// voicing gate, and a belt-like note with its 2nd harmonic dominant.

import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

/// Source-filter vowel: glottal pulse train (with optional cycle jitter and
/// vibrato) through F1/F2/F3 resonators, plus aspiration noise that is
/// high-passed (breath) at [hnrDb] below the voiced part.
Float32List _breathyVowel({
  required int sr,
  double f0 = 220,
  double sec = 1.5,
  double hnrDb = 5,
  double jitter = 0.01,
  double vibCents = 40,
  double vibHz = 5.5,
  bool highPassNoise = true,
  int seed = 1,
}) {
  final rng = Random(seed);
  final n = (sr * sec).round();
  final src = Float32List(n);
  var t = 0.0;
  while (t < n) {
    src[t.floor()] = 1.0;
    final inst = f0 * pow(2, vibCents / 1200 * sin(2 * pi * vibHz * t / sr));
    t += sr / inst * (1 + jitter * (rng.nextDouble() * 2 - 1));
  }
  var g1 = 0.0, g2 = 0.0;
  for (var i = 0; i < n; i++) {
    g1 = src[i] + 0.99 * g1;
    g2 = g1 + 0.99 * g2;
    src[i] = g2 * 1e-3;
  }
  Float32List reson(Float32List x, double fc, double bw, double gain) {
    final r = exp(-pi * bw / sr);
    final th = 2 * pi * fc / sr;
    final a1 = -2 * r * cos(th), a2 = r * r;
    final out = Float32List(n);
    for (var i = 0; i < n; i++) {
      final y1 = i >= 1 ? out[i - 1] : 0.0;
      final y2 = i >= 2 ? out[i - 2] : 0.0;
      out[i] = gain * x[i] - a1 * y1 - a2 * y2;
    }
    return out;
  }

  final o1 = reson(src, 700, 70, 1),
      o2 = reson(src, 1200, 90, 0.6),
      o3 = reson(src, 2600, 120, 0.4);
  final v = Float32List(n);
  for (var i = 0; i < n; i++) {
    v[i] = o1[i] + o2[i] + o3[i];
  }
  var pv = 0.0;
  for (final x in v) {
    pv += x * x;
  }
  pv /= n;
  final nz = Float32List(n);
  var prev = 0.0;
  for (var i = 0; i < n; i++) {
    final u1 = rng.nextDouble().clamp(1e-12, 1.0), u2 = rng.nextDouble();
    final g = sqrt(-2 * log(u1)) * cos(2 * pi * u2);
    nz[i] = highPassNoise ? g - prev : g; // first-difference = HP tilt
    prev = g;
  }
  var pn = 0.0;
  for (final x in nz) {
    pn += x * x;
  }
  pn /= n;
  final k = sqrt(pv / pn / pow(10, hnrDb / 10));
  final out = Float32List(n);
  var peak = 1e-9;
  for (var i = 0; i < n; i++) {
    out[i] = v[i] + k * nz[i];
    peak = max(peak, out[i].abs());
  }
  for (var i = 0; i < n; i++) {
    out[i] = out[i] / peak * 0.5;
  }
  return out;
}

/// H2 [db] over H1 and H3 — a bright vowel with F1 on H2, as voice_metrics_test's
/// `_harmonic(h2OverDb:)`.
Float32List _h2Dominant(double f0, double sr, int n, double db) {
  final out = Float32List(n);
  final side = pow(10, -db / 20).toDouble();
  for (var h = 1; h <= 3; h++) {
    final a = h == 2 ? 1.0 : side;
    for (var i = 0; i < n; i++) {
      out[i] += 0.4 * a * sin(2 * pi * h * f0 * i / sr + 0.7 * h);
    }
  }
  return out;
}

void main() {
  test('a breathy A3 keeps its octave and its range', () {
    // No dip falls under YIN's threshold, but the frame still passes the
    // voicing gate; the fallback took the deepest dip, and with noise the
    // dips at T, 2T and 3T are nearly equal — a third of the frames read an
    // octave or a twelfth low, and the profile put a steady A3's range low
    // end at D2.
    final x = _breathyVowel(sr: 48000, hnrDb: 8, highPassNoise: true, seed: 2);
    final m = computeVoiceMetrics(x, 48000);
    var voiced = 0, wrongOctave = 0;
    for (final f in m.f0) {
      if (f.isNaN) continue;
      voiced++;
      if ((1200 * log(f / 220) / ln2).abs() > 600) wrongOctave++;
    }
    expect(voiced, greaterThan(100));
    expect(
      wrongOctave,
      lessThan(voiced ~/ 20),
      reason: '$wrongOctave of $voiced voiced frames an octave or more off',
    );
    expect(analyzeVoiceProfile(m).lowMidi, greaterThanOrEqualTo(55));
  });

  test('a note with its 2nd harmonic dominant reads its own pitch below '
      '~567 Hz too', () {
    // The double-period rescue ran only for a first dip above the band, so an
    // E4-C5 belt with F1 on H2 read an octave high: the same timbre was right
    // at 620 Hz and exactly doubled at 523 Hz.
    final bad = <String>[];
    for (final sr in [22050.0, 24000.0]) {
      for (final hz in [330.0, 392.0, 440.0, 494.0, 523.0, 620.0, 700.0]) {
        for (final db in [12.0, 16.0, 20.0]) {
          final y = yinPitch(_h2Dominant(hz, sr, 1024, db), sr);
          if (!(y.f0 > hz * 0.99 && y.f0 < hz * 1.01)) {
            bad.add('$hz Hz, H2 +$db dB at $sr: ${y.f0.toStringAsFixed(1)}');
          }
        }
      }
    }
    expect(bad, isEmpty, reason: bad.join('\n'));
  });
}
