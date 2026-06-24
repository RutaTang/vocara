import 'package:flutter/foundation.dart';

/// One line of lyrics, optionally pinned to a time on the shared timeline.
///
/// [timeMs] is null until the line is synced (via .lrc import or tap-to-time);
/// synced lines drive the karaoke highlight and auto-scroll.
@immutable
class LyricLine {
  const LyricLine({required this.text, this.timeMs});

  final String text;

  /// Start time on the shared timeline (ms). Null = not yet synced.
  final int? timeMs;

  bool get isSynced => timeMs != null;
  bool get isBlank => text.trim().isEmpty;

  LyricLine copyWith({String? text, int? timeMs, bool clearTime = false}) =>
      LyricLine(
        text: text ?? this.text,
        timeMs: clearTime ? null : (timeMs ?? this.timeMs),
      );

  factory LyricLine.fromJson(Map<String, dynamic> json) => LyricLine(
    text: json['text'] as String? ?? '',
    timeMs: json['timeMs'] as int?,
  );

  Map<String, dynamic> toJson() => {
    'text': text,
    if (timeMs != null) 'timeMs': timeMs,
  };

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is LyricLine && other.text == text && other.timeMs == timeMs;

  @override
  int get hashCode => Object.hash(text, timeMs);
}
