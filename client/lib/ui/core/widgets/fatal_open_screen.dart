import 'package:flutter/widgets.dart';

import '../themes/studio.dart';

/// The minimal window shown when Persistence.open fails fatally at launch.
///
/// Deliberately tiny and dependency-light: no Material, no forui, no router,
/// no providers — when the library cannot open, this screen must still be
/// able to render. main() wraps it in [FatalOpenApp] and then reveals the
/// native window (which stays transparent until the show call, so without
/// this a fatal open would look like the app simply never launched).
class FatalOpenScreen extends StatelessWidget {
  const FatalOpenScreen({
    required this.title,
    required this.message,
    required this.onQuit,
    this.detail,
    this.onRevealLibrary,
    super.key,
  });

  /// The library on disk was written by a newer Vocara: refuse cleanly and
  /// tell the user the one action that helps. No detail and no folder button;
  /// the data is fine, only the app is too old.
  const FatalOpenScreen.newerLibrary({required VoidCallback onQuit, Key? key})
    : this(
        title: 'This library was created by a newer version of Vocara.',
        message: 'Please update Vocara to open it.',
        onQuit: onQuit,
        key: key,
      );

  /// Any other fatal open failure: show the error and offer the library
  /// folder (for backups or support) plus quit.
  const FatalOpenScreen.openFailed({
    required String detail,
    required VoidCallback onRevealLibrary,
    required VoidCallback onQuit,
    Key? key,
  }) : this(
         title: 'Vocara could not open its library.',
         message:
             'Quit and reopen Vocara to try again. If this keeps happening, '
             'the details below can help.',
         detail: detail,
         onRevealLibrary: onRevealLibrary,
         onQuit: onQuit,
         key: key,
       );

  final String title;
  final String message;

  /// Raw error detail, shown in an inset box when present.
  final String? detail;

  /// Opens the library folder in Finder; null hides the button.
  final VoidCallback? onRevealLibrary;

  final VoidCallback onQuit;

  @override
  Widget build(BuildContext context) {
    return Directionality(
      textDirection: TextDirection.ltr,
      child: ColoredBox(
        color: Studio.background,
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 460),
            child: Padding(
              padding: const EdgeInsets.all(32),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Text(
                    title,
                    textAlign: TextAlign.center,
                    style: const TextStyle(
                      color: Studio.textPrimary,
                      fontSize: 15,
                      fontWeight: FontWeight.w600,
                      decoration: TextDecoration.none,
                    ),
                  ),
                  const SizedBox(height: 10),
                  Text(
                    message,
                    textAlign: TextAlign.center,
                    style: const TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 13,
                      fontWeight: FontWeight.w400,
                      height: 1.4,
                      decoration: TextDecoration.none,
                    ),
                  ),
                  if (detail != null) ...[
                    const SizedBox(height: 16),
                    DecoratedBox(
                      decoration: BoxDecoration(
                        color: Studio.inset,
                        borderRadius: BorderRadius.circular(6),
                        border: Border.all(color: Studio.border),
                      ),
                      child: ConstrainedBox(
                        constraints: const BoxConstraints(maxHeight: 140),
                        child: SingleChildScrollView(
                          padding: const EdgeInsets.all(12),
                          child: Text(
                            detail!,
                            style: const TextStyle(
                              color: Studio.textSecondary,
                              fontSize: 11,
                              height: 1.5,
                              decoration: TextDecoration.none,
                            ),
                          ),
                        ),
                      ),
                    ),
                  ],
                  const SizedBox(height: 24),
                  // Wrap, not Row: the window can be resized narrow, and the
                  // actions must stay visible at any width.
                  Wrap(
                    alignment: WrapAlignment.center,
                    spacing: 12,
                    runSpacing: 8,
                    children: [
                      if (onRevealLibrary != null)
                        _FatalButton(
                          label: 'Reveal Library Folder',
                          onPressed: onRevealLibrary!,
                        ),
                      _FatalButton(
                        label: 'Quit Vocara',
                        primary: true,
                        onPressed: onQuit,
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
  }
}

/// The root wrapper main() runs a [FatalOpenScreen] with: a bare [WidgetsApp]
/// (title + text defaults, no navigator) so the screen renders standalone.
class FatalOpenApp extends StatelessWidget {
  const FatalOpenApp({required this.screen, super.key});

  final FatalOpenScreen screen;

  @override
  Widget build(BuildContext context) {
    return WidgetsApp(
      title: 'Vocara',
      color: Studio.background,
      debugShowCheckedModeBanner: false,
      builder: (context, child) => screen,
    );
  }
}

/// A plain non-Material button (hover + click cursor), enough for the two
/// fatal-window actions without pulling in a widget framework.
class _FatalButton extends StatefulWidget {
  const _FatalButton({
    required this.label,
    required this.onPressed,
    this.primary = false,
  });

  final String label;
  final VoidCallback onPressed;
  final bool primary;

  @override
  State<_FatalButton> createState() => _FatalButtonState();
}

class _FatalButtonState extends State<_FatalButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    // Same restrained palette as StudioButton's accent/subtle variants
    // (not imported: that widget is Material-based and this screen must
    // stay framework-free).
    final background = widget.primary
        ? Studio.accent.withValues(alpha: _hovered ? 0.24 : 0.16)
        : (_hovered ? Studio.hover : Studio.surfaceRaised);
    final border = widget.primary
        ? Studio.accent.withValues(alpha: _hovered ? 0.7 : 0.45)
        : (_hovered ? Studio.accent.withValues(alpha: 0.55) : Studio.border);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: widget.onPressed,
        child: DecoratedBox(
          decoration: BoxDecoration(
            color: background,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: border),
          ),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 7),
            child: Text(
              widget.label,
              style: TextStyle(
                color: widget.primary
                    ? Studio.accentBright
                    : Studio.textPrimary,
                fontSize: 12.5,
                fontWeight: FontWeight.w600,
                decoration: TextDecoration.none,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
