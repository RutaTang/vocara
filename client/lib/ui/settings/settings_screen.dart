import 'dart:async';
import 'dart:io';

import 'package:file_selector/file_selector.dart' show getSaveLocation;
import 'package:flutter/material.dart';
import 'package:forui/forui.dart';
import 'package:provider/provider.dart';

import '../../data/db/persistence.dart';
import '../../data/services/app_settings_service.dart';
import '../../data/services/diagnostics_log.dart';
import '../../data/services/graph_audio_playback_service.dart';
import '../../data/services/update_check_service.dart';
import '../../domain/models/audio_device.dart';
import '../../routing/navigation.dart';
import '../../routing/routes.dart';
import '../core/themes/studio.dart';
import '../core/widgets/confirm_dialog.dart';
import '../core/widgets/window_drag_bar.dart';
import '../home/widgets/update_notice.dart';
import 'licenses.dart';

/// Global app settings — audio devices and monitoring, library maintenance
/// (repair, diagnostics export), and the About block (version, updates,
/// licenses).
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    this.pickSavePath,
    this.openUrl,
    this.exportDiagnostics,
  });

  /// Test seam for the macOS save panel (defaults to file_selector's
  /// getSaveLocation): returns the chosen destination path, or null on cancel.
  final Future<String?> Function(String suggestedName)? pickSavePath;

  /// Test seam for opening a URL in the browser (defaults to
  /// [openUrlInBrowser]).
  final Future<bool> Function(String url)? openUrl;

  /// Test seam for producing the diagnostics bundle (defaults to
  /// [DiagnosticsLog.instance]'s exportArchive): returns the bundle's path.
  final Future<String> Function()? exportDiagnostics;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

/// The repair result as one plain sentence with the report's numbers.
///
/// Top-level and testable because the three counts are checked INDEPENDENTLY —
/// integrity, the asset sweep, and the pool's byte-level verification — so
/// combinations the widget test cannot easily stage (a clean database holding a
/// bit-rotted audio file) still have to read as one coherent sentence.
@visibleForTesting
String repairSummary(RepairReport report) {
  // Damaged audio is named on its own, never folded into a count of things
  // that were fixed: nothing was done about those files, and the sentence
  // has to say so.
  final damaged = report.corruptPoolFiles > 0
      ? ' ${_count(report.corruptPoolFiles, 'audio file')} '
            '${report.corruptPoolFiles == 1 ? 'looks' : 'look'} damaged and '
            'may not play correctly; nothing was changed.'
      : '';
  if (report.violationsFound == 0 && report.assetsSwept == 0) {
    // Nothing to fix and a damaged file are both true at once, and the
    // all-clear head then contradicts the very next sentence. Say what the
    // repair DID instead of vouching for a library it just reported damage in.
    final head = report.corruptPoolFiles > 0
        ? 'Repair finished. Nothing needed fixing.'
        : 'Repair finished. Everything looks consistent.';
    return '$head$damaged';
  }
  final parts = <String>[
    'found ${_count(report.violationsFound, 'issue')}',
    if (report.violationsFound > 0) 'repaired ${report.violationsRepaired}',
    'reclaimed ${_count(report.assetsSwept, 'unused file')}',
  ];
  return 'Repair finished: ${parts.join(', ')}.$damaged';
}

String _count(int n, String noun) => n == 1 ? '$n $noun' : '$n ${noun}s';

class _SettingsScreenState extends State<SettingsScreen> {
  Timer? _poll;

