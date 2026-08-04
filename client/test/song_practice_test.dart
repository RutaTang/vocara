import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';

AudioTrack _vocal(String id, {String? versionId}) => AudioTrack(
  id: id,
  kind: AudioTrackKind.vocal,
  displayName: id,
  audioFilePath: '/tmp/$id.wav',
  durationMs: 1000,
  createdAt: DateTime(2026),
  versionId: versionId,
);

PracticeNote _note(String id) => PracticeNote(
  id: id,
  positionMs: 100,
  content: 'note $id',
  createdAt: DateTime(2026),
  updatedAt: DateTime(2026),
);

/// A lane of the given [kind] carrying [noteCount] notes (ids are namespaced
/// by the lane id so they stay unique across lanes).
AudioTrack _laneWithNotes(
  String id,
  AudioTrackKind kind,
  int noteCount, {
  String? versionId,
}) => AudioTrack(
  id: id,
  kind: kind,
  displayName: id,
  audioFilePath: '/tmp/$id.wav',
  durationMs: 1000,
  createdAt: DateTime(2026),
  versionId: versionId,
  notes: [for (var i = 0; i < noteCount; i++) _note('$id-n$i')],
);

void main() {
  group('SongPractice interpretations', () {
    test('legacy JSON (no versions) migrates to a default interpretation', () {
      final json = {
        'id': 'p1',
        'title': 'Song',
        // Two takes with no versionId (the pre-feature shape).
        'vocalTracks': [_vocal('t1').toJson(), _vocal('t2').toJson()],
        'createdAt': DateTime(2026).toIso8601String(),
        'updatedAt': DateTime(2026).toIso8601String(),
      };
      final p = SongPractice.fromJson(json);
      expect(p.versions, hasLength(1));
      expect(p.versions.first.name, 'Interpretation 1');
      expect(p.activeVersionId, p.versions.first.id);
      // Every legacy take is tagged into the default interpretation and visible.
      expect(
        p.vocalTracks.every((t) => t.versionId == p.activeVersionId),
        isTrue,
      );
      expect(p.activeVocalTracks, hasLength(2));
    });

    test('activeVocalTracks shows only the active interpretation\'s takes', () {
      final p = SongPractice(
        id: 'p',
        title: 'S',
        versions: [
          TakeVersion(id: 'v1', name: 'Tender', createdAt: DateTime(2026)),
          TakeVersion(id: 'v2', name: 'Powerful', createdAt: DateTime(2026)),
        ],
        activeVersionId: 'v2',
        vocalTracks: [
          _vocal('a', versionId: 'v1'),
          _vocal('b', versionId: 'v2'),
          _vocal('c', versionId: 'v2'),
        ],
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      expect(p.activeVocalTracks.map((t) => t.id), ['b', 'c']);
      expect(
        p.copyWith(activeVersionId: 'v1').activeVocalTracks.map((t) => t.id),
        ['a'],
      );
    });

    test('JSON round-trips versions, active id and take versionId', () {
      final p = SongPractice(
        id: 'p',
        title: 'S',
        versions: [
          TakeVersion(
            id: 'v1',
            name: 'Tender',
            createdAt: DateTime(2026, 1, 2),
          ),
        ],
        activeVersionId: 'v1',
        vocalTracks: [_vocal('a', versionId: 'v1')],
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      final back = SongPractice.fromJson(p.toJson());
      expect(back.versions.map((v) => v.name), ['Tender']);
      expect(back.activeVersionId, 'v1');
      expect(back.vocalTracks.single.versionId, 'v1');
    });

    test(
      'separationSource round-trips through JSON (and defaults to null)',
      () {
        final p = SongPractice(
          id: 'p',
          title: 'S',
          separationSource: '/audio/p/original.mp3',
          createdAt: DateTime(2026),
          updatedAt: DateTime(2026),
        );
        expect(
          SongPractice.fromJson(p.toJson()).separationSource,
          '/audio/p/original.mp3',
        );
        // A practice that was never separated stays null.
        expect(
          p.copyWith(clearSeparationSource: true).separationSource,
          isNull,
        );
        expect(
          SongPractice.fromJson({
            'id': 'p',
            'title': 'S',
            'createdAt': DateTime(2026).toIso8601String(),
            'updatedAt': DateTime(2026).toIso8601String(),
          }).separationSource,
          isNull,
        );
      },
    );

    test(
      'a take with a stale (deleted-version) versionId is reattached, not lost',
      () {
        final json = {
          'id': 'p',
          'title': 'S',
          'versions': [
            TakeVersion(
              id: 'v1',
              name: 'Keep',
              createdAt: DateTime(2026),
            ).toJson(),
          ],
          'activeVersionId': 'v1',
          'vocalTracks': [
            _vocal('good', versionId: 'v1').toJson(),
            // Points to a version that no longer exists (orphaned id).
            _vocal('orphan', versionId: 'deleted').toJson(),
          ],
          'createdAt': DateTime(2026).toIso8601String(),
          'updatedAt': DateTime(2026).toIso8601String(),
        };
        final p = SongPractice.fromJson(json);
        // The orphan is healed into the active interpretation — never hidden.
        expect(
          p.vocalTracks.firstWhere((t) => t.id == 'orphan').versionId,
          'v1',
        );
        expect(
          p.activeVocalTracks.map((t) => t.id),
          containsAll(['good', 'orphan']),
        );
      },
    );

    test('noteCount counts the reference lane, not just accompaniment and '
        'takes', () {
      // The reference lane can carry notes (note updates route to it, and sync
      // can deliver them), so it must never be silently uncounted.
      final p = SongPractice(
        id: 'p',
        title: 'S',
        accompaniment: _laneWithNotes('acc', AudioTrackKind.accompaniment, 1),
        reference: _laneWithNotes('ref', AudioTrackKind.reference, 2),
        vocalTracks: [_laneWithNotes('take', AudioTrackKind.vocal, 3)],
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      expect(p.noteCount, 6);
      // Without a reference the count still works (nullable lane).
      expect(p.copyWith(clearReference: true).noteCount, 4);
    });

    test('activeNoteCount counts reference notes plus only the active '
        'interpretation\'s takes', () {
      final p = SongPractice(
        id: 'p',
        title: 'S',
        versions: [
          TakeVersion(id: 'v1', name: 'Tender', createdAt: DateTime(2026)),
          TakeVersion(id: 'v2', name: 'Powerful', createdAt: DateTime(2026)),
        ],
        activeVersionId: 'v2',
        accompaniment: _laneWithNotes('acc', AudioTrackKind.accompaniment, 1),
        reference: _laneWithNotes('ref', AudioTrackKind.reference, 2),
        vocalTracks: [
          _laneWithNotes('a', AudioTrackKind.vocal, 4, versionId: 'v1'),
          _laneWithNotes('b', AudioTrackKind.vocal, 8, versionId: 'v2'),
        ],
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      // Accompaniment (1) + reference (2) + active take b (8); the inactive
      // interpretation's take a (4) stays out.
      expect(p.activeNoteCount, 11);
      // Every lane counts in the total regardless of interpretation.
      expect(p.noteCount, 15);
    });

    test('a stale activeVersionId falls back to the first interpretation', () {
      final p = SongPractice(
        id: 'p',
        title: 'S',
        versions: [TakeVersion(id: 'v1', name: 'A', createdAt: DateTime(2026))],
        activeVersionId: 'gone',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      expect(p.activeVersion?.id, 'v1'); // getter resolves the stale id
      expect(
        SongPractice.fromJson(p.toJson()).activeVersionId,
        'v1',
      ); // and load repairs it
    });
  });
}
