import 'package:flutter/foundation.dart';

/// A named interpretation of a song — one emotional/technical reading the user
/// is working on (e.g. "Tender", "Powerful"). It groups a set of vocal takes:
/// switching the active interpretation swaps which takes are shown and played,
/// against the same shared accompaniment and lyrics. A practice always has at
/// least one.
@immutable
class TakeVersion {
  const TakeVersion({
    required this.id,
    required this.name,
    required this.createdAt,
  });

  final String id;

  /// User-facing label shown in the interpretation dropdown.
  final String name;

  final DateTime createdAt;

  TakeVersion copyWith({String? name}) {
    return TakeVersion(id: id, name: name ?? this.name, createdAt: createdAt);
  }

  factory TakeVersion.fromJson(Map<String, dynamic> json) {
    return TakeVersion(
      id: json['id'] as String,
      name: json['name'] as String,
      createdAt: DateTime.parse(json['createdAt'] as String),
    );
  }

  Map<String, dynamic> toJson() {
    return {'id': id, 'name': name, 'createdAt': createdAt.toIso8601String()};
  }
}
