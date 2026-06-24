import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../domain/models/audio_track.dart';
import '../../core/themes/studio.dart';
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
    super.key,
  });

  final SongPracticeDetailViewModel vm;
  final List<AudioTrack> tracks;
  final ValueChanged<AudioTrack> onDeleteTrack;

  @override
  Widget build(BuildContext context) {
    final open = vm.dockOpen;

    return Container(
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(top: BorderSide(color: Studio.border)),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (open)
            _ResizeHandle(onDrag: (dy) => vm.setDockHeight(vm.dockHeight - dy)),
          SizedBox(
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
                              active: open && vm.dockTab == t,
                              onTap: () => vm.showDockTab(t),
                            ),
                            const SizedBox(width: 6),
                          ],
                      ],
                    ),
                  ),
                ),
                if (open && vm.activeTabSupportsLive) ...[
                  const SizedBox(width: 8),
                  SourceToggle(
                    live: vm.liveMonitor,
                    onSelect: (v) => vm.setLiveMonitor(v),
                  ),
                ],
                // Right: Mixer, separated from the analysis tabs.
                const SizedBox(width: 10),
                Container(width: 1, height: 16, color: Studio.border),
                const SizedBox(width: 10),
                _Tab(
                  label: _tabLabel(DockTab.mixer),
                  active: open && vm.dockTab == DockTab.mixer,
                  onTap: () => vm.showDockTab(DockTab.mixer),
                ),
                const SizedBox(width: 6),
                MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: GestureDetector(
                    onTap: vm.toggleDock,
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
          ),
          if (open)
            SizedBox(
              height: vm.dockHeight,
              child: DecoratedBox(
                decoration: const BoxDecoration(
                  border: Border(top: BorderSide(color: Studio.border)),
                ),
                child: switch (vm.dockTab) {
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
