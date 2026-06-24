import 'package:flutter/material.dart';

import '../themes/studio.dart';

enum StudioButtonVariant {
  /// Neutral toolbar button: dark surface, coral icon, light text.
  subtle,

  /// Tinted accent button: coral-tinted surface, coral border + label.
  accent,

  /// Destructive: muted red-tinted surface, red border + label.
  danger,
}

/// A restrained, DAW-style button — flat, dark, lightly bordered, with a coral
/// accent — instead of a loud saturated pill.
class StudioButton extends StatefulWidget {
  const StudioButton({
    required this.label,
    required this.onTap,
    this.icon,
    this.variant = StudioButtonVariant.subtle,
    this.expand = false,
    this.enabled = true,
    super.key,
  });

  final String label;
  final VoidCallback onTap;
  final IconData? icon;
  final StudioButtonVariant variant;
  final bool expand;
  final bool enabled;

  @override
  State<StudioButton> createState() => _StudioButtonState();
}

class _StudioButtonState extends State<StudioButton> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final accent = widget.variant == StudioButtonVariant.accent;
    final danger = widget.variant == StudioButtonVariant.danger;
    final hover = _hover && widget.enabled;

    Color bg;
    Color border;
    Color fg;
    Color iconColor;

    if (!widget.enabled) {
      bg = Studio.surfaceRaised;
      border = Studio.border;
      fg = Studio.textFaint;
      iconColor = Studio.textFaint;
    } else if (accent) {
      bg = Studio.accent.withValues(alpha: hover ? 0.24 : 0.16);
      border = Studio.accent.withValues(alpha: hover ? 0.7 : 0.45);
      fg = Studio.accentBright;
      iconColor = Studio.accentBright;
    } else if (danger) {
      bg = Studio.record.withValues(alpha: hover ? 0.22 : 0.14);
      border = Studio.record.withValues(alpha: hover ? 0.7 : 0.45);
      fg = Studio.record;
      iconColor = Studio.record;
    } else {
      bg = hover ? Studio.hover : Studio.surfaceRaised;
      border = hover ? Studio.accent.withValues(alpha: 0.55) : Studio.border;
      fg = Studio.textPrimary;
      iconColor = Studio.accent;
    }

    final row = Row(
      mainAxisSize: widget.expand ? MainAxisSize.max : MainAxisSize.min,
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        if (widget.icon != null) ...[
          Icon(widget.icon, size: 15, color: iconColor),
          const SizedBox(width: 7),
        ],
        Text(
          widget.label,
          style: TextStyle(
            color: fg,
            fontSize: 13,
            fontWeight: FontWeight.w600,
          ),
        ),
      ],
    );

    return MouseRegion(
      cursor: widget.enabled
          ? SystemMouseCursors.click
          : SystemMouseCursors.basic,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.enabled ? widget.onTap : null,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          height: 34,
          padding: const EdgeInsets.symmetric(horizontal: 14),
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: bg,
            borderRadius: BorderRadius.circular(7),
            border: Border.all(color: border),
          ),
          child: row,
        ),
      ),
    );
  }
}
