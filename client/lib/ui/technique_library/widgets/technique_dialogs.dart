import 'package:flutter/material.dart';

import '../../core/themes/studio.dart';
import '../../core/widgets/studio_button.dart';
import 'studio_field.dart';

/// Name-first create dialog for a new technique. Returns the trimmed name, or
/// null if cancelled / left blank. Tags and the rest of the body are filled in
/// the detail editor afterward.
Future<String?> showNewTechniqueDialog(BuildContext context) {
  return showDialog<String>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (dialogContext) => const _NewTechniqueDialog(),
  );
}

/// Owns its [TextEditingController] so it is disposed in [State.dispose] — which
/// runs only after the dialog route's exit transition completes. Disposing right
/// after `await showDialog` would free it mid-fade-out, letting the dialog
/// rebuild its `EditableText` over a disposed controller (a use-after-dispose
/// assert under test pumping).
class _NewTechniqueDialog extends StatefulWidget {
  const _NewTechniqueDialog();

  @override
  State<_NewTechniqueDialog> createState() => _NewTechniqueDialogState();
}

class _NewTechniqueDialogState extends State<_NewTechniqueDialog> {
  final TextEditingController _name = TextEditingController();

  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  // Pops the dialog with the dialog's own context, so it is go_router safe.
  void _create() {
    final text = _name.text.trim();
    Navigator.of(context).pop(text.isEmpty ? null : text);
  }

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 440,
            padding: const EdgeInsets.all(22),
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
                const Text(
                  'New technique',
                  style: TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 16,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(height: 16),
                StudioField(
                  controller: _name,
                  label: 'Name',
                  hint: 'e.g. Twang, Mixed voice, Curbing',
                  autofocus: true,
                ),
                const SizedBox(height: 20),
                Row(
                  children: [
                    const Spacer(),
                    StudioButton(
                      label: 'Cancel',
                      onTap: () => Navigator.of(context).pop(),
                    ),
                    const SizedBox(width: 8),
                    StudioButton(
                      label: 'Create',
                      variant: StudioButtonVariant.accent,
                      onTap: _create,
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
