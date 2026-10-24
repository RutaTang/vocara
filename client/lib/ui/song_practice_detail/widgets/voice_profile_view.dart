import 'package:flutter/material.dart';

import '../../../data/services/monitor_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_hint.dart';
import '../../core/widgets/waveform_painter.dart';
import '../view_model/analysis_source.dart';
import 'analysis_lanes.dart';

/// Shared hint/loading guard for the take-only Range/Dynamics tabs.
Widget _profileGuard(
  AnalysisSource vm,
  String liveHint,
  String emptyHint,
  Widget Function(VoiceProfile p, VoiceMetrics m, AudioTrack track) build,
) {
  if (vm.liveMonitor) return StudioHint(liveHint);
  final track = vm.selectedTrack;
  if (track == null || track.isBacking) {
    return const StudioHint('Select a recorded take to analyze.');
  }
  if (vm.spectrogramLoading) {
    return const StudioHint('Analyzing…', spinner: true);
  }
  final p = vm.selectedProfile;
  final m = vm.selectedMetrics;
  if (p == null || m == null) return StudioHint(emptyHint);
  return build(p, m, track);
}

/// Bottom-dock tab: the sung range, comfortable tessitura and estimated
/// register break (passaggio) for the selected take.
class RangeView extends StatelessWidget {
  const RangeView({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: vm,
      builder: (context, _) => _profileGuard(
        vm,
        'Range is computed from a recorded take. Switch to Take.',
        'Sing a passage to map your range.',
        (p, m, track) {
          if (!p.hasRange) {
            return const StudioHint('Sing a passage to map your range.');
          }
          return Padding(
            padding: const EdgeInsets.fromLTRB(20, 28, 20, 18),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.center,
              children: [
                Wrap(
                  spacing: 28,
                  runSpacing: 12,
                  alignment: WrapAlignment.center,
                  children: [
                    _Stat(
                      label: 'RANGE',
                      value:
                          '${noteNameForMidi(p.lowMidi!)} – ${noteNameForMidi(p.highMidi!)}',
                      sub: '${p.highMidi! - p.lowMidi!} semitones',
                    ),
                    if (p.tessLoMidi != null && p.tessHiMidi != null)
                      _Stat(
                        label: 'TESSITURA',
                        value:
                            '${noteNameForMidi(p.tessLoMidi!)} – ${noteNameForMidi(p.tessHiMidi!)}',
                        sub: 'comfortable zone',
                      ),
                    if (p.hasPassaggio)
                      _Stat(
                        label: 'PASSAGGIO',
                        value: noteNameForMidi(p.passaggioMidi!),
                        sub: p.passaggioJumpDb >= 6
                            ? 'abrupt shift'
                            : 'gradual shift',
                        accent: Studio.record,
                      ),
                  ],
                ),
                const SizedBox(height: 18),
                const _CapLabel(
                  'RANGE  ·  tessitura highlighted, passaggio marked',
                ),
                const SizedBox(height: 6),
                // size:Size.infinite so the childless CustomPaint fills the
                // (non-stretched) Column width instead of collapsing to zero.
                Expanded(
                  child: CustomPaint(
                    size: Size.infinite,
                    painter: _RangeBarPainter(p),
                  ),
                ),
              ],
            ),
          );
        },
      ),
    );
  }
}

/// Bottom-dock tab: the take's loudness contour over time + its dynamic span.
class DynamicsView extends StatelessWidget {
  const DynamicsView({required this.vm, super.key});

