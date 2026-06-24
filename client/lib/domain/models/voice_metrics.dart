import 'dart:math' as math;
import 'dart:typed_data';

import 'spectrogram.dart';

/// Per-frame voice-timbre metrics derived from a take's STFT, aligned to the
/// same framing as its [Spectrogram] ([hop] / [sampleRate]). Every curve has
/// length [frames]; formant/F0 values are in Hz, or NaN on unvoiced/low-energy
/// frames. See `computeVoiceMetrics` for how each is computed.
class VoiceMetrics {
  VoiceMetrics({
    required this.frames,
    required this.hop,
    required this.sampleRate,
    required this.ring,
    required this.tilt,
    required this.cpp,
    required this.loudness,
    required this.f0,
    required this.f1,
    required this.f2,
    required this.f3,
    required this.h1h2,
    required this.active,
    required this.ringAvg,
    required this.tiltAvg,
    required this.cppAvg,
    this.peakDbfs = double.nan,
    this.clippedFraction = 0,
    this.snrDb = double.nan,
  });

  final int frames;
  final int hop;
  final int sampleRate;

  /// Singing Power Ratio (F18): peak dB in 2–4 kHz minus peak dB in 0–2 kHz.
  /// Higher (closer to 0) = more "ring" / projection.
  final Float32List ring;

  /// Spectral tilt / alpha ratio (F19): 10·log10(energy>1kHz / energy<1kHz), dB.
  /// Higher = brighter / more pressed; lower = darker / breathier.
  final Float32List tilt;

  /// Cepstral Peak Prominence (F20), dB. Higher = cleaner, well-closed tone;
  /// lower = breathier / leakier.
  final Float32List cpp;

  /// Per-frame loudness (F9): RMS level in dBFS (≤ 0). The dynamics/phrasing
  /// envelope — drops at phrase ends, swells on crescendos.
  final Float32List loudness;

  /// Fundamental frequency F0 in Hz per frame (the pitch / lowest harmonic);
  /// NaN on unvoiced frames.
  final Float32List f0;

  /// Formant frequencies in Hz per frame (F17); NaN where not estimated.
  final Float32List f1;
  final Float32List f2;
  final Float32List f3;

  /// H1−H2: level of the 1st harmonic minus the 2nd (dB), per voiced frame; NaN
  /// when unvoiced. A classic register cue — low/negative in chest voice (strong
  /// H2), rising/positive toward head voice & falsetto (H1 dominant), so a step
  /// up as pitch climbs marks the passaggio.
  final Float32List h1h2;

  /// 1 where the frame carries enough energy to be voice (drives the averages).
  final Uint8List active;

  /// Averages over [active] frames only.
  final double ringAvg;
  final double tiltAvg;
  final double cppAvg;

  // --- Input quality (a gate before trusting the analysis) -----------------

  /// Peak sample level of the clip in dBFS (0 = full scale). NaN when unknown.
  final double peakDbfs;

  /// Fraction of samples inside a flat-topped clipped run (0..1). Above ~0.001
  /// the recording is audibly clipped and the spectral metrics are unreliable.
  final double clippedFraction;

  /// Rough signal-to-noise ratio (dB): the median loudness of voiced frames over
  /// the noise floor (10th-percentile frame loudness). NaN when undetermined.
  final double snrDb;

  /// True when the recording is clipped enough to distort the analysis.
  bool get isClipped => clippedFraction > 0.001;

  /// True when the signal-to-noise ratio is too low to trust the timbre metrics.
  bool get isLowSnr => snrDb.isFinite && snrDb < 10;

  double timeOfFrame(int frame) => frame * hop / sampleRate;

  int frameAtTime(double seconds) {
    if (frames == 0) return 0;
    var f = (seconds * sampleRate / hop).round();
    if (f < 0) f = 0;
    if (f >= frames) f = frames - 1;
    return f;
  }

  /// The value of a curve nearest [seconds] (NaN if there are no frames).
  double sampleAt(Float32List curve, double seconds) =>
      frames == 0 ? double.nan : curve[frameAtTime(seconds)];
}

