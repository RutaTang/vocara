import 'package:flutter/material.dart';
import 'package:forui/forui.dart' show FIcons;

import '../../../domain/models/audio_fx.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../../utils/format.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/cap_label.dart';
import '../../core/widgets/stable_subtree.dart';
import '../../core/widgets/studio_controls.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'fx_panel.dart';
import 'pitch_lane.dart'
    show PitchNote, noteVibratoOf, pitchInTunePercent, pitchNotesOf;
import 'track_context_menu.dart';

const double _panelWidth = 272;

// Pass / warn / flag indicator colors for the scorecard dots.
const _good = Studio.good;
const _warn = Color(0xFFE0A04C);
const _flag = Studio.record;

// Register split colors (warm chest → cool head).
const _chest = Studio.registerChest;
const _head = Studio.registerHead;

const _months = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
];
String _fmtDate(DateTime d) => '${_months[d.month - 1]} ${d.day}';

/// The left inspector: an always-visible, at-a-glance summary of the selected
/// take (the analysis numbers that are otherwise scattered across the dock's
/// per-frame tabs). Collapsible to a thin rail. Driven by [vm.selectedTrack].
class LeftPanel extends StatefulWidget {
  const LeftPanel({required this.vm, super.key});

  final SongPracticeDetailViewModel vm;

  @override
  State<LeftPanel> createState() => _LeftPanelState();
}

class _LeftPanelState extends State<LeftPanel> {
  SongPracticeDetailViewModel get vm => widget.vm;

  @override
  Widget build(BuildContext context) {
    // The inspector is always visible, so it must trigger the analysis compute
    // itself (the dock would otherwise be responsible) the moment a take is
    // selected but its metrics aren't computed yet.
    final track = vm.selectedTrack;
    if (vm.inspectorOpen &&
        track != null &&
        !track.isBacking &&
        vm.selectedMetrics == null &&
        !vm.analysis.selectedMetricsUnavailable &&
        !vm.spectrogramLoading &&
        !vm.analysis.metricsLoadingFor(track.id)) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) vm.analysis.ensureSelectedSpectrogram();
      });
    }

    // Its own layer: whatever else on the screen repaints (the dock's live
    // readouts, a lane's take), the inspector's strips and faders don't.
    return RepaintBoundary(
      child: vm.inspectorOpen
          ? _Body(vm: vm)
          : _Rail(onTap: vm.toggleInspectorPanel),
    );
  }
}

/// The thin collapsed rail (mirrors the right panel's rail, flipped).
class _Rail extends StatelessWidget {
  const _Rail({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: 34,
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(right: BorderSide(color: Studio.border)),
      ),
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          onTap: onTap,
          behavior: HitTestBehavior.opaque,
          child: Column(
            children: const [
              SizedBox(height: 12),
              Icon(FIcons.panelLeftOpen, size: 15, color: Studio.textSecondary),
              SizedBox(height: 18),
              // Clipped, not overflowing, where the dock leaves the panels
              // less height than the label.
              Flexible(
                child: RotatedBox(
                  quarterTurns: 1,
                  child: Text(
                    'INSPECTOR',
                    maxLines: 1,
                    softWrap: false,
                    overflow: TextOverflow.clip,
                    style: TextStyle(
                      color: Studio.textSecondary,
                      fontSize: 11,
                      fontWeight: FontWeight.w700,
                      letterSpacing: 1.5,
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Body extends StatelessWidget {
  const _Body({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    final bus = vm.selectedBus;
    final track = vm.selectedTrack;
    return Container(
      width: _panelWidth,
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(right: BorderSide(color: Studio.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Header(onCollapse: vm.toggleInspectorPanel),
          const Divider(height: 1, color: Studio.border),
          Expanded(
            child: bus != null
                ? _BusBody(vm: vm, bus: bus)
                : track == null
                ? const _Empty('Select a take or bus to inspect it.')
                : _BodyContent(vm: vm, track: track),
          ),
        ],
      ),
    );
  }
}

/// Inspector body for a selected FX bus: a header + the (bus-targeted) FX panel.
class _BusBody extends StatelessWidget {
  const _BusBody({required this.vm, required this.bus});

  final SongPracticeDetailViewModel vm;
  final FxBus bus;

  @override
  Widget build(BuildContext context) {
    final header = Padding(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
      child: Row(
        children: [
          Container(
            width: 9,
            height: 9,
            decoration: BoxDecoration(
              color: bus.colorValue != null
                  ? Color(bus.colorValue!)
                  : Studio.accent,
              shape: BoxShape.circle,
            ),
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              bus.name,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 15,
                fontWeight: FontWeight.w700,
              ),
            ),
          ),
          const Text(
            'FX BUS',
            style: TextStyle(
              color: Studio.textFaint,
              fontSize: 9,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.8,
            ),
          ),
        ],
      ),
    );
    // Keyed per channel (here and in _BodyContent): unkeyed, selecting
    // another bus or take reused the panel's element tree, carrying the EQ
    // analyzer's smoothed spectrum, adaptive ceiling and latched "has signal"
    // over from the previous channel.
    final fx = StableSubtree<FxPanel>(
      sameAs: FxPanel.same,
      child: FxPanel(key: ValueKey(bus.id), vm: vm, bus: bus),
    );
    final footer = _BusChannelFooter(vm: vm, bus: bus);
    return LayoutBuilder(
      builder: (context, constraints) {
        // The take strip's degenerate-height fallback, for the same reason:
        // pinned, the header and footer overflowed a short panel and pushed
        // the bus's fader, mute/solo and pan under the dock.
        if (constraints.maxHeight < _BodyContent._kMinPinnedHeight) {
          return SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                header,
                Padding(
                  padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
                  child: SizedBox(height: 220, child: fx),
                ),
                footer,
              ],
            ),
          );
        }
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            header,
            Expanded(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
                child: fx,
              ),
            ),
            footer,
          ],
        );
      },
    );
  }
}

/// The channel-strip footer for an FX bus: output routing, sends, pan, fader +
/// dB, and mute/solo.
class _BusChannelFooter extends StatelessWidget {
  const _BusChannelFooter({required this.vm, required this.bus});