  final AnalysisSource vm;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: vm,
      builder: (context, _) {
        if (vm.liveMonitor) return _LiveDynamicsPanel(monitor: vm.monitor);
        return _profileGuard(
          vm,
          'Dynamics are computed from a recorded take. Switch to Take.',
          'Sing a passage to see your dynamics.',
          (p, m, track) {
            if (m.frames == 0 || !p.hasDynamics) {
              return const StudioHint('Sing a passage to see your dynamics.');
            }
            // [m] covers the analysis SUBJECT's span (one clip of a multi-clip
            // lane when a specific clip is selected), not always the whole
            // lane — map the loudness curve and playhead over that subject
            // axis, like the other tabs (see PitchTab / TimbreTab).
            final startMs = vm.analysisStartMs;
            final spanMs = vm.analysisDurationMs;
            return Padding(
              padding: const EdgeInsets.fromLTRB(20, 16, 20, 18),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Wrap(
                    spacing: 28,
                    runSpacing: 12,
                    children: [
                      _Stat(
                        label: 'DYNAMIC RANGE',
                        value: '${p.dynamicRangeDb.round()} dB',
                        sub: 'soft → loud',
                      ),
                      _Stat(
                        label: 'SOFTEST',
                        value: '${p.softDb.round()} dBFS',
                        sub: '5th pct',
                      ),
                      _Stat(
                        label: 'LOUDEST',
                        value: '${p.loudDb.round()} dBFS',
                        sub: '95th pct',
                      ),
                    ],
                  ),
                  const SizedBox(height: 16),
                  const _CapLabel('LOUDNESS OVER TIME'),
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
                              painter: LoudnessLanePainter(
                                metrics: m,
                                timelineDurationMs: spanMs,
                                startMs: 0,
                              ),
                            ),
                          ),
                          Positioned.fill(
                            child: PlayheadLayer(
                              position: vm.position,
                              timelineDurationMs: spanMs,
                              color: Studio.playhead,
                              startMs: startMs,
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                  const SizedBox(height: 10),
                  Row(
                    children: [
                      const _CapLabel('SPAN'),
                      const SizedBox(width: 12),
                      Expanded(
                        child: SizedBox(
                          height: 12,
                          child: CustomPaint(painter: _DynBarPainter(p)),
                        ),
                      ),
                      const SizedBox(width: 12),
                      const Text(
                        '−60',
                        style: TextStyle(color: Studio.textFaint, fontSize: 9),
                      ),
                      const Expanded(child: SizedBox()),
                      const Text(
                        '0 dBFS',
                        style: TextStyle(color: Studio.textFaint, fontSize: 9),
                      ),
                    ],
                  ),
                ],
              ),
            );
          },
        );
      },
    );
  }
}

/// A labeled summary value (e.g. RANGE  A3 – E5  /  14 semitones).
class _Stat extends StatelessWidget {
  const _Stat({
    required this.label,
    required this.value,
    required this.sub,
    this.accent = Studio.textPrimary,
  });

  final String label;
  final String value;
  final String sub;
  final Color accent;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        _CapLabel(label),
        const SizedBox(height: 3),
        Text(
          value,
          style: TextStyle(
            color: accent,
            fontSize: 17,
            fontWeight: FontWeight.w800,
          ),
        ),
        Text(
          sub,
          style: const TextStyle(color: Studio.textFaint, fontSize: 10),
        ),
      ],
    );
  }
}

class _CapLabel extends StatelessWidget {
  const _CapLabel(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => Text(
    text,
    style: const TextStyle(
      color: Studio.textFaint,
      fontSize: 9,
      fontWeight: FontWeight.w700,
      letterSpacing: 0.6,
    ),
  );
}

/// The live (mic) dynamics panel: rolling loudness curve + soft/loud/range over
/// the recent voiced window. Mirrors the take's [DynamicsView] for Live mode.
class _LiveDynamicsPanel extends StatelessWidget {
  const _LiveDynamicsPanel({required this.monitor});

  final MonitorService monitor;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: monitor,
      builder: (context, _) {
        final d = monitor.liveDynamics;
        if (d.rangeDb.isNaN) {
          return const StudioHint(
            'Sing into the mic to measure your live dynamics.',
          );
        }
        return Padding(
          padding: const EdgeInsets.fromLTRB(20, 16, 20, 18),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Wrap(
                spacing: 28,
                runSpacing: 12,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  Row(
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
                      const SizedBox(width: 14),
                      _Stat(
                        label: 'DYNAMIC RANGE',
                        value: '${d.rangeDb.round()} dB',
                        sub: 'soft → loud',
                      ),
                    ],
                  ),
                  _Stat(
                    label: 'SOFTEST',
                    value: '${d.softDb.round()} dBFS',
                    sub: '5th pct',
                  ),
                  _Stat(
                    label: 'LOUDEST',
                    value: '${d.loudDb.round()} dBFS',
                    sub: '95th pct',
                  ),
                ],
              ),
              const SizedBox(height: 16),
              const _CapLabel('LOUDNESS (LIVE)'),
              const SizedBox(height: 6),
              Expanded(
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: Studio.inset,
                    borderRadius: BorderRadius.circular(6),
                  ),
                  child: CustomPaint(
                    size: Size.infinite,
                    painter: _LiveLoudnessPainter(
                      List<double>.from(monitor.loudnessHistory),
                    ),
                  ),
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

/// Draws the rolling live loudness contour (dBFS, −60..0) left→right.
class _LiveLoudnessPainter extends CustomPainter {
  _LiveLoudnessPainter(this.loud);

  final List<double> loud;

  @override
  void paint(Canvas canvas, Size size) {
    final n = loud.length;
    if (n < 2) return;
    const minDb = -60.0, maxDb = 0.0;
    double y(double db) =>
        (1 - (db.clamp(minDb, maxDb) - minDb) / (maxDb - minDb)) * size.height;
    final path = Path();
    var started = false;
    for (var i = 0; i < n; i++) {
      final v = loud[i];
      if (!v.isFinite) {
        started = false;
        continue;
      }
      final x = i / (n - 1) * size.width;
      final yy = y(v);
      if (!started) {
        path.moveTo(x, yy);
        started = true;
      } else {
        path.lineTo(x, yy);
      }
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = Studio.accentBright
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.4,
    );
  }

