import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';

const _sr = 44100;

/// Bitwise Float32List equality — NaN-safe (NaN != NaN under ==, but the
/// streaming/batch contract is BIT-identical output, NaNs included).
void _expectBitsEqual(Float32List a, Float32List b, String what) {
  expect(a.length, b.length, reason: '$what length');
  final ab = a.buffer.asUint32List(a.offsetInBytes, a.length);
  final bb = b.buffer.asUint32List(b.offsetInBytes, b.length);
  for (var i = 0; i < ab.length; i++) {
    expect(ab[i], bb[i], reason: '$what[$i] bits');
  }
}

void _expectScalarEqual(double a, double b, String what) {
  if (a.isNaN && b.isNaN) return;
  expect(a, b, reason: what);
}

/// Synthesizes a vowel: an impulse train at [f0] driven through three two-pole
/// resonators at the given formant frequencies — a textbook source-filter vowel
/// whose spectral envelope peaks at F1/F2/F3.
Float32List _vowel(
  double f1,
  double f2,
  double f3, {
  double f0 = 130,
  double sec = 1.0,
}) {
  final n = (_sr * sec).round();
  final src = Float32List(n);
  final period = (_sr / f0).round();
  for (var i = 0; i < n; i += period) {
    src[i] = 1.0;
  }
  // Approximate a glottal source roll-off (~ -12 dB/oct) so the pre-emphasis in
  // the formant estimator behaves as it does on real voiced audio (a flat
  // impulse train would otherwise bias the estimates upward).
  var g1 = 0.0, g2 = 0.0;
  for (var i = 0; i < n; i++) {
    g1 = src[i] + 0.99 * g1;
    g2 = g1 + 0.99 * g2;
    src[i] = g2 * 1e-3;
  }
  Float32List reson(double fc, double bw, double gain) {
    final r = exp(-pi * bw / _sr);
    final theta = 2 * pi * fc / _sr;
    final a1 = -2 * r * cos(theta);
    final a2 = r * r;
    final out = Float32List(n);
    for (var i = 0; i < n; i++) {
      final y1 = i >= 1 ? out[i - 1] : 0.0;
      final y2 = i >= 2 ? out[i - 2] : 0.0;
      out[i] = gain * src[i] - a1 * y1 - a2 * y2;
    }
    return out;
  }

  final o1 = reson(f1, 70, 1.0);
  final o2 = reson(f2, 90, 0.6);
  final o3 = reson(f3, 120, 0.4);
  final out = Float32List(n);
  var peak = 1e-9;
  for (var i = 0; i < n; i++) {
    out[i] = o1[i] + o2[i] + o3[i];
    if (out[i].abs() > peak) peak = out[i].abs();
  }
  for (var i = 0; i < n; i++) {
    out[i] /= peak * 1.1;
  }
  return out;
}

/// Sum of equal-amplitude sine tones — a controlled way to put energy in
/// specific bands for the ring/tilt metrics.
Float32List _tones(List<double> freqs, {double sec = 1.0}) {
  final n = (_sr * sec).round();
  final out = Float32List(n);
  for (final f in freqs) {
    for (var i = 0; i < n; i++) {
      out[i] += sin(2 * pi * f * i / _sr);
    }
  }
  var peak = 1e-9;
  for (final v in out) {
    if (v.abs() > peak) peak = v.abs();
  }
  for (var i = 0; i < n; i++) {
    out[i] /= peak * 1.1;
  }
  return out;
}

/// A pure sine at [hz] sampled at [sr], length [n] — for YIN boundary checks in
/// the decimated domain (sr≈11025, n=512).
Float32List _sine(double hz, double sr, int n) {
  final out = Float32List(n);
  for (var i = 0; i < n; i++) {
    out[i] = sin(2 * pi * hz * i / sr);
  }
  return out;
}

/// A harmonic tone at [f0]: H1..H[harmonics] with amplitude 1/hᵖ (p =
/// [rolloff]) and fixed phases — or, with [h2OverDb], a note whose 2nd
/// harmonic sits that many dB over H1 and H3 (a bright high vowel).
Float32List _harmonic(
  double f0,
  double sr,
  int n, {
  int harmonics = 6,
  double rolloff = 1,
  double? h2OverDb,
}) {
  final out = Float32List(n);
  final side = h2OverDb == null ? 0.0 : pow(10, -h2OverDb / 20).toDouble();
  for (var h = 1; h <= (h2OverDb == null ? harmonics : 3); h++) {
    if (h * f0 >= sr / 2) break;
    final a = h2OverDb == null ? 1 / pow(h, rolloff) : (h == 2 ? 1.0 : side);
    for (var i = 0; i < n; i++) {
      out[i] += 0.4 * a * sin(2 * pi * h * f0 * i / sr + 0.7 * h);
    }
  }
  return out;
}

Float32List _noise({double sec = 1.0, int seed = 7}) {
  final n = (_sr * sec).round();
  final rnd = Random(seed);
  final out = Float32List(n);
  for (var i = 0; i < n; i++) {
    out[i] = (rnd.nextDouble() * 2 - 1) * 0.5;
  }
  return out;
}

double _median(Float32List xs) {
  final v = [
    for (final x in xs)
      if (!x.isNaN) x,
  ]..sort();
  return v.isEmpty ? double.nan : v[v.length ~/ 2];
}