  // One maintenance action at a time: each row disables itself while it runs.
  bool _repairing = false;
  bool _exporting = false;

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
    final updates = context.watch<UpdateCheckService>();
    // Only an output no name can tell takes the headphones mark: earbuds need none, speakers
    // take none.
    final toMark = settings.outputMarkable ? settings.outputToMark : null;
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
                if (settings.persistFailed) ...[
                  const _WarningBanner(
                    'Your settings could not be saved to disk. They are in '
                    'effect now, but the app will start with the previous '
                    'ones. Check that the disk is not full or locked.',
                  ),
                  const SizedBox(height: 20),
                ],
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
                if (toMark != null) ...[
                  const SizedBox(height: 12),
                  _HeadphonesCard(
                    device: toMark.name,
                    marked: settings.outputMarkedHeadphones,
                    // The device this card shows, not whatever renders when the tap lands.
                    onChanged: (on) =>
                        settings.setOutputIsHeadphones(toMark.uid, on),
                  ),
                ],
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
                // Only a device with more than one input has a choice to make.
                if (settings.inputChannelCount >= 2) ...[
                  const SizedBox(height: 12),
                  _InputChannelCard(
                    device: settings.inputChannelDeviceName,
                    mode: settings.inputChannel,
                    onSelect: settings.setInputChannel,
                  ),
                ],
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
                const SizedBox(height: 24),
                const _SectionLabel('Maintenance'),
                const SizedBox(height: 10),
                _Card(
                  children: [
                    _ActionRow(
                      icon: FIcons.wrench,
                      title: 'Repair library',
                      subtitle: _repairing
                          ? 'Repairing…'
                          : 'Checks the library for inconsistencies and '
                                'reclaims unused space.',
                      busy: _repairing,
                      onTap: _repairing ? null : _repairLibrary,
                    ),
                    const Divider(height: 1, color: Studio.border),
                    _ActionRow(
                      icon: FIcons.fileDown,
                      title: 'Export diagnostics…',
                      subtitle: _exporting
                          ? 'Exporting…'
                          : 'Saves a text file of technical logs for a bug '
                                'report. It contains no audio.',
                      busy: _exporting,
                      onTap: _exporting ? null : _exportDiagnostics,
                    ),
                  ],
                ),
                const SizedBox(height: 24),
                const _SectionLabel('About'),
                const SizedBox(height: 10),
                _Card(
                  children: [
                    const _InfoRow(
                      icon: FIcons.info,
                      title: 'Vocara',
                      subtitle:
                          'Version ${UpdateCheckService.appVersion} '
                          '(build ${UpdateCheckService.appBuildNumber})',
                    ),
                    const Divider(height: 1, color: Studio.border),
                    _ActionRow(
                      icon: FIcons.refreshCw,
                      title: 'Check for updates',
                      subtitle: _updateSubtitle(updates),
                      busy: updates.checking,
                      onTap: updates.checking ? null : _checkForUpdates,
                    ),
                    const Divider(height: 1, color: Studio.border),
                    _ActionRow(
                      icon: FIcons.scrollText,
                      title: 'Licenses',
                      subtitle: 'Open-source components that ship with Vocara.',
                      onTap: _showLicenses,
                    ),
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// The Check-for-updates row's one-line status: the last result, or what
  /// the row does before any check has run.
  String _updateSubtitle(UpdateCheckService updates) {
    if (updates.checking) return 'Checking…';
    switch (updates.status) {
      case UpdateStatus.updateAvailable:
        return 'Vocara ${updates.available!.version} is available. '
            'Click for the download.';
      case UpdateStatus.upToDate:
        return 'You have the latest version.';
      case UpdateStatus.unknown:
        return updates.lastCheckedAt == null
            ? 'Compares this build against the latest release.'
            : 'Could not reach the update server. You can try again later.';
    }
  }

  Future<void> _checkForUpdates() async {
    final updates = context.read<UpdateCheckService>();
    await updates.check();
    if (!mounted) return;
    final info = updates.available;
    if (info != null) {
      await showUpdateDownloadDialog(context, info, openUrl: widget.openUrl);
    }
  }

  Future<void> _repairLibrary() async {
    final persistence = context.read<Persistence>();
    final confirmed = await showConfirmDialog(
      context,
      title: 'Repair library?',
      body:
          'Vocara checks the library for inconsistencies, fixes what it '
          'finds, and reclaims unused space. Your practices and recordings '
          'are not touched. This can take a moment.',
      confirmLabel: 'Repair',
      destructive: false,
      icon: FIcons.wrench,
    );
    if (!confirmed || !mounted) return;
    setState(() => _repairing = true);
    try {
      final report = await persistence.repairLibrary();
      if (!mounted) return;
      _showToast(repairSummary(report));
    } catch (_) {
      if (!mounted) return;
      _showToast('The repair did not finish. It is safe to try again later.');
    } finally {
      if (mounted) setState(() => _repairing = false);
    }
  }

  Future<void> _exportDiagnostics() async {
    final pick = widget.pickSavePath ?? _saveLocationPath;
    final destination = await pick('vocara-diagnostics.txt');
    if (destination == null || !mounted) return;
    setState(() => _exporting = true);
    try {
      final export =
          widget.exportDiagnostics ?? DiagnosticsLog.instance.exportArchive;
      final bundle = await export();
      await File(bundle).copy(destination);
      if (!mounted) return;
      _showToast(
        'Diagnostics saved. The file holds technical logs only, '
        'never your audio.',
      );
    } catch (_) {
      if (!mounted) return;
      _showToast('Could not save the diagnostics file.');
    } finally {
      if (mounted) setState(() => _exporting = false);
    }
  }

  /// The production save panel (file_selector): destination path or null.
  static Future<String?> _saveLocationPath(String suggestedName) async {
    final location = await getSaveLocation(suggestedName: suggestedName);
    return location?.path;
  }

  void _showLicenses() {
    // Register the hand-maintained bundled-component entries before the page
    // collects the registry (idempotent across openings).
    registerBundledComponentLicenses();
    unawaited(
      Navigator.of(
        context,
      ).push(MaterialPageRoute<void>(builder: (_) => const _LicensesPage())),
    );
  }

  /// Transient result toast (same styling as home's SnackBar pattern).
  void _showToast(String message) {
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
          duration: const Duration(milliseconds: 3500),
          shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(8),
            side: const BorderSide(color: Studio.border),
          ),
        ),
      );
  }
}