/// Concatenates several takes' [VoiceMetrics] (passed in timeline order) into
/// one, for whole-lane analysis across a multi-clip take. Parts must share
/// [hop] and [sampleRate] — they do when computed by the same SpectrogramService
/// defaults — and any part whose framing differs from the first is skipped.
/// Returns null for an empty list and the single part unchanged for one. The
/// per-frame curves are concatenated and [ringAvg]/[tiltAvg]/[cppAvg] are
/// recomputed over the combined active frames. Pure.
VoiceMetrics? mergeVoiceMetrics(List<VoiceMetrics> parts) {
  if (parts.isEmpty) return null;
  if (parts.length == 1) return parts.first;
  final first = parts.first;
  final kept = parts
      .where((p) => p.hop == first.hop && p.sampleRate == first.sampleRate)
      .toList();
  if (kept.isEmpty) return null;
  if (kept.length == 1) return kept.first;

  final total = kept.fold(0, (n, p) => n + p.frames);
  final ring = Float32List(total);
  final tilt = Float32List(total);
  final cpp = Float32List(total);
  final loudness = Float32List(total);
  final f0 = Float32List(total);
  final f1 = Float32List(total);
  final f2 = Float32List(total);
  final f3 = Float32List(total);
  final h1h2 = Float32List(total);
  final active = Uint8List(total);

  var off = 0;
  for (final p in kept) {
    final end = off + p.frames;
    ring.setRange(off, end, p.ring);
    tilt.setRange(off, end, p.tilt);
    cpp.setRange(off, end, p.cpp);
    loudness.setRange(off, end, p.loudness);
    f0.setRange(off, end, p.f0);
    f1.setRange(off, end, p.f1);
    f2.setRange(off, end, p.f2);
    f3.setRange(off, end, p.f3);
    h1h2.setRange(off, end, p.h1h2);
    active.setRange(off, end, p.active);
    off = end;
  }

  // Recompute the timbre averages over the concatenation, gated on VOICED frames
  // (valid F0), to match computeVoiceMetrics's single-clip averages. `active`
  // alone is the looser AUDIBLE set; averaging ring/tilt/cpp over it would dilute
  // them with breath/consonant/onset noise, so the same audio would read
  // differently as a multi-clip aggregate than as one clip. f0 is NaN on unvoiced
  // frames, so `f0 finite && > 0` closely tracks the voiced gate.
  double avgVoiced(Float32List c) {
    var sum = 0.0;
    var n = 0;
    for (var i = 0; i < total; i++) {
      if (active[i] == 1 && f0[i].isFinite && f0[i] > 0 && c[i].isFinite) {
        sum += c[i];
        n++;
      }
    }
    return n == 0 ? 0.0 : sum / n;
  }

  // Aggregate the quality gate across clips so a multi-clip take still flags a
  // clipped / noisy take (otherwise the merged metrics keep the NaN/0 defaults
  // and the quality banner never fires). Be protective: the loudest peak, the
  // worst clipping, and the worst (lowest) SNR drive the warning.
  var peak = double.negativeInfinity;
  var worstClip = 0.0;
  var worstSnr = double.infinity;
  for (final p in kept) {
    if (p.peakDbfs.isFinite && p.peakDbfs > peak) peak = p.peakDbfs;
    if (p.clippedFraction > worstClip) worstClip = p.clippedFraction;
    if (p.snrDb.isFinite && p.snrDb < worstSnr) worstSnr = p.snrDb;
  }

  return VoiceMetrics(
    frames: total,
    hop: first.hop,
    sampleRate: first.sampleRate,
    ring: ring,
    tilt: tilt,
    cpp: cpp,
    loudness: loudness,
    f0: f0,
    f1: f1,
    f2: f2,
    f3: f3,
    h1h2: h1h2,
    active: active,
    ringAvg: avgVoiced(ring),
    tiltAvg: avgVoiced(tilt),
    cppAvg: avgVoiced(cpp),
    peakDbfs: peak.isFinite ? peak : double.nan,
    clippedFraction: worstClip,
    snrDb: worstSnr.isFinite ? worstSnr : double.nan,
  );
}

const _noteNames = [
  'C',
  'C♯',
  'D',
  'D♯',
  'E',
  'F',
  'F♯',
  'G',
  'G♯',
  'A',
  'A♯',
  'B',
];

