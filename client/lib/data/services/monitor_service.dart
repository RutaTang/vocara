import 'dart:math' as math;

import 'package:flutter/foundation.dart';

import '../../domain/models/voice_metrics.dart';
import 'audio_dsp.dart';

/// Real-time microphone monitor for the live Voiceprint: it is FED mono PCM (pulled from the
/// engine's input capture by the ViewModel), runs an incremental STFT, and keeps a rolling
/// window of dB columns plus the latest input level. A [ChangeNotifier] so the spectrogram
/// widget can listen directly without rebuilding the whole detail tree.
class MonitorService extends ChangeNotifier {
  MonitorService({this.maxColumns = 720});

  final int maxColumns;

  // The engine captures at its fixed internal rate (48 kHz).
  static const int _sampleRate = 48000;
  final LiveVoiceAnalyzer _analyzer = LiveVoiceAnalyzer(
    sampleRate: _sampleRate,
  );

  final List<Float32List> _columns = [];
  // Rolling per-column voice metrics (parallel to _columns) for the overlay.
  final List<double> _f0 = [], _f1 = [], _f2 = [], _f3 = [];
  // Rolling per-column loudness (dBFS), parallel to _columns, for live dynamics.
  final List<double> _loud = [];
  // Per-column voicing flag (1 = voiced), parallel to _columns. The live
  // spectrogram dims unvoiced columns so silence/noise reads dark.
  final List<int> _act = [];
  // Per-column: a pitched frame, parallel to _columns. [_act] holds a phrase
  // a few frames past its last pitched one (the overlay's release), which
  // the averages and the dynamics must not count: each phrase's tail of
  // fading breath read as voice, a 50 dB "range" over notes sung level.
  final List<bool> _voiced = [];
  bool _active = false;
  double _level = 0;
  // Latest smoothed metrics (for the readout bar / vowel dot).
  double _curF0 = double.nan,
      _curF1 = double.nan,
      _curF2 = double.nan,
      _curF3 = double.nan,
      _ring = double.nan,
      _tilt = double.nan,
      _cpp = double.nan,
      _curH1H2 = double.nan;
  // Running session averages over voiced frames.
  double _ringSum = 0, _tiltSum = 0, _cppSum = 0;
  int _activeN = 0;
  // Running LTAS accumulator: per-bin power sum (10^(dB/10)) over voiced
  // columns, plus the voiced-column count, for the live average spectrum.
  Float64List? _ltasPow;
  int _ltasN = 0;
  // Running register tally over voiced frames with a valid H1−H2, using the
  // same chest/mix/head score thresholds as the take's analyzeRegister.
  int _regChest = 0, _regMix = 0, _regHead = 0;
  // A stable, heavily-smoothed pitch lock just for the tuner readout (needs a
  // few consecutive clearly-voiced frames before it shows, then EMA-glides).
  final TunerTracker _tuner = TunerTracker();
  double _tunerF0 = double.nan;

  bool get active => _active;
  List<Float32List> get columns => _columns;
  // Per-column voicing flags (1 = voiced), aligned with [columns].
  List<int> get activeColumns => _act;
  Float32List? get latest => _columns.isEmpty ? null : _columns.last;
  int get bins => _analyzer.bins;
  double get binHz => _analyzer.binHz;
  double get maxHz => _analyzer.maxHz;
  double get frameRate => _analyzer.frameRate;

  /// Latest H1−H2 (dB) for the live register indicator; NaN when not voiced.
  double get currentH1H2 => _curH1H2;

  /// The running long-term average spectrum over voiced frames this session, or
  /// null until enough frames have accumulated for a meaningful average. Like
  /// every live analysis this is gated on sustained-voiced frames (the same gate
  /// as the live F0/formant overlays), so it is a real-time vocal fingerprint —
  /// close to, but not bit-identical to, the take's energy-gated computeLtas.
  Ltas? get liveLtas => (_ltasN < 8 || _ltasPow == null)
      ? null
      : finalizeLtas(_ltasPow!, _ltasN, _ltasPow!.length, binHz, maxHz);

  // Running register tallies (voiced frames) for the live chest/mix/head split.
  int get registerChest => _regChest;
  int get registerMix => _regMix;
  int get registerHead => _regHead;
  int get registerVoiced => _regChest + _regMix + _regHead;

  /// Input level in [0, 1] (mapped from RMS dBFS), for the meter.
  double get level => _level;

  // Live voice metrics: rolling histories (aligned with [columns]) for the
  // formant overlay, plus the latest smoothed values and session averages.
  List<double> get f0History => _f0;
  List<double> get f1History => _f1;
  List<double> get f2History => _f2;
  List<double> get f3History => _f3;
  double get currentF0 => _curF0;
  double get currentF1 => _curF1;
  double get currentF2 => _curF2;
  double get currentF3 => _curF3;
  double get ring => _ring;
  double get tilt => _tilt;
  double get cpp => _cpp;
  double get ringAvg => _activeN == 0 ? double.nan : _ringSum / _activeN;
  double get tiltAvg => _activeN == 0 ? double.nan : _tiltSum / _activeN;
  double get cppAvg => _activeN == 0 ? double.nan : _cppSum / _activeN;

  /// Rolling per-column loudness (dBFS), aligned with [columns].
  List<double> get loudnessHistory => _loud;

