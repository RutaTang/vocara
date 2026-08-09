import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../../domain/models/technique.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/confirm_dialog.dart';
import '../../core/widgets/studio_button.dart';
import '../view_model/technique_detail_view_model.dart';
import 'media/technique_media_section.dart';
import 'studio_field.dart';
import 'technique_analysis_section.dart';

/// A single technique's detail page: view or edit its fields and manage its
/// audio/video example clips.
///
/// The edit form's controllers are owned by this State (not a child widget) so
/// the Save/Cancel actions live in the top header. Media is managed in both
/// modes — adding a clip persists immediately through the view model, separate
/// from the buffered text form, so it never disturbs in-progress text edits.
class TechniqueDetailScreen extends StatefulWidget {
  const TechniqueDetailScreen({super.key});

  @override
  State<TechniqueDetailScreen> createState() => _TechniqueDetailScreenState();
}

class _TechniqueDetailScreenState extends State<TechniqueDetailScreen> {
  bool _editing = false;
  bool _editorDirty = false;

  // Edit-form controllers, non-null only while editing.
  TextEditingController? _name;
  TextEditingController? _aliases;
  TextEditingController? _tags;
  TextEditingController? _summary;
  TextEditingController? _description;
  TextEditingController? _cues;
  TextEditingController? _exercises;
  TextEditingController? _acoustic;

  /// The technique the form was seeded from (for dirty comparison).
  Technique? _editBase;

  List<TextEditingController> get _controllers => [
    _name!,
    _aliases!,
    _tags!,
    _summary!,
    _description!,
    _cues!,
    _exercises!,
    _acoustic!,
  ];

  void _seedForm(Technique t) {
    _name = TextEditingController(text: t.name);
    _aliases = TextEditingController(text: t.aliases.join(', '));
    _tags = TextEditingController(text: t.tags.join(', '));
    _summary = TextEditingController(text: t.summary);
    _description = TextEditingController(text: t.description);
    _cues = TextEditingController(text: t.cues);
    _exercises = TextEditingController(text: t.exercises);
    _acoustic = TextEditingController(text: t.acousticSignature);
    _editBase = t;
    _editorDirty = false;
    for (final c in _controllers) {
      c.addListener(_recomputeDirty);
    }
  }

  void _disposeForm() {
    if (_name == null) return;
    for (final c in _controllers) {
      c.dispose();
    }
    _name = _aliases = _tags = _summary = null;
    _description = _cues = _exercises = _acoustic = null;
    _editBase = null;
    _editorDirty = false;
    // The single point BOTH exits funnel through (cancel, successful save) and
    // also the screen's own dispose — so the VM's edit flag has exactly the
    // controllers' lifetime, which is the invariant it protects.
    _vm?.endEdit();
  }

  void _recomputeDirty() {
    final t = _editBase;
    if (t == null) return;
    final dirty =
        _name!.text != t.name ||
        _aliases!.text != t.aliases.join(', ') ||
        _tags!.text != t.tags.join(', ') ||
        _summary!.text != t.summary ||
        _description!.text != t.description ||
        _cues!.text != t.cues ||
        _exercises!.text != t.exercises ||
        _acoustic!.text != t.acousticSignature;
    if (dirty != _editorDirty) setState(() => _editorDirty = dirty);
  }

  bool get _isDirtyEditing => _editing && _editorDirty;

  /// The VM the edit session belongs to, held so [_disposeForm] can close that
  /// session from `dispose()` too, where a `context.read` is not allowed.
  TechniqueDetailViewModel? _vm;

  void _enterEdit(TechniqueDetailViewModel vm, Technique t) {
    _vm = vm;
    vm.beginMediaDraft();
    // Tell the VM the form is open BEFORE seeding it: from here until
    // _disposeForm, the base hashes must stay pinned to this snapshot, or a
    // remote change landing mid-edit disarms the save's keep-theirs branch.
    vm.beginEdit();
    _seedForm(t);
    setState(() => _editing = true);
  }