/// The nearest equal-tempered note (name + octave) and signed cents deviation
/// for [hz] (A4=440), or null when [hz] isn't a valid pitch. e.g. 223 Hz →
/// (name: 'A3', cents: +24.x).
({String name, double cents})? noteOf(double hz) {
  if (hz.isNaN || hz <= 0) return null;
  final midi = 69 + 12 * (math.log(hz / 440) / math.ln2);
  final m = midi.round();
  final name = '${_noteNames[((m % 12) + 12) % 12]}${(m ~/ 12) - 1}';
  return (name: name, cents: (midi - m) * 100);
}

/// Formats a frequency as a note name + cents, e.g. 223 Hz → "A3 +24¢";
/// "—" for NaN/≤0.
String noteLabel(double hz) {
  final n = noteOf(hz);
  if (n == null) return '—';
  final c = n.cents.round();
  return '${n.name} ${c >= 0 ? '+' : '−'}${c.abs()}¢';
}

/// The note name + octave for an integer MIDI number, e.g. 57 → "A3".
String noteNameForMidi(int midi) =>
    '${_noteNames[((midi % 12) + 12) % 12]}${(midi ~/ 12) - 1}';

/// A whole-take voice summary derived from [VoiceMetrics]: the sung range and
/// comfortable tessitura, an estimated register break (passaggio), and the
/// dynamic span. MIDI fields are null when there isn't enough voiced audio.
class VoiceProfile {
  const VoiceProfile({
    required this.lowMidi,
    required this.highMidi,
    required this.tessLoMidi,
    required this.tessHiMidi,
    required this.passaggioMidi,
    required this.passaggioJumpDb,
    required this.softDb,
    required this.loudDb,
    required this.medianDb,
  });

  /// Sung range — 5th/95th percentile of voiced F0 (MIDI); null if no pitch.
  final int? lowMidi;
  final int? highMidi;

  /// Comfortable tessitura — the central 50% (25th/75th percentile) of voiced
  /// F0 where the singer spends most time (MIDI).
  final int? tessLoMidi;
  final int? tessHiMidi;

  /// Estimated register break (MIDI), or null if not confidently detected.
  final int? passaggioMidi;

  /// How abrupt the break is (the H1−H2 jump in dB at the passaggio); 0 if none.
  final double passaggioJumpDb;

  /// Dynamic span — soft (5th pct), loud (95th pct) and median loudness (dBFS)
  /// over audible frames; NaN when there are no audible frames.
  final double softDb;
  final double loudDb;
  final double medianDb;

  bool get hasRange => lowMidi != null && highMidi != null;
  bool get hasPassaggio => passaggioMidi != null;
  bool get hasDynamics => !softDb.isNaN && !loudDb.isNaN;
  double get dynamicRangeDb => hasDynamics ? loudDb - softDb : double.nan;
}

/// Linear-interpolated [p]-quantile (0..1) of an already-sorted list.
double _percentile(List<double> sorted, double p) {
  if (sorted.isEmpty) return double.nan;
  if (sorted.length == 1) return sorted.first;
  final idx = (p * (sorted.length - 1)).clamp(0.0, sorted.length - 1.0);
  final lo = idx.floor();
  final hi = idx.ceil();
  if (lo == hi) return sorted[lo];
  return sorted[lo] * (hi - idx) + sorted[hi] * (idx - lo);
}

double _hzToMidi(double hz) => 69 + 12 * (math.log(hz / 440) / math.ln2);

