import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../../domain/models/technique_media.dart';
import '../../../core/themes/studio.dart';
import 'technique_audio_player.dart';
import 'technique_video_player.dart';

/// The "Examples" block in the technique viewer: the technique's audio/video
/// clips, each with an inline player, plus a drop/browse zone to add more.
///
/// Stateless over the data; all mutations go through the callbacks so the view
/// model owns persistence and file IO.
class TechniqueMediaSection extends StatelessWidget {
  const TechniqueMediaSection({
    required this.media,
    required this.importing,
    required this.onAddPaths,
    required this.onRemove,
    required this.onRename,
    this.editable = true,
    this.onStartRecord,
    this.onStopRecord,
    this.isRecording = false,
    this.savingRecording = false,
    this.startingRecording = false,
    this.canRecord = true,
    this.recordLevel,
    this.analyzedId,
    this.onPlaybackPosition,
    this.analysisScrub,
    this.pauseSignal,
    this.expandedId,
    this.onToggleExpand,
    this.analysisBuilder,
    this.liveMonitor = false,
    this.enabled = true,
    super.key,
  });

  final List<TechniqueMedia> media;
  final bool importing;
  final ValueChanged<List<String>> onAddPaths;
  final ValueChanged<String> onRemove;

  /// Media id of the example whose analysis is currently shown: its inline
  /// player drives the analysis playhead so the spectrogram follows along as
  /// it plays. An id, not a path: byte-identical examples share one pooled
  /// file, and a path matched every one of them.
  final String? analyzedId;
  final ValueChanged<Duration>? onPlaybackPosition;

  /// Pushes analysis-scrub times into the analyzed clip's player so scrubbing
  /// the spectrogram also moves playback (bidirectional sync).
  final ValueListenable<Duration>? analysisScrub;

  /// Fires when the mic arms for a take: every clip's player pauses, so no
  /// example plays into the recording.
  final Listenable? pauseSignal;

  /// Media id of the example whose inline analysis is expanded (accordion),
  /// or null.
  final String? expandedId;

  /// Toggles a card's inline analysis open/closed (called with its media id).
  final ValueChanged<String>? onToggleExpand;

  /// Builds the analysis panel shown inside the expanded card (and, while
  /// recording an example, under the record control). Reflects the selected
  /// example. Null in contexts without analysis.
  final Widget Function()? analysisBuilder;

  /// True while a recording is in flight (drives the live preview placement).
  final bool liveMonitor;

  /// (mediaId, newLabel).
  final void Function(String mediaId, String label) onRename;

  /// When false (the read view), the clips are play-only: no add/drop zone and
  /// no per-clip rename/remove controls.
  final bool editable;

  /// False freezes every mutation control while keeping playback alive —
  /// used while the technique save is committing, when a clip added, removed,
  /// renamed or recorded during the await would be silently thrown away as
  /// the save lands. Unlike [editable] it keeps the add zone and record row
  /// visible (just inert), so the layout doesn't jump for the moment the
  /// commit is in flight.
  final bool enabled;

  /// Starts recording a clip to save as an example; returns an error to show, or
  /// null. When non-null (edit view), a "Record" affordance is offered.
  final Future<String?> Function()? onStartRecord;
  final VoidCallback? onStopRecord;
  final bool isRecording;
  final bool savingRecording;

  /// True while an example recording's start is still pending (the mic
  /// permission prompt, the mic open). [savingRecording] covers this window
  /// too, so the controls stay busy; this only picks the record row's label.
  final bool startingRecording;

  /// Whether a new example recording can start now (false e.g. while a drag-drop
  /// import is still copying).
  final bool canRecord;
  final ValueListenable<double>? recordLevel;

