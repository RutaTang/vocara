import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/song_practice.dart';
import '../../../utils/format.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_controls.dart';
import '../view_model/song_practice_detail_view_model.dart';

/// The top toolbar of the detail view: practice identity on the left, the
/// always-visible transport (play/stop, clock, record) in the middle, and the
/// export action on the right.
class DetailTransportBar extends StatelessWidget {
  const DetailTransportBar({
    required this.vm,
    required this.practice,
    required this.onBack,
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final SongPractice practice;
  final VoidCallback onBack;

  @override
  Widget build(BuildContext context) {
    final takes = practice.activeVocalTracks.length;
    final notes = practice.activeNoteCount;
    // The reference (Original) is playable too — when the backing toggle is on
    // "Original" it's the active backing — so it must count toward playability.
    final hasTracks =
        practice.hasAccompaniment ||
        practice.hasReference ||
        practice.activeVocalTracks.isNotEmpty;
    final canPlay = hasTracks && !vm.isRecording;

    return Container(
      height: 54,
      padding: const EdgeInsets.symmetric(horizontal: 14),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Stack(
        children: [
          // Left: back + title + counts.
          Align(
            alignment: Alignment.centerLeft,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                _BackButton(onTap: onBack),
                const SizedBox(width: 6),
                Text(
                  practice.title,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 15,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(width: 10),
                Text(
                  '${takes == 1 ? '1 take' : '$takes takes'} · $notes notes',
                  style: const TextStyle(color: Studio.textFaint, fontSize: 12),
                ),
              ],
            ),
          ),
          // Right: timeline zoom — a horizontal slider flanked by zoom icons,
          // like Logic's zoom control.
          Align(
            alignment: Alignment.centerRight,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                _ZoomIcon(
                  icon: FIcons.zoomOut,
                  onTap: () => vm.zoomTimeline(1.25),
                ),
                const SizedBox(width: 8),
                _ZoomSlider(
                  value: vm.timelineZoom,
                  onChanged: vm.setTimelineZoomLive,
                  onChangeEnd: vm.commitTimelineZoom,
                ),
                const SizedBox(width: 8),
                _ZoomIcon(
                  icon: FIcons.zoomIn,
                  onTap: () => vm.zoomTimeline(0.8),
                ),
              ],
            ),
          ),
          // Center: the transport, independent of the title width.
          Align(
            alignment: Alignment.center,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                StudioRoundButton(
                  icon: vm.isPlaying ? FIcons.pause : FIcons.play,
                  filled: true,
                  size: 34,
                  onTap: canPlay ? vm.togglePlay : null,
                ),
                const SizedBox(width: 8),
                StudioRoundButton(
                  icon: FIcons.square,
                  color: Studio.textSecondary,
                  size: 34,
                  onTap: hasTracks ? vm.stop : null,
                ),
                const SizedBox(width: 8),
                StudioRoundButton(
                  icon: FIcons.repeat,
                  color: vm.loopEnabled ? Studio.accent : Studio.textSecondary,
                  size: 34,
                  onTap: vm.hasLoop ? vm.toggleLoop : null,
                ),
                const SizedBox(width: 8),
                StudioRoundButton(
                  icon: FIcons.locateFixed,
                  color: vm.followCursor ? Studio.accent : Studio.textSecondary,
                  size: 34,
                  onTap: vm.toggleFollowCursor,
                ),
                const SizedBox(width: 16),
                _Lcd(
                  position: vm.position,
                  totalDurationMs: practice.timelineDurationMs,
                ),
                const SizedBox(width: 12),
                // Input monitoring: arm the mic (meter + level live in the mixer's MIC strip).
                StudioRoundButton(
                  icon: FIcons.headphones,
                  color: vm.inputMonitorEnabled
                      ? Studio.accent
                      : Studio.textSecondary,
                  size: 34,
                  onTap: vm.toggleInputMonitor,
                ),
                const SizedBox(width: 12),
                _RecPill(
                  isRecording: vm.isRecording,
                  onTap: () =>
                      vm.isRecording ? vm.stopRecording() : vm.startRecording(),
                ),
                if (vm.busy) ...[
                  const SizedBox(width: 14),
                  const SizedBox(
                    width: 13,
                    height: 13,
                    child: CircularProgressIndicator(
                      strokeWidth: 2,
                      valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
                    ),
                  ),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _BackButton extends StatelessWidget {
  const _BackButton({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 30,
          height: 30,
          alignment: Alignment.center,
          child: const Icon(
            FIcons.chevronLeft,
            size: 20,
            color: Studio.textSecondary,
          ),
        ),
      ),
    );
  }
}

class _Lcd extends StatelessWidget {
  const _Lcd({required this.position, required this.totalDurationMs});

  final ValueListenable<Duration> position;
  final int totalDurationMs;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(6),
        border: Border.all(color: Studio.border),
      ),
      child: ValueListenableBuilder<Duration>(
        valueListenable: position,
        builder: (context, pos, _) => Text(
          '${formatClock(pos)} / ${formatClockMs(totalDurationMs)}',
          style: const TextStyle(
            color: Studio.lcd,
            fontSize: 14,
            fontWeight: FontWeight.w600,
            letterSpacing: 1,
            fontFeatures: [FontFeature.tabularFigures()],
          ),
        ),
      ),
    );
  }
}

class _RecPill extends StatelessWidget {
  const _RecPill({required this.isRecording, required this.onTap});

