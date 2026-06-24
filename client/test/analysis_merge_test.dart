import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

VoiceMetrics _metrics({
  required List<double> ring,
  required List<int> active,
  int hop = 512,
  int sampleRate = 44100,
  double peakDbfs = double.nan,
  double clippedFraction = 0,
  double snrDb = double.nan,
  List<double>? f0,
}) {
  final n = ring.length;
  Float32List flat(double v) => Float32List(n)..fillRange(0, n, v);
  return VoiceMetrics(
    frames: n,
    hop: hop,
    sampleRate: sampleRate,
    ring: Float32List.fromList(ring),
    tilt: flat(0),
    cpp: flat(0),
    loudness: flat(0),
    // Voiced by default (finite F0) so the timbre averages count these frames;
    // pass an explicit f0 with NaNs to model unvoiced (breath/consonant) frames.
    f0: f0 == null ? flat(200) : Float32List.fromList(f0),
    f1: flat(double.nan),
    f2: flat(double.nan),
    f3: flat(double.nan),
    h1h2: flat(double.nan),
    active: Uint8List.fromList(active),
    ringAvg: 0,
    tiltAvg: 0,
    cppAvg: 0,
    peakDbfs: peakDbfs,
    clippedFraction: clippedFraction,
    snrDb: snrDb,
  );
}

Spectrogram _spec({
  required List<double> db,
  required int frames,
  required double minDb,
  required double maxDb,
  int bins = 2,
  int sampleRate = 44100,
  int fftSize = 2048,
  int hop = 512,
}) {
  return Spectrogram(
    frames: frames,
    bins: bins,
    maxHz: 5000,
    sampleRate: sampleRate,
    fftSize: fftSize,
    hop: hop,
    db: Float32List.fromList(db),
    minDb: minDb,
    maxDb: maxDb,
  );
}

void main() {
  group('mergeVoiceMetrics', () {
    test('returns null for empty and the single part unchanged', () {
      expect(mergeVoiceMetrics(const []), isNull);
      final one = _metrics(ring: [1, 2], active: [1, 1]);
      expect(identical(mergeVoiceMetrics([one]), one), isTrue);
    });

    test('concatenates frames and recomputes active-frame averages', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [3, 4], active: [1, 0]);
      final m = mergeVoiceMetrics([a, b])!;
      expect(m.frames, 4);
      expect(m.ring, orderedEquals(<double>[1, 2, 3, 4]));
      expect(m.active, orderedEquals(<int>[1, 1, 1, 0]));
      // ringAvg over active frames (0,1,2) = (1+2+3)/3.
      expect(m.ringAvg, closeTo(2.0, 1e-9));
    });

    test('timbre averages gate on VOICED frames (unvoiced/breath excluded)', () {
      // Frame 1 is audible but unvoiced (NaN F0) — it must NOT pull ringAvg.
      final a = _metrics(ring: [2, 99], active: [1, 1], f0: [200, double.nan]);
      final b = _metrics(ring: [4, 99], active: [1, 1], f0: [200, double.nan]);
      final m = mergeVoiceMetrics([a, b])!;
      // Only the two voiced frames (ring 2 and 4) count -> avg 3, not (2+99+4+99)/4.
      expect(m.ringAvg, closeTo(3.0, 1e-9));
    });

    test('aggregates the quality gate so a clipped/noisy clip still warns', () {
      final clean = _metrics(
          ring: [1, 1], active: [1, 1], clippedFraction: 0, snrDb: 30);
      final bad = _metrics(
          ring: [1, 1], active: [1, 1], clippedFraction: 0.05, snrDb: 5);
      final m = mergeVoiceMetrics([clean, bad])!;
      expect(m.isClipped, isTrue); // worst clip drives it
      expect(m.isLowSnr, isTrue); // worst (lowest) snr drives it
      expect(m.clippedFraction, 0.05);
      expect(m.snrDb, 5);
    });

    test('clean clips merge with no false quality warning', () {
      final a =
          _metrics(ring: [1, 1], active: [1, 1], clippedFraction: 0, snrDb: 30);
      final b =
          _metrics(ring: [1, 1], active: [1, 1], clippedFraction: 0, snrDb: 28);
      final m = mergeVoiceMetrics([a, b])!;
      expect(m.isClipped, isFalse);
      expect(m.isLowSnr, isFalse);
    });

    test('skips parts whose framing differs from the first', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [9], active: [1], sampleRate: 48000);
      final m = mergeVoiceMetrics([a, b])!;
      // b is dropped (different sampleRate), so only a survives.
      expect(m.frames, 2);
      expect(m.ring, orderedEquals(<double>[1, 2]));
    });
  });

  group('mergeSpectrograms', () {
    test('returns null for empty and the single part unchanged', () {
      expect(mergeSpectrograms(const []), isNull);
      final one = _spec(db: [1, 2], frames: 1, minDb: 0, maxDb: 2);
      expect(identical(mergeSpectrograms([one]), one), isTrue);
    });

    test('concatenates db rows and spans min/max', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [5, 6], frames: 1, minDb: -1, maxDb: 7);
      final s = mergeSpectrograms([a, b])!;
      expect(s.frames, 3);
      expect(s.db, orderedEquals(<double>[1, 2, 3, 4, 5, 6]));
      expect(s.minDb, -1);
      expect(s.maxDb, 7);
    });

    test('skips parts whose bin/framing differs from the first', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [9, 9, 9], frames: 1, minDb: 0, maxDb: 9, bins: 3);
      final s = mergeSpectrograms([a, b])!;
      // b has different bins → dropped; only a survives.
      expect(s.frames, 2);
      expect(s.db, orderedEquals(<double>[1, 2, 3, 4]));
    });
  });
}
