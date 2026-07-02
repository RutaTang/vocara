import 'dart:async';

import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:go_router/go_router.dart';
import 'package:provider/provider.dart';

import '../../data/services/app_settings_service.dart';
import '../../domain/models/audio_device.dart';
import '../core/themes/studio.dart';

/// Global app settings — audio device selection (output now; input lands in a later phase).
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({super.key});

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    final settings = context.read<AppSettingsService>();
    // Refresh once on open, then poll so plugging/unplugging a device updates the lists live
    // (refreshDevices only rebuilds when a set actually changed). The first refresh is deferred
    // so its notifyListeners() doesn't fire during this build (initState runs mid-build).
    Future.microtask(settings.refreshDevices);
    _poll = Timer.periodic(
      const Duration(milliseconds: 1500),
      (_) => settings.refreshDevices(),
    );
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final settings = context.watch<AppSettingsService>();
    return Scaffold(
      backgroundColor: Studio.background,
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const _Header(),
          Expanded(
            child: ListView(
              padding: const EdgeInsets.fromLTRB(24, 20, 24, 24),
              children: [
                const _SectionLabel('Audio Output'),
                const SizedBox(height: 10),
                _DeviceCard(
                  devices: settings.outputDevices,
                  selectedUid: settings.outputDeviceUid,
                  lastKnownName: settings.outputDeviceName,
                  defaultSubtitle: 'Follow the macOS output device',
                  onSelect: settings.setOutputDevice,
                  onRefresh: settings.refreshDevices,
                ),
                const SizedBox(height: 24),
                const _SectionLabel('Audio Input'),
                const SizedBox(height: 10),
                _DeviceCard(
                  devices: settings.inputDevices,
                  selectedUid: settings.inputDeviceUid,
                  lastKnownName: settings.inputDeviceName,
                  defaultSubtitle: 'Follow the macOS input device',
                  onSelect: settings.setInputDevice,
                  onRefresh: settings.refreshDevices,
                ),
                if (settings.bluetoothInOutConflict) ...[
                  const SizedBox(height: 12),
                  const _WarningBanner(
                    'Input and output are the same Bluetooth device. Bluetooth can\'t do '
                    'high-quality output and microphone at once, so recording will drop the '
                    'sound quality. Pick a different microphone (e.g. the built-in mic).',
                  ),
                ],
                const SizedBox(height: 24),
                const _SectionLabel('Monitoring'),
                const SizedBox(height: 10),
                _MonitorReverbCard(
                  wet: settings.monitorReverbWet,
                  onChanged: settings.setMonitorReverb,
                ),
                const SizedBox(height: 14),
                Text(
                  'The engine runs at ${(settings.engineSampleRate / 1000).toStringAsFixed(1)} kHz; '
                  'devices are resampled to/from this rate automatically. The input device is '
                  'used for recording and live monitoring.',
                  style: const TextStyle(
                    color: Studio.textFaint,
                    fontSize: 12,
                    height: 1.45,
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header();

  @override
  Widget build(BuildContext context) {
    return Container(
      height: 56,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: const BoxDecoration(
        color: Studio.surface,
        border: Border(bottom: BorderSide(color: Studio.border)),
      ),
      child: Row(
        children: [
          _IconButton(icon: FIcons.chevronLeft, onTap: () => context.pop()),
          const SizedBox(width: 6),
          const Text(
            'Settings',
            style: TextStyle(
              color: Studio.textPrimary,
              fontSize: 18,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.3,
            ),
          ),
        ],
      ),
    );
  }
}

class _SectionLabel extends StatelessWidget {
  const _SectionLabel(this.text);
  final String text;

  @override
  Widget build(BuildContext context) {
    return Text(
      text.toUpperCase(),
      style: const TextStyle(
        color: Studio.textSecondary,
        fontSize: 11,
        fontWeight: FontWeight.w700,
        letterSpacing: 1.2,
      ),
    );
  }
}

class _DeviceCard extends StatelessWidget {
  const _DeviceCard({
    required this.devices,
    required this.selectedUid,
    required this.lastKnownName,
    required this.defaultSubtitle,
    required this.onSelect,
    required this.onRefresh,
  });

  final List<AudioDevice> devices;
  final String? selectedUid;

  /// Last-known display name of the persisted device — labels the offline
  /// pseudo-row when the device is unplugged (fallback: the raw UID).
  final String? lastKnownName;
  final String defaultSubtitle;
  final ValueChanged<String?> onSelect;
  final VoidCallback onRefresh;

  @override
  Widget build(BuildContext context) {
    final hasExplicit = selectedUid != null && selectedUid!.isNotEmpty;
    // The persisted device is unplugged: the engine runs on the system default,
    // but the choice is kept (and restored when the device returns).
    final missing = hasExplicit && !devices.any((d) => d.uid == selectedUid);

    return Container(
      decoration: BoxDecoration(
        color: Studio.surfaceRaised,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      clipBehavior: Clip.antiAlias,
      child: Column(
        children: [
          // "System Default" follows whatever the OS picks. While the selected
          // device is unplugged it is the effective route, so say so.
          _DeviceRow(
            title: 'System Default',
            subtitle: missing
                ? '$defaultSubtitle · currently active'
                : defaultSubtitle,
            selected: !hasExplicit,
            onTap: () => onSelect(null),
          ),
          for (final d in devices) ...[
            const Divider(height: 1, color: Studio.border),
            _DeviceRow(
              title: d.name,
              subtitle: _subtitle(d),
              selected: hasExplicit && d.uid == selectedUid,
              onTap: () => onSelect(d.uid),
            ),
          ],
          if (missing) ...[
            // The unplugged persisted selection: still the checked choice, but
            // disabled (not tappable) and marked as unavailable.
            const Divider(height: 1, color: Studio.border),
            _DeviceRow(
              title: (lastKnownName?.isNotEmpty ?? false)
                  ? lastKnownName!
                  : selectedUid!,
              subtitle: 'Unavailable · using system default',
              selected: true,
              enabled: false,
            ),
          ],
          const Divider(height: 1, color: Studio.border),
          _RefreshRow(onTap: onRefresh),
        ],
      ),
    );
  }

  String _subtitle(AudioDevice d) {
    final parts = <String>[];
    if (d.rate > 0) parts.add('${(d.rate / 1000).toStringAsFixed(1)} kHz');
    if (d.isDefault) parts.add('system default');
    return parts.join(' · ');
  }
}

class _DeviceRow extends StatelessWidget {
  const _DeviceRow({
    required this.title,
    required this.subtitle,
    required this.selected,
    this.onTap,
    this.enabled = true,
  });

  final String title;
  final String subtitle;
  final bool selected;
  final VoidCallback? onTap;

  /// False renders the row dimmed and non-tappable (the unplugged persisted
  /// selection: still checked, but not a live route to pick).
  final bool enabled;

  @override
  Widget build(BuildContext context) {
    // Dim everything on a disabled row, including the selection accents.
    final accent = enabled ? Studio.accent : Studio.textFaint;
    return InkWell(
      onTap: enabled ? onTap : null,
      hoverColor: Studio.hover,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
        child: Row(
          children: [
            Icon(
              FIcons.speaker,
              size: 16,
              color: selected ? accent : Studio.textFaint,
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      color: !enabled
                          ? Studio.textFaint
                          : selected
                          ? Studio.textPrimary
                          : Studio.textSecondary,
                      fontSize: 13.5,
                      fontWeight: selected ? FontWeight.w600 : FontWeight.w500,
                    ),
                  ),
                  if (subtitle.isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(
                      subtitle,
                      style: const TextStyle(
                        color: Studio.textFaint,
                        fontSize: 11,
                      ),
                    ),
                  ],
                ],
              ),
            ),
            if (selected) Icon(FIcons.check, size: 16, color: accent),
          ],
        ),
      ),
    );
  }
}

class _RefreshRow extends StatelessWidget {
  const _RefreshRow({required this.onTap});
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return InkWell(
      onTap: onTap,
      hoverColor: Studio.hover,
      child: const Padding(
        padding: EdgeInsets.symmetric(horizontal: 14, vertical: 11),
        child: Row(
          children: [
            Icon(FIcons.rotateCw, size: 14, color: Studio.textSecondary),
            SizedBox(width: 12),
            Text(
              'Refresh devices',
              style: TextStyle(color: Studio.textSecondary, fontSize: 12.5),
            ),
          ],
        ),
      ),
    );
  }
}

