import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../data/services/monitor_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/spectrogram_painter.dart';
import '../../core/widgets/studio_hint.dart';
import '../../core/widgets/waveform_painter.dart';
import '../view_model/analysis_source.dart';
import 'live_spectrogram.dart';
import 'pitch_lane.dart';

const double _axisWidth = 46;

// Per-metric colors for the Timbre tab's time curves.
const Color _timbreRing = Color(0xFFE0A050);
const Color _timbreTilt = Color(0xFF5B8DEF);
const Color _timbreCpp = Color(0xFF52B788);
const Color _timbreH1H2 = Color(0xFFD98A8A);

/// Hint/loading guard for take-based tabs that need the take's spectrogram.
Widget _withSpectrogram(
  AnalysisSource vm,
  Widget Function(Spectrogram spec, VoiceMetrics? metrics, AudioTrack track)
  build,
) {
  final track = vm.selectedTrack;
  if (track == null || track.isBacking) {
    return const StudioHint('Select a recorded take to analyze.');
  }
  if (vm.spectrogramLoading) {
    return const StudioHint('Analyzing…', spinner: true);
  }
  final spec = vm.selectedSpectrogram;
  if (spec == null) {
    return const StudioHint(
      'Analysis is available for recorded takes (WAV).\n'
      'Switch to Live to watch your voice as you sing.',
    );
  }
  return build(spec, vm.selectedMetrics, track);
}

/// Hint/loading guard for take-based tabs that need voice metrics.
Widget _withMetrics(
  AnalysisSource vm,
  Widget Function(VoiceMetrics m, AudioTrack track) build,
) {
  final track = vm.selectedTrack;
  if (track == null || track.isBacking) {
    return const StudioHint('Select a recorded take to analyze.');
  }
  if (vm.spectrogramLoading) {
    return const StudioHint('Analyzing…', spinner: true);
  }
  final m = vm.selectedMetrics;
  if (m == null || m.frames == 0) {
    return const StudioHint('Analysis is available for recorded takes (WAV).');
  }
  return build(m, track);
}

void _seekTo(
  AnalysisSource vm,
  double dx,
  double width,
  int durationMs,
  int startMs,
) {
  // The panel shows the subject in subject-local time [0, dur]; map the click
  // back to the absolute timeline (startMs + local) so seeking lands on it.
  vm.seek(
    Duration(
      milliseconds: (startMs + (dx / width).clamp(0.0, 1.0) * durationMs)
          .round(),
    ),
  );
}

/// The absolute playhead [pos] as subject-local seconds, clamped to the
/// analysis subject's [durationMs] span (which begins at [startMs] on the
/// shared timeline). Callers pass [AnalysisSource.analysisStartMs] /
/// [AnalysisSource.analysisDurationMs] — the span the shown metrics /
/// spectrogram actually cover: the lane span for a whole-lane subject (a
/// multi-clip aggregate places each clip's frames at its lane-relative
/// offset), or the clip's own span when one clip of a multi-clip lane is
/// selected (its analysis is clip-local).
double _localSeconds(Duration pos, int durationMs, int startMs) =>
    (pos.inMilliseconds - startMs).clamp(0, durationMs) / 1000.0;

/// The take spectrogram stack: image + grid + formant overlay + seek + playhead.
Widget _takeSpectroStack(
  AnalysisSource vm,
  Spectrogram spec,
  VoiceMetrics? metrics,
) {
  final spanMs = vm.analysisDurationMs;
  final startMs = vm.analysisStartMs;
  return LayoutBuilder(
    builder: (context, c) => Stack(
      children: [
        const Positioned.fill(child: ColoredBox(color: Studio.inset)),
        Positioned.fill(child: SpectrogramImage(spectrogram: spec)),
        Positioned.fill(
          child: IgnorePointer(
            child: CustomPaint(
              painter: FrequencyGridPainter(maxHz: spec.maxHz),
            ),
          ),
        ),
        if (metrics != null)
          Positioned.fill(
            child: IgnorePointer(
              child: CustomPaint(
                painter: FormantTrackPainter(
                  metrics: metrics,
                  maxHz: spec.maxHz,
                ),
              ),
            ),
          ),
        Positioned.fill(
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (d) =>
                _seekTo(vm, d.localPosition.dx, c.maxWidth, spanMs, startMs),
            onHorizontalDragUpdate: (d) =>
                _seekTo(vm, d.localPosition.dx, c.maxWidth, spanMs, startMs),
          ),
        ),
        Positioned.fill(
          child: IgnorePointer(
            child: CustomPaint(
              painter: PlayheadPainter(
                position: vm.position,
                timelineDurationMs: spanMs,
                color: Studio.playhead,
                showHead: true,
                startMs: startMs,
              ),
            ),
          ),
        ),
      ],
    ),
  );
}

/// A small uppercase header row for a tab, indented past the frequency axis.
class _TabHeader extends StatelessWidget {
  const _TabHeader(this.text, {this.trailing});

  final String text;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(left: _axisWidth, bottom: 6),
      child: Row(
        children: [
          Flexible(
            child: Text(
              text.toUpperCase(),
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                color: Studio.textFaint,
                fontSize: 10,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.8,
              ),
            ),
          ),
          if (trailing != null) ...[const SizedBox(width: 10), trailing!],
        ],
      ),
    );
  }
}

/// Bottom-dock tab: the 0–5 kHz spectrogram with the F0/formant overlay, from
/// the selected take or the live mic.
class SpectrogramTab extends StatelessWidget {
  const SpectrogramTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) return _LiveSpectrogramPanel(monitor: vm.monitor);
    return _withSpectrogram(vm, (spec, metrics, track) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const _TabHeader('Spectrogram', trailing: _FormantLegend()),
            Expanded(
              child: _AxisRow(
                maxHz: spec.maxHz,
                child: _takeSpectroStack(vm, spec, metrics),
              ),
            ),
          ],
        ),
      );
    });
  }
}

