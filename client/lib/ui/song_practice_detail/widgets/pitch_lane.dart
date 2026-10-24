import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../../domain/models/audio_track.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_hint.dart';
import '../../core/widgets/waveform_painter.dart';

/// Height of the inline pitch sub-lane that folds out under a vocal take.
const double kPitchLaneHeight = 96;

/// In-tune color (green); off-pitch ramps toward [Studio.record].
const Color _inTune = Studio.inTune;

/// A sung note within ±this many cents of its semitone counts as "in tune".
const double _inTuneCents = 25;

const List<String> _semiNames = [
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

double _hzToMidi(double hz) => 69 + 12 * (math.log(hz / 440) / math.ln2);

String _semiName(int semi) =>
    '${_semiNames[((semi % 12) + 12) % 12]}${(semi ~/ 12) - 1}';

String _centsStr(double cents) {
  final c = cents.round();
  return '${c >= 0 ? '+' : '−'}${c.abs()}¢';
}

/// The semitone range to display for [m]'s F0: the voiced min/max ± 2 semitones,
/// widened to at least one octave. Falls back to C3–C5 when nothing is voiced.
({int lo, int hi}) pitchSemitoneRange(VoiceMetrics m) {
  var lo = double.infinity, hi = double.negativeInfinity;
  for (final hz in m.f0) {
    if (hz.isNaN || hz <= 0) continue;
    final s = _hzToMidi(hz);
    if (s < lo) lo = s;
    if (s > hi) hi = s;
  }
  if (lo > hi) return (lo: 48, hi: 72); // no voiced frames → C3..C5
  var loI = lo.floor() - 2, hiI = hi.ceil() + 2;
  while (hiI - loI < 12) {
    loI -= 1;
    hiI += 1;
  }
  return (lo: loI, hi: hiI);
}

/// A detected sung note: a run of voiced frames quantized to one [semitone],
/// with the median cents deviation of the take's actual pitch within it.
class PitchNote {
  PitchNote({
    required this.startFrame,
    required this.endFrame,
    required this.semitone,
    required this.medianCents,
  });

  final int startFrame;
  final int endFrame;
  final int semitone;
  final double medianCents;

  int get frames => endFrame - startFrame + 1;
  bool get inTune => medianCents.abs() <= _inTuneCents;
}

/// Segments the F0 contour into discrete sung notes: consecutive voiced frames
/// rounded to the same semitone (tolerating brief dropouts), dropping runs
/// shorter than [minFrames]. Each note's [PitchNote.medianCents] is the median
/// deviation from its semitone — robust to vibrato (which swings around it).
List<PitchNote> segmentPitchNotes(
  VoiceMetrics m, {
  int minFrames = 7,
  int maxGap = 3,
}) {
  final out = <PitchNote>[];
  var startFr = -1, endFr = -1, semi = 0, gap = 0;
  final cents = <double>[];

  void flush() {
    if (startFr >= 0 &&
        (endFr - startFr + 1) >= minFrames &&
        cents.isNotEmpty) {
      cents.sort();
      out.add(
        PitchNote(
          startFrame: startFr,
          endFrame: endFr,
          semitone: semi,
          medianCents: cents[cents.length ~/ 2],
        ),
      );
    }
    startFr = -1;
    cents.clear();
    gap = 0;
  }

  // Sticky band: a frame stays in the current note while within ±(0.5 + 0.1)
  // semitone of it. Without the hysteresis, a note sung right at a semitone
  // boundary (~quarter-tone off) makes round() flip-flop frame-to-frame,
  // fragmenting it into sub-minFrames runs that all get dropped — so the worst
  // notes would vanish and bias the in-tune% upward.
  const band = 0.6;
  for (var fr = 0; fr < m.frames; fr++) {
    final hz = m.f0[fr];
    if (hz.isNaN || hz <= 0) {
      if (startFr >= 0 && ++gap > maxGap) flush();
      continue;
    }
    final midi = _hzToMidi(hz);
    if (startFr < 0) {
      startFr = fr;
      endFr = fr;
      semi = midi.round();
      gap = 0;
      cents
        ..clear()
        ..add((midi - semi) * 100);
    } else if ((midi - semi).abs() < band) {
      endFr = fr;
      gap = 0;
      cents.add((midi - semi) * 100);
    } else {
      flush();
      startFr = fr;
      endFr = fr;
      semi = midi.round();
      gap = 0;
      cents
        ..clear()
        ..add((midi - semi) * 100);
    }
  }
  flush();
  return out;
}

/// Duration-weighted fraction (0–100) of sung notes that are in tune; NaN if
/// there are no notes.
double pitchInTunePercent(List<PitchNote> notes) {
  var total = 0, good = 0;
  for (final n in notes) {
    total += n.frames;
    if (n.inTune) good += n.frames;
  }
  return total == 0 ? double.nan : 100.0 * good / total;
}

/// The (up to 3) most off-pitch notes worth flagging — indices into [notes].
Set<int> _worstNotes(List<PitchNote> notes) {
  final idx = [for (var i = 0; i < notes.length; i++) i]
    ..sort(
      (a, b) =>
          notes[b].medianCents.abs().compareTo(notes[a].medianCents.abs()),
    );
  return idx.where((i) => notes[i].medianCents.abs() > 30).take(3).toSet();
}

/// A Logic/Cubase-style sub-lane folded out under a vocal take: the take's pitch
/// shown as note blocks (quantized to semitones, tinted green→red by how in tune)
/// with the fine F0 contour inside, plus an in-tune% score and a playhead
/// readout. Read-only — a reference, not pitch correction. Shares the timeline +
/// header-column width with the parent track.
class PitchLane extends StatelessWidget {
  const PitchLane({
    required this.track,
    required this.timelineDurationMs,
    required this.position,
    required this.headerWidth,
    required this.loading,
    required this.metrics,
    this.viewStartMs = 0,
    this.height = kPitchLaneHeight,
    this.takeLocal = false,
    super.key,
  });

  final AudioTrack track;
  final int timelineDurationMs;

  /// Absolute time (ms) at the left edge of the viewport (0 in take-local mode).
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final double headerWidth;
  final bool loading;
  final VoiceMetrics? metrics;

  /// Lane height; defaults to the inline-lane height but the Pitch dock tab
  /// passes the full available height so it fills the dock.
  final double height;

  /// When true (the Pitch dock tab), the take fills the panel in take-local time
  /// [0, durationMs] instead of being placed at its startMs on a shared
  /// timeline — so [timelineDurationMs] should be the take's own duration.
  final bool takeLocal;

  @override
  Widget build(BuildContext context) {
    final m = metrics;
    final notes = (m != null && !loading)
        ? segmentPitchNotes(m)
        : const <PitchNote>[];
    final inTune = pitchInTunePercent(notes);
    final worst = _worstNotes(notes);
    final range = m != null ? pitchSemitoneRange(m) : (lo: 48, hi: 72);

    return Container(
      height: height,
      decoration: const BoxDecoration(
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          _header(notes, inTune),
          Expanded(child: _body(m, notes, worst, range)),
        ],
      ),
    );
  }

  Widget _header(List<PitchNote> notes, double inTune) {
    return Container(
      width: headerWidth,
      decoration: const BoxDecoration(
        color: Studio.panel,
        border: Border(right: BorderSide(color: Studio.border)),
      ),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(30, 8, 8, 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              'PITCH',
              style: TextStyle(
                color: Studio.textFaint,
                fontSize: 10,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.8,
              ),
            ),
            if (metrics != null && !loading) ...[
              const SizedBox(height: 7),
              Text.rich(
                TextSpan(
                  children: [
                    TextSpan(
                      text: inTune.isNaN ? '—' : '${inTune.round()}%',
                      style: const TextStyle(
                        color: Studio.textPrimary,
                        fontSize: 14,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                    const TextSpan(
                      text: '  in tune',
                      style: TextStyle(color: Studio.textFaint, fontSize: 9),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 3),
              // Live readout of the note under the playhead.
              _PlayheadNoteReadout(
                position: position,
                metrics: metrics!,
                notes: notes,
                trackStartMs: track.startMs,
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _body(
    VoiceMetrics? m,
    List<PitchNote> notes,
    Set<int> worst,
    ({int lo, int hi}) range,
  ) {
    if (loading) return const LaneHint('Analyzing pitch…');
    if (m == null) {
      return const LaneHint('Pitch is available for recorded WAV takes.');
    }
    return ColoredBox(
      color: Studio.inset,
      child: Stack(
        children: [
          // Its own layer: the header's note readout re-lays-out a few times
          // a second during playback, and must not repaint the notes with it.
          Positioned.fill(
            child: IgnorePointer(
              child: RepaintBoundary(
                child: CustomPaint(
                  painter: PitchLanePainter(
                    metrics: m,
                    notes: notes,
                    worst: worst,
                    timelineDurationMs: timelineDurationMs,
                    startMs: takeLocal ? 0 : track.startMs,
                    viewStartMs: viewStartMs,
                    loSemi: range.lo,
                    hiSemi: range.hi,
                  ),
                ),
              ),
            ),
          ),
          Positioned.fill(
            child: PlayheadLayer(
              position: position,
              timelineDurationMs: timelineDurationMs,
              color: Studio.playhead,
              startMs: takeLocal ? track.startMs : viewStartMs,
            ),
          ),
        ],
      ),
    );
  }
}

PitchNote? _noteAtFrame(List<PitchNote> notes, int fr) {
  for (final n in notes) {
    if (fr >= n.startFrame && fr <= n.endFrame) return n;
  }
  return null;
}

/// The pitch header's readout of the note under the playhead. It follows the
/// position notifier itself and rebuilds only when the text changes — a few
/// times a second, not on every tick. A per-tick rebuild here sits under the
/// timeline's LayoutBuilders, and re-laid-out (so repainted) the whole screen
/// on every frame of playback.
class _PlayheadNoteReadout extends StatefulWidget {
  const _PlayheadNoteReadout({
    required this.position,
    required this.metrics,
    required this.notes,
    required this.trackStartMs,
  });

  final ValueListenable<Duration> position;
  final VoiceMetrics metrics;
  final List<PitchNote> notes;
  final int trackStartMs;

  @override
  State<_PlayheadNoteReadout> createState() => _PlayheadNoteReadoutState();
}

class _PlayheadNoteReadoutState extends State<_PlayheadNoteReadout> {
  late String _text = _textAt(widget.position.value);

  String _textAt(Duration pos) {
    final m = widget.metrics;
    // Only read a note when the playhead is actually over this take
    // (frameAtTime would otherwise clamp to a stale edge).
    final t = (pos.inMilliseconds - widget.trackStartMs) / 1000.0;
    final n = (m.frames > 0 && t >= 0 && t <= m.timeOfFrame(m.frames - 1))
        ? _noteAtFrame(widget.notes, m.frameAtTime(t))
        : null;
    return n == null
        ? '—'
        : '${_semiName(n.semitone)} ${_centsStr(n.medianCents)}';
  }

  void _onTick() {
    final text = _textAt(widget.position.value);
    if (text != _text) setState(() => _text = text);
  }

  @override
  void initState() {
    super.initState();
    widget.position.addListener(_onTick);
  }

  @override
  void didUpdateWidget(_PlayheadNoteReadout old) {
    super.didUpdateWidget(old);
    if (!identical(old.position, widget.position)) {
      old.position.removeListener(_onTick);
      widget.position.addListener(_onTick);
    }
    // New metrics / notes / placement re-time the readout.
    _text = _textAt(widget.position.value);
  }

  @override
  void dispose() {
    widget.position.removeListener(_onTick);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Text(
      _text,
      style: const TextStyle(
        color: Studio.textSecondary,
        fontSize: 11,
        fontWeight: FontWeight.w600,
      ),
    );
  }
}

/// Draws the semitone grid, the detected note blocks (tinted by how in-tune,
/// the worst few outlined + labeled), and the fine F0 contour inside each note.
class PitchLanePainter extends CustomPainter {
  PitchLanePainter({
    required this.metrics,
    required this.notes,
    required this.worst,
    required this.timelineDurationMs,
    required this.startMs,
    required this.loSemi,
    required this.hiSemi,
    this.viewStartMs = 0,
  });

  final VoiceMetrics metrics;
  final List<PitchNote> notes;
  final Set<int> worst;
  final int timelineDurationMs;
  final int startMs;

  /// Absolute time (ms) at the left edge of the viewport.
  final int viewStartMs;
  final int loSemi;
  final int hiSemi;

  @override
  void paint(Canvas canvas, Size size) {
    final dur = timelineDurationMs;
    if (dur <= 0 || hiSemi <= loSemi) return;
    final span = (hiSemi - loSemi).toDouble();
    double yOf(double semi) => (1 - (semi - loSemi) / span) * size.height;
    // Map absolute timeline ms into the viewport; off-screen parts are clipped
    // by the lane, so no clamp (which would pin them to the edge).
    double xOf(double ms) => (ms - viewStartMs) / dur * size.width;
    double xOfFrame(int fr) => xOf(startMs + metrics.timeOfFrame(fr) * 1000);

    // Semitone grid (octave C lines bolder + labeled).
    final grid = Paint()..strokeWidth = 1;
    for (var s = loSemi; s <= hiSemi; s++) {
      final isC = s % 12 == 0;
      grid.color = isC ? Studio.gridStrong : Studio.grid.withValues(alpha: 0.5);
      final y = yOf(s.toDouble());
      canvas.drawLine(Offset(0, y), Offset(size.width, y), grid);
      if (isC) _label(canvas, 'C${(s ~/ 12) - 1}', 3, y, Studio.textFaint);
    }

    final semiH = size.height / span;
    final blockH = semiH * 0.62;

    // Note blocks.
    for (var i = 0; i < notes.length; i++) {
      final n = notes[i];
      final x0 = xOfFrame(n.startFrame);
      final x1 = math.max(xOfFrame(n.endFrame), x0 + 2);
      final yc = yOf(n.semitone.toDouble());
      final rrect = RRect.fromRectAndRadius(
        Rect.fromLTRB(x0, yc - blockH / 2, x1, yc + blockH / 2),
        const Radius.circular(3),
      );
      final col = _centsColor(n.medianCents.abs());
      canvas.drawRRect(rrect, Paint()..color = col.withValues(alpha: 0.26));
      final isWorst = worst.contains(i);
      canvas.drawRRect(
        rrect,
        Paint()
          ..color = isWorst ? Studio.record : col.withValues(alpha: 0.7)
          ..style = PaintingStyle.stroke
          ..strokeWidth = isWorst ? 1.6 : 1,
      );
      if (isWorst) {
        var ly = yc - blockH / 2 - 11;
        if (ly < 1) ly = yc + blockH / 2 + 2;
        _label(canvas, _centsStr(n.medianCents), x0 + 2, ly + 5, Studio.record);
      }
    }

    // The fine F0 contour, drawn only inside notes (so octave glitches and
    // unvoiced gaps between notes don't clutter the view).
    final cline = Paint()
      ..color = Studio.textPrimary.withValues(alpha: 0.85)
      ..strokeWidth = 1.3
      ..strokeCap = StrokeCap.round;
    for (final n in notes) {
      double? px, py;
      for (var fr = n.startFrame; fr <= n.endFrame; fr++) {
        final hz = metrics.f0[fr];
        if (hz.isNaN || hz <= 0) {
          px = null;
          continue;
        }
        final x = xOfFrame(fr);
        final y = yOf(_hzToMidi(hz));
        if (px != null) canvas.drawLine(Offset(px, py!), Offset(x, y), cline);
        px = x;
        py = y;
      }
    }
  }

  Color _centsColor(double absCents) =>
      Color.lerp(_inTune, Studio.record, (absCents / 50).clamp(0.0, 1.0))!;

  void _label(Canvas canvas, String text, double x, double y, Color color) {
    final tp = TextPainter(
      text: TextSpan(
        text: text,
        style: TextStyle(color: color, fontSize: 9),
      ),
      textDirection: TextDirection.ltr,
    )..layout();
    tp.paint(canvas, Offset(x, y - tp.height / 2));
  }

  @override
  bool shouldRepaint(PitchLanePainter old) =>
      // notes/worst are derived from metrics, so metrics identity covers them —
      // avoids repainting on every unrelated viewmodel notify (volume/move/zoom).
      old.metrics != metrics ||
      old.timelineDurationMs != timelineDurationMs ||
      old.startMs != startMs ||
      old.viewStartMs != viewStartMs ||
      old.loSemi != loSemi ||
      old.hiSemi != hiSemi;
}
