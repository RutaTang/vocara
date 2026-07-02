import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../core/themes/studio.dart';
import '../../core/widgets/confirm_dialog.dart';

/// Callbacks for the track right-click menu. Vocal-only actions are hidden for
/// the accompaniment; [canMergeDown] hides "Merge" when there is no lane below.
class TrackMenuActions {
  const TrackMenuActions({
    required this.isVocal,
    required this.canMergeDown,
    required this.canSeparate,
    required this.reseparate,
    required this.currentColorValue,
    required this.onPickColor,
    required this.onRename,
    required this.onSeparate,
    required this.onAddNote,
    required this.onAddTechnique,
    required this.onDuplicate,
    required this.onSplit,
    required this.onMergeDown,
    required this.onReanalyze,
    required this.onDelete,
  });

  final bool isVocal;
  final bool canMergeDown;

  /// Whether to offer "Separate into backing + vocal" (only for a full-song
  /// backing/reference track).
  final bool canSeparate;

  /// When true the backing was already separated, so the action re-separates
  /// the kept original (different label).
  final bool reseparate;

  /// The track's current custom tint (ARGB), or null when it uses the default.
  final int? currentColorValue;

  /// Picks a lane tint.
  final ValueChanged<int> onPickColor;

  final VoidCallback onRename;
  final VoidCallback onSeparate;
  final VoidCallback onAddNote;
  final VoidCallback onAddTechnique;
  final VoidCallback onDuplicate;
  final VoidCallback onSplit;
  final VoidCallback onMergeDown;
  final VoidCallback onReanalyze;
  final VoidCallback onDelete;
}

/// The preset lane tints (ARGB) — 16 colors, two rows of eight, tuned to read
/// well on the dark studio panels as a 3 px bar + waveform fill.
const List<int> kTrackColorPalette = [
  0xFFE3604E, // red
  0xFFE2934A, // orange
  0xFFD9B44A, // amber
  0xFFB7C44A, // lime
  0xFF6FB562, // green
  0xFF49B6A8, // teal
  0xFF4AAEC4, // cyan
  0xFF5B8DEF, // blue
  0xFF6F7CE8, // indigo
  0xFF8C7CE8, // violet
  0xFFB06FE0, // purple
  0xFFD46BA8, // pink
  0xFFD46172, // rose
  0xFFB57C5A, // tan
  0xFF8A93A8, // slate
  0xFF7FB0A0, // sage
];

/// A stable default tint for a vocal take with no custom color: picked from
/// [kTrackColorPalette] by a deterministic hash of the take id, so a take keeps
/// the same color across sessions and takes read as distinct (vs a flat gray).
Color defaultTakeColor(String id) {
  if (kTrackColorPalette.isEmpty) return Studio.waveform;
  final h = id.codeUnits.fold<int>(0, (sum, c) => sum + c);
  return Color(kTrackColorPalette[h % kTrackColorPalette.length]);
}

/// A brighter version of a lane's base color — used for the *border* of a
/// selected clip (a gentle brightness lift in the same hue, not pure white).
Color selectedLaneColor(Color base) {
  final hsl = HSLColor.fromColor(base);
  return hsl
      .withLightness((hsl.lightness + 0.14).clamp(0.0, 1.0))
      .withSaturation((hsl.saturation + 0.05).clamp(0.0, 1.0))
      .toColor();
}

const double _kMenuWidth = 268;

