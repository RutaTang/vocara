import 'dart:math';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/services/audio_dsp.dart';
import 'package:vocara/domain/models/voice_metrics.dart';
import 'package:vocara/ui/song_practice_detail/widgets/pitch_lane.dart';

const _sr = 44100;

/// Synthesizes a vowel: an impulse train at [f0] driven through three two-pole
/// resonators at the given formant frequencies — a textbook source-filter vowel
/// whose spectral envelope peaks at F1/F2/F3.
Float64List _vowel(
  double f1,
  double f2,
  double f3, {
  double f0 = 130,
  double sec = 1.0,
}) {
  final n = (_sr * sec).round();
  final src = Float64List(n);
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
  Float64List reson(double fc, double bw, double gain) {
    final r = exp(-pi * bw / _sr);
    final theta = 2 * pi * fc / _sr;
    final a1 = -2 * r * cos(theta);
    final a2 = r * r;
    final out = Float64List(n);
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
  final out = Float64List(n);
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
Float64List _tones(List<double> freqs, {double sec = 1.0}) {
  final n = (_sr * sec).round();
  final out = Float64List(n);
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
Float64List _sine(double hz, double sr, int n) {
  final out = Float64List(n);
  for (var i = 0; i < n; i++) {
    out[i] = sin(2 * pi * hz * i / sr);
  }
  return out;
}

Float64List _noise({double sec = 1.0, int seed = 7}) {
  final n = (_sr * sec).round();
  final rnd = Random(seed);
  final out = Float64List(n);
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

void main() {
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
    final y = yinPitch(Float64List.sublistView(sine, 0, 2048), _sr.toDouble());
    expect(y.f0, closeTo(220, 3));
    expect(y.periodicity, greaterThan(0.9));
  });

  test('yinPitch is octave-robust (strong 2nd harmonic stays at the root)', () {
    // 150 Hz with a 2nd harmonic LOUDER than the fundamental — the classic case
    // where autocorrelation/cepstrum report the octave (300 Hz). YIN should not.
    const f0 = 150.0;
    final n = 2048;
    final frame = Float64List(n);
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
      final y = yinPitch(Float64List(len), 11025);
      expect(y.f0.isNaN, isTrue, reason: 'len=$len should be NaN, not throw');
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

  test('yinPitch periodicity is far lower for noise than for a tone', () {
    final tone = yinPitch(
      Float64List.sublistView(_tones([220]), 0, 2048),
      _sr.toDouble(),
    );
    final noise = yinPitch(
      Float64List.sublistView(_noise(), 0, 2048),
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
      Float64List.fromList([for (final s in base) s * 0.1]),
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

  test('CPP is higher for a periodic voice than for noise', () {
    final voice = computeVoiceMetrics(_vowel(700, 1200, 2600), _sr);
    final noise = computeVoiceMetrics(_noise(), _sr);
    expect(voice.cppAvg, greaterThan(noise.cppAvg));
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
      analyzer.addSamples(Float64List.sublistView(samples, i, end), frames.add);
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
        analyzer.addSamples(Float64List.sublistView(noise, i, end), frames.add);
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
          Float64List.sublistView(samples, i, end),
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

  test('LiveVoiceAnalyzer gates out quiet (room-noise-level) frames', () {
    final analyzer = LiveVoiceAnalyzer();
    // A vowel attenuated to ~-66 dBFS — below the VAD floor.
    final quiet = Float64List.fromList([
      for (final s in _vowel(700, 1200, 2600, f0: 200)) s * 0.002,
    ]);
    final frames = <LiveFrame>[];
    for (var i = 0; i < quiet.length; i += 1000) {
      final end = i + 1000 < quiet.length ? i + 1000 : quiet.length;
      analyzer.addSamples(Float64List.sublistView(quiet, i, end), frames.add);
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
    final m = computeVoiceMetrics(Float64List(0), _sr);
    expect(m.frames, 0);
    expect(m.ringAvg, 0);
  });

  test(
    'a sub-fftSize take still yields a (zero-padded) frame, not empty curves',
    () {
      final m = computeVoiceMetrics(Float64List(1000), _sr); // < 2048 samples
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
    final samples = Float64List(n)
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
    final samples = Float64List(n);
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
      final samples = Float64List(len);
      final m = computeVoiceMetrics(samples, _sr);
      final s = computeSpectrogram(samples, _sr);
      expect(
        m.frames,
        s.frames,
        reason: 'len=$len: metrics=${m.frames} spectrogram=${s.frames}',
      );
    }
  });
}