  final SongPracticeDetailViewModel vm;
  final FxBus bus;

  @override
  Widget build(BuildContext context) {
    final buses = vm.mixer.fxBuses;
    var dest = 'Stereo Out';
    for (final b in buses) {
      if (b.id == bus.outputBusId) dest = b.name;
    }
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 12),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(top: BorderSide(color: Studio.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _SendsSection(
            sends: bus.sends,
            targets: [
              for (final b in vm.mixer.fxBuses)
                if (b.id != bus.id) b,
            ],
            onAdd: (busId) => vm.mixer.addBusSend(bus.id, busId),
            onRemove: (slot) => vm.mixer.removeBusSend(bus.id, slot),
            onRetarget: (slot, busId) =>
                vm.mixer.setBusSendTarget(bus.id, slot, busId),
            onLevel: (slot, db) =>
                vm.mixer.setBusSendLevelLive(bus.id, slot, db),
            onCommit: vm.commitFx,
          ),
          Row(
            children: [
              const _StripLabel('OUTPUT'),
              const Spacer(),
              // Bounded by the row: a long bus name ellipsizes in the chip
              // instead of running past the inspector's edge.
              Flexible(
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: Builder(
                    builder: (anchor) => GestureDetector(
                      // The chip's own context: the footer's anchored the menu
                      // below the whole footer instead of under the chip.
                      onTap: () =>
                          showStudioMenu(anchor, menuAnchorBelow(anchor), [
                            StudioMenuItem(
                              label: 'Stereo Out',
                              selected: bus.outputBusId == null,
                              onTap: () => vm.mixer.setBusOutput(bus.id, null),
                            ),
                            for (final b in buses)
                              if (b.id != bus.id)
                                StudioMenuItem(
                                  label: b.name,
                                  selected: bus.outputBusId == b.id,
                                  onTap: () =>
                                      vm.mixer.setBusOutput(bus.id, b.id),
                                ),
                          ], width: 180),
                      child: Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 9,
                          vertical: 5,
                        ),
                        decoration: BoxDecoration(
                          color: Studio.inset,
                          borderRadius: BorderRadius.circular(6),
                          border: Border.all(color: Studio.border),
                        ),
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Flexible(
                              child: Text(
                                dest,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: const TextStyle(
                                  color: Studio.textSecondary,
                                  fontSize: 11,
                                ),
                              ),
                            ),
                            const SizedBox(width: 4),
                            const Icon(
                              Icons.expand_more,
                              size: 14,
                              color: Studio.textFaint,
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 10),
          // Mute / solo on the left, pan on the right.
          Row(
            children: [
              _MsToggle(
                label: 'M',
                active: bus.muted,
                color: Studio.record,
                onTap: () => vm.mixer.toggleBusMute(bus.id),
              ),
              const SizedBox(width: 8),
              _MsToggle(
                label: 'S',
                active: bus.soloed,
                color: Studio.accent,
                onTap: () => vm.mixer.toggleBusSolo(bus.id),
              ),
              const Spacer(),
              StudioPanKnob(
                value: bus.pan,
                onChanged: (p) => vm.mixer.setBusPanLive(bus.id, p),
                onCommit: vm.commitFx,
              ),
            ],
          ),
          const SizedBox(height: 14),
          // The fader fills the width; its level reads to the right.
          Row(
            children: [
              Expanded(
                child: StudioFader(
                  value: bus.volume,
                  accent: Studio.accent,
                  onChanged: (v) => vm.mixer.setBusVolumeLive(bus.id, v),
                  onChangeEnd: vm.commitFx,
                ),
              ),
              const SizedBox(width: 8),
              FaderDbReadout(bus.volume),
            ],
          ),
        ],
      ),
    );
  }
}

/// The body for a selected track: a fixed take header + (for vocal takes, when the engine
/// supports effects) an Analysis | FX toggle, then the scrollable detail for that mode.
class _BodyContent extends StatelessWidget {
  const _BodyContent({required this.vm, required this.track});

  final SongPracticeDetailViewModel vm;
  final AudioTrack track;

  /// Below this panel height the pinned-footer layout cannot fit its fixed
  /// chrome (take header + channel-strip footer) and would overflow; the
  /// strip falls back to scrolling as one column instead. Generous on
  /// purpose: near this height the FX list has no usable room anyway.
  static const double _kMinPinnedHeight = 420;

  @override
  Widget build(BuildContext context) {
    // A Logic-style channel strip: the FX inserts (EQ/comp/reverb) scroll above,
    // and the channel-strip footer (output routing, pan, fader, mute/solo) is
    // pinned to the bottom. Analysis moved to the bottom dock's summary tab.
    final isVocal = !track.isBacking && vm.mixer.supportsEffects;
    final header = Padding(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
      child: _TakeHeader(vm: vm, track: track),
    );
    final footer = _TrackChannelFooter(
      vm: vm,
      track: track,
      showRouting: isVocal,
    );
    // Both FxPanels below are keyed by the take, so selecting another one
    // starts its EQ analyzer fresh (see the bus inspector's).
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxHeight < _kMinPinnedHeight) {
          // Degenerate height (tiny window with the dock open): scroll the
          // whole strip. FxPanel's inner ListView needs a bounded height, so
          // it gets a fixed window inside the scrolling column.
          return SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                header,
                if (isVocal)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
                    child: SizedBox(
                      height: 220,
                      child: StableSubtree<FxPanel>(
                        sameAs: FxPanel.same,
                        child: FxPanel(
                          key: ValueKey(track.id),
                          vm: vm,
                          track: track,
                        ),
                      ),
                    ),
                  )
                else
                  const SizedBox(
                    height: 120,
                    child: _Empty(
                      'Backing track plays dry, with no effects.',
                      inset: false,
                    ),
                  ),
                footer,
              ],
            ),
          );
        }
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            header,
            Expanded(
              child: isVocal
                  ? Padding(
                      padding: const EdgeInsets.fromLTRB(14, 12, 14, 0),
                      child: StableSubtree<FxPanel>(
                        sameAs: FxPanel.same,
                        child: FxPanel(
                          key: ValueKey(track.id),
                          vm: vm,
                          track: track,
                        ),
                      ),
                    )
                  : const _Empty(
                      'Backing track plays dry, with no effects.',
                      inset: false,
                    ),
            ),
            footer,
          ],
        );
      },
    );
  }
}

