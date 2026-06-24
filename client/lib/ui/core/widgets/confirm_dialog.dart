import 'package:flutter/material.dart';

import '../themes/studio.dart';
import 'studio_button.dart';

/// A studio-themed confirm dialog (dark card, coral accent button) — used where
/// the forui adaptive dialog's light primary button looks out of place. The
/// title ellipsizes so a long song name can't blow up the dialog.
Future<bool> showStudioConfirm(
  BuildContext context, {
  required String title,
  String? body,
  String confirmLabel = 'Confirm',
  String cancelLabel = 'Cancel',
  IconData? icon,
}) async {
  final result = await showDialog<bool>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (ctx) => Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 400,
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
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (icon != null) ...[
                      Container(
                        width: 30,
                        height: 30,
                        alignment: Alignment.center,
                        decoration: BoxDecoration(
                          color: Studio.accent.withValues(alpha: 0.14),
                          borderRadius: BorderRadius.circular(8),
                        ),
                        child: Icon(icon, size: 15, color: Studio.accent),
                      ),
                      const SizedBox(width: 12),
                    ],
                    Expanded(
                      child: Text(
                        title,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          color: Studio.textPrimary,
                          fontSize: 15,
                          fontWeight: FontWeight.w700,
                          height: 1.3,
                        ),
                      ),
                    ),
                  ],
                ),
                if (body != null) ...[
                  const SizedBox(height: 12),
                  Text(
                    body,
                    style: const TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 13,
                      height: 1.5,
                    ),
                  ),
                ],
                const SizedBox(height: 22),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    StudioButton(
                      label: cancelLabel,
                      onTap: () => Navigator.of(ctx).pop(false),
                    ),
                    const SizedBox(width: 10),
                    StudioButton(
                      label: confirmLabel,
                      variant: StudioButtonVariant.accent,
                      onTap: () => Navigator.of(ctx).pop(true),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    ),
  );
  return result ?? false;
}

/// A studio-themed single-line text prompt (dark card, coral primary button) —
/// the dark-theme replacement for the forui adaptive dialog. Returns the trimmed
/// text, or null on cancel / empty.
Future<String?> showStudioPrompt(
  BuildContext context, {
  required String title,
  String label = '',
  String hint = '',
  String? initial,
  String confirmLabel = 'Create',
  String cancelLabel = 'Cancel',
}) {
  return showDialog<String>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (ctx) => _StudioPrompt(
      title: title,
      label: label,
      hint: hint,
      initial: initial,
      confirmLabel: confirmLabel,
      cancelLabel: cancelLabel,
    ),
  );
}

/// The prompt body as a StatefulWidget so its [TextEditingController] is owned by the dialog
/// route's element and disposed only when that element unmounts — i.e. AFTER the dismiss
/// transition. Disposing it inline right after `showDialog` returned let the still-animating
/// TextField touch a dead controller ("TextEditingController used after being disposed").
class _StudioPrompt extends StatefulWidget {
  const _StudioPrompt({
    required this.title,
    required this.label,
    required this.hint,
    required this.initial,
    required this.confirmLabel,
    required this.cancelLabel,
  });

  final String title;
  final String label;
  final String hint;
  final String? initial;
  final String confirmLabel;
  final String cancelLabel;

  @override
  State<_StudioPrompt> createState() => _StudioPromptState();
}

class _StudioPromptState extends State<_StudioPrompt> {
  late final TextEditingController _controller =
      TextEditingController(text: widget.initial ?? '');

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _submit() {
    final t = _controller.text.trim();
    Navigator.of(context).pop(t.isEmpty ? null : t);
  }

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 400,
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
                Text(
                  widget.title,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 15,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(height: 16),
                if (widget.label.isNotEmpty) ...[
                  Text(
                    widget.label.toUpperCase(),
                    style: const TextStyle(
                      color: Studio.textFaint,
                      fontSize: 10.5,
                      fontWeight: FontWeight.w700,
                      letterSpacing: 0.6,
                    ),
                  ),
                  const SizedBox(height: 6),
                ],
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
                    onSubmitted: (_) => _submit(),
                    cursorColor: Studio.accent,
                    style: const TextStyle(
                      color: Studio.textPrimary,
                      fontSize: 14,
                    ),
                    decoration: InputDecoration(
                      isCollapsed: true,
                      border: InputBorder.none,
                      hintText: widget.hint,
                      hintStyle: const TextStyle(
                        color: Studio.textFaint,
                        fontSize: 14,
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: 22),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    StudioButton(
                      label: widget.cancelLabel,
                      onTap: () => Navigator.of(context).pop(),
                    ),
                    const SizedBox(width: 10),
                    StudioButton(
                      label: widget.confirmLabel,
                      variant: StudioButtonVariant.accent,
                      onTap: _submit,
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

/// A reusable confirm/cancel modal in the dark studio style. Returns true if the
/// user confirmed.
Future<bool> showConfirmDialog(
  BuildContext context, {
  required String title,
  String? body,
  String confirmLabel = 'Delete',
  String cancelLabel = 'Cancel',
  bool destructive = true,
}) async {
  final result = await showDialog<bool>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (ctx) => Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 400,
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
                Text(
                  title,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 15,
                    fontWeight: FontWeight.w700,
                    height: 1.3,
                  ),
                ),
                if (body != null) ...[
                  const SizedBox(height: 12),
                  Text(
                    body,
                    style: const TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 13,
                      height: 1.5,
                    ),
                  ),
                ],
                const SizedBox(height: 22),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    StudioButton(
                      label: cancelLabel,
                      onTap: () => Navigator.of(ctx).pop(false),
                    ),
                    const SizedBox(width: 10),
                    StudioButton(
                      label: confirmLabel,
                      variant: destructive
                          ? StudioButtonVariant.danger
                          : StudioButtonVariant.accent,
                      onTap: () => Navigator.of(ctx).pop(true),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    ),
  );
  return result ?? false;
}
