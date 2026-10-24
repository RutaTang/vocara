import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/practice_summary.dart';
import '../../core/themes/studio.dart';

/// A song-practice tile on the home grid. Tapping opens the workspace; the
/// trash button requests deletion (confirmation handled by the caller).
class PracticeProjectCard extends StatefulWidget {
  const PracticeProjectCard({
    required this.practice,
    required this.onOpen,
    required this.onDelete,
    super.key,
  });

  final PracticeSummary practice;
  final VoidCallback onOpen;
  final VoidCallback onDelete;

  @override
  State<PracticeProjectCard> createState() => _PracticeProjectCardState();
}

class _PracticeProjectCardState extends State<PracticeProjectCard> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final p = widget.practice;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onOpen,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          padding: const EdgeInsets.all(16),
          decoration: BoxDecoration(
            color: Studio.panel,
            borderRadius: BorderRadius.circular(10),
            border: Border.all(
              color: _hover
                  ? Studio.accent.withValues(alpha: 0.6)
                  : Studio.border,
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Container(
                    padding: const EdgeInsets.all(8),
                    decoration: BoxDecoration(
                      color: Studio.accent.withValues(alpha: 0.15),
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: const Icon(
                      FIcons.music,
                      size: 18,
                      color: Studio.accent,
                    ),
                  ),
                  const Spacer(),
                  if (_hover)
                    MouseRegion(
                      cursor: SystemMouseCursors.click,
                      child: GestureDetector(
                        onTap: widget.onDelete,
                        child: const Icon(
                          FIcons.trash2,
                          size: 16,
                          color: Studio.textFaint,
                        ),
                      ),
                    ),
                ],
              ),
              const Spacer(),
              Text(
                p.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  color: Studio.textPrimary,
                  fontSize: 16,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 4),
              Text(
                _subtitle(p),
                style: const TextStyle(
                  color: Studio.textSecondary,
                  fontSize: 12,
                ),
              ),
              const SizedBox(height: 2),
              Text(
                'Updated ${_formatDate(p.updatedAt)}',
                style: const TextStyle(color: Studio.textFaint, fontSize: 11),
              ),
            ],
          ),
        ),
      ),
    );
  }

  String _subtitle(PracticeSummary p) {
    final takes = p.takeCount;
    final takeLabel = takes == 1 ? '1 take' : '$takes takes';
    final accompaniment = p.hasAccompaniment ? 'backing · ' : '';
    return '$accompaniment$takeLabel · ${p.noteCount} notes';
  }

  String _formatDate(DateTime date) {
    String two(int n) => n.toString().padLeft(2, '0');
    return '${date.year}-${two(date.month)}-${two(date.day)} '
        '${two(date.hour)}:${two(date.minute)}';
  }
}
