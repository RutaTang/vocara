import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../../../utils/format.dart';

/// Picks a readable time-grid interval (in ms) so labels/lines land roughly
/// every [minPx] pixels.
int niceTimeIntervalMs(int durationMs, double width, {double minPx = 72}) {
  if (durationMs <= 0 || width <= 0) return 1000;
  final minMs = minPx / (width / durationMs);
  const candidates = [
    1000,
    2000,
    5000,
    10000,
    15000,
    30000,
    60000,
    120000,
    300000,
    600000,
  ];
  for (final c in candidates) {
    if (c >= minMs) return c;
  }
  return candidates.last;
}

/// Paints a track's normalized peaks as a filled, mirrored audio-clip
/// silhouette with a faint center axis. A track shorter than the shared
/// timeline only fills [widthFraction] of the width so lanes align on time.
class WaveformPainter extends CustomPainter {
  WaveformPainter({
    required this.peaks,
    required this.color,
    this.widthFraction = 1.0,
    this.indexOffset = 0.0,
    this.indexSpan,
  });

  final List<double> peaks;
  final Color color;
  final double widthFraction;

  /// Viewport mapping for a whole-peak slice, in PEAK-INDEX units: the index
  /// position at the draw area's left edge, and the index distance its width
  /// covers ([indexSpan]; null = `peaks.length - 1`, the classic
  /// spread-all-vertices mapping). A slice cut at whole-peak bounds starts up
  /// to one peak interval EARLIER than the viewport edge; anchoring vertex k
  /// at `(k − indexOffset) / indexSpan` keeps every peak at its true timeline
  /// x — aligned with ruler/grid/playhead — instead of shifting the waveform
  /// by the discarded fraction. Vertices landing outside the box are clipped.
  final double indexOffset;
  final double? indexSpan;

  @override
  void paint(Canvas canvas, Size size) {
    if (peaks.isEmpty) return;
    final fraction = widthFraction <= 0
        ? 0.0
        : (widthFraction > 1 ? 1.0 : widthFraction);
    final drawWidth = size.width * fraction;
    if (drawWidth <= 0) return;

    final mid = size.height / 2;
    final n = peaks.length;
    final span = indexSpan ?? (n - 1).toDouble();
    // A slice's edge vertices map outside the box by design (see
    // [indexOffset]); keep them from bleeding into the neighboring padding.
    if (indexOffset != 0 || indexSpan != null) {
      canvas.clipRect(Rect.fromLTWH(0, 0, drawWidth, size.height));
    }
    // One vertex per peak, but never more than per pixel — so a clip mapped far
    // wider than the viewport (when zoomed in) doesn't build a giant path.
    final steps = n.clamp(1, drawWidth.ceil() + 1);
    double idxAt(int i) => steps <= 1 ? 0.0 : i / (steps - 1) * (n - 1);
    double xAt(int i) =>
        span <= 0 ? 0.0 : (idxAt(i) - indexOffset) / span * drawWidth;
    double ampAt(int i) => peaks[idxAt(i).round().clamp(0, n - 1)];

    final path = Path()..moveTo(0, mid);
    for (var i = 0; i < steps; i++) {
      path.lineTo(xAt(i), mid - ampAt(i) * mid * 0.9);
    }
    for (var i = steps - 1; i >= 0; i--) {
      path.lineTo(xAt(i), mid + ampAt(i) * mid * 0.9);
    }
    path.close();

    canvas.drawPath(path, Paint()..color = color.withValues(alpha: 0.92));
    canvas.drawLine(
      Offset(0, mid),
      Offset(drawWidth, mid),
      Paint()
        ..color = color.withValues(alpha: 0.35)
        ..strokeWidth = 1,
    );
  }

  @override
  bool shouldRepaint(covariant WaveformPainter old) =>
      old.peaks != peaks ||
      old.color != color ||
      old.widthFraction != widthFraction ||
      old.indexOffset != indexOffset ||
      old.indexSpan != indexSpan;
}

