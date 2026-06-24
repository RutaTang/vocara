import 'dart:async';

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../../domain/models/song_practice.dart';
import '../../../routing/routes.dart';
import '../../../utils/result.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/confirm_dialog.dart';
import '../../core/widgets/studio_button.dart';
import '../view_model/home_view_model.dart';
import 'new_practice_dialog.dart';
import 'practice_project_card.dart';

/// Home screen: the dark collection of song practices.
class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  // Guards the open flow against a double-tap: without it a second tap shows a
  // second dialog and the stale rootNavigator.pop() lands on the just-pushed
  // detail route, bouncing the user straight back to Home.
  bool _opening = false;

  @override
  Widget build(BuildContext context) {
    final vm = context.read<HomeViewModel>();

    return Scaffold(
      backgroundColor: Studio.background,
      body: Column(
        children: [
          _Header(onNew: () => _createPractice(vm)),
          Expanded(
            child: ListenableBuilder(
              listenable: Listenable.merge([
                vm,
                vm.load,
                vm.createProject,
                vm.deleteProject,
              ]),
              builder: (context, _) => _Body(
                vm: vm,
                onOpen: (p) => _openDetail(vm, p.id),
                onDelete: (p) => _deletePractice(vm, p),
              ),
            ),
          ),
        ],
      ),
    );
  }

  Future<void> _createPractice(HomeViewModel vm) async {
    final title = await showNewPracticeDialog(context);
    if (title == null || !mounted) return;
    await vm.createProject.execute(title);
    final result = vm.createProject.result;
    if (result is Ok<SongPractice> && mounted) {
      await _openDetail(vm, result.value.id);
    }
  }

  Future<void> _openDetail(HomeViewModel vm, String id) async {
    if (_opening) return; // ignore a rapid second tap while one open is in flight
    _opening = true;
    try {
      // Show a small "loading practice" dialog over the library for a beat (so it
      // never just flashes), then enter. Capture the dialog's own context so we
      // pop EXACTLY it, not whatever route happens to be topmost.
      BuildContext? dialogCtx;
      unawaited(
        showDialog<void>(
          context: context,
          barrierDismissible: false,
          barrierColor: Colors.black.withValues(alpha: 0.28),
          builder: (ctx) {
            dialogCtx = ctx;
            return const _OpeningDialog();
          },
        ),
      );
      await Future<void>.delayed(const Duration(milliseconds: 1500));
      if (!mounted) return;
      if (dialogCtx != null && dialogCtx!.mounted) {
        Navigator.of(dialogCtx!).pop(); // close the dialog by its own handle
      }
      await context.push(Routes.songPractice(id));
      if (!mounted) return;
      await vm.load.execute();
    } finally {
      _opening = false;
    }
  }

  Future<void> _deletePractice(HomeViewModel vm, SongPractice practice) async {
    final confirmed = await showConfirmDialog(
      context,
      title: 'Delete "${practice.title}"?',
      body:
          'This removes the practice and all of its notes. '
          'This cannot be undone.',
    );
    if (!confirmed || !mounted) return;
    await vm.deleteProject.execute(practice.id);
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.onNew});

  final VoidCallback onNew;

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 56,
      padding: const EdgeInsets.symmetric(horizontal: 24),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          const Text(
            'Vocara',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 18,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.3,
            ),
          ),
          const Spacer(),
          StudioButton(
            icon: FIcons.plus,
            label: 'New practice',
            variant: StudioButtonVariant.accent,
            onTap: onNew,
          ),
          const SizedBox(width: 10),
          StudioButton(
            icon: FIcons.library,
            label: 'Techniques',
            onTap: () => context.push(Routes.techniqueLibrary),
          ),
          const SizedBox(width: 10),
          _HeaderIconButton(
            icon: FIcons.settings,
            tooltip: 'Settings',
            onTap: () => context.push(Routes.settings),
          ),
        ],
      ),
    );
  }
}

/// A bare icon button for the home chrome (e.g. settings).
class _HeaderIconButton extends StatelessWidget {
  const _HeaderIconButton({
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Container(
            width: 34,
            height: 34,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: Studio.surfaceRaised,
              borderRadius: BorderRadius.circular(7),
              border: Border.all(color: Studio.border),
            ),
            child: Icon(icon, size: 16, color: Studio.textSecondary),
          ),
        ),
      ),
    );
  }
}

class _Body extends StatelessWidget {
  const _Body({
    required this.vm,
    required this.onOpen,
    required this.onDelete,
  });

  final HomeViewModel vm;
  final void Function(SongPractice) onOpen;
  final void Function(SongPractice) onDelete;

  @override
  Widget build(BuildContext context) {
    if (vm.load.running && vm.projects.isEmpty) {
      return const Center(
        child: CircularProgressIndicator(
          valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
        ),
      );
    }
    if (vm.projects.isEmpty) {
      return const _EmptyState();
    }
    return GridView.builder(
      padding: const EdgeInsets.all(24),
      gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
        maxCrossAxisExtent: 280,
        mainAxisExtent: 150,
        crossAxisSpacing: 16,
        mainAxisSpacing: 16,
      ),
      itemCount: vm.projects.length,
      itemBuilder: (context, index) {
        final practice = vm.projects[index];
        return PracticeProjectCard(
          practice: practice,
          onOpen: () => onOpen(practice),
          onDelete: () => onDelete(practice),
        );
      },
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState();

  @override
  Widget build(BuildContext context) {
    // No inline button — the header's "New practice" is the single, always-
    // present way to create one.
    return const Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(FIcons.music2, size: 48, color: Studio.textFaint),
          SizedBox(height: 16),
          Text(
            'No practices yet',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 20,
              fontWeight: FontWeight.w600,
            ),
          ),
          SizedBox(height: 8),
          Text(
            'Create your first song practice to start recording and taking notes.',
            textAlign: TextAlign.center,
            style: TextStyle(color: Studio.textSecondary, fontSize: 13),
          ),
        ],
      ),
    );
  }
}

/// A small "loading practice" dialog shown over the library while a project opens (it stays on the
/// library, dimmed, so opening feels like a quick load rather than a jump to a blank screen).
class _OpeningDialog extends StatelessWidget {
  const _OpeningDialog();

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Container(
        width: 168,
        padding: const EdgeInsets.symmetric(horizontal: 24, vertical: 26),
        decoration: BoxDecoration(
          color: Studio.surfaceRaised,
          borderRadius: BorderRadius.circular(16),
          border: Border.all(color: Studio.border),
          boxShadow: [
            BoxShadow(
              color: Colors.black.withValues(alpha: 0.4),
              blurRadius: 28,
              offset: const Offset(0, 10),
            ),
          ],
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            // Accent disc behind the spinner — reads as a deliberate, branded loader.
            Container(
              width: 52,
              height: 52,
              decoration: BoxDecoration(
                color: Studio.accent.withValues(alpha: 0.1),
                shape: BoxShape.circle,
              ),
              child: const Padding(
                padding: EdgeInsets.all(14),
                child: CircularProgressIndicator(
                  strokeWidth: 2.6,
                  valueColor: AlwaysStoppedAnimation(Studio.accent),
                ),
              ),
            ),
            const SizedBox(height: 16),
            const Text(
              'Loading practice',
              style: TextStyle(
                color: Studio.textPrimary,
                fontSize: 13.5,
                fontWeight: FontWeight.w600,
                letterSpacing: 0.2,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