/// The Logic-style channel-strip footer for a track: output routing (vocal
/// only), a pan knob, the volume fader + dB, and mute/solo.
class _TrackChannelFooter extends StatelessWidget {
  const _TrackChannelFooter({
    required this.vm,
    required this.track,
    required this.showRouting,
  });

  final SongPracticeDetailViewModel vm;
  final AudioTrack track;
  final bool showRouting;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 12),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(top: BorderSide(color: Studio.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (showRouting) ...[
            _SendsSection(
              sends: track.sends,
              targets: vm.mixer.fxBuses,
              onAdd: (busId) => vm.mixer.addTrackSend(track.id, busId),
              onRemove: (slot) => vm.mixer.removeTrackSend(track.id, slot),
              onRetarget: (slot, busId) =>
                  vm.mixer.setTrackSendTarget(track.id, slot, busId),
              onLevel: (slot, db) =>
                  vm.mixer.setTrackSendLevelLive(track.id, slot, db),
              onCommit: vm.commitFx,
            ),
            Row(
              children: [
                const _StripLabel('OUTPUT'),
                const Spacer(),
                Flexible(
                  child: _OutputChip(vm: vm, track: track),
                ),
              ],
            ),
            const SizedBox(height: 10),
          ],
          // Mute / solo on the left, pan on the right.
          Row(
            children: [
              _MsToggle(
                label: 'M',
                active: track.muted,
                color: Studio.record,
                onTap: () => vm.mixer.toggleMute(track.id),
              ),
              const SizedBox(width: 8),
              _MsToggle(
                label: 'S',
                active: track.soloed,
                color: Studio.accent,
                onTap: () => vm.mixer.toggleSolo(track.id),
              ),
              const Spacer(),
              StudioPanKnob(
                value: track.pan,
                onChanged: (p) => vm.mixer.setTrackPanLive(track.id, p),
                onCommit: vm.mixer.commitTrackVolume,
              ),
            ],
          ),
          const SizedBox(height: 14),
          // The fader fills the width; its level reads to the right.
          Row(
            children: [
              Expanded(
                child: StudioFader(
                  value: track.volume,
                  accent: Studio.accent,
                  onChanged: (v) => vm.mixer.setTrackVolumeLive(track.id, v),
                  onChangeEnd: vm.mixer.commitTrackVolume,
                ),
              ),
              const SizedBox(width: 8),
              FaderDbReadout(track.volume),
            ],
          ),
        ],
      ),
    );
  }
}