  /// Live dynamics over the voiced frames in the rolling window: the 5th/95th
  /// loudness percentiles and their span. NaN fields until enough voiced frames.
  ({double softDb, double loudDb, double rangeDb}) get liveDynamics {
    final v = <double>[];
    for (var i = 0; i < _loud.length; i++) {
      if (i < _voiced.length && _voiced[i] && _loud[i].isFinite) {
        v.add(_loud[i]);
      }
    }
    if (v.length < 8) {
      return (softDb: double.nan, loudDb: double.nan, rangeDb: double.nan);
    }
    v.sort();
    final soft = v[(v.length * 0.05).floor().clamp(0, v.length - 1)];
    final loud = v[(v.length * 0.95).floor().clamp(0, v.length - 1)];
    return (softDb: soft, loudDb: loud, rangeDb: loud - soft);
  }

  /// A stable, smoothed pitch (Hz) for the tuner; NaN until a clear sustained
  /// note is locked.
  double get tunerF0 => _tunerF0;

  /// Begin a live-analysis session (the ViewModel opens the engine capture and feeds frames via
  /// [feedSamples]). Resets all rolling state.
  void start() {
    if (_active) return;
    _columns.clear();
    _f0.clear();
    _f1.clear();
    _f2.clear();
    _f3.clear();
    _act.clear();
    _voiced.clear();
    _loud.clear();
    _ringSum = _tiltSum = _cppSum = 0;
    _activeN = 0;
    _ltasPow = null;
    _ltasN = 0;
    _regChest = _regMix = _regHead = 0;
    _curF0 = _curF1 = _curF2 = _curF3 = _ring = _tilt = _cpp = double.nan;
    _curH1H2 = double.nan;
    _tuner.reset();
    _tunerF0 = double.nan;
    _analyzer.reset();
    _active = true;
    notifyListeners();
  }

  /// Feed a chunk of mono samples (range ~[-1, 1], at 48 kHz) captured from the engine input.
  void feedSamples(Float32List samples) {
    if (!_active) return;
    final n = samples.length;
    if (n == 0) return;
    final pcm = _finiteOnly(samples);
    var sumSq = 0.0;
    for (var i = 0; i < n; i++) {
      sumSq += pcm[i] * pcm[i];
    }
    final rms = math.sqrt(sumSq / n);
    final dbfs = 20 * (math.log(rms + 1e-9) / math.ln10);
    _level = ((dbfs + 50) / 50).clamp(0.0, 1.0);

    _analyzer.addSamples(pcm, (f) {
      _columns.add(f.column);
      _f0.add(f.f0);
      _f1.add(f.f1);
      _f2.add(f.f2);
      _f3.add(f.f3);
      _act.add(f.active ? 1 : 0);
      final voiced = f.active && f.f0Confidence > 0;
      _voiced.add(voiced);
      _loud.add(f.loudness);
      if (_columns.length > maxColumns) {
        _columns.removeAt(0);
        _f0.removeAt(0);
        _f1.removeAt(0);
        _f2.removeAt(0);
        _f3.removeAt(0);
        _act.removeAt(0);
        _voiced.removeAt(0);
        _loud.removeAt(0);
      }
      _curF0 = f.f0;
      _curF1 = f.f1;
      _curF2 = f.f2;
      _curF3 = f.f3;
      _ring = f.ring;
      _tilt = f.tilt;
      _cpp = f.cpp;
      _curH1H2 = f.h1h2;
      if (voiced) {
        _ringSum += f.ring;
        _tiltSum += f.tilt;
        _cppSum += f.cpp;
        _activeN++;
        // Accumulate the LTAS power sum over voiced columns.
        final col = f.column;
        final pow = _ltasPow ??= Float64List(col.length);
        if (pow.length == col.length) {
          for (var b = 0; b < col.length; b++) {
            pow[b] += math.pow(10.0, col[b] / 10.0).toDouble();
          }
          _ltasN++;
        }
        // Tally the current register from the (smoothed, sustained-voiced)
        // H1−H2 using the same chest/mix/head thresholds as the take.
        if (f.h1h2.isFinite) {
          final s = registerScore(f.h1h2);
          if (s < 0.4) {
            _regChest++;
          } else if (s < 0.6) {
            _regMix++;
          } else {
            _regHead++;
          }
        }
      }

      _tunerF0 = _tuner.add(f.f0, f.f0Confidence);
    });
    notifyListeners();
  }

  /// [samples] with every non-finite value zeroed before any DSP touches
  /// them, as the take paths do (SpectrogramService's reader,
  /// MediaAnalysisService). One NaN inside a held note lands in a frame the
  /// voiced gate still counts, and from there in the LTAS power sum and the
  /// tilt / CPP sums — running totals only [start] resets, so one glitch from
  /// the input driver would blank those readouts for the rest of the session.
  /// A scrubbed COPY rather than an in-place fix (the buffer is the
  /// caller's), paid for only by the rare chunk that needs one.
  static Float32List _finiteOnly(Float32List samples) {
    for (var i = 0; i < samples.length; i++) {
      if (samples[i].isFinite) continue;
      final out = Float32List.fromList(samples);
      for (var j = i; j < out.length; j++) {
        if (!out[j].isFinite) out[j] = 0.0;
      }
      return out;
    }
    return samples;
  }

  void stop() {
    if (!_active) return;
    _active = false;
    _level = 0;
    notifyListeners();
  }
}
