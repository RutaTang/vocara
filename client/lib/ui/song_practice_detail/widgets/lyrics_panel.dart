import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart' show XTypeGroup, openFile;
import 'package:flutter/foundation.dart' show listEquals;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show RenderAbstractViewport;
import 'package:flutter/services.dart';
import 'package:forui/forui.dart' show FIcons;

import '../../../domain/models/lyric_line.dart';
import '../../../domain/models/practice_note.dart';
import '../../../domain/models/technique_marker.dart';
import '../../../utils/format.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_button.dart';
import '../view_model/song_practice_detail_view_model.dart';

const double _panelWidth = 300;

/// The right-side panel with three tabs: Lyrics (karaoke reader / editor / tap-
/// to-time sync), Notes and Techniques (the selected take's time-pinned notes /
/// technique markers). Collapsible to a thin rail.
class RightPanel extends StatelessWidget {
  const RightPanel({
    required this.vm,
    required this.onAddNote,
    required this.onEditNote,
    required this.onAddTechnique,
    required this.onEditTechnique,
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final ValueChanged<String> onAddNote;
  final void Function(String trackId, PracticeNote note) onEditNote;
  final ValueChanged<String> onAddTechnique;
  final void Function(String trackId, TechniqueMarker marker) onEditTechnique;

  @override
  Widget build(BuildContext context) {
    if (!vm.lyricsOpen) return _Rail(vm: vm);
    // Its own layer, so the rest of the screen repainting (the dock's live
    // readouts, the center pane) doesn't repaint the lyrics and notes.
    return RepaintBoundary(
      child: Container(
        width: _panelWidth,
        decoration: const BoxDecoration(
          color: Studio.surface,
          border: Border(left: BorderSide(color: Studio.border)),
        ),
        child: Column(
          children: [
            _TabBar(vm: vm),
            const Divider(height: 1, color: Studio.border),
            Expanded(child: _tabBody()),
          ],
        ),
      ),
    );
  }

  Widget _tabBody() {
    switch (vm.rightTab) {
      case RightTab.notes:
        return _NotesView(vm: vm, onAddNote: onAddNote, onEditNote: onEditNote);
      case RightTab.techniques:
        return _TechniquesView(
          vm: vm,
          onAddTechnique: onAddTechnique,
          onEditTechnique: onEditTechnique,
        );
      case RightTab.lyrics:
        return _LyricsTab(vm: vm);
    }
  }
}

class _TabBar extends StatelessWidget {
  const _TabBar({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 40,
      child: Padding(
        padding: const EdgeInsets.only(left: 12, right: 6),
        child: Row(
          children: [
            _Tab(
              'Lyrics',
              vm.rightTab == RightTab.lyrics,
              () => vm.setRightTab(RightTab.lyrics),
            ),
            const SizedBox(width: 4),
            _Tab(
              'Notes',
              vm.rightTab == RightTab.notes,
              () => vm.setRightTab(RightTab.notes),
            ),
            const SizedBox(width: 4),
            _Tab(
              'Techniques',
              vm.rightTab == RightTab.techniques,
              () => vm.setRightTab(RightTab.techniques),
            ),
            const Spacer(),
            // Panel-toggle icon, matching the left inspector's collapse button.
            _IconBtn(FIcons.panelRightClose, 'Collapse', vm.toggleLyricsPanel),
          ],
        ),
      ),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab(this.label, this.active, this.onTap);

  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
          decoration: BoxDecoration(
            color: active ? Studio.surfaceRaised : Colors.transparent,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(
              color: active ? Studio.border : Colors.transparent,
            ),
          ),
          child: Text(
            label.toUpperCase(),
            style: TextStyle(
              color: active ? Studio.textPrimary : Studio.textFaint,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.6,
            ),
          ),
        ),
      ),
    );
  }
}

