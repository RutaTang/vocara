import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/technique.dart';
import '../../core/themes/studio.dart';

/// Opens a picker to choose a technique from the user's library, to drop on a
/// take's technique track. Returns the chosen [Technique], or null if cancelled.
/// [techniques] is loaded lazily (e.g. from the repository); [title] reflects
/// where it lands (a playhead point or a drawn range).
Future<Technique?> showTechniquePicker(
  BuildContext context,
  Future<List<Technique>?> techniques, {
  String title = 'Add technique at playhead',
}) {
  return showDialog<Technique>(
    context: context,
    barrierColor: const Color(0xB0000000),
    builder: (ctx) => _TechniquePickerDialog(load: techniques, title: title),
  );
}

class _TechniquePickerDialog extends StatefulWidget {
  const _TechniquePickerDialog({required this.load, required this.title});

  /// The library, or null when it could not be read.
  final Future<List<Technique>?> load;
  final String title;

  @override
  State<_TechniquePickerDialog> createState() => _TechniquePickerDialogState();
}

class _TechniquePickerDialogState extends State<_TechniquePickerDialog> {
  final _searchCtrl = TextEditingController();
  String _query = '';
  List<Technique>? _all;

  /// The library could not be read: said so, not "no techniques yet".
  bool _failed = false;

  @override
  void initState() {
    super.initState();
    widget.load.then((list) {
      if (!mounted) return;
      setState(() {
        _all = list ?? const [];
        _failed = list == null;
      });
    });
  }

  @override
  void dispose() {
    _searchCtrl.dispose();
    super.dispose();
  }

  List<Technique> get _filtered {
    final all = _all ?? const <Technique>[];
    final q = _query.trim().toLowerCase();
    if (q.isEmpty) return all;
    bool matches(Technique t) {
      if (t.name.toLowerCase().contains(q)) return true;
      for (final a in t.aliases) {
        if (a.toLowerCase().contains(q)) return true;
      }
      for (final tag in t.tags) {
        if (tag.toLowerCase().contains(q)) return true;
      }
      return false;
    }

    return all.where(matches).toList();
  }

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Material(
          color: Colors.transparent,
          child: Container(
            width: 420,
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
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(20, 18, 20, 12),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        widget.title,
                        style: const TextStyle(
                          color: Studio.textPrimary,
                          fontSize: 16,
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const SizedBox(height: 12),
                      _SearchField(
                        controller: _searchCtrl,
                        onChanged: (v) => setState(() => _query = v),
                      ),
                    ],
                  ),
                ),
                const Divider(height: 1, thickness: 1, color: Studio.border),
                Flexible(child: _body()),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _body() {
    if (_all == null) {
      return const SizedBox(
        height: 120,
        child: Center(
          child: SizedBox(
            width: 20,
            height: 20,
            child: CircularProgressIndicator(
              strokeWidth: 2,
              valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
            ),
          ),
        ),
      );
    }
    if (_failed) {
      return const _Hint(
        "Couldn't open the technique library. Try again in a moment.",
      );
    }
    if (_all!.isEmpty) {
      return const _Hint(
        'No techniques yet. Add some in the technique library first.',
      );
    }
    final filtered = _filtered;
    if (filtered.isEmpty) {
      return const _Hint('No techniques match.');
    }
    return ConstrainedBox(
      constraints: const BoxConstraints(maxHeight: 360),
      child: ListView.builder(
        padding: const EdgeInsets.symmetric(vertical: 6),
        shrinkWrap: true,
        itemCount: filtered.length,
        itemBuilder: (context, index) {
          final t = filtered[index];
          return _TechniqueRow(
            technique: t,
            onTap: () => Navigator.of(context).pop(t),
          );
        },
      ),
    );
  }
}

class _SearchField extends StatelessWidget {
  const _SearchField({required this.controller, required this.onChanged});

  final TextEditingController controller;
  final ValueChanged<String> onChanged;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 34,
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.border),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 10),
      child: Row(
        children: [
          const Icon(FIcons.search, size: 14, color: Studio.textFaint),
          const SizedBox(width: 8),
          Expanded(
            child: TextField(
              controller: controller,
              autofocus: true,
              onChanged: onChanged,
              cursorColor: Studio.accent,
              style: const TextStyle(color: Studio.textPrimary, fontSize: 13),
              decoration: const InputDecoration(
                isCollapsed: true,
                border: InputBorder.none,
                hintText: 'Search techniques',
                hintStyle: TextStyle(color: Studio.textFaint, fontSize: 13),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _TechniqueRow extends StatefulWidget {
  const _TechniqueRow({required this.technique, required this.onTap});

  final Technique technique;
  final VoidCallback onTap;

  @override
  State<_TechniqueRow> createState() => _TechniqueRowState();
}

class _TechniqueRowState extends State<_TechniqueRow> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final t = widget.technique;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          color: _hover ? Studio.hover : Colors.transparent,
          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 9),
          child: Row(
            children: [
              const Icon(FIcons.waypoints, size: 13, color: Studio.accent),
              const SizedBox(width: 10),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      t.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        color: Studio.textPrimary,
                        fontSize: 13.5,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    if (t.tags.isNotEmpty) ...[
                      const SizedBox(height: 2),
                      Text(
                        t.tags.join(' · '),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          color: Studio.textFaint,
                          fontSize: 11,
                        ),
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Hint extends StatelessWidget {
  const _Hint(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 28),
      child: Text(
        text,
        textAlign: TextAlign.center,
        style: const TextStyle(
          color: Studio.textFaint,
          fontSize: 12.5,
          height: 1.4,
        ),
      ),
    );
  }
}
