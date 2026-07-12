import 'dart:async';
import 'dart:io' show Process;

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../data/services/update_check_service.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_button.dart';

/// Opens [url] in the default browser via `/usr/bin/open` (the same pattern
/// main() uses to reveal the library folder in Finder; no url_launcher
/// dependency). Returns false when the launch fails — the caller's dialog
/// keeps the selectable URL on screen as the fallback.
Future<bool> openUrlInBrowser(String url) async {
  try {
    final result = await Process.run('open', [url]);
    return result.exitCode == 0;
  } catch (_) {
    return false;
  }
}

/// The dismissible "a newer Vocara exists" banner under the home header.
/// Renders nothing until [UpdateCheckService.available] is set and disappears
/// for the rest of the run once dismissed (Settings still shows the update).
class UpdateNoticeBanner extends StatelessWidget {
  const UpdateNoticeBanner({required this.updates, this.openUrl, super.key});

  final UpdateCheckService updates;

  /// Test seam: replaces [openUrlInBrowser] for the Download dialog.
  final Future<bool> Function(String url)? openUrl;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: updates,
      builder: (context, _) {
        final info = updates.available;
        if (info == null || updates.noticeDismissed) {
          return const SizedBox.shrink();
        }
        return Container(
          padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 10),
          decoration: BoxDecoration(
            color: Studio.accent.withValues(alpha: 0.10),
            border: const Border(bottom: BorderSide(color: Studio.border)),
          ),
          child: Row(
            children: [
              const Icon(FIcons.circleArrowUp, size: 16, color: Studio.accent),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  'Vocara ${info.version} is available.',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              const SizedBox(width: 10),
              StudioButton(
                icon: FIcons.download,
                label: 'Download',
                variant: StudioButtonVariant.accent,
                onTap: () => unawaited(
                  showUpdateDownloadDialog(context, info, openUrl: openUrl),
                ),
              ),
              const SizedBox(width: 8),
              _DismissButton(onTap: updates.dismissNotice),
            ],
          ),
        );
      },
    );
  }
}

/// The download dialog for a newer release: version, release notes, and the
/// release page URL as selectable text (so it stays reachable even when
/// opening the browser fails), plus an "Open Download Page" button.
Future<void> showUpdateDownloadDialog(
  BuildContext context,
  UpdateInfo info, {
  Future<bool> Function(String url)? openUrl,
}) {
  final open = openUrl ?? openUrlInBrowser;
  return showDialog<void>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (ctx) => Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 480,
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
                  'Vocara ${info.version} is available',
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 15,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                if (info.notes.isNotEmpty) ...[
                  const SizedBox(height: 12),
                  ConstrainedBox(
                    constraints: const BoxConstraints(maxHeight: 200),
                    child: SingleChildScrollView(
                      child: Text(
                        info.notes,
                        style: const TextStyle(
                          color: Studio.textSecondary,
                          fontSize: 12.5,
                          height: 1.5,
                        ),
                      ),
                    ),
                  ),
                ],
                const SizedBox(height: 14),
                // Selectable so the user can copy the address by hand if the
                // browser cannot be opened.
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 10,
                  ),
                  decoration: BoxDecoration(
                    color: Studio.inset,
                    borderRadius: BorderRadius.circular(8),
                    border: Border.all(color: Studio.border),
                  ),
                  child: SelectableText(
                    info.url,
                    style: const TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 12,
                    ),
                  ),
                ),
                const SizedBox(height: 22),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    StudioButton(
                      label: 'Later',
                      onTap: () => Navigator.of(ctx).pop(),
                    ),
                    const SizedBox(width: 10),
                    StudioButton(
                      icon: FIcons.download,
                      label: 'Open Download Page',
                      variant: StudioButtonVariant.accent,
                      // The dialog stays open on purpose: if the browser
                      // launch fails the selectable URL above is the fallback.
                      onTap: () => unawaited(open(info.url)),
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

class _DismissButton extends StatelessWidget {
  const _DismissButton({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: 'Dismiss',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Container(
            width: 28,
            height: 28,
            alignment: Alignment.center,
            child: const Icon(FIcons.x, size: 15, color: Studio.textSecondary),
          ),
        ),
      ),
    );
  }
}
