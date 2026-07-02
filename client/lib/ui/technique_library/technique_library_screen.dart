import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../domain/models/technique.dart';
import '../../routing/routes.dart';
import '../../utils/result.dart';
import '../core/themes/studio.dart';
import '../core/widgets/confirm_dialog.dart';
import '../core/widgets/studio_button.dart';
import 'view_model/technique_library_view_model.dart';
import 'widgets/technique_dialogs.dart';

/// The technique library home: a flat pool of vocal techniques browsed by a tag
/// filter plus search. Framework agnostic — tags (including framework tags like
/// EVT/CVT) are the only organizing axis, and a technique can carry many.
class TechniqueLibraryScreen extends StatefulWidget {
  const TechniqueLibraryScreen({super.key});

  @override
  State<TechniqueLibraryScreen> createState() => _TechniqueLibraryScreenState();
}

class _TechniqueLibraryScreenState extends State<TechniqueLibraryScreen> {
  // Guards against a double-tap pushing the detail route twice (two stacked
  // identical screens the user must back out of twice).
  bool _opening = false;

  @override
  Widget build(BuildContext context) {
    final vm = context.read<TechniqueLibraryViewModel>();

    return Scaffold(
      backgroundColor: Studio.background,
      body: ListenableBuilder(
        listenable: Listenable.merge([vm, vm.load]),
        builder: (context, _) => Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _Header(vm: vm, onNew: () => _createTechnique(vm)),
            Expanded(
              child: _Body(
                vm: vm,
                onOpen: (t) => _openDetail(vm, t.id),
                onDelete: (t) => _deleteTechnique(vm, t),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _openDetail(TechniqueLibraryViewModel vm, String id) async {
    if (_opening) {
      return; // ignore a rapid second tap while one open is in flight
    }
    _opening = true;
    try {
      await context.push(Routes.techniqueDetail(id));
      // Edits/deletes happen on the detail page; refresh on return.
      if (mounted) await vm.load.execute();
    } finally {
      _opening = false;
    }
  }

  Future<void> _createTechnique(TechniqueLibraryViewModel vm) async {
    final name = await showNewTechniqueDialog(context);
    if (name == null || !mounted) return;
    await vm.createTechnique.execute(name);
    if (!mounted) return;
    final result = vm.createTechnique.result;
    if (result is Ok<Technique>) {
      await _openDetail(vm, result.value.id);
    } else {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text("Couldn't create the technique.")),
      );
    }
  }

  Future<void> _deleteTechnique(
    TechniqueLibraryViewModel vm,
    Technique technique,
  ) async {
    final confirmed = await showConfirmDialog(
      context,
      title: 'Delete "${technique.name}"?',
      body:
          'This removes the technique and its example clips. '
          'This cannot be undone.',
    );
    if (!confirmed || !mounted) return;
    await vm.deleteTechnique.execute(technique.id);
    if (vm.deleteTechnique.result is! Ok<void> && mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text("Couldn't delete the technique.")),
      );
    }
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.vm, required this.onNew});

  final TechniqueLibraryViewModel vm;
  final VoidCallback onNew;

  @override
  Widget build(BuildContext context) {
    // Search + tag chips live in the header (left of New technique) and only
    // appear once there's something to filter.
    final showFilter = vm.all.isNotEmpty;
    return Container(
      height: 56,
      padding: const EdgeInsets.only(left: 12, right: 16),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => context.pop(),
              child: Container(
                width: 32,
                height: 32,
                alignment: Alignment.center,
                child: const Icon(
                  FIcons.chevronLeft,
                  color: Studio.textSecondary,
                  size: 20,
                ),
              ),
            ),
          ),
          const SizedBox(width: 6),
          const Text(
            'Technique library',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 18,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.3,
            ),
          ),
          // Search + tag chips form one cluster that right-anchors against New
          // technique. It lives in the only flexible child, so it soaks up all
          // the slack on the left (pushing the cluster + New technique flush
          // right) and scrolls (reversed, staying right-anchored) when there are
          // more tags than fit.
          // The Spacer absorbs the slack so the search + tag filter + New
          // technique cluster sits flush right. The tag filter is a dropdown
          // (not inline chips) so it stays compact no matter how many tags.
          const Spacer(),
          if (showFilter) ...[
            SizedBox(
              width: 240,
              child: _SearchBox(value: vm.query, onChanged: vm.setQuery),
            ),
            const SizedBox(width: 10),
            _TagFilterButton(vm: vm),
            const SizedBox(width: 12),
          ],
          StudioButton(
            icon: FIcons.plus,
            label: 'New technique',
            variant: StudioButtonVariant.accent,
            onTap: onNew,
          ),
        ],
      ),
    );
  }
}