  Future<void> _browse() async {
    // Derive the picker filter from the same source as drag-drop's
    // isSupportedMedia so the two entry paths accept identical formats.
    final group = XTypeGroup(
      label: 'Audio or video',
      extensions: TechniqueMedia.pickerExtensions,
    );
    final files = await openFiles(acceptedTypeGroups: [group]);
    if (files.isEmpty) return;
    onAddPaths(files.map((f) => f.path).toList());
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(
              'EXAMPLES',
              style: TextStyle(
                color: Studio.accentBright,
                fontSize: 10.5,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
            const SizedBox(width: 8),
            if (importing)
              const SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(
                  strokeWidth: 1.8,
                  valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
                ),
              ),
          ],
        ),
        const SizedBox(height: 10),
        for (final m in media) ...[
          _MediaTile(
            // Keyed by clip: unkeyed, removing a card handed every later
            // slot to the next clip and rebuilt its player mid-playback.
            key: ValueKey(m.id),
            media: m,
            // A frozen edit surface (in-flight save) hides the hover
            // rename/remove icons too — the clip stays playable.
            editable: editable && enabled,
            onRemove: () => onRemove(m.id),
            onRename: (label) => onRename(m.id, label),
            // Only the analyzed clip's player is linked to the analysis playhead
            // (both directions).
            onPosition: m.id == analyzedId ? onPlaybackPosition : null,
            seekSignal: m.id == analyzedId ? analysisScrub : null,
            pauseSignal: pauseSignal,
            // Pair each audio example with a collapsible analysis. While
            // recording an example the live preview shows under the record
            // control instead, so the card's static analysis is hidden then.
            canExpand: !m.isVideo && analysisBuilder != null,
            expanded: m.id == expandedId,
            onToggleExpand: onToggleExpand == null
                ? null
                : () => onToggleExpand!(m.id),
            analysis: m.id == expandedId && (!liveMonitor || !editable)
                ? analysisBuilder?.call()
                : null,
          ),
          const SizedBox(height: 12),
        ],
        if (editable) ...[
          _AddZone(
            onBrowse: _browse,
            onDropPaths: onAddPaths,
            dense: media.isNotEmpty,
            enabled: enabled && !importing && !isRecording && !savingRecording,
          ),
          if (onStartRecord != null) ...[
            const SizedBox(height: 8),
            _RecordRow(
              onStart: onStartRecord!,
              // A frozen surface also inerts Stop: finalizing a take
              // mid-commit would stage a clip the airborne save has already
              // snapshotted past (the VM guards this too).
              onStop: enabled ? onStopRecord : null,
              isRecording: isRecording,
              saving: savingRecording,
              starting: startingRecording,
              canRecord: canRecord && enabled,
              level: recordLevel,
            ),
          ],
          // Live preview while recording an example (where the new clip has no
          // card yet) — surfaced so a recording is never a blank panel.
          if (liveMonitor && analysisBuilder != null) ...[
            const SizedBox(height: 12),
            analysisBuilder!.call(),
          ],
        ] else if (media.isEmpty)
          // Read view with no clips: keep the field visible with a placeholder.
          const Text(
            'Empty content',
            style: TextStyle(
              color: Studio.textFaint,
              fontSize: 13,
              fontStyle: FontStyle.italic,
            ),
          ),
      ],
    );
  }
}

/// One example, as a card: a header (icon + name, rename/remove on hover while
/// editing, and an expand chevron), its inline player, and — when expanded — its
/// paired analysis. Each recording is thus paired one-to-one with its analysis.
class _MediaTile extends StatefulWidget {
  const _MediaTile({
    required this.media,
    required this.editable,
    required this.onRemove,
    required this.onRename,
    this.onPosition,
    this.seekSignal,
    this.pauseSignal,
    this.canExpand = false,
    this.expanded = false,
    this.onToggleExpand,
    this.analysis,
    super.key,
  });

  final TechniqueMedia media;
  final bool editable;
  final VoidCallback onRemove;
  final ValueChanged<String> onRename;
  final ValueChanged<Duration>? onPosition;
  final ValueListenable<Duration>? seekSignal;
  final Listenable? pauseSignal;

  /// Whether this card offers an expandable analysis (audio clips only).
  final bool canExpand;
  final bool expanded;
  final VoidCallback? onToggleExpand;

  /// The analysis panel, when this card is the expanded one (else null).
  final Widget? analysis;

  @override
  State<_MediaTile> createState() => _MediaTileState();
}

