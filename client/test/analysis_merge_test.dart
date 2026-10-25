import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

// Test framing: hop=100 @ 1 kHz → one frame per 100 ms, so an offset of
// N·100 ms lands exactly at frame N and the placement math stays readable.
const int _hop = 100;
const int _sr = 1000;

VoiceMetrics _metrics({
  required List<double> ring,
  required List<int> active,
  int hop = _hop,
  int fftSize = 2048,
  int sampleRate = _sr,
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
    fftSize: fftSize,
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
  int sampleRate = _sr,
  int fftSize = 2048,
  int hop = _hop,
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

/// A clip's metrics positioned at [offsetMs] after the lane's span start.
({int offsetMs, VoiceMetrics metrics}) _mAt(int offsetMs, VoiceMetrics m) =>
    (offsetMs: offsetMs, metrics: m);

/// A clip's spectrogram positioned at [offsetMs] after the lane's span start.
({int offsetMs, Spectrogram spectrogram}) _sAt(int offsetMs, Spectrogram s) =>
    (offsetMs: offsetMs, spectrogram: s);

void main() {
  group('mergeVoiceMetrics', () {
    test('returns null for empty and a lone origin part unchanged', () {
      expect(mergeVoiceMetrics(const []), isNull);
      final one = _metrics(ring: [1, 2], active: [1, 1]);
      // Single-clip lane: byte-for-byte the same object as the input.
      expect(identical(mergeVoiceMetrics([_mAt(0, one)]), one), isTrue);
    });

    test('a lone part off the lane origin is padded with a leading gap', () {
      // The earliest clip's analysis failed, so the only surviving part starts
      // 300 ms into the lane — it must NOT slide back to the origin.
      final one = _metrics(ring: [7, 8], active: [1, 1]);
      final m = mergeVoiceMetrics([_mAt(300, one)])!;
      expect(m.frames, 5);
      expect(m.ring.sublist(0, 3).every((v) => v.isNaN), isTrue);
      expect(m.f0.sublist(0, 3).every((v) => v.isNaN), isTrue);
      expect(m.active.sublist(0, 3), everyElement(0));
      expect(m.ring.sublist(3), orderedEquals(<double>[7, 8]));
    });

    test('back-to-back clips concatenate and recompute the averages', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [3, 4], active: [1, 0]);
      // b starts exactly where a ends (2 frames = 200 ms).
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      expect(m.frames, 4);
      expect(m.ring, orderedEquals(<double>[1, 2, 3, 4]));
      expect(m.active, orderedEquals(<int>[1, 1, 1, 0]));
      // ringAvg over active voiced frames (0,1,2) = (1+2+3)/3.
      expect(m.ringAvg, closeTo(2.0, 1e-9));
    });

    test('a gap between clips becomes NaN, inactive frames', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [5, 6], active: [1, 1]);
      // a covers frames 0–1; b starts at 500 ms → frames 5–6; 2–4 are the gap.
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(500, b)])!;
      expect(m.frames, 7);
      expect(m.ring.sublist(0, 2), orderedEquals(<double>[1, 2]));
      for (var fr = 2; fr < 5; fr++) {
        expect(m.ring[fr].isNaN, isTrue);
        expect(m.f0[fr].isNaN, isTrue);
        expect(m.loudness[fr].isNaN, isTrue);
        expect(m.active[fr], 0);
      }
      expect(m.ring.sublist(5), orderedEquals(<double>[5, 6]));
      // The gap frames are unvoiced, so they don't dilute the average.
      expect(m.ringAvg, closeTo((1 + 2 + 5 + 6) / 4, 1e-9));
    });

    test('overlapping clips: the later-starting clip wins, tails survive', () {
      final a = _metrics(ring: [1, 2, 3, 4, 5], active: [1, 1, 1, 1, 1]);
      final b = _metrics(ring: [9, 9], active: [0, 0]);
      // b overlaps a's frames 1–2; a's tail (frames 3–4) stays visible.
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(100, b)])!;
      expect(m.frames, 5);
      expect(m.ring, orderedEquals(<double>[1, 9, 9, 4, 5]));
      expect(m.active, orderedEquals(<int>[1, 0, 0, 1, 1]));
      // Overwritten frames don't count toward the average.
      expect(m.ringAvg, closeTo((1 + 4 + 5) / 3, 1e-9));
    });

    test('placement follows offsets, not input order', () {
      final a = _metrics(ring: [1, 2, 3, 4, 5], active: [1, 1, 1, 1, 1]);
      final b = _metrics(ring: [9, 9], active: [0, 0]);
      final inOrder = mergeVoiceMetrics([_mAt(0, a), _mAt(100, b)])!;
      final reversed = mergeVoiceMetrics([_mAt(100, b), _mAt(0, a)])!;
      expect(reversed.ring, orderedEquals(inOrder.ring));
      expect(reversed.active, orderedEquals(inOrder.active));
    });

    test('timbre averages gate on VOICED frames (unvoiced/breath excluded)', () {
      // Frame 1 is audible but unvoiced (NaN F0) — it must NOT pull ringAvg.
      final a = _metrics(ring: [2, 99], active: [1, 1], f0: [200, double.nan]);
      final b = _metrics(ring: [4, 99], active: [1, 1], f0: [200, double.nan]);
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      // Only the two voiced frames (ring 2 and 4) count -> avg 3, not (2+99+4+99)/4.
      expect(m.ringAvg, closeTo(3.0, 1e-9));
    });

    test('a lane with no voiced frame has no timbre averages (NaN, not 0)', () {
      // Audible but unvoiced throughout (breath between phrases on both
      // clips): nothing to average, which the readouts show as "—".
      final a = _metrics(
        ring: [2, 3],
        active: [1, 1],
        f0: [double.nan, double.nan],
      );
      final b = _metrics(
        ring: [4, 5],
        active: [1, 1],
        f0: [double.nan, double.nan],
      );
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      expect(m.ringAvg, isNaN);
      expect(m.tiltAvg, isNaN);
      expect(m.cppAvg, isNaN);
    });

    test('aggregates the quality gate so a clipped/noisy clip still warns', () {
      final clean = _metrics(
        ring: [1, 1],
        active: [1, 1],
        clippedFraction: 0,
        snrDb: 30,
      );
      final bad = _metrics(
        ring: [1, 1],
        active: [1, 1],
        clippedFraction: 0.05,
        snrDb: 5,
      );
      final m = mergeVoiceMetrics([_mAt(0, clean), _mAt(200, bad)])!;
      expect(m.isClipped, isTrue); // worst clip drives it
      expect(m.isLowSnr, isTrue); // worst (lowest) snr drives it
      expect(m.clippedFraction, 0.05);
      expect(m.snrDb, 5);
    });

    test('clean clips merge with no false quality warning', () {
      final a = _metrics(
        ring: [1, 1],
        active: [1, 1],
        clippedFraction: 0,
        snrDb: 30,
      );
      final b = _metrics(
        ring: [1, 1],
        active: [1, 1],
        clippedFraction: 0,
        snrDb: 28,
      );
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      expect(m.isClipped, isFalse);
      expect(m.isLowSnr, isFalse);
    });

    test('skips parts whose framing differs from the first', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [9], active: [1], sampleRate: 48000);
      // b is dropped (different sampleRate), so only a survives — unchanged.
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      expect(identical(m, a), isTrue);
    });

    test('a part with a different WINDOW is a different framing too', () {
      // Same hop and rate, but frames are placed at their window centre, so a
      // 1024-sample window puts every frame of b at another time than the
      // merged result's frame would claim: b is dropped, not misplaced.
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final b = _metrics(ring: [9], active: [1], fftSize: 1024);
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(200, b)])!;
      expect(identical(m, a), isTrue);
      expect(m.fftSize, 2048);
    });

    test('the merged result keeps the parts\' window length', () {
      final a = _metrics(ring: [1, 2], active: [1, 1], fftSize: 400);
      final b = _metrics(ring: [3], active: [1], fftSize: 400);
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(300, b)])!;
      expect(m.fftSize, 400);
      // hop 100 @ 1 kHz with a 400-sample window: frame f is centred at
      // f·100 + 200 ms, so b's lone frame (placed at frame 3) reads 500 ms.
      expect(m.timeOfFrame(3), closeTo(0.5, 1e-9));
      expect(m.timeOfFrame(3), closeTo(0.3 + b.timeOfFrame(0), 1e-9));
    });

    test('a dropped part leaves its gap instead of shifting the survivor', () {
      // The clip at the lane origin has a mismatched framing and is dropped;
      // the surviving later clip must keep its offset, not slide back to 0.
      final mismatched = _metrics(ring: [9], active: [1], sampleRate: 48000);
      final b = _metrics(ring: [7, 8], active: [1, 1]);
      final m = mergeVoiceMetrics([_mAt(300, b), _mAt(0, mismatched)])!;
      expect(m.frames, 5);
      expect(m.ring.sublist(0, 3).every((v) => v.isNaN), isTrue);
      expect(m.ring.sublist(3), orderedEquals(<double>[7, 8]));
    });

    test('frameless parts are filtered out (no span stretch)', () {
      final a = _metrics(ring: [1, 2], active: [1, 1]);
      final empty = _metrics(ring: [], active: []);
      // The empty clip (e.g. a degenerate recording) contributes nothing and
      // must not stretch the merged span to its offset.
      final m = mergeVoiceMetrics([_mAt(0, a), _mAt(500, empty)])!;
      expect(identical(m, a), isTrue);
      expect(mergeVoiceMetrics([_mAt(0, empty)]), isNull);
    });
  });

  group('mergeSpectrograms', () {
    test('returns null for empty and a lone origin part unchanged', () {
      expect(mergeSpectrograms(const []), isNull);
      final one = _spec(db: [1, 2], frames: 1, minDb: 0, maxDb: 2);
      expect(identical(mergeSpectrograms([_sAt(0, one)]), one), isTrue);
    });

    test('back-to-back clips concatenate rows and span min/max', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [5, 6], frames: 1, minDb: -1, maxDb: 7);
      final s = mergeSpectrograms([_sAt(0, a), _sAt(200, b)])!;
      expect(s.frames, 3);
      expect(s.db, orderedEquals(<double>[1, 2, 3, 4, 5, 6]));
      expect(s.minDb, -1);
      expect(s.maxDb, 7);
    });

    test('a gap between clips becomes floor (merged minDb) columns', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [5, 6], frames: 1, minDb: -1, maxDb: 7);
      // a covers frames 0–1; b starts at 400 ms → frame 4; 2–3 are the gap.
      final s = mergeSpectrograms([_sAt(0, a), _sAt(400, b)])!;
      expect(s.frames, 5);
      expect(s.db, orderedEquals(<double>[1, 2, 3, 4, -1, -1, -1, -1, 5, 6]));
      expect(s.minDb, -1);
      expect(s.maxDb, 7);
    });

    test('overlapping clips: the later-starting clip wins', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [9, 9], frames: 1, minDb: 0, maxDb: 9);
      // b lands on a's frame 1.
      final s = mergeSpectrograms([_sAt(0, a), _sAt(100, b)])!;
      expect(s.frames, 2);
      expect(s.db, orderedEquals(<double>[1, 2, 9, 9]));
      // And placement follows offsets, not input order.
      final r = mergeSpectrograms([_sAt(100, b), _sAt(0, a)])!;
      expect(r.db, orderedEquals(<double>[1, 2, 9, 9]));
    });

    test('a lone part off the lane origin is padded with a leading gap', () {
      final one = _spec(db: [5, 6], frames: 1, minDb: -2, maxDb: 6);
      final s = mergeSpectrograms([_sAt(200, one)])!;
      expect(s.frames, 3);
      expect(s.db, orderedEquals(<double>[-2, -2, -2, -2, 5, 6]));
    });

    test('skips parts whose bin/framing differs from the first', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final b = _spec(db: [9, 9, 9], frames: 1, minDb: 0, maxDb: 9, bins: 3);
      // b has different bins → dropped; only a survives — unchanged.
      final s = mergeSpectrograms([_sAt(0, a), _sAt(200, b)])!;
      expect(identical(s, a), isTrue);
    });

    test('frameless parts are filtered out (no span stretch)', () {
      final a = _spec(db: [1, 2, 3, 4], frames: 2, minDb: 0, maxDb: 5);
      final empty = _spec(db: [], frames: 0, minDb: 0, maxDb: 0);
      final s = mergeSpectrograms([_sAt(0, a), _sAt(500, empty)])!;
      expect(identical(s, a), isTrue);
      expect(mergeSpectrograms([_sAt(0, empty)]), isNull);
    });

    test('an absurd lane span fails soft instead of allocating by the gap', () {
      // The merged array is sized by offset + frames: two TINY clips with a
      // huge gap between them used to allocate gigabytes of silence columns.
      // Beyond the span cap the merge yields no view rather than an OOM.
      final a = _spec(db: [1, 2], frames: 1, minDb: 0, maxDb: 5);
      final b = _spec(db: [3, 4], frames: 1, minDb: 0, maxDb: 5);
      // ~10 years of lane offset at this framing.
      final s = mergeSpectrograms([_sAt(0, a), _sAt(320000000000, b)]);
      expect(s, isNull);
      // A sane gap still merges.
      final ok = mergeSpectrograms([_sAt(0, a), _sAt(500, b)]);
      expect(ok, isNotNull);
    });
  });

  group('shared lane-span budget', () {
    test('a lane span past maxMergedLaneFrames is refused by BOTH merges, '
        'not just the spectrogram one', () async {
      // Both merges are sized by the lane SPAN, not by the audio in it. The
      // metrics merge allocates nine span-sized Float32 curves and runs
      // FIRST, so a corrupt or hostile offset used to size that allocation
      // behind a release-stripped assert while the sibling merge's cap never
      // got a say.
      final offsetMs =
          (maxMergedLaneFrames + 10) * 1000 * _hop ~/ _sr; // past the budget

      expect(
        mergeVoiceMetrics([
          (offsetMs: 0, metrics: _metrics(ring: [1, 2], active: [1, 1])),
          (offsetMs: offsetMs, metrics: _metrics(ring: [1, 2], active: [1, 1])),
        ]),
        isNull,
      );
      expect(
        mergeSpectrograms([
          (
            offsetMs: 0,
            spectrogram: _spec(
              db: [0, 0, 0, 0],
              frames: 2,
              minDb: -60,
              maxDb: 0,
            ),
          ),
          (
            offsetMs: offsetMs,
            spectrogram: _spec(
              db: [0, 0, 0, 0],
              frames: 2,
              minDb: -60,
              maxDb: 0,
            ),
          ),
        ]),
        isNull,
      );
    });

    test('a span exactly at the budget still merges', () {
      // The bound is inclusive, so the admissible lane is not one frame
      // shorter than the constant advertises.
      final offsetMs = (maxMergedLaneFrames - 2) * 1000 * _hop ~/ _sr;
      final merged = mergeVoiceMetrics([
        (offsetMs: 0, metrics: _metrics(ring: [1, 2], active: [1, 1])),
        (offsetMs: offsetMs, metrics: _metrics(ring: [1, 2], active: [1, 1])),
      ]);
      expect(merged, isNotNull);
      expect(merged!.frames, maxMergedLaneFrames);
    });

    test('both merges convert an offset to the same frame', () {
      // The two outputs are read against ONE lane axis, so a one-frame
      // disagreement slides the pitch/formant overlays off the columns they
      // annotate.
      for (final offsetMs in [0, 49, 50, 51, 150, 12345]) {
        final m = mergeVoiceMetrics([
          (offsetMs: 0, metrics: _metrics(ring: [1], active: [1])),
          (offsetMs: offsetMs, metrics: _metrics(ring: [1], active: [1])),
        ]);
        final s = mergeSpectrograms([
          (
            offsetMs: 0,
            spectrogram: _spec(db: [0, 0], frames: 1, minDb: -60, maxDb: 0),
          ),
          (
            offsetMs: offsetMs,
            spectrogram: _spec(db: [0, 0], frames: 1, minDb: -60, maxDb: 0),
          ),
        ]);
        expect(m!.frames, s!.frames, reason: 'offset $offsetMs');
      }
    });
  });
}
