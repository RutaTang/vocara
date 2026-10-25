import 'dart:math' as math;

import 'package:flutter/gestures.dart' show PointerDeviceKind;
import 'package:flutter/material.dart';

import '../themes/studio.dart';

/// Pointer devices a timeline-area drag (a clip move, a marker range, a loop
/// edge, a lane fader) responds to — every kind except
/// [PointerDeviceKind.trackpad]. A two-finger trackpad swipe is how the
/// timeline pans, and drag recognizers accept a trackpad pan-zoom by default,
/// so the swipe also dragged whatever the cursor rested on: it moved (and
/// saved) a clip, opened the add-note editor, shifted the loop or jumped a
/// take's volume.
const Set<PointerDeviceKind> kPointerDragDevices = {
  PointerDeviceKind.touch,
  PointerDeviceKind.mouse,
  PointerDeviceKind.stylus,
  PointerDeviceKind.invertedStylus,
  PointerDeviceKind.unknown,
};

/// A compact rotary pan knob ([-1, 1]; 0 = center). Drag horizontally to pan,
/// double-tap to re-center; optionally shows an L##/C/R## label below.
class StudioPanKnob extends StatefulWidget {
  const StudioPanKnob({
    required this.value,
    required this.onChanged,
    required this.onCommit,
    this.size = 30,
    this.showLabel = true,
    this.accent = Studio.accent,
    super.key,
  });

  final double value;
  final ValueChanged<double> onChanged;
  final VoidCallback onCommit;
  final double size;
  final bool showLabel;
  final Color accent;

  @override
  State<StudioPanKnob> createState() => _StudioPanKnobState();
}

class _StudioPanKnobState extends State<StudioPanKnob> {
  // The pan a held drag has reached. Each move adds to THIS rather than to
  // widget.value, which only catches up on the next rebuild: two pointer moves
  // inside one frame (a fast mouse) would otherwise both start from the same
  // stale value, and the first would be lost. Seeded from value per drag.
  double? _drag;

  String get _label {
    final p = (widget.value * 100).round();
    if (p == 0) return 'C';
    return p < 0 ? 'L${-p}' : 'R$p';
  }