  Future<void> _cancelEdit(TechniqueDetailViewModel vm) async {
    // Dirty if text fields OR the staged example edits changed OR a recording is
    // in flight OR a just-stopped example is still being imported. savingExample
    // matters because stopExampleRecording clears isRecordingExample and only adds
    // the clip to the media draft AFTER its importMedia await — so during that
    // window all the other flags read false and Cancel would skip the discard
    // confirm, tear down the session, then strand the imported file + lose the
    // recording. Prompting keeps the session alive until the import lands (so
    // discardMediaDraft can clean it up) or the user keeps editing.
    final dirty =
        _editorDirty ||
        vm.hasMediaDraft ||
        vm.isRecordingExample ||
        vm.savingExample ||
        vm.starting ||
        // A drag-drop/browse import in flight: addMedia only folds the copied
        // files into the draft AFTER its awaits, so like savingExample the other
        // flags read false meanwhile and Cancel would skip the confirm + tear the
        // session down mid-copy.
        vm.importingMedia;
    if (dirty && !await _confirmDiscard()) return;
    await vm.discardMediaDraft();
    _disposeForm();
    if (mounted) setState(() => _editing = false);
  }

  Future<void> _save(TechniqueDetailViewModel vm) async {
    // Finalize any in-flight example recording first, so Save preserves it
    // (and releases the mic) instead of leaving it running.
    if (vm.isRecordingExample) await vm.stopExampleRecording();
    final base = vm.technique;
    if (base == null) return;
    final name = _name!.text.trim();
    // Build from the LIVE technique so any media added mid-edit is preserved.
    final built = base.copyWith(
      name: name.isEmpty ? base.name : name,
      aliases: parseCommaList(_aliases!.text),
      tags: parseCommaList(_tags!.text),
      summary: _summary!.text.trim(),
      description: _description!.text.trim(),
      cues: _cues!.text.trim(),
      exercises: _exercises!.text.trim(),
      acousticSignature: _acoustic!.text.trim(),
    );
    final ok = await vm.save(built);
    if (!mounted) return;
    if (!ok) {
      // Stay in edit so the staged draft (and the files copied for it) isn't
      // stranded; the user can retry. Leaving now would orphan those files.
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text("Couldn't save. Please try again.")),
      );
      return;
    }
    _disposeForm();
    setState(() => _editing = false);
  }

  @override
  void dispose() {
    _disposeForm();
    super.dispose();
  }

  Future<bool> _confirmDiscard() {
    return showConfirmDialog(
      context,
      title: 'Discard unsaved changes?',
      body:
          'Your edits to this technique have not been saved yet. '
          'This cannot be undone.',
      confirmLabel: 'Discard',
      cancelLabel: 'Keep editing',
    );
  }

  Future<void> _handleBack() async {
    if (_isDirtyEditing && !await _confirmDiscard()) return;
    if (mounted) context.pop();
  }

  Future<void> _delete(TechniqueDetailViewModel vm) async {
    final t = vm.technique;
    if (t == null) return;
    final confirmed = await showConfirmDialog(
      context,
      title: 'Delete "${t.name}"?',
      body:
          'This removes the technique and its example clips. '
          'This cannot be undone.',
    );
    if (!confirmed) return;
    final ok = await vm.delete();
    if (ok && mounted) context.pop();
  }

  Future<void> _removeMedia(TechniqueDetailViewModel vm, String mediaId) async {
    final confirmed = await showConfirmDialog(
      context,
      title: 'Remove this example?',
      body:
          "It's removed from this technique when you save. Cancel the edit to "
          'keep it.',
      confirmLabel: 'Remove',
    );
    if (!confirmed) return;
    await vm.removeMedia(mediaId);
  }

  @override
  Widget build(BuildContext context) {
    final vm = context.watch<TechniqueDetailViewModel>();
    final technique = vm.technique;

    // Surface a one-shot async recording notice (e.g. the input device was lost)
    // after the frame, then clear it so it shows exactly once.
    if (vm.recordingNotice != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        final notice = vm.recordingNotice;
        if (notice == null || !mounted) return;
        vm.clearRecordingNotice();
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text(notice)));
      });
    }

    return Scaffold(
      backgroundColor: Studio.background,
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Header(
            title: _editing
                ? 'Edit technique'
                : (technique?.name ?? 'Technique'),
            editing: _editing,
            canEdit: technique != null,
            onBack: _handleBack,
            onEdit: technique == null ? null : () => _enterEdit(vm, technique),
            onDelete: () => _delete(vm),
            onCancel: () => _cancelEdit(vm),
            onSave: () => _save(vm),
            saveBlocked: vm.saving || vm.savingExample || vm.importingMedia,
            cancelBlocked: vm.saving,
          ),
          Expanded(child: _content(vm)),
        ],
      ),
    );
  }

  Widget _content(TechniqueDetailViewModel vm) {
    if (vm.load.running && vm.technique == null) {
      return const Center(
        child: CircularProgressIndicator(
          valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
        ),
      );
    }
    final t = vm.technique;
    if (t == null) {
      return const Center(
        child: Text(
          'This technique could not be loaded.',
          style: TextStyle(color: Studio.textSecondary, fontSize: 13),
        ),
      );
    }

    if (_editing && _name != null) {
      return _EditForm(
        name: _name!,
        aliases: _aliases!,
        tags: _tags!,
        summary: _summary!,
        description: _description!,
        cues: _cues!,
        exercises: _exercises!,
        acoustic: _acoustic!,
        // Freeze the whole edit surface while the save commits: the commit
        // snapshotted the form and the media draft when it started, so text
        // or media edits made during the await would be silently thrown away
        // when the save lands and the screen drops out of edit mode.
        enabled: !vm.saving,
        // Editable: add/drop zone + record + per-clip controls. Shows the
        // staged draft (added/removed/renamed examples not yet saved).
        mediaSection: TechniqueMediaSection(
          media: vm.effectiveMedia,
          importing: vm.importingMedia,
          enabled: !vm.saving,
          onAddPaths: vm.addMedia,
          onRemove: (id) => _removeMedia(vm, id),
          onRename: vm.renameMedia,
          onStartRecord: vm.startExampleRecording,
          onStopRecord: vm.stopExampleRecording,
          isRecording: vm.isRecordingExample,
          savingRecording: vm.savingExample,
          canRecord: vm.canRecordExample,
          recordLevel: vm.recordLevel,
          analyzedPath: vm.referenceMedia?.path,
          onPlaybackPosition: vm.setPlayheadFromPlayback,
          analysisScrub: vm.analysisScrub,
          expandedPath: vm.expandedExamplePath,
          onToggleExpand: vm.toggleExampleExpanded,
          liveMonitor: vm.liveMonitor,
          // Editing builds the examples — analysis shown, no practice recorder.
          analysisBuilder: () =>
              TechniqueExampleAnalysis(vm: vm, allowPractice: false),
        ),
      );
    }

    return _TechniqueViewer(
      key: ValueKey('view-${t.id}'),
      technique: t,
      // Read view: clips are play-only; the block still shows its label and an
      // "Empty content" placeholder when there are none.
      mediaSection: TechniqueMediaSection(
        media: t.media,
        importing: false,
        editable: false,
        onAddPaths: (_) {},
        onRemove: (_) {},
        onRename: (_, _) {},
        analyzedPath: vm.referenceMedia?.path,
        onPlaybackPosition: vm.setPlayheadFromPlayback,
        analysisScrub: vm.analysisScrub,
        expandedPath: vm.expandedExamplePath,
        onToggleExpand: vm.toggleExampleExpanded,
        liveMonitor: vm.liveMonitor,
        // Read view: each example pairs with its analysis + a practice recorder.
        analysisBuilder: () => TechniqueExampleAnalysis(vm: vm),
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.title,
    required this.editing,
    required this.canEdit,
    required this.onBack,
    required this.onEdit,
    required this.onDelete,
    required this.onCancel,
    required this.onSave,
    required this.saveBlocked,
    required this.cancelBlocked,
  });

  final String title;
  final bool editing;
  final bool canEdit;
  final VoidCallback onBack;
  final VoidCallback? onEdit;
  final VoidCallback onDelete;
  final VoidCallback onCancel;
  final VoidCallback onSave;

  /// While true Save is disabled because a media import is mid-flight (a just-
  /// stopped example still importing, OR a drag-drop/browse import copying) — the
  /// clip isn't in the draft yet, so saving now would persist the technique
  /// without it and orphan the copied file — OR the technique save itself is
  /// in flight (a double-click would run two commits against the same stale
  /// base hashes).
  final bool saveBlocked;

  /// While true Cancel is disabled: the save is committing, and Cancel →
  /// Discard would delete the staged media files the airborne commit is
  /// persisting rows for (committed rows pointing at deleted files). It also
  /// keeps the edit form alive until the save resolves either way.
  final bool cancelBlocked;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 56,
      padding: const EdgeInsets.only(left: 12, right: 16),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          // The back chevron only exists in view mode; editing is exited through
          // Cancel/Save, so there's no "back" affordance to confuse it.
          if (!editing) ...[
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: onBack,
                child: Container(
                  width: 32,
                  height: 32,
                  alignment: Alignment.center,
                  child: const Icon(
                    FIcons.chevronLeft,
                    color: Studio.textSecondary,
                    size: 20,
                  ),
                ),
              ),
            ),
            const SizedBox(width: 6),
          ] else
            const SizedBox(width: 8),
          // Expanded (not Flexible + Spacer) so the title fills the middle and
          // the action buttons sit flush against the right edge.
          Expanded(
            child: Text(
              title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 18,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.3,
              ),
            ),
          ),
          const SizedBox(width: 12),
          if (editing) ...[
            StudioButton(
              label: 'Cancel',
              enabled: !cancelBlocked,
              onTap: onCancel,
            ),
            const SizedBox(width: 8),
            StudioButton(
              icon: FIcons.check,
              label: 'Save',
              variant: StudioButtonVariant.accent,
              enabled: !saveBlocked,
              onTap: onSave,
            ),
          ] else if (canEdit) ...[
            StudioButton(
              icon: FIcons.pencil,
              label: 'Edit',
              onTap: onEdit ?? () {},
            ),
            const SizedBox(width: 8),
            StudioButton(
              icon: FIcons.trash2,
              label: 'Delete',
              variant: StudioButtonVariant.danger,
              onTap: onDelete,
            ),
          ],
        ],
      ),
    );
  }
}

