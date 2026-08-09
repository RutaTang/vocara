// The analysis pipeline runs on whatever the user imported or recorded, which
// includes files far outside the "a few seconds of singing" shape the DSP was
// tuned for: a take stopped after one buffer, a muted input, a DC-offset
// interface, a decoder that emitted a non-finite sample. None of those may
// throw — the dock renders "unavailable", it does not take the screen down.
//
// Every window/frame size in the chain (YIN, the spectrogram STFT, the LTAS
// voiced gate) has its own minimum-length branch; the loop below crosses each
// of them from both sides at every rate the decoder can hand us.

import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

Float32List _sine(int n, double f, int sr, {double amp = 0.3}) {
  final s = Float32List(n);
  for (var i = 0; i < n; i++) {
    s[i] = amp * math.sin(2 * math.pi * f * i / sr);
  }
  return s;
}

/// The whole suite as the take dock and the technique reference run it.
void _fullPass(Float32List samples, int sampleRate) {
  final metrics = computeVoiceMetrics(samples, sampleRate);
  final spectrogram = computeSpectrogram(samples, sampleRate);
  computeLtas(spectrogram, metrics);
  analyzeVoiceProfile(metrics);
  analyzeRegister(metrics);
}

void main() {
  group('the analysis suite survives every input length and rate', () {
    for (final sr in const [8000, 22050, 44100, 48000]) {
      for (final n in const [1, 2, 63, 64, 255, 256, 1000]) {
        test('$n samples at $sr Hz', () => _fullPass(_sine(n, 220, sr), sr));
      }
    }
  });

  test('silence and DC analyze without throwing', () {
    const sr = 48000;
    for (final data in [
      Float32List(4800), // a muted or unplugged input
      Float32List.fromList(List.filled(4800, 1.0)), // full-scale DC offset
      Float32List.fromList(List.filled(4800, -1.0)),
    ]) {
      _fullPass(data, sr);
    }
  });

  test('non-finite samples analyze without throwing', () {
    const sr = 48000;
    final data = _sine(4800, 220, sr);
    data[100] = double.nan;
    data[200] = double.infinity;
    data[300] = double.negativeInfinity;
    _fullPass(data, sr);
  });

  group('lane span', () {
    AudioTrack lane(List<AudioClip> clips) => AudioTrack(
      id: 't',
      kind: AudioTrackKind.vocal,
      displayName: 'T',
      createdAt: DateTime(2026),
      clips: clips,
    );

    test('an empty lane spans nothing', () {
      final t = lane(const []);
      expect(t.endMs - t.startMs, 0);
    });

    test('clips held out of order still span first..last', () {
      final t = lane(const [
        AudioClip(id: 'b', audioFilePath: '/b', durationMs: 100, startMs: 500),
        AudioClip(id: 'a', audioFilePath: '/a', durationMs: 100),
      ]);
      expect(t.startMs, 0);
      expect(t.endMs, 600);
    });

    test('a zero-duration clip cannot invert the span', () {
      final t = lane(const [
        AudioClip(id: 'a', audioFilePath: '/a', durationMs: 0, startMs: 900),
      ]);
      expect(t.endMs, greaterThanOrEqualTo(t.startMs));
    });
  });
}