class _LyricsTab extends StatefulWidget {
  const _LyricsTab({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  State<_LyricsTab> createState() => _LyricsTabState();
}

class _LyricsTabState extends State<_LyricsTab> {
  /// Bumped when a drop-import lands while the EDITOR is open. It keys the
  /// editor, so the bump re-creates it and re-seeds its controller from the
  /// just-imported lyrics. Without this the editor kept its pre-drop snapshot
  /// and Save silently overwrote the import (timestamps included).
  int _editorSeed = 0;

  SongPracticeDetailViewModel get vm => widget.vm;

  Future<void> _importDrop(String path) async {
    final editing = vm.lyricsMode == LyricsMode.edit;
    final before = editing ? vm.lyricsAsText() : '';
    // The VM validates type + size, reads, and toasts on every failure
    // (and guards against the recording/busy lost-update race).
    await vm.importLyricsFromFile(path);
    // Route a successful edit-mode drop INTO the open editor: the drop is an
    // explicit "replace the lyrics" action, so the editor must show (and Save
    // must apply) the imported text. A failed or no-op import leaves the
    // in-progress edit untouched.
    if (editing && vm.lyricsAsText() != before) {
      // The import replaces the draft too, or the re-created editor would
      // reopen on it instead of the imported lyrics.
      vm.lyricsDraft = null;
      if (mounted) setState(() => _editorSeed++);
    }
  }

  /// Click-to-upload twin of the drop: the macOS open panel filtered to the
  /// same extensions the VM's import validation accepts, routed through the
  /// same handler (validation, toasts and editor re-seeding included).
  Future<void> _browseImport() async {
    final group = XTypeGroup(
      label: 'Lyrics',
      extensions: ['lrc', 'txt', 'text'],
    );
    final file = await openFile(acceptedTypeGroups: [group]);
    if (file == null) return;
    await _importDrop(file.path);
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        if (vm.hasLyrics) _LyricsToolbar(vm: vm, onImport: _browseImport),
        Expanded(
          child: DropTarget(
            onDragDone: (d) async {
              if (d.files.isEmpty) return;
              await _importDrop(d.files.first.path);
            },
            child: _body(),
          ),
        ),
      ],
    );
  }

  Widget _body() {
    switch (vm.lyricsMode) {
      // Edit is always available — it's how the first lyrics get pasted/typed.
      case LyricsMode.edit:
        return _LyricsEditor(key: ValueKey(_editorSeed), vm: vm);
      case LyricsMode.read:
        return vm.hasLyrics
            ? _LyricsReader(vm: vm)
            : _EmptyState(vm: vm, onImport: _browseImport);
      case LyricsMode.sync:
        return vm.hasLyrics
            ? _LyricsSync(vm: vm)
            : _EmptyState(vm: vm, onImport: _browseImport);
    }
  }
}

class _LyricsToolbar extends StatelessWidget {
  const _LyricsToolbar({required this.vm, required this.onImport});

  final SongPracticeDetailViewModel vm;

  /// Opens the lyrics-file picker (the click twin of dropping a file anywhere
  /// on the panel).
  final VoidCallback onImport;

  @override
  Widget build(BuildContext context) {
    final mode = vm.lyricsMode;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 6, 4),
      child: Row(
        children: [
          const Spacer(),
          _IconBtn(FIcons.fileUp, 'Import lyrics file', onImport),
          _IconBtn(
            FIcons.pencil,
            'Edit lyrics',
            () => vm.setLyricsMode(
              mode == LyricsMode.edit ? LyricsMode.read : LyricsMode.edit,
            ),
            active: mode == LyricsMode.edit,
          ),
          _IconBtn(
            FIcons.timer,
            'Sync (tap-to-time)',
            () => vm.setLyricsMode(
              mode == LyricsMode.sync ? LyricsMode.read : LyricsMode.sync,
            ),
            active: mode == LyricsMode.sync,
          ),
          _IconBtn(FIcons.maximize2, 'Focus', () => vm.setLyricsFocus(true)),
        ],
      ),
    );
  }
}

class _NotesView extends StatelessWidget {
  const _NotesView({
    required this.vm,
    required this.onAddNote,
    required this.onEditNote,
  });

  final SongPracticeDetailViewModel vm;
  final ValueChanged<String> onAddNote;
  final void Function(String trackId, PracticeNote note) onEditNote;

