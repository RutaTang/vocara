// What a save and a load COST, pinned in statements: every statement is an
// isolate round trip in production (the store runs in a background isolate),
// so a per-clip query inside a save or load turns a mute click on a
// 100-clip practice into hundreds of hops. These tests hold the shape —
// set reads, one batch — and that the batched paths leave the store exactly
// as the per-clip paths did.

import 'dart:io';

import 'package:drift/drift.dart';
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/take_version.dart';
import 'package:vocara/domain/models/technique_marker.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Records every statement the store executes (batched ones included).
class _Statements extends QueryInterceptor {
  final List<String> sql = [];

  int matching(String fragment) =>
      sql.where((s) => s.contains(fragment)).length;

  @override
  Future<List<Map<String, Object?>>> runSelect(
    QueryExecutor e,
    String s,
    List<Object?> a,
  ) {
    sql.add(s);
    return super.runSelect(e, s, a);
  }

  @override
  Future<int> runInsert(QueryExecutor e, String s, List<Object?> a) {
    sql.add(s);
    return super.runInsert(e, s, a);
  }

  @override
  Future<int> runUpdate(QueryExecutor e, String s, List<Object?> a) {
    sql.add(s);
    return super.runUpdate(e, s, a);
  }

  @override
  Future<int> runDelete(QueryExecutor e, String s, List<Object?> a) {
    sql.add(s);
    return super.runDelete(e, s, a);
  }

  @override
  Future<void> runCustom(QueryExecutor e, String s, List<Object?> a) {
    sql.add(s);
    return super.runCustom(e, s, a);
  }

  @override
  Future<void> runBatched(QueryExecutor e, BatchedStatements statements) {
    sql.addAll(statements.statements);
    return super.runBatched(e, statements);
  }
}

