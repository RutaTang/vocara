import 'package:flutter/foundation.dart';

/// A technique annotation pinned to a vocal take at a point on the timeline.
///
/// Like a [PracticeNote], it captures "what's happening at this moment" — but
/// instead of free text it references a technique from the user's technique
/// library, so the singer can mark where they're using (or aiming for) Twang,
/// a register shift, vibrato, etc. The marker is created at the playback cursor.
///
/// [techniqueId] points at the library technique; [techniqueName] is a snapshot
/// of its name at creation time so the marker still reads sensibly if the
/// library entry is later renamed or deleted.
@immutable
class TechniqueMarker {
  const TechniqueMarker({
    required this.id,
    required this.positionMs,
    required this.techniqueId,
    required this.techniqueName,
    required this.createdAt,
    required this.updatedAt,
    this.endMs,
  });

  /// Stable unique identifier.
  final String id;

  /// Start (or, for a point marker, the only) position on the shared timeline,
  /// in milliseconds.
  final int positionMs;

  /// End of the range, in milliseconds, or null for a point marker. When set it
  /// is always greater than [positionMs] (see [isRange]) — marking a span the
  /// technique applies across, e.g. "keep twang through this phrase".
  final int? endMs;

  /// True when this marker spans a range rather than a single point.
  bool get isRange => endMs != null && endMs! > positionMs;

  /// The referenced library technique's id.
  final String techniqueId;

  /// The referenced technique's name, snapshotted for display.
  final String techniqueName;

  final DateTime createdAt;
  final DateTime updatedAt;

  TechniqueMarker copyWith({
    String? id,
    int? positionMs,
    int? endMs,
    bool clearEnd = false,
    String? techniqueId,
    String? techniqueName,
    DateTime? updatedAt,
  }) {
    return TechniqueMarker(
      id: id ?? this.id,
      positionMs: positionMs ?? this.positionMs,
      endMs: clearEnd ? null : (endMs ?? this.endMs),
      techniqueId: techniqueId ?? this.techniqueId,
      techniqueName: techniqueName ?? this.techniqueName,
      createdAt: createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
    );
  }

  factory TechniqueMarker.fromJson(Map<String, dynamic> json) {
    return TechniqueMarker(
      id: json['id'] as String,
      positionMs: json['positionMs'] as int,
      endMs: json['endMs'] as int?,
      techniqueId: json['techniqueId'] as String? ?? '',
      techniqueName: json['techniqueName'] as String? ?? '',
      createdAt: DateTime.parse(json['createdAt'] as String),
      updatedAt: DateTime.parse(json['updatedAt'] as String),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'positionMs': positionMs,
      'endMs': endMs,
      'techniqueId': techniqueId,
      'techniqueName': techniqueName,
      'createdAt': createdAt.toIso8601String(),
      'updatedAt': updatedAt.toIso8601String(),
    };
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is TechniqueMarker &&
          other.id == id &&
          other.positionMs == positionMs &&
          other.endMs == endMs &&
          other.techniqueId == techniqueId &&
          other.techniqueName == techniqueName &&
          other.createdAt == createdAt &&
          other.updatedAt == updatedAt;

  @override
  int get hashCode => Object.hash(
    id,
    positionMs,
    endMs,
    techniqueId,
    techniqueName,
    createdAt,
    updatedAt,
  );
}