  @override
  Widget build(BuildContext context) {
    final size = widget.size;
    return MouseRegion(
      cursor: SystemMouseCursors.resizeLeftRight,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onPanStart: (_) => _drag = widget.value,
        onPanUpdate: (d) {
          final next = ((_drag ?? widget.value) + d.delta.dx / 80).clamp(
            -1.0,
            1.0,
          );
          _drag = next;
          widget.onChanged(next);
        },
        onPanEnd: (_) {
          _drag = null;
          widget.onCommit();
        },
        onPanCancel: () => _drag = null,
        onDoubleTap: () {
          widget.onChanged(0);
          widget.onCommit();
        },
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            SizedBox(
              width: size,
              height: size,
              child: CustomPaint(
                painter: _PanKnobPainter(widget.value, widget.accent),
              ),
            ),
            if (widget.showLabel) ...[
              const SizedBox(height: 2),
              SizedBox(
                width: size + 8,
                child: Text(
                  _label,
                  textAlign: TextAlign.center,
                  style: const TextStyle(
                    color: Studio.textSecondary,
                    fontSize: 9,
                    fontWeight: FontWeight.w600,
                    fontFeatures: [FontFeature.tabularFigures()],
                  ),
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _PanKnobPainter extends CustomPainter {
  _PanKnobPainter(this.value, this.accent);
  final double value;
  final Color accent;

  @override
  void paint(Canvas canvas, Size size) {
    final c = Offset(size.width / 2, size.height / 2);
    final r = size.width / 2 - 1;
    canvas.drawCircle(c, r, Paint()..color = Studio.surfaceRaised);
    canvas.drawCircle(
      c,
      r,
      Paint()
        ..color = Studio.borderStrong
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
    // Indicator: map [-1,1] → [-135°, +135°] from straight up.
    final angle = value.clamp(-1.0, 1.0) * (3 * math.pi / 4);
    final dir = Offset(math.sin(angle), -math.cos(angle));
    canvas.drawLine(
      c,
      c + dir * (r - 3),
      Paint()
        ..color = accent
        ..strokeWidth = 2
        ..strokeCap = StrokeCap.round,
    );
  }

  @override
  bool shouldRepaint(_PanKnobPainter old) =>
      old.value != value || old.accent != accent;
}

/// A thin horizontal volume fader (track line + filled portion + thumb).
class StudioFader extends StatelessWidget {
  const StudioFader({
    required this.value,
    required this.accent,
    required this.onChanged,
    required this.onChangeEnd,
    super.key,
  });

  final double value;
  final Color accent;
  final ValueChanged<double> onChanged;
  final VoidCallback onChangeEnd;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        void update(double dx) => onChanged((dx / width).clamp(0.0, 1.0));
        return MouseRegion(
          cursor: SystemMouseCursors.resizeLeftRight,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            supportedDevices: kPointerDragDevices,
            onTapDown: (d) => update(d.localPosition.dx),
            onTapUp: (_) => onChangeEnd(),
            onHorizontalDragUpdate: (d) => update(d.localPosition.dx),
            onHorizontalDragEnd: (_) => onChangeEnd(),
            child: SizedBox(
              height: 16,
              child: CustomPaint(
                painter: _FaderPainter(
                  value: value.clamp(0.0, 1.0),
                  accent: accent,
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

class _FaderPainter extends CustomPainter {
  _FaderPainter({required this.value, required this.accent});

  final double value;
  final Color accent;

  @override
  void paint(Canvas canvas, Size size) {
    final mid = size.height / 2;
    final x = value * size.width;
    canvas.drawLine(
      Offset(0, mid),
      Offset(size.width, mid),
      Paint()
        ..color = Studio.gridStrong
        ..strokeWidth = 3
        ..strokeCap = StrokeCap.round,
    );
    canvas.drawLine(
      Offset(0, mid),
      Offset(x, mid),
      Paint()
        ..color = accent
        ..strokeWidth = 3
        ..strokeCap = StrokeCap.round,
    );
    canvas.drawCircle(Offset(x, mid), 5, Paint()..color = accent);
    canvas.drawCircle(
      Offset(x, mid),
      5,
      Paint()
        ..color = Studio.background
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
  }

  @override
  bool shouldRepaint(covariant _FaderPainter old) =>
      old.value != value || old.accent != accent;
}

/// An iOS-style on/off toggle used for mute/enable in the mixer.
class StudioToggle extends StatelessWidget {
  const StudioToggle({
    required this.value,
    required this.onChanged,
    this.activeColor = Studio.accent,
    super.key,
  });

  final bool value;
  final ValueChanged<bool> onChanged;
  final Color activeColor;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => onChanged(!value),
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          width: 34,
          height: 19,
          padding: const EdgeInsets.all(2),
          alignment: value ? Alignment.centerRight : Alignment.centerLeft,
          decoration: BoxDecoration(
            color: value ? activeColor : Studio.toggleOff,
            borderRadius: BorderRadius.circular(10),
          ),
          child: Container(
            width: 15,
            height: 15,
            decoration: const BoxDecoration(
              color: Colors.white,
              shape: BoxShape.circle,
            ),
          ),
        ),
      ),
    );
  }
}

/// A round transport button (play / stop / record).
class StudioRoundButton extends StatelessWidget {
  const StudioRoundButton({
    required this.icon,
    required this.onTap,
    this.color = Studio.accent,
    this.filled = false,
    this.size = 40,
    super.key,
  });

  final IconData icon;
  final VoidCallback? onTap;
  final Color color;
  final bool filled;
  final double size;

  @override
  Widget build(BuildContext context) {
    final enabled = onTap != null;
    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: size,
          height: size,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: filled
                ? color.withValues(alpha: enabled ? 1 : 0.3)
                : Studio.surfaceRaised,
            border: filled ? null : Border.all(color: Studio.borderStrong),
          ),
          child: Icon(
            icon,
            size: size * 0.42,
            color: filled
                ? Studio.background
                : color.withValues(alpha: enabled ? 1 : 0.35),
          ),
        ),
      ),
    );
  }
}

/// A small bordered icon button used in track headers.
class StudioMiniButton extends StatelessWidget {
  const StudioMiniButton({
    required this.icon,
    required this.onTap,
    this.color = Studio.textSecondary,
    super.key,
  });

  final IconData icon;
  final VoidCallback onTap;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 24,
          height: 22,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(4),
            border: Border.all(color: Studio.border),
          ),
          child: Icon(icon, size: 13, color: color),
        ),
      ),
    );
  }
}
