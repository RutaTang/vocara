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
    // The mixer-side count, not SongPractice.activeNoteCount: only ONE backing
    // is on screen at a time (the Accompaniment ⇄ Original toggle), and the
    // model can't know which — counting both could show more notes than the
    // user can find. The toggle notifies the mixer, which notifies the view
    // model, so this rebuilds when the backing flips.
    final notes = vm.mixer.visibleNoteCount;
    // The reference (Original) is playable too — when the backing toggle is on
    // "Original" it's the active backing — so it must count toward playability.
    final hasTracks =
        practice.hasAccompaniment ||
        practice.hasReference ||
        practice.activeVocalTracks.isNotEmpty;
    final canPlay = hasTracks && !vm.isRecording;
    // A take in REC's arm window is a take too: Stop and the pill cancel it.
    final taking = vm.isRecording || vm.recording.isStartingTake;

    // Its own layer: a relayout elsewhere on the screen (the dock's per-tick
    // readouts climb to the Scaffold) repaints the screen's shell, not the
    // bar's controls.
    return RepaintBoundary(
      child: Container(
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
                    style: const TextStyle(
                      color: Studio.textFaint,
                      fontSize: 12,
                    ),
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
                    onTap: () => vm.playback.zoomTimeline(1.25),
                  ),
                  const SizedBox(width: 8),
                  // View state notifies the viewport only (see
                  // PlaybackController.viewport), as do the loop and follow
                  // buttons' below.
                  ListenableBuilder(
                    listenable: vm.playback.viewport,
                    builder: (context, _) => _ZoomSlider(
                      value: vm.playback.timelineZoom,
                      onChanged: vm.playback.setTimelineZoomLive,
                      onChangeEnd: vm.playback.commitTimelineZoom,
                    ),
                  ),
                  const SizedBox(width: 8),
                  _ZoomIcon(
                    icon: FIcons.zoomIn,
                    onTap: () => vm.playback.zoomTimeline(0.8),
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
                    icon: vm.playback.isPlaying ? FIcons.pause : FIcons.play,
                    filled: true,
                    size: 34,
                    onTap: canPlay ? vm.playback.togglePlay : null,
                  ),
                  const SizedBox(width: 8),
                  StudioRoundButton(
                    icon: FIcons.square,
                    color: Studio.textSecondary,
                    size: 34,
                    // Mid-take, Stop ends the take (what Stop means in any DAW);
                    // the transport stop itself is a no-op while recording.
                    onTap: taking
                        ? vm.stopRecording
                        : (hasTracks ? vm.playback.stop : null),
                  ),
                  const SizedBox(width: 8),
                  ListenableBuilder(
                    listenable: vm.playback.viewport,
                    builder: (context, _) => StudioRoundButton(
                      icon: FIcons.repeat,
                      color: vm.playback.loopEnabled
                          ? Studio.accent
                          : Studio.textSecondary,
                      size: 34,
                      onTap: vm.playback.hasLoop
                          ? vm.playback.toggleLoop
                          : null,
                    ),
                  ),
                  const SizedBox(width: 8),
                  ListenableBuilder(
                    listenable: vm.playback.viewport,
                    builder: (context, _) => StudioRoundButton(
                      icon: FIcons.locateFixed,
                      color: vm.playback.followCursor
                          ? Studio.accent
                          : Studio.textSecondary,
                      size: 34,
                      onTap: vm.playback.toggleFollowCursor,
                    ),
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
                    color: vm.recording.inputMonitorEnabled
                        ? Studio.accent
                        : Studio.textSecondary,
                    size: 34,
                    onTap: vm.recording.toggleInputMonitor,
                  ),
                  const SizedBox(width: 12),
                  _RecPill(
                    isRecording: taking,
                    onTap: () => taking
                        ? vm.stopRecording()
                        : vm.recording.startRecording(),
                  ),
                  if (vm.busy) ...[
                    const SizedBox(width: 14),
                    const SizedBox(
                      width: 13,
                      height: 13,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        valueColor: AlwaysStoppedAnimation(
                          Studio.textSecondary,
                        ),
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ],
        ),
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
      // The readout changes on every position tick; its own layer keeps that
      // repaint from spreading to the rest of the transport bar and beyond.
      child: RepaintBoundary(
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