  @override
  Widget build(BuildContext context) {
    final track = vm.selectedTrack;
    if (track != null) {
      // Notes annotate vocal takes only; the backing track gets no add button.
      final canAnnotate = !track.isBacking;
      final notes = vm.selectedTrackNotes;
      return ListView(
        padding: const EdgeInsets.fromLTRB(14, 14, 14, 16),
        children: [
          _NotesHeader(
            title: track.displayName,
            onAdd: canAnnotate ? () => onAddNote(track.id) : null,
          ),
          if (!canAnnotate)
            const _Hint('Notes apply to vocal takes only.')
          else if (notes.isEmpty)
            const _Hint(
              'No notes on this take yet.\nUse + to pin one at the '
              'playhead, or tap and drag the timeline lane.',
            )
          else
            for (final n in notes)
              _NoteTile(
                note: n,
                onTap: () {
                  vm.seek(Duration(milliseconds: n.positionMs));
                  onEditNote(track.id, n);
                },
              ),
        ],
      );
    }
    final all = vm.allNotes;
    return ListView(
      padding: const EdgeInsets.fromLTRB(14, 14, 14, 16),
      children: [
        const _NotesHeader(title: 'All notes'),
        if (all.isEmpty)
          const _Hint('Select a track and pin notes on the timeline.')
        else
          for (final ref in all)
            _NoteTile(
              note: ref.note,
              subtitle: ref.trackName,
              onTap: () {
                vm.selectTrack(ref.trackId);
                vm.seek(Duration(milliseconds: ref.note.positionMs));
              },
            ),
      ],
    );
  }
}

class _NotesHeader extends StatelessWidget {
  const _NotesHeader({
    required this.title,
    this.onAdd,
    this.addTooltip = 'Add note at playhead',
  });