/// Paints a recording-in-progress waveform that grows rightward at a FIXED
/// pixels-per-sample, so audio already drawn never shifts or rescales as more is
/// captured — the wobble-free way Logic / Pro Tools / Cubase draw a live take.
///
/// Unlike [WaveformPainter], which re-spreads every peak across the clip's
/// *current* width (so growing the clip slides all the earlier samples), this
/// bins samples into fixed pixel columns anchored at the left edge: a given
/// sample always lands in the same column. Only the newest column at the right
/// changes each frame; everything to its left is frozen.
///
/// The mapping uses an ABSOLUTE [pixelsPerSample] (the timeline's fixed
/// pixels-per-ms × the record interval), not `n / width`, so column c covers the
/// exact same sample indices no matter how many more samples arrive — the
/// already-drawn waveform is pixel-identical frame to frame.
class LiveWaveformPainter extends CustomPainter {
  LiveWaveformPainter({
    required this.peaks,
    required this.color,
    required this.pixelsPerSample,
    this.sampleOffset = 0.0,
  });

  final List<double> peaks;
  final Color color;

  /// Width in device pixels one captured sample occupies — fixed for the whole
  /// take, so the waveform grows rightward without the rest shifting.
  final double pixelsPerSample;

  /// Sample-index position at the drawn box's left edge, in samples of
  /// [peaks] — the fraction a whole-sample viewport slice discarded when the
  /// window edge lands mid-sample. Binning columns from `c·spc + offset`
  /// keeps the absolute mapping glued to the TIMELINE (not the box), so the
  /// drawn take doesn't freeze on screen and snap while the view pans.
  final double sampleOffset;

  @override
  void paint(Canvas canvas, Size size) {
    final n = peaks.length;
    if (n == 0 || size.width <= 0 || size.height <= 0) return;
    final mid = size.height / 2;
    final cols = size.width.floor().clamp(1, 1 << 20);
    // Samples per 1-pixel column, from the absolute scale (not n/width), so the
    // bin for a given column is frozen as the take grows.
    final spc = pixelsPerSample <= 0 ? 1.0 : 1.0 / pixelsPerSample;

    double ampAt(int c) {
      var lo = (c * spc + sampleOffset).floor();
      var hi = ((c + 1) * spc + sampleOffset).ceil();
      if (lo >= n) lo = n - 1;
      if (lo < 0) lo = 0;
      if (hi > n) hi = n;
      if (hi <= lo) hi = lo + 1;
      var m = 0.0;
      for (var i = lo; i < hi; i++) {
        final a = peaks[i];
        if (a > m) m = a;
      }
      return m;
    }

    final path = Path()..moveTo(0, mid);
    for (var c = 0; c <= cols; c++) {
      path.lineTo(c.toDouble(), mid - ampAt(c) * mid * 0.9);
    }
    for (var c = cols; c >= 0; c--) {
      path.lineTo(c.toDouble(), mid + ampAt(c) * mid * 0.9);
    }
    path.close();

    canvas.drawPath(path, Paint()..color = color.withValues(alpha: 0.92));
    canvas.drawLine(
      Offset(0, mid),
      Offset(size.width, mid),
      Paint()
        ..color = color.withValues(alpha: 0.35)
        ..strokeWidth = 1,
    );
  }

  @override
  bool shouldRepaint(covariant LiveWaveformPainter old) =>
      old.peaks != peaks ||
      old.color != color ||
      old.pixelsPerSample != pixelsPerSample ||
      old.sampleOffset != sampleOffset;
}

/// Vertical time-grid lines drawn behind a lane's waveform.
class TimelineGridPainter extends CustomPainter {
  TimelineGridPainter({
    required this.timelineDurationMs,
    required this.color,
    this.startMs = 0,
  });

  /// The visible span (ms) mapped across the width.
  final int timelineDurationMs;
  final Color color;

  /// The absolute time (ms) at the left edge of the view.
  final int startMs;

