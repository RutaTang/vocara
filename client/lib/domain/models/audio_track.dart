import 'package:flutter/foundation.dart';

import 'audio_fx.dart';
import 'practice_note.dart';
import 'technique_marker.dart';

/// Distinguishes the backing track (imported by the user) from the vocal
/// takes the user records or imports to compare against it.
enum AudioTrackKind {
  /// The song's backing track / instrumental, shown at the top of the detail
  /// view. A practice has at most one.
  accompaniment,

  /// The original recording of the song (with the original vocal), used as a
  /// reference to A/B against the accompaniment. A practice has at most one.
  reference,

  /// A vocal take. A practice can hold many so the user can compare their
  /// own performances.
  vocal,
}

/// A single audio clip on a lane: an audio file placed at [startMs] on the
/// shared timeline, with its own pre-computed waveform. A lane ([AudioTrack])
/// holds zero or more clips (Logic / Cubase style), so recording onto a lane
/// drops a new, independent clip rather than merging into the existing audio.
@immutable
class AudioClip {
  const AudioClip({
    required this.id,
    required this.audioFilePath,
    required this.durationMs,
    this.startMs = 0,
    this.waveformPeaks = const [],
  });

  final String id;

  /// Absolute path to the audio file. Files live in the app support directory
  /// so they travel with the practice.
  final String audioFilePath;

  /// Duration of the clip's audio in milliseconds.
  final int durationMs;

  /// Start offset on the shared timeline (ms).
  final int startMs;

  /// Down-sampled peak amplitudes in [0, 1], one per render bucket. Empty until
  /// analysis completes.
  final List<double> waveformPeaks;

  int get endMs => startMs + durationMs;
  bool get hasWaveform => waveformPeaks.isNotEmpty;

  AudioClip copyWith({
    String? id,
    String? audioFilePath,
    int? durationMs,
    int? startMs,
    List<double>? waveformPeaks,
  }) {
    return AudioClip(
      id: id ?? this.id,
      audioFilePath: audioFilePath ?? this.audioFilePath,
      durationMs: durationMs ?? this.durationMs,
      startMs: startMs ?? this.startMs,
      // Defensive copy: never retain a caller's list, so mutating it after
      // the call can't reach into this (otherwise immutable) value.
      waveformPeaks: waveformPeaks != null
          ? List.of(waveformPeaks)
          : this.waveformPeaks,
    );
  }

  factory AudioClip.fromJson(Map<String, dynamic> json) {
    return AudioClip(
      id: json['id'] as String,
      audioFilePath: json['audioFilePath'] as String? ?? '',
      durationMs: json['durationMs'] as int? ?? 0,
      startMs: json['startMs'] as int? ?? 0,
      waveformPeaks: (json['waveformPeaks'] as List<dynamic>? ?? const [])
          .map((e) => (e as num).toDouble())
          .toList(),
    );
  }

  Map<String, dynamic> toJson() => {
    'id': id,
    'audioFilePath': audioFilePath,
    'durationMs': durationMs,
    'startMs': startMs,
    'waveformPeaks': waveformPeaks,
  };

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is AudioClip &&
          other.id == id &&
          other.audioFilePath == audioFilePath &&
          other.durationMs == durationMs &&
          other.startMs == startMs &&
          listEquals(other.waveformPeaks, waveformPeaks);

  @override
  int get hashCode => Object.hash(
    id,
    audioFilePath,
    durationMs,
    startMs,
    Object.hashAll(waveformPeaks),
  );
}

/// A single lane in the multi-track editor: a list of audio [clips] plus its own
/// lane of notes, mute/solo, volume, color and effect chain.
@immutable
class AudioTrack {
  const AudioTrack._({
    required this.id,
    required this.kind,
    required this.displayName,
    required this.createdAt,
    required this.clips,
    this.notes = const [],
    this.techniqueMarkers = const [],
    this.colorValue,
    this.muted = false,
    this.soloed = false,
    this.volume = 1.0,
    this.pan = 0.0,
    this.versionId,
    this.fx,
    this.busId,
    this.sends = const [],
  });

  /// Builds a lane. Pass [clips] directly, or the single-clip convenience
  /// fields (audioFilePath / startMs / durationMs / waveformPeaks) which become
  /// the lane's one clip — the common way imports and recordings create takes.
  factory AudioTrack({
    required String id,
    required AudioTrackKind kind,
    required String displayName,
    required DateTime createdAt,
    List<AudioClip>? clips,
    String audioFilePath = '',
    int startMs = 0,
    int durationMs = 0,
    List<double> waveformPeaks = const [],
    List<PracticeNote> notes = const [],
    List<TechniqueMarker> techniqueMarkers = const [],
    int? colorValue,
    bool muted = false,
    bool soloed = false,
    double volume = 1.0,
    double pan = 0.0,
    String? versionId,
    TrackFx? fx,
    String? busId,
    List<TrackSend> sends = const [],
  }) {
    final resolved =
        clips ??
        (audioFilePath.isEmpty
            ? const <AudioClip>[]
            : [
                AudioClip(
                  id: '$id-c0',
                  audioFilePath: audioFilePath,
                  durationMs: durationMs,
                  startMs: startMs,
                  waveformPeaks: waveformPeaks,
                ),
              ]);
    return AudioTrack._(
      id: id,
      kind: kind,
      displayName: displayName,
      createdAt: createdAt,
      clips: resolved,
      notes: notes,
      techniqueMarkers: techniqueMarkers,
      colorValue: colorValue,
      muted: muted,
      soloed: soloed,
      volume: volume,
      pan: pan,
      versionId: versionId,
      fx: fx,
      busId: busId,
      sends: sends,
    );
  }