/// Read-only view of a technique: the same fields as the edit form, in the same
/// order, but display only. Every field is shown; empty ones read "Empty
/// content". Editing happens in place via the header's Edit button.
class _TechniqueViewer extends StatelessWidget {
  const _TechniqueViewer({
    required this.technique,
    required this.mediaSection,
    super.key,
  });

  final Technique technique;

  /// The Examples block: each example card pairs its player with a collapsible
  /// analysis + practice recorder.
  final Widget mediaSection;

  @override
  Widget build(BuildContext context) {
    final t = technique;
    return ListView(
      padding: const EdgeInsets.fromLTRB(28, 22, 28, 28),
      children: [
        _ViewField(
          label: 'Aliases',
          child: t.aliases.isEmpty
              ? null
              : _ChipRow(labels: t.aliases, tint: false),
        ),
        const SizedBox(height: 20),
        _ViewField(
          label: 'Tags',
          child: t.tags.isEmpty ? null : _ChipRow(labels: t.tags, tint: true),
        ),
        const SizedBox(height: 20),
        mediaSection,
        const SizedBox(height: 24),
        _ViewField(label: 'Summary', text: t.summary),
        const SizedBox(height: 20),
        _ViewField(label: 'Description', text: t.description),
        const SizedBox(height: 20),
        _ViewField(label: 'How to produce it', text: t.cues),
        const SizedBox(height: 20),
        _ViewField(label: 'Exercises', text: t.exercises),
        const SizedBox(height: 20),
        _ViewField(
          label: 'Acoustic signature in Vocara',
          text: t.acousticSignature,
          accent: true,
        ),
      ],
    );
  }
}