/// Summarizes a take's [VoiceMetrics] into a [VoiceProfile]: range/tessitura
/// from voiced F0, a passaggio estimate from the largest upward H1−H2 step
/// between adjacent semitone bins, and dynamics from the audible loudness. Pure.
VoiceProfile analyzeVoiceProfile(
  VoiceMetrics m, {
  int passaggioMinFramesPerBin = 8,
  int passaggioMinSpanSemis = 6,
  double passaggioMinJumpDb = 3,
  int minFrames = 8,
}) {
  // Range + tessitura from voiced F0 (in continuous MIDI). Needs enough voiced
  // frames to be meaningful — a 1-frame take is not a "range".
  final pitches = <double>[];
  for (var i = 0; i < m.frames; i++) {
    final hz = m.f0[i];
    if (!hz.isNaN && hz > 0) pitches.add(_hzToMidi(hz));
  }
  int? lowMidi, highMidi, tessLo, tessHi;
  if (pitches.length >= minFrames) {
    pitches.sort();
    lowMidi = _percentile(pitches, 0.05).round();
    highMidi = _percentile(pitches, 0.95).round();
    tessLo = _percentile(pitches, 0.25).round();
    tessHi = _percentile(pitches, 0.75).round();
  }

  // Passaggio: median H1−H2 per semitone bin, then the largest upward jump
  // between adjacent (≤3-semitone-apart) sufficiently-populated bins. Bins are
  // confined to the reported [lowMidi, highMidi] range so an outlier excursion
  // can't put the marker off the range bar.
  final binVals = <int, List<double>>{};
  if (lowMidi != null && highMidi != null) {
    for (var i = 0; i < m.frames; i++) {
      final hz = m.f0[i];
      final h = m.h1h2[i];
      if (hz.isNaN || hz <= 0 || h.isNaN) continue;
      final mr = _hzToMidi(hz).round();
      if (mr < lowMidi || mr > highMidi) continue;
      (binVals[mr] ??= []).add(h);
    }
  }
  final bins = <int, double>{};
  binVals.forEach((midi, vals) {
    if (vals.length >= passaggioMinFramesPerBin) {
      vals.sort();
      bins[midi] = vals[vals.length ~/ 2];
    }
  });
  int? passaggioMidi;
  var passaggioJump = 0.0;
  if (bins.length >= 2) {
    final keys = bins.keys.toList()..sort();
    if (keys.last - keys.first >= passaggioMinSpanSemis) {
      for (var i = 1; i < keys.length; i++) {
        if (keys[i] - keys[i - 1] > 3) continue; // skip across big gaps
        // A "break" touching the very top/bottom of the sung range is usually a
        // sparse-bin / low-F0 artifact — but a genuine edge break (e.g. a high
        // belt flip) is real, so accept an edge boundary only when BOTH its bins
        // are well populated (≥ 2× the per-bin minimum), instead of excluding it
        // outright.
        if (keys[i - 1] == keys.first || keys[i] == keys.last) {
          final loN = binVals[keys[i - 1]]?.length ?? 0;
          final hiN = binVals[keys[i]]?.length ?? 0;
          if (loN < passaggioMinFramesPerBin * 2 ||
              hiN < passaggioMinFramesPerBin * 2) {
            continue;
          }
        }
        final jump = bins[keys[i]]! - bins[keys[i - 1]]!;
        if (jump > passaggioJump) {
          passaggioJump = jump;
          passaggioMidi = keys[i];
        }
      }
      if (passaggioJump < passaggioMinJumpDb) {
        passaggioMidi = null;
        passaggioJump = 0.0;
      }
    }
  }

  // Dynamics from audible loudness.
  final louds = <double>[];
  for (var i = 0; i < m.frames; i++) {
    if (m.active[i] == 1) {
      final v = m.loudness[i];
      if (v.isFinite) louds.add(v);
    }
  }
  var softDb = double.nan, loudDb = double.nan, medDb = double.nan;
  if (louds.length >= minFrames) {
    louds.sort();
    softDb = _percentile(louds, 0.05);
    loudDb = _percentile(louds, 0.95);
    medDb = _percentile(louds, 0.5);
  }

  return VoiceProfile(
    lowMidi: lowMidi,
    highMidi: highMidi,
    tessLoMidi: tessLo,
    tessHiMidi: tessHi,
    passaggioMidi: passaggioMidi,
    passaggioJumpDb: passaggioJump,
    softDb: softDb,
    loudDb: loudDb,
    medianDb: medDb,
  );
}

/// Long-term average spectrum: the per-bin average power (in dB) over a take's
/// voiced frames — the take's overall spectral signature (brightness + the
/// singer's-formant cluster), independent of the moment-to-moment notes/vowels.
class Ltas {
  const Ltas({
    required this.db,
    required this.binHz,
    required this.maxHz,
    required this.maxDb,
    required this.alphaRatio,
    required this.singersFormantDb,
    required this.frameCount,
  });

  /// Average magnitude per bin in dB (same 20·log10 convention as the
  /// spectrogram); bin b ≈ b·[binHz] Hz.
  final Float32List db;
  final double binHz;
  final double maxHz;
  final double maxDb;