/// The live-mic spectrogram panel (the Spectrogram tab in Live mode).
class _LiveSpectrogramPanel extends StatelessWidget {
  const _LiveSpectrogramPanel({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(left: _axisWidth, bottom: 6),
            child: Row(
              children: [
                Container(
                  width: 8,
                  height: 8,
                  decoration: const BoxDecoration(
                    color: Studio.record,
                    shape: BoxShape.circle,
                  ),
                ),
                const SizedBox(width: 6),
                const Text(
                  'LIVE',
                  style: TextStyle(
                    color: Studio.record,
                    fontSize: 10,
                    fontWeight: FontWeight.w700,
                    letterSpacing: 0.5,
                  ),
                ),
                const SizedBox(width: 12),
                SizedBox(width: 80, child: _MonitorLevel(monitor: monitor)),
                const Spacer(),
                const _FormantLegend(),
              ],
            ),
          ),
          Expanded(
            child: _AxisRow(
              maxHz: monitor.maxHz,
              child: ClipRRect(
                borderRadius: BorderRadius.circular(6),
                child: Stack(
                  children: [
                    Positioned.fill(child: LiveSpectrogram(monitor: monitor)),
                    Positioned.fill(
                      child: IgnorePointer(
                        child: ListenableBuilder(
                          listenable: monitor,
                          builder: (context, _) => CustomPaint(
                            painter: LiveFormantPainter(
                              f0: monitor.f0History,
                              f1: monitor.f1History,
                              f2: monitor.f2History,
                              f3: monitor.f3History,
                              maxHz: monitor.maxHz,
                            ),
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Bottom-dock tab: the instantaneous power spectrum (take playhead or live).
class PowerTab extends StatelessWidget {
  const PowerTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      final monitor = vm.monitor;
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
        child: ListenableBuilder(
          listenable: monitor,
          builder: (context, _) => _SpectrumStrip(
            column: monitor.latest ?? Float32List(0),
            binHz: monitor.binHz,
            maxHz: monitor.maxHz,
            minCeilDb: 48,
            formants: [monitor.currentF1, monitor.currentF2, monitor.currentF3],
          ),
        ),
      );
    }
    return _withSpectrogram(vm, (spec, metrics, track) {
      final spanMs = vm.analysisDurationMs;
      final startMs = vm.analysisStartMs;
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
        child: ValueListenableBuilder<Duration>(
          valueListenable: vm.position,
          builder: (context, pos, _) {
            final seconds = _localSeconds(pos, spanMs, startMs);
            // Guard frames>0: an empty take's f1/f2/f3 are empty lists.
            final hasFrames = metrics != null && metrics.frames > 0;
            final fr = hasFrames ? metrics.frameAtTime(seconds) : null;
            final formants = fr == null
                ? const <double>[]
                : [metrics!.f1[fr], metrics.f2[fr], metrics.f3[fr]];
            return _SpectrumStrip(
              column: spec.columnAtTime(seconds),
              binHz: spec.binHz,
              maxHz: spec.maxHz,
              formants: formants,
            );
          },
        ),
      );
    });
  }
}

/// Bottom-dock tab: the F1×F2 vowel chart (take playhead or live).
class VowelTab extends StatelessWidget {
  const VowelTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      final monitor = vm.monitor;
      return ListenableBuilder(
        listenable: monitor,
        builder: (context, _) =>
            _vowelPanel(monitor.currentF1, monitor.currentF2),
      );
    }
    return _withMetrics(vm, (m, track) {
      final spanMs = vm.analysisDurationMs;
      final startMs = vm.analysisStartMs;
      return ValueListenableBuilder<Duration>(
        valueListenable: vm.position,
        builder: (context, pos, _) {
          final fr = m.frameAtTime(_localSeconds(pos, spanMs, startMs));
          return _vowelPanel(m.f1[fr], m.f2[fr]);
        },
      );
    });
  }
}

/// A large centered F1×F2 vowel chart filling the tab.
Widget _vowelPanel(double f1, double f2) {
  return Padding(
    padding: const EdgeInsets.fromLTRB(20, 14, 20, 18),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text(
          'VOWEL  F1×F2',
          style: TextStyle(
            color: Studio.textFaint,
            fontSize: 10,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.8,
          ),
        ),
        const SizedBox(height: 8),
        Expanded(
          child: Center(
            child: AspectRatio(
              aspectRatio: 1.3,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Studio.inset,
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Padding(
                  padding: const EdgeInsets.all(12),
                  child: CustomPaint(
                    size: Size.infinite,
                    painter: VowelSpacePainter(f1: f1, f2: f2),
                  ),
                ),
              ),
            ),
          ),
        ),
      ],
    ),
  );
}

/// Bottom-dock tab: pitch — the live tuner, or the take's Logic-style pitch map.
class PitchTab extends StatelessWidget {
  const PitchTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 20, 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [_LiveTuner(monitor: vm.monitor)],
        ),
      );
    }
    return _withMetrics(vm, (m, track) {
      // PitchLane derives the metrics' absolute origin from track.startMs (its
      // take-local playhead mapping and note readout both subtract it). When
      // the analysis subject is one clip of a multi-clip lane, [m] is
      // clip-local, so hand it a single-clip view spanning the subject axis
      // instead of the whole lane. A whole-lane subject's axis equals the lane
      // span, so the original track passes through untouched there.
      final startMs = vm.analysisStartMs;
      final spanMs = vm.analysisDurationMs;
      final subject = (startMs == track.startMs && spanMs == track.durationMs)
          ? track
          : track.copyWith(
              clips: [
                AudioClip(
                  id: '${track.id}-analysis-subject',
                  audioFilePath: track.primaryClip?.audioFilePath ?? '',
                  durationMs: spanMs,
                  startMs: startMs,
                ),
              ],
            );
      return LayoutBuilder(
        builder: (context, c) => PitchLane(
          track: subject,
          timelineDurationMs: spanMs,
          position: vm.position,
          headerWidth: 132,
          loading: false,
          metrics: m,
          height: c.maxHeight,
          takeLocal: true,
        ),
      );
    });
  }
}