class _WarningBanner extends StatelessWidget {
  const _WarningBanner(this.text);
  final String text;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
      decoration: BoxDecoration(
        color: Studio.record.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.record.withValues(alpha: 0.4)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Icon(FIcons.triangleAlert, size: 16, color: Studio.record),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              text,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 12,
                height: 1.4,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _MonitorReverbCard extends StatelessWidget {
  const _MonitorReverbCard({required this.wet, required this.onChanged});

  final double wet;
  final ValueChanged<double> onChanged;

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: BoxDecoration(
        color: Studio.surfaceRaised,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      padding: const EdgeInsets.fromLTRB(14, 8, 14, 8),
      child: Row(
        children: [
          const Icon(FIcons.waves, size: 16, color: Studio.textFaint),
          const SizedBox(width: 12),
          const SizedBox(
            width: 52,
            child: Text(
              'Reverb',
              style: TextStyle(color: Studio.textSecondary, fontSize: 13),
            ),
          ),
          Expanded(
            child: SliderTheme(
              data: SliderThemeData(
                trackHeight: 3,
                activeTrackColor: Studio.accent,
                inactiveTrackColor: Studio.inset,
                thumbColor: Studio.accent,
                overlayShape: const RoundSliderOverlayShape(overlayRadius: 12),
                thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 6),
              ),
              child: Slider(value: wet.clamp(0.0, 1.0), onChanged: onChanged),
            ),
          ),
          SizedBox(
            width: 38,
            child: Text(
              '${(wet * 100).round()}%',
              textAlign: TextAlign.right,
              style: const TextStyle(color: Studio.textFaint, fontSize: 12),
            ),
          ),
        ],
      ),
    );
  }
}

class _IconButton extends StatelessWidget {
  const _IconButton({required this.icon, required this.onTap});
  final IconData icon;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onTap,
        child: Container(
          width: 34,
          height: 34,
          alignment: Alignment.center,
          decoration: BoxDecoration(borderRadius: BorderRadius.circular(7)),
          child: Icon(icon, size: 18, color: Studio.textSecondary),
        ),
      ),
    );
  }
}