/// A caption label for the channel-strip footer rows.
/// Logic-style "Sends" section: one row per post-fader aux send (target bus
/// chip + an independent level fader + remove), plus a "+ Send" to add one.
class _SendsSection extends StatelessWidget {
  const _SendsSection({
    required this.sends,
    required this.targets,
    required this.onAdd,
    required this.onRemove,
    required this.onRetarget,
    required this.onLevel,
    required this.onCommit,
  });

  final List<TrackSend> sends;
  final List<FxBus> targets;
  final ValueChanged<String> onAdd;
  final ValueChanged<int> onRemove;
  final void Function(int slot, String busId) onRetarget;
  final void Function(int slot, double levelDb) onLevel;
  final VoidCallback onCommit;

  static String _fmtDb(double db) => db <= -47.5 ? '-∞' : '${db.round()}';

  String _name(String id) {
    for (final b in targets) {
      if (b.id == id) return b.name;
    }
    return '—';
  }

  /// Opens the target picker under the chip whose context is [context] —
  /// callers pass the CHIP's (a Builder below it), not this section's, or the
  /// menu anchors below the last send row.
  void _menu(
    BuildContext context,
    List<FxBus> options,
    ValueChanged<String> onPick,
  ) {
    showStudioMenu(context, menuAnchorBelow(context), [
      for (final b in options)
        StudioMenuItem(label: b.name, onTap: () => onPick(b.id)),
    ], width: 160);
  }