  final bool isRecording;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          height: 32,
          padding: const EdgeInsets.symmetric(horizontal: 12),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(16),
            color: isRecording
                ? Studio.record.withValues(alpha: 0.16)
                : Studio.surfaceRaised,
            border: Border.all(
              color: isRecording ? Studio.record : Studio.borderStrong,
            ),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 9,
                height: 9,
                decoration: BoxDecoration(
                  color: Studio.record,
                  shape: isRecording ? BoxShape.rectangle : BoxShape.circle,
                  borderRadius: isRecording ? BorderRadius.circular(2) : null,
                ),
              ),
              const SizedBox(width: 7),
              const Text(
                'REC',
                style: TextStyle(
                  color: Studio.record,
                  fontSize: 12,
                  fontWeight: FontWeight.w700,
                  letterSpacing: 0.5,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A small zoom glyph at either end of the zoom slider; tapping nudges the zoom.
class _ZoomIcon extends StatelessWidget {
  const _ZoomIcon({required this.icon, required this.onTap});

  final IconData icon;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(2),
          child: Icon(icon, size: 15, color: Studio.textSecondary),
        ),
      ),
    );
  }
}

/// A horizontal timeline-zoom slider: dragging the knob left zooms out (longer
/// timeline), right zooms in (shorter). Continuous, like Logic's zoom slider.
class _ZoomSlider extends StatelessWidget {
  const _ZoomSlider({
    required this.value,
    required this.onChanged,
    required this.onChangeEnd,
  });

  final double value;
  final ValueChanged<double> onChanged;
  final VoidCallback onChangeEnd;

  static const double _width = 96;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.resizeLeftRight,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (d) =>
            onChanged((d.localPosition.dx / _width).clamp(0.0, 1.0)),
        onTapUp: (_) => onChangeEnd(),
        onHorizontalDragUpdate: (d) =>
            onChanged((d.localPosition.dx / _width).clamp(0.0, 1.0)),
        onHorizontalDragEnd: (_) => onChangeEnd(),
        child: CustomPaint(
          size: const Size(_width, 22),
          painter: _ZoomSliderPainter(value),
        ),
      ),
    );
  }
}

class _ZoomSliderPainter extends CustomPainter {
  _ZoomSliderPainter(this.value);

  final double value;

  @override
  void paint(Canvas canvas, Size size) {
    final cy = size.height / 2;
    const left = 3.0;
    final right = size.width - 3;
    final track = Paint()
      ..color = Studio.borderStrong
      ..strokeWidth = 3
      ..strokeCap = StrokeCap.round;
    canvas.drawLine(Offset(left, cy), Offset(right, cy), track);
    final tx = left + value.clamp(0.0, 1.0) * (right - left);
    final filled = Paint()
      ..color = Studio.textSecondary
      ..strokeWidth = 3
      ..strokeCap = StrokeCap.round;
    canvas.drawLine(Offset(left, cy), Offset(tx, cy), filled);
    canvas.drawCircle(Offset(tx, cy), 5, Paint()..color = Studio.textPrimary);
  }

  @override
  bool shouldRepaint(covariant _ZoomSliderPainter old) => old.value != value;
}