void main() {
  late AppDatabase db;
  late Directory tmp;
  late SongPracticeRepositoryDrift repo;
  late _Statements statements;

  setUp(() async {
    statements = _Statements();
    db = AppDatabase(NativeDatabase.memory().interceptWith(statements));
    tmp = await Directory.systemTemp.createTemp('vocara_save_cost');
    final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
    repo = SongPracticeRepositoryDrift(
      db: db,
      writer: writer,
      assets: AssetStore(db: db, root: tmp, writer: writer),
    );
  });
  tearDown(() async {
    await db.close();
    if (await tmp.exists()) await tmp.delete(recursive: true);
  });

  var fileSeq = 0;
  Future<AudioClip> clip(List<int> bytes, List<double> peaks) async {
    final f = File(p.join(tmp.path, 'src${fileSeq++}.wav'));
    await f.writeAsBytes(bytes);
    return AudioClip(
      id: newId(),
      audioFilePath: f.path,
      durationMs: 1000,
      waveformPeaks: peaks,
    );
  }

  /// A practice with [lanes] vocal takes of [clipsPerLane] distinct clips,
  /// one note and one marker each.
  Future<SongPractice> practice(int lanes, int clipsPerLane) async {
    final now = DateTime.fromMillisecondsSinceEpoch(1000);
    final v = TakeVersion(id: newId(), name: 'I1', createdAt: now);
    var seed = 0;
    return SongPractice(
      id: newId(),
      title: 'Song',
      createdAt: now,
      updatedAt: now,
      versions: [v],
      activeVersionId: v.id,
      vocalTracks: [
        for (var l = 0; l < lanes; l++)
          AudioTrack(
            id: newId(),
            kind: AudioTrackKind.vocal,
            displayName: 'Take $l',
            createdAt: now,
            versionId: v.id,
            clips: [
              for (var c = 0; c < clipsPerLane; c++)
                await clip([seed++, l, c], [0.25, 0.5, (c + 1) / 8]),
            ],
            notes: [
              PracticeNote(
                id: newId(),
                positionMs: 10,
                content: 'n$l',
                createdAt: now,
                updatedAt: now,
              ),
            ],
            techniqueMarkers: [
              TechniqueMarker(
                id: newId(),
                positionMs: 20,
                techniqueId: newId(),
                techniqueName: 'Twang',
                createdAt: now,
                updatedAt: now,
              ),
            ],
          ),
      ],
    );
  }

  Future<Loaded<SongPractice>> load(String id) async =>
      (await repo.loadPractice(id) as Ok<Loaded<SongPractice>>).value;

  Future<int> totalChanges() async =>
      (await db.customSelect('SELECT total_changes() AS n').getSingle())
          .read<int>('n');

  test('a save resolves every pool path in one owner read, and an unchanged '
      'save writes no row at all (not even an identical waveform)', () async {
    final pr = await practice(3, 4);
    expect(await repo.savePractice(pr), isA<Ok<SongPractice>>());
    final loaded = await load(pr.id);

    statements.sql.clear();
    final before = await totalChanges();
    final saved = await repo.savePracticeWithBase(
      loaded.aggregate,
      baseHashes: loaded.baseHashes,
    );
    expect(saved, isA<Ok<Loaded<SongPractice>>>());

    // 12 clips, one owner read (it was one per clip).
    expect(statements.matching('"local_rel_path" IN'), 1);
    expect(statements.matching('"local_rel_path" = ?'), 0);
    // Nothing changed, so nothing was written: no reconcile write, no
    // updatedAt touch, and no waveform blob rewritten over identical bytes
    // (the per-clip upserts rewrote all 12).
    expect(await totalChanges(), before);
  });

  test('a save reads each child table once, however many lanes', () async {
    // It read every lane's clips, notes and markers with a query each: 90
    // statements on a 30-lane practice, inside the save's transaction.
    Future<int> childReads(int lanes) async {
      final pr = await practice(lanes, 2);
      expect(await repo.savePractice(pr), isA<Ok<SongPractice>>());
      final loaded = await load(pr.id);
      // An edit on every lane, so every lane's children are reconciled.
      final edited = loaded.aggregate.copyWith(
        vocalTracks: [
          for (final t in loaded.aggregate.vocalTracks)
            t.copyWith(
              notes: [for (final n in t.notes) n.copyWith(content: 'edited')],
            ),
        ],
      );
      statements.sql.clear();
      final saved = await repo.savePracticeWithBase(
        edited,
        baseHashes: loaded.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      // The save's own reads, not its re-read (hydration reads with IN too).
      final perTrack = [
        for (final table in ['clips', 'practice_notes', 'technique_markers'])
          statements.sql
              .where(
                (q) =>
                    q.startsWith('SELECT') &&
                    q.contains('FROM "$table"') &&
                    q.contains('"track_id" = ?'),
              )
              .length,
      ];
      expect(perTrack, [0, 0, 0], reason: '$lanes lanes');
      return statements.sql.length;
    }

    final small = await childReads(2);
    final large = await childReads(12);
    // Writes still scale with the edits (one note update per lane); reads
    // must not add three per lane on top.
    expect(large - small, lessThanOrEqualTo((12 - 2) * 4));
  });

  test('the waveform cache ends as the per-clip writes left it: last write '
      'per content hash wins, in clip order', () async {
    final now = DateTime.fromMillisecondsSinceEpoch(1000);
    // Two clips over IDENTICAL bytes (one asset, one cache row) with
    // different peaks, then a third with its own bytes.
    final first = await clip([7, 7, 7], [0.25]);
    final second = await clip([7, 7, 7], [0.75]);
    final third = await clip([8, 8], [0.5, 0.125]);
    final pr = SongPractice(
      id: newId(),
      title: 'Song',
      createdAt: now,
      updatedAt: now,
      vocalTracks: [
        AudioTrack(
          id: newId(),
          kind: AudioTrackKind.vocal,
          displayName: 'Take',
          createdAt: now,
          clips: [first, second, third],
        ),
      ],
    );
    expect(await repo.savePractice(pr), isA<Ok<SongPractice>>());

    Future<Map<int, List<double>>> cache() async {
      final rows = await db.select(db.waveformCache).get();
      return {
        for (final r in rows)
          r.peaks.length: Uint8List.fromList(
            r.peaks,
          ).buffer.asFloat32List().toList(),
      };
    }

    expect(await db.select(db.waveformCache).get(), hasLength(2));
    expect((await cache())[4], [0.75]); // the later clip's write
    expect((await cache())[8], [0.5, 0.125]);

    // New peaks for an existing hash replace the cached bytes.
    final loaded = await load(pr.id);
    final take = loaded.aggregate.vocalTracks.single;
    final edited = loaded.aggregate.copyWith(
      vocalTracks: [
        take.copyWith(
          clips: [
            for (final c in take.clips)
              c.waveformPeaks.length == 2
                  ? c.copyWith(waveformPeaks: [0.0625, 1.0])
                  : c,
          ],
        ),
      ],
    );
    expect(
      await repo.savePracticeWithBase(edited, baseHashes: loaded.baseHashes),
      isA<Ok<Loaded<SongPractice>>>(),
    );
    expect((await cache())[8], [0.0625, 1.0]);
  });

  test('a load costs the same few statements whatever the practice '
      'holds', () async {
    Future<int> loadCost(SongPractice pr) async {
      expect(await repo.savePractice(pr), isA<Ok<SongPractice>>());
      statements.sql.clear();
      await load(pr.id);
      return statements.sql.length;
    }

    final small = await loadCost(await practice(2, 1));
    final large = await loadCost(await practice(9, 5));
    // practice + interpretations + tracks + buses, and one read each for
    // clips / notes / markers / their assets / their cached peaks. It was
    // 3 per lane plus 3 per clip (16 vs 166 here).
    expect(large, small);
    expect(large, lessThanOrEqualTo(9));
  });

  test('hydration keeps each lane\'s children in the order their per-lane '
      'reads returned them', () async {
    final now = DateTime.fromMillisecondsSinceEpoch(1000);
    // Ids minted in REVERSE of list order, so any sort by id would visibly
    // reorder them: lanes come back by order index, notes and markers in
    // insertion (rowid) order.
    final ids = [for (var i = 0; i < 24; i++) newId()].reversed.toList();
    var next = 0;
    PracticeNote note(String text) => PracticeNote(
      id: ids[next++],
      positionMs: 0,
      content: text,
      createdAt: now,
      updatedAt: now,
    );
    TechniqueMarker marker(String name) => TechniqueMarker(
      id: ids[next++],
      positionMs: 0,
      techniqueId: newId(),
      techniqueName: name,
      createdAt: now,
      updatedAt: now,
    );
    AudioTrack lane(String name) => AudioTrack(
      id: ids[next++],
      kind: AudioTrackKind.vocal,
      displayName: name,
      createdAt: now,
      notes: [note('$name-c'), note('$name-a'), note('$name-b')],
      techniqueMarkers: [marker('$name-z'), marker('$name-y')],
    );
    final pr = SongPractice(
      id: newId(),
      title: 'Song',
      createdAt: now,
      updatedAt: now,
      vocalTracks: [lane('one'), lane('two'), lane('three')],
    );
    expect(await repo.savePractice(pr), isA<Ok<SongPractice>>());

    final got = (await load(pr.id)).aggregate;
    for (final (i, t) in got.vocalTracks.indexed) {
      final want = pr.vocalTracks[i];
      expect(t.id, want.id);
      expect(
        [for (final n in t.notes) n.content],
        [for (final n in want.notes) n.content],
      );
      expect(
        [for (final m in t.techniqueMarkers) m.techniqueName],
        [for (final m in want.techniqueMarkers) m.techniqueName],
      );
    }
  });
}