  @override
  Widget build(BuildContext context) {
    final used = {for (final s in sends) s.busId};
    final addable = [
      for (final b in targets)
        if (!used.contains(b.id)) b,
    ];
    final canAdd = addable.isNotEmpty && sends.length < kMaxSends;
    if (sends.isEmpty && !canAdd) return const SizedBox.shrink();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            const _StripLabel('SENDS'),
            const Spacer(),
            if (canAdd)
              MouseRegion(
                cursor: SystemMouseCursors.click,
                child: Builder(
                  builder: (anchor) => GestureDetector(
                    onTap: () => _menu(anchor, addable, onAdd),
                    child: Container(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 7,
                        vertical: 3,
                      ),
                      decoration: BoxDecoration(
                        color: Studio.surfaceRaised,
                        borderRadius: BorderRadius.circular(5),
                        border: Border.all(color: Studio.border),
                      ),
                      child: const Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(
                            FIcons.plus,
                            size: 11,
                            color: Studio.textSecondary,
                          ),
                          SizedBox(width: 3),
                          Text(
                            'Send',
                            style: TextStyle(
                              color: Studio.textSecondary,
                              fontSize: 10,
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ),
          ],
        ),
        for (var i = 0; i < sends.length; i++) ...[
          const SizedBox(height: 6),
          Row(
            children: [
              SizedBox(
                width: 66,
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  child: Builder(
                    builder: (anchor) => GestureDetector(
                      onTap: () {
                        final options = [
                          for (final b in targets)
                            if (b.id == sends[i].busId || !used.contains(b.id))
                              b,
                        ];
                        _menu(anchor, options, (id) => onRetarget(i, id));
                      },
                      child: Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 6,
                          vertical: 3,
                        ),
                        decoration: BoxDecoration(
                          color: Studio.inset,
                          borderRadius: BorderRadius.circular(4),
                          border: Border.all(color: Studio.border),
                        ),
                        child: Text(
                          _name(sends[i].busId),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: const TextStyle(
                            color: Studio.textSecondary,
                            fontSize: 10,
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
              const SizedBox(width: 6),
              Expanded(
                child: StudioFader(
                  value: ((sends[i].levelDb + 48) / 48).clamp(0.0, 1.0),
                  accent: Studio.accent,
                  onChanged: (v) => onLevel(i, v * 48 - 48),
                  onChangeEnd: onCommit,
                ),
              ),
              const SizedBox(width: 6),
              SizedBox(
                width: 26,
                child: Text(
                  _fmtDb(sends[i].levelDb),
                  textAlign: TextAlign.right,
                  style: const TextStyle(
                    color: Studio.textSecondary,
                    fontSize: 9,
                    fontFeatures: [FontFeature.tabularFigures()],
                  ),
                ),
              ),
              const SizedBox(width: 4),
              MouseRegion(
                cursor: SystemMouseCursors.click,
                child: GestureDetector(
                  onTap: () => onRemove(i),
                  child: const Icon(
                    Icons.close,
                    size: 13,
                    color: Studio.textFaint,
                  ),
                ),
              ),
            ],
          ),
        ],
        const SizedBox(height: 10),
      ],
    );
  }
}

class _StripLabel extends StatelessWidget {
  const _StripLabel(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => Text(
    text,
    style: const TextStyle(
      color: Studio.textFaint,
      fontSize: 9,
      fontWeight: FontWeight.w700,
      letterSpacing: 0.5,
    ),
  );
}

/// The output-routing chip: "Stereo Out" or a bus name; tap to choose.
class _OutputChip extends StatelessWidget {
  const _OutputChip({required this.vm, required this.track});

  final SongPracticeDetailViewModel vm;
  final AudioTrack track;

  @override
  Widget build(BuildContext context) {
    final buses = vm.mixer.fxBuses;
    var dest = 'Stereo Out';
    for (final b in buses) {
      if (b.id == track.busId) dest = b.name;
    }
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => showStudioMenu(context, menuAnchorBelow(context), [
          StudioMenuItem(
            label: 'Stereo Out',
            selected: track.busId == null,
            onTap: () => vm.mixer.setTrackBus(track.id, null),
          ),
          for (final b in buses)
            StudioMenuItem(
              label: b.name,
              selected: track.busId == b.id,
              onTap: () => vm.mixer.setTrackBus(track.id, b.id),
            ),
        ], width: 180),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 5),
          decoration: BoxDecoration(
            color: Studio.inset,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Flexible(
                child: Text(
                  dest,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    color: Studio.textSecondary,
                    fontSize: 11,
                  ),
                ),
              ),
              const SizedBox(width: 4),
              const Icon(Icons.expand_more, size: 14, color: Studio.textFaint),
            ],
          ),
        ),
      ),
    );
  }
}

/// A small mute/solo toggle button.
class _MsToggle extends StatelessWidget {
  const _MsToggle({
    required this.label,
    required this.active,
    required this.color,
    required this.onTap,
  });

