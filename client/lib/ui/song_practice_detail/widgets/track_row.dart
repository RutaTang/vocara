import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/audio_track.dart';
import '../../../domain/models/practice_note.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_controls.dart';
import '../../core/widgets/waveform_painter.dart';
import 'track_context_menu.dart';

/// Width of the per-track header column (Logic/Cubase style): name, mute/solo,
/// volume and actions live here; the waveform fills the timeline to its right.
const double kTrackHeaderW = 208;
const double kTrackRowHeight = 74;

/// Horizontal inset applied to the timeline column (ruler + lanes + loop band)
/// so the tracks line up with the section labels and drop-hint boxes.
const double kTimelineHMargin = 14;

/// A Logic/Cubase-style track row: a fixed-width left control header (color bar,
/// name, mute/solo, volume fader, add-note and a ⋯ menu) followed by the
/// timeline waveform. The clip can be dragged horizontally to move it in time;
/// a vocal lane can be long-pressed and dragged onto another to reorder.
/// Right-click anywhere opens the context menu.
class TrackRow extends StatelessWidget {
  const TrackRow({
    required this.track,
    required this.label,
    required this.timelineDurationMs,
    this.viewStartMs = 0,
    required this.position,
    required this.selected,
    this.armed = false,
    required this.reorderable,
    required this.onSelect,
    required this.onClipMoveLive,
    required this.onMoveCommit,
    required this.onSelectClip,
    required this.onClipContextMenu,
    required this.onCrossLaneMove,
    required this.onContextMenu,
    required this.onAddNote,
    required this.onEditNote,
    required this.onAddTechnique,
    this.selectedClipId,
    this.recordingPeaks,
    this.recordingStartMs = 0,
    required this.onToggleMute,
    required this.onToggleSolo,
    required this.onVolume,
    required this.onVolumeCommit,
    required this.onDelete,
    this.canExpand = false,
    this.expanded = false,
    this.onToggleExpand,
    this.subLanes,
    super.key,
  });

  final AudioTrack track;
  final String label;

  /// The visible span (ms) across the lane width, and the absolute time at the
  /// left edge — together they form the timeline viewport.
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final bool selected;

  /// Whether this take is the record target (REC punches into it) — shown as a
  /// small red arm dot in the lane header.
  final bool armed;

  /// Whether this lane can be long-press dragged onto another to reorder.
  final bool reorderable;

  final VoidCallback onSelect;

  /// Called on every drag tick with a clip's id and its new absolute start (ms).
  final void Function(String clipId, int startMs) onClipMoveLive;

  /// Called when the time-move drag ends, to persist the new position.
  final VoidCallback onMoveCommit;

  /// Called when a clip is tapped, to select it.
  final ValueChanged<String> onSelectClip;

  /// Called on right-click of a clip, with its id and the global pointer.
  final void Function(String clipId, Offset position) onClipContextMenu;

  /// The selected clip on this lane (highlighted), if any.
  final String? selectedClipId;

  /// While this lane is the armed recording target, its live waveform peaks
  /// (one per 50 ms) — drawn as a forming clip. Null otherwise.
  final ValueListenable<List<double>>? recordingPeaks;
  final int recordingStartMs;

  /// Called when another lane (its track id) is dropped onto this one.
  final ValueChanged<String> onCrossLaneMove;

  /// Called on right-click with the global pointer position.
  final ValueChanged<Offset> onContextMenu;

  final VoidCallback onAddNote;
  final ValueChanged<PracticeNote> onEditNote;
  final VoidCallback onAddTechnique;
  final VoidCallback onToggleMute;
  final VoidCallback onToggleSolo;

  /// Called on every volume-fader tick with the new 0..1 value.
  final ValueChanged<double> onVolume;

  /// Called when the volume drag ends, to persist it.
  final VoidCallback onVolumeCommit;

  final VoidCallback onDelete;

