import 'package:flutter/material.dart';
import 'package:forui/forui.dart' show FIcons;

import '../../../domain/models/technique_analysis.dart';
import '../../../domain/models/voice_metrics.dart';
import '../../core/themes/studio.dart';
import '../../song_practice_detail/widgets/voice_profile_view.dart';
import '../../song_practice_detail/widgets/voiceprint_view.dart';
import '../view_model/technique_detail_view_model.dart';

/// The analysis dimensions shown for a technique, reusing the song-practice
/// dock views (driven through [TechniqueDetailViewModel]'s AnalysisSource).
final List<(String, Widget Function(TechniqueDetailViewModel))> _dimensions = [
  ('Spectrogram', (vm) => SpectrogramTab(vm: vm)),
  ('Power', (vm) => PowerTab(vm: vm)),
  ('Vowel', (vm) => VowelTab(vm: vm)),
  ('Pitch', (vm) => PitchTab(vm: vm)),
  ('Timbre', (vm) => TimbreTab(vm: vm)),
  ('LTAS', (vm) => LtasTab(vm: vm)),
  ('Vibrato', (vm) => VibratoTab(vm: vm)),
  ('Register', (vm) => RegisterTab(vm: vm)),
  ('Range', (vm) => RangeView(vm: vm)),
  ('Dynamics', (vm) => DynamicsView(vm: vm)),
];

/// The inline analysis for ONE example (the currently-selected/expanded card):
/// the dimension tabs + analysis box, plus — in the read view — a practice
/// recorder and gap comparison against that example. While a recording is in
/// flight it shows the live mic instead. Header/selector live on the card.
class TechniqueExampleAnalysis extends StatefulWidget {
  const TechniqueExampleAnalysis({
    required this.vm,
    this.allowPractice = true,
    super.key,
  });

  final TechniqueDetailViewModel vm;

  /// Whether the practice recorder + Reference/Practice switch are offered. Off
  /// while editing — there you build the examples, not practice against them.
  final bool allowPractice;

  @override
  State<TechniqueExampleAnalysis> createState() =>
      _TechniqueExampleAnalysisState();
}

class _TechniqueExampleAnalysisState extends State<TechniqueExampleAnalysis> {
  TechniqueDetailViewModel get vm => widget.vm;

  @override
  Widget build(BuildContext context) {
    // Compute the reference analysis lazily once the example clip is known.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) vm.ensureReferenceAnalysis();
    });

    final live = vm.liveMonitor;
    // The selected dimension lives on the VM so it survives the panel moving
    // between cards or in/out of the live (recording) slot.
    final dim = vm.analysisDim;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (widget.allowPractice) ...[
          _Controls(vm: vm),
          const SizedBox(height: 10),
        ],
        _DimensionBar(active: dim, onSelect: vm.setAnalysisDim),
        const SizedBox(height: 8),
        _AnalysisBox(vm: vm, child: _dimensions[dim].$2(vm)),
        // Static-take extras: hidden while the live mic feed is showing.
        if (!live && vm.selectedMetrics != null)
          _QualityWarning(metrics: vm.selectedMetrics!),
        if (widget.allowPractice &&
            !live &&
            vm.referenceAnalysis != null &&
            vm.practiceAnalysis != null) ...[
          const SizedBox(height: 14),
          _GapTable(
            reference: vm.referenceAnalysis!,
            practice: vm.practiceAnalysis!,
          ),
        ],
      ],
    );
  }
}

/// The Reference/Practice subject switch + the record / stop control.
class _Controls extends StatelessWidget {
  const _Controls({required this.vm});

  final TechniqueDetailViewModel vm;