  @override
  bool shouldRepaint(_LiveLoudnessPainter old) => true;
}

void _profileText(
  Canvas canvas,
  String s,
  double x,
  double y,
  Color color,
  double size, {
  bool center = false,
  bool alignRight = false,
}) {
  final tp = TextPainter(
    text: TextSpan(
      text: s,
      style: TextStyle(
        color: color,
        fontSize: size,
        fontWeight: FontWeight.w600,
      ),
    ),
    textDirection: TextDirection.ltr,
  )..layout();
  var dx = x;
  if (center) {
    dx = x - tp.width / 2;
  } else if (alignRight) {
    dx = x - tp.width;
  }
  tp.paint(canvas, Offset(dx, y));
}

/// Horizontal pitch axis: the full sung range as a bar, the tessitura brighter,
/// the passaggio as a labeled tick, with C-octave gridlines + range labels.
class _RangeBarPainter extends CustomPainter {
  _RangeBarPainter(this.p);

  final VoiceProfile p;

  @override
  void paint(Canvas canvas, Size size) {
    final low = p.lowMidi!, high = p.highMidi!;
    final minMidi = low - 2, maxMidi = high + 2;
    final span = (maxMidi - minMidi).clamp(1, 1 << 30);
    final w = size.width, h = size.height;
    final barY = h * 0.5;
    double x(num midi) =>
        ((midi - minMidi) / span * w).clamp(0.0, w); // never off-panel

    // C-octave gridlines.
    final grid = Paint()
      ..color = Studio.gridStrong.withValues(alpha: 0.25)
      ..strokeWidth = 1;
    for (var mm = (minMidi / 12).ceil() * 12; mm <= maxMidi; mm += 12) {
      final gx = x(mm);
      canvas.drawLine(
        Offset(gx, barY - h * 0.32),
        Offset(gx, barY + h * 0.32),
        grid,
      );
    }

    // Full sung range (faint), then the brighter tessitura band.
    canvas.drawLine(
      Offset(x(low), barY),
      Offset(x(high), barY),
      Paint()
        ..color = Studio.accent.withValues(alpha: 0.45)
        ..strokeWidth = 9
        ..strokeCap = StrokeCap.round,
    );
    if (p.tessLoMidi != null && p.tessHiMidi != null) {
      canvas.drawLine(
        Offset(x(p.tessLoMidi!), barY),
        Offset(x(p.tessHiMidi!), barY),
        Paint()
          ..color = Studio.accentBright
          ..strokeWidth = 9
          ..strokeCap = StrokeCap.round,
      );
    }

    // Passaggio marker (constrained to the range, so always on the bar).
    if (p.passaggioMidi != null) {
      final px = x(p.passaggioMidi!);
      canvas.drawLine(
        Offset(px, barY - 13),
        Offset(px, barY + 13),
        Paint()
          ..color = Studio.record
          ..strokeWidth = 2,
      );
      _profileText(
        canvas,
        noteNameForMidi(p.passaggioMidi!),
        px,
        barY - 28,
        Studio.record,
        10,
        center: true,
      );
    }

    // Range end labels.
    _profileText(
      canvas,
      noteNameForMidi(low),
      x(low),
      barY + 14,
      Studio.textSecondary,
      11,
    );
    _profileText(
      canvas,
      noteNameForMidi(high),
      x(high),
      barY + 14,
      Studio.textSecondary,
      11,
      alignRight: true,
    );
  }

  @override
  bool shouldRepaint(_RangeBarPainter old) => old.p != p;
}

/// A soft→loud span drawn on a −60..0 dBFS scale.
class _DynBarPainter extends CustomPainter {
  _DynBarPainter(this.p);

  final VoiceProfile p;
  static const double _lo = -60, _hi = 0;

  double _x(double db, double w) =>
      ((db - _lo) / (_hi - _lo)).clamp(0.0, 1.0) * w;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width, midY = size.height / 2;
    canvas.drawLine(
      Offset(0, midY),
      Offset(w, midY),
      Paint()
        ..color = Studio.border
        ..strokeWidth = 5
        ..strokeCap = StrokeCap.round,
    );
    final x0 = _x(p.softDb, w), x1 = _x(p.loudDb, w);
    canvas.drawLine(
      Offset(x0, midY),
      Offset(x1, midY),
      Paint()
        ..shader = const LinearGradient(
          colors: [Studio.textSecondary, Studio.accentBright],
        ).createShader(Rect.fromLTWH(0, 0, w, size.height))
        ..strokeWidth = 5
        ..strokeCap = StrokeCap.round,
    );
  }

  @override
  bool shouldRepaint(_DynBarPainter old) => old.p != p;
}