  final String label;
  final bool active;
  final Color color;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 30,
          height: 22,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: active ? color.withValues(alpha: 0.2) : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(5),
            border: Border.all(
              color: active ? color.withValues(alpha: 0.6) : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? color : Studio.textSecondary,
              fontSize: 11,
              fontWeight: FontWeight.w700,
            ),
          ),
        ),
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.onCollapse});

  final VoidCallback onCollapse;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 40,
      child: Padding(
        padding: const EdgeInsets.only(left: 14, right: 6),
        child: Row(
          children: [
            const Text(
              'INSPECTOR',
              style: TextStyle(
                color: Studio.textPrimary,
                fontSize: 11,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
            const Spacer(),
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: onCollapse,
                child: const Padding(
                  padding: EdgeInsets.all(8),
                  child: Icon(
                    FIcons.panelLeftClose,
                    size: 15,
                    color: Studio.textSecondary,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Take identity: color dot, name, interpretation/take #, duration + date.
class _TakeHeader extends StatelessWidget {
  const _TakeHeader({required this.vm, required this.track});

  final SongPracticeDetailViewModel vm;
  final AudioTrack track;

  @override
  Widget build(BuildContext context) {
    final dotColor = track.colorValue != null
        ? Color(track.colorValue!)
        : (track.isBacking ? Studio.accent : Studio.waveform);
    final sub = track.isBacking
        ? (track.isReference ? 'Original' : 'Accompaniment')
        : _takeLabel();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Container(
              width: 9,
              height: 9,
              decoration: BoxDecoration(
                color: dotColor,
                shape: BoxShape.circle,
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                track.displayName,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  color: Studio.textPrimary,
                  fontSize: 13.5,
                  fontWeight: FontWeight.w700,
                  height: 1.25,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Text(
          '$sub  ·  ${formatClockMs(track.durationMs)}  ·  ${_fmtDate(track.createdAt)}',
          style: const TextStyle(color: Studio.textFaint, fontSize: 11),
        ),
      ],
    );
  }

  String _takeLabel() {
    final takes = vm.versions.vocalTakes;
    final idx = takes.indexWhere((t) => t.id == track.id);
    final interp = vm.versions.active?.name;
    final num = idx >= 0 ? 'Take ${idx + 1}' : 'Take';
    return interp == null ? num : '$num · $interp';
  }
}

/// The voice "scorecard": range + a grid of vital metrics + register split.
/// The voice-analysis summary for the selected take — range, in-tune %, vibrato,
/// clarity, dynamics, brightness, register. Hosted in the bottom dock's
/// "Analysis Summary" tab (moved out of the inspector, which is now a channel
/// strip).
/// A red banner when the selected take is clipped or too noisy to trust.
Widget _qualityBanner(VoiceMetrics m) {
  final msg = m.isClipped
      ? 'Clipped ${(m.clippedFraction * 100).toStringAsFixed(1)}% — lower the '
            'mic gain and re-record.'
      : 'Low SNR (${m.snrDb.round()} dB) — record closer to the mic / quieter.';
  return Padding(
    padding: const EdgeInsets.only(bottom: 12),
    child: Container(
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        color: Studio.record.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.record.withValues(alpha: 0.4)),
      ),
      child: Row(
        children: [
          const Icon(FIcons.triangleAlert, size: 14, color: Studio.record),
          const SizedBox(width: 7),
          Expanded(
            child: Text(
              msg,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 11,
                height: 1.3,
              ),
            ),
          ),
        ],
      ),
    ),
  );
}

class VoiceSummaryView extends StatelessWidget {
  const VoiceSummaryView({required this.vm, super.key});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    final track = vm.selectedTrack;
    if (track == null) {
      return const _Empty('Select a take to see its analysis.');
    }
    if (track.isBacking) {
      return const _Empty('Backing track has no voice analysis.');
    }
    return ListView(
      padding: const EdgeInsets.fromLTRB(16, 14, 16, 18),
      children: [_VoiceSummary(vm: vm)],
    );
  }
}

class _VoiceSummary extends StatelessWidget {
  const _VoiceSummary({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    final track = vm.selectedTrack;
    final loadingMetrics =
        track != null && vm.analysis.metricsLoadingFor(track.id);
    if (vm.spectrogramLoading || loadingMetrics) {
      return const _Empty('Analyzing…', inset: false, spinner: true);
    }
    final m = vm.selectedMetrics;
    final profile = vm.selectedProfile;
    if (m == null || profile == null) {
      return const _Empty('No analysis for this take.', inset: false);
    }

    final notes = pitchNotesOf(m);
    final inTune = notes.isEmpty ? null : pitchInTunePercent(notes);
    final vib = _longestNoteVibrato(m, notes);
    final reg = vm.selectedRegister;
    final ltas = vm.selectedLtas;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (m.isClipped || m.isLowSnr) _qualityBanner(m),
        _SectionLabel('Voice summary'),
        const SizedBox(height: 10),
        _RangeRow(
          profile: profile,
          onTap: () => vm.analysis.showDockTab(DockTab.range),
        ),
        const SizedBox(height: 14),
        _grid([
          _Metric(
            label: 'In tune',
            value: inTune == null ? '—' : '${inTune.round()}%',
            sub: 'of notes',
            dot: inTune == null
                ? null
                : (inTune >= 85 ? _good : (inTune >= 70 ? _warn : _flag)),
            onTap: () => vm.analysis.showDockTab(DockTab.pitch),
          ),
          _Metric(
            label: 'Vibrato',
            value: vib == null || !vib.present
                ? 'straight'
                : '${vib.rateHz.toStringAsFixed(1)} Hz',
            sub: vib == null || !vib.present
                ? 'no vibrato'
                : '±${vib.extentCents.round()}¢',
            dot: vib == null || !vib.present
                ? null
                : ((vib.rateHz >= 5 && vib.rateHz <= 7) ? _good : _warn),
            onTap: () => vm.analysis.showDockTab(DockTab.vibrato),
          ),
          _Metric(
            label: 'Clarity',
            value: m.cppAvg.isNaN ? '—' : m.cppAvg.toStringAsFixed(1),
            sub: 'CPP',
            onTap: () => vm.analysis.showDockTab(DockTab.timbre),
          ),
          _Metric(
            label: 'Ring',
            value: (ltas == null || !ltas.hasData)
                ? '—'
                : '${ltas.singersFormantDb.round()} dB',
            sub: "singer's formant",
            onTap: () => vm.analysis.showDockTab(DockTab.ltas),
          ),
          _Metric(
            label: 'Dynamics',
            value: profile.hasDynamics
                ? '${profile.dynamicRangeDb.round()} dB'
                : '—',
            sub: 'soft → loud',
            onTap: () => vm.analysis.showDockTab(DockTab.dynamics),
          ),
          _Metric(
            label: 'Brightness',
            value: (ltas == null || !ltas.hasData)
                ? '—'
                : '${ltas.alphaRatio.round()} dB',
            sub: 'α ratio',
            onTap: () => vm.analysis.showDockTab(DockTab.ltas),
          ),
        ]),
        if (reg != null && reg.hasData) ...[
          const SizedBox(height: 16),
          _RegisterBar(
            reg: reg,
            onTap: () => vm.analysis.showDockTab(DockTab.registers),
          ),
        ],
      ],
    );
  }

  Widget _grid(List<Widget> cells) {
    final rows = <Widget>[];
    for (var i = 0; i < cells.length; i += 2) {
      rows.add(
        Padding(
          padding: const EdgeInsets.only(bottom: 4),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(child: cells[i]),
              const SizedBox(width: 8),
              Expanded(
                child: i + 1 < cells.length ? cells[i + 1] : const SizedBox(),
              ),
            ],
          ),
        ),
      );
    }
    return Column(children: rows);
  }

  Vibrato? _longestNoteVibrato(VoiceMetrics m, List<PitchNote> notes) {
    if (notes.isEmpty) return null;
    var longest = notes.first;
    for (final n in notes) {
      if (n.endFrame - n.startFrame > longest.endFrame - longest.startFrame) {
        longest = n;
      }
    }
    return noteVibratoOf(m, longest);
  }
}

