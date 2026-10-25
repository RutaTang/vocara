import 'dart:async';
import 'dart:math' as math;

import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart' show XTypeGroup, openFiles;
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/practice_note.dart';
import '../../../domain/models/song_practice.dart';
import '../../../domain/models/technique_marker.dart';
import '../../../routing/navigation.dart';
import '../../../routing/routes.dart';
import '../../../utils/format.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/confirm_dialog.dart';
import '../../core/widgets/stable_subtree.dart';
import '../../core/widgets/studio_button.dart';
import '../../core/widgets/studio_controls.dart';
import '../../core/widgets/tick_boundary.dart';
import '../../core/widgets/waveform_painter.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'analysis_lanes.dart';
import 'bottom_dock.dart';
import 'detail_transport_bar.dart';
import 'left_panel.dart';
import 'lyrics_panel.dart';
import 'note_editor.dart';
import 'ruler_bar.dart';
import 'technique_picker.dart';
import 'track_context_menu.dart';
import 'track_row.dart';

const _audioExtensions = {
  '.wav',
  '.mp3',
  '.m4a',
  '.aac',
  '.flac',
  '.ogg',
  '.oga',
  '.aif',
  '.aiff',
  '.caf',
  '.opus',
  '.wma',
};

/// The least height the timeline (and the side panels beside it) keeps
/// however tall the dock is dragged: the ruler, the scrollbar and most of a
/// lane.
const double _kMinTimelineHeight = 120;

bool _isAudioPath(String path) {
  final dot = path.lastIndexOf('.');
  if (dot < 0) return false;
  return _audioExtensions.contains(path.substring(dot).toLowerCase());
}

/// The macOS open panel, filtered to the same formats the drop path accepts so
/// both entry paths take identical files. Multi-select: every zone that accepts
/// a drop of many files accepts a browse of many files. Empty on cancel.
Future<List<String>> _pickAudioPaths() async {
  final group = XTypeGroup(
    label: 'Audio',
    extensions: [for (final e in _audioExtensions) e.substring(1)],
  );
  final files = await openFiles(acceptedTypeGroups: [group]);
  return [for (final f in files) f.path];
}

/// Detail view of one song practice — a dark studio workspace:
/// top transport / center arrange (Logic-style track-header lanes) /
/// right lyrics+notes panel / bottom dock.
class SongPracticeDetailScreen extends StatelessWidget {
  const SongPracticeDetailScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final vm = context.read<SongPracticeDetailViewModel>();

    // Block leaving while recording: finalize (stop + analyze + save) the take
    // first so navigating away mid-record never loses it — and in REC's arm
    // window cancel the start first, or it could still arm on the leaving
    // page. canPop must react to both, so wrap in a ListenableBuilder on the vm.
    return ListenableBuilder(
      listenable: vm,
      builder: (context, _) => PopScope(
        canPop: !vm.takeHoldsScreen,
        onPopInvokedWithResult: (didPop, _) async {
          if (didPop || !await vm.stopBeforeLeaving()) return;
          if (context.mounted) context.pop();
        },
        child: Scaffold(
          backgroundColor: Studio.background,
          body: ListenableBuilder(
            listenable: Listenable.merge([vm, vm.load]),
            builder: (context, _) {
              if (vm.load.running && vm.practice == null) {
                return const _NoWorkspace(
                  child: CircularProgressIndicator(
                    valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
                  ),
                );
              }
              final practice = vm.practice;
              if (practice == null) {
                return const _NoWorkspace(
                  child: Text(
                    'Could not load this practice.',
                    style: TextStyle(color: Studio.textSecondary),
                  ),
                );
              }
              return _Workspace(vm: vm, practice: practice);
            },
          ),
        ),
      ),
    );
  }
}

/// The screen while there is no workspace — loading, or a practice that could
/// not be loaded: [child] centred under a bar with the transport's back
/// control. Without it there was no way back at all, and Home's open guard,
/// released only when this route pops, stayed set.
class _NoWorkspace extends StatelessWidget {
  const _NoWorkspace({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Container(
          height: 54,
          padding: const EdgeInsets.symmetric(horizontal: 14),
          alignment: Alignment.centerLeft,
          decoration: const BoxDecoration(
            color: Studio.surface,
            border: Border(bottom: BorderSide(color: Studio.border)),
          ),
          child: MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => popOrGo(context, Routes.home),
              child: const SizedBox(
                width: 30,
                height: 30,
                child: Icon(
                  FIcons.chevronLeft,
                  size: 20,
                  color: Studio.textSecondary,
                ),
              ),
            ),
          ),
        ),
        Expanded(child: Center(child: child)),
      ],
    );
  }
}

class _Workspace extends StatefulWidget {
  const _Workspace({required this.vm, required this.practice});

  final SongPracticeDetailViewModel vm;
  final SongPractice practice;

  @override
  State<_Workspace> createState() => _WorkspaceState();
}

class _WorkspaceState extends State<_Workspace> {
  StreamSubscription<String>? _toastSub;

  SongPracticeDetailViewModel get vm => widget.vm;
  SongPractice get practice => widget.practice;

  @override
  void initState() {
    super.initState();
    _toastSub = vm.toasts.listen(_showToast);
  }

  @override
  void dispose() {
    _toastSub?.cancel();
    super.dispose();
  }