  /// Alpha ratio: 10·log10(energy>1 kHz / energy<1 kHz). Higher = brighter.
  final double alphaRatio;

  /// Singer's-formant prominence: peak LTAS level in 2.6–3.8 kHz minus the peak
  /// below 2 kHz (dB). Higher = a stronger carrying "ring" cluster.
  final double singersFormantDb;

  /// Number of frames averaged (voiced where possible).
  final int frameCount;

  bool get hasData => frameCount > 0 && db.isNotEmpty;
}

/// Averages a take's [Spectrogram] over its voiced frames (gated by
/// [m].active, falling back to all frames if too few are voiced) into an [Ltas].
/// Averaging is done in linear power (10^(dB/10)) then converted back to dB.
Ltas computeLtas(Spectrogram s, VoiceMetrics? m) {
  final bins = s.bins;
  final frames = m == null ? s.frames : math.min(s.frames, m.frames);
  final powSum = Float64List(bins);

  int accumulate(bool Function(int fr) include) {
    powSum.fillRange(0, bins, 0);
    var n = 0;
    for (var fr = 0; fr < frames; fr++) {
      if (!include(fr)) continue;
      final base = fr * bins;
      for (var b = 0; b < bins; b++) {
        // db = 20·log10(mag) ⇒ mag² = 10^(db/10) = power.
        powSum[b] += math.pow(10.0, s.db[base + b] / 10.0).toDouble();
      }
      n++;
    }
    return n;
  }

  var n = m == null ? 0 : accumulate((fr) => m.active[fr] == 1);
  if (n < 8) n = accumulate((_) => true);

  return finalizeLtas(powSum, n, bins, s.binHz, s.maxHz);
}

/// Converts an accumulated per-bin power sum ([powSum] over [n] frames) into an
/// [Ltas] (per-bin dB, alpha ratio, singer's-formant prominence). Shared by the
/// take ([computeLtas]) and the live running accumulator.
Ltas finalizeLtas(
  Float64List powSum,
  int n,
  int bins,
  double binHz,
  double maxHz,
) {
  final db = Float32List(bins);
  var maxDb = 0.0;
  if (n > 0) {
    maxDb = -1e9;
    for (var b = 0; b < bins; b++) {
      db[b] = 10.0 * (math.log(powSum[b] / n + 1e-12) / math.ln10);
      if (db[b] > maxDb) maxDb = db[b];
    }
  }

  int binOf(double hz) => (hz / binHz).round().clamp(0, bins - 1);
  double bandPow(int lo, int hi) {
    var e = 0.0;
    for (var b = lo; b < hi && b < bins; b++) {
      e += powSum[b];
    }
    return n > 0 ? e / n : 0.0;
  }

  final eLow = bandPow(binOf(50), binOf(1000));
  final eHigh = bandPow(binOf(1000), binOf(math.min(5000.0, maxHz)));
  final alpha = 10.0 * (math.log((eHigh + 1e-12) / (eLow + 1e-12)) / math.ln10);

  double peakDb(int lo, int hi) {
    var p = -1e9;
    for (var b = lo; b < hi && b < bins; b++) {
      if (db[b] > p) p = db[b];
    }
    return p;
  }

  final sfHi = peakDb(binOf(2600), binOf(3800));
  final sfLo = peakDb(0, binOf(2000));
  final sf = (sfHi <= -1e8 || sfLo <= -1e8) ? 0.0 : sfHi - sfLo;

  return Ltas(
    db: db,
    binHz: binHz,
    maxHz: maxHz,
    maxDb: maxDb,
    alphaRatio: alpha,
    singersFormantDb: sf,
    frameCount: n,
  );
}

/// Vibrato measured on one sustained note: the pitch oscillation's rate, extent
/// and regularity. [present] is false for short or straight notes.
class Vibrato {
  const Vibrato({
    required this.present,
    required this.rateHz,
    required this.extentCents,
    required this.regularity,
    required this.jitter,
    required this.onsetMs,
    required this.deviationCents,
  });

  final bool present;

  /// Oscillation rate (Hz); ~5–7 is healthy.
  final double rateHz;

  /// Half the peak-to-peak pitch swing (± cents).
  final double extentCents;

  /// 0..1 — how regular the oscillation is (normalized autocorrelation peak).
  final double regularity;