class _MediaTileState extends State<_MediaTile>
    with AutomaticKeepAliveClientMixin {
  bool _hover = false;

  /// The page around the cards is a lazy ListView: without this, scrolling
  /// the Examples block past its cache extent disposed every player in it,
  /// stopping a clip the user was following and resetting it to 0:00.
  @override
  bool get wantKeepAlive => true;

  @override
  Widget build(BuildContext context) {
    super.build(context);
    final m = widget.media;
    final canExpand = widget.canExpand && widget.onToggleExpand != null;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: Container(
        decoration: BoxDecoration(
          color: Studio.surface,
          borderRadius: BorderRadius.circular(10),
          border: Border.all(
            color: widget.expanded
                ? Studio.accent.withValues(alpha: 0.4)
                : Studio.border,
          ),
        ),
        padding: const EdgeInsets.fromLTRB(12, 10, 12, 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  m.isVideo ? FIcons.video : FIcons.music2,
                  size: 13,
                  color: Studio.textFaint,
                ),
                const SizedBox(width: 6),
                // The name area toggles the analysis (when expandable).
                Expanded(
                  child: MouseRegion(
                    cursor: canExpand
                        ? SystemMouseCursors.click
                        : SystemMouseCursors.basic,
                    child: GestureDetector(
                      behavior: HitTestBehavior.opaque,
                      onTap: canExpand ? widget.onToggleExpand : null,
                      child: Text(
                        m.displayLabel,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          color: Studio.textSecondary,
                          fontSize: 12,
                          fontWeight: FontWeight.w500,
                        ),
                      ),
                    ),
                  ),
                ),
                if (widget.editable) ...[
                  const SizedBox(width: 8),
                  AnimatedOpacity(
                    opacity: _hover ? 1 : 0,
                    duration: const Duration(milliseconds: 120),
                    child: Row(
                      children: [
                        _HoverIcon(
                          icon: FIcons.pencil,
                          tooltip: 'Rename',
                          onTap: () async {
                            final label = await _promptLabel(
                              context,
                              m.displayLabel,
                            );
                            if (label != null) widget.onRename(label);
                          },
                        ),
                        const SizedBox(width: 4),
                        _HoverIcon(
                          icon: FIcons.trash2,
                          tooltip: 'Remove',
                          onTap: widget.onRemove,
                        ),
                      ],
                    ),
                  ),
                ],
                if (canExpand) ...[
                  const SizedBox(width: 8),
                  _ExpandChevron(
                    expanded: widget.expanded,
                    onTap: widget.onToggleExpand!,
                  ),
                ],
              ],
            ),
            const SizedBox(height: 8),
            m.isVideo
                ? TechniqueVideoPlayer(
                    key: ValueKey(m.id),
                    path: m.path,
                    pauseSignal: widget.pauseSignal,
                  )
                : TechniqueAudioPlayer(
                    key: ValueKey(m.id),
                    path: m.path,
                    onPosition: widget.onPosition,
                    seekSignal: widget.seekSignal,
                    pauseSignal: widget.pauseSignal,
                  ),
            if (widget.analysis != null) ...[
              const SizedBox(height: 14),
              const Divider(height: 1, thickness: 1, color: Studio.border),
              const SizedBox(height: 14),
              widget.analysis!,
            ],
          ],
        ),
      ),
    );
  }
}

/// The expand/collapse affordance: a chevron that flips when open.
class _ExpandChevron extends StatelessWidget {
  const _ExpandChevron({required this.expanded, required this.onTap});

  final bool expanded;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        behavior: HitTestBehavior.opaque,
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: AnimatedRotation(
            turns: expanded ? 0.5 : 0,
            duration: const Duration(milliseconds: 150),
            child: Icon(
              FIcons.chevronDown,
              size: 16,
              color: expanded ? Studio.accentBright : Studio.textFaint,
            ),
          ),
        ),
      ),
    );
  }
}

class _HoverIcon extends StatelessWidget {
  const _HoverIcon({
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: onTap,
          child: Icon(icon, size: 14, color: Studio.textFaint),
        ),
      ),
    );
  }
}

/// The drop + browse zone. Full size when empty, compact once clips exist.
class _AddZone extends StatefulWidget {
  const _AddZone({
    required this.onBrowse,
    required this.onDropPaths,
    required this.dense,
    required this.enabled,
  });

  final VoidCallback onBrowse;
  final ValueChanged<List<String>> onDropPaths;
  final bool dense;

  /// False while an import is in flight: ignore taps and drops so a second
  /// import can't be launched on top of the first.
  final bool enabled;

  @override
  State<_AddZone> createState() => _AddZoneState();
}