  Future<void> _record(BuildContext context) async {
    final error = await vm.startPractice();
    if (error != null && context.mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(error)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final recording = vm.isRecordingPractice;
    final hasPractice = vm.practiceAnalysis != null;
    return Row(
      children: [
        _SubjectToggle(
          subject: vm.analysisSubject,
          practiceEnabled: hasPractice || vm.practiceAnalysisLoading,
          onSelect: vm.showAnalysisSubject,
        ),
        const Spacer(),
        if (recording) ...[
          SizedBox(width: 90, child: _Level(vm: vm)),
          const SizedBox(width: 10),
          _RecButton(
            label: 'Stop',
            icon: FIcons.square,
            danger: true,
            onTap: vm.stopPractice,
          ),
        ] else if (vm.practiceAnalysisLoading)
          const _RecButton(label: 'Analyzing…', icon: FIcons.loader)
        else ...[
          if (hasPractice) _TextButton(label: 'Clear', onTap: vm.clearPractice),
          const SizedBox(width: 8),
          _RecButton(
            label: hasPractice ? 'Re-record' : 'Record practice',
            icon: FIcons.mic,
            onTap: () => _record(context),
          ),
        ],
      ],
    );
  }
}

class _SubjectToggle extends StatelessWidget {
  const _SubjectToggle({
    required this.subject,
    required this.practiceEnabled,
    required this.onSelect,
  });