  /// Inline analysis sub-lanes (Logic-style fold-out). [subLanes] is the built
  /// toolbar + lanes rendered below the row when [expanded].
  final bool canExpand;
  final bool expanded;
  final VoidCallback? onToggleExpand;
  final Widget? subLanes;

  @override
  Widget build(BuildContext context) {
    if (!reorderable) return _content(context, dropping: false);
    return DragTarget<String>(
      onWillAcceptWithDetails: (d) => d.data != track.id,
      onAcceptWithDetails: (d) => onCrossLaneMove(d.data),
      builder: (context, candidate, rejected) =>
          _content(context, dropping: candidate.isNotEmpty),
    );
  }

  /// The main row (the drag handle when reorderable) plus any expanded
  /// sub-lanes. The whole column is the drop target — so an expanded take's tall
  /// body still accepts a dropped lane — while only the main row drags/highlights.
  Widget _content(BuildContext context, {required bool dropping}) {
    Widget mainRow = _row(context, dropping: dropping);
    if (reorderable) {
      mainRow = LongPressDraggable<String>(
        data: track.id,
        // The feedback is a compact label, not a copy of the wide row — anchor
        // it to the pointer so it doesn't jump away from the cursor.
        dragAnchorStrategy: pointerDragAnchorStrategy,
        feedback: _DragGhost(label: label),
        childWhenDragging: Opacity(opacity: 0.35, child: mainRow),
        child: mainRow,
      );
    }
    if (!expanded || subLanes == null) return mainRow;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [mainRow, subLanes!],
    );
  }

  Widget _row(BuildContext context, {required bool dropping}) {
    // Each lane has a base color: a custom tint if set; otherwise the backing
    // shares the master's signature accent, and a vocal take gets a stable
    // per-take palette color (so takes read as distinct, not a flat gray). The
    // color never changes with selection (Logic style) — selection is shown by
    // the header background and the selected clip's outer border only.
    final Color accent;
    if (track.colorValue != null) {
      accent = Color(track.colorValue!);
    } else if (track.isBacking) {
      accent = Studio.accent;
    } else {
      accent = defaultTakeColor(track.id);
    }
    return Container(
      height: kTrackRowHeight,
      decoration: BoxDecoration(
        color: selected ? Studio.hover.withValues(alpha: 0.55) : null,
        border: Border(
          bottom: const BorderSide(color: Studio.border),
          top: dropping
              ? const BorderSide(color: Studio.accentBright, width: 2)
              : BorderSide.none,
        ),
      ),
      child: Row(
        children: [
          _TrackHeader(
            track: track,
            label: label,
            selected: selected,
            armed: armed,
            accent: accent,
            canExpand: canExpand,
            expanded: expanded,
            onToggleExpand: onToggleExpand,
            onSelect: onSelect,
            onContextMenu: onContextMenu,
            onToggleMute: onToggleMute,
            onToggleSolo: onToggleSolo,
            onVolume: onVolume,
            onVolumeCommit: onVolumeCommit,
            onAddNote: onAddNote,
            onAddTechnique: onAddTechnique,
          ),
          Expanded(
            child: _Body(
              track: track,
              timelineDurationMs: timelineDurationMs,
              viewStartMs: viewStartMs,
              position: position,
              accent: accent,
              laneSelected: selected,
              selectedClipId: selectedClipId,
              recordingPeaks: recordingPeaks,
              recordingStartMs: recordingStartMs,
              onSelect: onSelect,
              onSelectClip: onSelectClip,
              onClipMoveLive: onClipMoveLive,
              onMoveCommit: onMoveCommit,
              onContextMenu: onContextMenu,
              onClipContextMenu: onClipContextMenu,
              onEditNote: onEditNote,
            ),
          ),
        ],
      ),
    );
  }
}