  final String title;
  final VoidCallback? onAdd;
  final String addTooltip;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        children: [
          Expanded(
            child: Text(
              title.toUpperCase(),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                color: Studio.textFaint,
                fontSize: 11,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.8,
              ),
            ),
          ),
          if (onAdd != null)
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: onAdd,
                child: Tooltip(
                  message: addTooltip,
                  child: Container(
                    width: 24,
                    height: 24,
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: Studio.surfaceRaised,
                      borderRadius: BorderRadius.circular(6),
                      border: Border.all(color: Studio.border),
                    ),
                    child: const Icon(
                      FIcons.plus,
                      size: 15,
                      color: Studio.accent,
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

class _NoteTile extends StatelessWidget {
  const _NoteTile({required this.note, required this.onTap, this.subtitle});

  final PracticeNote note;
  final VoidCallback onTap;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          margin: const EdgeInsets.only(bottom: 6),
          padding: const EdgeInsets.all(9),
          decoration: BoxDecoration(
            color: Studio.panel,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: Studio.inset,
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Text(
                  note.isRange
                      ? '${formatClockMs(note.positionMs)} → '
                            '${formatClockMs(note.endMs!)}'
                      : formatClockMs(note.positionMs),
                  style: const TextStyle(
                    color: Studio.accent,
                    fontSize: 10,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      note.content,
                      style: const TextStyle(
                        color: Studio.textPrimary,
                        fontSize: 12.5,
                        height: 1.35,
                      ),
                    ),
                    if (subtitle != null) ...[
                      const SizedBox(height: 3),
                      Text(
                        subtitle!,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          color: Studio.textFaint,
                          fontSize: 10,
                        ),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The Techniques tab: the selected take's technique markers, mirroring
/// [_NotesView]. Backing tracks get no add button; tapping a tile seeks to the
/// marker and opens its edit/remove flow.
class _TechniquesView extends StatelessWidget {
  const _TechniquesView({
    required this.vm,
    required this.onAddTechnique,
    required this.onEditTechnique,
  });

  final SongPracticeDetailViewModel vm;
  final ValueChanged<String> onAddTechnique;
  final void Function(String trackId, TechniqueMarker marker) onEditTechnique;

  @override
  Widget build(BuildContext context) {
    final track = vm.selectedTrack;
    if (track == null) {
      return ListView(
        padding: const EdgeInsets.fromLTRB(14, 14, 14, 16),
        children: const [_Hint('Select a take to see the techniques it uses.')],
      );
    }
    final canAnnotate = !track.isBacking;
    final markers = vm.selectedTrackTechniques;
    return ListView(
      padding: const EdgeInsets.fromLTRB(14, 14, 14, 16),
      children: [
        _NotesHeader(
          title: track.displayName,
          onAdd: canAnnotate ? () => onAddTechnique(track.id) : null,
          addTooltip: 'Add technique at playhead',
        ),
        if (!canAnnotate)
          const _Hint('Techniques apply to vocal takes only.')
        else if (markers.isEmpty)
          const _Hint(
            'No techniques on this take yet.\nUse + to pin one at the '
            'playhead, or tap and drag the timeline lane.',
          )
        else
          for (final m in markers)
            _TechniqueTile(
              marker: m,
              // Tapping a tile navigates to it; removal is an explicit button so
              // a tap can't accidentally trigger a delete prompt.
              onTap: () => vm.seek(Duration(milliseconds: m.positionMs)),
              onRemove: () => onEditTechnique(track.id, m),
            ),
      ],
    );
  }
}

class _TechniqueTile extends StatelessWidget {
  const _TechniqueTile({
    required this.marker,
    required this.onTap,
    required this.onRemove,
  });

  final TechniqueMarker marker;
  final VoidCallback onTap;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          margin: const EdgeInsets.only(bottom: 6),
          padding: const EdgeInsets.fromLTRB(9, 9, 6, 9),
          decoration: BoxDecoration(
            color: Studio.panel,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            children: [
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: Studio.inset,
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Text(
                  marker.isRange
                      ? '${formatClockMs(marker.positionMs)} → '
                            '${formatClockMs(marker.endMs!)}'
                      : formatClockMs(marker.positionMs),
                  style: const TextStyle(
                    color: Studio.accent,
                    fontSize: 10,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              const SizedBox(width: 8),
              const Icon(FIcons.waypoints, size: 13, color: Studio.accent),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  marker.techniqueName,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 12.5,
                  ),
                ),
              ),
              MouseRegion(
                cursor: SystemMouseCursors.click,
                child: GestureDetector(
                  onTap: onRemove,
                  child: Tooltip(
                    message: 'Remove technique',
                    child: Container(
                      padding: const EdgeInsets.all(4),
                      color: Colors.transparent,
                      child: const Icon(
                        FIcons.trash2,
                        size: 14,
                        color: Studio.textFaint,
                      ),
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Hint extends StatelessWidget {
  const _Hint(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Text(
        text,
        style: const TextStyle(
          color: Studio.textFaint,
          fontSize: 12,
          height: 1.4,
        ),
      ),
    );
  }
}

class _Rail extends StatelessWidget {
  const _Rail({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: 34,
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(left: BorderSide(color: Studio.border)),
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: vm.toggleLyricsPanel,
          behavior: HitTestBehavior.opaque,
          child: Column(
            children: [
              const SizedBox(height: 12),
              const Icon(
                FIcons.panelRightOpen,
                size: 15,
                color: Studio.textSecondary,
              ),
              const SizedBox(height: 18),
              // Clipped, not overflowing, where the dock leaves the panels
              // less height than the label.
              const Flexible(
                child: RotatedBox(
                  quarterTurns: 1,
                  child: Text(
                    'LYRICS   NOTES   TECHNIQUES',
                    maxLines: 1,
                    softWrap: false,
                    overflow: TextOverflow.clip,
                    style: TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 11,
                      fontWeight: FontWeight.w700,
                      letterSpacing: 1.5,
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _IconBtn extends StatelessWidget {
  const _IconBtn(this.icon, this.tooltip, this.onTap, {this.active = false});

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;
  final bool active;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Tooltip(
          message: tooltip,
          child: Container(
            margin: const EdgeInsets.symmetric(horizontal: 1),
            padding: const EdgeInsets.all(6),
            decoration: active
                ? BoxDecoration(
                    color: Studio.accent.withValues(alpha: 0.16),
                    borderRadius: BorderRadius.circular(6),
                  )
                : null,
            child: Icon(
              icon,
              size: 15,
              color: active ? Studio.accentBright : Studio.textSecondary,
            ),
          ),
        ),
      ),
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.vm, required this.onImport});

  final SongPracticeDetailViewModel vm;

  /// Opens the lyrics-file picker (the click twin of dropping a file here).
  final VoidCallback onImport;

  @override
  Widget build(BuildContext context) {
    // Center + scroll view: centered whenever there is room, and on a panel
    // shorter than the content (tiny window with the dock open) it scrolls
    // instead of overflowing the Column by a few pixels.
    return Center(
      child: SingleChildScrollView(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(FIcons.quote, size: 26, color: Studio.textFaint),
              const SizedBox(height: 14),
              const Text(
                'No lyrics yet',
                style: TextStyle(
                  color: Studio.textSecondary,
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 16),
              StudioButton(
                label: 'Paste lyrics',
                icon: FIcons.clipboard,
                variant: StudioButtonVariant.accent,
                onTap: () => vm.setLyricsMode(LyricsMode.edit),
              ),
              const SizedBox(height: 10),
              StudioButton(
                label: 'Import lyrics file',
                icon: FIcons.fileUp,
                variant: StudioButtonVariant.subtle,
                onTap: onImport,
              ),
              const SizedBox(height: 12),
              const Text(
                'or drop a .lrc or .txt file here',
                textAlign: TextAlign.center,
                style: TextStyle(color: Studio.textFaint, fontSize: 12),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Karaoke reader — highlights the current line and auto-scrolls with the
/// playhead. [large] is the full-screen Focus variant.
class _LyricsReader extends StatefulWidget {
  const _LyricsReader({required this.vm, this.large = false});

  final SongPracticeDetailViewModel vm;
  final bool large;

  @override
  State<_LyricsReader> createState() => _LyricsReaderState();
}

class _LyricsReaderState extends State<_LyricsReader> {
  final ScrollController _scroll = ScrollController();
  List<GlobalKey> _keys = const [];
  int _active = -1;
  // The lyrics [_active] indexes into (see didUpdateWidget).
  List<LyricLine> _lines = const [];

  SongPracticeDetailViewModel get vm => widget.vm;

  @override
  void initState() {
    super.initState();
    _lines = vm.lyrics;
    _keys = List.generate(vm.lyrics.length, (_) => GlobalKey());
    vm.position.addListener(_onTick);
    WidgetsBinding.instance.addPostFrameCallback((_) => _onTick());
  }

  @override
  void didUpdateWidget(_LyricsReader old) {
    super.didUpdateWidget(old);
    if (vm.lyrics.length != _keys.length) {
      _keys = List.generate(vm.lyrics.length, (_) => GlobalKey());
    }
    // New lyrics (an import or edit while reading) re-time the lines, so the
    // latched index belongs to the OLD list: it would light whatever line now
    // sits there, and _onTick's same-index early-out would keep it until the
    // playhead next crossed a line — never, while paused. Re-pick it for this
    // build, and reveal it once laid out. Compared by value: the practice is
    // rebuilt on unrelated edits too, and that must not yank the scroll back.
    if (!listEquals(vm.lyrics, _lines)) {
      _lines = vm.lyrics;
      final idx = _active = vm.currentLyricIndex(
        vm.position.value.inMilliseconds,
      );
      WidgetsBinding.instance.addPostFrameCallback((_) => _reveal(idx));
    }
  }

  void _onTick() {
    if (!mounted) return;
    final idx = vm.currentLyricIndex(vm.position.value.inMilliseconds);
    if (idx == _active) return;
    setState(() => _active = idx);
    _reveal(idx);
  }

  /// Scrolls the active line [idx] into view. ListView.builder only builds the
  /// lines near the viewport, so after a seek (or opening Focus mid-song) the
  /// line can have no element to ensureVisible; skipping it there left the
  /// reader parked while the playhead ran on, for good — every later line is
  /// just as unbuilt. So an unbuilt line is JUMPED to at an offset
  /// extrapolated from the lines that are built, and revealed a frame later
  /// once the list has built around it; each pass extrapolates from lines
  /// nearer the target, so a rough first guess still converges.
  void _reveal(int idx, [int pass = 0]) {
    // The playhead moved on to another line: its own _reveal has taken over.
    if (!mounted || idx != _active || idx < 0 || idx >= _keys.length) return;
    final ctx = _keys[idx].currentContext;
    if (ctx != null) {
      Scrollable.ensureVisible(
        ctx,
        alignment: 0.4,
        duration: const Duration(milliseconds: 320),
        curve: Curves.easeOut,
      );
      return;
    }
    if (pass >= 4 || !_scroll.hasClients) return;
    final top = _estimateTop(idx);
    if (top == null) return;
    final pos = _scroll.position;
    _scroll.jumpTo(
      (top - pos.viewportDimension * 0.4).clamp(
        pos.minScrollExtent,
        pos.maxScrollExtent,
      ),
    );
    WidgetsBinding.instance.addPostFrameCallback((_) => _reveal(idx, pass + 1));
  }

  /// The scroll offset at which unbuilt line [idx] would start, extrapolated
  /// at the average height of the lines that are built; null when none are.
  double? _estimateTop(int idx) {
    int? first, last;
    var firstTop = 0.0, lastEnd = 0.0;
    for (var i = 0; i < _keys.length; i++) {
      final box = _keys[i].currentContext?.findRenderObject();
      if (box is! RenderBox || !box.hasSize) continue;
      final top = RenderAbstractViewport.of(
        box,
      ).getOffsetToReveal(box, 0).offset;
      if (first == null) {
        first = i;
        firstTop = top;
      }
      last = i;
      lastEnd = top + box.size.height;
    }
    if (first == null || last == null) return null;
    final avg = (lastEnd - firstTop) / (last - first + 1);
    return idx < first
        ? firstTop - (first - idx) * avg
        : lastEnd + (idx - last - 1) * avg;
  }

  @override
  void dispose() {
    vm.position.removeListener(_onTick);
    _scroll.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final lines = vm.lyrics;
    // Regenerate keys here too — not only in didUpdateWidget. _onTick fires off the
    // position listener and can setState THIS widget in the same frame that vm.lyrics
    // grew (an import/edit) but before the parent rebuild runs didUpdateWidget, so
    // itemBuilder would index a stale, shorter _keys and throw RangeError. Same
    // length-divergence guard, so steady-state rebuilds allocate nothing new.
    if (_keys.length != lines.length) {
      _keys = List.generate(lines.length, (_) => GlobalKey());
    }
    final big = widget.large;
    return ListView.builder(
      controller: _scroll,
      padding: EdgeInsets.symmetric(
        horizontal: big ? 40 : 16,
        vertical: big ? 120 : 20,
      ),
      itemCount: lines.length,
      itemBuilder: (context, i) {
        final l = lines[i];
        if (l.isBlank) return SizedBox(height: big ? 28 : 14, key: _keys[i]);
        final active = i == _active;
        return Container(
          key: _keys[i],
          padding: EdgeInsets.symmetric(vertical: big ? 9 : 5),
          child: MouseRegion(
            cursor: l.isSynced ? SystemMouseCursors.click : MouseCursor.defer,
            child: GestureDetector(
              onTap: l.isSynced ? () => vm.seekToLyric(l) : null,
              child: Text(
                l.text,
                textAlign: big ? TextAlign.center : TextAlign.left,
                style: TextStyle(
                  color: active
                      ? Studio.accentBright
                      : (l.isSynced
                            ? Studio.textPrimary
                            : Studio.textSecondary),
                  fontSize: big ? (active ? 34 : 26) : (active ? 16 : 14),
                  fontWeight: active ? FontWeight.w700 : FontWeight.w500,
                  height: 1.35,
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}

class _LyricsEditor extends StatefulWidget {
  // Keyed by _LyricsTab so a successful drop-import re-creates the editor
  // (re-seeding the controller from the imported lyrics).
  const _LyricsEditor({required this.vm, super.key});

  final SongPracticeDetailViewModel vm;

  @override
  State<_LyricsEditor> createState() => _LyricsEditorState();
}

class _LyricsEditorState extends State<_LyricsEditor> {
  // Reopens on the unsaved draft, if any: a tab switch or a collapse of the
  // panel disposes this editor (see [SongPracticeDetailViewModel.lyricsDraft]).
  late final TextEditingController _c = TextEditingController(
    text: widget.vm.lyricsDraft ?? widget.vm.lyricsAsText(),
  );

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final vm = widget.vm;
    return Padding(
      padding: const EdgeInsets.all(12),
      child: Column(
        children: [
          Expanded(
            child: Container(
              decoration: BoxDecoration(
                color: Studio.inset,
                borderRadius: BorderRadius.circular(8),
                border: Border.all(color: Studio.border),
              ),
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
              child: TextField(
                controller: _c,
                // The user's edits only, not focus or selection churn: the
                // draft must never be re-written behind a re-seed.
                onChanged: (text) => vm.lyricsDraft = text,
                autofocus: true,
                expands: true,
                maxLines: null,
                minLines: null,
                textAlignVertical: TextAlignVertical.top,
                cursorColor: Studio.accent,
                style: const TextStyle(
                  color: Studio.textPrimary,
                  fontSize: 14,
                  height: 1.4,
                ),
                decoration: const InputDecoration(
                  isCollapsed: true,
                  border: InputBorder.none,
                  hintText: 'Paste lyrics (plain text or .lrc), one line each…',
                  hintStyle: TextStyle(color: Studio.textFaint, fontSize: 14),
                ),
              ),
            ),
          ),
          const SizedBox(height: 12),
          Row(
            children: [
              StudioButton(
                label: 'Cancel',
                onTap: () => vm.setLyricsMode(LyricsMode.read),
              ),
              const Spacer(),
              StudioButton(
                label: 'Save',
                variant: StudioButtonVariant.accent,
                onTap: () async {
                  // Leave edit mode only once the text is on disk: a failed
                  // write reverts the model, so closing regardless would drop
                  // the draft with nothing but a toast to show for it.
                  final text = _c.text;
                  if (!await vm.applyEditedLyrics(text)) return;
                  // Nor over text typed while the write waited (it queues
                  // behind other saves, a slow import's copy among them):
                  // that is still the draft, and closing dropped it.
                  // And only out of edit mode: the user may have moved on to
                  // Sync while it waited.
                  final now = mounted ? _c.text : (vm.lyricsDraft ?? text);
                  if (now == text && vm.lyricsMode == LyricsMode.edit) {
                    vm.setLyricsMode(LyricsMode.read);
                  }
                },
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _LyricsSync extends StatefulWidget {
  const _LyricsSync({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  State<_LyricsSync> createState() => _LyricsSyncState();
}

class _LyricsSyncState extends State<_LyricsSync> {
  final ScrollController _scroll = ScrollController();
  List<GlobalKey> _keys = const [];

  SongPracticeDetailViewModel get vm => widget.vm;

  @override
  void initState() {
    super.initState();
    _keys = List.generate(vm.lyrics.length, (_) => GlobalKey());
  }

  @override
  void didUpdateWidget(_LyricsSync old) {
    super.didUpdateWidget(old);
    if (vm.lyrics.length != _keys.length) {
      _keys = List.generate(vm.lyrics.length, (_) => GlobalKey());
    }
    WidgetsBinding.instance.addPostFrameCallback((_) => _scrollToTarget());
  }

  void _scrollToTarget() {
    final i = vm.syncIndex;
    if (i >= 0 && i < _keys.length) {
      final ctx = _keys[i].currentContext;
      if (ctx != null) {
        Scrollable.ensureVisible(
          ctx,
          alignment: 0.5,
          duration: const Duration(milliseconds: 200),
        );
      }
    }
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  static String _fmt(int ms) {
    final s = ms ~/ 1000;
    return '${(s ~/ 60).toString().padLeft(2, '0')}:'
        '${(s % 60).toString().padLeft(2, '0')}.'
        '${((ms % 1000) ~/ 10).toString().padLeft(2, '0')}';
  }

  @override
  Widget build(BuildContext context) {
    final lines = vm.lyrics;
    return Focus(
      autofocus: true,
      onKeyEvent: (node, e) {
        if (e is KeyDownEvent && e.logicalKey == LogicalKeyboardKey.space) {
          vm.stampNextLyric();
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: Column(
        children: [
          Container(
            width: double.infinity,
            padding: const EdgeInsets.fromLTRB(14, 10, 14, 10),
            color: Studio.accent.withValues(alpha: 0.06),
            child: const Text(
              'Play, then press Space (or Tap) on each line as you reach it. '
              'Click a line to re-time it.',
              style: TextStyle(
                color: Studio.textSecondary,
                fontSize: 11,
                height: 1.4,
              ),
            ),
          ),
          Expanded(
            child: ListView.builder(
              controller: _scroll,
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 10),
              itemCount: lines.length,
              itemBuilder: (context, i) {
                final l = lines[i];
                if (l.isBlank) return SizedBox(height: 10, key: _keys[i]);
                final target = i == vm.syncIndex;
                return Container(
                  key: _keys[i],
                  margin: const EdgeInsets.symmetric(vertical: 2),
                  padding: const EdgeInsets.symmetric(
                    horizontal: 8,
                    vertical: 6,
                  ),
                  decoration: BoxDecoration(
                    color: target
                        ? Studio.accent.withValues(alpha: 0.12)
                        : Colors.transparent,
                    borderRadius: BorderRadius.circular(6),
                    border: Border.all(
                      color: target
                          ? Studio.accent.withValues(alpha: 0.5)
                          : Colors.transparent,
                    ),
                  ),
                  child: MouseRegion(
                    cursor: SystemMouseCursors.click,
                    child: GestureDetector(
                      onTap: () => vm.stampLyricAt(i),
                      child: Row(
                        children: [
                          SizedBox(
                            width: 52,
                            child: Text(
                              l.isSynced ? _fmt(l.timeMs!) : '—',
                              style: TextStyle(
                                color: l.isSynced
                                    ? Studio.accent
                                    : Studio.textFaint,
                                fontSize: 11,
                                fontWeight: FontWeight.w600,
                                fontFeatures: const [
                                  FontFeature.tabularFigures(),
                                ],
                              ),
                            ),
                          ),
                          Expanded(
                            child: Text(
                              l.text,
                              maxLines: 2,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                color: target
                                    ? Studio.textPrimary
                                    : Studio.textSecondary,
                                fontSize: 13,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                );
              },
            ),
          ),
          const Divider(height: 1, color: Studio.border),
          Padding(
            padding: const EdgeInsets.all(10),
            child: Row(
              children: [
                StudioButton(
                  label: 'Clear',
                  variant: StudioButtonVariant.danger,
                  onTap: vm.clearLyricTimes,
                ),
                const Spacer(),
                StudioButton(
                  label: 'Tap (Space)',
                  icon: FIcons.target,
                  variant: StudioButtonVariant.accent,
                  onTap: vm.stampNextLyric,
                ),
                const SizedBox(width: 8),
                StudioButton(
                  label: 'Done',
                  onTap: () => vm.setLyricsMode(LyricsMode.read),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// Full-window large-text karaoke for hands-free singing.
class LyricsFocusOverlay extends StatelessWidget {
  const LyricsFocusOverlay({required this.vm, super.key});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: Studio.background.withValues(alpha: 0.97),
      child: Stack(
        children: [
          Positioned.fill(child: _LyricsReader(vm: vm, large: true)),
          Positioned(
            top: 14,
            right: 16,
            child: _IconBtn(
              FIcons.minimize2,
              'Exit focus',
              () => vm.setLyricsFocus(false),
            ),
          ),
        ],
      ),
    );
  }
}
