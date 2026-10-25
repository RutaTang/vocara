import 'dart:math' as math;
import 'dart:typed_data';

import 'package:fftea/fftea.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/spectrogram.dart';

/// The spectrogram assembly every builder used to share, kept verbatim as the
/// equality oracle: one Float32 column per STFT frame, then a flat copy with
/// the dB range scanned over the stored values. The builders now write
/// straight into one preallocated buffer instead; their output must not move
/// by a single bit.
({Float32List db, int frames, int bins, double minDb, double maxDb})
_oldAssembly(Float32List samples, int sr, {double maxHz = 5000}) {
  const fftSize = 2048, hop = 512;
  const ln10 = 2.302585092994046;
  final maxBin = math.min(fftSize ~/ 2 + 1, (maxHz * fftSize / sr).ceil() + 1);
  final columns = <Float32List>[];
  if (samples.isNotEmpty) {
    STFT(fftSize, Window.hanning(fftSize)).run(samples, (Float64x2List freq) {
      final mags = freq.discardConjugates().magnitudes();
      final col = Float32List(maxBin);
      for (var b = 0; b < maxBin; b++) {
        col[b] = 20.0 * (math.log(mags[b] + 1e-9) / ln10);
      }
      columns.add(col);
    }, hop);
  }
  final frames = columns.length;
  final db = Float32List(frames * maxBin);
  var minDb = double.infinity, maxDb = -double.infinity;
  for (var f = 0; f < frames; f++) {
    for (var b = 0; b < maxBin; b++) {
      final v = columns[f][b];
      db[f * maxBin + b] = v;
      if (v < minDb) minDb = v;
      if (v > maxDb) maxDb = v;
    }
  }
  if (frames == 0) {
    minDb = -90;
    maxDb = 0;
  }
  return (db: db, frames: frames, bins: maxBin, minDb: minDb, maxDb: maxDb);
}

Float32List _signal(int n, int sr) {
  final rnd = math.Random(3);
  return Float32List.fromList([
    for (var i = 0; i < n; i++)
      0.4 * math.sin(2 * math.pi * 220 * i / sr) +
          0.2 * math.sin(2 * math.pi * 3100 * i / sr) +
          (rnd.nextDouble() - 0.5) * 0.05,
  ]);
}

void _expectSame(
  Spectrogram s,
  ({Float32List db, int frames, int bins, double minDb, double maxDb}) want,
  String what,
) {
  expect(s.frames, want.frames, reason: '$what frames');
  expect(s.bins, want.bins, reason: '$what bins');
  expect(s.db.length, want.frames * want.bins, reason: '$what db length');
  expect(
    Uint8List.sublistView(s.db),
    Uint8List.sublistView(want.db),
    reason: '$what db must be bit-identical',
  );
  expect(s.minDb, want.minDb, reason: '$what minDb');
  expect(s.maxDb, want.maxDb, reason: '$what maxDb');
}

void main() {
  test('every spectrogram builder still assembles exactly the old columns', () {
    for (final (sr, n, maxHz) in [
      (48000, 0, 5000.0),
      (48000, 1000, 5000.0), // shorter than one window
      (48000, 2048, 5000.0),
      (48000, 30011, 5000.0), // not a multiple of the hop
      (44100, 22050, 5000.0),
      (48000, 30011, 18000.0), // the EQ display's wide range
      (8000, 9001, 5000.0), // bins stop at Nyquist first
    ]) {
      final samples = _signal(n, sr);
      final want = _oldAssembly(samples, sr, maxHz: maxHz);
      final what = 'sr=$sr n=$n maxHz=$maxHz';

      _expectSame(
        computeSpectrogram(samples, sr, maxHz: maxHz),
        want,
        '$what batch',
      );

      // Sized from the frame count (what computeFromFile passes), and also
      // told too few frames (it regrows) or too many (a file cut short on
      // disk): the image is the same either way, and exactly sized.
      final exact = stftFrameCount(n);
      for (final hint in {0, exact, exact ~/ 2, exact + 7}) {
        final b = StreamingSpectrogramBuilder(
          sr,
          maxHz: maxHz,
          expectedFrames: hint,
        );
        for (var at = 0; at < n; at += 4097) {
          b.add(Float32List.sublistView(samples, at, math.min(n, at + 4097)));
        }
        final s = b.finish();
        _expectSame(s, want, '$what streamed (hint $hint)');
        expect(s.db.buffer.lengthInBytes, s.db.lengthInBytes, reason: what);
      }

      final streamed = StreamingSpectrogramBuilder(sr, maxHz: maxHz);
      for (var at = 0; at < n; at += 4097) {
        streamed.add(
          Float32List.sublistView(samples, at, math.min(n, at + 4097)),
        );
      }
      _expectSame(streamed.finish(), want, '$what streamed');

      _expectSame(
        computeVoiceMetricsAndSpectrogram(
          samples,
          sr,
          maxHz: maxHz,
        ).spectrogram,
        want,
        '$what metrics pass',
      );

      final expected = stftFrameCount(n);
      expect(expected, want.frames, reason: '$what frame count');
      final take = StreamingTakeAnalyzer(
        sr,
        expectedFrames: expected,
        maxHz: maxHz,
      );
      for (var at = 0; at < n; at += 3001) {
        take.add(Float32List.sublistView(samples, at, math.min(n, at + 3001)));
      }
      _expectSame(take.finish().spectrogram!, want, '$what streamed take');
    }
  });
}
