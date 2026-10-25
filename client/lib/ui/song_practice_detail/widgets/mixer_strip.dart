import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';

import '../../../data/services/app_settings_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/fx_bus.dart';
import '../../core/themes/studio.dart';
import '../../core/widgets/studio_controls.dart';
import '../../core/widgets/tick_boundary.dart';
import '../view_model/song_practice_detail_view_model.dart';
import 'track_context_menu.dart';

/// The MIXER tab of the bottom dock: one console-style channel strip per track
/// (mute / solo / a vertical volume fader / delete), aligned left-to-right like
/// a mixing desk. The selected channel is tinted; clicking a strip selects it.
class MixerStrip extends StatelessWidget {
  const MixerStrip({
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
    final buses = vm.mixer.fxBuses;
    if (tracks.isEmpty && buses.isEmpty) {
      return const Center(
        child: Text(
          'No tracks yet',
          style: TextStyle(color: Studio.textFaint, fontSize: 12),
        ),
      );
    }
    return ListView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.all(12),
      children: [
        // Mic input strip: the live input's meter + record gain + monitor (hear-yourself) gain.
        // Only meaningful on the native engine (device I/O + monitoring).
        if (vm.mixer.supportsEffects) ...[
          _MicInputChannel(vm: vm),
          Container(
            width: 1,
            margin: const EdgeInsets.fromLTRB(2, 14, 12, 14),
            color: Studio.border,
          ),
        ],
        for (final t in tracks)
          _Channel(
            track: t,
            selected: vm.selectedTrackId == t.id && vm.selectedBusId == null,
            // Backing tracks stay dry → no output routing chip.
            buses: t.isBacking ? const [] : buses,
            routedBusId: t.busId,
            onRoute: (id) => vm.mixer.setTrackBus(t.id, id),
            onSelect: () => vm.selectTrack(t.id),
            onMute: () => vm.mixer.toggleMute(t.id),
            onSolo: () => vm.mixer.toggleSolo(t.id),
            onVolume: (v) => vm.mixer.setTrackVolumeLive(t.id, v),
            onVolumeCommit: vm.mixer.commitTrackVolume,
            onPan: (p) => vm.mixer.setTrackPanLive(t.id, p),
            onPanCommit: vm.mixer.commitTrackVolume,
            onDelete: () => onDeleteTrack(t),
          ),
        // FX buses (DAW groups). Only meaningful on the native effects engine.
        if (vm.mixer.supportsEffects) ...[
          // Separator between the vocal takes and the bus section (incl. + Bus).
          Container(
            width: 1,
            margin: const EdgeInsets.fromLTRB(2, 14, 12, 14),
            color: Studio.border,
          ),
          for (final b in buses)
            _BusChannel(
              bus: b,
              selected: vm.selectedBusId == b.id,
              buses: buses,
              onSelect: () => vm.selectBus(b.id),
              onRoute: (out) => vm.mixer.setBusOutput(b.id, out),
              onMute: () => vm.mixer.toggleBusMute(b.id),
              onSolo: () => vm.mixer.toggleBusSolo(b.id),
              onVolume: (v) => vm.mixer.setBusVolumeLive(b.id, v),
              onVolumeCommit: vm.commitFx,
              onPan: (p) => vm.mixer.setBusPanLive(b.id, p),
              onPanCommit: vm.commitFx,
              onRename: () async {
                final name = await showRenameDialog(
                  context,
                  current: b.name,
                  title: 'Rename bus',
                  hint: 'Bus name',
                );
                if (name != null) await vm.mixer.renameBus(b.id, name);
              },
              // Asks first, like the timeline lane's Delete: the bus's FX
              // chain and every send into it go with it, with no undo.
              onDelete: () async {
                if (await confirmDeleteBus(context, b.name)) {
                  await vm.mixer.deleteBus(b.id);
                }
              },
            ),
          _AddBusButton(onTap: () => vm.mixer.createBus()),
        ],
        // The master bus strip — the whole mix sums here. Set apart on the right.
        Container(
          width: 1,
          margin: const EdgeInsets.fromLTRB(2, 14, 14, 14),
          color: Studio.border,
        ),
        _MasterChannel(
          value: vm.mixer.masterVolume,
          onVolume: vm.mixer.setMasterVolumeLive,
          onVolumeCommit: vm.mixer.commitMasterVolume,
        ),
      ],
    );
  }
}

