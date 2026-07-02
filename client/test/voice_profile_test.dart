import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

/// Builds a VoiceMetrics from synthetic per-frame curves (only the fields the
/// profile reads matter; the rest are zero/NaN). active=1 where loudness is
/// finite.
VoiceMetrics _metrics({
  required List<double> f0,
  required List<double> h1h2,
  required List<double> loud,
}) {
  final n = f0.length;
  final active = Uint8List(n);
  for (var i = 0; i < n; i++) {
    active[i] = loud[i].isFinite ? 1 : 0;
  }
  final nan = Float32List(n)..fillRange(0, n, double.nan);
  return VoiceMetrics(
    frames: n,
    hop: 512,
    sampleRate: 44100,
    ring: Float32List(n),
    tilt: Float32List(n),
    cpp: Float32List(n),
    loudness: Float32List.fromList(loud),
    f0: Float32List.fromList(f0),
    f1: nan,
    f2: nan,
    f3: nan,
    h1h2: Float32List.fromList(h1h2),
    active: active,
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
  );
}

double _midiToHz(double midi) => 440 * pow(2, (midi - 69) / 12).toDouble();

void main() {
  test('range + tessitura + passaggio + dynamics on a C4→C5 glide', () {
    const n = 240;
    final f0 = <double>[], h = <double>[], loud = <double>[];
    for (var i = 0; i < n; i++) {
      final t = i / (n - 1); // 0..1
      final midi = 60 + 12 * t; // C4 → C5
      f0.add(_midiToHz(midi));
      // Chest below E4 (midi 64), head above — a clean +8 dB H1−H2 step. Decide
      // on the *binned* midi so every semitone bin is internally consistent.
      h.add(midi.round() < 64 ? -2.0 : 6.0);
      loud.add(-30 + 20 * t); // −30 → −10 dBFS
    }
    final p = analyzeVoiceProfile(_metrics(f0: f0, h1h2: h, loud: loud));

    expect(p.hasRange, isTrue);
    expect(p.lowMidi, inInclusiveRange(60, 62)); // ~C4
    expect(p.highMidi, inInclusiveRange(70, 72)); // ~C5
    expect(p.tessLoMidi, inInclusiveRange(62, 64));
    expect(p.tessHiMidi, inInclusiveRange(68, 70));
    expect(p.passaggioMidi, 64, reason: 'register step sits at E4 (midi 64)');
    expect(p.passaggioJumpDb, greaterThan(5));
    expect(p.hasDynamics, isTrue);
    expect(p.dynamicRangeDb, greaterThan(14));
  });

  test('no passaggio on a flat pitch (range span too small)', () {
    final p = analyzeVoiceProfile(
      _metrics(
        f0: List.filled(120, _midiToHz(60)),
        h1h2: List.filled(120, 0.0),
        loud: List.filled(120, -20.0),
      ),
    );
    expect(p.hasRange, isTrue);
    expect(p.lowMidi, 60);
    expect(p.highMidi, 60);
    expect(p.hasPassaggio, isFalse); // span < 6 semitones
  });

  test('no passaggio when the timbre is uniform across the range', () {
    const n = 200;
    final f0 = <double>[];
    for (var i = 0; i < n; i++) {
      f0.add(_midiToHz(55 + 12 * (i / (n - 1)))); // G3 → G4, no timbre step
    }
    final p = analyzeVoiceProfile(
      _metrics(
        f0: f0,
        h1h2: List.filled(n, 1.0), // flat H1−H2 everywhere
        loud: List.filled(n, -18.0),
      ),
    );
    expect(p.hasRange, isTrue);
    expect(p.hasPassaggio, isFalse);
  });

  test(
    'passaggio never lands outside the reported range (outlier ignored)',
    () {
      // 200 frames at C4 (chest) + a brief 5+5-frame excursion well above. The
      // outlier bins used to be picked as a passaggio above the whole range.
      final f0 = <double>[], h = <double>[], loud = <double>[];
      for (var i = 0; i < 200; i++) {
        f0.add(_midiToHz(60));
        h.add(-2);
        loud.add(-20);
      }
      for (var i = 0; i < 5; i++) {
        f0.add(_midiToHz(66));
        h.add(-2);
        loud.add(-20);
      }
      for (var i = 0; i < 5; i++) {
        f0.add(_midiToHz(67));
        h.add(6);
        loud.add(-20);
      }
      final p = analyzeVoiceProfile(_metrics(f0: f0, h1h2: h, loud: loud));
      expect(p.lowMidi, 60);
      expect(p.highMidi, 60); // 66/67 are <5% of frames → trimmed off the range
      expect(p.hasPassaggio, isFalse); // no break above the range anymore
    },
  );

  test('a single frame is not a profile (no range / dynamics)', () {
    final p = analyzeVoiceProfile(
      _metrics(f0: [_midiToHz(60)], h1h2: [0.0], loud: [-20.0]),
    );
    expect(p.hasRange, isFalse);
    expect(p.hasDynamics, isFalse);
  });

  test('empty / unvoiced input yields no range, passaggio or dynamics', () {
    final p = analyzeVoiceProfile(
      _metrics(
        f0: List.filled(50, double.nan),
        h1h2: List.filled(50, double.nan),
        loud: List.filled(50, double.nan),
      ),
    );
    expect(p.hasRange, isFalse);
    expect(p.hasPassaggio, isFalse);
    expect(p.hasDynamics, isFalse);
  });
}