/// The tag filter: a compact dropdown button showing the active filter, which
/// opens an anchored, scrollable menu of all tags. Scales to any number of tags
/// without widening the header.
class _TagFilterButton extends StatelessWidget {
  const _TagFilterButton({required this.vm});

  final TechniqueLibraryViewModel vm;

  String get _label {
    final tag = vm.activeTag;
    if (tag == null) return 'All tags';
    if (tag == kUntaggedFilter) return 'Untagged';
    return tag;
  }

  void _open(BuildContext context) {
    final box = context.findRenderObject() as RenderBox?;
    // Anchor relative to the navigator overlay, not the whole window: the menu
    // is painted inside the overlay (which sits below the window drag bar), so
    // raw global coordinates would push it down by the drag bar's height.
    final overlay =
        Navigator.of(context).overlay?.context.findRenderObject() as RenderBox?;
    if (box == null || overlay == null) return;
    final topLeft = box.localToGlobal(Offset.zero, ancestor: overlay);
    Navigator.of(context).push(
      _TagMenuRoute(
        anchorRight: topLeft.dx + box.size.width,
        top: topLeft.dy + box.size.height + 6,
        vm: vm,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final active = vm.activeTag != null;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => _open(context),
        child: Container(
          height: 34,
          padding: const EdgeInsets.only(left: 11, right: 8),
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: active
                ? Studio.accent.withValues(alpha: 0.16)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(8),
            border: Border.all(
              color: active
                  ? Studio.accent.withValues(alpha: 0.55)
                  : Studio.border,
            ),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                FIcons.tag,
                size: 13,
                color: active ? Studio.accentBright : Studio.textSecondary,
              ),
              const SizedBox(width: 7),
              ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 150),
                child: Text(
                  _label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    color: active ? Studio.accentBright : Studio.textPrimary,
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              const SizedBox(width: 6),
              const Icon(
                FIcons.chevronDown,
                size: 14,
                color: Studio.textSecondary,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The anchored, dismissible dropdown route for the tag filter. Its menu
/// right-aligns under the filter button and scrolls when there are many tags.
class _TagMenuRoute extends PopupRoute<void> {
  _TagMenuRoute({
    required this.anchorRight,
    required this.top,
    required this.vm,
  });

  final double anchorRight;
  final double top;
  final TechniqueLibraryViewModel vm;

  @override
  Color? get barrierColor => null;
  @override
  bool get barrierDismissible => true;
  @override
  String get barrierLabel => 'Dismiss tag filter';
  @override
  Duration get transitionDuration => Duration.zero;

  @override
  Widget buildPage(
    BuildContext context,
    Animation<double> animation,
    Animation<double> secondaryAnimation,
  ) {
    final facets = vm.tagFacets;
    return CustomSingleChildLayout(
      delegate: _TagMenuDelegate(anchorRight: anchorRight, top: top),
      child: Material(
        color: Studio.surfaceRaised,
        elevation: 10,
        borderRadius: BorderRadius.circular(10),
        child: Container(
          width: 240,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(10),
            border: Border.all(color: Studio.border),
          ),
          clipBehavior: Clip.antiAlias,
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
                  children: [
                    _TagMenuItem(
                      label: 'All tags',
                      count: vm.all.length,
                      active: vm.activeTag == null,
                      onTap: () {
                        vm.setTag(null);
                        Navigator.of(context).pop();
                      },
                    ),
                    if (vm.untaggedCount > 0)
                      _TagMenuItem(
                        label: 'Untagged',
                        count: vm.untaggedCount,
                        active: vm.activeTag == kUntaggedFilter,
                        onTap: () {
                          vm.setTag(kUntaggedFilter);
                          Navigator.of(context).pop();
                        },
                      ),
                    if (facets.isNotEmpty)
                      const Divider(
                        height: 9,
                        thickness: 1,
                        color: Studio.border,
                      ),
                    for (final f in facets)
                      _TagMenuItem(
                        label: f.tag,
                        count: f.count,
                        active: vm.isTagActive(f.tag),
                        onTap: () {
                          vm.setTag(f.tag);
                          Navigator.of(context).pop();
                        },
                      ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _TagMenuDelegate extends SingleChildLayoutDelegate {
  _TagMenuDelegate({required this.anchorRight, required this.top});

  final double anchorRight;
  final double top;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) =>
      BoxConstraints.loose(
        constraints.biggest,
      ).deflate(const EdgeInsets.all(8));

  @override
  Offset getPositionForChild(Size size, Size childSize) {
    var x = anchorRight - childSize.width; // right-align under the button
    var y = top;
    if (x + childSize.width > size.width - 8) {
      x = size.width - childSize.width - 8;
    }
    if (y + childSize.height > size.height - 8) {
      y = size.height - childSize.height - 8;
    }
    return Offset(x.clamp(8, size.width), y.clamp(8, size.height));
  }

  @override
  bool shouldRelayout(_TagMenuDelegate old) =>
      old.anchorRight != anchorRight || old.top != top;
}

/// One row in the tag menu, styled like the app's other studio menus
/// (full-bleed [InkWell] hover). The active filter is shown by its accent label
/// color; the count stays pinned to the right so every row's count lines up.
class _TagMenuItem extends StatelessWidget {
  const _TagMenuItem({
    required this.label,
    required this.count,
    required this.active,
    required this.onTap,
  });

  final String label;
  final int count;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final color = active ? Studio.accent : Studio.textPrimary;
    return InkWell(
      onTap: onTap,
      hoverColor: Studio.hover,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
        child: Row(
          children: [
            Expanded(
              child: Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(
                  color: color,
                  fontSize: 13,
                  fontWeight: active ? FontWeight.w600 : FontWeight.w400,
                ),
              ),
            ),
            const SizedBox(width: 10),
            Text(
              '$count',
              style: TextStyle(
                color: active ? Studio.accent : Studio.textFaint,
                fontSize: 12,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _Body extends StatelessWidget {
  const _Body({required this.vm, required this.onOpen, required this.onDelete});

  final TechniqueLibraryViewModel vm;
  final void Function(Technique) onOpen;
  final void Function(Technique) onDelete;

  @override
  Widget build(BuildContext context) {
    if (vm.load.running && vm.all.isEmpty) {
      return const Center(
        child: CircularProgressIndicator(
          valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
        ),
      );
    }
    if (vm.all.isEmpty) {
      return const _EmptyState();
    }

    final filtered = vm.filtered;
    if (filtered.isEmpty) return const _NoMatches();
    return GridView.builder(
      padding: const EdgeInsets.all(24),
      gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
        maxCrossAxisExtent: 300,
        mainAxisExtent: 168,
        crossAxisSpacing: 16,
        mainAxisSpacing: 16,
      ),
      itemCount: filtered.length,
      itemBuilder: (context, index) {
        final technique = filtered[index];
        return _TechniqueCard(
          technique: technique,
          isActiveTag: vm.isTagActive,
          onOpen: () => onOpen(technique),
          onDelete: () => onDelete(technique),
          onTapTag: vm.setTag,
        );
      },
    );
  }
}

class _SearchBox extends StatefulWidget {
  const _SearchBox({required this.value, required this.onChanged});

  final String value;
  final ValueChanged<String> onChanged;

  @override
  State<_SearchBox> createState() => _SearchBoxState();
}

class _SearchBoxState extends State<_SearchBox> {
  late final TextEditingController _c = TextEditingController(
    text: widget.value,
  );

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

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
              controller: _c,
              onChanged: widget.onChanged,
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

class _TechniqueCard extends StatefulWidget {
  const _TechniqueCard({
    required this.technique,
    required this.isActiveTag,
    required this.onOpen,
    required this.onDelete,
    required this.onTapTag,
  });

  final Technique technique;
  final bool Function(String) isActiveTag;
  final VoidCallback onOpen;
  final VoidCallback onDelete;
  final ValueChanged<String> onTapTag;

  @override
  State<_TechniqueCard> createState() => _TechniqueCardState();
}

class _TechniqueCardState extends State<_TechniqueCard> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final t = widget.technique;
    final subtitle = t.summary.trim();
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
                  Expanded(
                    child: Text(
                      t.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        color: Studio.textPrimary,
                        fontSize: 16,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (t.media.isNotEmpty) ...[
                    const SizedBox(width: 8),
                    Icon(
                      t.media.any((m) => m.isVideo)
                          ? FIcons.video
                          : FIcons.music2,
                      size: 13,
                      color: Studio.textFaint,
                    ),
                    if (t.media.length > 1) ...[
                      const SizedBox(width: 2),
                      Text(
                        '${t.media.length}',
                        style: const TextStyle(
                          color: Studio.textFaint,
                          fontSize: 11,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ],
                  ],
                  if (_hover) ...[
                    const SizedBox(width: 8),
                    MouseRegion(
                      cursor: SystemMouseCursors.click,
                      child: GestureDetector(
                        onTap: widget.onDelete,
                        child: const Icon(
                          FIcons.trash2,
                          size: 15,
                          color: Studio.textFaint,
                        ),
                      ),
                    ),
                  ],
                ],
              ),
              const SizedBox(height: 6),
              // Expanded absorbs the slack and clips the summary, so the card
              // content can never overflow its fixed grid-cell height regardless
              // of summary length or how many rows the tags wrap to.
              Expanded(
                child: subtitle.isEmpty
                    ? const SizedBox.shrink()
                    : Text(
                        subtitle,
                        maxLines: 3,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          color: Studio.textSecondary,
                          fontSize: 12.5,
                          height: 1.35,
                        ),
                      ),
              ),
              if (t.tags.isNotEmpty) ...[
                const SizedBox(height: 8),
                _TagChips(
                  tags: t.tags,
                  isActiveTag: widget.isActiveTag,
                  onTapTag: widget.onTapTag,
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// The small tag chips on a technique card. Tapping one filters the library to
/// that tag.
class _TagChips extends StatelessWidget {
  const _TagChips({
    required this.tags,
    required this.isActiveTag,
    required this.onTapTag,
  });

  final List<String> tags;
  final bool Function(String) isActiveTag;
  final ValueChanged<String> onTapTag;

  @override
  Widget build(BuildContext context) {
    // Show up to three tags, then a "+N" overflow marker.
    const maxShown = 3;
    final shown = tags.take(maxShown).toList();
    final overflow = tags.length - shown.length;
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final tag in shown)
          _MiniTag(
            label: tag,
            active: isActiveTag(tag),
            onTap: () => onTapTag(tag),
          ),
        if (overflow > 0)
          Text(
            '+$overflow',
            style: const TextStyle(color: Studio.textFaint, fontSize: 11),
          ),
      ],
    );
  }
}

class _MiniTag extends StatelessWidget {
  const _MiniTag({
    required this.label,
    required this.active,
    required this.onTap,
  });

  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        // Stop the tap from also opening the card.
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
          decoration: BoxDecoration(
            color: active
                ? Studio.accent.withValues(alpha: 0.16)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(5),
            border: Border.all(
              color: active
                  ? Studio.accent.withValues(alpha: 0.45)
                  : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? Studio.accentBright : Studio.textSecondary,
              fontSize: 10.5,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

class _NoMatches extends StatelessWidget {
  const _NoMatches();

  @override
  Widget build(BuildContext context) {
    return const Center(
      child: Text(
        'No techniques match this filter.',
        style: TextStyle(color: Studio.textFaint, fontSize: 13),
      ),
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState();

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Container(
            width: 56,
            height: 56,
            decoration: BoxDecoration(
              color: Studio.accent.withValues(alpha: 0.1),
              borderRadius: BorderRadius.circular(14),
            ),
            child: const Icon(FIcons.library, color: Studio.accent, size: 26),
          ),
          const SizedBox(height: 16),
          const Text(
            'Build your technique library',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 20,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 8),
          const SizedBox(
            width: 380,
            child: Text(
              'Collect the techniques you train, in your own words. Add a '
              'technique, tag it however your method does, and attach audio or '
              'video examples.',
              textAlign: TextAlign.center,
              style: TextStyle(
                color: Studio.textSecondary,
                fontSize: 13,
                height: 1.45,
              ),
            ),
          ),
        ],
      ),
    );
  }
}
