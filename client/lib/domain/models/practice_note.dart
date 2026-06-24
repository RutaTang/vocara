import 'package:flutter/foundation.dart';

/// A single note pinned to a track on the timeline — either at one point, or
/// across a range when [endMs] is set.
///
/// Notes capture observations about the user's singing practice (e.g. "breath
/// runs out here", "pitch flat on the high note"). A point note marks a moment;
/// a range note ([endMs] > [positionMs]) marks a span the observation applies to
/// (e.g. "keep twang through this phrase").
@immutable
class PracticeNote {
  const PracticeNote({
    required this.id,
    required this.positionMs,
    required this.content,
    required this.createdAt,
    required this.updatedAt,
    this.endMs,
  });

  /// Stable unique identifier.
  final String id;

  /// Start (or, for a point note, the only) position on the shared timeline, in
  /// milliseconds.
  final int positionMs;

  /// End of the range, in milliseconds, or null for a point note. When set it is
  /// always greater than [positionMs] (see [isRange]).
  final int? endMs;

  /// Free-text body of the note.
  final String content;

  final DateTime createdAt;
  final DateTime updatedAt;

  /// True when this note spans a range rather than a single point.
  bool get isRange => endMs != null && endMs! > positionMs;

  PracticeNote copyWith({
    String? id,
    int? positionMs,
    int? endMs,
    bool clearEnd = false,
    String? content,
    DateTime? updatedAt,
  }) {
    return PracticeNote(
      id: id ?? this.id,
      positionMs: positionMs ?? this.positionMs,
      endMs: clearEnd ? null : (endMs ?? this.endMs),
      content: content ?? this.content,
      createdAt: createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
    );
  }

  factory PracticeNote.fromJson(Map<String, dynamic> json) {
    return PracticeNote(
      id: json['id'] as String,
      positionMs: json['positionMs'] as int,
      endMs: json['endMs'] as int?,
      content: json['content'] as String,
      createdAt: DateTime.parse(json['createdAt'] as String),
      updatedAt: DateTime.parse(json['updatedAt'] as String),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'positionMs': positionMs,
      'endMs': endMs,
      'content': content,
      'createdAt': createdAt.toIso8601String(),
      'updatedAt': updatedAt.toIso8601String(),
    };
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is PracticeNote &&
          other.id == id &&
          other.positionMs == positionMs &&
          other.endMs == endMs &&
          other.content == content &&
          other.createdAt == createdAt &&
          other.updatedAt == updatedAt;

  @override
  int get hashCode =>
      Object.hash(id, positionMs, endMs, content, createdAt, updatedAt);
}