/// Flutter's stock [LicensePage] under the window chrome, nudged toward the
/// studio palette. It shows the auto-bundled pub package licenses plus the
/// hand-maintained bundled-component entries from licenses.dart.
class _LicensesPage extends StatelessWidget {
  const _LicensesPage();

  @override
  Widget build(BuildContext context) {
    final base = Theme.of(context);
    return Column(
      children: [
        const WindowDragBar(),
        Expanded(
          child: Theme(
            data: base.copyWith(
              scaffoldBackgroundColor: Studio.background,
              cardColor: Studio.surface,
              appBarTheme: base.appBarTheme.copyWith(
                backgroundColor: Studio.surface,
                foregroundColor: Studio.textPrimary,
              ),
            ),
            child: const LicensePage(
              applicationName: 'Vocara',
              applicationVersion:
                  'Version ${UpdateCheckService.appVersion} '
                  '(build ${UpdateCheckService.appBuildNumber})',
              applicationLegalese: '© 2026 Ruta Tang. All rights reserved.',
            ),
          ),
        ),
      ],
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
          _IconButton(
            icon: FIcons.chevronLeft,
            onTap: () => popOrGo(context, Routes.home),
          ),
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

/// Whether only headphones play from the current output — wired ones on an audio interface, or
/// a pair whose name does not say so. On a marked output, takes skip echo cancelling (which
/// dulled held in-key notes on an echo the mic never heard) and the monitor is allowed. Offered
/// only for a device that can be headphones of its own (see
/// AppSettingsService.outputMarkable), and plain about what goes wrong on speakers. Same chrome
/// as the input-channel card.
class _HeadphonesCard extends StatelessWidget {
  const _HeadphonesCard({
    required this.device,
    required this.marked,
    required this.onChanged,
  });

  /// The output the mark applies to.
  final String device;
  final bool marked;
  final ValueChanged<bool> onChanged;

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: BoxDecoration(
        color: Studio.surfaceRaised,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 10, 10),
      child: Row(
        children: [
          const Icon(FIcons.headphones, size: 16, color: Studio.textFaint),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  'Headphones only',
                  style: TextStyle(color: Studio.textSecondary, fontSize: 13),
                ),
                const SizedBox(height: 2),
                Text(
                  '$device — turn on if you always listen on headphones '
                  'here. Recordings then skip echo cancelling, which keeps '
                  'long notes clear, and hearing yourself is allowed. '
                  'Remembered for this output.',
                  style: const TextStyle(color: Studio.textFaint, fontSize: 11),
                ),
                const SizedBox(height: 2),
                const Text(
                  'Leave it off if speakers ever play from it: takes would '
                  'pick up the backing track, and hearing yourself could howl.',
                  style: TextStyle(color: Studio.textSecondary, fontSize: 11),
                ),
              ],
            ),
          ),
          const SizedBox(width: 12),
          Semantics(
            label: 'Headphones only on $device',
            child: Switch(
              value: marked,
              onChanged: onChanged,
              activeTrackColor: Studio.accent,
              inactiveTrackColor: Studio.inset,
            ),
          ),
        ],
      ),
    );
  }
}

/// Which input(s) of a multi-input device (an audio interface) are the voice: Auto follows the
/// live one — a mic on either input records at full level, two live inputs blend — while Input 1
/// or Input 2 keeps the other out (say, a guitar DI on input 2). Same chrome as the reverb card;
/// the segmented control matches the backing-track toggle.
class _InputChannelCard extends StatelessWidget {
  const _InputChannelCard({
    required this.device,
    required this.mode,
    required this.onSelect,
  });