/// The fixed-width left control column for a track (Logic/Cubase mixer-strip
/// style): a color bar, the name + ⋯ menu, and a row of mute/solo, volume and
/// add-note controls.
class _TrackHeader extends StatelessWidget {
  const _TrackHeader({
    required this.track,
    required this.label,
    required this.selected,
    required this.armed,
    required this.accent,
    required this.canExpand,
    required this.expanded,
    required this.onToggleExpand,
    required this.onSelect,
    required this.onContextMenu,
    required this.onToggleMute,
    required this.onToggleSolo,
    required this.onVolume,
    required this.onVolumeCommit,
    required this.onAddNote,
    required this.onAddTechnique,
  });

  final AudioTrack track;
  final String label;
  final bool selected;
  final bool armed;
  final Color accent;
  final bool canExpand;
  final bool expanded;
  final VoidCallback? onToggleExpand;
  final VoidCallback onSelect;
  final ValueChanged<Offset> onContextMenu;
  final VoidCallback onToggleMute;
  final VoidCallback onToggleSolo;
  final ValueChanged<double> onVolume;
  final VoidCallback onVolumeCommit;
  final VoidCallback onAddNote;
  final VoidCallback onAddTechnique;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: kTrackHeaderW,
      decoration: BoxDecoration(
        color: selected ? Studio.surfaceRaised : Studio.panel,
        border: const Border(right: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          Container(width: 3, color: accent),
          Expanded(
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: onSelect,
                onSecondaryTapDown: (d) => onContextMenu(d.globalPosition),
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(9, 8, 8, 8),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisAlignment: MainAxisAlignment.spaceBetween,
                    children: [
                      Row(
                        children: [
                          if (canExpand)
                            _DiscloseButton(
                              expanded: expanded,
                              onTap: onToggleExpand,
                            ),
                          if (armed) ...[
                            Container(
                              width: 7,
                              height: 7,
                              decoration: const BoxDecoration(
                                color: Studio.record,
                                shape: BoxShape.circle,
                              ),
                            ),
                            const SizedBox(width: 6),
                          ],
                          Expanded(
                            child: Text(
                              label.toUpperCase(),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: const TextStyle(
                                color: Studio.textPrimary,
                                fontSize: 11.5,
                                fontWeight: FontWeight.w700,
                                letterSpacing: 0.3,
                              ),
                            ),
                          ),
                          TrackMenuButton(onTap: onContextMenu),
                        ],
                      ),
                      Row(
                        children: [
                          MsToggle(
                            label: 'M',
                            active: track.muted,
                            activeColor: Studio.record,
                            onTap: onToggleMute,
                          ),
                          const SizedBox(width: 4),
                          MsToggle(
                            label: 'S',
                            active: track.soloed,
                            activeColor: Studio.accent,
                            onTap: onToggleSolo,
                          ),
                          const SizedBox(width: 10),
                          Expanded(
                            child: StudioFader(
                              value: track.volume,
                              accent: accent,
                              onChanged: onVolume,
                              onChangeEnd: onVolumeCommit,
                            ),
                          ),
                          // Note/technique annotations apply to vocal takes
                          // only — the backing track gets no add buttons.
                          if (!track.isBacking) ...[
                            const SizedBox(width: 8),
                            StudioMiniButton(
                              icon: FIcons.stickyNote,
                              onTap: onAddNote,
                            ),
                            const SizedBox(width: 6),
                            StudioMiniButton(
                              icon: FIcons.waypoints,
                              onTap: onAddTechnique,
                            ),
                          ],
                        ],
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The disclosure chevron that folds the inline pitch lane in/out.
class _DiscloseButton extends StatelessWidget {
  const _DiscloseButton({required this.expanded, required this.onTap});

  final bool expanded;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.only(right: 4),
          child: Icon(
            expanded ? FIcons.chevronDown : FIcons.chevronRight,
            size: 14,
            color: expanded ? Studio.accent : Studio.textSecondary,
          ),
        ),
      ),
    );
  }
}

/// The ⋯ overflow button — opens the track context menu at its own position.
class TrackMenuButton extends StatelessWidget {
  const TrackMenuButton({required this.onTap, super.key});

  final ValueChanged<Offset> onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (d) => onTap(d.globalPosition),
        child: const Padding(
          padding: EdgeInsets.only(left: 4),
          child: Icon(FIcons.ellipsis, size: 16, color: Studio.textSecondary),
        ),
      ),
    );
  }
}