/// A labeled read-only field. Shows [child] (e.g. chips) or [text]; when both
/// are absent/blank it shows a muted "Empty content" placeholder so every field
/// stays visible.
class _ViewField extends StatelessWidget {
  const _ViewField({
    required this.label,
    this.text,
    this.child,
    this.accent = false,
  });

  final String label;
  final String? text;
  final Widget? child;
  final bool accent;

  @override
  Widget build(BuildContext context) {
    Widget body;
    if (child != null) {
      body = child!;
    } else if (text != null && text!.trim().isNotEmpty) {
      body = Text(
        text!,
        style: const TextStyle(
          color: Studio.textPrimary,
          fontSize: 14,
          height: 1.5,
        ),
      );
    } else {
      body = const Text(
        'Empty content',
        style: TextStyle(
          color: Studio.textFaint,
          fontSize: 13,
          fontStyle: FontStyle.italic,
        ),
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          label.toUpperCase(),
          style: TextStyle(
            color: accent ? Studio.accentBright : Studio.textFaint,
            fontSize: 10.5,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.6,
          ),
        ),
        const SizedBox(height: 7),
        body,
      ],
    );
  }
}

/// The edit form: the labeled text fields, then the examples block at the
/// bottom. The screen owns the controllers; this widget is presentation only.
class _EditForm extends StatelessWidget {
  const _EditForm({
    required this.name,
    required this.aliases,
    required this.tags,
    required this.summary,
    required this.description,
    required this.cues,
    required this.exercises,
    required this.acoustic,
    required this.mediaSection,
    required this.enabled,
  });