  /// The device the choice applies to ('' when unknown): the one recording,
  /// which is the system default while the chosen device is unplugged.
  final String device;

  /// A GraphAudioPlaybackService `inputChannel*` mode.
  final int mode;
  final ValueChanged<int> onSelect;

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: BoxDecoration(
        color: Studio.surfaceRaised,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 10),
      child: Row(
        children: [
          const Icon(FIcons.mic, size: 16, color: Studio.textFaint),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                  'Input channel',
                  style: TextStyle(color: Studio.textSecondary, fontSize: 13),
                ),
                const SizedBox(height: 2),
                Text(
                  '${device.isEmpty ? '' : '$device — '}Auto follows the input '
                  'you sing into. Pick one to keep a second source out.',
                  style: const TextStyle(color: Studio.textFaint, fontSize: 11),
                ),
              ],
            ),
          ),
          Container(
            padding: const EdgeInsets.all(2),
            decoration: BoxDecoration(
              color: Studio.inset,
              borderRadius: BorderRadius.circular(7),
              border: Border.all(color: Studio.border),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                _seg('Auto', GraphAudioPlaybackService.inputChannelAuto),
                _seg('Input 1', GraphAudioPlaybackService.inputChannel1),
                _seg('Input 2', GraphAudioPlaybackService.inputChannel2),
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _seg(String label, int value) {
    final active = mode == value;
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: () => onSelect(value),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
          decoration: BoxDecoration(
            color: active
                ? Studio.accent.withValues(alpha: 0.18)
                : Colors.transparent,
            borderRadius: BorderRadius.circular(5),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: active ? Studio.accentBright : Studio.textSecondary,
              fontSize: 11,
              fontWeight: FontWeight.w700,
              letterSpacing: 0.3,
            ),
          ),
        ),
      ),
    );
  }
}

/// The raised card container the maintenance/about rows sit in (same chrome
/// as [_DeviceCard]).
class _Card extends StatelessWidget {
  const _Card({required this.children});

  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: BoxDecoration(
        color: Studio.surfaceRaised,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: Studio.border),
      ),
      clipBehavior: Clip.antiAlias,
      child: Column(children: children),
    );
  }
}

/// One tappable settings action row: icon, title, explanatory subtitle, and a
/// small spinner while its action runs. A null [onTap] disables the row.
class _ActionRow extends StatelessWidget {
  const _ActionRow({
    required this.icon,
    required this.title,
    required this.subtitle,
    this.onTap,
    this.busy = false,
  });

  final IconData icon;
  final String title;
  final String subtitle;
  final VoidCallback? onTap;
  final bool busy;

  @override
  Widget build(BuildContext context) {
    final enabled = onTap != null;
    return InkWell(
      onTap: onTap,
      hoverColor: Studio.hover,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
        child: Row(
          children: [
            Icon(
              icon,
              size: 16,
              color: enabled ? Studio.accent : Studio.textFaint,
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    title,
                    style: TextStyle(
                      color: enabled ? Studio.textPrimary : Studio.textFaint,
                      fontSize: 13.5,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  const SizedBox(height: 2),
                  Text(
                    subtitle,
                    style: const TextStyle(
                      color: Studio.textFaint,
                      fontSize: 11,
                      height: 1.35,
                    ),
                  ),
                ],
              ),
            ),
            if (busy)
              const SizedBox(
                width: 14,
                height: 14,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  valueColor: AlwaysStoppedAnimation(Studio.textSecondary),
                ),
              )
            else
              const Icon(
                FIcons.chevronRight,
                size: 14,
                color: Studio.textFaint,
              ),
          ],
        ),
      ),
    );
  }
}

/// A non-tappable information row (the About block's version line).
class _InfoRow extends StatelessWidget {
  const _InfoRow({
    required this.icon,
    required this.title,
    required this.subtitle,
  });

  final IconData icon;
  final String title;
  final String subtitle;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 12),
      child: Row(
        children: [
          Icon(icon, size: 16, color: Studio.textFaint),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  title,
                  style: const TextStyle(
                    color: Studio.textPrimary,
                    fontSize: 13.5,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  subtitle,
                  style: const TextStyle(color: Studio.textFaint, fontSize: 11),
                ),
              ],
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