/// A sustained tone (with harmonics) whose pitch oscillates sinusoidally — a
/// synthetic vibrato at [rateHz] / ±[extentCents] around [f0].
Float32List _vibratoTone(
  double f0,
  double rateHz,
  double extentCents, {
  double sec = 1.6,
  int sr = _sr,
}) {
  final n = (sr * sec).round();
  final out = Float32List(n);
  var phase = 0.0;
  for (var i = 0; i < n; i++) {
    final t = i / sr;
    final ratio = pow(2, (extentCents / 1200) * sin(2 * pi * rateHz * t));
    phase += 2 * pi * f0 * ratio / sr;
    out[i] = 0.6 * sin(phase) + 0.3 * sin(2 * phase) + 0.15 * sin(3 * phase);
  }
  return out;
}

/// A sung vowel at [f0]: a band-limited harmonic source at -12 dB/octave through
/// one resonator per formant (80 Hz bandwidth), normalized to [amp] peak.
Float32List _sungVowel(
  double f0, {
  required List<double> formants,
  double sec = 1.0,
  double amp = 0.3,
}) {
  final n = (_sr * sec).round();
  var x = Float64List(n);
  var phase = 0.0;
  for (var i = 0; i < n; i++) {
    phase += 2 * pi * f0 / _sr;
    var v = 0.0;
    for (var k = 1; k * f0 < _sr / 2 * 0.95; k++) {
      v += sin(k * phase) / (k * k);
    }
    x[i] = v;
  }
  for (final fc in formants) {
    final r = exp(-pi * 80 / _sr);
    final a1 = -2 * r * cos(2 * pi * fc / _sr), a2 = r * r;
    final out = Float64List(n);
    for (var i = 0; i < n; i++) {
      out[i] =
          x[i] -
          a1 * (i >= 1 ? out[i - 1] : 0) -
          a2 * (i >= 2 ? out[i - 2] : 0);
    }
    x = out;
  }
  var peak = 0.0;
  for (final v in x) {
    peak = max(peak, v.abs());
  }
  return Float32List.fromList([for (final v in x) v / peak * amp]);
}