/// Bottom-dock tab: timbre — RING/TILT/CPP/H1−H2 numbers + their time curves
/// (take), or the live numbers.
class TimbreTab extends StatelessWidget {
  const TimbreTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 20, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _LiveMetricsBar(monitor: vm.monitor),
            const SizedBox(height: 16),
            const Text(
              'Timbre curves are available for recorded takes.',
              style: TextStyle(color: Studio.textFaint, fontSize: 12),
            ),
          ],
        ),
      );
    }
    return _withMetrics(vm, (m, track) {
      final spanMs = vm.analysisDurationMs;
      final startMs = vm.analysisStartMs;
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _MetricsBar(
              vm: vm,
              metrics: m,
              durationMs: spanMs,
              startMs: startMs,
            ),
            const SizedBox(height: 12),
            const _TimbreLegend(),
            const SizedBox(height: 6),
            Expanded(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Studio.inset,
                  borderRadius: BorderRadius.circular(6),
                ),
                child: Stack(
                  children: [
                    Positioned.fill(
                      child: CustomPaint(
                        painter: _TimbreCurvesPainter(
                          metrics: m,
                          timelineDurationMs: spanMs,
                        ),
                      ),
                    ),
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: PlayheadPainter(
                            position: vm.position,
                            timelineDurationMs: spanMs,
                            color: Studio.playhead,
                            startMs: startMs,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      );
    });
  }
}

/// Color key for the timbre time curves.
class _TimbreLegend extends StatelessWidget {
  const _TimbreLegend();

  @override
  Widget build(BuildContext context) {
    Widget entry(Color c, String l) => Padding(
      padding: const EdgeInsets.only(right: 16),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(width: 12, height: 2.5, color: c),
          const SizedBox(width: 5),
          Text(
            l,
            style: const TextStyle(color: Studio.textSecondary, fontSize: 10),
          ),
        ],
      ),
    );
    return Row(
      children: [
        entry(_timbreRing, 'RING'),
        entry(_timbreTilt, 'TILT'),
        entry(_timbreCpp, 'CPP'),
        entry(_timbreH1H2, 'H1−H2'),
      ],
    );
  }
}

/// Draws RING/TILT/CPP/H1−H2 over time, each auto-scaled to its own range so
/// all four are visible together (shape matters, not absolute level).
class _TimbreCurvesPainter extends CustomPainter {
  _TimbreCurvesPainter({
    required this.metrics,
    required this.timelineDurationMs,
  });

  final VoiceMetrics metrics;
  final int timelineDurationMs;

  @override
  void paint(Canvas canvas, Size size) {
    final frames = metrics.frames, dur = timelineDurationMs;
    if (frames == 0 || dur <= 0) return;

    void curve(Float32List c, Color color) {
      var mn = double.infinity, mx = -double.infinity;
      for (final v in c) {
        if (v.isFinite) {
          if (v < mn) mn = v;
          if (v > mx) mx = v;
        }
      }
      if (!(mx > mn)) return;
      final path = Path();
      var started = false;
      for (var fr = 0; fr < frames; fr++) {
        final v = c[fr];
        if (!v.isFinite) {
          started = false;
          continue;
        }
        final x =
            (metrics.timeOfFrame(fr) * 1000 / dur).clamp(0.0, 1.0) * size.width;
        // 7% top/bottom padding so curves don't touch the edges.
        final y =
            (1 - (v - mn) / (mx - mn)) * size.height * 0.86 +
            size.height * 0.07;
        if (!started) {
          path.moveTo(x, y);
          started = true;
        } else {
          path.lineTo(x, y);
        }
      }
      canvas.drawPath(
        path,
        Paint()
          ..color = color
          ..style = PaintingStyle.stroke
          ..strokeWidth = 1.4,
      );
    }

    curve(metrics.cpp, _timbreCpp);
    curve(metrics.tilt, _timbreTilt);
    curve(metrics.ring, _timbreRing);
    curve(metrics.h1h2, _timbreH1H2);
  }

  @override
  bool shouldRepaint(_TimbreCurvesPainter old) =>
      old.metrics != metrics || old.timelineDurationMs != timelineDurationMs;
}

/// A small inline "LABEL value" stat used in tab headers.
class _StatInline extends StatelessWidget {
  const _StatInline({
    required this.label,
    required this.value,
    this.accent = Studio.textPrimary,
  });

  final String label;
  final String value;
  final Color accent;

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          '${label.toUpperCase()}  ',
          style: const TextStyle(
            color: Studio.textFaint,
            fontSize: 9,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.5,
          ),
        ),
        Text(
          value,
          style: TextStyle(
            color: accent,
            fontSize: 12,
            fontWeight: FontWeight.w700,
          ),
        ),
      ],
    );
  }
}

