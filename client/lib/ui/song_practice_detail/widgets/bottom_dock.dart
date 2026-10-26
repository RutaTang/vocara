import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/audio_track.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/stable_subtree.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'left_panel.dart' show VoiceSummaryView;
import 'mixer_strip.dart';
import 'voice_profile_view.dart';
import 'voiceprint_view.dart';

/// Short tab label for each dock feature.
String _tabLabel(DockTab t) => switch (t) {
  DockTab.analysisSummary => 'Analysis Summary',
  DockTab.spectrogram => 'Spectrogram',
  DockTab.power => 'Power',
  DockTab.ltas => 'LTAS',
  DockTab.vowel => 'Vowel',
  DockTab.pitch => 'Pitch',
  DockTab.vibrato => 'Vibrato',
  DockTab.timbre => 'Timbre',
  DockTab.registers => 'Register',
  DockTab.range => 'Range',
  DockTab.dynamics => 'Dynamics',
  DockTab.mixer => 'Mixer',
};

/// The collapsible bottom dock: one tab per analysis feature (spectrogram,
/// power, vowel, pitch, timbre, range, dynamics) plus the mixer, sharing the
/// timeline/playhead with the arrangement above. The spectral/pitch/timbre tabs
/// also show the live mic via the Take/Live toggle. Collapsed by default.
class BottomDock extends StatelessWidget {
  const BottomDock({
    required this.vm,
    required this.tracks,
    required this.onDeleteTrack,
    this.maxHeight = double.infinity,
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final List<AudioTrack> tracks;
  final ValueChanged<AudioTrack> onDeleteTrack;

  /// The most the whole dock may take: what the workspace has left once the
  /// timeline keeps a usable height. The dragged height is clamped to a
  /// fixed range, not to the window, so on a window shorter than the dock at
  /// its tallest a drag up (or a window shrunk under a tall dock) overflowed
  /// the workspace, collapsed the timeline and cut off the dock's bottom.
  final double maxHeight;

  /// The dock's chrome over its content: the top border, the resize grabber
  /// and the tab bar.
  static const double _chromeHeight = 1 + 10 + 34;

  /// The content height on screen: as dragged, but never more than the
  /// workspace has room for (see [maxHeight]).
  double get _contentHeight => math.max(
    0.0,
    math.min(vm.analysis.dockHeight, maxHeight - _chromeHeight),
  );

  @override
  Widget build(BuildContext context) {
    final open = vm.analysis.dockOpen;
    // The inspector asks again for metrics the cache let go; the dock does for
    // the rest of what its take tabs show, once until that leaves the take
    // whole (see AnalysisCache.healSelectedAnalysis).
    if (open &&
        vm.analysis.dockShowsTake &&
        !vm.liveMonitor &&
        vm.analysis.selectedAnalysisIncomplete) {
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => vm.analysis.healSelectedAnalysis(),
      );
    }

    // Its own layer, like the side panels: the workspace re-lays-out under
    // its LayoutBuilder on every notification of the view model, and that
    // repainted the tab strip's dozen labels with it.
    return RepaintBoundary(
      child: Container(
        decoration: const BoxDecoration(
          color: Studio.surface,
          border: Border(top: BorderSide(color: Studio.border)),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (open)
              _ResizeHandle(
                // From the height on screen, so a drag down from a capped dock
                // moves it at once — read per move, not per build, or two
                // moves inside one frame would both start from the same value.
                onDrag: (dy) => vm.analysis.setDockHeight(_contentHeight - dy),
              ),
            // Kept while what it shows is unchanged: every notification of
            // the view model rebuilds the dock, a fader or clip-drag tick
            // included, and the tab strip is some 70 widgets.
            StableSubtree<_TabBar>(
              sameAs: _TabBar.same,
              child: _TabBar(
                vm: vm,
                open: open,
                tab: vm.analysis.dockTab,
                showsLive: open && vm.analysis.activeTabSupportsLive,
                live: vm.liveMonitor,
              ),
            ),
            if (open)
              SizedBox(
                height: _contentHeight,
                child: DecoratedBox(
                  decoration: const BoxDecoration(
                    border: Border(top: BorderSide(color: Studio.border)),
                  ),
                  // The tabs' per-tick content (playhead readouts, the live
                  // mic) repaints inside this layer rather than the screen's.
                  child: RepaintBoundary(
                    child: switch (vm.analysis.dockTab) {
                      DockTab.analysisSummary => VoiceSummaryView(vm: vm),
                      DockTab.spectrogram => SpectrogramTab(vm: vm),
                      DockTab.power => PowerTab(vm: vm),
                      DockTab.ltas => LtasTab(vm: vm),
                      DockTab.vowel => VowelTab(vm: vm),
                      DockTab.pitch => PitchTab(vm: vm),
                      DockTab.vibrato => VibratoTab(vm: vm),
                      DockTab.timbre => TimbreTab(vm: vm),
                      DockTab.registers => RegisterTab(vm: vm),
                      DockTab.range => RangeView(vm: vm),
                      DockTab.dynamics => DynamicsView(vm: vm),
                      DockTab.mixer => MixerStrip(
                        vm: vm,
                        tracks: tracks,
                        onDeleteTrack: onDeleteTrack,
                      ),
                    },
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// The dock's tab strip: the analysis tabs, the Take/Live toggle where the
/// open tab has one, the mixer tab and the dock toggle.
class _TabBar extends StatelessWidget {
  const _TabBar({
    required this.vm,
    required this.open,
    required this.tab,
    required this.showsLive,
    required this.live,
  });

  final SongPracticeDetailViewModel vm;
  final bool open;
  final DockTab tab;
  final bool showsLive;
  final bool live;

  /// The [StableSubtree.sameAs] of the strip: its callbacks capture only the
  /// view model.
  static bool same(_TabBar next, _TabBar shown) =>
      identical(next.vm, shown.vm) &&
      next.open == shown.open &&
      next.tab == shown.tab &&
      next.showsLive == shown.showsLive &&
      next.live == shown.live;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 34,
      child: Row(
        children: [
          const SizedBox(width: 12),
          // Left: the voice-analysis tabs (scroll on a narrow window).
          Expanded(
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: Row(
                children: [
                  for (final t in DockTab.values)
                    if (t != DockTab.mixer) ...[
                      _Tab(
                        label: _tabLabel(t),
                        active: open && tab == t,
                        onTap: () => vm.analysis.showDockTab(t),
                      ),
                      const SizedBox(width: 6),
                    ],
                ],
              ),
            ),
          ),
          if (showsLive) ...[
            const SizedBox(width: 8),
            SourceToggle(
              live: live,
              onSelect: (v) => vm.analysis.setLiveMonitor(v),
            ),
          ],
          // Right: Mixer, separated from the analysis tabs.
          const SizedBox(width: 10),
          Container(width: 1, height: 16, color: Studio.border),
          const SizedBox(width: 10),
          _Tab(
            label: _tabLabel(DockTab.mixer),
            active: open && tab == DockTab.mixer,
            onTap: () => vm.analysis.showDockTab(DockTab.mixer),
          ),
          const SizedBox(width: 6),
          MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: vm.analysis.toggleDock,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 12),
                child: Icon(
                  // Panel-toggle icon, matching the left inspector + right
                  // lyrics panel's collapse buttons (this is the bottom
                  // panel).
                  open ? FIcons.panelBottomClose : FIcons.panelBottomOpen,
                  size: 15,
                  color: Studio.textSecondary,
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// A thin grabber along the dock's top edge; drag it to resize the dock.
class _ResizeHandle extends StatelessWidget {
  const _ResizeHandle({required this.onDrag});

  final ValueChanged<double> onDrag;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.resizeUpDown,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onVerticalDragUpdate: (d) => onDrag(d.delta.dy),
        child: SizedBox(
          height: 10,
          child: Center(
            child: Container(
              width: 36,
              height: 3,
              decoration: BoxDecoration(
                color: Studio.borderStrong,
                borderRadius: BorderRadius.circular(2),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab({required this.label, required this.active, required this.onTap});

  final String label;
  final bool active;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
          decoration: BoxDecoration(
            color: active ? Studio.surfaceRaised : Colors.transparent,
            borderRadius: BorderRadius.circular(5),
            border: Border.all(
              color: active ? Studio.border : Colors.transparent,
            ),
          ),
          child: Text(
            label.toUpperCase(),
            style: TextStyle(
              color: active ? Studio.textPrimary : Studio.textFaint,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.8,
            ),
          ),
        ),
      ),
    );
  }
}