/// Shows the dark studio context menu at [globalPosition] — instantly (no
/// expand animation) — and runs the chosen action.
Future<void> showTrackContextMenu(
  BuildContext context,
  Offset globalPosition,
  TrackMenuActions actions,
) {
  return _showStudioMenu(
    context,
    globalPosition,
    (close) => [
      _ColorGrid(
        current: actions.currentColorValue,
        onPick: (value) {
          actions.onPickColor(value);
          close();
        },
      ),
      const _MenuDivider(),
      _MenuRow(FIcons.pencil, 'Rename', () {
        close();
        actions.onRename();
      }),
      if (actions.canSeparate)
        _MenuRow(
          FIcons.wandSparkles,
          actions.reseparate ? 'Re-extract backing' : 'Extract backing',
          () {
            close();
            actions.onSeparate();
          },
        ),
      // Notes and techniques annotate vocal takes only — the backing
      // (accompaniment/original) track has no use for them.
      if (actions.isVocal)
        _MenuRow(FIcons.stickyNote, 'Add note at playhead', () {
          close();
          actions.onAddNote();
        }),
      if (actions.isVocal)
        _MenuRow(FIcons.waypoints, 'Add technique at playhead', () {
          close();
          actions.onAddTechnique();
        }),
      if (actions.isVocal)
        _MenuRow(FIcons.copy, 'Duplicate', () {
          close();
          actions.onDuplicate();
        }),
      if (actions.isVocal)
        _MenuRow(FIcons.scissors, 'Split at playhead', () {
          close();
          actions.onSplit();
        }),
      if (actions.isVocal && actions.canMergeDown)
        _MenuRow(FIcons.combine, 'Merge with take below', () {
          close();
          actions.onMergeDown();
        }),
      _MenuRow(FIcons.rotateCw, 'Re-analyze waveform', () {
        close();
        actions.onReanalyze();
      }),
      const _MenuDivider(),
      _MenuRow(FIcons.trash2, actions.isVocal ? 'Delete take' : 'Delete', () {
        close();
        actions.onDelete();
      }, danger: true),
    ],
  );
}

/// Callbacks for the FX-bus right-click menu — a slimmer cousin of
/// [TrackMenuActions]: a bus has no notes, clips, split/merge or re-analyze, so
/// it offers just color, rename, duplicate and delete.
class BusMenuActions {
  const BusMenuActions({
    required this.currentColorValue,
    required this.onPickColor,
    required this.onRename,
    required this.onDuplicate,
    required this.onDelete,
  });

  final int? currentColorValue;
  final ValueChanged<int> onPickColor;
  final VoidCallback onRename;
  final VoidCallback onDuplicate;
  final VoidCallback onDelete;
}

/// Shows the FX-bus context menu at [globalPosition], styled like the track
/// menu, and runs the chosen action.
Future<void> showBusContextMenu(
  BuildContext context,
  Offset globalPosition,
  BusMenuActions actions,
) {
  return _showStudioMenu(
    context,
    globalPosition,
    (close) => [
      _ColorGrid(
        current: actions.currentColorValue,
        onPick: (value) {
          actions.onPickColor(value);
          close();
        },
      ),
      const _MenuDivider(),
      _MenuRow(FIcons.pencil, 'Rename', () {
        close();
        actions.onRename();
      }),
      _MenuRow(FIcons.copy, 'Duplicate', () {
        close();
        actions.onDuplicate();
      }),
      const _MenuDivider(),
      _MenuRow(FIcons.trash2, 'Delete', () {
        close();
        actions.onDelete();
      }, danger: true),
    ],
  );
}

/// A minimal context menu for a single clip — right now just "Delete clip".
Future<void> showClipContextMenu(
  BuildContext context,
  Offset globalPosition, {
  required VoidCallback onDelete,
}) {
  return _showStudioMenu(
    context,
    globalPosition,
    (close) => [
      _MenuRow(FIcons.trash2, 'Delete clip', () {
        close();
        onDelete();
      }, danger: true),
    ],
  );
}

/// One entry in a [showStudioMenu] popup.
class StudioMenuItem {
  const StudioMenuItem({
    required this.label,
    required this.onTap,
    this.icon,
    this.subtitle,
    this.selected = false,
    this.danger = false,
    this.divider = false,
  });

  final String label;
  final VoidCallback onTap;
  final IconData? icon;

  /// Optional one-line description shown faintly under the label.
  final String? subtitle;
  final bool selected;
  final bool danger;

  /// Renders a divider above this item.
  final bool divider;
}

/// Shows an instant (no-animation) dark studio menu of [items] at [position] —
/// used by the in-app dropdowns so every menu opens the same way.
Future<void> showStudioMenu(
  BuildContext context,
  Offset position,
  List<StudioMenuItem> items, {
  double width = 200,
}) {
  return _showStudioMenu(
    context,
    position,
    (close) => [
      for (final it in items) ...[
        if (it.divider) const _MenuDivider(),
        _MenuRow(
          it.icon,
          it.label,
          () {
            close();
            it.onTap();
          },
          danger: it.danger,
          selected: it.selected,
          subtitle: it.subtitle,
        ),
      ],
    ],
    width: width,
  );
}

