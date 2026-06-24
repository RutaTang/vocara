import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/ui/song_practice_detail/view_model/song_practice_detail_view_model.dart';

PracticeNote _note({int start = 1000, int? end}) => PracticeNote(
  id: 'n1',
  positionMs: start,
  endMs: end,
  content: 'breathy onset',
  createdAt: DateTime(2026, 6, 20),
  updatedAt: DateTime(2026, 6, 20),
);

TechniqueMarker _marker({int start = 1000, int? end}) => TechniqueMarker(
  id: 'm1',
  positionMs: start,
  endMs: end,
  techniqueId: 'tech-1',
  techniqueName: 'Twang',
  createdAt: DateTime(2026, 6, 20),
  updatedAt: DateTime(2026, 6, 20),
);

void main() {
  group('PracticeNote range', () {
    test('endMs round-trips; isRange reflects the span', () {
      final point = _note();
      expect(point.isRange, isFalse);
      expect(PracticeNote.fromJson(point.toJson()).endMs, isNull);

      final range = _note(start: 1000, end: 3000);
      expect(range.isRange, isTrue);
      final back = PracticeNote.fromJson(range.toJson());
      expect(back.endMs, 3000);
      expect(back.isRange, isTrue);
    });

    test('legacy JSON without endMs is a point', () {
      final json = _note(start: 500).toJson()..remove('endMs');
      final back = PracticeNote.fromJson(json);
      expect(back.endMs, isNull);
      expect(back.isRange, isFalse);
    });

    test('endMs not greater than start is not a range', () {
      expect(_note(start: 1000, end: 1000).isRange, isFalse);
      expect(_note(start: 1000, end: 900).isRange, isFalse);
    });

    test('copyWith preserves endMs but clearEnd resets to a point', () {
      final range = _note(start: 1000, end: 2000);
      expect(range.copyWith(content: 'x').endMs, 2000);
      expect(range.copyWith(clearEnd: true).endMs, isNull);
      expect(range.copyWith(clearEnd: true).isRange, isFalse);
    });
  });

  group('TechniqueMarker range', () {
    test('endMs round-trips; isRange reflects the span', () {
      expect(_marker().isRange, isFalse);
      final range = _marker(start: 1000, end: 4000);
      final back = TechniqueMarker.fromJson(range.toJson());
      expect(back.endMs, 4000);
      expect(back.isRange, isTrue);
    });

    test('legacy JSON without endMs is a point', () {
      final json = _marker().toJson()..remove('endMs');
      expect(TechniqueMarker.fromJson(json).endMs, isNull);
    });
  });

  group('normalizeMarkerRange', () {
    test('null end stays a point', () {
      expect(normalizeMarkerRange(1000, null), (1000, null));
    });

    test('orders a backwards drag', () {
      expect(normalizeMarkerRange(3000, 1000), (1000, 3000));
    });

    test('keeps a long-enough range', () {
      expect(normalizeMarkerRange(1000, 1000 + kMinMarkerRangeMs), (
        1000,
        1000 + kMinMarkerRangeMs,
      ));
    });

    test('collapses a too-short drag to a point at the start', () {
      // Below the threshold → a point, anchored at the (ordered) start.
      expect(normalizeMarkerRange(1000, 1050), (1000, null));
      expect(normalizeMarkerRange(1050, 1000), (1000, null));
    });

    test('never returns a negative start', () {
      expect(normalizeMarkerRange(-50, null), (0, null));
      expect(normalizeMarkerRange(-50, 2000), (0, 2000));
    });
  });

  group('clampNoteEnd / clampMarkerEnd', () {
    test('a point is returned unchanged', () {
      final n = _note();
      expect(identical(clampNoteEnd(n, 500), n), isTrue);
    });

    test('a range ending before the boundary is unchanged', () {
      final n = _note(start: 1000, end: 2000);
      expect(clampNoteEnd(n, 5000).endMs, 2000);
    });

    test('a range past the boundary is clamped to it', () {
      final n = _note(start: 1000, end: 5000);
      expect(clampNoteEnd(n, 3000).endMs, 3000);
    });

    test('clamping below the minimum span collapses to a point', () {
      final n = _note(start: 1000, end: 5000);
      // boundary leaves only 50ms < kMinMarkerRangeMs → point.
      final clamped = clampNoteEnd(n, 1050);
      expect(clamped.endMs, isNull);
      expect(clamped.isRange, isFalse);
    });

    test('marker variant clamps and collapses the same way', () {
      final m = _marker(start: 0, end: 9000);
      expect(clampMarkerEnd(m, 4000).endMs, 4000);
      expect(clampMarkerEnd(m, 80).endMs, isNull);
    });
  });
}