  /// Cycle-to-cycle rate jitter: the coefficient of variation of successive
  /// vibrato-cycle periods (0 = perfectly even rate; ~0.05 is a steady trained
  /// vibrato, > ~0.15 reads as uneven/wobbly). NaN when no vibrato is present.
  final double jitter;

  /// Delay from the note start to vibrato onset (ms), or NaN.
  final double onsetMs;

  /// Per-(voiced)-frame pitch deviation from the note median (cents) — the
  /// vibrato waveform, for drawing.
  final Float32List deviationCents;
}

Vibrato _noVibrato() => Vibrato(
  present: false,
  rateHz: double.nan,
  extentCents: 0,
  regularity: 0,
  jitter: double.nan,
  onsetMs: double.nan,
  deviationCents: Float32List(0),
);

/// Cycle-to-cycle rate jitter of the detrended vibrato oscillation [osc]: the
/// coefficient of variation (std/mean) of the periods between successive
/// rising zero-crossings. NaN when fewer than ~3 cycles are present.
double _vibratoJitter(Float64List osc) {
  final crossings = <int>[];
  for (var i = 1; i < osc.length; i++) {
    if (osc[i - 1] < 0 && osc[i] >= 0) crossings.add(i);
  }
  if (crossings.length < 4) return double.nan; // need ≥3 full periods
  var sum = 0.0;
  final periods = <double>[];
  for (var i = 1; i < crossings.length; i++) {
    final p = (crossings[i] - crossings[i - 1]).toDouble();
    periods.add(p);
    sum += p;
  }
  final mean = sum / periods.length;
  if (mean <= 0) return double.nan;
  var v = 0.0;
  for (final p in periods) {
    v += (p - mean) * (p - mean);
  }
  return math.sqrt(v / periods.length) / mean;
}

/// Removes a best-fit linear trend from [y] into [out] — so a scoop/slide into
/// or out of a sustained note (and slow drift) doesn't contaminate the vibrato.
void _linearDetrend(Float64List y, Float64List out) {
  final n = y.length;
  if (n < 2) {
    for (var i = 0; i < n; i++) {
      out[i] = 0;
    }
    return;
  }
  var sx = 0.0, sy = 0.0, sxx = 0.0, sxy = 0.0;
  for (var i = 0; i < n; i++) {
    final x = i.toDouble();
    sx += x;
    sy += y[i];
    sxx += x * x;
    sxy += x * y[i];
  }
  final d = n * sxx - sx * sx;
  final b = d.abs() < 1e-12 ? 0.0 : (n * sxy - sx * sy) / d;
  final a = (sy - b * sx) / n;
  for (var i = 0; i < n; i++) {
    out[i] = y[i] - (a + b * i);
  }
}

/// Vibrato of the note spanning frames [startFrame]..[endFrame] (inclusive).
Vibrato analyzeVibrato(
  VoiceMetrics m,
  int startFrame,
  int endFrame, {
  double minRateHz = 3.5,
  double maxRateHz = 9.0,
  double minExtentCents = 20,
  double minRegularity = 0.55,
}) {
  final lo = startFrame < 0 ? 0 : startFrame;
  final hi = endFrame >= m.frames ? m.frames - 1 : endFrame;
  if (hi < lo) return _noVibrato();
  return _vibratoOf(
    m.f0,
    lo,
    hi,
    m.sampleRate / m.hop,
    minRateHz: minRateHz,
    maxRateHz: maxRateHz,
    minExtentCents: minExtentCents,
    minRegularity: minRegularity,
  );
}

/// Live vibrato over the trailing [windowSec] of a rolling per-frame F0 history.
Vibrato analyzeVibratoLive(
  List<double> f0History,
  double frameRate, {
  double windowSec = 1.6,
  double minRateHz = 3.5,
  double maxRateHz = 9.0,
  double minExtentCents = 20,
  double minRegularity = 0.55,
}) {
  final n = f0History.length;
  if (n == 0) return _noVibrato();
  final w = (frameRate * windowSec).round();
  final lo = n - w < 0 ? 0 : n - w;
  return _vibratoOf(
    f0History,
    lo,
    n - 1,
    frameRate,
    minRateHz: minRateHz,
    maxRateHz: maxRateHz,
    minExtentCents: minExtentCents,
    minRegularity: minRegularity,
  );
}

