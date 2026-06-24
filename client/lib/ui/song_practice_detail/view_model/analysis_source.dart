import 'package:flutter/foundation.dart';

import '../../../data/services/monitor_service.dart';
import '../../../domain/models/audio_track.dart';
import '../../../domain/models/spectrogram.dart';
import '../../../domain/models/voice_metrics.dart';

/// The read-only surface the bottom-dock analysis views need from their host
/// view-model: the selected subject's computed analysis plus playback /
/// live-monitor state.
///
/// Implemented by [SongPracticeDetailViewModel] (a vocal take) and by the
/// technique library's analysis source (a reference clip or an ephemeral
/// practice recording), so the same spectrogram / power / vowel / pitch /
/// timbre / LTAS / vibrato / register / range / dynamics views render in both
/// places without duplication.
///
/// It is a [Listenable] so the analysis views can rebuild when it changes (both
/// implementers are [ChangeNotifier]s).
abstract class AnalysisSource implements Listenable {
  /// The take/clip whose analysis is shown — its `durationMs`/`startMs` set the
  /// time axis. Null when nothing is selected; for the technique screen this is
  /// a synthetic track wrapping the reference or practice clip.
  AudioTrack? get selectedTrack;

  VoiceMetrics? get selectedMetrics;
  Spectrogram? get selectedSpectrogram;
  VoiceProfile? get selectedProfile;
  Ltas? get selectedLtas;
  RegisterMap? get selectedRegister;

  /// True while the subject's analysis is still computing.
  bool get spectrogramLoading;

  /// True when the views should show the live mic instead of the subject.
  bool get liveMonitor;
  MonitorService get monitor;

  /// The shared playhead (subject-local for the technique screen).
  ValueListenable<Duration> get position;

  /// Seeks playback to [to].
  Future<void> seek(Duration to);
}