  void _showToast(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..clearSnackBars()
      ..showSnackBar(
        SnackBar(
          content: Text(
            message,
            style: const TextStyle(color: Studio.textPrimary, fontSize: 13),
          ),
          backgroundColor: Studio.surfaceRaised,
          behavior: SnackBarBehavior.floating,
          margin: const EdgeInsets.all(16),
          duration: const Duration(milliseconds: 2800),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(8),
            side: const BorderSide(color: Studio.border),
          ),
        ),
      );
  }

  List<AudioTrack> get _orderedTracks {
    // Only the active backing (per the Accompaniment / Original toggle) is shown
    // — it's the only one that's audible, so the mixer mirrors the timeline.
    final backing = vm.mixer.playReference
        ? practice.reference
        : practice.accompaniment;
    return [?backing, ...practice.activeVocalTracks];
  }

  /// Common guard for every drop zone: never let a drop interleave with a recording
  /// finalize / separation / other busy mutation — both write _practice, so the import
  /// would clobber the just-recorded take (lost-update race). Filters to supported audio
  /// and runs [action] on it.
  Future<void> _guardedAudioDrop(
    List<String> paths,
    Future<void> Function(List<String> audio) action,
  ) async {
    if (vm.takeInFlight || vm.isSeparating || vm.busy) {
      _showToast('Finish the current action before importing.');
      return;
    }
    final audio = paths.where(_isAudioPath).toList();
    if (audio.isEmpty) {
      if (paths.isNotEmpty) {
        _showToast('No supported audio files in the drop.');
      }
      return;
    }
    await action(audio);
  }

  /// Drop onto the backing region: the first file fills the empty backing slot (per the
  /// Accompaniment / Original toggle); any remaining files become new vocal takes.
  Future<void> _dropBacking(List<String> paths) =>
      _guardedAudioDrop(paths, (audio) async {
        var index = 0;
        if (vm.mixer.playReference) {
          if (practice.reference == null) {
            await vm.addReference(audio.first);
            index = 1;
          }
        } else if (practice.accompaniment == null) {
          await vm.addAccompaniment(audio.first);
          index = 1;
        }
        for (var i = index; i < audio.length; i++) {
          await vm.addVocalTake(audio[i]);
        }
      });

  /// Drop onto the (empty) vocal-takes area: each file becomes a new take.
  Future<void> _dropNewTake(List<String> paths) =>
      _guardedAudioDrop(paths, (audio) async {
        for (final a in audio) {
          await vm.addVocalTake(a);
        }
      });

  /// Drop onto an existing vocal take's lane: each file is added as a new clip on that
  /// take (never merged into the existing clips), the first at the playhead — the playhead
  /// at the drop, or [atMs] when the caller took it earlier — and each next one where the
  /// one before it ends, as a DAW lays out a multi-file drop on one track. Positions come
  /// from the drop, not from the playhead per file: each import waits for the one before,
  /// and read per file, playback running on meanwhile spread the files across the song.
  /// (Stacked at one spot instead, they hid each other, and the lane's analysis kept only
  /// the last.)
  Future<void> _dropClipToTake(
    String trackId,
    List<String> paths, {
    int? atMs,
  }) {
    var at = atMs ?? vm.position.value.inMilliseconds;
    return _guardedAudioDrop(paths, (audio) async {
      for (final a in audio) {
        final end = await vm.addClipToTake(trackId, a, atMs: at);
        if (end != null) at = end;
      }
    });
  }

  /// Adds a note at the playhead (a point) — used by the header / context menu /
  /// right panel.
  Future<void> _addNote(String trackId) =>
      _addNoteAt(trackId, vm.position.value.inMilliseconds, null);

  /// Adds a note at [startMs] (point) or across [startMs]..[endMs] (range) — the
  /// lane passes a range when the user drags.
  Future<void> _addNoteAt(String trackId, int startMs, int? endMs) async {
    final result = await showNoteEditor(
      context,
      at: Duration(milliseconds: startMs),
      end: endMs == null ? null : Duration(milliseconds: endMs),
    );
    if (result?.content != null) {
      await vm.addNote(trackId, startMs, endMs, result!.content!);
    }
  }

  Future<void> _editNote(String trackId, PracticeNote note) async {
    final result = await showNoteEditor(
      context,
      existing: note,
      at: vm.position.value,
    );
    if (result == null) return;
    if (result.deleted) {
      await vm.deleteNote(trackId, note.id);
    } else if (result.content != null) {
      await vm.updateNote(
        trackId,
        note.copyWith(content: result.content, updatedAt: DateTime.now()),
      );
    }
  }

  /// Adds a technique at the playhead (a point) — header / context menu / right
  /// panel.
  Future<void> _addTechnique(String trackId) =>
      _addTechniqueAt(trackId, vm.position.value.inMilliseconds, null);

  /// Adds a technique at [startMs] (point) or across [startMs]..[endMs] (range).
  Future<void> _addTechniqueAt(String trackId, int startMs, int? endMs) async {
    final title = (endMs != null && endMs > startMs)
        ? 'Add technique across ${formatClockMs(startMs)} → '
              '${formatClockMs(endMs)}'
        : 'Add technique at ${formatClockMs(startMs)}';
    final picked = await showTechniquePicker(
      context,
      vm.availableTechniques(),
      title: title,
    );
    if (picked != null && mounted) {
      await vm.addTechnique(trackId, picked.id, picked.name, startMs, endMs);
    }
  }

  Future<void> _editTechnique(String trackId, TechniqueMarker marker) async {
    final removed = await showConfirmDialog(
      context,
      title: 'Remove "${marker.techniqueName}"?',
      body:
          'This removes the technique marker from this take. '
          'This cannot be undone.',
      confirmLabel: 'Remove',
    );
    if (removed) await vm.deleteTechniqueMarker(trackId, marker.id);
  }

  Future<void> _deleteTrack(AudioTrack track) async {
    final confirmed = await showConfirmDialog(
      context,
      title: 'Delete "${track.displayName}"?',
      body: 'This removes the track, its audio file and its notes.',
    );
    if (confirmed) await vm.deleteTrack(track.id);
  }

  Future<void> _renameTrack(AudioTrack track) async {
    final name = await showRenameDialog(context, current: track.displayName);
    if (name != null) await vm.renameTrack(track.id, name);
  }

  Future<void> _separateBacking(AudioTrack track) async {
    final reseparate = vm.canReseparate;
    final confirmed = await showConfirmDialog(
      context,
      icon: FIcons.wandSparkles,
      destructive: false,
      title: reseparate ? 'Re-extract the backing?' : 'Extract the backing?',
      body: reseparate
          ? 'Runs separation on the original again. The instrumental in the '
                'Accompaniment slot is replaced. This takes a minute or two.'
          : 'Pulls the instrumental out of the song into the Accompaniment slot '
                'to sing over. The Original stays put. This takes a minute or two.',
      confirmLabel: reseparate ? 'Re-extract' : 'Extract backing',
    );
    if (confirmed) await vm.separateBacking(track.id);
  }

  void _showTrackMenu(AudioTrack track, Offset position) {
    final vocals = practice.activeVocalTracks;
    final idx = vocals.indexWhere((t) => t.id == track.id);
    showTrackContextMenu(
      context,
      position,
      TrackMenuActions(
        isVocal: !track.isBacking,
        canMergeDown: idx >= 0 && idx + 1 < vocals.length,
        // Only the Original (reference) can be separated — the Accompaniment is
        // already the backing. The item is HIDDEN when the engine is unusable
        // (vm probes availability once at construction, so it is resolved long
        // before a menu opens) — a dead action must not be offered.
        canSeparate: track.isReference && vm.separationAvailable,
        canExport: track.hasClip,
        reseparate: vm.canReseparate,
        currentColorValue: track.colorValue,
        onPickColor: (value) => vm.setTrackColor(track.id, value),
        onRename: () => _renameTrack(track),
        onSeparate: () => _separateBacking(track),
        onExport: () => vm.exportTrackAudio(track.id),
        onAddNote: () => _addNote(track.id),
        onAddTechnique: () => _addTechnique(track.id),
        onAddClip: () async {
          // "At playhead" means where it was at the tap: the open panel can
          // stay up for seconds while playback runs on.
          final atMs = vm.position.value.inMilliseconds;
          final paths = await _pickAudioPaths();
          if (paths.isNotEmpty) {
            await _dropClipToTake(track.id, paths, atMs: atMs);
          }
        },
        onDuplicate: () => vm.duplicateTrack(track.id),
        onSplit: () => vm.splitTrackAtPlayhead(track.id),
        onMergeDown: () => vm.mergeTrackDown(track.id),
        onReanalyze: () => vm.reanalyzeTrack(track.id),
        onDelete: () => _deleteTrack(track),
      ),
    );
  }

  void _showClipMenu(AudioTrack track, String clipId, Offset position) {
    showClipContextMenu(
      context,
      position,
      onDelete: () => vm.deleteClip(track.id, clipId),
      onExport: () => vm.exportClipAudio(track.id, clipId),
    );
  }

  @override
  Widget build(BuildContext context) {
    final content = Column(
      children: [
        DetailTransportBar(
          vm: vm,
          practice: practice,
          // A programmatic pop (or go) is not vetoed by the PopScope, so
          // finalize an in-flight recording here too — otherwise the in-app
          // back arrow would drop the take the PopScope protects.
          //
          // Pop, never go, when Home pushed this route: Home awaits that push
          // to reload its list and release its open guard, and go_router
          // completes a push future only on pop — after a go(home), every
          // later card tap was silently ignored. go is only the fallback for
          // a deep link, where nothing lies beneath to pop to.
          onBack: () async {
            if (vm.takeHoldsScreen && !await vm.stopBeforeLeaving()) return;
            if (!context.mounted) return;
            if (context.canPop()) {
              context.pop();
            } else {
              context.go(Routes.home);
            }
          },
        ),
        Expanded(
          child: _CatchUpGuard(
            catchingUp: vm.catchingUp,
            child: LayoutBuilder(
              builder: (context, constraints) => Column(
                children: [
                  Expanded(
                    child: Row(
                      children: [
                        LeftPanel(vm: vm),
                        Expanded(
                          child: _Center(
                            vm: vm,
                            practice: practice,
                            onDropBacking: _dropBacking,
                            onDropNewTake: _dropNewTake,
                            onDropClipToTake: _dropClipToTake,
                            onAddNote: _addNote,
                            onAddNoteAt: _addNoteAt,
                            onEditNote: _editNote,
                            onAddTechnique: _addTechnique,
                            onAddTechniqueAt: _addTechniqueAt,
                            onEditTechnique: _editTechnique,
                            onDeleteTrack: _deleteTrack,
                            onContextMenu: _showTrackMenu,
                            onClipContextMenu: _showClipMenu,
                          ),
                        ),
                        RightPanel(
                          vm: vm,
                          onAddNote: _addNote,
                          onEditNote: _editNote,
                          onAddTechnique: _addTechnique,
                          onEditTechnique: _editTechnique,
                        ),
                      ],
                    ),
                  ),
                  BottomDock(
                    vm: vm,
                    tracks: _orderedTracks,
                    onDeleteTrack: _deleteTrack,
                    // However tall it is dragged, the timeline keeps its place.
                    maxHeight: constraints.maxHeight - _kMinTimelineHeight,
                  ),
                ],
              ),
            ),
          ),
        ),
      ],
    );
    return Stack(
      children: [
        content,
        if (vm.lyricsFocus) Positioned.fill(child: LyricsFocusOverlay(vm: vm)),
        if (vm.isSeparating)
          Positioned.fill(
            child: _SeparationOverlay(
              progress: vm.separationProgress,
              onCancel: vm.cancelSeparation,
            ),
          ),
      ],
    );
  }
}