/// Display name of the bus [id] routes to, or "Master" when it's null/missing.
String _busName(List<FxBus> buses, String? id) {
  if (id == null) return 'Master';
  for (final b in buses) {
    if (b.id == id) return b.name;
  }
  return 'Master';
}

/// Inner strip height (px) below which a strip switches to its scrolling
/// fallback. Derived from the tallest strip's fixed rows: title ≈14, M/S row
/// 20, pan knob 26 + its label ≈14, dB readout ≈13, routing chip ≈19, delete
/// icon 14 and the SizedBox gaps between them (10+10+10+8+8+10 = 56) ≈ 176 in
/// total, plus ~56px of minimum usable fader travel.
const double _kStripMinContentHeight = 232;

/// Fixed fader height used in the squeezed scrolling fallback, where the
/// column's height is unbounded so Expanded cannot size the fader.
const double _kSqueezedFaderHeight = 120;

/// Lays out a channel strip's column. At normal dock heights it returns the
/// plain [Column] — the fader's Expanded stretches to fill, console style.
/// When the strip is squeezed below [_kStripMinContentHeight] it swaps to a
/// vertically scrollable column (with the fader at a fixed height) instead of
/// collapsing the fader to zero and clipping the dB readout / routing chip /
/// delete control with a RenderFlex overflow.
///
/// Deliberately NOT SingleChildScrollView + IntrinsicHeight: the faders are
/// LayoutBuilder-based, and LayoutBuilder does not support intrinsic
/// measurement — every layout would throw in asserts-enabled builds.
class _StripBody extends StatelessWidget {
  const _StripBody({required this.builder});

  /// Builds the strip column's children. [squeezed] is true in the scrolling
  /// fallback, where the fader slot must use a fixed height (see
  /// [_stripFaderSlot]).
  final List<Widget> Function(bool squeezed) builder;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxHeight >= _kStripMinContentHeight) {
          return Column(
            crossAxisAlignment: CrossAxisAlignment.center,
            children: builder(false),
          );
        }
        return SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: builder(true),
          ),
        );
      },
    );
  }
}

/// The fader slot inside a strip column: fills the leftover height at normal
/// dock heights, a fixed (still usable) height in the squeezed scrolling
/// fallback where the column's height is unbounded.
Widget _stripFaderSlot({required bool squeezed, required Widget child}) {
  return squeezed
      ? SizedBox(height: _kSqueezedFaderHeight, child: child)
      : Expanded(child: child);
}

/// A routing chip ("→ Master" / "→ Bus name") that opens a destination menu.
class _RouteChip extends StatelessWidget {
  const _RouteChip({
    required this.label,
    required this.options,
    required this.accent,
  });

  /// The current destination label.
  final String label;

