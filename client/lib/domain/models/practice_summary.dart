import 'package:flutter/foundation.dart';

import 'song_practice.dart';

/// What the Home list shows of a practice, read without hydrating it: the
/// list needs a title, a date and three counts, and a full load per card —
/// every clip's waveform peaks and a file check per clip — was paid again on
/// every return to Home.
@immutable
class PracticeSummary {
  const PracticeSummary({
    required this.id,
    required this.title,
    required this.updatedAt,
    required this.takeCount,
    required this.hasAccompaniment,
    required this.noteCount,
  });

  /// The summary of an aggregate already in hand (e.g. one just created),
  /// by the same rules the summary read applies to the stored rows.
  factory PracticeSummary.of(SongPractice p) => PracticeSummary(
    id: p.id,
    title: p.title,
    updatedAt: p.updatedAt,
    takeCount: p.vocalTracks.length,
    hasAccompaniment: p.hasAccompaniment,
    noteCount: p.noteCount,
  );

  final String id;
  final String title;
  final DateTime updatedAt;

  /// Every vocal take, in every interpretation.
  final int takeCount;
  final bool hasAccompaniment;

  /// Notes on every lane.
  final int noteCount;
}