/// The sung-range line + a mini bar (tessitura highlight + passaggio marker).
class _RangeRow extends StatelessWidget {
  const _RangeRow({required this.profile, required this.onTap});

  final VoiceProfile profile;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final p = profile;
    if (!p.hasRange) {
      return const Text(
        'No pitch detected',
        style: TextStyle(color: Studio.textFaint, fontSize: 12),
      );
    }
    final span = p.highMidi! - p.lowMidi!;
    return _Tappable(
      onTap: onTap,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.baseline,
            textBaseline: TextBaseline.alphabetic,
            children: [
              Text(
                '${noteNameForMidi(p.lowMidi!)} – ${noteNameForMidi(p.highMidi!)}',
                style: const TextStyle(
                  color: Studio.textPrimary,
                  fontSize: 15,
                  fontWeight: FontWeight.w800,
                ),
              ),
              const SizedBox(width: 6),
              Text(
                '$span st',
                style: const TextStyle(color: Studio.textFaint, fontSize: 11),
              ),
              const Spacer(),
              CapLabel('Range'),
            ],
          ),
          const SizedBox(height: 7),
          SizedBox(
            height: 8,
            child: CustomPaint(size: Size.infinite, painter: _RangeBar(p)),
          ),
        ],
      ),
    );
  }
}

class _RangeBar extends CustomPainter {
  _RangeBar(this.p);

  final VoiceProfile p;

  @override
  void paint(Canvas canvas, Size size) {
    final lo = p.lowMidi!, hi = p.highMidi!;
    final span = (hi - lo).clamp(1, 1 << 30);
    double x(int midi) => ((midi - lo) / span).clamp(0.0, 1.0) * size.width;
    final r = RRect.fromRectAndRadius(
      Offset(0, size.height / 2 - 1.5) & Size(size.width, 3),
      const Radius.circular(2),
    );
    canvas.drawRRect(r, Paint()..color = Studio.inset);
    // Tessitura highlight.
    if (p.tessLoMidi != null && p.tessHiMidi != null) {
      final tess = RRect.fromRectAndRadius(
        Rect.fromLTRB(
          x(p.tessLoMidi!),
          size.height / 2 - 2.5,
          x(p.tessHiMidi!),
          size.height / 2 + 2.5,
        ),
        const Radius.circular(3),
      );
      canvas.drawRRect(tess, Paint()..color = Studio.accent);
    }
    // End caps.
    final cap = Paint()..color = Studio.textSecondary;
    canvas.drawCircle(Offset(2, size.height / 2), 2.5, cap);
    canvas.drawCircle(Offset(size.width - 2, size.height / 2), 2.5, cap);
    // Passaggio marker.
    if (p.passaggioMidi != null) {
      final px = x(p.passaggioMidi!);
      canvas.drawRect(
        Rect.fromLTWH(px - 1, 0, 2, size.height),
        Paint()..color = Studio.accentBright,
      );
    }
  }

