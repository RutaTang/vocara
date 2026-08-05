import 'package:flutter/material.dart';

import '../../core/themes/studio.dart';

/// A labeled, studio-styled text field used across the technique editor and its
/// dialogs: a small caption over a dark inset box wrapping a borderless
/// [TextField]. Single line by default; pass [minLines]/[maxLines] for prose.
class StudioField extends StatelessWidget {
  const StudioField({
    required this.controller,
    required this.label,
    this.hint,
    this.minLines = 1,
    this.maxLines = 1,
    this.autofocus = false,
    this.enabled = true,
    super.key,
  });

  final TextEditingController controller;
  final String label;
  final String? hint;
  final int minLines;
  final int maxLines;
  final bool autofocus;

  /// False freezes the field (e.g. while the technique save is committing —
  /// edits typed during the await would be thrown away when the save lands
  /// and the screen leaves edit mode). Rendered like a disabled StudioButton:
  /// still visible, text dimmed to the faint tone, no edit cursor.
  final bool enabled;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          label.toUpperCase(),
          style: const TextStyle(
            color: Studio.textFaint,
            fontSize: 10.5,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.6,
          ),
        ),
        const SizedBox(height: 6),
        Container(
          decoration: BoxDecoration(
            color: Studio.inset,
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: Studio.border),
          ),
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
          child: TextField(
            controller: controller,
            enabled: enabled,
            autofocus: autofocus,
            minLines: minLines,
            maxLines: maxLines,
            cursorColor: Studio.accent,
            style: TextStyle(
              color: enabled ? Studio.textPrimary : Studio.textFaint,
              fontSize: 14,
              height: 1.45,
            ),
            decoration: InputDecoration(
              isCollapsed: true,
              border: InputBorder.none,
              hintText: hint,
              hintStyle: const TextStyle(color: Studio.textFaint, fontSize: 14),
            ),
          ),
        ),
      ],
    );
  }
}

/// Parses a comma-separated input ("Twang, Edge") into a trimmed, de-duplicated
/// list, dropping blanks. Used for the aliases and tags fields.
List<String> parseCommaList(String raw) {
  final seen = <String>{};
  final out = <String>[];
  for (final part in raw.split(',')) {
    final t = part.trim();
    if (t.isEmpty || !seen.add(t.toLowerCase())) continue;
    out.add(t);
  }
  return out;
}