  /// (label, onTap) destinations, in order (Master first).
  final List<({String label, bool selected, VoidCallback onTap})> options;
  final Color accent;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => showStudioMenu(context, menuAnchorBelow(context), [
          for (final o in options)
            StudioMenuItem(
              label: o.label,
              selected: o.selected,
              onTap: o.onTap,
            ),
        ], width: 160),
        child: Container(
          width: double.infinity,
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
          decoration: BoxDecoration(
            color: Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(4),
            border: Border.all(color: Studio.border),
          ),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Flexible(
                child: Text(
                  '→ $label',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  textAlign: TextAlign.center,
                  style: TextStyle(
                    color: accent.withValues(alpha: 0.9),
                    fontSize: 9,
                    fontWeight: FontWeight.w600,
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

/// An FX bus channel strip: select (opens its FX in the inspector), mute/solo,
/// output routing, fader, rename / delete.
class _BusChannel extends StatelessWidget {
  const _BusChannel({
    required this.bus,
    required this.selected,
    required this.buses,
    required this.onSelect,
    required this.onRoute,
    required this.onMute,
    required this.onSolo,
    required this.onVolume,
    required this.onVolumeCommit,
    required this.onPan,
    required this.onPanCommit,
    required this.onRename,
    required this.onDelete,
  });

  final FxBus bus;
  final bool selected;
  final List<FxBus> buses;
  final VoidCallback onSelect;
  final ValueChanged<String?> onRoute;
  final VoidCallback onMute;
  final VoidCallback onSolo;
  final ValueChanged<double> onVolume;
  final VoidCallback onVolumeCommit;
  final ValueChanged<double> onPan;
  final VoidCallback onPanCommit;
  final VoidCallback onRename;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    final accent = bus.colorValue != null
        ? Color(bus.colorValue!)
        : Studio.accent;
    final outName = _busName(buses, bus.outputBusId);
    return GestureDetector(
      onTap: onSelect,
      // At the click, like a context menu: anchoring below this full-height
      // strip opened it at the bottom of the mixer.
      onSecondaryTapDown: (d) => showStudioMenu(context, d.globalPosition, [
        StudioMenuItem(label: 'Rename', onTap: onRename),
        StudioMenuItem(label: 'Delete bus', danger: true, onTap: onDelete),
      ], width: 160),
      child: Container(
        width: 92,
        margin: const EdgeInsets.only(right: 10),
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 12),
        decoration: BoxDecoration(
          color: selected ? Studio.hover : Studio.panel,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(
            color: selected ? accent.withValues(alpha: 0.6) : Studio.border,
          ),
        ),
        child: _StripBody(
          builder: (squeezed) => [
            Text(
              bus.name,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              textAlign: TextAlign.center,
              style: TextStyle(
                color: accent,
                fontSize: 11,
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(height: 10),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                _Toggle(
                  label: 'M',
                  active: bus.muted,
                  activeColor: Studio.record,
                  onTap: onMute,
                ),
                const SizedBox(width: 6),
                _Toggle(
                  label: 'S',
                  active: bus.soloed,
                  activeColor: Studio.accent,
                  onTap: onSolo,
                ),
              ],
            ),
            const SizedBox(height: 10),
            StudioPanKnob(
              value: bus.pan,
              accent: accent,
              size: 26,
              onChanged: onPan,
              onCommit: onPanCommit,
            ),
            const SizedBox(height: 10),
            _stripFaderSlot(
              squeezed: squeezed,
              child: Center(
                child: SizedBox(
                  width: 46,
                  child: _VerticalFader(
                    value: bus.volume,
                    accent: accent,
                    onChanged: onVolume,
                    onChangeEnd: onVolumeCommit,
                  ),
                ),
              ),
            ),
            const SizedBox(height: 8),
            Text(
              '${_Channel._formatDb(bus.volume)} dB',
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 10,
                fontWeight: FontWeight.w600,
                fontFeatures: [FontFeature.tabularFigures()],
              ),
            ),
            const SizedBox(height: 8),
            _RouteChip(
              label: outName,
              accent: accent,
              options: [
                (
                  label: 'Master',
                  selected: bus.outputBusId == null,
                  onTap: () => onRoute(null),
                ),
                for (final b in buses)
                  if (b.id != bus.id)
                    (
                      label: b.name,
                      selected: bus.outputBusId == b.id,
                      onTap: () => onRoute(b.id),
                    ),
              ],
            ),
            const SizedBox(height: 10),
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: onDelete,
                child: const Icon(
                  FIcons.trash2,
                  size: 14,
                  color: Studio.textFaint,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// The "+ Bus" button that adds a new FX bus to the mixer.
class _AddBusButton extends StatelessWidget {
  const _AddBusButton({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 56,
          margin: const EdgeInsets.only(right: 10),
          decoration: BoxDecoration(
            color: Studio.panel.withValues(alpha: 0.4),
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: Studio.border),
          ),
          child: const Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Icon(FIcons.plus, size: 16, color: Studio.textSecondary),
              SizedBox(height: 6),
              Text(
                'BUS',
                style: TextStyle(
                  color: Studio.textFaint,
                  fontSize: 9,
                  fontWeight: FontWeight.w700,
                  letterSpacing: 0.5,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The master bus channel: like a track strip but it controls the overall output
/// gain (no mute/solo, no delete). Accent-tinted and set apart on the right.
class _MasterChannel extends StatelessWidget {
  const _MasterChannel({
    required this.value,
    required this.onVolume,
    required this.onVolumeCommit,
  });

  final double value;
  final ValueChanged<double> onVolume;
  final VoidCallback onVolumeCommit;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: 92,
      margin: const EdgeInsets.only(right: 10),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 12),
      decoration: BoxDecoration(
        color: Studio.accent.withValues(alpha: 0.06),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.accent.withValues(alpha: 0.4)),
      ),
      child: _StripBody(
        builder: (squeezed) => [
          const Text(
            'MASTER',
            style: TextStyle(
              color: Studio.accent,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.5,
            ),
          ),
          // Pad where the M/S row + pan knob sit on a track strip, so the
          // fader lines up with the channel faders.
          const SizedBox(height: 92),
          _stripFaderSlot(
            squeezed: squeezed,
            child: Center(
              child: SizedBox(
                width: 46,
                child: _VerticalFader(
                  value: value,
                  accent: Studio.accent,
                  onChanged: onVolume,
                  onChangeEnd: onVolumeCommit,
                ),
              ),
            ),
          ),
          const SizedBox(height: 8),
          Text(
            '${_Channel._formatDb(value)} dB',
            style: const TextStyle(
              color: Studio.textSecondary,
              fontSize: 10,
              fontWeight: FontWeight.w600,
              fontFeatures: [FontFeature.tabularFigures()],
            ),
          ),
          // Pad where the delete button sits on a track strip.
          const SizedBox(height: 24),
        ],
      ),
    );
  }
}

/// The mic-input channel strip: the shared input level meter + a record-gain fader (how loud the
/// captured voice records) + a monitor fader (how loud you hear yourself; 0 = off, e.g. on a
/// speaker where it would feed back). Wired to the global audio settings.
class _MicInputChannel extends StatelessWidget {
  const _MicInputChannel({required this.vm});

  final SongPracticeDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    final settings = vm.appSettings;
    return ListenableBuilder(
      listenable: settings,
      builder: (context, _) {
        const minDb = AppSettingsService.recordGainMinDb;
        const maxDb = AppSettingsService.recordGainMaxDb;
        final recPos = ((settings.recordGainDb - minDb) / (maxDb - minDb))
            .clamp(0.0, 1.0);
        return Container(
          width: 138,
          margin: const EdgeInsets.only(right: 10),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 12),
          decoration: BoxDecoration(
            color: Studio.record.withValues(alpha: 0.06),
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: Studio.record.withValues(alpha: 0.4)),
          ),
          child: _StripBody(
            builder: (squeezed) => [
              const Text(
                'MIC',
                style: TextStyle(
                  color: Studio.record,
                  fontSize: 11,
                  fontWeight: FontWeight.w700,
                  letterSpacing: 0.5,
                ),
              ),
              const SizedBox(height: 8),
              // Shared input level meter (monitor + record are the same mic).
              // It moves at 20 Hz while the mic is live; the TickBoundary (in
              // the fixed-size box) keeps that rebuild from re-laying-out the
              // strip column's LayoutBuilder and the screen around it.
              ClipRRect(
                borderRadius: BorderRadius.circular(3),
                child: SizedBox(
                  height: 6,
                  width: double.infinity,
                  child: ColoredBox(
                    color: Studio.inset,
                    child: TickBoundary(
                      child: Align(
                        alignment: Alignment.centerLeft,
                        child: ValueListenableBuilder<double>(
                          valueListenable: vm.recording.inputLevel,
                          builder: (_, v, _) => FractionallySizedBox(
                            widthFactor: v.clamp(0.0, 1.0),
                            heightFactor: 1.0,
                            child: const ColoredBox(color: Studio.record),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 12),
              _stripFaderSlot(
                squeezed: squeezed,
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Expanded(
                      child: _FaderColumn(
                        label: 'REC',
                        value: recPos,
                        accent: Studio.record,
                        readout: '${settings.recordGainDb.round()} dB',
                        onChanged: (p) => settings.setRecordGainDbLive(
                          minDb + p * (maxDb - minDb),
                        ),
                        onEnd: settings.commitAudioGains,
                      ),
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      // A locked MON says why, and where to unlock it when it can be.
                      child: Tooltip(
                        message: settings.monitorAllowed
                            ? ''
                            : settings.suggestHeadphonesMark
                            ? 'Hearing yourself is off here so speakers can\'t '
                                  'feed back. Listening on headphones? Turn on '
                                  '"Headphones only" in Settings.'
                            : 'Hearing yourself is off on this output so '
                                  'speakers can\'t feed back.',
                        child: _FaderColumn(
                          label: 'MON',
                          enabled: settings.monitorAllowed,
                          value: settings.monitorAllowed
                              ? settings.monitorGain
                              : 0.0,
                          accent: Studio.accent,
                          readout: !settings.monitorAllowed
                              ? 'spkr'
                              : (settings.monitorGain <= 0.001
                                    ? 'off'
                                    : '${(settings.monitorGain * 100).round()}%'),
                          onChanged: settings.setMonitorGainLive,
                          onEnd: settings.commitAudioGains,
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

/// A labelled vertical fader (label on top, fader, value readout below).
class _FaderColumn extends StatelessWidget {
  const _FaderColumn({
    required this.label,
    required this.value,
    required this.accent,
    required this.readout,
    required this.onChanged,
    required this.onEnd,
    this.enabled = true,
  });

  final String label;
  final double value;
  final Color accent;
  final String readout;
  final ValueChanged<double> onChanged;
  final VoidCallback onEnd;
  final bool enabled;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Text(
          label,
          style: TextStyle(
            color: accent.withValues(alpha: enabled ? 0.9 : 0.35),
            fontSize: 9,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.5,
          ),
        ),
        const SizedBox(height: 6),
        Expanded(
          child: Center(
            child: SizedBox(
              width: 40,
              // Disabled (e.g. monitor on a speaker → feedback): grey it out + ignore input.
              child: IgnorePointer(
                ignoring: !enabled,
                child: Opacity(
                  opacity: enabled ? 1.0 : 0.35,
                  child: _VerticalFader(
                    value: value,
                    accent: accent,
                    onChanged: onChanged,
                    onChangeEnd: onEnd,
                  ),
                ),
              ),
            ),
          ),
        ),
        const SizedBox(height: 6),
        Text(
          readout,
          style: TextStyle(
            color: enabled ? Studio.textSecondary : Studio.textFaint,
            fontSize: 9,
            fontWeight: FontWeight.w600,
            fontFeatures: const [FontFeature.tabularFigures()],
          ),
        ),
      ],
    );
  }
}

class _Channel extends StatelessWidget {
  const _Channel({
    required this.track,
    required this.selected,
    required this.buses,
    required this.routedBusId,
    required this.onRoute,
    required this.onSelect,
    required this.onMute,
    required this.onSolo,
    required this.onVolume,
    required this.onVolumeCommit,
    required this.onPan,
    required this.onPanCommit,
    required this.onDelete,
  });

  final AudioTrack track;
  final bool selected;
  final List<FxBus> buses;
  final String? routedBusId;
  final ValueChanged<String?> onRoute;
  final VoidCallback onSelect;
  final VoidCallback onMute;
  final VoidCallback onSolo;
  final ValueChanged<double> onVolume;
  final VoidCallback onVolumeCommit;
  final ValueChanged<double> onPan;
  final VoidCallback onPanCommit;
  final VoidCallback onDelete;

  static String _formatDb(double v) {
    if (v <= 0.0001) return '-∞';
    final db = 20 * math.log(v) / math.ln10;
    return db >= -0.05 ? '0.0' : db.toStringAsFixed(1);
  }

  @override
  Widget build(BuildContext context) {
    final Color accent;
    if (track.colorValue != null) {
      accent = Color(track.colorValue!);
    } else if (track.isBacking) {
      accent = Studio.accent;
    } else {
      accent = defaultTakeColor(track.id);
    }
    return GestureDetector(
      onTap: onSelect,
      child: Container(
        width: 92,
        margin: const EdgeInsets.only(right: 10),
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 12),
        decoration: BoxDecoration(
          color: selected ? Studio.hover : Studio.panel,
          borderRadius: BorderRadius.circular(8),
          border: Border.all(
            color: selected ? accent.withValues(alpha: 0.6) : Studio.border,
          ),
        ),
        child: _StripBody(
          builder: (squeezed) => [
            Text(
              track.displayName,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              textAlign: TextAlign.center,
              style: const TextStyle(
                color: Studio.textPrimary,
                fontSize: 11,
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(height: 10),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                _Toggle(
                  label: 'M',
                  active: track.muted,
                  activeColor: Studio.record,
                  onTap: onMute,
                ),
                const SizedBox(width: 6),
                _Toggle(
                  label: 'S',
                  active: track.soloed,
                  activeColor: Studio.accent,
                  onTap: onSolo,
                ),
              ],
            ),
            const SizedBox(height: 10),
            StudioPanKnob(
              value: track.pan,
              accent: accent,
              size: 26,
              onChanged: onPan,
              onCommit: onPanCommit,
            ),
            const SizedBox(height: 10),
            _stripFaderSlot(
              squeezed: squeezed,
              child: Center(
                child: SizedBox(
                  width: 46,
                  child: _VerticalFader(
                    value: track.volume,
                    accent: accent,
                    onChanged: onVolume,
                    onChangeEnd: onVolumeCommit,
                  ),
                ),
              ),
            ),
            const SizedBox(height: 8),
            Text(
              '${_formatDb(track.volume)} dB',
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 10,
                fontWeight: FontWeight.w600,
                fontFeatures: [FontFeature.tabularFigures()],
              ),
            ),
            // Output routing chip — vocal takes only, when buses exist.
            if (!track.isBacking && buses.isNotEmpty) ...[
              const SizedBox(height: 8),
              _RouteChip(
                label: _busName(buses, routedBusId),
                accent: accent,
                options: [
                  (
                    label: 'Master',
                    selected: routedBusId == null,
                    onTap: () => onRoute(null),
                  ),
                  for (final b in buses)
                    (
                      label: b.name,
                      selected: routedBusId == b.id,
                      onTap: () => onRoute(b.id),
                    ),
                ],
              ),
            ],
            const SizedBox(height: 10),
            MouseRegion(
              cursor: SystemMouseCursors.click,
              child: GestureDetector(
                onTap: onDelete,
                child: const Icon(
                  FIcons.trash2,
                  size: 14,
                  color: Studio.textFaint,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A console-style vertical volume fader: a grooved track with tick marks and a
/// draggable cap. Drag up for louder, down for quieter.
class _VerticalFader extends StatefulWidget {
  const _VerticalFader({
    required this.value,
    required this.accent,
    required this.onChanged,
    required this.onChangeEnd,
  });

  final double value;
  final Color accent;
  final ValueChanged<double> onChanged;
  final VoidCallback onChangeEnd;

  @override
  State<_VerticalFader> createState() => _VerticalFaderState();
}

class _VerticalFaderState extends State<_VerticalFader> {
  /// A drag in flight: it has moved the level and owes its [onChangeEnd].
  bool _active = false;

  @override
  void dispose() {
    // Unmounted mid-drag (a sideways wheel scrolled its strip out of the lazy strip list): a
    // disposed recognizer fires no end, so the level the engine already plays was never saved.
    // Deferred past this frame (the save notifies listeners), as for [StudioFader].
    if (_active) {
      _active = false;
      scheduleMicrotask(widget.onChangeEnd);
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, constraints) {
        final h = constraints.maxHeight;
        const pad = 8.0;
        void update(double dy) {
          // math.max (lower bound only) instead of clamp(1.0, h): when the fader
          // is laid out below 1px (e.g. the master strip squeezed to the minimum
          // dock height) h - 2*pad < 1.0 < h would make clamp's lowerLimit exceed
          // its upperLimit and throw ArgumentError (release too). Identical to the
          // old clamp at usable sizes (h - 2*pad >= 1).
          final span = math.max(1.0, h - 2 * pad);
          widget.onChanged((1 - (dy - pad) / span).clamp(0.0, 1.0));
        }

        return MouseRegion(
          cursor: SystemMouseCursors.resizeUpDown,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (d) {
              _active = true;
              update(d.localPosition.dy);
            },
            onTapUp: (_) {
              _active = false;
              widget.onChangeEnd();
            },
            onVerticalDragUpdate: (d) {
              _active = true;
              update(d.localPosition.dy);
            },
            onVerticalDragEnd: (_) {
              _active = false;
              widget.onChangeEnd();
            },
            child: CustomPaint(
              size: Size.infinite,
              painter: _VFaderPainter(
                value: widget.value.clamp(0.0, 1.0),
                accent: widget.accent,
              ),
            ),
          ),
        );
      },
    );
  }
}

class _VFaderPainter extends CustomPainter {
  _VFaderPainter({required this.value, required this.accent});

  final double value;
  final Color accent;

  static const double _pad = 8;

  @override
  void paint(Canvas canvas, Size size) {
    final cx = size.width / 2;
    final top = _pad;
    final bottom = size.height - _pad;
    final span = bottom - top;
    final capY = bottom - value * span;

    // Groove.
    canvas.drawLine(
      Offset(cx, top),
      Offset(cx, bottom),
      Paint()
        ..color = Studio.gridStrong
        ..strokeWidth = 4
        ..strokeCap = StrokeCap.round,
    );
    // Filled portion below the cap.
    canvas.drawLine(
      Offset(cx, bottom),
      Offset(cx, capY),
      Paint()
        ..color = accent.withValues(alpha: 0.55)
        ..strokeWidth = 4
        ..strokeCap = StrokeCap.round,
    );
    // Scale ticks down the right side (incl. a 0 dB mark at the top).
    final tick = Paint()
      ..color = Studio.border
      ..strokeWidth = 1;
    for (var i = 0; i <= 4; i++) {
      final ty = top + i / 4 * span;
      canvas.drawLine(Offset(cx + 9, ty), Offset(cx + 14, ty), tick);
    }
    // Fader cap (console handle).
    final cap = RRect.fromRectAndRadius(
      Rect.fromCenter(center: Offset(cx, capY), width: 28, height: 13),
      const Radius.circular(3),
    );
    canvas.drawRRect(cap, Paint()..color = Studio.surfaceRaised);
    canvas.drawRRect(
      cap,
      Paint()
        ..color = accent
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5,
    );
    canvas.drawLine(
      Offset(cx - 10, capY),
      Offset(cx + 10, capY),
      Paint()
        ..color = accent
        ..strokeWidth = 2
        ..strokeCap = StrokeCap.round,
    );
  }

  @override
  bool shouldRepaint(covariant _VFaderPainter old) =>
      old.value != value || old.accent != accent;
}

class _Toggle extends StatelessWidget {
  const _Toggle({
    required this.label,
    required this.active,
    required this.activeColor,
    required this.onTap,
  });

  final String label;
  final bool active;
  final Color activeColor;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          width: 24,
          height: 20,
          alignment: Alignment.center,
          decoration: BoxDecoration(
            color: active
                ? activeColor.withValues(alpha: 0.2)
                : Studio.surfaceRaised,
            borderRadius: BorderRadius.circular(4),
            border: Border.all(
              color: active
                  ? activeColor.withValues(alpha: 0.6)
                  : Studio.border,
            ),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? activeColor : Studio.textSecondary,
              fontSize: 11,
              fontWeight: FontWeight.w700,
            ),
          ),
        ),
      ),
    );
  }
}
