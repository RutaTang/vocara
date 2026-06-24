import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:media_kit/media_kit.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/data/services/media_analysis_service.dart';
import 'package:vocara/data/services/spectrogram_service.dart';
import 'package:vocara/domain/models/spectrogram.dart';
import 'package:vocara/domain/models/technique_analysis.dart';
import 'package:vocara/domain/models/voice_metrics.dart';

const _sr = 44100;

/// Writes mono 16-bit PCM WAV from samples in [-1, 1] and returns its path.
Future<String> _writeWav(String path, Float64List samples) async {
  final bytes = writeWavMono(samples, _sr);
  final file = File(path);
  await file.parent.create(recursive: true);
  await file.writeAsBytes(bytes);
  return path;
}

/// A steady sine at [hz] with a few harmonics so the framing/voicing gate has a
/// real periodic source to lock onto (a pure single sine can read low-confidence
/// in some frames). Amplitude is kept well below clipping.
Float64List _tone(double hz, {double sec = 1.5}) {
  final n = (_sr * sec).round();
  final out = Float64List(n);
  for (var i = 0; i < n; i++) {
    final t = i / _sr;
    out[i] = 0.6 * sin(2 * pi * hz * t) +
        0.3 * sin(2 * pi * 2 * hz * t) +
        0.15 * sin(2 * pi * 3 * hz * t);
  }
  return out;
}

/// A pitch glide from [loHz] to [hiHz] over [sec] seconds, harmonically rich.
Float64List _glide(double loHz, double hiHz, {double sec = 2.0}) {
  final n = (_sr * sec).round();
  final out = Float64List(n);
  var phase = 0.0;
  for (var i = 0; i < n; i++) {
    final f = loHz + (hiHz - loHz) * (i / (n - 1));
    phase += 2 * pi * f / _sr;
    out[i] = 0.6 * sin(phase) + 0.3 * sin(2 * phase) + 0.15 * sin(3 * phase);
  }
  return out;
}