  @override
  bool shouldRepaint(_RangeBar old) => old.p != p;
}

class _RegisterBar extends StatelessWidget {
  const _RegisterBar({required this.reg, required this.onTap});

  final RegisterMap reg;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final mix = Color.lerp(_chest, _head, 0.5)!;
    int pct(double v) => v.round();
    return _Tappable(
      onTap: onTap,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              CapLabel('Register'),
              const Spacer(),
              Text(
                'chest ${pct(reg.chestPct)} · mix ${pct(reg.mixPct)} · head ${pct(reg.headPct)}',
                style: const TextStyle(color: Studio.textFaint, fontSize: 10),
              ),
            ],
          ),
          const SizedBox(height: 7),
          ClipRRect(
            borderRadius: BorderRadius.circular(3),
            child: SizedBox(
              height: 7,
              child: Row(
                children: [
                  Expanded(
                    flex: (reg.chestPct * 10).round().clamp(0, 1000),
                    child: ColoredBox(color: _chest),
                  ),
                  Expanded(
                    flex: (reg.mixPct * 10).round().clamp(0, 1000),
                    child: ColoredBox(color: mix),
                  ),
                  Expanded(
                    flex: (reg.headPct * 10).round().clamp(0, 1000),
                    child: ColoredBox(color: _head),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// One scorecard cell: label, big value, sub-line, an optional pass/warn dot,
/// tappable to open the matching dock tab.
class _Metric extends StatelessWidget {
  const _Metric({
    required this.label,
    required this.value,
    required this.sub,
    this.dot,
    this.onTap,
  });

  final String label;
  final String value;
  final String sub;
  final Color? dot;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    return _Tappable(
      onTap: onTap,
      child: Container(
        padding: const EdgeInsets.fromLTRB(10, 9, 10, 9),
        decoration: BoxDecoration(
          color: Studio.inset,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(color: Studio.border),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                CapLabel(label),
                const Spacer(),
                if (dot != null)
                  Container(
                    width: 7,
                    height: 7,
                    decoration: BoxDecoration(
                      color: dot,
                      shape: BoxShape.circle,
                    ),
                  ),
              ],
            ),
            const SizedBox(height: 4),
            Text(
              value,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 16,
                fontWeight: FontWeight.w800,
              ),
            ),
            Text(
              sub,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(color: Studio.textFaint, fontSize: 9.5),
            ),
          ],
        ),
      ),
    );
  }
}

class _SectionLabel extends StatelessWidget {
  const _SectionLabel(this.text);
  final String text;
  @override
  Widget build(BuildContext context) => Text(
    text.toUpperCase(),
    style: const TextStyle(
      color: Studio.textSecondary,
      fontSize: 10,
      fontWeight: FontWeight.w700,
      letterSpacing: 0.8,
    ),
  );
}

/// Wraps a child in a click cursor + tap, only when [onTap] is non-null.
class _Tappable extends StatelessWidget {
  const _Tappable({required this.child, this.onTap});
  final Widget child;
  final VoidCallback? onTap;
  @override
  Widget build(BuildContext context) {
    if (onTap == null) return child;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        behavior: HitTestBehavior.opaque,
        child: child,
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty(this.text, {this.inset = true, this.spinner = false});

  final String text;
  final bool inset;
  final bool spinner;

  @override
  Widget build(BuildContext context) {
    final content = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (spinner) ...[
          const SizedBox(
            width: 12,
            height: 12,
            child: CircularProgressIndicator(
              strokeWidth: 2,
              valueColor: AlwaysStoppedAnimation(Studio.textFaint),
            ),
          ),
          const SizedBox(width: 8),
        ],
        Flexible(
          child: Text(
            text,
            textAlign: TextAlign.center,
            style: const TextStyle(color: Studio.textFaint, fontSize: 12),
          ),
        ),
      ],
    );
    // Always center (horizontally + vertically); only pad in the inset variant.
    final body = inset
        ? Padding(padding: const EdgeInsets.all(20), child: content)
        : content;
    return Center(child: body);
  }
}