class MsToggle extends StatelessWidget {
  const MsToggle({
    required this.label,
    required this.active,
    required this.activeColor,
    required this.onTap,
    super.key,
  });

  final String label;
  final bool active;
  final Color activeColor;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 20,
          height: 18,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: active
                ? activeColor.withValues(alpha: 0.2)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(3),
            border: Border.all(
              color: active
                  ? activeColor.withValues(alpha: 0.6)
                  : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? activeColor : Studio.textSecondary,
              fontSize: 10,
              fontWeight: FontWeight.w700,
            ),
          ),
        ),
      ),
    );
  }
}

/// Live recording peaks arrive one per this many ms (matches the ViewModel).
const int _kRecordIntervalMs = 50;

class _Body extends StatefulWidget {
  const _Body({
    required this.track,
    required this.timelineDurationMs,
    required this.viewStartMs,
    required this.position,
    required this.accent,
    required this.laneSelected,
    required this.selectedClipId,
    required this.recordingPeaks,
    required this.recordingStartMs,
    required this.onSelect,
    required this.onSelectClip,
    required this.onClipMoveLive,
    required this.onMoveCommit,
    required this.onContextMenu,
    required this.onClipContextMenu,
    required this.onEditNote,
  });

  final AudioTrack track;
  final int timelineDurationMs;
  final int viewStartMs;
  final ValueListenable<Duration> position;
  final Color accent;

  /// Whether the lane itself is selected. With no specific clip selected, that
  /// highlights every clip on the lane; with one selected, only that clip.
  final bool laneSelected;
  final String? selectedClipId;
  final ValueListenable<List<double>>? recordingPeaks;
  final int recordingStartMs;
  final VoidCallback onSelect;
  final ValueChanged<String> onSelectClip;
  final void Function(String clipId, int startMs) onClipMoveLive;
  final VoidCallback onMoveCommit;
  final ValueChanged<Offset> onContextMenu;
  final void Function(String clipId, Offset position) onClipContextMenu;
  final ValueChanged<PracticeNote> onEditNote;

  @override
  State<_Body> createState() => _BodyState();
}

class _BodyState extends State<_Body> {
  /// The clip whose move-drag is in flight, if any. That clip is exempt from
  /// the visibility cull in [build]: unmounting it mid-gesture would dispose
  /// its drag recognizer, so the end-of-drag commit (persistence + engine
  /// realign) would never fire.
  String? _draggingClipId;

