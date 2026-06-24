import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

/// The kind of a technique example clip, which decides whether it plays in an
/// audio bar or a video surface.
enum TechniqueMediaKind { audio, video }

/// An audio or video example clip attached to a [Technique]. The file is copied
/// into the app sandbox on import, so [path] is an absolute sandbox path the
/// technique owns (it never points at the user's original file).
@immutable
class TechniqueMedia {
  const TechniqueMedia({
    required this.id,
    required this.path,
    required this.kind,
    this.label = '',
    required this.addedAt,
  });

  final String id;

  /// Absolute sandbox path to the copied media file.
  final String path;

  final TechniqueMediaKind kind;

  /// Optional caption shown under/next to the clip (defaults to the file name
  /// when blank).
  final String label;

  final DateTime addedAt;

  bool get isVideo => kind == TechniqueMediaKind.video;

  /// The label to display: the user's caption, or the file name as a fallback.
  String get displayLabel =>
      label.trim().isNotEmpty ? label.trim() : p.basename(path);

  TechniqueMedia copyWith({String? label}) {
    return TechniqueMedia(
      id: id,
      path: path,
      kind: kind,
      label: label ?? this.label,
      addedAt: addedAt,
    );
  }

  factory TechniqueMedia.fromJson(Map<String, dynamic> json) {
    return TechniqueMedia(
      id: json['id'] as String,
      path: json['path'] as String,
      kind: (json['kind'] as String?) == 'video'
          ? TechniqueMediaKind.video
          : TechniqueMediaKind.audio,
      label: json['label'] as String? ?? '',
      addedAt: DateTime.parse(json['addedAt'] as String),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'path': path,
      'kind': kind == TechniqueMediaKind.video ? 'video' : 'audio',
      'label': label,
      'addedAt': addedAt.toIso8601String(),
    };
  }

  /// File extensions recognized as video; everything else importable is treated
  /// as audio.
  static const _videoExtensions = {
    '.mp4',
    '.mov',
    '.m4v',
    '.webm',
    '.mkv',
    '.avi',
  };

  static const _audioExtensions = {
    '.mp3',
    '.m4a',
    '.aac',
    '.wav',
    '.aif',
    '.aiff',
    '.flac',
    '.ogg',
    '.opus',
    '.wma',
  };

  /// Whether [path] looks like an importable audio or video file.
  static bool isSupportedMedia(String path) {
    final ext = p.extension(path).toLowerCase();
    return _videoExtensions.contains(ext) || _audioExtensions.contains(ext);
  }

  /// Every importable extension WITHOUT the leading dot, for a file picker's
  /// type filter. Derived from the same sets as [isSupportedMedia] so the Browse
  /// dialog and drag-drop can never accept different formats.
  static List<String> get pickerExtensions => [
    for (final e in {..._audioExtensions, ..._videoExtensions}) e.substring(1),
  ];

  /// Classifies a file path as audio or video by its extension.
  static TechniqueMediaKind kindForPath(String path) {
    final ext = p.extension(path).toLowerCase();
    return _videoExtensions.contains(ext)
        ? TechniqueMediaKind.video
        : TechniqueMediaKind.audio;
  }
}