/// A sustained tone whose pitch oscillates sinusoidally — a synthetic vibrato at
/// [rateHz] / ±[extentCents] around [f0].
Float64List _vibratoTone(
  double f0,
  double rateHz,
  double extentCents, {
  double sec = 1.6,
}) {
  final n = (_sr * sec).round();
  final out = Float64List(n);
  var phase = 0.0;
  for (var i = 0; i < n; i++) {
    final t = i / _sr;
    final ratio = pow(2, (extentCents / 1200) * sin(2 * pi * rateHz * t));
    phase += 2 * pi * f0 * ratio / _sr;
    out[i] = 0.6 * sin(phase) + 0.3 * sin(2 * phase) + 0.15 * sin(3 * phase);
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

List<double> _voiced(Float32List xs) => [
      for (final x in xs)
        if (!x.isNaN && x > 0) x,
    ];

bool _allFinite(Float32List xs) {
  for (final x in xs) {
    if (!x.isFinite) return false;
  }
  return true;
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();

  late Directory tmp;
  setUpAll(() async {
    tmp = await getTemporaryDirectory();
  });

  String wavPath(String name) => p.join(
        tmp.path,
        'analysis_${name}_${DateTime.now().microsecondsSinceEpoch}.wav',
      );

  group('direct DSP pipeline (computeVoiceMetrics + computeSpectrogram)', () {
    test('a steady 440 Hz tone yields a voiced f0 within a few Hz of 440', () {
      final m = computeVoiceMetrics(_tone(440), _sr);
      expect(m.frames, greaterThan(10));

      final voiced = _voiced(m.f0);
      expect(voiced, isNotEmpty, reason: 'a steady tone must voice frames');
      // YIN runs on a ~22 kHz decimation — assert a few-Hz band, not sub-Hz.
      final med = _median(m.f0);
      expect(med, isNotNaN);
      expect(med, closeTo(440, 6), reason: 'median f0=$med Hz (A4=440)');

      // Timbre means are over VOICED frames only, by design — they must be real.
      expect(m.ringAvg.isFinite, isTrue, reason: 'ringAvg=${m.ringAvg}');
      expect(m.tiltAvg.isFinite, isTrue, reason: 'tiltAvg=${m.tiltAvg}');
      expect(m.cppAvg.isFinite, isTrue, reason: 'cppAvg=${m.cppAvg}');

      // Per-frame curves are finite (NaN only allowed on the F0/formant/H1−H2
      // unvoiced fields — ring/tilt/cpp/loudness are dense).
      expect(_allFinite(m.ring), isTrue);
      expect(_allFinite(m.tilt), isTrue);
      expect(_allFinite(m.cpp), isTrue);
      expect(_allFinite(m.loudness), isTrue);

      // H1−H2 is defined on voiced frames; values are either NaN (unvoiced) or
      // finite — never Inf — and at least some voiced frames carry a value.
      var anyFiniteH = false;
      for (final x in m.h1h2) {
        if (x.isFinite) anyFiniteH = true;
        expect(x.isNaN || x.isFinite, isTrue);
      }
      expect(anyFiniteH, isTrue, reason: 'voiced frames should carry H1−H2');
    });

    test('the glide 220→440 Hz: f0 increases over time', () {
      final m = computeVoiceMetrics(_glide(220, 440), _sr);
      expect(m.frames, greaterThan(20));

      // Compare the median f0 of the first third vs the last third of voiced
      // frames; the back should sit clearly higher than the front.
      final n = m.frames;
      final firstThird = <double>[];
      final lastThird = <double>[];
      for (var i = 0; i < n; i++) {
        final v = m.f0[i];
        if (v.isNaN || v <= 0) continue;
        if (i < n ~/ 3) firstThird.add(v);
        if (i >= 2 * n ~/ 3) lastThird.add(v);
      }
      expect(firstThird, isNotEmpty);
      expect(lastThird, isNotEmpty);
      firstThird.sort();
      lastThird.sort();
      final loMed = firstThird[firstThird.length ~/ 2];
      final hiMed = lastThird[lastThird.length ~/ 2];
      expect(hiMed, greaterThan(loMed + 50),
          reason: 'glide front=$loMed Hz back=$hiMed Hz should rise');
      // Sanity: the endpoints should bracket the 220→440 sweep loosely.
      expect(loMed, closeTo(220, 40));
      expect(hiMed, closeTo(440, 40));
    });

    test('a vibrato-modulated tone is detected with a healthy rate', () {
      final m = computeVoiceMetrics(_vibratoTone(440, 5.5, 60), _sr);
      final v = analyzeVibrato(m, 0, m.frames - 1);
      expect(v.present, isTrue, reason: 'should detect the ~5.5 Hz vibrato');
      expect(v.rateHz, inInclusiveRange(4.5, 7.0));
      expect(v.extentCents.isFinite, isTrue);
      expect(v.regularity, greaterThan(0.5));
      // jitter is finite (not NaN) when vibrato is present.
      expect(v.jitter.isNaN, isFalse);
    });

    test('the spectrogram has the expected frame/bin shape and aligns', () {
      final samples = _tone(440);
      final spec = computeSpectrogram(samples, _sr, fftSize: 2048, hop: 512,
          maxHz: 5000);
      final m = computeVoiceMetrics(samples, _sr, fftSize: 2048, hop: 512);

      // bins == min(fftSize/2+1, ceil(maxHz*fftSize/sr)+1) == ceil(5000*2048/44100)+1.
      final expectedBins = (5000 * 2048 / _sr).ceil() + 1;
      expect(spec.bins, expectedBins, reason: 'maxHz=5000 bin count');
      expect(spec.frames, greaterThan(10));
      expect(spec.db.length, spec.frames * spec.bins);
      // Framing must match the metrics framing (overlay/playhead alignment).
      expect(spec.frames, m.frames);
      expect(spec.binHz, closeTo(_sr / 2048, 1e-6));
      // The dominant bin should sit at ~440 Hz on average.
      final avg = Float64List(spec.bins);
      for (var f = 0; f < spec.frames; f++) {
        final base = f * spec.bins;
        for (var b = 0; b < spec.bins; b++) {
          avg[b] += spec.db[base + b];
        }
      }
      var argmax = 0;
      for (var b = 1; b < spec.bins; b++) {
        if (avg[b] > avg[argmax]) argmax = b;
      }
      final peakHz = argmax * spec.binHz;
      // One STFT bin is ~21.5 Hz wide — accept the fundamental's bin.
      expect(peakHz, closeTo(440, spec.binHz * 1.5),
          reason: 'dominant bin at $peakHz Hz');
    });

    test('voice profile + register are finite/sane on the glide', () {
      final m = computeVoiceMetrics(_glide(220, 440), _sr);
      final profile = analyzeVoiceProfile(m);
      // A 220→440 sweep is a full octave: a real range should be reported.
      expect(profile.hasRange, isTrue);
      expect(profile.lowMidi, isNotNull);
      expect(profile.highMidi, isNotNull);
      expect(profile.highMidi! - profile.lowMidi!, greaterThanOrEqualTo(6),
          reason: 'an octave glide spans well over 6 semitones');
      // Dynamics aggregates are finite or cleanly absent.
      expect(profile.softDb.isNaN || profile.softDb.isFinite, isTrue);
      expect(profile.loudDb.isNaN || profile.loudDb.isFinite, isTrue);

      final reg = analyzeRegister(m);
      expect(reg.chestPct + reg.mixPct + reg.headPct,
          anyOf(closeTo(0, 1e-6), closeTo(100, 1e-3)),
          reason: 'register fractions sum to 0 (no data) or 100');
    });
  });

  group('LTAS bundle (computeLtas over a real spectrogram)', () {
    test('LTAS aggregates are finite for a steady tone', () {
      final samples = _tone(440);
      final spec = computeSpectrogram(samples, _sr);
      final m = computeVoiceMetrics(samples, _sr);
      final ltas = computeLtas(spec, m);
      expect(ltas.hasData, isTrue);
      expect(ltas.db.length, spec.bins);
      expect(_allFinite(ltas.db), isTrue);
      expect(ltas.alphaRatio.isFinite, isTrue);
      expect(ltas.singersFormantDb.isFinite, isTrue);
      expect(ltas.frameCount, greaterThan(0));
    });
  });

  group('edge cases must not crash', () {
    test('pure silence yields no voiced f0 and finite/empty aggregates', () {
      final silence = Float64List((_sr * 1.0).round()); // all zeros
      final m = computeVoiceMetrics(silence, _sr);
      // Framing still produces frames, but nothing should voice.
      expect(_voiced(m.f0), isEmpty, reason: 'silence has no pitch');
      // Voiced-only averages collapse to 0 (no voiced frames) — finite, no NaN.
      expect(m.ringAvg.isFinite, isTrue);
      expect(m.tiltAvg.isFinite, isTrue);
      expect(m.cppAvg.isFinite, isTrue);

      final spec = computeSpectrogram(silence, _sr);
      expect(spec.frames, m.frames, reason: 'silence framing still aligns');

      final profile = analyzeVoiceProfile(m);
      expect(profile.hasRange, isFalse, reason: 'no pitch → no range');
      expect(profile.hasPassaggio, isFalse);

      final ltas = computeLtas(spec, m);
      expect(_allFinite(ltas.db), isTrue);

      final reg = analyzeRegister(m);
      expect(reg.hasData, isFalse, reason: 'no voiced H1−H2 → no register data');

      final vib = analyzeVibrato(m, 0, m.frames - 1);
      expect(vib.present, isFalse, reason: 'silence has no vibrato');
    });

    test('a buffer shorter than the FFT window returns one padded frame, no crash',
        () {
      final shortBuf = Float64List(1000); // < fftSize (2048)
      // Put a faint 440 Hz tone in it so it is not pure zeros.
      for (var i = 0; i < shortBuf.length; i++) {
        shortBuf[i] = 0.4 * sin(2 * pi * 440 * i / _sr);
      }
      final m = computeVoiceMetrics(shortBuf, _sr);
      expect(m.frames, greaterThanOrEqualTo(1),
          reason: 'sub-window input still yields a zero-padded frame');
      // Every curve length matches the frame count (alignment invariant).
      expect(m.f0.length, m.frames);
      expect(m.f1.length, m.frames);
      expect(m.ring.length, m.frames);

      final spec = computeSpectrogram(shortBuf, _sr);
      expect(spec.frames, m.frames,
          reason: 'short-buffer metrics/spectrogram framing must match');

      // Aggregates are finite even when too short to be meaningful.
      expect(m.ringAvg.isFinite, isTrue);
      expect(m.cppAvg.isFinite, isTrue);

      // Profile/register/vibrato on a 1-frame-ish take: sane empties, no throw.
      final profile = analyzeVoiceProfile(m);
      expect(profile.hasRange, isFalse,
          reason: 'too few frames to be a range');
      expect(analyzeRegister(m).hasData, anyOf(isTrue, isFalse));
      expect(analyzeVibrato(m, 0, m.frames - 1).present, isFalse);
    });

    test('truly empty input yields zero frames', () {
      final m = computeVoiceMetrics(Float64List(0), _sr);
      expect(m.frames, 0);
      expect(m.ringAvg, 0);
      final spec = computeSpectrogram(Float64List(0), _sr);
      expect(spec.frames, 0);
      expect(analyzeVoiceProfile(m).hasRange, isFalse);
    });
  });

  group('file-path services (SpectrogramService reads WAV PCM16)', () {
    testWidgets('computeMetricsFromFile + computeFromFile on a 440 Hz WAV',
        (tester) async {
      final wav = await _writeWav(wavPath('tone440'), _tone(440));

      final svc = const SpectrogramService();
      VoiceMetrics? metrics;
      Spectrogram? spec;
      // The services run the STFT on a background isolate; await directly.
      metrics = await svc.computeMetricsFromFile(wav);
      spec = await svc.computeFromFile(wav);

      expect(metrics, isNotNull, reason: 'WAV PCM16 must decode via readWavMono');
      expect(spec, isNotNull);
      final med = _median(metrics!.f0);
      expect(med, closeTo(440, 6), reason: 'file-path median f0=$med Hz');
      expect(_voiced(metrics.f0), isNotEmpty);
      expect(spec!.frames, metrics.frames,
          reason: 'service framing aligns metrics ↔ spectrogram');
      expect(_allFinite(spec.db), isTrue);
    });

    testWidgets('a missing file returns null (no throw)', (tester) async {
      final svc = const SpectrogramService();
      final missing = p.join(tmp.path, 'does_not_exist_${DateTime.now()
          .microsecondsSinceEpoch}.wav');
      expect(await svc.computeFromFile(missing), isNull);
      expect(await svc.computeMetricsFromFile(missing), isNull);
    });
  });

  group('MediaAnalysisService end-to-end (native engine decode)', () {
    testWidgets('analyze a 440 Hz WAV → full bundle with sane f0/spectrogram',
        (tester) async {
      final wav = await _writeWav(wavPath('media440'), _tone(440));
      final TechniqueAnalysis? a = await const MediaAnalysisService()
          .analyze(wav);

      expect(a, isNotNull, reason: 'the native engine should decode WAV');
      expect(a!.durationMs, greaterThan(1000),
          reason: 'a ~1.5 s tone reports a real duration');
      expect(a.spectrogram.frames, greaterThan(10));
      expect(a.spectrogram.frames, a.metrics.frames,
          reason: 'bundle framing aligns');

      final med = _median(a.metrics.f0);
      expect(med, isNotNaN);
      // Decode resamples to 44.1 kHz; YIN still on the ~22 kHz path → few-Hz band.
      expect(med, closeTo(440, 8), reason: 'decoded median f0=$med Hz');
      expect(_voiced(a.metrics.f0), isNotEmpty);

      // Every bundle aggregate is finite.
      expect(a.metrics.ringAvg.isFinite, isTrue);
      expect(a.metrics.tiltAvg.isFinite, isTrue);
      expect(a.metrics.cppAvg.isFinite, isTrue);
      expect(a.ltas.alphaRatio.isFinite, isTrue);
      expect(a.ltas.singersFormantDb.isFinite, isTrue);
      expect(_allFinite(a.ltas.db), isTrue);
      expect(a.register.chestPct + a.register.mixPct + a.register.headPct,
          anyOf(closeTo(0, 1e-6), closeTo(100, 1e-3)));
    });

    testWidgets('analyze a glide → f0 rises front-to-back', (tester) async {
      final wav = await _writeWav(wavPath('mediaGlide'), _glide(220, 440));
      final a = await const MediaAnalysisService().analyze(wav);
      expect(a, isNotNull);

      final f0 = a!.metrics.f0;
      final n = a.metrics.frames;
      final front = <double>[];
      final back = <double>[];
      for (var i = 0; i < n; i++) {
        final v = f0[i];
        if (v.isNaN || v <= 0) continue;
        if (i < n ~/ 3) front.add(v);
        if (i >= 2 * n ~/ 3) back.add(v);
      }
      expect(front, isNotEmpty);
      expect(back, isNotEmpty);
      front.sort();
      back.sort();
      expect(back[back.length ~/ 2],
          greaterThan(front[front.length ~/ 2] + 50),
          reason: 'decoded glide f0 must rise');
    });
  });
}