  void _setDragging(String clipId, bool active) {
    setState(() {
      if (active) {
        _draggingClipId = clipId;
      } else if (_draggingClipId == clipId) {
        _draggingClipId = null;
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final track = widget.track;
    final dur = widget.timelineDurationMs; // the visible span
    final viewStart = widget.viewStartMs;
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        double xOf(int ms) => dur <= 0 ? 0.0 : (ms - viewStart) / dur * width;
        double noteX(PracticeNote n) => xOf(n.positionMs);

        // The visible box (clamped to the window) + sliced peaks for a span, so a
        // clip far wider than the view stays bounded and stays aligned.
        ({
          double left,
          double width,
          List<double> peaks,
          double peakOffset,
          double peakSpan,
        })?
        visible(int spanStart, int spanDur, List<double> peaks) {
          if (dur <= 0 || spanDur <= 0) return null;
          final spanEnd = spanStart + spanDur;
          final vs = spanStart > viewStart ? spanStart : viewStart;
          final ve = spanEnd < viewStart + dur ? spanEnd : viewStart + dur;
          if (ve <= vs) return null;
          // The cut is at WHOLE peak indices while the box starts at the
          // un-quantized viewport edge, so slice[0] belongs up to one peak
          // interval BEFORE the box. The shared slice helper hands the painter
          // the discarded fraction (and the box width in peak units) so every
          // peak draws at its true timeline x — anchored to the ruler, not
          // the box.
          final cut = visiblePeakSlice(
            peaks,
            spanStartMs: spanStart,
            spanDurMs: spanDur,
            visibleStartMs: vs,
            visibleEndMs: ve,
          );
          final left = xOf(vs);
          return (
            left: left,
            width: (xOf(ve) - left).clamp(1.0, width),
            peaks: cut.peaks,
            peakOffset: cut.indexOffset,
            peakSpan: cut.indexSpan,
          );
        }

        // The clip's on-lane box. The actively-dragged clip must stay MOUNTED
        // even when it leaves the zoomed viewport — culling it would tear down
        // the drag recognizer mid-gesture, so the commit (persistence + engine
        // realign) would never fire — so it parks as a 1px sliver just outside
        // the lane's edge, where the Stack clips it invisible.
        ({
          double left,
          double width,
          List<double> peaks,
          double peakOffset,
          double peakSpan,
        })?
        clipBox(AudioClip clip) {
          final v = visible(clip.startMs, clip.durationMs, clip.waveformPeaks);
          if (v != null || clip.id != _draggingClipId) return v;
          return (
            left: clip.endMs <= viewStart ? -1.0 : width,
            width: 1.0,
            peaks: const <double>[],
            peakOffset: 0.0,
            peakSpan: 1.0,
          );
        }

        return Stack(
          children: [
            // Empty-lane background + grid (bottom of the stack).
            Positioned.fill(
              child: IgnorePointer(
                child: ColoredBox(
                  color: Studio.inset,
                  child: CustomPaint(
                    painter: TimelineGridPainter(
                      timelineDurationMs: dur,
                      startMs: viewStart,
                      color: Studio.grid,
                    ),
                  ),
                ),
              ),
            ),
            // Background surface: tap selects the lane, right-click = lane menu.
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: widget.onSelect,
                onSecondaryTapDown: (d) =>
                    widget.onContextMenu(d.globalPosition),
                child: const SizedBox.expand(),
              ),
            ),
            // Each clip — its own draggable, selectable, right-clickable block.
            // Keyed by clip id so the cull can never re-match an in-flight
            // clip element (and its drag state) onto a NEIGHBORING clip when
            // an entry leaves the list.
            for (final clip in track.clips)
              if (clipBox(clip) case final v?)
                Positioned(
                  key: ValueKey(clip.id),
                  left: v.left,
                  top: 3,
                  bottom: 3,
                  width: v.width,
                  child: _ClipRegion(
                    peaks: v.peaks,
                    peakOffset: v.peakOffset,
                    peakSpan: v.peakSpan,
                    color: widget.accent,
                    analyzing: !clip.hasWaveform,
                    selected:
                        widget.laneSelected &&
                        (widget.selectedClipId == null ||
                            widget.selectedClipId == clip.id),
                    laneWidth: width,
                    timelineDurationMs: dur,
                    clipStartMs: clip.startMs,
                    onTap: () => widget.onSelectClip(clip.id),
                    onContextMenu: (pos) =>
                        widget.onClipContextMenu(clip.id, pos),
                    onMoveLive: (ms) => widget.onClipMoveLive(clip.id, ms),
                    onMoveCommit: widget.onMoveCommit,
                    onDragActive: (active) => _setDragging(clip.id, active),
                  ),
                ),
            // The forming clip while this lane is the armed recording target.
            if (widget.recordingPeaks != null)
              ValueListenableBuilder<List<double>>(
                valueListenable: widget.recordingPeaks!,
                builder: (context, peaks, _) {
                  final v = visible(
                    widget.recordingStartMs,
                    peaks.length * _kRecordIntervalMs,
                    peaks,
                  );
                  if (v == null) return const SizedBox.shrink();
                  // Fixed pixels-per-sample for the whole take (timeline px/ms ×
                  // record interval) — the live painter anchors columns to it so
                  // the waveform grows without the drawn part shifting.
                  final pps = dur <= 0 ? 1.0 : _kRecordIntervalMs * width / dur;
                  return Positioned(
                    left: v.left,
                    top: 3,
                    bottom: 3,
                    width: v.width,
                    child: IgnorePointer(
                      child: _FormingClip(
                        peaks: v.peaks,
                        pixelsPerSample: pps,
                        // Keeps the absolute sample→column mapping glued to
                        // the timeline when the window edge lands mid-peak.
                        sampleOffset: v.peakOffset,
                      ),
                    ),
                  );
                },
              ),
            for (final note in track.notes)
              Positioned(
                left: noteX(note) - 8,
                bottom: 1,
                child: _NoteMarker(
                  color: widget.accent,
                  onTap: () => widget.onEditNote(note),
                ),
              ),
            Positioned.fill(
              child: PlayheadLayer(
                position: widget.position,
                timelineDurationMs: dur,
                startMs: viewStart,
                color: Studio.playhead,
              ),
            ),
          ],
        );
      },
    );
  }
}