  final TechniqueAnalysisSubject subject;
  final bool practiceEnabled;
  final ValueChanged<TechniqueAnalysisSubject> onSelect;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.all(2),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(7),
        border: Border.all(color: Studio.border),
      ),
      child: Row(
        children: [
          _seg(
            'Reference',
            subject == TechniqueAnalysisSubject.reference,
            true,
            () => onSelect(TechniqueAnalysisSubject.reference),
          ),
          _seg(
            'Your practice',
            subject == TechniqueAnalysisSubject.practice,
            practiceEnabled,
            () => onSelect(TechniqueAnalysisSubject.practice),
          ),
        ],
      ),
    );
  }

  Widget _seg(String label, bool selected, bool enabled, VoidCallback onTap) {
    return MouseRegion(
      cursor: enabled ? SystemMouseCursors.click : SystemMouseCursors.basic,
      child: GestureDetector(
        onTap: enabled ? onTap : null,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 5),
          decoration: BoxDecoration(
            color: selected ? Studio.accent : Colors.transparent,
            borderRadius: BorderRadius.circular(5),
          ),
          child: Text(
            label,
            style: TextStyle(
              color: selected
                  ? Studio.background
                  : (enabled ? Studio.textSecondary : Studio.textFaint),
              fontSize: 12,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

class _RecButton extends StatelessWidget {
  const _RecButton({
    required this.label,
    required this.icon,
    this.danger = false,
    this.onTap,
  });

  final String label;
  final IconData icon;
  final bool danger;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final fill = danger ? Studio.record : Studio.accent;
    return MouseRegion(
      cursor: onTap == null
          ? SystemMouseCursors.basic
          : SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
          decoration: BoxDecoration(
            color: onTap == null ? Studio.surfaceRaised : fill,
            borderRadius: BorderRadius.circular(7),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                icon,
                size: 14,
                color: onTap == null ? Studio.textFaint : Studio.background,
              ),
              const SizedBox(width: 6),
              Text(
                label,
                style: TextStyle(
                  color: onTap == null ? Studio.textFaint : Studio.background,
                  fontSize: 12,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _TextButton extends StatelessWidget {
  const _TextButton({required this.label, required this.onTap});

  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 7),
          child: Text(
            label,
            style: const TextStyle(
              color: Studio.textSecondary,
              fontSize: 12,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}

/// The live input-level meter shown while recording.
class _Level extends StatelessWidget {
  const _Level({required this.vm});

  final TechniqueDetailViewModel vm;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        const Icon(FIcons.mic, size: 13, color: Studio.record),
        const SizedBox(width: 6),
        Expanded(
          child: Container(
            height: 8,
            decoration: BoxDecoration(
              color: Studio.inset,
              borderRadius: BorderRadius.circular(4),
            ),
            clipBehavior: Clip.antiAlias,
            child: Align(
              alignment: Alignment.centerLeft,
              child: ValueListenableBuilder<double>(
                valueListenable: vm.recordLevel,
                builder: (context, level, _) => FractionallySizedBox(
                  widthFactor: level.clamp(0.0, 1.0),
                  child: const ColoredBox(color: Studio.record),
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}

class _DimensionBar extends StatelessWidget {
  const _DimensionBar({required this.active, required this.onSelect});

  final int active;
  final ValueChanged<int> onSelect;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 28,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        itemCount: _dimensions.length,
        separatorBuilder: (_, _) => const SizedBox(width: 6),
        itemBuilder: (context, i) {
          final on = i == active;
          return MouseRegion(
            cursor: SystemMouseCursors.click,
            child: GestureDetector(
              onTap: () => onSelect(i),
              child: Container(
                alignment: Alignment.center,
                padding: const EdgeInsets.symmetric(horizontal: 11),
                decoration: BoxDecoration(
                  color: on
                      ? Studio.accent.withValues(alpha: 0.18)
                      : Studio.surfaceRaised,
                  borderRadius: BorderRadius.circular(6),
                  border: Border.all(
                    color: on
                        ? Studio.accent.withValues(alpha: 0.6)
                        : Studio.border,
                  ),
                ),
                child: Text(
                  _dimensions[i].$1,
                  style: TextStyle(
                    color: on ? Studio.accentBright : Studio.textSecondary,
                    fontSize: 11,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}

/// A fixed-height frame for the selected analysis view, with its own loading /
/// empty states (so the reused dock views never show take-specific hints).
class _AnalysisBox extends StatelessWidget {
  const _AnalysisBox({required this.vm, required this.child});

  final TechniqueDetailViewModel vm;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final practice = vm.analysisSubject == TechniqueAnalysisSubject.practice;
    final loading = vm.spectrogramLoading;
    final analysis = practice ? vm.practiceAnalysis : vm.referenceAnalysis;
    final failed = practice
        ? vm.practiceAnalysisFailed
        : vm.referenceAnalysisFailed;

    Widget body;
    if (vm.liveMonitor) {
      // While recording, the reused dock views render the live mic feed.
      body = child;
    } else if (loading) {
      body = _Hint(
        practice ? 'Analyzing your take…' : 'Analyzing the example…',
        spinner: true,
      );
    } else if (analysis == null) {
      body = _Hint(
        failed
            ? (practice
                  ? "Couldn't analyze your recording. Try again."
                  : "Couldn't analyze this example clip.")
            : 'Record yourself to compare against the reference.',
      );
    } else {
      body = child;
    }

    return Container(
      height: 320,
      decoration: BoxDecoration(
        color: Studio.surface,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.border),
      ),
      clipBehavior: Clip.antiAlias,
      child: body,
    );
  }
}

/// The per-metric gap between the reference and the practice take.
class _GapTable extends StatelessWidget {
  const _GapTable({required this.reference, required this.practice});

  final TechniqueAnalysis reference;
  final TechniqueAnalysis practice;

  @override
  Widget build(BuildContext context) {
    final r = reference, p = practice;
    final rows = <Widget>[
      _GapRow(label: 'RING', ref: r.metrics.ringAvg, you: p.metrics.ringAvg),
      _GapRow(label: 'TILT', ref: r.metrics.tiltAvg, you: p.metrics.tiltAvg),
      _GapRow(label: 'CPP', ref: r.metrics.cppAvg, you: p.metrics.cppAvg),
      if (r.ltas.hasData && p.ltas.hasData)
        _GapRow(
          label: "SINGER'S FORMANT",
          ref: r.ltas.singersFormantDb,
          you: p.ltas.singersFormantDb,
          tol: 3,
        ),
      if (r.profile.hasRange && p.profile.hasRange)
        _GapRow(
          label: 'RANGE',
          ref: (r.profile.highMidi! - r.profile.lowMidi!).toDouble(),
          you: (p.profile.highMidi! - p.profile.lowMidi!).toDouble(),
          unit: 'st',
          tol: 2,
        ),
      if (r.profile.hasDynamics && p.profile.hasDynamics)
        _GapRow(
          label: 'DYNAMICS',
          ref: r.profile.dynamicRangeDb,
          you: p.profile.dynamicRangeDb,
          tol: 4,
        ),
    ];
    return Container(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 6),
      decoration: BoxDecoration(
        color: Studio.inset,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.border),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Padding(
            padding: EdgeInsets.only(bottom: 8),
            child: Row(
              children: [
                Expanded(flex: 4, child: SizedBox()),
                Expanded(
                  flex: 3,
                  child: Text(
                    'REFERENCE',
                    textAlign: TextAlign.right,
                    style: _capStyle,
                  ),
                ),
                Expanded(
                  flex: 3,
                  child: Text(
                    'YOU',
                    textAlign: TextAlign.right,
                    style: _capStyle,
                  ),
                ),
                Expanded(
                  flex: 3,
                  child: Text(
                    'GAP',
                    textAlign: TextAlign.right,
                    style: _capStyle,
                  ),
                ),
              ],
            ),
          ),
          ...rows,
        ],
      ),
    );
  }
}

const TextStyle _capStyle = TextStyle(
  color: Studio.textFaint,
  fontSize: 9,
  fontWeight: FontWeight.w700,
  letterSpacing: 0.5,
);

class _GapRow extends StatelessWidget {
  const _GapRow({
    required this.label,
    required this.ref,
    required this.you,
    this.unit = 'dB',
    this.tol = 2,
  });

  final String label;
  final double ref;
  final double you;
  final String unit;
  final double tol;

  String _fmt(double v) => v.isFinite ? v.toStringAsFixed(1) : '—';

  @override
  Widget build(BuildContext context) {
    final has = ref.isFinite && you.isFinite;
    final delta = you - ref;
    final close = has && delta.abs() <= tol;
    final deltaText = !has
        ? '—'
        : '${delta >= 0 ? '+' : '−'}${delta.abs().toStringAsFixed(1)} $unit';
    final num = const TextStyle(
      color: Studio.textPrimary,
      fontSize: 12,
      fontWeight: FontWeight.w700,
    );
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Row(
        children: [
          Expanded(
            flex: 4,
            child: Text(
              label,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 11,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          Expanded(
            flex: 3,
            child: Text(
              '${_fmt(ref)} $unit',
              textAlign: TextAlign.right,
              style: num,
            ),
          ),
          Expanded(
            flex: 3,
            child: Text(
              '${_fmt(you)} $unit',
              textAlign: TextAlign.right,
              style: num,
            ),
          ),
          Expanded(
            flex: 3,
            child: Text(
              deltaText,
              textAlign: TextAlign.right,
              style: TextStyle(
                color: !has
                    ? Studio.textFaint
                    : (close ? const Color(0xFF63C089) : Studio.accentBright),
                fontSize: 12,
                fontWeight: FontWeight.w700,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// A red banner when the shown clip is clipped or too noisy to trust — so a
/// bad practice recording is flagged rather than silently mis-analyzed.
class _QualityWarning extends StatelessWidget {
  const _QualityWarning({required this.metrics});

  final VoiceMetrics metrics;

  @override
  Widget build(BuildContext context) {
    final clipped = metrics.isClipped;
    final lowSnr = metrics.isLowSnr;
    if (!clipped && !lowSnr) return const SizedBox.shrink();
    final msg = clipped
        ? 'Clipped (${(metrics.clippedFraction * 100).toStringAsFixed(1)}% of '
              'samples). Lower the mic gain and re-record — the spectral metrics '
              'are unreliable on a clipped take.'
        : 'Low signal-to-noise (${metrics.snrDb.round()} dB). Record closer to '
              'the mic, or somewhere quieter, for a trustworthy analysis.';
    return Container(
      margin: const EdgeInsets.only(top: 10),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
      decoration: BoxDecoration(
        color: Studio.record.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: Studio.record.withValues(alpha: 0.4)),
      ),
      child: Row(
        children: [
          const Icon(FIcons.triangleAlert, size: 15, color: Studio.record),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              msg,
              style: const TextStyle(
                color: Studio.textSecondary,
                fontSize: 12,
                height: 1.3,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _Hint extends StatelessWidget {
  const _Hint(this.text, {this.spinner = false});

  final String text;
  final bool spinner;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (spinner) ...[
              const SizedBox(
                width: 18,
                height: 18,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  valueColor: AlwaysStoppedAnimation(Studio.accent),
                ),
              ),
              const SizedBox(height: 14),
            ],
            Text(
              text,
              textAlign: TextAlign.center,
              style: const TextStyle(
                color: Studio.textFaint,
                fontSize: 12.5,
                height: 1.4,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
