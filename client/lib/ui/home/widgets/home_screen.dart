import 'dart:async';

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../../data/services/recording_finalizer.dart';
import '../../../data/services/update_check_service.dart';
import '../../../domain/models/practice_summary.dart';
import '../../../domain/models/song_practice.dart';
import '../../../routing/routes.dart';
import '../../../utils/result.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/confirm_dialog.dart';
import '../../core/widgets/studio_button.dart';
import '../view_model/home_view_model.dart';
import 'new_practice_dialog.dart';
import 'practice_project_card.dart';
import 'update_notice.dart';

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

  late final HomeViewModel _vm;

  // The once-per-app-run update auto-check, deferred a beat after home mounts
  // so it never competes with first paint. The service itself makes repeat
  // mounts a no-op; this timer only spaces out the first one.
  Timer? _updateCheckTimer;

  @override
  void initState() {
    super.initState();
    // Surface create/delete failures as a transient toast the moment the command
    // settles (mirrors the detail screen's SnackBar pattern). Without this, a
    // failed mutation is silently dropped and the UI diverges from SQLite.
    _vm = context.read<HomeViewModel>();
    _vm.createProject.addListener(_onMutationResult);
    _vm.deleteProject.addListener(_onMutationResult);
    final updates = context.read<UpdateCheckService>();
    _updateCheckTimer = Timer(const Duration(seconds: 5), () {
      // Fire-and-forget: the check is non-blocking and swallows every failure.
      unawaited(updates.autoCheck());
    });
  }

  @override
  void dispose() {
    _updateCheckTimer?.cancel();
    _vm.createProject.removeListener(_onMutationResult);
    _vm.deleteProject.removeListener(_onMutationResult);
    super.dispose();
  }

  void _onMutationResult() {
    if (!mounted) return;
    if (_vm.createProject.error) {
      _showError('Could not create the practice. Please try again.');
      _vm.createProject.clearResult();
    }
    if (_vm.deleteProject.error) {
      _showError('Could not delete the practice. Please try again.');
      _vm.deleteProject.clearResult();
    }
  }

  void _showError(String message) {
    ScaffoldMessenger.of(context)
      ..clearSnackBars()
      ..showSnackBar(
        SnackBar(
          content: Text(
            message,
            style: const TextStyle(color: Studio.textPrimary, fontSize: 13),
          ),
          backgroundColor: Studio.surfaceRaised,
          behavior: SnackBarBehavior.floating,
          margin: const EdgeInsets.all(16),
          duration: const Duration(milliseconds: 2800),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(8),
            side: const BorderSide(color: Studio.border),
          ),
        ),
      );
  }

  @override
  Widget build(BuildContext context) {
    final vm = context.read<HomeViewModel>();

    return Scaffold(
      backgroundColor: Studio.background,
      body: Column(
        children: [
          _Header(onNew: () => _createPractice(vm)),
          UpdateNoticeBanner(updates: context.read<UpdateCheckService>()),
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
                onRetry: () => vm.load.execute(),
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
    if (_opening) {
      return; // ignore a rapid second tap while one open is in flight
    }
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
      // A take stopped there (or an import, a delete) can outlive the screen,
      // and the list read now may not hold it: read it again once it has
      // landed, without holding the open guard meanwhile. Asked before this
      // read: one landing while it reads may not be in it, and is no longer
      // landing after.
      final landing = RecordingFinalizer.instance.isLanding(id);
      await vm.load.execute();
      if (landing) unawaited(_reloadOnceLanded(vm, id));
    } finally {
      _opening = false;
    }
  }

  Future<void> _reloadOnceLanded(HomeViewModel vm, String id) async {
    await RecordingFinalizer.instance.settled(id);
    if (!mounted) return;
    // A load already running ignores another ask, and may have read before
    // the landing: ask once it is done.
    if (vm.load.running) {
      final idle = Completer<void>();
      void check() {
        if (!vm.load.running && !idle.isCompleted) idle.complete();
      }

      vm.load.addListener(check);
      await idle.future;
      vm.load.removeListener(check);
    }
    if (mounted) await vm.load.execute();
  }

  Future<void> _deletePractice(
    HomeViewModel vm,
    PracticeSummary practice,
  ) async {
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
    required this.onRetry,
  });

  final HomeViewModel vm;
  final void Function(PracticeSummary) onOpen;
  final void Function(PracticeSummary) onDelete;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    if (vm.load.running && vm.projects.isEmpty) {
      return const Center(
        child: CircularProgressIndicator(
          valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
        ),
      );
    }
    // A failed list read must read as an error (with Retry), never collapse into
    // the empty state — otherwise a load failure looks like "No practices yet".
    if (vm.load.error && vm.projects.isEmpty) {
      return _ErrorState(onRetry: onRetry);
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

/// Shown when the practice list fails to load — distinct from the empty state so a
/// read error is never mistaken for a lost library. Offers a Retry that re-runs load.
class _ErrorState extends StatelessWidget {
  const _ErrorState({required this.onRetry});

  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const Icon(FIcons.triangleAlert, size: 48, color: Studio.textFaint),
          const SizedBox(height: 16),
          const Text(
            'Could not load your practices',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 20,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(height: 8),
          const Text(
            'Something went wrong reading your library. Your data is safe.',
            textAlign: TextAlign.center,
            style: TextStyle(color: Studio.textSecondary, fontSize: 13),
          ),
          const SizedBox(height: 20),
          StudioButton(icon: FIcons.refreshCw, label: 'Retry', onTap: onRetry),
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