/// Core: vibrato of the per-frame F0 in [f0] over [lo]..[hi] (NaN = unvoiced;
/// uniform frame spacing). Builds the cents deviation on the full grid
/// (interpolating short unvoiced gaps so the time axis is uniform), removes a
/// linear trend (scoop/drift), then finds the dominant oscillation in the
/// [minRateHz]..[maxRateHz] band by normalized autocorrelation (parabolic-
/// interpolated, with a subharmonic guard). Needs ≥ 1 s, ≥ 70 % voiced. Pure.
Vibrato _vibratoOf(
  List<double> f0,
  int lo,
  int hi,
  double frameRate, {
  required double minRateHz,
  required double maxRateHz,
  required double minExtentCents,
  required double minRegularity,
}) {
  final span = hi - lo + 1;
  if (span < (frameRate * 1.0).round()) return _noVibrato(); // need ≥ ~1 s

  // Cents deviation (NaN on unvoiced) relative to the voiced median.
  final voiced = <double>[];
  final dev = Float64List(span);
  for (var i = 0; i < span; i++) {
    final hz = f0[lo + i];
    if (!hz.isNaN && hz > 0) {
      voiced.add(hz);
      dev[i] = double.nan; // filled with cents below, once we have the median
    } else {
      dev[i] = double.nan;
    }
  }
  if (voiced.length < (span * 0.7).round()) return _noVibrato(); // too gappy
  final sorted = [...voiced]..sort();
  final med = sorted[sorted.length ~/ 2];
  for (var i = 0; i < span; i++) {
    final hz = f0[lo + i];
    if (!hz.isNaN && hz > 0) {
      dev[i] = 1200 * (math.log(hz / med) / math.ln2);
    }
  }

  // Interpolate unvoiced gaps so the series is uniform; edges hold the nearest.
  var firstV = 0;
  while (firstV < span && dev[firstV].isNaN) {
    firstV++;
  }
  var lastV = span - 1;
  while (lastV >= 0 && dev[lastV].isNaN) {
    lastV--;
  }
  if (firstV > lastV) return _noVibrato();
  for (var i = 0; i < firstV; i++) {
    dev[i] = dev[firstV];
  }
  for (var i = lastV + 1; i < span; i++) {
    dev[i] = dev[lastV];
  }
  var g = firstV;
  while (g <= lastV) {
    if (!dev[g].isNaN) {
      g++;
      continue;
    }
    var j = g;
    while (j <= lastV && dev[j].isNaN) {
      j++;
    }
    final a = dev[g - 1], b = dev[j];
    for (var k = g; k < j; k++) {
      dev[k] = a + (b - a) * (k - g + 1) / (j - g + 1);
    }
    g = j;
  }

  // Detrend (remove scoop/slide) → the oscillatory signal.
  final osc = Float64List(span);
  _linearDetrend(dev, osc);

  final lagMin = (frameRate / maxRateHz).floor().clamp(1, span - 1);
  final hiBound = span ~/ 2;
  if (hiBound < lagMin + 1) return _noVibrato();
  final lagMax = (frameRate / minRateHz).ceil().clamp(lagMin + 1, hiBound);

  final norm = Float64List(lagMax + 1);
  var bestLag = lagMin;
  var bestNorm = -1e30;
  for (var lag = lagMin; lag <= lagMax; lag++) {
    var r = 0.0, e1 = 0.0, e2 = 0.0;
    for (var t = 0; t + lag < span; t++) {
      r += osc[t] * osc[t + lag];
      e1 += osc[t] * osc[t];
      e2 += osc[t + lag] * osc[t + lag];
    }
    norm[lag] = (e1 > 0 && e2 > 0) ? r / math.sqrt(e1 * e2) : 0.0;
    if (norm[lag] > bestNorm) {
      bestNorm = norm[lag];
      bestLag = lag;
    }
  }
  if (bestNorm <= 0) return _noVibrato();

  // Subharmonic guard: prefer the shortest lag that is nearly as strong (the
  // true period rather than an integer multiple of it).
  for (var lag = lagMin; lag < bestLag; lag++) {
    if (norm[lag] >= bestNorm * 0.92) {
      bestLag = lag;
      break;
    }
  }
  // Parabolic interpolation for sub-lag rate precision.
  var lagF = bestLag.toDouble();
  if (bestLag > lagMin && bestLag < lagMax) {
    final y0 = norm[bestLag - 1], y1 = norm[bestLag], y2 = norm[bestLag + 1];
    final den = y0 - 2 * y1 + y2;
    if (den.abs() > 1e-12) lagF = bestLag + 0.5 * (y0 - y2) / den;
  }

  // Regularity is the strength of the STRONGEST periodicity peak; the subharmonic
  // guard above only re-points [bestLag] for the rate, so a true vibrato can't be
  // dropped by the (≥0.92) guard lowering the reported self-similarity.
  final regularity = bestNorm.clamp(0.0, 1.0);
  final rawRate = frameRate / lagF;
  var sq = 0.0;
  for (final v in osc) {
    sq += v * v;
  }
  final extent = math.sqrt(sq / span) * math.sqrt2;

  // A small tolerance keeps a true vibrato sitting on a band edge from being
  // dropped by lag quantization; the displayed rate is clamped into the band.
  final inBand = rawRate >= minRateHz - 0.6 && rawRate <= maxRateHz + 0.6;
  final present =
      regularity >= minRegularity && extent >= minExtentCents && inBand;
  final rateHz = rawRate.clamp(minRateHz, maxRateHz);

  var onsetMs = double.nan;
  if (present) {
    for (var t = 0; t + bestLag < span; t++) {
      var a = 0.0;
      for (var k = 0; k < bestLag; k++) {
        final av = osc[t + k].abs();
        if (av > a) a = av;
      }
      if (a >= extent * 0.5) {
        onsetMs = t / frameRate * 1000;
        break;
      }
    }
  }

  return Vibrato(
    present: present,
    rateHz: rateHz,
    extentCents: extent,
    regularity: regularity,
    jitter: present ? _vibratoJitter(osc) : double.nan,
    onsetMs: onsetMs,
    deviationCents: Float32List.fromList(osc),
  );
}