/// Bottom-dock tab: the long-term average spectrum (the take's spectral
/// "fingerprint" — brightness + singer's-formant cluster). Take-only.
class LtasTab extends StatelessWidget {
  const LtasTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      final monitor = vm.monitor;
      return ListenableBuilder(
        listenable: monitor,
        builder: (context, _) {
          final ltas = monitor.liveLtas;
          if (ltas == null || !ltas.hasData) {
            return const StudioHint(
              'Sing into the mic to build your live average spectrum.',
            );
          }
          return _ltasBody(ltas, live: true);
        },
      );
    }
    return _withSpectrogram(vm, (spec, metrics, track) {
      final ltas = vm.selectedLtas;
      if (ltas == null || !ltas.hasData) {
        return const StudioHint('Sing a passage to see your average spectrum.');
      }
      return _ltasBody(ltas, live: false);
    });
  }

  Widget _ltasBody(Ltas ltas, {required bool live}) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(left: _axisWidth, bottom: 6),
            child: Row(
              children: [
                if (live) ...[const _LiveDot(), const SizedBox(width: 14)],
                _StatInline(
                  label: "Singer's formant",
                  value: '${ltas.singersFormantDb.round()} dB',
                ),
                const SizedBox(width: 22),
                _StatInline(
                  label: 'Brightness α',
                  value: '${ltas.alphaRatio.round()} dB',
                ),
              ],
            ),
          ),
          Expanded(
            child: _SpectrumStrip(
              column: ltas.db,
              binHz: ltas.binHz,
              maxHz: ltas.maxHz,
              label: live
                  ? 'LTAS · running average'
                  : 'LTAS · average spectrum',
            ),
          ),
        ],
      ),
    );
  }
}

/// Bottom-dock tab: vibrato of the note under the playhead (take-only).
class VibratoTab extends StatelessWidget {
  const VibratoTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      final monitor = vm.monitor;
      return ListenableBuilder(
        listenable: monitor,
        builder: (context, _) {
          final v = analyzeVibratoLive(monitor.f0History, monitor.frameRate);
          return _vibratoBody(
            v,
            live: true,
            emptyHint:
                'Hold a sustained note into the mic to measure its vibrato.',
          );
        },
      );
    }
    return _withMetrics(vm, (m, track) {
      final spanMs = vm.analysisDurationMs;
      final startMs = vm.analysisStartMs;
      final notes = segmentPitchNotes(m);
      // Memoize per note once per build — the take's vibrato doesn't change as
      // the playhead moves, so we must not recompute it on every position tick.
      final byNote = <PitchNote, Vibrato>{
        for (final n in notes) n: analyzeVibrato(m, n.startFrame, n.endFrame),
      };
      return ValueListenableBuilder<Duration>(
        valueListenable: vm.position,
        builder: (context, pos, _) {
          final fr = m.frameAtTime(_localSeconds(pos, spanMs, startMs));
          Vibrato? v;
          for (final n in notes) {
            if (fr >= n.startFrame && fr <= n.endFrame) {
              v = byNote[n];
              break;
            }
          }
          return _vibratoBody(
            v,
            live: false,
            emptyHint:
                'Hold a sustained note on the playhead to measure its vibrato.',
          );
        },
      );
    });
  }

  Widget _vibratoBody(
    Vibrato? v, {
    required bool live,
    required String emptyHint,
  }) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(20, 16, 20, 18),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _vibratoHeader(v, live: live),
          const SizedBox(height: 14),
          Expanded(
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: Studio.inset,
                borderRadius: BorderRadius.circular(6),
              ),
              child: (v != null && v.deviationCents.length > 4)
                  ? CustomPaint(
                      size: Size.infinite,
                      painter: _VibratoPainter(v),
                    )
                  : Center(
                      child: Text(
                        emptyHint,
                        style: const TextStyle(
                          color: Studio.textFaint,
                          fontSize: 12,
                        ),
                      ),
                    ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _vibratoHeader(Vibrato? v, {required bool live}) {
    final liveDot = live
        ? const Padding(padding: EdgeInsets.only(right: 6), child: _LiveDot())
        : const SizedBox.shrink();
    if (v == null || !v.present) {
      return Row(
        children: [
          liveDot,
          const _StatInline(label: 'Vibrato', value: 'none / straight tone'),
        ],
      );
    }
    final healthy = v.rateHz >= 5 && v.rateHz <= 7;
    return Wrap(
      spacing: 22,
      runSpacing: 8,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
        if (live) const _LiveDot(),
        _StatInline(
          label: 'Rate',
          value: '${v.rateHz.toStringAsFixed(1)} Hz',
          accent: healthy ? _tunerGreen : Studio.record,
        ),
        _StatInline(label: 'Extent', value: '±${v.extentCents.round()}¢'),
        _StatInline(
          label: 'Regularity',
          value: '${(v.regularity * 100).round()}%',
        ),
        if (!v.jitter.isNaN)
          _StatInline(
            label: 'Jitter',
            value: '${(v.jitter * 100).round()}%',
            accent: v.jitter <= 0.1 ? _tunerGreen : Studio.record,
          ),
        if (!v.onsetMs.isNaN)
          _StatInline(label: 'Onset', value: '${v.onsetMs.round()} ms'),
      ],
    );
  }
}

/// Draws the vibrato pitch-deviation waveform (cents around 0).
class _VibratoPainter extends CustomPainter {
  _VibratoPainter(this.v);

  final Vibrato v;