class _AddZoneState extends State<_AddZone> {
  bool _dragging = false;

  @override
  Widget build(BuildContext context) {
    return DropTarget(
      onDragEntered: (_) =>
          widget.enabled ? setState(() => _dragging = true) : null,
      onDragExited: (_) => setState(() => _dragging = false),
      onDragDone: (detail) {
        setState(() => _dragging = false);
        if (!widget.enabled) return;
        // Hand over EVERY dropped path: the view model filters, and it needs
        // the unsupported ones to report them. Filtering here made a drop of
        // only unsupported files do nothing at all — no import, no notice —
        // and under-counted the skips in a mixed drop.
        final paths = detail.files.map((f) => f.path).toList();
        if (paths.isNotEmpty) widget.onDropPaths(paths);
      },
      child: MouseRegion(
        cursor: widget.enabled
            ? SystemMouseCursors.click
            : SystemMouseCursors.basic,
        child: GestureDetector(
          onTap: widget.enabled ? widget.onBrowse : null,
          child: Opacity(
            opacity: widget.enabled ? 1 : 0.5,
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 120),
              width: double.infinity,
              padding: EdgeInsets.symmetric(
                horizontal: 16,
                vertical: widget.dense ? 12 : 22,
              ),
              decoration: BoxDecoration(
                color: _dragging
                    ? Studio.accent.withValues(alpha: 0.08)
                    : Studio.panel,
                borderRadius: BorderRadius.circular(10),
                border: Border.all(
                  color: _dragging
                      ? Studio.accent.withValues(alpha: 0.6)
                      : Studio.border,
                ),
              ),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Icon(
                    FIcons.plus,
                    size: 15,
                    color: _dragging ? Studio.accent : Studio.textSecondary,
                  ),
                  const SizedBox(width: 8),
                  Flexible(
                    child: Text(
                      widget.dense
                          ? 'Add another example'
                          : 'Drag an audio or video clip here, or click to browse',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        color: _dragging ? Studio.accent : Studio.textSecondary,
                        fontSize: 12.5,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A tiny single-field dialog to rename a clip. Returns the trimmed label, or
/// null if cancelled. An empty value clears back to the file name.
Future<String?> _promptLabel(BuildContext context, String current) {
  return showDialog<String>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (dialogContext) => _RenameExampleDialog(current: current),
  );
}

/// Owns its [TextEditingController] so it is disposed in [State.dispose] —
/// which runs only after the dialog route's exit transition completes.
/// Disposing right after `await showDialog` would free it mid-fade-out,
/// letting the dialog rebuild its `EditableText` over a disposed controller
/// (the use-after-dispose _StudioPrompt / _NewTechniqueDialog already fixed).
class _RenameExampleDialog extends StatefulWidget {
  const _RenameExampleDialog({required this.current});

  final String current;

  @override
  State<_RenameExampleDialog> createState() => _RenameExampleDialogState();
}

class _RenameExampleDialogState extends State<_RenameExampleDialog> {
  late final TextEditingController _controller = TextEditingController(
    text: widget.current,
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 380,
            padding: const EdgeInsets.all(20),
            decoration: BoxDecoration(
              color: Studio.surfaceRaised,
              borderRadius: BorderRadius.circular(16),
              border: Border.all(color: Studio.border),
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  'Rename example',
                  style: TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 15,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(height: 14),
                Container(
                  decoration: BoxDecoration(
                    color: Studio.inset,
                    borderRadius: BorderRadius.circular(8),
                    border: Border.all(color: Studio.border),
                  ),
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 10,
                  ),
                  child: TextField(
                    controller: _controller,
                    autofocus: true,
                    cursorColor: Studio.accent,
                    style: const TextStyle(
                      color: Studio.textPrimary,
                      fontSize: 14,
                    ),
                    decoration: const InputDecoration(
                      isCollapsed: true,
                      border: InputBorder.none,
                      hintText: 'Caption',
                      hintStyle: TextStyle(
                        color: Studio.textFaint,
                        fontSize: 14,
                      ),
                    ),
                    onSubmitted: (v) => Navigator.of(context).pop(v.trim()),
                  ),
                ),
                const SizedBox(height: 18),
                Row(
                  children: [
                    const Spacer(),
                    _DialogButton(
                      label: 'Cancel',
                      onTap: () => Navigator.of(context).pop(),
                    ),
                    const SizedBox(width: 8),
                    _DialogButton(
                      label: 'Save',
                      accent: true,
                      onTap: () =>
                          Navigator.of(context).pop(_controller.text.trim()),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _DialogButton extends StatelessWidget {
  const _DialogButton({
    required this.label,
    required this.onTap,
    this.accent = false,
  });

  final String label;
  final VoidCallback onTap;
  final bool accent;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
          decoration: BoxDecoration(
            color: accent
                ? Studio.accent.withValues(alpha: 0.16)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(7),
            border: Border.all(
              color: accent
                  ? Studio.accent.withValues(alpha: 0.45)
                  : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: accent ? Studio.accentBright : Studio.textPrimary,
              fontSize: 13,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

/// Record-an-example control under the drop zone: a Record button that shows a
/// "Starting…" state while the mic opens, becomes a level meter + Stop while
/// recording, then a "Saving…" state while the clip is imported as a persisted
/// example.
class _RecordRow extends StatelessWidget {
  const _RecordRow({
    required this.onStart,
    required this.onStop,
    required this.isRecording,
    required this.saving,
    required this.starting,
    required this.canRecord,
    required this.level,
  });

  final Future<String?> Function() onStart;
  final VoidCallback? onStop;
  final bool isRecording;
  final bool saving;
  final bool starting;
  final bool canRecord;
  final ValueListenable<double>? level;

  Future<void> _start(BuildContext context) async {
    final error = await onStart();
    if (error != null && context.mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error)));
    }
  }

  @override
  Widget build(BuildContext context) {
    if (saving) {
      return Row(
        children: [
          const SizedBox(
            width: 13,
            height: 13,
            child: CircularProgressIndicator(
              strokeWidth: 1.8,
              valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
            ),
          ),
          const SizedBox(width: 8),
          Text(
            // A pending start is busy too, but nothing is recorded yet: it
            // read "Saving example…" through the permission prompt and the
            // mic open.
            starting ? 'Starting the microphone…' : 'Saving example…',
            style: const TextStyle(color: Studio.textFaint, fontSize: 12),
          ),
        ],
      );
    }
    if (isRecording) {
      return Row(
        children: [
          Expanded(
            child: Container(
              height: 8,
              decoration: BoxDecoration(
                color: Studio.inset,
                borderRadius: BorderRadius.circular(4),
              ),
              clipBehavior: Clip.antiAlias,
              child: Align(
                alignment: Alignment.centerLeft,
                child: ValueListenableBuilder<double>(
                  valueListenable: level ?? const _Zero(),
                  builder: (context, v, _) => FractionallySizedBox(
                    widthFactor: v.clamp(0.0, 1.0),
                    child: const ColoredBox(color: Studio.record),
                  ),
                ),
              ),
            ),
          ),
          const SizedBox(width: 10),
          _RecBtn(
            label: 'Stop',
            icon: FIcons.square,
            danger: true,
            onTap: onStop,
          ),
        ],
      );
    }
    return Align(
      alignment: Alignment.centerLeft,
      child: _RecBtn(
        label: 'Record an example',
        icon: FIcons.mic,
        onTap: canRecord ? () => _start(context) : null,
      ),
    );
  }
}

/// A constant zero [ValueListenable] fallback for the meter.
class _Zero extends ValueListenable<double> {
  const _Zero();
  @override
  void addListener(VoidCallback listener) {}
  @override
  void removeListener(VoidCallback listener) {}
  @override
  double get value => 0;
}

class _RecBtn extends StatelessWidget {
  const _RecBtn({
    required this.label,
    required this.icon,
    this.danger = false,
    this.onTap,
  });

  final String label;
  final IconData icon;
  final bool danger;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final enabled = onTap != null;
    final fill = enabled
        ? (danger ? Studio.record : Studio.accent)
        : Studio.surfaceRaised;
    final fg = enabled ? Studio.background : Studio.textFaint;
    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
          decoration: BoxDecoration(
            color: fill,
            borderRadius: BorderRadius.circular(7),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(icon, size: 13, color: fg),
              const SizedBox(width: 6),
              Text(
                label,
                style: TextStyle(
                  color: fg,
                  fontSize: 12,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