/// One clip block on a lane: draggable along the timeline, tap to select,
/// right-click to delete. Holds its own drag accumulator.
class _ClipRegion extends StatefulWidget {
  const _ClipRegion({
    required this.peaks,
    required this.peakOffset,
    required this.peakSpan,
    required this.color,
    required this.analyzing,
    required this.selected,
    required this.laneWidth,
    required this.timelineDurationMs,
    required this.clipStartMs,
    required this.onTap,
    required this.onContextMenu,
    required this.onMoveLive,
    required this.onMoveCommit,
    required this.onDragActive,
  });

  final List<double> peaks;

  /// The slice's viewport mapping in peak-index units (see `visible()`), so
  /// the painter anchors each peak at its true timeline x.
  final double peakOffset;
  final double peakSpan;
  final Color color;
  final bool analyzing;
  final bool selected;
  final double laneWidth;
  final int timelineDurationMs;
  final int clipStartMs;
  final VoidCallback onTap;
  final ValueChanged<Offset> onContextMenu;
  final ValueChanged<int> onMoveLive;
  final VoidCallback onMoveCommit;

  /// Reports drag-in-flight transitions so the lane can exempt this clip from
  /// its visibility cull while the gesture is live.
  final ValueChanged<bool> onDragActive;

  @override
  State<_ClipRegion> createState() => _ClipRegionState();
}

class _ClipRegionState extends State<_ClipRegion> {
  int _baseStartMs = 0;
  double _dx = 0;
  bool _dragging = false;

  /// Commits once per started drag, from BOTH teardown paths (end and cancel):
  /// a cancelled drag has already applied live moves to the model, so skipping
  /// the commit would leave persistence and the engine silently diverged from
  /// what's on screen. The [_dragging] guard keeps a plain tap (whose arena
  /// loss also fires onHorizontalDragCancel) from committing anything.
  void _finishDrag() {
    if (!_dragging) return;
    _dragging = false;
    widget.onMoveCommit();
    widget.onDragActive(false);
  }

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.grab,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        onSecondaryTapDown: (d) => widget.onContextMenu(d.globalPosition),
        onHorizontalDragStart: (_) {
          _baseStartMs = widget.clipStartMs;
          _dx = 0;
          _dragging = true;
          widget.onDragActive(true);
        },
        onHorizontalDragUpdate: (d) {
          final dur = widget.timelineDurationMs;
          final w = widget.laneWidth;
          if (dur <= 0 || w <= 0) return;
          _dx += d.delta.dx;
          widget.onMoveLive(_baseStartMs + (_dx / w * dur).round());
        },
        onHorizontalDragEnd: (_) => _finishDrag(),
        onHorizontalDragCancel: _finishDrag,
        child: _Region(
          peaks: widget.peaks,
          peakOffset: widget.peakOffset,
          peakSpan: widget.peakSpan,
          color: widget.color,
          analyzing: widget.analyzing,
          selected: widget.selected,
        ),
      ),
    );
  }
}

