import 'package:flutter/foundation.dart';

import 'technique_media.dart';

/// A single vocal technique entry in the user's technique library.
///
/// Framework agnostic by design: it carries whatever name, aliases, and tags
/// the singer's own system (EVT, CVT, SLS, bel canto, or a personal vocabulary)
/// uses, plus free-text how-to notes. Nothing here assumes one school of
/// thought — the user organizes it their way.
///
/// [acousticSignature] is Vocara's angle: the singer can describe how the
/// technique reads in the app's own measurements (RING, spectral tilt, CPP,
/// formants) so they can later verify it against their own takes. Free text in
/// this phase; structured, analysis-linked targets come later.
@immutable
class Technique {
  const Technique({
    required this.id,
    required this.name,
    this.aliases = const [],
    this.tags = const [],
    this.summary = '',
    this.description = '',
    this.cues = '',
    this.exercises = '',
    this.acousticSignature = '',
    this.media = const [],
    required this.createdAt,
    required this.updatedAt,
  });

  final String id;

  /// The technique's primary name (e.g. "Twang", "Curbing", "Mixed voice").
  final String name;

  /// Other names the same technique goes by in different systems (e.g. a Twang
  /// quality is close to CVT's "Edge"). Shown as chips and searchable.
  final List<String> aliases;

  /// User-defined labels for grouping/filtering (e.g. "Register", "Quality",
  /// "Ornament", "Figure"). Not a fixed taxonomy — each singer organizes their
  /// own way.
  final List<String> tags;

  /// One-line gloss shown in the technique list.
  final String summary;

  /// The full explanation of what the technique is.
  final String description;

  /// How to produce it — cues, sensations, step by step.
  final String cues;

  /// Practice drills / exercises for the technique.
  final String exercises;

  /// How the technique reads in Vocara's acoustic measurements.
  final String acousticSignature;

  /// Audio/video example clips for the technique, in display order. Each file
  /// lives in the app sandbox (copied on import).
  final List<TechniqueMedia> media;

  final DateTime createdAt;
  final DateTime updatedAt;

  /// Whether the entry has any body content beyond its name (used to show a
  /// gentle "nothing written yet" hint in the viewer).
  bool get hasBody =>
      summary.trim().isNotEmpty ||
      description.trim().isNotEmpty ||
      cues.trim().isNotEmpty ||
      exercises.trim().isNotEmpty ||
      acousticSignature.trim().isNotEmpty ||
      media.isNotEmpty;

  Technique copyWith({
    String? name,
    List<String>? aliases,
    List<String>? tags,
    String? summary,
    String? description,
    String? cues,
    String? exercises,
    String? acousticSignature,
    List<TechniqueMedia>? media,
    DateTime? updatedAt,
  }) {
    return Technique(
      id: id,
      name: name ?? this.name,
      // Defensive copies: never retain a caller's list, so mutating it after
      // the call can't reach into this (otherwise immutable) value.
      aliases: aliases != null ? List.of(aliases) : this.aliases,
      tags: tags != null ? List.of(tags) : this.tags,
      summary: summary ?? this.summary,
      description: description ?? this.description,
      cues: cues ?? this.cues,
      exercises: exercises ?? this.exercises,
      acousticSignature: acousticSignature ?? this.acousticSignature,
      media: media != null ? List.of(media) : this.media,
      createdAt: createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
    );
  }

  factory Technique.fromJson(Map<String, dynamic> json) {
    List<String> strings(String key) =>
        (json[key] as List<dynamic>? ?? const [])
            .map((e) => e as String)
            .toList();
    return Technique(
      id: json['id'] as String,
      name: json['name'] as String,
      aliases: strings('aliases'),
      tags: strings('tags'),
      summary: json['summary'] as String? ?? '',
      description: json['description'] as String? ?? '',
      cues: json['cues'] as String? ?? '',
      exercises: json['exercises'] as String? ?? '',
      acousticSignature: json['acousticSignature'] as String? ?? '',
      media: (json['media'] as List<dynamic>? ?? const [])
          .map((e) => TechniqueMedia.fromJson(e as Map<String, dynamic>))
          .toList(),
      createdAt: DateTime.parse(json['createdAt'] as String),
      updatedAt: DateTime.parse(json['updatedAt'] as String),
    );
  }

  Map<String, dynamic> toJson() {
    return {
      'id': id,
      'name': name,
      'aliases': aliases,
      'tags': tags,
      'summary': summary,
      'description': description,
      'cues': cues,
      'exercises': exercises,
      'acousticSignature': acousticSignature,
      'media': media.map((e) => e.toJson()).toList(),
      'createdAt': createdAt.toIso8601String(),
      'updatedAt': updatedAt.toIso8601String(),
    };
  }

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is Technique &&
          other.id == id &&
          other.name == name &&
          listEquals(other.aliases, aliases) &&
          listEquals(other.tags, tags) &&
          other.summary == summary &&
          other.description == description &&
          other.cues == cues &&
          other.exercises == exercises &&
          other.acousticSignature == acousticSignature &&
          listEquals(other.media, media) &&
          other.createdAt == createdAt &&
          other.updatedAt == updatedAt;

  @override
  int get hashCode => Object.hash(
    id,
    name,
    Object.hashAll(aliases),
    Object.hashAll(tags),
    summary,
    description,
    cues,
    exercises,
    acousticSignature,
    Object.hashAll(media),
    createdAt,
    updatedAt,
  );
}