  @override
  void paint(Canvas canvas, Size size) {
    if (timelineDurationMs <= 0) return;
    final interval = niceTimeIntervalMs(timelineDurationMs, size.width);
    final paint = Paint()
      ..color = color
      ..strokeWidth = 1;
    final end = startMs + timelineDurationMs;
    for (var t = (startMs ~/ interval + 1) * interval; t < end; t += interval) {
      final x = ((t - startMs) / timelineDurationMs) * size.width;
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), paint);
    }
  }

  @override
  bool shouldRepaint(covariant TimelineGridPainter old) =>
      old.timelineDurationMs != timelineDurationMs ||
      old.color != color ||
      old.startMs != startMs;
}

/// The time ruler shown above the tracks: ticks plus `m:ss` labels.
class TimeRulerPainter extends CustomPainter {
  TimeRulerPainter({
    required this.timelineDurationMs,
    required this.tickColor,
    required this.textColor,
    this.startMs = 0,
  });

  final int timelineDurationMs;
  final Color tickColor;
  final Color textColor;

  /// The absolute time (ms) at the left edge of the view.
  final int startMs;

  @override
  void paint(Canvas canvas, Size size) {
    if (timelineDurationMs <= 0) return;
    final interval = niceTimeIntervalMs(timelineDurationMs, size.width);
    final tickPaint = Paint()
      ..color = tickColor
      ..strokeWidth = 1;

    final end = startMs + timelineDurationMs;
    var t0 = (startMs ~/ interval) * interval;
    if (t0 < startMs) t0 += interval;
    for (var t = t0; t < end; t += interval) {
      final x = ((t - startMs) / timelineDurationMs) * size.width;
      canvas.drawLine(
        Offset(x, size.height - 7),
        Offset(x, size.height),
        tickPaint,
      );
      final tp = TextPainter(
        text: TextSpan(
          text: formatClockMs(t),
          style: TextStyle(
            color: textColor,
            fontSize: 10,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
        textDirection: TextDirection.ltr,
      )..layout();
      tp.paint(canvas, Offset(x + 3, size.height - tp.height - 5));
    }
  }

  @override
  bool shouldRepaint(covariant TimeRulerPainter old) =>
      old.timelineDurationMs != timelineDurationMs ||
      old.tickColor != tickColor ||
      old.textColor != textColor ||
      old.startMs != startMs;
}

/// Paints the playback playhead as a vertical line. Repaints itself whenever
/// [position] ticks (via `super.repaint`), so the tree doesn't rebuild.
class PlayheadPainter extends CustomPainter {
  PlayheadPainter({
    required this.position,
    required this.timelineDurationMs,
    required this.color,
    this.showHead = false,
    this.startMs = 0,
  }) : super(repaint: position);

  final ValueListenable<Duration> position;
  final int timelineDurationMs;
  final Color color;
  final bool showHead;

  /// Offset (ms) subtracted from the absolute [position] before mapping — set to
  /// a take's startMs when a panel shows that take in take-local time [0, dur].
  final int startMs;

  @override
  void paint(Canvas canvas, Size size) {
    if (timelineDurationMs <= 0) return;
    final raw = (position.value.inMilliseconds - startMs) / timelineDurationMs;
    // Outside the visible window → don't draw it pinned to the edge; the
    // playhead simply isn't here (it's off-screen left/right).
    if (raw < 0 || raw > 1) return;
    final x = raw * size.width;
    canvas.drawLine(
      Offset(x, 0),
      Offset(x, size.height),
      Paint()
        ..color = color
        ..strokeWidth = 1.5,
    );
    if (showHead) {
      final path = Path()
        ..moveTo(x - 5, 0)
        ..lineTo(x + 5, 0)
        ..lineTo(x, 6)
        ..close();
      canvas.drawPath(path, Paint()..color = color);
    }
  }

  @override
  bool shouldRepaint(covariant PlayheadPainter old) =>
      old.timelineDurationMs != timelineDurationMs ||
      old.startMs != startMs ||
      old.color != color ||
      old.showHead != showHead;
}