/// The global top-left of a widget [context], for anchoring a dropdown menu
/// just below it.
Offset menuAnchorBelow(BuildContext context, {double dy = 4}) {
  final box = context.findRenderObject() as RenderBox?;
  if (box == null) return Offset.zero;
  return box.localToGlobal(Offset(0, box.size.height + dy));
}

/// Pops a dark studio menu instantly (zero transition) at [globalPosition],
/// dismissible by tapping outside. [build] returns the menu's rows; calling the
/// passed `close` callback closes the menu.
Future<void> _showStudioMenu(
  BuildContext context,
  Offset globalPosition,
  List<Widget> Function(VoidCallback close) build, {
  double width = _kMenuWidth,
}) {
  return Navigator.of(context).push(
    _InstantMenuRoute(
      position: globalPosition,
      builder: (context, close) => Material(
        color: Studio.surfaceRaised,
        elevation: 10,
        borderRadius: BorderRadius.circular(10),
        child: Container(
          width: width,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(10),
            border: Border.all(color: Studio.border),
          ),
          clipBehavior: Clip.antiAlias,
          // Cap the menu height; a long item list scrolls inside it instead of
          // running the full height of the viewport.
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxHeight: 440),
            child: ScrollConfiguration(
              behavior: ScrollConfiguration.of(context).copyWith(
                scrollbars: false,
                physics: const ClampingScrollPhysics(),
              ),
              child: SingleChildScrollView(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: build(close),
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

/// A [PopupRoute] with no transition — the menu just appears, anchored at the
/// click and clamped to stay on screen.
class _InstantMenuRoute extends PopupRoute<void> {
  _InstantMenuRoute({required this.position, required this.builder});

  final Offset position;
  final Widget Function(BuildContext, VoidCallback close) builder;

  @override
  Color? get barrierColor => null;
  @override
  bool get barrierDismissible => true;
  @override
  String get barrierLabel => 'Dismiss menu';
  @override
  Duration get transitionDuration => Duration.zero;

  @override
  Widget buildPage(
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
  ) {
    return CustomSingleChildLayout(
      delegate: _MenuLayoutDelegate(position),
      child: builder(context, () => Navigator.of(context).pop()),
    );
  }
}

class _MenuLayoutDelegate extends SingleChildLayoutDelegate {
  _MenuLayoutDelegate(this.position);
  final Offset position;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) =>
      BoxConstraints.loose(
        constraints.biggest,
      ).deflate(const EdgeInsets.all(8));

  @override
  Offset getPositionForChild(Size size, Size childSize) {
    var x = position.dx;
    var y = position.dy;
    if (x + childSize.width > size.width - 8) {
      x = size.width - childSize.width - 8;
    }
    if (y + childSize.height > size.height - 8) {
      y = size.height - childSize.height - 8;
    }
    return Offset(x.clamp(8, size.width), y.clamp(8, size.height));
  }

  @override
  bool shouldRelayout(_MenuLayoutDelegate old) => old.position != position;
}

class _MenuRow extends StatelessWidget {
  const _MenuRow(
    this.icon,
    this.label,
    this.onTap, {
    this.danger = false,
    this.selected = false,
    this.subtitle,
  });

  final IconData? icon;
  final String label;
  final VoidCallback onTap;
  final bool danger;
  final bool selected;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    final color = danger
        ? Studio.record
        : selected
        ? Studio.accent
        : Studio.textPrimary;
    return InkWell(
      onTap: onTap,
      hoverColor: Studio.hover,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            if (icon != null) ...[
              Icon(icon, size: 15, color: color),
              const SizedBox(width: 12),
            ],
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(
                    label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(color: color, fontSize: 13),
                  ),
                  if (subtitle != null) ...[
                    const SizedBox(height: 2),
                    Text(
                      subtitle!,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        color: Studio.textFaint,
                        fontSize: 10.5,
                        height: 1.3,
                      ),
                    ),
                  ],
                ],
              ),
            ),
            if (selected)
              const Icon(FIcons.check, size: 14, color: Studio.accent),
          ],
        ),
      ),
    );
  }
}

class _MenuDivider extends StatelessWidget {
  const _MenuDivider();
  @override
  Widget build(BuildContext context) =>
      const Divider(height: 9, thickness: 1, color: Studio.border);
}