  @override
  void paint(Canvas canvas, Size size) {
    final dev = v.deviationCents;
    final n = dev.length;
    if (n < 2) return;
    final w = size.width, midY = size.height / 2;
    final amp = (v.extentCents <= 0 ? 50.0 : v.extentCents) * 1.6;
    double y(double cents) =>
        midY - (cents / amp).clamp(-1.0, 1.0) * (size.height * 0.42);

    canvas.drawLine(
      Offset(0, midY),
      Offset(w, midY),
      Paint()
        ..color = Studio.border
        ..strokeWidth = 1,
    );
    final path = Path();
    for (var i = 0; i < n; i++) {
      final x = i / (n - 1) * w;
      final yy = y(dev[i]);
      if (i == 0) {
        path.moveTo(x, yy);
      } else {
        path.lineTo(x, yy);
      }
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = Studio.accentBright
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(_VibratoPainter old) => old.v != v;
}

const Color _chestColor = Studio.registerChest; // warm = chest
const Color _headColor = Studio.registerHead; // cool = head

/// Bottom-dock tab: the pitch contour colored by register (chest↔head). An
/// indicator from H1−H2, not a verdict. Take-only.
class RegisterTab extends StatelessWidget {
  const RegisterTab({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    if (vm.liveMonitor) {
      final monitor = vm.monitor;
      return ListenableBuilder(
        listenable: monitor,
        builder: (context, _) => _LiveRegisterPanel(monitor: monitor),
      );
    }
    return _withMetrics(vm, (m, track) {
      final reg = vm.selectedRegister;
      if (reg == null || !reg.hasData) {
        return const StudioHint('Sing a passage to map your registers.');
      }
      final range = pitchSemitoneRange(m);
      final spanMs = vm.analysisDurationMs;
      final startMs = vm.analysisStartMs;
      return Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.only(left: _axisWidth, bottom: 6),
              child: Wrap(
                spacing: 18,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  _regSwatch(_chestColor, 'chest ${reg.chestPct.round()}%'),
                  _regSwatch(
                    Color.lerp(_chestColor, _headColor, 0.5)!,
                    'mix ${reg.mixPct.round()}%',
                  ),
                  _regSwatch(_headColor, 'head ${reg.headPct.round()}%'),
                ],
              ),
            ),
            Expanded(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Studio.inset,
                  borderRadius: BorderRadius.circular(6),
                ),
                child: Stack(
                  children: [
                    Positioned.fill(
                      child: CustomPaint(
                        painter: _RegisterMapPainter(
                          metrics: m,
                          register: reg,
                          loSemi: range.lo,
                          hiSemi: range.hi,
                          durationMs: spanMs,
                        ),
                      ),
                    ),
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: PlayheadPainter(
                            position: vm.position,
                            timelineDurationMs: spanMs,
                            color: Studio.playhead,
                            startMs: startMs,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      );
    });
  }
}

Widget _regSwatch(Color c, String label) => Row(
  mainAxisSize: MainAxisSize.min,
  children: [
    Container(
      width: 9,
      height: 9,
      decoration: BoxDecoration(color: c, shape: BoxShape.circle),
    ),
    const SizedBox(width: 5),
    Text(
      label,
      style: const TextStyle(color: Studio.textSecondary, fontSize: 10),
    ),
  ],
);

/// The live register panel: running chest/mix/head split plus a real-time
/// chest↔head meter driven by the current frame's H1−H2.
class _LiveRegisterPanel extends StatelessWidget {
  const _LiveRegisterPanel({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    final total = monitor.registerVoiced;
    final inv = total == 0 ? 0.0 : 100.0 / total;
    final h = monitor.currentH1H2;
    final hasNow = h.isFinite;
    final score = hasNow ? registerScore(h) : double.nan;
    final label = !hasNow
        ? '—'
        : score < 0.4
        ? 'CHEST'
        : score < 0.6
        ? 'MIX'
        : 'HEAD';
    if (total == 0 && !hasNow) {
      return const StudioHint('Sing into the mic to map your registers live.');
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 20, 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(left: _axisWidth, bottom: 6),
            child: Wrap(
              spacing: 18,
              runSpacing: 6,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                const _LiveDot(),
                _regSwatch(
                  _chestColor,
                  'chest ${(monitor.registerChest * inv).round()}%',
                ),
                _regSwatch(
                  Color.lerp(_chestColor, _headColor, 0.5)!,
                  'mix ${(monitor.registerMix * inv).round()}%',
                ),
                _regSwatch(
                  _headColor,
                  'head ${(monitor.registerHead * inv).round()}%',
                ),
              ],
            ),
          ),
          Expanded(
            child: Center(
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Text(
                    label,
                    style: TextStyle(
                      color: hasNow
                          ? Color.lerp(
                              _chestColor,
                              _headColor,
                              score.clamp(0.0, 1.0),
                            )
                          : Studio.textFaint,
                      fontSize: 24,
                      fontWeight: FontWeight.w800,
                      letterSpacing: 1.5,
                    ),
                  ),
                  const SizedBox(height: 4),
                  Text(
                    hasNow
                        ? 'H1−H2  ${h.toStringAsFixed(1)} dB'
                        : 'Hold a sustained note',
                    style: const TextStyle(
                      color: Studio.textFaint,
                      fontSize: 11,
                    ),
                  ),
                  const SizedBox(height: 20),
                  SizedBox(width: 280, child: _RegisterMeter(score: score)),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// A horizontal chest↔head gradient meter with a marker at the current [score]
/// (NaN hides the marker).
class _RegisterMeter extends StatelessWidget {
  const _RegisterMeter({required this.score});

  final double score;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        SizedBox(
          height: 14,
          child: LayoutBuilder(
            builder: (context, c) => Stack(
              clipBehavior: Clip.none,
              children: [
                Container(
                  decoration: BoxDecoration(
                    borderRadius: BorderRadius.circular(7),
                    gradient: const LinearGradient(
                      colors: [_chestColor, _headColor],
                    ),
                  ),
                ),
                if (!score.isNaN)
                  Positioned(
                    left: (score.clamp(0.0, 1.0) * c.maxWidth - 1.5).clamp(
                      0.0,
                      c.maxWidth - 3,
                    ),
                    top: -3,
                    child: Container(
                      width: 3,
                      height: 20,
                      decoration: BoxDecoration(
                        color: Studio.textPrimary,
                        borderRadius: BorderRadius.circular(2),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 6),
        const Padding(
          padding: EdgeInsets.symmetric(horizontal: 2),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              Text(
                'chest',
                style: TextStyle(color: Studio.textFaint, fontSize: 9),
              ),
              Text(
                'head',
                style: TextStyle(color: Studio.textFaint, fontSize: 9),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A small red "● LIVE" badge for live analysis-tab headers.
class _LiveDot extends StatelessWidget {
  const _LiveDot();

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Container(
          width: 8,
          height: 8,
          decoration: const BoxDecoration(
            color: Studio.record,
            shape: BoxShape.circle,
          ),
        ),
        const SizedBox(width: 6),
        const Text(
          'LIVE',
          style: TextStyle(
            color: Studio.record,
            fontSize: 10,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.5,
          ),
        ),
      ],
    );
  }
}

/// Draws each voiced frame as a dot at its pitch (semitone Y), colored by its
/// register score (chest→head gradient).
class _RegisterMapPainter extends CustomPainter {
  _RegisterMapPainter({
    required this.metrics,
    required this.register,
    required this.loSemi,
    required this.hiSemi,
    required this.durationMs,
  });

  final VoiceMetrics metrics;
  final RegisterMap register;
  final int loSemi, hiSemi;
  final int durationMs;

  @override
  void paint(Canvas canvas, Size size) {
    final frames = metrics.frames;
    if (frames == 0 || durationMs <= 0) return;
    final span = (hiSemi - loSemi).clamp(1, 1 << 30);
    for (var fr = 0; fr < frames; fr++) {
      final hz = metrics.f0[fr];
      final s = register.score[fr];
      if (hz.isNaN || hz <= 0 || s.isNaN) continue;
      final midi = 69 + 12 * (math.log(hz / 440) / math.ln2);
      // Time-based x (matches the playhead), not (fr/frames) — the frame count
      // is ~fftSize/hop short of len/hop, which would drift the dots right.
      final x =
          (metrics.timeOfFrame(fr) * 1000 / durationMs).clamp(0.0, 1.0) *
          size.width;
      final y = (1 - (midi - loSemi) / span).clamp(0.0, 1.0) * size.height;
      canvas.drawCircle(
        Offset(x, y),
        1.4,
        Paint()
          ..color = Color.lerp(_chestColor, _headColor, s.clamp(0.0, 1.0))!,
      );
    }
  }

  @override
  bool shouldRepaint(_RegisterMapPainter old) =>
      old.metrics != metrics ||
      old.register != register ||
      old.loSemi != loSemi ||
      old.hiSemi != hiSemi ||
      old.durationMs != durationMs;
}

/// The live metrics readout bar (F0 + ring/tilt/CPP), driven by the monitor.
class _LiveMetricsBar extends StatelessWidget {
  const _LiveMetricsBar({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: monitor,
      builder: (context, _) => Row(
        children: [
          // F0 is shown prominently by the tuner above, so it's dropped here.
          _Metric(
            label: 'RING',
            tip: 'Singer\'s-formant ring (3 kHz projection)',
            value: monitor.ring,
            avg: monitor.ringAvg,
          ),
          const SizedBox(width: 8),
          _Metric(
            label: 'TILT',
            tip: 'Spectral tilt: higher = brighter/pressed',
            value: monitor.tilt,
            avg: monitor.tiltAvg,
          ),
          const SizedBox(width: 8),
          _Metric(
            label: 'CPP',
            tip: 'Cepstral peak prominence: higher = cleaner',
            value: monitor.cpp,
            avg: monitor.cppAvg,
          ),
          const Spacer(),
          const _FormantLegend(),
        ],
      ),
    );
  }
}

/// In-tune color for the live tuner (green); off-pitch ramps toward red.
const Color _tunerGreen = Studio.inTune;

/// A prominent real-time tuner (A2): the nearest note + a needle that swings
/// left/right with how flat/sharp the live pitch is, centering green when in
/// tune. Driven by the monitor's smoothed live F0.
class _LiveTuner extends StatelessWidget {
  const _LiveTuner({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: monitor,
      builder: (context, _) {
        final note = noteOf(monitor.tunerF0);
        return Container(
          height: 62,
          padding: const EdgeInsets.fromLTRB(14, 8, 14, 8),
          decoration: BoxDecoration(
            color: Studio.surface,
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: Studio.border),
          ),
          child: note == null
              ? const Center(
                  child: Text(
                    'Sing a sustained note to tune…',
                    style: TextStyle(color: Studio.textFaint, fontSize: 13),
                  ),
                )
              : _content(note.name, note.cents),
        );
      },
    );
  }

  Widget _content(String name, double cents) {
    final color = Color.lerp(
      _tunerGreen,
      Studio.record,
      (cents.abs() / 35).clamp(0.0, 1.0),
    )!;
    return Row(
      children: [
        SizedBox(
          width: 56,
          child: Text(
            name,
            style: TextStyle(
              color: color,
              fontSize: 24,
              fontWeight: FontWeight.w800,
            ),
          ),
        ),
        Expanded(
          child: CustomPaint(
            size: Size.infinite,
            painter: _TunerDialPainter(cents: cents, color: color),
          ),
        ),
        SizedBox(
          width: 60,
          child: Text(
            '${cents >= 0 ? '+' : '−'}${cents.abs().round()}¢',
            textAlign: TextAlign.right,
            style: TextStyle(
              color: color,
              fontSize: 15,
              fontWeight: FontWeight.w700,
            ),
          ),
        ),
      ],
    );
  }
}

class _TunerDialPainter extends CustomPainter {
  _TunerDialPainter({required this.cents, required this.color});

  final double cents;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final midY = size.height / 2;
    final w = size.width;
    double xOf(double c) => (c.clamp(-50.0, 50.0) + 50) / 100 * w;

    // Baseline.
    canvas.drawLine(
      Offset(0, midY),
      Offset(w, midY),
      Paint()
        ..color = Studio.gridStrong
        ..strokeWidth = 2
        ..strokeCap = StrokeCap.round,
    );
    // In-tune green zone (±12¢).
    canvas.drawLine(
      Offset(xOf(-12), midY),
      Offset(xOf(12), midY),
      Paint()
        ..color = _tunerGreen.withValues(alpha: 0.55)
        ..strokeWidth = 5
        ..strokeCap = StrokeCap.round,
    );
    // Ticks at −50/−25/0/+25/+50.
    for (final c in const [-50.0, -25.0, 0.0, 25.0, 50.0]) {
      final x = xOf(c);
      final h = c == 0 ? 11.0 : 6.0;
      canvas.drawLine(
        Offset(x, midY - h),
        Offset(x, midY + h),
        Paint()
          ..color = c == 0 ? Studio.textSecondary : Studio.textFaint
          ..strokeWidth = c == 0 ? 2 : 1,
      );
    }
    // Needle.
    final nx = xOf(cents);
    canvas.drawLine(
      Offset(nx, midY - 15),
      Offset(nx, midY + 15),
      Paint()
        ..color = color
        ..strokeWidth = 3
        ..strokeCap = StrokeCap.round,
    );
    canvas.drawCircle(Offset(nx, midY), 4.5, Paint()..color = color);
  }

  @override
  bool shouldRepaint(_TunerDialPainter old) =>
      old.cents != cents || old.color != color;
}

/// A frequency axis on the left + the given spectrogram [child] on the right.
class _AxisRow extends StatelessWidget {
  const _AxisRow({required this.maxHz, required this.child});

  final double maxHz;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SizedBox(
          width: _axisWidth,
          child: CustomPaint(painter: _FreqAxisPainter(maxHz: maxHz)),
        ),
        Expanded(child: child),
      ],
    );
  }
}

/// Frequency tick labels along the power-spectrum x-axis, placed at each kHz's
/// actual position (0 → left edge, [maxHz] → right edge) so they line up with
/// the curve instead of being crammed together.
class _FreqAxisLabels extends StatelessWidget {
  const _FreqAxisLabels({required this.maxHz});

  final double maxHz;

  @override
  Widget build(BuildContext context) {
    final maxKHz = (maxHz / 1000).floor();
    return LayoutBuilder(
      builder: (context, c) {
        final w = c.maxWidth;
        return Stack(
          fit: StackFit.expand,
          children: [
            for (var k = 0; k <= maxKHz; k++)
              Positioned(
                left: (k * 1000 / maxHz * w).clamp(0.0, w),
                bottom: 2,
                child: FractionalTranslation(
                  translation: Offset(
                    k == 0 ? 0 : (k == maxKHz ? -1 : -0.5),
                    0,
                  ),
                  child: Text(
                    '$k kHz',
                    style: const TextStyle(
                      color: Studio.textFaint,
                      fontSize: 9,
                    ),
                  ),
                ),
              ),
          ],
        );
      },
    );
  }
}

/// A small uppercase panel caption (e.g. SPECTROGRAM / POWER SPECTRUM),
/// indented past the frequency axis so the two panels read consistently.
class _PanelLabel extends StatelessWidget {
  const _PanelLabel(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(left: _axisWidth, bottom: 4),
      child: Text(
        text.toUpperCase(),
        style: const TextStyle(
          color: Studio.textFaint,
          fontSize: 10,
          fontWeight: FontWeight.w700,
          letterSpacing: 0.8,
        ),
      ),
    );
  }
}

class _SpectrumStrip extends StatelessWidget {
  const _SpectrumStrip({
    required this.column,
    required this.binHz,
    required this.maxHz,
    this.formants = const [],
    this.minCeilDb,
    this.label = 'Power spectrum',
  });

  final Float32List column;
  final double binHz;
  final double maxHz;
  final List<double> formants;
  final double? minCeilDb;
  final String label;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        _PanelLabel(label),
        Expanded(
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const SizedBox(
                width: _axisWidth,
                child: Center(
                  child: RotatedBox(
                    quarterTurns: 3,
                    child: Text(
                      'dB',
                      style: TextStyle(color: Studio.textFaint, fontSize: 10),
                    ),
                  ),
                ),
              ),
              Expanded(
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: Studio.inset,
                    borderRadius: BorderRadius.circular(6),
                  ),
                  child: CustomPaint(
                    painter: PowerSpectrumPainter(
                      column: column,
                      binHz: binHz,
                      maxHz: maxHz,
                      formants: formants,
                      minCeilDb: minCeilDb,
                    ),
                    child: _FreqAxisLabels(maxHz: maxHz),
                  ),
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A compact readout of the take's timbre metrics — ring (F18), tilt (F19) and
/// CPP (F20) — each showing the value at the playhead plus the take average.
class _MetricsBar extends StatelessWidget {
  const _MetricsBar({
    required this.vm,
    required this.metrics,
    required this.durationMs,
    this.startMs = 0,
  });

  final AnalysisSource vm;
  final VoiceMetrics metrics;
  final int durationMs;
  final int startMs;

  @override
  Widget build(BuildContext context) {
    return ValueListenableBuilder<Duration>(
      valueListenable: vm.position,
      builder: (context, pos, _) {
        final t = (pos.inMilliseconds - startMs).clamp(0, durationMs) / 1000.0;
        return Row(
          children: [
            _F0Chip(hz: metrics.sampleAt(metrics.f0, t)),
            const SizedBox(width: 8),
            _Metric(
              label: 'RING',
              tip:
                  'Singer\'s-formant ring (3 kHz projection): higher carries more',
              value: metrics.sampleAt(metrics.ring, t),
              avg: metrics.ringAvg,
            ),
            const SizedBox(width: 8),
            _Metric(
              label: 'TILT',
              tip:
                  'Spectral tilt: higher = brighter/pressed, lower = darker/breathy',
              value: metrics.sampleAt(metrics.tilt, t),
              avg: metrics.tiltAvg,
            ),
            const SizedBox(width: 8),
            _Metric(
              label: 'CPP',
              tip:
                  'Cepstral peak prominence: higher = cleaner, lower = breathier',
              value: metrics.sampleAt(metrics.cpp, t),
              avg: metrics.cppAvg,
            ),
            const Spacer(),
            const _FormantLegend(),
          ],
        );
      },
    );
  }
}

/// Color key for the F1/F2/F3 formant tracks drawn on the spectrogram.
class _FormantLegend extends StatelessWidget {
  const _FormantLegend();

  @override
  Widget build(BuildContext context) {
    Widget entry(Color color, String label) => Padding(
      padding: const EdgeInsets.only(left: 10),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            width: 8,
            height: 8,
            decoration: BoxDecoration(color: color, shape: BoxShape.circle),
          ),
          const SizedBox(width: 4),
          Text(
            label,
            style: const TextStyle(color: Studio.textSecondary, fontSize: 10),
          ),
        ],
      ),
    );
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        entry(f0Color, 'F0'),
        entry(formantColors[0], 'F1'),
        entry(formantColors[1], 'F2'),
        entry(formantColors[2], 'F3'),
      ],
    );
  }
}

/// The F0 (pitch) readout: frequency + equal-tempered note name & cents.
class _F0Chip extends StatelessWidget {
  const _F0Chip({required this.hz});

  final double hz;

  @override
  Widget build(BuildContext context) {
    final has = !hz.isNaN && hz > 0;
    return Tooltip(
      message: 'Fundamental pitch (F0) at the playhead',
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: Studio.inset,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: Studio.border),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Container(
              width: 8,
              height: 8,
              decoration: const BoxDecoration(
                color: f0Color,
                shape: BoxShape.circle,
              ),
            ),
            const SizedBox(width: 6),
            const Text(
              'F0',
              style: TextStyle(
                color: Studio.textFaint,
                fontSize: 9,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
            const SizedBox(width: 6),
            Text(
              has ? '${hz.round()} Hz' : '—',
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 12,
                fontWeight: FontWeight.w700,
              ),
            ),
            if (has) ...[
              const SizedBox(width: 6),
              Text(
                noteLabel(hz),
                style: const TextStyle(
                  color: Studio.accentBright,
                  fontSize: 11,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _Metric extends StatelessWidget {
  const _Metric({
    required this.label,
    required this.tip,
    required this.value,
    required this.avg,
  });

  final String label;
  final String tip;
  final double value;
  final double avg;

  String _fmt(double v) => v.isNaN ? '—' : v.toStringAsFixed(1);

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: tip,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
        decoration: BoxDecoration(
          color: Studio.inset,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: Studio.border),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              label,
              style: const TextStyle(
                color: Studio.textFaint,
                fontSize: 9,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
            const SizedBox(width: 6),
            Text(
              '${_fmt(value)} dB',
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 12,
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(width: 5),
            Text(
              'avg ${_fmt(avg)}',
              style: const TextStyle(color: Studio.textFaint, fontSize: 9),
            ),
          ],
        ),
      ),
    );
  }
}

class SourceToggle extends StatelessWidget {
  const SourceToggle({required this.live, required this.onSelect, super.key});

  final bool live;
  final ValueChanged<bool> onSelect;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.all(2),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(7),
        border: Border.all(color: Studio.border),
      ),
      child: Row(
        children: [
          _seg('Take', !live, () => onSelect(false)),
          _seg('Live', live, () => onSelect(true)),
        ],
      ),
    );
  }

  Widget _seg(String label, bool selected, VoidCallback onTap) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
          decoration: BoxDecoration(
            color: selected
                ? (label == 'Live' ? Studio.record : Studio.accent)
                : Colors.transparent,
            borderRadius: BorderRadius.circular(5),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: selected ? Studio.background : Studio.textSecondary,
              fontSize: 12,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

class _MonitorLevel extends StatelessWidget {
  const _MonitorLevel({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 8,
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(4),
      ),
      clipBehavior: Clip.antiAlias,
      child: Align(
        alignment: Alignment.centerLeft,
        child: ListenableBuilder(
          listenable: monitor,
          builder: (context, _) => FractionallySizedBox(
            widthFactor: monitor.level.clamp(0.0, 1.0),
            child: const ColoredBox(color: Studio.record),
          ),
        ),
      ),
    );
  }
}

class _FreqAxisPainter extends CustomPainter {
  _FreqAxisPainter({required this.maxHz});

  final double maxHz;

  @override
  void paint(Canvas canvas, Size size) {
    for (var hz = 1000.0; hz <= maxHz; hz += 1000) {
      final y = size.height * (1 - hz / maxHz);
      final tp = TextPainter(
        text: TextSpan(
          text: '${(hz / 1000).round()}k',
          style: const TextStyle(color: Studio.textFaint, fontSize: 10),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, Offset(size.width - tp.width - 6, y - tp.height / 2));
    }
  }

  @override
  bool shouldRepaint(_FreqAxisPainter old) => old.maxHz != maxHz;
}
