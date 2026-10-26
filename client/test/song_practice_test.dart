import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/domain/models/audio_fx.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/fx_bus.dart';
import 'package:vocara/domain/models/lyric_line.dart';
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

    test('activeNoteCount counts BOTH backings plus only the active '
        'interpretation\'s takes', () {
      // Deliberate semantics: the Accompaniment / Original toggle is transient
      // view-state on the detail screen's mixer, which the model cannot know,
      // so activeNoteCount is "active interpretation" — not "visible on
      // screen". The on-screen count (only the shown backing) is
      // MixerController.visibleNoteCount, pinned in mixer_controller_test.
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

  group('SongPractice take numbering', () {
    SongPractice practiceWith(List<String> names) => SongPractice(
      id: 'p',
      title: 'S',
      vocalTracks: [
        for (final (i, name) in names.indexed)
          _vocal('t$i').copyWith(displayName: name),
      ],
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
    );

    test('counts plain and recovered take names alike (the one shared '
        'numbering for new takes AND boot recovery)', () {
      expect(practiceWith([]).nextTakeNumber, 1);
      expect(practiceWith(['Take 1', 'Take 2']).nextTakeNumber, 3);
      // A recovered take's number must be visible to the normal path…
      expect(practiceWith(['Take 1', 'Take 3 (recovered)']).nextTakeNumber, 4);
      // …and renamed/foreign labels stay out, as before.
      expect(practiceWith(['Chorus try', 'Take 2 (edited)']).nextTakeNumber, 1);
    });

    test('an absurd take number (user rename / sync) neither throws nor '
        'wraps negative — it is skipped like any foreign label', () {
      // 20 digits overflowed int.parse (FormatException on every new take and
      // every boot recovery); max-int made max + 1 wrap negative.
      expect(
        practiceWith(['Take 2', 'Take 99999999999999999999']).nextTakeNumber,
        3,
      );
      expect(
        practiceWith(['Take 2', 'Take 9223372036854775807']).nextTakeNumber,
        3,
      );
      // Large-but-sane numbers still count.
      expect(practiceWith(['Take 2', 'Take 1000000']).nextTakeNumber, 1000001);
    });

    test('numbers over the ACTIVE interpretation only', () {
      final p = SongPractice(
        id: 'p',
        title: 'S',
        versions: [
          TakeVersion(id: 'v1', name: 'A', createdAt: DateTime(2026)),
          TakeVersion(id: 'v2', name: 'B', createdAt: DateTime(2026)),
        ],
        activeVersionId: 'v1',
        vocalTracks: [
          _vocal('a', versionId: 'v1').copyWith(displayName: 'Take 1'),
          _vocal('b', versionId: 'v2').copyWith(displayName: 'Take 9'),
        ],
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      expect(p.nextTakeNumber, 2);
    });
  });

  // A save's re-read is all-new objects; taking it in must keep what it
  // reproduces, so what compares by identity sees only real changes.
  group('SongPractice.sharing', () {
    SongPractice practice({
      List<AudioTrack>? vocals,
      AudioTrack? backing,
      List<FxBus>? buses,
      List<LyricLine> lyrics = const [],
    }) => SongPractice(
      id: 'p',
      title: 'Song',
      createdAt: DateTime(2026),
      updatedAt: DateTime(2026),
      accompaniment: backing,
      vocalTracks: vocals ?? [_vocal('a'), _vocal('b'), _vocal('c')],
      versions: [TakeVersion(id: 'v', name: 'I', createdAt: DateTime(2026))],
      activeVersionId: 'v',
      lyrics: lyrics,
      fxBuses: buses ?? [FxBus(id: 'x', name: 'Verb', fx: TrackFx.flat())],
    );

    test('keeps every equal part, and each list all of whose parts it '
        'kept', () {
      final held = practice(
        backing: _laneWithNotes('acc', AudioTrackKind.accompaniment, 2),
        lyrics: const [LyricLine(text: 'la')],
      );
      final shared = practice(
        backing: _laneWithNotes('acc', AudioTrackKind.accompaniment, 2),
        lyrics: const [LyricLine(text: 'la')],
      ).sharing(held);
      expect(shared, held);
      expect(identical(shared.accompaniment, held.accompaniment), isTrue);
      expect(identical(shared.vocalTracks, held.vocalTracks), isTrue);
      expect(identical(shared.versions, held.versions), isTrue);
      expect(identical(shared.fxBuses, held.fxBuses), isTrue);
      expect(identical(shared.lyrics, held.lyrics), isTrue);
    });

    test('takes what changed, and a new list around it', () {
      final held = practice();
      final shared = practice(
        vocals: [_vocal('a'), _vocal('b').copyWith(muted: true), _vocal('c')],
      ).sharing(held);
      expect(shared.vocalTracks[1].muted, isTrue);
      expect(identical(shared.vocalTracks[0], held.vocalTracks[0]), isTrue);
      expect(identical(shared.vocalTracks[2], held.vocalTracks[2]), isTrue);
      expect(identical(shared.vocalTracks, held.vocalTracks), isFalse);
      expect(identical(shared.fxBuses, held.fxBuses), isTrue);
    });

    test('follows a reorder, an addition and a removal', () {
      final held = practice();
      final reordered = practice(
        vocals: [_vocal('c'), _vocal('a'), _vocal('b')],
      ).sharing(held);
      expect(reordered.vocalTracks.map((t) => t.id), ['c', 'a', 'b']);
      expect(identical(reordered.vocalTracks[0], held.vocalTracks[2]), isTrue);
      expect(identical(reordered.vocalTracks, held.vocalTracks), isFalse);

      final grown = practice(
        vocals: [_vocal('a'), _vocal('b'), _vocal('c'), _vocal('d')],
      ).sharing(held);
      expect(grown.vocalTracks.map((t) => t.id), ['a', 'b', 'c', 'd']);
      expect(identical(grown.vocalTracks, held.vocalTracks), isFalse);

      final shrunk = practice(vocals: [_vocal('a'), _vocal('b')]).sharing(held);
      expect(shrunk.vocalTracks.map((t) => t.id), ['a', 'b']);
      expect(identical(shrunk.vocalTracks[1], held.vocalTracks[1]), isTrue);

      final cleared = practice(buses: const []).sharing(held);
      expect(cleared.fxBuses, isEmpty);
    });

    test('keeps every scalar and a backing that came or went', () {
      final held = practice(
        backing: _laneWithNotes('acc', AudioTrackKind.accompaniment, 0),
      );
      final fresh = SongPractice(
        id: 'p',
        title: 'Renamed',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2027),
        vocalTracks: held.vocalTracks,
        versions: held.versions,
        activeVersionId: 'v',
        timelineDurationMs: 9000,
        separationSource: '/song.wav',
        masterVolume: 0.5,
      );
      final shared = fresh.sharing(held);
      expect(shared, fresh);
      expect(shared.accompaniment, isNull);
      expect(
        practice().sharing(held.copyWith(clearAccompaniment: true)),
        practice(),
      );
      final came = _laneWithNotes('acc', AudioTrackKind.accompaniment, 0);
      expect(
        identical(
          practice(backing: came).sharing(practice()).accompaniment,
          came,
        ),
        isTrue,
      );
    });

    test("ignores another practice's parts", () {
      final other = SongPractice(
        id: 'q',
        title: 'Other',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
        vocalTracks: [_vocal('a')],
      );
      final fresh = practice();
      expect(identical(fresh.sharing(other), fresh), isTrue);
    });
  });
}