/// The workspace under the transport while the screen catches up on a save an
/// earlier screen on this practice left landing (see the view model's
/// `catchingUp`): shown, dimmed, and taking no input — an edit made on the
/// read from before that save lands loses either itself or the save. The
/// transport above stays live, so Back and playback still work.
class _CatchUpGuard extends StatelessWidget {
  const _CatchUpGuard({required this.catchingUp, required this.child});

  final bool catchingUp;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        // One subtree either way, so the workspace keeps its state when the
        // guard lifts.
        AbsorbPointer(absorbing: catchingUp, child: child),
        if (catchingUp)
          const Positioned.fill(
            child: ColoredBox(
              color: Color(0x800A0A0E),
              child: Center(
                child: Text(
                  'Finishing your last save…',
                  style: TextStyle(color: Studio.textSecondary, fontSize: 13),
                ),
              ),
            ),
          ),
      ],
    );
  }
}

/// A blocking overlay shown while stems are being separated (a multi-minute
/// background job). The progress is coarse, so the bar stays indeterminate.
class _SeparationOverlay extends StatelessWidget {
  const _SeparationOverlay({this.progress, required this.onCancel});

  /// 0..1 progress (driven by the engine's real tqdm progress).
  final double? progress;
  final VoidCallback onCancel;

  @override
  Widget build(BuildContext context) {
    final p = (progress ?? 0).clamp(0.0, 1.0);
    final pct = (p * 100).round();
    return GestureDetector(
      // Absorb taps so the blocked UI underneath can't be interacted with.
      behavior: HitTestBehavior.opaque,
      onTap: () {},
      child: ColoredBox(
        color: const Color(0xC40A0A0E),
        child: Center(
          child: Container(
            width: 380,
            padding: const EdgeInsets.fromLTRB(24, 22, 24, 20),
            decoration: BoxDecoration(
              color: Studio.surfaceRaised,
              borderRadius: BorderRadius.circular(16),
              border: Border.all(color: Studio.border),
              boxShadow: const [
                BoxShadow(
                  color: Color(0x80000000),
                  blurRadius: 40,
                  offset: Offset(0, 16),
                ),
              ],
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Container(
                      width: 32,
                      height: 32,
                      alignment: Alignment.center,
                      decoration: BoxDecoration(
                        color: Studio.accent.withValues(alpha: 0.14),
                        borderRadius: BorderRadius.circular(9),
                      ),
                      child: const Icon(
                        FIcons.wandSparkles,
                        size: 16,
                        color: Studio.accent,
                      ),
                    ),
                    const SizedBox(width: 12),
                    const Expanded(
                      child: Text(
                        'Extracting the backing',
                        style: TextStyle(
                          color: Studio.textPrimary,
                          fontSize: 15,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                    ),
                    Text(
                      '$pct%',
                      style: const TextStyle(
                        color: Studio.accent,
                        fontSize: 14,
                        fontWeight: FontWeight.w700,
                        fontFeatures: [FontFeature.tabularFigures()],
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 18),
                ClipRRect(
                  borderRadius: BorderRadius.circular(5),
                  child: SizedBox(
                    height: 8,
                    child: LayoutBuilder(
                      builder: (context, c) => Stack(
                        children: [
                          const Positioned.fill(
                            child: ColoredBox(color: Studio.inset),
                          ),
                          AnimatedContainer(
                            duration: const Duration(milliseconds: 280),
                            curve: Curves.easeOut,
                            width: p * c.maxWidth,
                            decoration: const BoxDecoration(
                              gradient: LinearGradient(
                                colors: [Studio.accent, Studio.accentBright],
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 16),
                const Text(
                  'Pulling the instrumental out of the song to sing over.\n'
                  'This can take a minute or two.',
                  style: TextStyle(
                    color: Studio.textFaint,
                    fontSize: 12,
                    height: 1.45,
                  ),
                ),
                const SizedBox(height: 20),
                Align(
                  alignment: Alignment.centerRight,
                  child: StudioButton(label: 'Cancel', onTap: onCancel),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _Center extends StatelessWidget {
  const _Center({
    required this.vm,
    required this.practice,
    required this.onDropBacking,
    required this.onDropNewTake,
    required this.onDropClipToTake,
    required this.onAddNote,
    required this.onAddNoteAt,
    required this.onEditNote,
    required this.onAddTechnique,
    required this.onAddTechniqueAt,
    required this.onEditTechnique,
    required this.onDeleteTrack,
    required this.onContextMenu,
    required this.onClipContextMenu,
  });

  final SongPracticeDetailViewModel vm;
  final SongPractice practice;

  /// Per-region drop handlers (each drop zone has its own hover highlight, so dragging over
  /// one region never lights up the others). Backing → fill the backing slot; new-take →
  /// add a take; clip-to-take → add a clip onto a specific existing take.
  final Future<void> Function(List<String> paths) onDropBacking;
  final Future<void> Function(List<String> paths) onDropNewTake;
  final Future<void> Function(String trackId, List<String> paths)
  onDropClipToTake;

  /// Add a point at the playhead (header button / context menu).
  final ValueChanged<String> onAddNote;

  /// Add a note at an explicit position/range (lane tap or drag).
  final void Function(String trackId, int startMs, int? endMs) onAddNoteAt;
  final void Function(String trackId, PracticeNote note) onEditNote;
  final ValueChanged<String> onAddTechnique;
  final void Function(String trackId, int startMs, int? endMs) onAddTechniqueAt;
  final void Function(String trackId, TechniqueMarker marker) onEditTechnique;
  final ValueChanged<AudioTrack> onDeleteTrack;
  final void Function(AudioTrack track, Offset position) onContextMenu;
  final void Function(AudioTrack track, String clipId, Offset position)
  onClipContextMenu;

  TrackRow _row(AudioTrack track) => TrackRow(
    key: ValueKey(track.id),
    track: track,
    label: track.displayName,
    timelineDurationMs: vm.playback.viewSpanMs,
    viewStartMs: vm.playback.viewStartMs,
    position: vm.position,
    selected: vm.selectedTrackId == track.id,
    armed: !track.isBacking && vm.recording.recordTargetId == track.id,
    selectedClipId: vm.selectedTrackId == track.id ? vm.selectedClipId : null,
    recordingPeaks: (vm.isRecording && vm.recording.recordTargetId == track.id)
        ? vm.recording.recordingPeaks
        : null,
    recordingStartMs: vm.recording.recordingStartMs,
    onSelect: () => vm.selectTrack(track.id),
    reorderable: !track.isBacking,
    onSelectClip: (clipId) => vm.selectClip(track.id, clipId),
    onClipMoveLive: (clipId, ms) => vm.moveClipStartLive(track.id, clipId, ms),
    onMoveCommit: vm.commitTrackMove,
    onClipContextMenu: (clipId, pos) => onClipContextMenu(track, clipId, pos),
    onCrossLaneMove: (draggedId) =>
        vm.moveVocalTrackToLaneOf(draggedId, track.id),
    onContextMenu: (pos) => onContextMenu(track, pos),
    onAddNote: () => onAddNote(track.id),
    onEditNote: (note) => onEditNote(track.id, note),
    onAddTechnique: () => onAddTechnique(track.id),
    onToggleMute: () => vm.mixer.toggleMute(track.id),
    onToggleSolo: () => vm.mixer.toggleSolo(track.id),
    onVolume: (v) => vm.mixer.setTrackVolumeLive(track.id, v),
    onVolumeCommit: vm.mixer.commitTrackVolume,
    onDelete: () => onDeleteTrack(track),
    canExpand: !track.isBacking,
    expanded: vm.analysis.isTrackExpanded(track.id),
    onToggleExpand: () => vm.analysis.toggleTrackExpanded(track.id),
    subLanes: vm.analysis.isTrackExpanded(track.id)
        ? AnalysisLanes(
            vm: vm,
            track: track,
            timelineDurationMs: vm.playback.viewSpanMs,
            onAddNote: (s, e) => onAddNoteAt(track.id, s, e),
            onEditNote: (n) => onEditNote(track.id, n),
            onAddTechnique: (s, e) => onAddTechniqueAt(track.id, s, e),
            onEditTechnique: (m) => onEditTechnique(track.id, m),
          )
        : null,
  );

  @override
  Widget build(BuildContext context) {
    // The center pane is a relayout and repaint boundary of its own (it fills
    // its Expanded either way): rebuilding anything under [_pane]'s
    // LayoutBuilder — a lane's live take, the master envelope — re-lays it
    // out, and without the boundary that layout climbed to the Scaffold and
    // repainted the side panels, the transport bar and the dock with it.
    //
    // The view itself (pan, follow, zoom, loop edges) notifies only
    // [PlaybackController.viewport], so the timeline rebuilds for it alone.
    return SizedBox.expand(
      child: RepaintBoundary(
        child: ListenableBuilder(
          listenable: vm.playback.viewport,
          builder: (context, _) => _pane(),
        ),
      ),
    );
  }

  Widget _pane() {
    final backing = vm.mixer.activeBacking;
    final ref = vm.mixer.playReference;
    // The visible timeline span (zoomable down to a 1-second window) and the
    // absolute time at the left edge; the ruler, grid, lanes, loop band and
    // master all scale to this viewport.
    final dur = vm.playback.viewSpanMs;
    final viewStart = vm.playback.viewStartMs;
    return LayoutBuilder(
      builder: (context, constraints) {
        // The timeline area starts after the fixed track-header column and the
        // left inset, so the loop band lines up with the waveforms + ruler.
        final timelineW =
            constraints.maxWidth - kTrackHeaderW - 2 * kTimelineHMargin;
        return Stack(
          children: [
            Column(
              children: [
                RulerBar(
                  vm: vm,
                  timelineDurationMs: dur,
                  position: vm.position,
                ),
                Expanded(
                  child: ColoredBox(
                    color: Studio.background,
                    child: Listener(
                      // Pan the viewport via either delivery path macOS uses
                      // for a two-finger horizontal swipe: a scroll signal
                      // (dx, or Shift+wheel dy) OR a trackpad pan-zoom gesture
                      // (panDelta.dx). Plain vertical scroll falls through to
                      // the lane list.
                      onPointerSignal: (signal) {
                        if (signal is! PointerScrollEvent || timelineW <= 0) {
                          return;
                        }
                        final shift = HardwareKeyboard.instance.isShiftPressed;
                        final delta = shift
                            ? signal.scrollDelta.dy
                            : signal.scrollDelta.dx;
                        if (delta != 0) {
                          vm.playback.panView(
                            (delta / timelineW * dur).round(),
                          );
                        }
                      },
                      onPointerPanZoomUpdate: (e) {
                        if (timelineW <= 0) return;
                        final dx = e.panDelta.dx;
                        if (dx != 0) {
                          vm.playback.panView((-dx / timelineW * dur).round());
                        }
                      },
                      child: ListView(
                        // No rubber-band overscroll on the tracks list.
                        physics: const ClampingScrollPhysics(),
                        padding: const EdgeInsets.only(bottom: 28),
                        children: [
                          _BackingHeader(
                            playReference: ref,
                            onSelect: vm.mixer.setPlayReference,
                          ),
                          // Backing region: its own drop zone, so only it highlights when a
                          // file is dragged over it. Drop fills the empty backing slot; any
                          // extra files become new takes.
                          StableSubtree<_Lane>(
                            sameAs: _Lane.same,
                            child: _Lane(
                              row: backing == null ? null : _row(backing),
                              onDropPaths: onDropBacking,
                              empty: (hover) => _LanePad(
                                child: _BrowseTap(
                                  onPaths: onDropBacking,
                                  child: _DropHint(
                                    icon: ref ? FIcons.disc3 : FIcons.music,
                                    text: ref
                                        ? 'Drag the original song here, or click to browse'
                                        : 'Drag a backing track here, or click to browse',
                                    highlighted: hover,
                                  ),
                                ),
                              ),
                            ),
                          ),
                          TakesHeader(vm: vm),
                          if (vm.versions.vocalTakes.isEmpty && !vm.isRecording)
                            _AudioDropZone(
                              onDropPaths: onDropNewTake,
                              builder: (hover) => _LanePad(
                                child: _BrowseTap(
                                  onPaths: onDropNewTake,
                                  child: _DropHint(
                                    icon: FIcons.mic,
                                    text:
                                        'Record a take, drag in vocal audio, or click to browse',
                                    highlighted: hover,
                                  ),
                                ),
                              ),
                            )
                          else
                            // Each take is its own drop zone: dropping audio onto a take's
                            // lane adds it as a new clip on THAT take (only that lane
                            // highlights). The live recording draws on the armed lane itself
                            // (Logic / Cubase), so there's no separate REC lane.
                            for (final t in vm.versions.vocalTakes)
                              StableSubtree<_Lane>(
                                sameAs: _Lane.same,
                                child: _Lane(
                                  row: _row(t),
                                  onDropPaths: (paths) =>
                                      onDropClipToTake(t.id, paths),
                                ),
                              ),
                          // FX BUSES — DAW group lanes (Cubase-style), between the
                          // takes and the master. Only on the native FX engine.
                          if (vm.mixer.supportsEffects) ...[
                            _BusesHeader(vm: vm),
                            for (final b in vm.mixer.fxBuses)
                              _BusRow(vm: vm, bus: b),
                          ],
                          const _MasterHeader(),
                          _MasterRow(vm: vm),
                        ],
                      ),
                    ),
                  ),
                ),
                _TimelineScrollbar(vm: vm),
              ],
            ),
            // Loop region band across the timeline area (only while looping).
            if (vm.playback.loopEnabled && dur > 0 && timelineW > 0)
              Positioned(
                // Clamp the band to the timeline viewport: a loop that starts
                // before the visible window must not paint over the track-header
                // column (negative left) or overshoot past the right edge. (The
                // ruler's equivalent band clamps too; this one has a header column
                // to bleed into.)
                left:
                    kTimelineHMargin +
                    kTrackHeaderW +
                    ((vm.playback.loopStartMs - viewStart) / dur * timelineW)
                        .clamp(0.0, timelineW),
                width:
                    ((vm.playback.loopEndMs - viewStart) / dur * timelineW)
                        .clamp(0.0, timelineW) -
                    ((vm.playback.loopStartMs - viewStart) / dur * timelineW)
                        .clamp(0.0, timelineW),
                top: rulerHeight,
                bottom: 0,
                child: IgnorePointer(
                  child: ColoredBox(
                    color: Studio.accent.withValues(alpha: 0.06),
                  ),
                ),
              ),
          ],
        );
      },
    );
  }
}

/// Header for the backing-track section: a section label plus a segmented
/// Accompaniment ⇄ Original toggle that one-click switches which backing plays
/// (and is shown). Switching is seamless — the playhead doesn't move.
class _BackingHeader extends StatelessWidget {
  const _BackingHeader({required this.playReference, required this.onSelect});

  final bool playReference;
  final ValueChanged<bool> onSelect;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 8),
      child: Row(
        children: [
          Text(
            (playReference ? 'Reference · original' : 'Accompaniment')
                .toUpperCase(),
            style: const TextStyle(
              color: Studio.textFaint,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.8,
            ),
          ),
          const Spacer(),
          _BackingToggle(playReference: playReference, onSelect: onSelect),
        ],
      ),
    );
  }
}

class _BackingToggle extends StatelessWidget {
  const _BackingToggle({required this.playReference, required this.onSelect});

  final bool playReference;
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
        mainAxisSize: MainAxisSize.min,
        children: [
          _seg('Accompaniment', !playReference, () => onSelect(false)),
          _seg('Original', playReference, () => onSelect(true)),
        ],
      ),
    );
  }

  Widget _seg(String label, bool active, VoidCallback onTap) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
          decoration: BoxDecoration(
            color: active
                ? Studio.accent.withValues(alpha: 0.18)
                : Colors.transparent,
            borderRadius: BorderRadius.circular(5),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? Studio.accentBright : Studio.textSecondary,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.3,
            ),
          ),
        ),
      ),
    );
  }
}

/// Header for the vocal-takes section: the "VOCAL TAKES" label plus a dropdown
/// that switches between the song's interpretations (emotional/technical
/// readings), each grouping its own takes against the shared backing.
@visibleForTesting
class TakesHeader extends StatelessWidget {
  const TakesHeader({required this.vm, super.key});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 16, 14, 10),
      child: Row(
        children: [
          const Text(
            'VOCAL TAKES',
            style: TextStyle(
              color: Studio.textFaint,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.8,
            ),
          ),
          const SizedBox(width: 10),
          // On a narrow centre pane the dropdown gives way (its name
          // ellipsizes) rather than overflowing the row off its chevron.
          Expanded(
            child: Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                _NewTakeButton(onTap: () => vm.createEmptyVocalTake()),
                const SizedBox(width: 10),
                Flexible(child: _InterpretationDropdown(vm: vm)),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// "+ New take" — creates an empty vocal lane (no clip yet), selects it and arms
/// it, so the next REC records into it.
class _NewTakeButton extends StatelessWidget {
  const _NewTakeButton({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 4),
          decoration: BoxDecoration(
            color: Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: Studio.border),
          ),
          child: const Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(FIcons.plus, size: 13, color: Studio.textSecondary),
              SizedBox(width: 5),
              Text(
                'New take',
                style: TextStyle(color: Studio.textSecondary, fontSize: 11.5),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The timeline scrollbar's thumb in a groove [grooveW] px wide: its left
/// edge, its width, and how many ms one px of thumb drag pans (0 when there is
/// nothing to scroll).
///
/// The thumb has a 28 px floor so it stays grabbable at deep zoom, so it is
/// wider than its share of the timeline and its left edge can only travel
/// `grooveW - width`: the start maps over THAT range ([0, extent − span] onto
/// [0, travel]), and a drag maps back the same way. Mapping the start onto the
/// whole groove instead ran the floored thumb off the groove's end near the
/// last page, clipped to a ~2 px sliver at deep zoom.
@visibleForTesting
({double left, double width, double msPerPx}) timelineScrollThumb({
  required int startMs,
  required int spanMs,
  required int extentMs,
  required double grooveW,
}) {
  final floor = grooveW < 28.0 ? grooveW : 28.0;
  final width = (spanMs / extentMs * grooveW).clamp(floor, grooveW);
  final travel = grooveW - width;
  // A song shorter than the minimum view span has span > extent.
  final scrollMs = extentMs - spanMs;
  if (travel <= 0 || scrollMs <= 0) {
    return (left: 0.0, width: width, msPerPx: 0.0);
  }
  return (
    left: (startMs / scrollMs * travel).clamp(0.0, travel),
    width: width,
    msPerPx: scrollMs / travel,
  );
}

/// The painter for a whole-timeline envelope — the master or a bus lane's,
/// [peaks] bucketed over [extentMs] from 0 — in the view [viewStartMs] +
/// [viewSpanMs]: only the visible slice, anchored through the shared helper
/// so it lines up with the zoomed lanes, ruler and playhead above.
///
/// The view can reach past [extentMs] (a take recording on past the
/// timeline's end stretches the view until it is saved; a song shorter than
/// the minimum view span): the slice then fills only the lane's share up to
/// the timeline's end. Spread over the whole lane, every peak drew late.
@visibleForTesting
WaveformPainter timelineEnvelopePainter(
  List<double> peaks, {
  required int extentMs,
  required int viewStartMs,
  required int viewSpanMs,
  required Color color,
}) {
  if (extentMs <= 0 ||
      peaks.isEmpty ||
      (viewStartMs <= 0 && viewSpanMs == extentMs)) {
    return WaveformPainter(peaks: peaks, color: color);
  }
  final end = viewStartMs + viewSpanMs > extentMs
      ? extentMs
      : viewStartMs + viewSpanMs;
  if (end <= viewStartMs) {
    return WaveformPainter(peaks: const [], color: color);
  }
  final cut = visiblePeakSlice(
    peaks,
    spanStartMs: 0,
    spanDurMs: extentMs,
    visibleStartMs: viewStartMs,
    visibleEndMs: end,
  );
  return WaveformPainter(
    peaks: cut.peaks,
    color: color,
    widthFraction: (end - viewStartMs) / viewSpanMs,
    indexOffset: cut.indexOffset,
    indexSpan: cut.indexSpan,
  );
}

/// A draggable horizontal scrollbar under the lanes — the reliable way to pan
/// the timeline (works regardless of trackpad gesture quirks). Hidden at fit-all
/// zoom (nothing to scroll); aligned to the timeline column (right of headers).
class _TimelineScrollbar extends StatelessWidget {
  const _TimelineScrollbar({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    if ((vm.practice?.timelineDurationMs ?? 0) <= 0) {
      return const SizedBox(height: 14);
    }
    // The view's own extent, which reaches past the timeline while a take
    // runs beyond its end: the practice's alone pinned the thumb at the right
    // end and panned at the wrong rate.
    final ext = vm.playback.extentMs;
    final span = vm.playback.viewSpanMs;
    // Always shown: the thumb fills the groove at fit-all (nothing to scroll)
    // and narrows / becomes draggable once zoomed in. It sits in the timeline
    // area (right of the header column); the left header space is left empty.
    return SizedBox(
      height: 14,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: kTimelineHMargin),
        child: Row(
          children: [
            const SizedBox(width: kTrackHeaderW),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 3),
                child: LayoutBuilder(
                  builder: (context, c) {
                    final w = c.maxWidth;
                    final thumb = timelineScrollThumb(
                      startMs: vm.playback.viewStartMs,
                      spanMs: span,
                      extentMs: ext,
                      grooveW: w,
                    );
                    return Stack(
                      children: [
                        Positioned.fill(
                          child: DecoratedBox(
                            decoration: BoxDecoration(
                              color: Studio.inset,
                              borderRadius: BorderRadius.circular(4),
                            ),
                          ),
                        ),
                        Positioned(
                          left: thumb.left,
                          width: thumb.width,
                          top: 0,
                          bottom: 0,
                          child: MouseRegion(
                            cursor: SystemMouseCursors.grab,
                            child: GestureDetector(
                              behavior: HitTestBehavior.opaque,
                              onHorizontalDragUpdate: (d) {
                                if (thumb.msPerPx <= 0) return;
                                vm.playback.panView(
                                  (d.delta.dx * thumb.msPerPx).round(),
                                );
                              },
                              child: DecoratedBox(
                                decoration: BoxDecoration(
                                  color: Studio.borderStrong,
                                  borderRadius: BorderRadius.circular(4),
                                ),
                              ),
                            ),
                          ),
                        ),
                      ],
                    );
                  },
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// The section label for the master lane (set apart from VOCAL TAKES).
class _MasterHeader extends StatelessWidget {
  const _MasterHeader();

  @override
  Widget build(BuildContext context) {
    return const Padding(
      padding: EdgeInsets.fromLTRB(14, 18, 14, 10),
      child: Text(
        'MASTER TRACK',
        style: TextStyle(
          color: Studio.textFaint,
          fontSize: 11,
          fontWeight: FontWeight.w700,
          letterSpacing: 0.8,
        ),
      ),
    );
  }
}

/// The master bus lane: its header carries the overall output fader (the whole
/// mix sums into the engine's master node), and its timeline area shows a live
/// composite of the audible mix — recomputed as tracks/levels change and while
/// recording (via [recordingPeaks]).
class _MasterRow extends StatelessWidget {
  const _MasterRow({required this.vm});

  final SongPracticeDetailViewModel vm;

  static String _formatDb(double v) {
    if (v <= 0.0001) return '-∞';
    final db = 20 * math.log(v) / math.ln10;
    return db >= -0.05 ? '0.0' : db.toStringAsFixed(1);
  }

  @override
  Widget build(BuildContext context) {
    final vol = vm.mixer.masterVolume;
    final tl = vm.playback.viewSpanMs;
    final viewStart = vm.playback.viewStartMs;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: kTimelineHMargin),
      child: Container(
        height: kTrackRowHeight,
        decoration: const BoxDecoration(
          border: Border(bottom: BorderSide(color: Studio.border)),
        ),
        child: Row(
          children: [
            // Kept, and on its own layer, as a take's header: the envelope
            // beside it follows every pan or zoom, the fader need not.
            RepaintBoundary(
              child: StableSubtree<_MasterHeaderStrip>(
                sameAs: _MasterHeaderStrip.same,
                child: _MasterHeaderStrip(vm: vm, volume: vol),
              ),
            ),
            Expanded(
              child: ColoredBox(
                color: Studio.inset,
                child: Stack(
                  children: [
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: TimelineGridPainter(
                            timelineDurationMs: tl,
                            startMs: viewStart,
                            color: Studio.grid,
                          ),
                        ),
                      ),
                    ),
                    // The composite mix envelope. Rebuilt on every recording tick
                    // (via recordingPeaks) so it renders live while recording —
                    // inside a TickBoundary, so that 20 Hz rebuild stays here.
                    Positioned.fill(
                      child: IgnorePointer(
                        child: Padding(
                          padding: const EdgeInsets.symmetric(vertical: 10),
                          child: TickBoundary(
                            child: ValueListenableBuilder<List<double>>(
                              valueListenable: vm.recording.recordingPeaks,
                              builder: (context, _, _) => CustomPaint(
                                painter: timelineEnvelopePainter(
                                  vm.mixer.masterWaveformPeaks,
                                  extentMs:
                                      vm.practice?.timelineDurationMs ?? 0,
                                  viewStartMs: viewStart,
                                  viewSpanMs: tl,
                                  color: Studio.accent,
                                ),
                              ),
                            ),
                          ),
                        ),
                      ),
                    ),
                    Positioned.fill(
                      child: PlayheadLayer(
                        position: vm.position,
                        timelineDurationMs: tl,
                        startMs: viewStart,
                        color: Studio.playhead,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// The master lane's header: the label, the master fader and its dB readout.
class _MasterHeaderStrip extends StatelessWidget {
  const _MasterHeaderStrip({required this.vm, required this.volume});

  final SongPracticeDetailViewModel vm;
  final double volume;

  /// The [StableSubtree.sameAs] of the strip: its callbacks are the mixer's
  /// own methods.
  static bool same(_MasterHeaderStrip next, _MasterHeaderStrip shown) =>
      identical(next.vm, shown.vm) && next.volume == shown.volume;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: kTrackHeaderW,
      decoration: const BoxDecoration(
        color: Studio.panel,
        border: Border(right: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          Container(width: 3, color: Studio.accent),
          Expanded(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(9, 10, 8, 10),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                children: [
                  const Text(
                    'MASTER',
                    style: TextStyle(
                      color: Studio.accent,
                      fontSize: 11.5,
                      fontWeight: FontWeight.w700,
                      letterSpacing: 0.4,
                    ),
                  ),
                  Row(
                    children: [
                      Expanded(
                        child: StudioFader(
                          value: volume,
                          accent: Studio.accent,
                          onChanged: vm.mixer.setMasterVolumeLive,
                          onChangeEnd: vm.mixer.commitMasterVolume,
                        ),
                      ),
                      const SizedBox(width: 8),
                      SizedBox(
                        width: 40,
                        child: Text(
                          '${_MasterRow._formatDb(volume)} dB',
                          textAlign: TextAlign.right,
                          style: const TextStyle(
                            color: Studio.textSecondary,
                            fontSize: 10,
                            fontWeight: FontWeight.w600,
                            fontFeatures: [FontFeature.tabularFigures()],
                          ),
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The "FX BUSES" section label + a button to add a bus from the timeline.
class _BusesHeader extends StatelessWidget {
  const _BusesHeader({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 18, 14, 10),
      child: Row(
        children: [
          const Text(
            'FX BUSES',
            style: TextStyle(
              color: Studio.textFaint,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.8,
            ),
          ),
          const Spacer(),
          MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: () => vm.mixer.createBus(),
              child: Container(
                padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 4),
                decoration: BoxDecoration(
                  color: Studio.surfaceRaised,
                  borderRadius: BorderRadius.circular(6),
                  border: Border.all(color: Studio.border),
                ),
                child: const Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(FIcons.plus, size: 13, color: Studio.textSecondary),
                    SizedBox(width: 5),
                    Text(
                      'New bus',
                      style: TextStyle(
                        color: Studio.textSecondary,
                        fontSize: 11.5,
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

/// An FX bus lane (Cubase-style group track): its header carries the bus name,
/// mute and fader (clicking it opens the bus's FX in the inspector), and its
/// timeline area renders the composite of every take routed into it.
class _BusRow extends StatelessWidget {
  const _BusRow({required this.vm, required this.bus});

  final SongPracticeDetailViewModel vm;
  final FxBus bus;

  static String _formatDb(double v) {
    if (v <= 0.0001) return '-∞';
    final db = 20 * math.log(v) / math.ln10;
    return db >= -0.05 ? '0.0' : db.toStringAsFixed(1);
  }

  void _showMenu(BuildContext context, Offset position) {
    showBusContextMenu(
      context,
      position,
      BusMenuActions(
        currentColorValue: bus.colorValue,
        onPickColor: (value) => vm.mixer.setBusColor(bus.id, value),
        onRename: () => _rename(context),
        onDuplicate: () => vm.mixer.duplicateBus(bus.id),
        onDelete: () => _delete(context),
      ),
    );
  }

  Future<void> _rename(BuildContext context) async {
    final name = await showRenameDialog(
      context,
      current: bus.name,
      title: 'Rename bus',
      hint: 'Bus name',
    );
    if (name != null) await vm.mixer.renameBus(bus.id, name);
  }

  Future<void> _delete(BuildContext context) async {
    if (await confirmDeleteBus(context, bus.name)) {
      await vm.mixer.deleteBus(bus.id);
    }
  }

  @override
  Widget build(BuildContext context) {
    final tl = vm.playback.viewSpanMs;
    final viewStart = vm.playback.viewStartMs;
    final selected = vm.selectedBusId == bus.id;
    final accent = bus.colorValue != null
        ? Color(bus.colorValue!)
        : Studio.accent;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: kTimelineHMargin),
      child: Container(
        height: kTrackRowHeight,
        decoration: const BoxDecoration(
          border: Border(bottom: BorderSide(color: Studio.border)),
        ),
        child: Row(
          children: [
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: () => vm.selectBus(bus.id),
                onSecondaryTapDown: (d) => _showMenu(context, d.globalPosition),
                child: Container(
                  width: kTrackHeaderW,
                  decoration: BoxDecoration(
                    color: selected ? Studio.surfaceRaised : Studio.panel,
                    border: const Border(
                      right: BorderSide(color: Studio.border),
                    ),
                  ),
                  child: Row(
                    children: [
                      Container(width: 3, color: accent),
                      Expanded(
                        child: Padding(
                          padding: const EdgeInsets.fromLTRB(9, 8, 8, 8),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            mainAxisAlignment: MainAxisAlignment.spaceBetween,
                            children: [
                              Row(
                                children: [
                                  Expanded(
                                    child: Text(
                                      bus.name,
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: TextStyle(
                                        color: accent,
                                        fontSize: 11.5,
                                        fontWeight: FontWeight.w700,
                                        letterSpacing: 0.2,
                                      ),
                                    ),
                                  ),
                                  TrackMenuButton(
                                    onTap: (pos) => _showMenu(context, pos),
                                  ),
                                ],
                              ),
                              Row(
                                children: [
                                  MsToggle(
                                    label: 'M',
                                    active: bus.muted,
                                    activeColor: Studio.record,
                                    onTap: () => vm.mixer.toggleBusMute(bus.id),
                                  ),
                                  const SizedBox(width: 4),
                                  MsToggle(
                                    label: 'S',
                                    active: bus.soloed,
                                    activeColor: Studio.accent,
                                    onTap: () => vm.mixer.toggleBusSolo(bus.id),
                                  ),
                                  const SizedBox(width: 10),
                                  Expanded(
                                    child: StudioFader(
                                      value: bus.volume,
                                      accent: accent,
                                      onChanged: (v) =>
                                          vm.mixer.setBusVolumeLive(bus.id, v),
                                      onChangeEnd: vm.commitFx,
                                    ),
                                  ),
                                  const SizedBox(width: 8),
                                  SizedBox(
                                    width: 40,
                                    child: Text(
                                      '${_formatDb(bus.volume)} dB',
                                      textAlign: TextAlign.right,
                                      style: const TextStyle(
                                        color: Studio.textSecondary,
                                        fontSize: 10,
                                        fontWeight: FontWeight.w600,
                                        fontFeatures: [
                                          FontFeature.tabularFigures(),
                                        ],
                                      ),
                                    ),
                                  ),
                                ],
                              ),
                            ],
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
            Expanded(
              child: ColoredBox(
                color: Studio.inset,
                child: Stack(
                  children: [
                    Positioned.fill(
                      child: IgnorePointer(
                        child: CustomPaint(
                          painter: TimelineGridPainter(
                            timelineDurationMs: tl,
                            startMs: viewStart,
                            color: Studio.grid,
                          ),
                        ),
                      ),
                    ),
                    Positioned.fill(
                      child: IgnorePointer(
                        child: Padding(
                          padding: const EdgeInsets.symmetric(vertical: 10),
                          child: TickBoundary(
                            child: ValueListenableBuilder<List<double>>(
                              valueListenable: vm.recording.recordingPeaks,
                              builder: (context, _, _) => CustomPaint(
                                painter: timelineEnvelopePainter(
                                  vm.mixer.busWaveformPeaks(bus.id),
                                  extentMs:
                                      vm.practice?.timelineDurationMs ?? 0,
                                  viewStartMs: viewStart,
                                  viewSpanMs: tl,
                                  color: accent,
                                ),
                              ),
                            ),
                          ),
                        ),
                      ),
                    ),
                    Positioned.fill(
                      child: PlayheadLayer(
                        position: vm.position,
                        timelineDurationMs: tl,
                        startMs: viewStart,
                        color: Studio.playhead,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A dropdown showing the active interpretation; its menu switches between them
/// and offers new/rename/delete. Many interpretations are expected, so it's a
/// menu rather than a segmented toggle.
class _InterpretationDropdown extends StatelessWidget {
  const _InterpretationDropdown({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    final active = vm.versions.active;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTapDown: (d) => _open(context, d.globalPosition),
        child: Container(
          padding: const EdgeInsets.fromLTRB(10, 5, 8, 5),
          decoration: BoxDecoration(
            color: Studio.inset,
            borderRadius: BorderRadius.circular(7),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(FIcons.drama, size: 13, color: Studio.accentBright),
              const SizedBox(width: 7),
              Flexible(
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 170),
                  child: Text(
                    active?.name ?? 'Interpretation',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                      color: Studio.textPrimary,
                      fontSize: 12,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                ),
              ),
              const SizedBox(width: 4),
              const Icon(FIcons.chevronDown, size: 14, color: Studio.textFaint),
            ],
          ),
        ),
      ),
    );
  }

  void _open(BuildContext context, Offset globalPosition) {
    final active = vm.versions.active;
    showStudioMenu(context, globalPosition, [
      for (final v in vm.versions.all)
        StudioMenuItem(
          label: v.name,
          selected: v.id == active?.id,
          onTap: () => vm.versions.setActive(v.id),
        ),
      StudioMenuItem(
        divider: true,
        icon: FIcons.plus,
        label: 'New interpretation',
        onTap: () => _create(context),
      ),
      StudioMenuItem(
        icon: FIcons.pencil,
        label: 'Rename',
        onTap: () {
          final a = active;
          if (a != null) _rename(context, a.id, a.name);
        },
      ),
      if (vm.versions.all.length > 1)
        StudioMenuItem(
          icon: FIcons.trash2,
          label: 'Delete',
          danger: true,
          onTap: () {
            final a = active;
            if (a != null) _delete(context, a.id, a.name);
          },
        ),
    ], width: 224);
  }

  Future<void> _create(BuildContext context) async {
    final name = await showRenameDialog(
      context,
      current: 'Interpretation ${vm.versions.all.length + 1}',
      title: 'New interpretation',
      hint: 'e.g. Tender, Powerful',
      confirmLabel: 'Create',
    );
    if (name != null) await vm.versions.create(name);
  }

  Future<void> _rename(BuildContext context, String id, String name) async {
    final next = await showRenameDialog(
      context,
      current: name,
      title: 'Rename interpretation',
      hint: 'Interpretation name',
    );
    if (next != null) await vm.versions.rename(id, next);
  }

  Future<void> _delete(BuildContext context, String id, String name) async {
    final ok = await showConfirmDialog(
      context,
      title: 'Delete "$name"?',
      body: 'This removes the interpretation and all of its takes.',
    );
    if (ok) await vm.versions.delete(id);
  }
}

/// Horizontal inset for non-lane items (drop hints) so they don't sit flush
/// against the edges the way full-width track lanes do.
class _LanePad extends StatelessWidget {
  const _LanePad({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 4, 14, 10),
      child: child,
    );
  }
}

class _DropHint extends StatelessWidget {
  const _DropHint({
    required this.icon,
    required this.text,
    this.highlighted = false,
  });

  final IconData icon;
  final String text;

  /// Lit up (accent border + tint) while a file is being dragged over the area,
  /// so the drop target is the box itself — not the whole background.
  final bool highlighted;

  @override
  Widget build(BuildContext context) {
    final color = highlighted ? Studio.accent : Studio.textFaint;
    return AnimatedContainer(
      duration: const Duration(milliseconds: 120),
      height: 64,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: highlighted
            ? Studio.accent.withValues(alpha: 0.08)
            : Studio.panel,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(
          color: highlighted ? Studio.accent : Studio.border,
          width: highlighted ? 1.5 : 1,
        ),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 16, color: color),
          const SizedBox(width: 8),
          Text(text, style: TextStyle(color: color, fontSize: 13)),
        ],
      ),
    );
  }
}

/// Makes an empty-state drop hint ALSO a click target: tapping opens the audio
/// file picker and feeds the picked paths through the same handler the drop
/// path uses (busy guard, format filter and toasts included). Only hints are
/// wrapped — never whole lanes, whose taps belong to clips/seek/menus.
class _BrowseTap extends StatelessWidget {
  const _BrowseTap({required this.onPaths, required this.child});

  final Future<void> Function(List<String> paths) onPaths;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () async {
          final paths = await _pickAudioPaths();
          if (paths.isNotEmpty) await onPaths(paths);
        },
        child: child,
      ),
    );
  }
}

/// A lane of the timeline list: [row] in its own file drop zone, or, for an
/// empty backing slot (no [row]), the [empty] hint. Kept while its row shows
/// the same thing ([same]): every notification of the view model rebuilds the
/// lane list, and a fader, pan-knob or FX tick, a clip drag or the master
/// fader changes at most one lane, so rebuilding every other lane's drop zone
/// and row shell cost a dozen widgets a lane, at pointer rate.
class _Lane extends StatelessWidget {
  const _Lane({required this.row, required this.onDropPaths, this.empty});

  final TrackRow? row;

  /// Captures only the lane's track id (which an unchanged row pins) and the
  /// workspace's drop handlers.
  final Future<void> Function(List<String> paths) onDropPaths;
  final Widget Function(bool hovering)? empty;

  /// The [StableSubtree.sameAs] of a lane. An empty slot is never kept.
  static bool same(_Lane next, _Lane shown) {
    final row = next.row, before = shown.row;
    return row != null && before != null && row.showsSameAs(before);
  }

  @override
  Widget build(BuildContext context) {
    final row = this.row;
    return _AudioDropZone(
      onDropPaths: onDropPaths,
      builder: (hover) => row == null
          ? empty!(hover)
          : _DropHighlight(
              active: hover,
              child: Padding(
                // Inset to align lanes with the ruler, labels and drop-hint
                // boxes.
                padding: const EdgeInsets.symmetric(
                  horizontal: kTimelineHMargin,
                ),
                child: row,
              ),
            ),
    );
  }
}

/// A per-region file drop target with its OWN hover state, so dragging a file over one
/// region never lights up the others (a single screen-wide DropTarget used to highlight
/// every drop hint at once). [builder] is given the current hover state to render its
/// highlight; [onDropPaths] receives the dropped file paths.
class _AudioDropZone extends StatefulWidget {
  const _AudioDropZone({required this.onDropPaths, required this.builder});

  final Future<void> Function(List<String> paths) onDropPaths;
  final Widget Function(bool hovering) builder;

  @override
  State<_AudioDropZone> createState() => _AudioDropZoneState();
}

class _AudioDropZoneState extends State<_AudioDropZone> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    return DropTarget(
      onDragEntered: (_) => setState(() => _hover = true),
      onDragExited: (_) => setState(() => _hover = false),
      onDragDone: (d) {
        if (_hover) setState(() => _hover = false);
        widget.onDropPaths(d.files.map((f) => f.path).toList());
      },
      child: widget.builder(_hover),
    );
  }
}

/// Overlays an accent tint + border on [child] while a file is dragged over it, so an
/// existing track lane reads as an active drop target (empty-state hints use [_DropHint]).
class _DropHighlight extends StatelessWidget {
  const _DropHighlight({required this.active, required this.child});

  final bool active;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Stack(
      children: [
        child,
        if (active)
          Positioned.fill(
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Studio.accent.withValues(alpha: 0.08),
                  border: Border.all(color: Studio.accent, width: 1.5),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
