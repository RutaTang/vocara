import 'package:flutter/material.dart';
import 'package:forui/forui.dart' show FIcons;

import '../../../domain/models/practice_note.dart';
import '../../../utils/format.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_button.dart';
import '../view_model/song_practice_detail_view_model.dart'
    show normalizeMarkerRange;

/// Outcome of the note editor: either saved [content], or [deleted].
class NoteEditorResult {
  const NoteEditorResult({this.content, this.deleted = false});

  final String? content;
  final bool deleted;
}

/// Shows the add/edit note modal — a dark studio card matching the app. When
/// [existing] is provided, offers a Delete action. [at] is the start the note is
/// (or will be) pinned to; [end] (when given) marks a range note's end so the
/// header reflects the span the user drew.
Future<NoteEditorResult?> showNoteEditor(
  BuildContext context, {
  PracticeNote? existing,
  required Duration at,
  Duration? end,
}) {
  // As it will be kept: a drag too short for a range is saved as a point,
  // and was announced as "Note across 00:12 → 00:12".
  final (positionMs, endMs) = normalizeMarkerRange(
    existing?.positionMs ?? at.inMilliseconds,
    existing?.endMs ?? end?.inMilliseconds,
  );
  final title = endMs != null
      ? 'Note across ${formatClockMs(positionMs)} → ${formatClockMs(endMs)}'
      : 'Note at ${formatClockMs(positionMs)}';

  return showDialog<NoteEditorResult>(
    context: context,
    barrierColor: Colors.black.withValues(alpha: 0.55),
    builder: (dialogContext) => _NoteDialog(
      initialText: existing?.content ?? '',
      title: title,
      editing: existing != null,
    ),
  );
}

/// Owns its [TextEditingController] so it is disposed in [State.dispose] — which
/// runs only after the dialog route's exit transition completes. Disposing the
/// controller earlier (right after `await showDialog`) lets the still-fading
/// dialog rebuild its `EditableText` over a disposed controller.
class _NoteDialog extends StatefulWidget {
  const _NoteDialog({
    required this.initialText,
    required this.title,
    required this.editing,
  });

  final String initialText;
  final String title;
  final bool editing;

  @override
  State<_NoteDialog> createState() => _NoteDialogState();
}

class _NoteDialogState extends State<_NoteDialog> {
  late final TextEditingController controller = TextEditingController(
    text: widget.initialText,
  );

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  void _save(BuildContext context) {
    final text = controller.text.trim();
    Navigator.of(
      context,
    ).pop(text.isEmpty ? null : NoteEditorResult(content: text));
  }

  @override
  Widget build(BuildContext context) {
    return Dialog(
      backgroundColor: Colors.transparent,
      elevation: 0,
      insetPadding: const EdgeInsets.all(24),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Container(
          padding: const EdgeInsets.all(20),
          decoration: BoxDecoration(
            color: Studio.surface,
            borderRadius: BorderRadius.circular(12),
            border: Border.all(color: Studio.border),
            boxShadow: const [
              BoxShadow(
                color: Color(0x99000000),
                blurRadius: 28,
                offset: Offset(0, 12),
              ),
            ],
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  const Icon(FIcons.stickyNote, size: 16, color: Studio.accent),
                  const SizedBox(width: 8),
                  Text(
                    widget.title,
                    style: const TextStyle(
                      color: Studio.textPrimary,
                      fontSize: 15,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                ],
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
                  controller: controller,
                  autofocus: true,
                  minLines: 3,
                  maxLines: 6,
                  cursorColor: Studio.accent,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 14,
                    height: 1.4,
                  ),
                  decoration: const InputDecoration(
                    isCollapsed: true,
                    border: InputBorder.none,
                    hintText:
                        'What happened here? (e.g. breath runs out, pitch flat)',
                    hintStyle: TextStyle(color: Studio.textFaint, fontSize: 14),
                  ),
                ),
              ),
              const SizedBox(height: 18),
              Row(
                children: [
                  if (widget.editing)
                    StudioButton(
                      label: 'Delete',
                      icon: FIcons.trash2,
                      variant: StudioButtonVariant.danger,
                      onTap: () => Navigator.of(
                        context,
                      ).pop(const NoteEditorResult(deleted: true)),
                    ),
                  const Spacer(),
                  StudioButton(
                    label: 'Cancel',
                    onTap: () => Navigator.of(context).pop(),
                  ),
                  const SizedBox(width: 8),
                  // Not with nothing to save: it closed as if it had saved,
                  // the note keeping its old text.
                  ValueListenableBuilder<TextEditingValue>(
                    valueListenable: controller,
                    builder: (context, value, _) => StudioButton(
                      label: widget.editing ? 'Save' : 'Add',
                      variant: StudioButtonVariant.accent,
                      enabled: value.text.trim().isNotEmpty,
                      onTap: () => _save(context),
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}