/// The inline lane-color picker: two rows of eight preset swatches.
class _ColorGrid extends StatelessWidget {
  const _ColorGrid({required this.current, required this.onPick});

  final int? current;
  final ValueChanged<int> onPick;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Padding(
            padding: EdgeInsets.only(left: 1, bottom: 8),
            child: Text(
              'COLOR',
              style: TextStyle(
                color: Studio.textFaint,
                fontSize: 9,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
          ),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              for (final value in kTrackColorPalette)
                _ColorSwatch(
                  color: Color(value),
                  selected: current == value,
                  onTap: () => onPick(value),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

class _ColorSwatch extends StatelessWidget {
  const _ColorSwatch({
    required this.color,
    required this.selected,
    required this.onTap,
  });

  final Color color;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Container(
          width: 22,
          height: 22,
          decoration: BoxDecoration(
            color: color,
            shape: BoxShape.circle,
            border: Border.all(
              color: selected ? Studio.textPrimary : Studio.border,
              width: selected ? 2 : 1,
            ),
          ),
        ),
      ),
    );
  }
}

/// Modal text dialog used to name/rename something — dark studio styling.
/// Delegates to [showStudioPrompt], whose [TextEditingController] is owned by
/// the dialog's own State and disposed only after the dismiss transition —
/// disposing a local controller right after `showDialog` returns let the
/// still-animating TextField touch a dead controller (debug crash).
Future<String?> showRenameDialog(
  BuildContext context, {
  required String current,
  String title = 'Rename track',
  String hint = 'Track name',
  String confirmLabel = 'Rename',
}) {
  return showStudioPrompt(
    context,
    title: title,
    hint: hint,
    initial: current,
    confirmLabel: confirmLabel,
  );
}

/// A dark, rounded studio modal shell: title, body and a right-aligned action
/// row — shared by the rename + confirm dialogs.
class _StudioDialog extends StatelessWidget {
  const _StudioDialog({
    required this.title,
    required this.body,
    required this.actions,
    this.message,
  });

  final String title;
  final String? message;
  final Widget? body;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 420),
        child: Material(
          color: Studio.surfaceRaised,
          elevation: 16,
          borderRadius: BorderRadius.circular(14),
          child: Container(
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(14),
              border: Border.all(color: Studio.border),
            ),
            padding: const EdgeInsets.fromLTRB(22, 20, 22, 18),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  title,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 16,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                if (message != null) ...[
                  const SizedBox(height: 8),
                  Text(
                    message!,
                    style: const TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 13,
                      height: 1.4,
                    ),
                  ),
                ],
                if (body != null) ...[const SizedBox(height: 16), body!],
                const SizedBox(height: 20),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    for (final a in actions) ...[a, const SizedBox(width: 10)],
                  ]..removeLast(),
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
    this.primary = false,
    this.danger = false,
  });

  final String label;
  final VoidCallback onTap;
  final bool primary;
  final bool danger;

  @override
  Widget build(BuildContext context) {
    final fill = danger
        ? Studio.record
        : primary
        ? Studio.accent
        : Colors.transparent;
    final fg = (primary || danger) ? Colors.white : Studio.textPrimary;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 18, vertical: 9),
          decoration: BoxDecoration(
            color: fill,
            borderRadius: BorderRadius.circular(8),
            border: Border.all(
              color: (primary || danger) ? Colors.transparent : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: fg,
              fontSize: 13.5,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

/// A dark studio confirm dialog (used for Delete and other yes/no prompts).
/// Returns true if confirmed.
Future<bool> showStudioConfirmDialog(
  BuildContext context, {
  required String title,
  required String message,
  String confirmLabel = 'Confirm',
  String cancelLabel = 'Cancel',
  bool danger = false,
}) async {
  final ok = await showDialog<bool>(
    context: context,
    barrierColor: Colors.black54,
    builder: (dialogContext) => _StudioDialog(
      title: title,
      message: message,
      body: null,
      actions: [
        _DialogButton(
          label: cancelLabel,
          onTap: () => Navigator.of(dialogContext).pop(false),
        ),
        _DialogButton(
          label: confirmLabel,
          primary: !danger,
          danger: danger,
          onTap: () => Navigator.of(dialogContext).pop(true),
        ),
      ],
    ),
  );
  return ok ?? false;
}