  final String id;
  final AudioTrackKind kind;

  /// Effect chain (EQ / compressor / reverb send) for vocal takes. Null = no
  /// effects configured (dry); backing tracks leave this null.
  final TrackFx? fx;

  /// Output routing (vocal takes): the [FxBus] id this take feeds, or null =
  /// straight to the master. Backing tracks leave this null.
  final String? busId;

  /// Post-fader aux sends to FX buses (independent of the main output).
  final List<TrackSend> sends;

  /// For vocal takes: the interpretation (TakeVersion) this take belongs to.
  /// Null for backing tracks and for legacy takes before migration.
  final String? versionId;

  /// Human-readable label shown on the lane header.
  final String displayName;

  /// The clips on this lane, in creation order — NOT necessarily timeline
  /// order (a punch-in can land before an existing clip). Empty = an empty
  /// take. The lane-span getters below are order-independent.
  final List<AudioClip> clips;

  /// Notes pinned to this lane's timeline — the lane's "note track".
  final List<PracticeNote> notes;

  /// Technique annotations pinned to this lane's timeline — the lane's
  /// "technique track" (each references a library technique at a moment).
  final List<TechniqueMarker> techniqueMarkers;

  /// Optional ARGB color used to tint the lane (helps distinguish takes).
  final int? colorValue;

  final bool muted;

  /// When any track is soloed, only soloed tracks are audible.
  final bool soloed;

  /// Linear playback gain in the range [0.0, 1.0].
  final double volume;

  /// Stereo pan in [-1, 1] (0 = center). Backing tracks keep 0.
  final double pan;

  final DateTime createdAt;

  bool get isAccompaniment => kind == AudioTrackKind.accompaniment;
  bool get isReference => kind == AudioTrackKind.reference;

  /// Backing tracks (accompaniment or reference) are the song you sing against.
  bool get isBacking =>
      kind == AudioTrackKind.accompaniment || kind == AudioTrackKind.reference;

  /// The lane's first clip — for file-centric operations that require a
  /// single-clip lane (byte-level split/merge) or one representative clip.
  /// Null when the lane is empty. Time/analysis consumers should use the
  /// lane-span fields ([startMs]/[endMs]/[durationMs]) or iterate [clips].
  AudioClip? get primaryClip => clips.isEmpty ? null : clips.first;

  AudioClip? clipById(String clipId) {
    for (final c in clips) {
      if (c.id == clipId) return c;
    }
    return null;
  }

  // --- Lane-span time model: the lane's time fields cover ALL of its clips,
  // so multi-clip takes (punch-in recordings, dropped clips) read correctly
  // everywhere. For a single-clip lane they equal the clip's own fields. ---

  /// Start of the lane's content on the shared timeline: the earliest clip
  /// start (0 for an empty lane).
  int get startMs => clips.isEmpty
      ? 0
      : clips.fold(
          clips.first.startMs,
          (m, c) => c.startMs < m ? c.startMs : m,
        );

  /// End of the lane's content on the shared timeline: the latest clip end
  /// (0 for an empty lane).
  int get endMs => clips.fold(0, (m, c) => c.endMs > m ? c.endMs : m);

  /// The lane's content span in ms ([startMs]..[endMs]), gaps included.
  int get durationMs => endMs - startMs;

  bool get hasClip => clips.isNotEmpty;
  bool get isEmptyTake => kind == AudioTrackKind.vocal && clips.isEmpty;