void main() {
  test('StreamingTakeAnalyzer equals computeVoiceMetrics BIT-FOR-BIT over '
      'ragged chunk boundaries (batch is the oracle)', () {
    // Same _MetricsPass body, decimators reproducing _decimate exactly,
    // identical zero-pad edges — pinned here, NaNs compared by bits.
    for (final (sr, seconds) in [(_sr, 1.2), (48000, 0.9), (22050, 0.7)]) {
      final base = _vibratoTone(220, 5.5, 30, sec: seconds, sr: sr);
      // Splice in silence and a noisy tail so voicing gates/NaN paths and
      // the zero-padded final frame all exercise.
      final n = base.length;
      final samples = Float32List(n + 700);
      samples.setRange(0, n, base);
      final rng = Random(11);
      for (var i = 0; i < 500; i++) {
        samples[n + 200 + i] = (rng.nextDouble() - 0.5) * 0.02;
      }
      final batch = computeVoiceMetrics(samples, sr);

      for (final chunkSizes in [
        [samples.length],
        [1, 313, 4097, samples.length - 1 - 313 - 4097],
        List.filled(samples.length ~/ 777, 777) + [samples.length % 777],
      ]) {
        final an = StreamingTakeAnalyzer(
          sr,
          expectedFrames: batch.frames,
          spectrogram: true,
        );
        var at = 0;
        for (final c in chunkSizes) {
          if (c == 0) continue;
          an.add(Float32List.sublistView(samples, at, at + c));
          at += c;
        }
        expect(at, samples.length, reason: 'fixture sanity');
        final out = an.finish();
        final m = out.metrics;
        expect(m.frames, batch.frames, reason: 'sr=$sr frames');
        _expectBitsEqual(m.ring, batch.ring, 'ring');
        _expectBitsEqual(m.tilt, batch.tilt, 'tilt');
        _expectBitsEqual(m.cpp, batch.cpp, 'cpp');
        _expectBitsEqual(m.loudness, batch.loudness, 'loudness');
        _expectBitsEqual(m.f0, batch.f0, 'f0');
        _expectBitsEqual(m.f1, batch.f1, 'f1');
        _expectBitsEqual(m.f2, batch.f2, 'f2');
        _expectBitsEqual(m.f3, batch.f3, 'f3');
        _expectBitsEqual(m.h1h2, batch.h1h2, 'h1h2');
        expect(m.active, batch.active, reason: 'active');
        _expectScalarEqual(m.ringAvg, batch.ringAvg, 'ringAvg');
        _expectScalarEqual(m.tiltAvg, batch.tiltAvg, 'tiltAvg');
        _expectScalarEqual(m.cppAvg, batch.cppAvg, 'cppAvg');
        _expectScalarEqual(m.peakDbfs, batch.peakDbfs, 'peakDbfs');
        _expectScalarEqual(
          m.clippedFraction,
          batch.clippedFraction,
          'clippedFraction',
        );
        _expectScalarEqual(m.snrDb, batch.snrDb, 'snrDb');
        // The bundled spectrogram equals the batch one too.
        final spec = out.spectrogram!;
        final bSpec = computeSpectrogram(samples, sr);
        expect(spec.frames, bSpec.frames);
        _expectBitsEqual(spec.db, bSpec.db, 'spectrogram db');
      }
    }

    // Sub-fftSize and empty inputs match the batch edge semantics.
    final tiny = Float32List(300);
    for (var i = 0; i < 300; i++) {
      tiny[i] = sin(i / 5) * 0.4;
    }
    final tb = computeVoiceMetrics(tiny, _sr);
    final ta = StreamingTakeAnalyzer(_sr, expectedFrames: tb.frames)..add(tiny);
    expect(ta.finish().metrics.frames, tb.frames);
    final eb = computeVoiceMetrics(Float32List(0), _sr);
    final ea = StreamingTakeAnalyzer(_sr, expectedFrames: 0);
    expect(ea.finish().metrics.frames, eb.frames);
  });

  test('formants land near a synthetic /a/ (F1~700, F2~1200)', () {
    final m = computeVoiceMetrics(_vowel(700, 1200, 2600), _sr);
    expect(_median(m.f1), inInclusiveRange(560, 860));
    expect(_median(m.f2), inInclusiveRange(1040, 1420));
  });

  test('formants track a different vowel /i/ (F1~320, F2~2300)', () {
    final m = computeVoiceMetrics(_vowel(320, 2300, 3200), _sr);
    expect(_median(m.f1), inInclusiveRange(220, 500));
    expect(_median(m.f2), inInclusiveRange(2020, 2580));
  });

  test('LPC formant tracks are stable (low jitter) on a sustained vowel', () {
    final m = computeVoiceMetrics(_vowel(700, 1200, 2600), _sr);
    final vals = [
      for (final x in m.f1)
        if (!x.isNaN) x,
    ];
    expect(vals.length, greaterThan(10));
    final mean = vals.reduce((a, b) => a + b) / vals.length;
    final sd = sqrt(
      vals.map((v) => (v - mean) * (v - mean)).reduce((a, b) => a + b) /
          vals.length,
    );
    expect(sd, lessThan(60), reason: 'F1 stddev=$sd Hz (should be steady)');
  });

  test('F0 tracks the fundamental of a synthetic vowel', () {
    final m = computeVoiceMetrics(_vowel(700, 1200, 2600, f0: 196), _sr);
    final vals = [
      for (final x in m.f0)
        if (!x.isNaN) x,
    ]..sort();
    expect(vals.length, greaterThan(10));
    final med = vals[vals.length ~/ 2];
    expect(med, inInclusiveRange(186, 206), reason: 'F0 median=$med Hz');
  });

  test('F0 stays accurate for a high note (lightly-decimated YIN path)', () {
    // E5 (~660 Hz): with YIN on the coarse 11 kHz formant decimation, the lag
    // spacing here is ~1 semitone and prone to octave/quantization error. The
    // ~22 kHz YIN path must still land on the root.
    final m = computeVoiceMetrics(_vowel(700, 1200, 2600, f0: 660), _sr);
    final vals = [
      for (final x in m.f0)
        if (!x.isNaN) x,
    ]..sort();
    expect(vals.length, greaterThan(10));
    final med = vals[vals.length ~/ 2];
    expect(
      med,
      inInclusiveRange(630, 690),
      reason: 'F0 median=$med Hz (E5≈660)',
    );
  });

  test('vibrato jitter is finite and low for an even vibrato', () {
    final m = computeVoiceMetrics(_vibratoTone(440, 5.5, 60), _sr);
    final v = analyzeVibrato(m, 0, m.frames - 1);
    expect(v.present, isTrue, reason: 'should detect the 5.5 Hz vibrato');
    expect(v.rateHz, closeTo(5.5, 1.0));
    expect(v.jitter.isNaN, isFalse);
    expect(v.jitter, lessThan(0.15), reason: 'even-vibrato jitter=${v.jitter}');
  });

  test('yinPitch tracks a pure tone with high periodicity', () {
    final sine = _tones([220]);
    final y = yinPitch(Float32List.sublistView(sine, 0, 2048), _sr.toDouble());
    expect(y.f0, closeTo(220, 3));
    expect(y.periodicity, greaterThan(0.9));
  });

  test('yinPitch is octave-robust (strong 2nd harmonic stays at the root)', () {
    // 150 Hz with a 2nd harmonic LOUDER than the fundamental — the classic case
    // where autocorrelation/cepstrum report the octave (300 Hz). YIN should not.
    const f0 = 150.0;
    final n = 2048;
    final frame = Float32List(n);
    for (var i = 0; i < n; i++) {
      frame[i] =
          0.6 * sin(2 * pi * f0 * i / _sr) +
          1.0 * sin(2 * pi * 2 * f0 * i / _sr);
    }
    final y = yinPitch(frame, _sr.toDouble());
    expect(
      y.f0,
      closeTo(f0, 6),
      reason: 'should lock the root, not the octave',
    );
  });

  test('yinPitch returns NaN (not a crash) on tiny frames', () {
    for (final len in [0, 1, 2, 3]) {
      final y = yinPitch(Float32List(len), 11025);
      expect(y.f0.isNaN, isTrue, reason: 'len=$len should be NaN, not throw');
    }
  });

  test('yinDifference matches the one-lag loop bit for bit (3000 frames, '
      'every lag-count remainder)', () {
    // YIN step 1 as it was: one accumulator per lag, summed in j order.
    Float64List oneLag(Float32List x, int tauMax, int w) {
      final d = Float64List(tauMax + 1);
      for (var tau = 1; tau <= tauMax; tau++) {
        var sum = 0.0;
        for (var j = 0; j < w; j++) {
          final diff = x[j] - x[j + tau];
          sum += diff * diff;
        }
        d[tau] = sum;
      }
      return d;
    }

    final rng = Random(11);
    for (var t = 0; t < 3000; t++) {
      final n = const [1024, 2048, 1000, 517, 9][t % 5];
      final rate = const [24000.0, 22050.0, 48000.0, 44100.0][t % 4];
      final hz = 60 + rng.nextDouble() * 1200;
      final noise = (t % 3) * 0.2;
      final frame = Float32List(n);
      for (var i = 0; i < n; i++) {
        frame[i] =
            0.5 * sin(2 * pi * hz * i / rate) +
            noise * (rng.nextDouble() - 0.5);
      }
      // yinPitch's own bounds, plus arbitrary ones so every tauMax % 8
      // (the eight- and four-lag sweeps' tails) is exercised.
      final tauMax = t.isEven
          ? min((rate / 63).ceil(), n - 1)
          : 1 + rng.nextInt(n - 2);
      final w = n - tauMax;
      final want = oneLag(frame, tauMax, w);
      final got = yinDifference(frame, tauMax, w);
      expect(
        got.buffer.asUint64List(),
        want.buffer.asUint64List(),
        reason: 'frame $t (n=$n, tauMax=$tauMax)',
      );
    }
  });

  test('yinPitch rejects a sub-fmin tone instead of reporting a false ~fmin', () {
    // 60 Hz is below fmin (70 Hz). The boundary lag used to read a false ~70 Hz
    // with high confidence and lock the tuner a semitone sharp; now it's NaN.
    final y = yinPitch(_sine(60, 11025, 512), 11025, fmin: 70, fmax: 1100);
    expect(
      y.f0.isNaN,
      isTrue,
      reason: 'got ${y.f0} Hz (should be out-of-band)',
    );
  });

  test(
    'yinPitch resolves a tone at the very top of the range (no dead-band)',
    () {
      final y = yinPitch(_sine(1100, 11025, 512), 11025, fmin: 70, fmax: 1100);
      expect(
        y.f0,
        closeTo(1100, 20),
        reason: 'top-of-range tone must not drop',
      );
    },
  );

  test('yinPitch reads an above-fmax tone as NaN, not one octave low', () {
    // A period shorter than the search band's first lag has its first d′ dip at
    // 2T, which the range gate accepted as f0/2 — every whistle-register note
    // above ~1.36 kHz read an octave low with full confidence.
    for (final hz in [1397.0, 1568.0, 1760.0, 2093.0]) {
      final y = yinPitch(_sine(hz, _sr.toDouble(), 2048), _sr.toDouble());
      expect(y.f0.isNaN, isTrue, reason: '$hz Hz read as ${y.f0} Hz');
    }
    // The batch pass agrees: no frame of a held high note carries a pitch.
    final m = computeVoiceMetrics(_tones([1760]), _sr);
    expect(m.f0.where((v) => !v.isNaN), isEmpty);
  });

  test('yinPitch reads a high note whose 2nd harmonic dominates at its own '
      'pitch, not NaN', () {
    // H2 16-20 dB over H1/H3 repeats almost exactly at T/2, below the band
    // for any note above ~605 Hz: the scan's first dip sat there and the
    // range gate dropped a real, in-band note. 22.05/24 kHz are the
    // decimated YIN rates the take analysis runs at for 44.1/48 kHz input.
    for (final sr in [22050.0, 24000.0, 44100.0, 48000.0]) {
      final n = sr < 30000 ? 1024 : 2048;
      for (final hz in [620.0, 700.0, 800.0, 950.0, 1050.0]) {
        for (final db in [16.0, 20.0]) {
          final y = yinPitch(_harmonic(hz, sr, n, h2OverDb: db), sr);
          expect(
            y.f0,
            closeTo(hz, hz * 0.01),
            reason: '$hz Hz, H2 +$db dB at $sr Hz read as ${y.f0}',
          );
          expect(y.periodicity, greaterThan(0.9));
        }
      }
    }
    // End to end: a held bright 700 Hz note carries its pitch in the take.
    final note = _harmonic(700, _sr.toDouble(), _sr, h2OverDb: 16);
    final voiced = computeVoiceMetrics(note, _sr).f0.where((v) => !v.isNaN);
    expect(voiced, isNotEmpty);
    expect(_median(Float32List.fromList(voiced.toList())), closeTo(700, 7));
  });

  test('an above-band tone still reads NaN, bright timbres included', () {
    // The rescue above must never turn an above-band tone into its octave-
    // low double: integer-lag misalignment can leave a bright tone's 2T dip
    // deeper than its T dip, and only the absent fundamental gives it away.
    for (final sr in [22050.0, 24000.0, 44100.0]) {
      final n = sr < 30000 ? 1024 : 2048;
      for (var hz = 1250.0; hz < sr / 4; hz *= 1.005) {
        for (final rolloff in [0.0, 1.0, 2.0]) {
          final y = yinPitch(
            _harmonic(
              hz,
              sr,
              n,
              harmonics: rolloff == 0 ? 1 : 6,
              rolloff: rolloff,
            ),
            sr,
          );
          expect(
            y.f0.isNaN,
            isTrue,
            reason: '$hz Hz (1/h^$rolloff) at $sr Hz read as ${y.f0} Hz',
          );
        }
      }
    }
  });

  test('yinPitch periodicity is far lower for noise than for a tone', () {
    final tone = yinPitch(
      Float32List.sublistView(_tones([220]), 0, 2048),
      _sr.toDouble(),
    );
    final noise = yinPitch(
      Float32List.sublistView(_noise(), 0, 2048),
      _sr.toDouble(),
    );
    expect(tone.periodicity, greaterThan(0.9));
    expect(
      noise.periodicity,
      lessThan(tone.periodicity - 0.4),
      reason: 'noise periodicity=${noise.periodicity} tone=${tone.periodicity}',
    );
  });

  test(
    'segmentPitchNotes groups a steady vowel into one note near its pitch',
    () {
      final m = computeVoiceMetrics(
        _vowel(700, 1200, 2600, f0: 196),
        _sr,
      ); // G3
      final notes = segmentPitchNotes(m);
      expect(notes, isNotEmpty);
      final main = notes.reduce((a, b) => a.frames >= b.frames ? a : b);
      expect(main.semitone, 55, reason: 'G3 = MIDI 55');
      expect(main.medianCents.abs(), lessThan(20));
      expect(pitchInTunePercent(notes), greaterThan(80));
    },
  );

  test(
    'a ~50¢-off note (near a semitone boundary) survives as one off note',
    () {
      // 201.7 Hz ≈ 50 cents above G3 — without hysteresis this would flip-flop
      // round() and fragment into dropped sub-min runs, vanishing entirely.
      final m = computeVoiceMetrics(_vowel(700, 1200, 2600, f0: 201.7), _sr);
      final notes = segmentPitchNotes(m);
      expect(notes, isNotEmpty);
      final main = notes.reduce((a, b) => a.frames >= b.frames ? a : b);
      expect(
        main.medianCents.abs(),
        greaterThan(35),
        reason: 'should stay flagged off-pitch, not disappear',
      );
    },
  );

  test('pitchSemitoneRange brackets the F0 with at least an octave span', () {
    final m = computeVoiceMetrics(_vowel(700, 1200, 2600, f0: 196), _sr);
    final r = pitchSemitoneRange(m); // 196 Hz ≈ G3 ≈ MIDI 55
    expect(r.lo, lessThan(55));
    expect(r.hi, greaterThan(55));
    expect(r.hi - r.lo, greaterThanOrEqualTo(12));
  });

  test('noteLabel maps frequencies to note names', () {
    expect(noteLabel(440), startsWith('A4'));
    expect(noteLabel(261.63), startsWith('C4'));
    expect(noteLabel(double.nan), '—');
  });

  test('noteOf returns the nearest note name + signed cents', () {
    final a = noteOf(440)!;
    expect(a.name, 'A4');
    expect(a.cents.abs(), lessThan(1));
    final sharp = noteOf(445)!; // ~+20¢ above A4
    expect(sharp.name, 'A4');
    expect(sharp.cents, greaterThan(10));
    expect(noteOf(double.nan), isNull);
    expect(noteOf(0), isNull);
  });

  test('loudness tracks signal amplitude (dB drops ~20 for a 0.1x signal)', () {
    final base = _tones([440]);
    final loud = computeVoiceMetrics(base, _sr);
    final quiet = computeVoiceMetrics(
      Float32List.fromList([for (final s in base) s * 0.1]),
      _sr,
    );
    double peak(Float32List x) => x.reduce((a, b) => a > b ? a : b);
    expect(
      peak(loud.loudness),
      greaterThan(peak(quiet.loudness) + 10),
      reason: 'loud peak=${peak(loud.loudness)} quiet=${peak(quiet.loudness)}',
    );
  });

  test(
    'TunerTracker locks a sustained note after the attack, releases silence',
    () {
      final t = TunerTracker();
      expect(t.add(double.nan, 0).isNaN, isTrue); // silence → nothing
      var out = double.nan;
      for (var i = 0; i < 10; i++) {
        out = t.add(220, 1.0); // a sustained, clearly-voiced 220 Hz note
      }
      expect(out, closeTo(220, 5));
      for (var i = 0; i < 10; i++) {
        out = t.add(double.nan, 0); // go silent
      }
      expect(out.isNaN, isTrue); // released
    },
  );

  test('TunerTracker ignores a brief blip shorter than the attack', () {
    final t = TunerTracker(attack: 4);
    expect(t.add(220, 1.0).isNaN, isTrue);
    expect(t.add(220, 1.0).isNaN, isTrue);
    expect(t.add(double.nan, 0).isNaN, isTrue); // blip ended, never locked
  });

  test('TunerTracker rejects low-confidence (breathy / noise) frames', () {
    final t = TunerTracker(minConfidence: 0.4);
    var out = 1.0;
    for (var i = 0; i < 10; i++) {
      out = t.add(220, 0.2); // below the periodicity threshold
    }
    expect(out.isNaN, isTrue);
  });

  test('maxAnalysisSampleRate pins the YIN envelope: voiced at the ceiling, '
      'structurally unvoiced past the dead bound', () {
    // The YIN slice spans fftSize / sampleRate seconds while the fmin search
    // lag is a fixed 1/63 s, so past ~129 kHz yinPitch bails out unvoiced for
    // EVERY frame regardless of content — the reason the admission gate
    // refuses rates above [maxAnalysisSampleRate]. Pin both sides so the
    // ceiling can't drift away from the math it is derived from.
    Float32List tone(int sr) {
      final n = sr; // 1 s
      final out = Float32List(n);
      for (var i = 0; i < n; i++) {
        out[i] = 0.6 * sin(2 * pi * 220 * i / sr);
      }
      return out;
    }

    int voiced(VoiceMetrics m) {
      var c = 0;
      for (final f in m.f0) {
        if (!f.isNaN) c++;
      }
      return c;
    }

    final atCeiling = computeVoiceMetrics(
      tone(maxAnalysisSampleRate),
      maxAnalysisSampleRate,
    );
    expect(
      voiced(atCeiling),
      greaterThan(0),
      reason: 'the admitted envelope must actually be analyzable',
    );

    final dead = computeVoiceMetrics(tone(192000), 192000);
    expect(
      voiced(dead),
      0,
      reason:
          '192 kHz is past the structural dead bound — this is what the '
          'gate exists to refuse',
    );
  });

  test('CPP is higher for a periodic voice than for noise', () {
    final voice = computeVoiceMetrics(_vowel(700, 1200, 2600), _sr);
    final noise = computeVoiceMetrics(_noise(), _sr);
    expect(voice.cppAvg, isNot(isNaN));
    // Noise voices no frame, so it has no CPP average at all ("—"), which is
    // the sharpest form of "lower". Per frame, the voice still beats it.
    expect(noise.cppAvg, isNaN);
    expect(_median(voice.cpp), greaterThan(_median(noise.cpp)));
  });

  test('a take with no voiced frame reports its timbre averages as no data', () {
    // Breath, a whispered exercise or a muted mic: there is no voiced frame to
    // average. A 0.0 here read as a measurement ("Clarity 0.0", a gap of
    // several dB to the technique reference); NaN is what every readout
    // renders as "—", and what the live monitor's averages already answer.
    final takes = {
      'noise': computeVoiceMetrics(_noise(), _sr),
      'silence': computeVoiceMetrics(Float32List(_sr), _sr),
      'empty': computeVoiceMetrics(Float32List(0), _sr),
    };
    for (final MapEntry(key: what, value: m) in takes.entries) {
      expect(m.f0.where((v) => v.isFinite), isEmpty, reason: what);
      expect(m.ringAvg, isNaN, reason: '$what ringAvg');
      expect(m.tiltAvg, isNaN, reason: '$what tiltAvg');
      expect(m.cppAvg, isNaN, reason: '$what cppAvg');
    }
  });

  test('ring index rises when there is strong 2–4 kHz energy', () {
    final bright = computeVoiceMetrics(_tones([400, 3000]), _sr);
    final dark = computeVoiceMetrics(_tones([400, 700]), _sr);
    expect(bright.ringAvg, greaterThan(dark.ringAvg));
  });

  test('spectral tilt is higher (brighter) for more high-frequency energy', () {
    final bright = computeVoiceMetrics(_tones([400, 3000, 4000]), _sr);
    final dark = computeVoiceMetrics(_tones([300, 500]), _sr);
    expect(bright.tiltAvg, greaterThan(dark.tiltAvg));
  });

  test('LiveVoiceAnalyzer emits correct metrics incrementally (chunked)', () {
    final analyzer = LiveVoiceAnalyzer();
    final samples = _vowel(700, 1200, 2600, f0: 200);
    final frames = <LiveFrame>[];
    // Feed in ~1000-sample packets, like a mic stream.
    for (var i = 0; i < samples.length; i += 1000) {
      final end = i + 1000 < samples.length ? i + 1000 : samples.length;
      analyzer.addSamples(Float32List.sublistView(samples, i, end), frames.add);
    }
    expect(frames.length, greaterThan(10));
    final f1s = [
      for (final f in frames)
        if (!f.f1.isNaN) f.f1,
    ]..sort();
    final f0s = [
      for (final f in frames)
        if (!f.f0.isNaN) f.f0,
    ]..sort();
    expect(f1s, isNotEmpty);
    expect(f0s, isNotEmpty);
    expect(
      f1s[f1s.length ~/ 2],
      inInclusiveRange(560, 860),
      reason: 'live F1 median',
    );
    expect(
      f0s[f0s.length ~/ 2],
      inInclusiveRange(186, 214),
      reason: 'live F0 median',
    );
  });

  test(
    'LiveVoiceAnalyzer draws no F0/formant dots on loud noise (voicing gate)',
    () {
      // Loud white noise: well above the RMS floor (so the old loudness-only gate
      // would have drawn formant dots all over it) but aperiodic, so the YIN
      // periodicity gate must reject it. Guards the "dots on noise" regression.
      final analyzer = LiveVoiceAnalyzer();
      final noise = _noise(sec: 1.0);
      final frames = <LiveFrame>[];
      for (var i = 0; i < noise.length; i += 1000) {
        final end = i + 1000 < noise.length ? i + 1000 : noise.length;
        analyzer.addSamples(Float32List.sublistView(noise, i, end), frames.add);
      }
      expect(frames.length, greaterThan(10));
      final voiced = frames.where((f) => f.active).length;
      expect(
        voiced / frames.length,
        lessThan(0.15),
        reason: 'aperiodic noise should almost never read as voiced',
      );
      final dotted = frames.where((f) => !f.f1.isNaN || !f.f0.isNaN).length;
      expect(
        dotted / frames.length,
        lessThan(0.15),
        reason: 'no pitch/formant dots on noise',
      );
    },
  );

  test(
    'LiveVoiceAnalyzer delays dots until a note is sustained (overlay attack)',
    () {
      // The overlay sustain gate suppresses the first few voiced frames so brief
      // voiced blips never draw dots; a held note still shows F0 once sustained.
      final analyzer = LiveVoiceAnalyzer();
      final samples = _vowel(700, 1200, 2600, f0: 200);
      final frames = <LiveFrame>[];
      for (var i = 0; i < samples.length; i += 1000) {
        final end = i + 1000 < samples.length ? i + 1000 : samples.length;
        analyzer.addSamples(
          Float32List.sublistView(samples, i, end),
          frames.add,
        );
      }
      final firstDot = frames.indexWhere((f) => !f.f0.isNaN);
      expect(
        firstDot,
        greaterThan(0),
        reason: 'onset frames must be suppressed',
      );
      expect(
        [
          for (final f in frames)
            if (!f.f0.isNaN) f.f0,
        ],
        isNotEmpty,
        reason: 'a held note still shows F0 after the attack',
      );
    },
  );

  test('the live analyzers do not depend on how their input is chunked, and '
      'reset starts a session afresh', () {
    // A vowel between silences, so frames cross the voicing and sustain
    // gates both ways.
    final vowel = _vowel(700, 1200, 2600, f0: 200);
    final samples = Float32List(vowel.length + 20000)
      ..setRange(9000, 9000 + vowel.length, vowel);
    final n = samples.length;
    // Every LiveFrame field and every StreamingStft column, as bits (NaN-safe).
    Uint64List run(
      LiveVoiceAnalyzer live,
      StreamingStft stft,
      List<int> chunks, {
      int? stopAt,
    }) {
      final out = <double>[], cols = <double>[];
      var at = 0, k = 0;
      while (at < (stopAt ?? n)) {
        final chunk = Float32List.sublistView(
          samples,
          at,
          min(n, at + chunks[k++ % chunks.length]),
        );
        live.addSamples(chunk, (f) {
          out
            ..addAll(f.column)
            ..addAll([f.ring, f.tilt, f.cpp, f.f0, f.f1, f.f2, f.f3])
            ..addAll([f.f0Confidence, f.h1h2, f.loudness, f.active ? 1 : 0]);
        });
        stft.addSamples(chunk, cols.addAll);
        at += chunk.length;
      }
      return Float64List.fromList(out + cols).buffer.asUint64List();
    }

    final whole = run(LiveVoiceAnalyzer(), StreamingStft(), [n]);
    expect(whole.length, greaterThan(1000));
    for (final chunks in [
      [1],
      [480],
      [1440],
      [1, 511, 2049, 0, 7, 9000],
    ]) {
      expect(
        run(LiveVoiceAnalyzer(), StreamingStft(), chunks),
        whole,
        reason: 'chunks $chunks',
      );
    }
    // A session cut off mid-vowel, then reset: the next one is a fresh one.
    final live = LiveVoiceAnalyzer();
    final stft = StreamingStft();
    run(live, stft, [1000], stopAt: 30000);
    live.reset();
    stft.reset();
    expect(run(live, stft, [1440]), whole, reason: 'after reset');
  });

  test('LiveVoiceAnalyzer gates out quiet (room-noise-level) frames', () {
    final analyzer = LiveVoiceAnalyzer();
    // A vowel attenuated to ~-66 dBFS — below the VAD floor.
    final quiet = Float32List.fromList([
      for (final s in _vowel(700, 1200, 2600, f0: 200)) s * 0.002,
    ]);
    final frames = <LiveFrame>[];
    for (var i = 0; i < quiet.length; i += 1000) {
      final end = i + 1000 < quiet.length ? i + 1000 : quiet.length;
      analyzer.addSamples(Float32List.sublistView(quiet, i, end), frames.add);
    }
    expect(
      [
        for (final f in frames)
          if (!f.f0.isNaN) f.f0,
      ],
      isEmpty,
      reason: 'noise-level input should not register as voiced pitch',
    );
  });

  test('truly empty input yields zero frames without throwing', () {
    final m = computeVoiceMetrics(Float32List(0), _sr);
    expect(m.frames, 0);
    expect(m.ringAvg, isNaN); // nothing to average: no data, not 0 dB
  });

  test(
    'a sub-fftSize take still yields a (zero-padded) frame, not empty curves',
    () {
      final m = computeVoiceMetrics(Float32List(1000), _sr); // < 2048 samples
      expect(m.frames, greaterThanOrEqualTo(1));
      expect(m.f1.length, m.frames);
    },
  );

  test('voicing gate survives median smoothing: no F0/formants painted onto a '
      'mid-take silence gap', () {
    const fft = 2048, hop = 512; // computeVoiceMetrics defaults
    final vowel = _vowel(700, 1200, 2600, f0: 200, sec: 0.7);
    final gapLen = (_sr * 0.35).round();
    final n = vowel.length * 2 + gapLen;
    final samples = Float32List(n)
      ..setRange(0, vowel.length, vowel)
      ..setRange(vowel.length + gapLen, n, vowel);
    final m = computeVoiceMetrics(samples, _sr);

    // Frames whose analysis window lies FULLY inside the gap are pure
    // silence: the gate must keep them NaN even though the ±2-frame median
    // bridges NaN dropouts on voiced frames (the voicing mask is re-applied
    // after smoothing).
    final firstFull = (vowel.length / hop).ceil();
    final lastFull = (vowel.length + gapLen - fft) ~/ hop;
    expect(
      lastFull - firstFull,
      greaterThan(10),
      reason: 'the gap should span many frames',
    );
    for (var fr = firstFull; fr <= lastFull; fr++) {
      expect(m.f0[fr].isNaN, isTrue, reason: 'f0 painted onto silence @$fr');
      expect(m.f1[fr].isNaN, isTrue, reason: 'f1 painted onto silence @$fr');
      expect(
        m.h1h2[fr].isNaN,
        isTrue,
        reason: 'h1h2 painted onto silence @$fr',
      );
    }
    // Sanity: the voiced regions themselves do carry pitch.
    expect(
      [for (var fr = 10; fr < 40; fr++) m.f0[fr]].any((v) => !v.isNaN),
      isTrue,
    );
  });

  test('median smoothing bridges measurement dropouts on VOICED frames '
      '(h1h2 holes from 2·F0 crossing Nyquist are filled)', () {
    // A pure FM sine around 1000 Hz at sr 4000: every frame is loud and
    // periodic (voiced) and F0 stays in-band, but H1−H2 is only measurable
    // while 2·F0 < Nyquist (est. F0 < 1000 Hz). The slow FM therefore
    // punches deterministic few-frame NaN holes into the raw h1h2 track on
    // VOICED frames — exactly the measurement dropouts the ±2-frame median
    // must bridge (while the previous test pins that gate-NaN stays NaN).
    const sr = 4000;
    final n = sr * 2;
    final samples = Float32List(n);
    var phase = 0.0;
    for (var i = 0; i < n; i++) {
      final hz = 1000 + 60 * sin(2 * pi * 5 * i / sr);
      phase += 2 * pi * hz / sr;
      samples[i] = 0.7 * sin(phase);
    }
    final m = computeVoiceMetrics(samples, sr, fftSize: 512, hop: 128);

    // Interior frames only (the zero-padded edge windows may gate unvoiced).
    var voiced = 0, h1h2Finite = 0;
    for (var fr = 3; fr < m.frames - 3; fr++) {
      if (!m.f0[fr].isNaN) voiced++;
      if (!m.h1h2[fr].isNaN) h1h2Finite++;
    }
    expect(
      voiced,
      greaterThan(((m.frames - 6) * 0.9).round()),
      reason: 'the tone should be voiced throughout',
    );
    // Without bridging, only the below-1000 Hz half of each FM cycle carries
    // h1h2 (~50% of voiced frames); with dropout bridging nearly all do.
    expect(h1h2Finite, greaterThan((voiced * 0.8).round()));
  });

  test('frame count matches the spectrogram framing (alignment invariant)', () {
    // Any non-empty take: the formant overlay and per-frame playhead read must
    // line up with the spectrogram columns. (len==0 is the one pathological case
    // — fftea still emits a padded column — and is handled by a UI frames>0
    // guard rather than by matching here.)
    for (final len in [1000, 2048, 3000, 22050, 44100, 50000, 100000]) {
      final samples = Float32List(len);
      final m = computeVoiceMetrics(samples, _sr);
      final s = computeSpectrogram(samples, _sr);
      expect(
        m.frames,
        s.frames,
        reason: 'len=$len: metrics=${m.frames} spectrogram=${s.frames}',
      );
    }
  });
  // Each formant track is median-smoothed on its own and their gaps differ, so
  // at a vowel change F2 took a later frame's value and F3 an earlier one.
  test('smoothed formants stay ordered across a vowel change', () {
    // /E/ (600/1800/2500) into a front rounded /y/ (300/1900/2200), at E4.
    final a = _sungVowel(330, formants: [600, 1800, 2500], sec: 0.25);
    final b = _sungVowel(330, formants: [300, 1900, 2200], sec: 0.25);
    final m = computeVoiceMetrics(Float32List.fromList([...a, ...b]), _sr);
    var both = 0;
    for (var fr = 0; fr < m.frames; fr++) {
      final f1 = m.f1[fr], f2 = m.f2[fr], f3 = m.f3[fr];
      if (f2.isFinite && f3.isFinite) {
        both++;
        expect(f2, lessThan(f3), reason: 'frame $fr: F2=$f2 F3=$f3');
      }
      if (f1.isFinite && f2.isFinite) {
        expect(f1, lessThan(f2), reason: 'frame $fr: F1=$f1 F2=$f2');
      }
    }
    expect(both, greaterThan(10), reason: 'the vowels must be measured');
  });

  test('orderedFormants sorts the measured ones and keeps the gaps', () {
    expect(orderedFormants(600, 1900, 1800), (600, 1800, 1900));
    expect(orderedFormants(2500, 700, 1200), (700, 1200, 2500));
    final (a, b, c) = orderedFormants(double.nan, 2000, 1900);
    expect(a.isNaN, isTrue);
    expect((b, c), (1900, 2000));
    final (d, e, f) = orderedFormants(1000, double.nan, 900);
    expect((d, f), (900, 1000));
    expect(e.isNaN, isTrue);
  });

  test("a DC offset leaves the singer's-formant reading alone", () {
    // Soft singing (-30 dBFS peak) on an interface with a -40 dBFS DC offset:
    // read from bin 0, the offset was taken for the voice's low peak.
    final clean = _sungVowel(
      220,
      formants: [600, 1100, 3000],
      sec: 2,
      amp: 0.03,
    );
    final offset = Float32List.fromList([for (final v in clean) v + 0.01]);
    double sf(Float32List s) => computeLtas(
      computeSpectrogram(s, _sr),
      computeVoiceMetrics(s, _sr),
    ).singersFormantDb;
    expect((sf(clean) - sf(offset)).abs(), lessThan(1.0));
  });

  test('clipping reads a held rail, not a loud low note cresting above it', () {
    // The engine records float after its input gain: a clean low note can
    // crest above 1.0, its peak samples a few 1e-5 apart, and it read as
    // clipped ("lower the mic gain") though nothing clipped.
    Float32List tone(double hz, double amp, {double? rail}) {
      final x = Float32List(_sr * 2);
      for (var i = 0; i < x.length; i++) {
        final v = amp * sin(2 * pi * hz * i / _sr);
        x[i] = rail == null ? v : v.clamp(-rail, rail);
      }
      return x;
    }

    for (final hz in [73.4, 82.4, 110.0]) {
      final m = computeVoiceMetrics(tone(hz, 2.0), _sr);
      expect(m.clippedFraction, 0, reason: '$hz Hz cresting at 2.0');
      expect(m.isClipped, isFalse);
    }
    // A file clipped where it was made holds the rail exactly.
    final clipped = computeVoiceMetrics(tone(220, 2.0, rail: 1.0), _sr);
    expect(clipped.isClipped, isTrue);
    expect(clipped.clippedFraction, greaterThan(0.5));
  });
}
