// Writes voice_metrics.txt: the Flutter app's own analysis summaries
// (voice_metrics.dart, spectrogram.dart) on deterministic synthetic inputs,
// for the Rust port to reproduce bit for bit.
//
// Each line is one JSON object: {"case": name, ...inputs..., "out": {...}}.
// Every double is written as its Dart `toString()` in a JSON string (so NaN,
// Infinity and -0.0 survive); Float32 curves are written as the doubles they
// widen to. Run like models_json.gen.dart:
//   dart run --packages=<config> voice_metrics.gen.dart > voice_metrics.txt
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

var _seed = 12345;

/// A deterministic uniform double in [0, 1) (an LCG, so both sides agree).
double rnd() {
  _seed = (_seed * 1103515245 + 12345) & 0x7fffffff;
  return _seed / 0x80000000;
}

List<String> d(Iterable<double> xs) => [for (final x in xs) x.toString()];

Float32List f32(List<double> xs) => Float32List.fromList(xs);

VoiceMetrics metrics({
  required Float32List f0,
  Float32List? h1h2,
  Float32List? loud,
  Uint8List? active,
  Float32List? ring,
  int hop = 512,
  int sampleRate = 44100,
  double peakDbfs = double.nan,
  double clippedFraction = 0,
  double snrDb = double.nan,
}) {
  final n = f0.length;
  final nan = Float32List(n)..fillRange(0, n, double.nan);
  return VoiceMetrics(
    frames: n,
    hop: hop,
    fftSize: 2048,
    sampleRate: sampleRate,
    ring: ring ?? Float32List(n),
    tilt: Float32List.fromList([for (var i = 0; i < n; i++) i * 0.25 - 3]),
    cpp: Float32List.fromList([for (var i = 0; i < n; i++) (i % 7) * 1.5]),
    loudness: loud ?? Float32List(n),
    f0: f0,
    f1: nan,
    f2: nan,
    f3: nan,
    h1h2: h1h2 ?? nan,
    active: active ?? (Uint8List(n)..fillRange(0, n, 1)),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
    peakDbfs: peakDbfs,
    clippedFraction: clippedFraction,
    snrDb: snrDb,
  );
}

Map<String, Object?> metricsJson(VoiceMetrics m) => {
  'frames': m.frames,
  'hop': m.hop,
  'sampleRate': m.sampleRate,
  'f0': d(m.f0),
  'h1h2': d(m.h1h2),
  'loudness': d(m.loudness),
  'ring': d(m.ring),
  'tilt': d(m.tilt),
  'cpp': d(m.cpp),
  'active': m.active.toList(),
  'peakDbfs': m.peakDbfs.toString(),
  'clippedFraction': m.clippedFraction.toString(),
  'snrDb': m.snrDb.toString(),
};

Map<String, Object?> vibratoJson(Vibrato v) => {
  'present': v.present,
  'rateHz': v.rateHz.toString(),
  'extentCents': v.extentCents.toString(),
  'regularity': v.regularity.toString(),
  'jitter': v.jitter.toString(),
  'onsetMs': v.onsetMs.toString(),
  'deviationCents': d(v.deviationCents),
};

void emit(Map<String, Object?> line) => print(jsonEncode(line));