  AudioTrack copyWith({
    String? id,
    String? displayName,
    List<AudioClip>? clips,
    List<PracticeNote>? notes,
    List<TechniqueMarker>? techniqueMarkers,
    int? colorValue,
    bool clearColor = false,
    bool? muted,
    bool? soloed,
    double? volume,
    double? pan,
    DateTime? createdAt,
    String? versionId,
    TrackFx? fx,
    bool clearFx = false,
    String? busId,
    bool clearBus = false,
    List<TrackSend>? sends,
  }) {
    return AudioTrack._(
      id: id ?? this.id,
      kind: kind,
      displayName: displayName ?? this.displayName,
      createdAt: createdAt ?? this.createdAt,
      // Defensive copies: never retain a caller's list, so mutating it after
      // the call can't reach into this (otherwise immutable) value.
      clips: clips != null ? List.of(clips) : this.clips,
      notes: notes != null ? List.of(notes) : this.notes,
      techniqueMarkers: techniqueMarkers != null
          ? List.of(techniqueMarkers)
          : this.techniqueMarkers,
      // colorValue == null normally means "unchanged"; clearColor resets it to
      // the default (no custom tint).
      colorValue: clearColor ? null : (colorValue ?? this.colorValue),
      muted: muted ?? this.muted,
      soloed: soloed ?? this.soloed,
      volume: volume ?? this.volume,
      pan: pan ?? this.pan,
      versionId: versionId ?? this.versionId,
      fx: clearFx ? null : (fx ?? this.fx),
      busId: clearBus ? null : (busId ?? this.busId),
      sends: sends != null ? List.of(sends) : this.sends,
    );
  }

  factory AudioTrack.fromJson(Map<String, dynamic> json) {
    final clipsJson = json['clips'] as List<dynamic>?;
    final List<AudioClip> clips;
    if (clipsJson != null) {
      clips = clipsJson
          .map((e) => AudioClip.fromJson(e as Map<String, dynamic>))
          .toList();
    } else {
      // Legacy take: a single inline clip (or empty).
      final path = json['audioFilePath'] as String? ?? '';
      clips = path.isEmpty
          ? const []
          : [
              AudioClip(
                id: '${json['id']}-c0',
                audioFilePath: path,
                durationMs: json['durationMs'] as int? ?? 0,
                startMs: json['startMs'] as int? ?? 0,
                waveformPeaks:
                    (json['waveformPeaks'] as List<dynamic>? ?? const [])
                        .map((e) => (e as num).toDouble())
                        .toList(),
              ),
            ];
    }
    return AudioTrack._(
      id: json['id'] as String,
      kind: AudioTrackKind.values.byName(json['kind'] as String),
      displayName: json['displayName'] as String,
      createdAt: DateTime.parse(json['createdAt'] as String),
      clips: clips,
      notes: (json['notes'] as List<dynamic>? ?? const [])
          .map((e) => PracticeNote.fromJson(e as Map<String, dynamic>))
          .toList(),
      techniqueMarkers: (json['techniqueMarkers'] as List<dynamic>? ?? const [])
          .map((e) => TechniqueMarker.fromJson(e as Map<String, dynamic>))
          .toList(),
      colorValue: json['colorValue'] as int?,
      muted: json['muted'] as bool? ?? false,
      soloed: json['soloed'] as bool? ?? false,
      // Clamp on load (like ReverbSettings): a synced / migrated / hand-edited
      // value outside the fader's range must not reach the engine verbatim.
      volume: ((json['volume'] as num?)?.toDouble() ?? 1.0).clamp(0.0, 1.0),
      pan: ((json['pan'] as num?)?.toDouble() ?? 0.0).clamp(-1.0, 1.0),
      versionId: json['versionId'] as String?,
      fx: json['fx'] != null
          ? TrackFx.fromJson(json['fx'] as Map<String, dynamic>)
          : null,
      busId: json['busId'] as String?,
      sends: (json['sends'] as List<dynamic>? ?? const [])
          .map((e) => TrackSend.fromJson(e as Map<String, dynamic>))
          .toList(),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'kind': kind.name,
      'displayName': displayName,
      'clips': clips.map((e) => e.toJson()).toList(),
      'notes': notes.map((e) => e.toJson()).toList(),
      'techniqueMarkers': techniqueMarkers.map((e) => e.toJson()).toList(),
      'colorValue': colorValue,
      'muted': muted,
      'soloed': soloed,
      'volume': volume,
      'pan': pan,
      'createdAt': createdAt.toIso8601String(),
      'versionId': versionId,
      'fx': fx?.toJson(),
      'busId': busId,
      'sends': sends.map((s) => s.toJson()).toList(),
    };
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is AudioTrack &&
          other.id == id &&
          other.kind == kind &&
          other.displayName == displayName &&
          // Order matters: clips are kept in creation order (see [clips]), so
          // two lanes with the same clips in a different order are NOT equal.
          listEquals(other.clips, clips) &&
          listEquals(other.notes, notes) &&
          listEquals(other.techniqueMarkers, techniqueMarkers) &&
          other.colorValue == colorValue &&
          other.muted == muted &&
          other.soloed == soloed &&
          other.volume == volume &&
          other.pan == pan &&
          other.createdAt == createdAt &&
          other.versionId == versionId &&
          other.fx == fx &&
          other.busId == busId &&
          listEquals(other.sends, sends);

  @override
  int get hashCode => Object.hash(
    id,
    kind,
    displayName,
    Object.hashAll(clips),
    Object.hashAll(notes),
    Object.hashAll(techniqueMarkers),
    colorValue,
    muted,
    soloed,
    volume,
    pan,
    createdAt,
    versionId,
    fx,
    busId,
    Object.hashAll(sends),
  );
}