  final TextEditingController name;
  final TextEditingController aliases;
  final TextEditingController tags;
  final TextEditingController summary;
  final TextEditingController description;
  final TextEditingController cues;
  final TextEditingController exercises;
  final TextEditingController acoustic;

  /// False while the save is committing: every field is frozen so edits typed
  /// during the await can't be silently discarded when edit mode closes.
  final bool enabled;

  /// The Examples block: each example card pairs its player with a collapsible
  /// analysis (so a just-recorded example's analysis is visible immediately).
  final Widget mediaSection;

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.fromLTRB(28, 20, 28, 28),
      children: [
        StudioField(
          enabled: enabled,
          controller: name,
          label: 'Name',
          hint: 'e.g. Twang',
          autofocus: true,
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: aliases,
          label: 'Aliases (comma separated)',
          hint: 'e.g. Edge, Brassy quality',
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: tags,
          label: 'Tags (comma separated)',
          hint: 'e.g. EVT, Voice quality, Resonance',
        ),
        const SizedBox(height: 22),
        const Divider(height: 1, thickness: 1, color: Studio.border),
        const SizedBox(height: 20),
        // Examples sit right after the identity fields so they're visible
        // without scrolling past the long prose fields.
        mediaSection,
        const SizedBox(height: 22),
        const Divider(height: 1, thickness: 1, color: Studio.border),
        const SizedBox(height: 20),
        StudioField(
          enabled: enabled,
          controller: summary,
          label: 'Summary',
          hint: 'One line shown on the card',
          minLines: 1,
          maxLines: 2,
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: description,
          label: 'Description',
          hint: 'What the technique is',
          minLines: 3,
          maxLines: 8,
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: cues,
          label: 'How to produce it',
          hint: 'Cues, sensations, steps',
          minLines: 3,
          maxLines: 8,
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: exercises,
          label: 'Exercises',
          hint: 'Drills to practise it',
          minLines: 2,
          maxLines: 6,
        ),
        const SizedBox(height: 16),
        StudioField(
          enabled: enabled,
          controller: acoustic,
          label: 'Acoustic signature in Vocara',
          hint: 'How it reads in RING, tilt, CPP, formants',
          minLines: 2,
          maxLines: 6,
        ),
      ],
    );
  }
}

class _ChipRow extends StatelessWidget {
  const _ChipRow({required this.labels, required this.tint});

  final List<String> labels;
  final bool tint;

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
        for (final label in labels)
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
            decoration: BoxDecoration(
              color: tint
                  ? Studio.accent.withValues(alpha: 0.12)
                  : Studio.surfaceRaised,
              borderRadius: BorderRadius.circular(6),
              border: Border.all(
                color: tint
                    ? Studio.accent.withValues(alpha: 0.3)
                    : Studio.border,
              ),
            ),
            child: Text(
              label,
              style: TextStyle(
                color: tint ? Studio.accentBright : Studio.textSecondary,
                fontSize: 11.5,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
      ],
    );
  }
}