/// A per-frame vocal-register estimate from H1−H2: 0 = chest, 1 = head/falsetto
/// (NaN where unvoiced), plus the chest/mix/head time fractions. An *indicator*,
/// not a verdict — H1−H2 is individual and also moves with vowel/dynamics.
class RegisterMap {
  const RegisterMap({
    required this.score,
    required this.chestPct,
    required this.mixPct,
    required this.headPct,
    required this.voicedFrames,
  });

  /// Per-frame register score in [0,1] (0 = chest, 1 = head); NaN unvoiced.
  final Float32List score;
  final double chestPct;
  final double mixPct;
  final double headPct;
  final int voicedFrames;

  bool get hasData => voicedFrames > 0;
}

/// Maps an H1−H2 value (dB) to a register score in [0,1] (0 = chest, 1 = head)
/// via a soft absolute sigmoid centered near the chest↔head boundary.
double registerScore(
  double h1h2, {
  double centerDb = 3.5,
  double scaleDb = 4.0,
}) => (1.0 / (1.0 + math.exp(-(h1h2 - centerDb) / scaleDb))).clamp(0.0, 1.0);

/// Per-frame register from H1−H2 via [registerScore] (absolute mapping, so a
/// take sung entirely in one register isn't artificially split). Pure.
RegisterMap analyzeRegister(
  VoiceMetrics m, {
  double centerDb = 3.5,
  double scaleDb = 4.0,
}) {
  final n = m.frames;
  final score = Float32List(n)..fillRange(0, n, double.nan);
  var chest = 0, mix = 0, head = 0;
  for (var i = 0; i < n; i++) {
    final h = m.h1h2[i];
    if (!(h.isFinite && m.f0[i] > 0)) continue;
    final s = registerScore(h, centerDb: centerDb, scaleDb: scaleDb);
    score[i] = s;
    if (s < 0.4) {
      chest++;
    } else if (s < 0.6) {
      mix++;
    } else {
      head++;
    }
  }
  final v = chest + mix + head;
  final inv = v == 0 ? 0.0 : 100.0 / v;
  return RegisterMap(
    score: score,
    chestPct: chest * inv,
    mixPct: mix * inv,
    headPct: head * inv,
    voicedFrames: v,
  );
}