void main() {
  const frameRate = 44100 / 512;

  // Vibrato: sung notes with rate/extent/drift/jitter/gaps.
  for (final (rate, extent, drift, secs, gap) in [
    (6.0, 50.0, 0.0, 1.5, false),
    (5.2, 35.0, 120.0, 2.0, true),
    (7.5, 80.0, -60.0, 1.2, false),
    (3.0, 20.0, 0.0, 1.0, true),
    (0.0, 0.0, 150.0, 1.5, false),
  ]) {
    final n = (frameRate * secs).round();
    final f0 = Float32List(n);
    for (var i = 0; i < n; i++) {
      final t = i / frameRate;
      final cents =
          extent * math.sin(2 * math.pi * rate * t + rnd() * 0.3) +
          drift * (i / (n - 1)) +
          (rnd() - 0.5) * 4;
      f0[i] = gap && i % 11 == 5 ? double.nan : 220 * math.pow(2, cents / 1200).toDouble();
    }
    final m = metrics(f0: f0);
    emit({
      'case': 'vibrato',
      'metrics': metricsJson(m),
      'lo': -3,
      'hi': n + 5,
      'out': vibratoJson(analyzeVibrato(m, -3, n + 5)),
    });
    final hist = [for (var i = 0; i < 40; i++) 220.0, ...f0];
    emit({
      'case': 'vibratoLive',
      'history': d(hist),
      'frameRate': frameRate.toString(),
      'out': vibratoJson(analyzeVibratoLive(hist, frameRate)),
    });
  }

  // Voice profile + register: glides with timbre steps, noise, silences.
  for (final (lo, hi, step, n) in [
    (60.0, 72.0, 64, 240),
    (55.0, 67.0, 99, 200),
    (48.0, 76.0, 62, 400),
    (60.0, 60.4, 99, 120),
  ]) {
    final f0 = Float32List(n);
    final h = Float32List(n);
    final loud = Float32List(n);
    final active = Uint8List(n);
    for (var i = 0; i < n; i++) {
      final t = i / (n - 1);
      final midi = lo + (hi - lo) * t + (rnd() - 0.5) * 0.6;
      final silent = i % 37 == 3;
      f0[i] = silent ? double.nan : 440 * math.pow(2, (midi - 69) / 12).toDouble();
      h[i] = (midi.round() < step ? -2.0 : 6.0) + (rnd() - 0.5) * 3;
      loud[i] = silent ? double.negativeInfinity : -30 + 20 * t + rnd();
      active[i] = silent ? 0 : 1;
    }
    final m = metrics(f0: f0, h1h2: h, loud: loud, active: active);
    final p = analyzeVoiceProfile(m);
    final r = analyzeRegister(m);
    emit({
      'case': 'profile',
      'metrics': metricsJson(m),
      'out': {
        'lowMidi': p.lowMidi,
        'highMidi': p.highMidi,
        'tessLoMidi': p.tessLoMidi,
        'tessHiMidi': p.tessHiMidi,
        'passaggioMidi': p.passaggioMidi,
        'passaggioJumpDb': p.passaggioJumpDb.toString(),
        'softDb': p.softDb.toString(),
        'loudDb': p.loudDb.toString(),
        'medianDb': p.medianDb.toString(),
        'score': d(r.score),
        'chestPct': r.chestPct.toString(),
        'mixPct': r.mixPct.toString(),
        'headPct': r.headPct.toString(),
        'voicedFrames': r.voicedFrames,
      },
    });
  }

  // LTAS: spectra with a DC offset, a formant cluster, gated and ungated.
  for (final (frames, bins, maxHz, voiced) in [
    (16, 233, 5000.0, 12),
    (12, 120, 2500.0, 5),
    (9, 64, 1376.0, 9),
  ]) {
    final db = Float32List(frames * bins);
    for (var f = 0; f < frames; f++) {
      for (var b = 0; b < bins; b++) {
        final hz = b * 44100 / 2048;
        db[f * bins + b] =
            -70 +
            (b == 0 ? 30 : 0) +
            25 * math.exp(-math.pow((hz - 3000) / 400, 2)) +
            20 * math.exp(-math.pow((hz - 600) / 200, 2)) +
            rnd() * 6;
      }
    }
    final s = Spectrogram(
      frames: frames,
      bins: bins,
      maxHz: maxHz,
      sampleRate: 44100,
      fftSize: 2048,
      hop: 512,
      db: db,
      minDb: -90,
      maxDb: 0,
    );
    final active = Uint8List(frames);
    for (var i = 0; i < voiced; i++) {
      active[i] = 1;
    }
    final m = metrics(f0: Float32List(frames), active: active);
    Map<String, Object?> ltasJson(Ltas l) => {
      'db': d(l.db),
      'binHz': l.binHz.toString(),
      'maxDb': l.maxDb.toString(),
      'alphaRatio': l.alphaRatio.toString(),
      'singersFormantDb': l.singersFormantDb.toString(),
      'frameCount': l.frameCount,
    };
    emit({
      'case': 'ltas',
      'frames': frames,
      'bins': bins,
      'maxHz': maxHz.toString(),
      'db': d(db),
      'active': active.toList(),
      'out': ltasJson(computeLtas(s, m)),
      'outUngated': ltasJson(computeLtas(s, null)),
    });
  }

  // Merges: overlapping, gapped and unaligned offsets.
  for (final offsets in [
    [0, 230, 1999],
    [40, 12, 700],
    [0, 0, 5],
  ]) {
    final parts = <({int offsetMs, VoiceMetrics metrics})>[];
    for (final o in offsets) {
      final n = 5 + (rnd() * 20).floor();
      final f0 = Float32List.fromList([
        for (var i = 0; i < n; i++) rnd() < 0.2 ? double.nan : 100 + rnd() * 300,
      ]);
      final ring = Float32List.fromList([for (var i = 0; i < n; i++) rnd() * -20]);
      final active = Uint8List.fromList([
        for (var i = 0; i < n; i++) rnd() < 0.8 ? 1 : 0,
      ]);
      parts.add((
        offsetMs: o,
        metrics: metrics(
          f0: f0,
          ring: ring,
          active: active,
          hop: 100,
          sampleRate: 1000,
          peakDbfs: rnd() < 0.5 ? double.nan : -rnd() * 10,
          clippedFraction: rnd() * 0.01,
          snrDb: rnd() < 0.3 ? double.nan : rnd() * 40,
        ),
      ));
    }
    final merged = mergeVoiceMetrics(parts)!;
    emit({
      'case': 'mergeMetrics',
      'parts': [
        for (final p in parts) {'offsetMs': p.offsetMs, 'metrics': metricsJson(p.metrics)},
      ],
      'out': {
        ...metricsJson(merged),
        'ringAvg': merged.ringAvg.toString(),
        'tiltAvg': merged.tiltAvg.toString(),
        'cppAvg': merged.cppAvg.toString(),
      },
    });
  }

  // Note names across the range.
  final notes = <String>[];
  for (final hz in [
    8.0, 16.35, 27.5, 55.0, 61.7, 110.0, 196.0, 220.0, 223.0, 261.63, 277.0,
    311.1, 439.0, 440.0, 445.0, 466.16, 880.0, 1046.5, 2093.0, 4186.0,
    12543.85, 0.0, -1.0, double.nan,
  ]) {
    notes.add('$hz=${noteLabel(hz)}');
  }
  for (final midi in [-13, -12, -1, 0, 11, 12, 57, 60, 127, 128]) {
    notes.add('midi$midi=${noteNameForMidi(midi)}');
  }
  emit({'case': 'notes', 'out': notes});
}