/// A Logic Pro-style audio region: a rounded, tinted block with a colored cap
/// strip and the waveform drawn inside, so the clip's extent is unmistakable.
class _Region extends StatelessWidget {
  const _Region({
    required this.peaks,
    required this.peakOffset,
    required this.peakSpan,
    required this.color,
    required this.analyzing,
    this.selected = false,
  });

  final List<double> peaks;
  final double peakOffset;
  final double peakSpan;
  final Color color;
  final bool analyzing;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    return Container(
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        // Fill stays constant; only the outer border brightens when selected
        // (Logic style) — a lift of the lane's own hue, not pure white — so the
        // waveform inside reads the same either way.
        color: color.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(4),
        border: Border.all(
          color: selected
              ? selectedLaneColor(color)
              : color.withValues(alpha: 0.55),
          width: selected ? 2 : 1,
        ),
      ),
      child: Column(
        children: [
          // Region color cap.
          Container(height: 4, color: color.withValues(alpha: 0.9)),
          Expanded(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(2, 3, 2, 3),
              child: analyzing
                  ? const Center(
                      child: Text(
                        'Analyzing…',
                        style: TextStyle(color: Studio.textFaint, fontSize: 10),
                      ),
                    )
                  : CustomPaint(
                      size: Size.infinite,
                      painter: WaveformPainter(
                        peaks: peaks,
                        color: color,
                        indexOffset: peakOffset,
                        indexSpan: peakSpan,
                      ),
                    ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The clip forming while recording. Draws its waveform full-width (border as an
/// overlay, not an inset) at a fixed [pixelsPerSample], so the painter width is
/// exactly the clip width and the already-drawn waveform stays pixel-stable as
/// the take grows — the wobble-free way Logic / Pro Tools / Cubase show a live
/// take, rather than a waveform that restretches every frame.
class _FormingClip extends StatelessWidget {
  const _FormingClip({
    required this.peaks,
    required this.pixelsPerSample,
    required this.sampleOffset,
  });

  final List<double> peaks;
  final double pixelsPerSample;
  final double sampleOffset;

  @override
  Widget build(BuildContext context) {
    const color = Studio.record;
    return Container(
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(4),
      ),
      // Border overlaid via foregroundDecoration so it doesn't consume layout
      // width — keeping the waveform area exactly the clip width.
      foregroundDecoration: BoxDecoration(
        borderRadius: BorderRadius.circular(4),
        border: Border.all(color: color.withValues(alpha: 0.55)),
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(4),
        child: Column(
          children: [
            Container(height: 4, color: color.withValues(alpha: 0.9)),
            Expanded(
              child: Padding(
                // Vertical inset only: horizontal padding would shrink the
                // painter and break the fixed pixels-per-sample mapping.
                padding: const EdgeInsets.symmetric(vertical: 3),
                child: CustomPaint(
                  size: Size.infinite,
                  painter: LiveWaveformPainter(
                    peaks: peaks,
                    color: color,
                    pixelsPerSample: pixelsPerSample,
                    sampleOffset: sampleOffset,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _NoteMarker extends StatelessWidget {
  const _NoteMarker({required this.color, required this.onTap});

  final Color color;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 16,
          height: 16,
          alignment: Alignment.center,
          child: Icon(FIcons.stickyNote, size: 12, color: color),
        ),
      ),
    );
  }
}

/// Floating label shown under the pointer while long-press dragging a lane.
class _DragGhost extends StatelessWidget {
  const _DragGhost({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    return Material(
      color: Colors.transparent,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
        decoration: BoxDecoration(
          color: Studio.surfaceRaised,
          borderRadius: BorderRadius.circular(6),
          border: Border.all(color: Studio.accentBright),
          boxShadow: const [
            BoxShadow(
              color: Color(0x66000000),
              blurRadius: 12,
              offset: Offset(0, 4),
            ),
          ],
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(
              FIcons.gripVertical,
              size: 14,
              color: Studio.textSecondary,
            ),
            const SizedBox(width: 8),
            Text(
              label.toUpperCase(),
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 11,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.8,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
