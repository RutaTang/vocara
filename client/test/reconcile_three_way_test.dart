import 'dart:io';

import 'package:drift/drift.dart' hide isNull, isNotNull;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/asset_store.dart';
import 'package:vocara/data/repositories/song_practice_repository_drift.dart';
import 'package:vocara/data/repositories/technique_repository_drift.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/reconcile.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/domain/models/technique.dart';
import 'package:vocara/domain/models/technique_media.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

/// Three-way reconcile: BASE (what the editor loaded) vs MINE (what the editor
/// wants) vs DB (what is persisted now). The unit group drives every rule of
/// [reconcileCollection] directly; the repository group replays the two
/// verbatim regression scenarios with TWO sequential edit sessions against one
/// drift database, simulating a remote apply with raw SQL (adopting a foreign
/// HLC and appending NO oplog, exactly like SyncApplier).

/// A tiny desired-item for the unit tests, mapped onto `interpretations`.
class _Interp {
  _Interp(this.id, this.name, this.order);
  final String id;
  final String name;
  final int order;
}

String _id(String s) => s.padRight(26, '0').substring(0, 26);

void main() {
  group('reconcileCollection three-way rules (unit)', () {
    late AppDatabase db;
    late SyncWriter writer;
    final practiceId = _id('P1');

    setUp(() async {
      db = AppDatabase(NativeDatabase.memory());
      writer = SyncWriter(db, HlcClock(node: _id('NODE')));
      final s = writer.stamp();
      await db
          .into(db.practices)
          .insert(
            PracticesCompanion.insert(
              id: practiceId,
              createdAt: s.at,
              updatedAt: s.at,
              hlcWall: s.hlc.wallMs,
              hlcCtr: s.hlc.counter,
              hlcNode: s.hlc.node,
              hlcPacked: s.hlc.packed,
            ),
          );
    });
    tearDown(() => db.close());

    // Both sides hash through the SHARED sync_hashes projections, like the
    // production repositories.
    String hashOf(_Interp i) =>
        interpretationContentHash(name: i.name, orderIndex: i.order);

    Future<Map<String, String>> currentHashes() async {
      final rows = await (db.select(
        db.interpretations,
      )..where((t) => t.deletedAt.isNull())).get();
      return {for (final r in rows) r.id: interpretationRowHash(r)};
    }

    /// One save of [desired] against an explicit [base] (the editor's loaded
    /// snapshot); current hashes are always read fresh from the DB.
    Future<ReconcileResult> sync(
      List<_Interp> desired,
      Map<String, String> base,
    ) {
      return db.transaction(() async {
        final current = await currentHashes();
        return reconcileCollection<_Interp>(
          writer: writer,
          table: 'interpretations',
          baseHashes: base,
          currentHashes: current,
          desired: desired,
          idOf: (i) => i.id,
          hashOf: hashOf,
          // The insert path is an UPSERT that clears the tombstone, mirroring
          // the repositories (rule 5's edit-vs-delete revive relies on it).
          insert: (i, s) => db
              .into(db.interpretations)
              .insertOnConflictUpdate(
                InterpretationsCompanion.insert(
                  id: i.id,
                  deletedAt: const Value(null),
                  practiceId: practiceId,
                  name: Value(i.name),
                  orderIndex: Value(i.order),
                  createdAt: s.at,
                  updatedAt: s.at,
                  hlcWall: s.hlc.wallMs,
                  hlcCtr: s.hlc.counter,
                  hlcNode: s.hlc.node,
                  hlcPacked: s.hlc.packed,
                ),
              ),
          update: (i, s) =>
              (db.update(
                db.interpretations,
              )..where((t) => t.id.equals(i.id))).write(
                InterpretationsCompanion(
                  name: Value(i.name),
                  orderIndex: Value(i.order),
                  updatedAt: Value(s.at),
                  hlcWall: Value(s.hlc.wallMs),
                  hlcCtr: Value(s.hlc.counter),
                  hlcNode: Value(s.hlc.node),
                  hlcPacked: Value(s.hlc.packed),
                ),
              ),
          tombstone: (id, s) =>
              (db.update(
                db.interpretations,
              )..where((t) => t.id.equals(id))).write(
                InterpretationsCompanion(
                  deletedAt: Value(s.at),
                  updatedAt: Value(s.at),
                  hlcWall: Value(s.hlc.wallMs),
                  hlcCtr: Value(s.hlc.counter),
                  hlcNode: Value(s.hlc.node),
                  hlcPacked: Value(s.hlc.packed),
                ),
              ),
        );
      });
    }

    Future<InterpretationRow> rowOf(String id) => (db.select(
      db.interpretations,
    )..where((t) => t.id.equals(id))).getSingle();

    Future<int> oplogCount() async => (await db.select(db.oplog).get()).length;

    /// A "remote apply" of an UPDATE: rewrites content + adopts a foreign HLC,
    /// journals nothing — the SyncApplier contract.
    Future<void> remoteUpdate(String id, String name) => db.customStatement(
      'UPDATE interpretations SET name = ?, hlc_wall = 9999999999999, '
      "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
      'WHERE id = ?',
      [name, id],
    );

    /// A "remote apply" of a brand-new row (no oplog, foreign HLC).
    Future<void> remoteInsert(String id, String name, int order) =>
        db.customStatement(
          'INSERT INTO interpretations '
          '(id, practice_id, name, order_index, created_at, updated_at, '
          ' deleted_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed) VALUES '
          "(?, ?, ?, ?, 1, 1, NULL, 9999999999999, 0, 'REMOTE', "
          " 'REMOTE-PACKED')",
          [id, practiceId, name, order],
        );

    /// A "remote apply" of a tombstone (no oplog, foreign HLC).
    Future<void> remoteTombstone(String id) => db.customStatement(
      'UPDATE interpretations SET deleted_at = 999, hlc_wall = 9999999999999, '
      "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
      'WHERE id = ?',
      [id],
    );

    /// Seeds one row through the normal local path and returns the base map an
    /// editor loading NOW would capture.
    Future<Map<String, String>> seed(List<_Interp> items) async {
      await sync(items, const {});
      return currentHashes();
    }

    test(
      'rule 1: a plain local edit (db == base, mine != base) updates',
      () async {
        final a = _Interp(_id('A'), 'Tender', 0);
        final base = await seed([a]);
        final r = await sync([_Interp(_id('A'), 'Soft', 0)], base);
        expect(r.updates, 1);
        expect(r.skips, 0);
        expect((await rowOf(_id('A'))).name, 'Soft');
      },
    );

    test('rule 2: a remote-updated row the editor never touched is kept '
        '(no revert, no oplog)', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final base = await seed([a]); // editor loads here
      await remoteUpdate(_id('A'), 'Remote'); // remote lands while open
      final ops = await oplogCount();

      // Stale save: mine == base ('Tender'), db != base ('Remote').
      final r = await sync([a], base);
      expect(r.total, 0, reason: 'nothing may be written');
      expect(r.skips, 1);
      final row = await rowOf(_id('A'));
      expect(row.name, 'Remote', reason: 'the remote value must survive');
      expect(
        row.hlcPacked,
        'REMOTE-PACKED',
        reason: 'no fresh local HLC may overwrite the remote stamp',
      );
      expect(await oplogCount(), ops, reason: 'no echo op');
    });

    test('rule 3: a true conflict (both sides changed) is won by the active '
        'editor with a fresh HLC', () async {
      final base = await seed([_Interp(_id('A'), 'Tender', 0)]);
      await remoteUpdate(_id('A'), 'Remote');
      final r = await sync([_Interp(_id('A'), 'MyEdit', 0)], base);
      expect(r.updates, 1);
      final row = await rowOf(_id('A'));
      expect(row.name, 'MyEdit');
      expect(
        row.hlcPacked,
        isNot('REMOTE-PACKED'),
        reason: 'the winning write must carry a fresh local HLC',
      );
    });

    test('rule 4: a remote-created row absent from the stale aggregate is NOT '
        'tombstoned', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final base = await seed([a]);
      await remoteInsert(_id('B'), 'RemoteNew', 1);
      final ops = await oplogCount();

      // The stale aggregate only knows A; B's absence is staleness.
      final r = await sync([a], base);
      expect(r.tombstones, 0);
      expect(r.skips, 1);
      final row = await rowOf(_id('B'));
      expect(row.deletedAt, isNull, reason: 'B must stay live');
      expect(await oplogCount(), ops);
    });

    test('rule 5: a remote delete of an UNTOUCHED held row is accepted '
        '(no revive)', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final b = _Interp(_id('B'), 'Powerful', 1);
      final base = await seed([a, b]);
      await remoteTombstone(_id('B'));
      final ops = await oplogCount();

      // Stale save still carries B unchanged (mine == base).
      final r = await sync([a, b], base);
      expect(r.total, 0);
      expect(r.skips, 1);
      final row = await rowOf(_id('B'));
      expect(row.deletedAt, isNotNull, reason: 'the tombstone must hold');
      expect(await oplogCount(), ops);
    });

    test('rule 5: an edit-vs-delete conflict revives the row with the local '
        'edit (fresh HLC, insert op)', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final b = _Interp(_id('B'), 'Powerful', 1);
      final base = await seed([a, b]);
      await remoteTombstone(_id('B'));

      // The editor changed B before the remote delete arrived.
      final r = await sync([a, _Interp(_id('B'), 'Belted', 1)], base);
      expect(r.inserts, 1, reason: 'revive goes through the upsert path');
      final row = await rowOf(_id('B'));
      expect(row.deletedAt, isNull, reason: 'the active editor wins');
      expect(row.name, 'Belted');
      expect(row.hlcPacked, isNot('REMOTE-PACKED'));
    });

    test('rule 6: an explicit local removal tombstones even over a concurrent '
        'remote edit (delete intent wins)', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final b = _Interp(_id('B'), 'Powerful', 1);
      final base = await seed([a, b]);
      await remoteUpdate(_id('B'), 'RemoteEdit'); // db != base

      final r = await sync([a], base); // the user dropped B
      expect(r.tombstones, 1);
      expect((await rowOf(_id('B'))).deletedAt, isNotNull);
    });

    test('rule 7: a new local row (not in base, not in DB) inserts', () async {
      final a = _Interp(_id('A'), 'Tender', 0);
      final base = await seed([a]);
      final r = await sync([a, _Interp(_id('C'), 'Fresh', 1)], base);
      expect(r.inserts, 1);
      expect((await rowOf(_id('C'))).deletedAt, isNull);
    });

    test(
      'unchanged rows (mine == db) emit nothing, whatever the base',
      () async {
        final a = _Interp(_id('A'), 'Tender', 0);
        await seed([a]);
        final ops = await oplogCount();
        // Base deliberately mismatched: mine == db short-circuits first, so an
        // identical row never becomes a phantom write even under a bogus base.
        final r = await sync([a], {_id('A'): 'bogus-base-hash'});
        expect(r.total, 0);
        expect(r.skips, 0);
        expect(await oplogCount(), ops);
      },
    );
  });

  group('repository three-way merge (two sequential edit sessions)', () {
    late AppDatabase db;
    late Directory tmp;
    late SongPracticeRepositoryDrift repo;
    late AssetStore assets;

    setUp(() async {
      db = AppDatabase(NativeDatabase.memory());
      tmp = await Directory.systemTemp.createTemp('vocara_three_way');
      final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
      assets = AssetStore(db: db, root: tmp, writer: writer);
      repo = SongPracticeRepositoryDrift(
        db: db,
        writer: writer,
        assets: assets,
      );
    });
    tearDown(() async {
      await db.close();
      if (await tmp.exists()) await tmp.delete(recursive: true);
    });

    SongPractice ok(Result<SongPractice> r) => (r as Ok<SongPractice>).value;
    Loaded<SongPractice> okLoaded(Result<Loaded<SongPractice>> r) =>
        (r as Ok<Loaded<SongPractice>>).value;

    Future<int> oplogCountFor(String table, String id) async =>
        (await db
                .customSelect(
                  'SELECT COUNT(*) AS n FROM oplog '
                  'WHERE entity_table = ? AND entity_id = ?',
                  variables: [
                    Variable.withString(table),
                    Variable.withString(id),
                  ],
                )
                .getSingle())
            .read<int>('n');

    Future<Map<String, Object?>> rawRow(String table, String id) async =>
        (await db
                .customSelect(
                  'SELECT * FROM $table WHERE id = ?',
                  variables: [Variable.withString(id)],
                )
                .getSingle())
            .data;

    /// Seeds a practice holding one vocal take with one clip and one note,
    /// through the normal local save path.
    Future<SongPractice> seedPractice() async {
      final base = ok(await repo.createPractice('Song'));
      final f = File('${tmp.path}/take.wav');
      await f.writeAsBytes([1, 2, 3]);
      final vocal = AudioTrack(
        id: newId(),
        kind: AudioTrackKind.vocal,
        displayName: 'Take 1',
        createdAt: DateTime(2026),
        versionId: base.versions.single.id,
        clips: [AudioClip(id: newId(), audioFilePath: f.path, durationMs: 900)],
        notes: [
          PracticeNote(
            id: newId(),
            positionMs: 100,
            content: 'original note',
            createdAt: DateTime(2026),
            updatedAt: DateTime(2026),
          ),
        ],
      );
      return ok(
        await repo.savePractice(
          SongPractice(
            id: base.id,
            title: base.title,
            createdAt: base.createdAt,
            updatedAt: base.updatedAt,
            vocalTracks: [vocal],
            versions: base.versions,
            activeVersionId: base.versions.single.id,
          ),
        ),
      );
    }

    /// A "remote apply" over raw SQL: content + foreign HLC, NO oplog — the
    /// SyncApplier contract, so these rows look exactly like synced-in state.
    Future<void> remoteSql(String sql, List<Object?> args) =>
        db.customStatement(sql, args);

    test(
      'REGRESSION (a): a stale save must NOT revert a remote-updated row the '
      'user never touched',
      () async {
        final seeded = await seedPractice();
        final noteId = seeded.vocalTracks.single.notes.single.id;

        // Session: the editor loads (and holds) the aggregate.
        final session = okLoaded(await repo.loadPractice(seeded.id));

        // A remote apply lands while the aggregate is open.
        await remoteSql(
          'UPDATE practice_notes SET content = ?, hlc_wall = 9999999999999, '
          "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
          'WHERE id = ?',
          ['remote-edited', noteId],
        );
        final noteOps = await oplogCountFor('practice_notes', noteId);

        // The user edits ONLY the title, then saves the (now stale) aggregate.
        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(title: 'Renamed'),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<SongPractice>>>());

        // The remote note update survives: content, HLC, and no echo op.
        final row = await rawRow('practice_notes', noteId);
        expect(row['content'], 'remote-edited');
        expect(row['hlc_packed'], 'REMOTE-PACKED');
        expect(await oplogCountFor('practice_notes', noteId), noteOps);

        // The user's own edit did land.
        expect(okLoaded(saved).aggregate.title, 'Renamed');
        // And the returned aggregate converges onto the remote value.
        expect(
          okLoaded(saved).aggregate.vocalTracks.single.notes.single.content,
          'remote-edited',
        );
      },
    );

    test(
      'REGRESSION (b): a stale save must NOT tombstone rows created remotely '
      'while the aggregate was open',
      () async {
        final seeded = await seedPractice();
        final trackId = seeded.vocalTracks.single.id;
        final session = okLoaded(await repo.loadPractice(seeded.id));

        // Remote creates a note under the existing track, a whole new vocal
        // track, and an fx bus — none of which the open editor knows about.
        final remoteNote = newId();
        final remoteTrack = newId();
        final remoteBus = newId();
        await remoteSql(
          'INSERT INTO practice_notes '
          '(id, track_id, position_ms, content, created_at, updated_at, '
          ' hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
          "VALUES (?, ?, 5, 'remote note', 1, 1, 9999999999999, 0, 'REMOTE', "
          "'REMOTE-PACKED')",
          [remoteNote, trackId],
        );
        await remoteSql(
          'INSERT INTO tracks '
          '(id, practice_id, kind, display_name, created_at, updated_at, '
          ' hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
          "VALUES (?, ?, 'vocal', 'Remote take', 1, 1, 9999999999999, 0, "
          "'REMOTE', 'REMOTE-PACKED')",
          [remoteTrack, seeded.id],
        );
        await remoteSql(
          'INSERT INTO fx_buses '
          '(id, practice_id, name, created_at, updated_at, '
          ' hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
          "VALUES (?, ?, 'Remote bus', 1, 1, 9999999999999, 0, 'REMOTE', "
          "'REMOTE-PACKED')",
          [remoteBus, seeded.id],
        );

        // The stale save (a real local edit rides along).
        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(title: 'Renamed'),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<SongPractice>>>());

        // Every remote-created row is still live and un-journaled.
        for (final (table, id) in [
          ('practice_notes', remoteNote),
          ('tracks', remoteTrack),
          ('fx_buses', remoteBus),
        ]) {
          final row = await rawRow(table, id);
          expect(
            row['deleted_at'],
            isNull,
            reason: '$table/$id must stay live',
          );
          expect(await oplogCountFor(table, id), 0);
        }
      },
    );

    test('delete intent wins: a row the user removed is tombstoned even though '
        'remote edited it meanwhile', () async {
      final seeded = await seedPractice();
      final noteId = seeded.vocalTracks.single.notes.single.id;
      final session = okLoaded(await repo.loadPractice(seeded.id));

      await remoteSql(
        'UPDATE practice_notes SET content = ?, hlc_wall = 9999999999999, '
        "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
        'WHERE id = ?',
        ['remote-edited', noteId],
      );

      // The user explicitly removes the note they held.
      final track = session.aggregate.vocalTracks.single;
      final without = session.aggregate.copyWith(
        vocalTracks: [track.copyWith(notes: const [])],
      );
      final saved = await repo.savePracticeWithBase(
        without,
        baseHashes: session.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      expect((await rawRow('practice_notes', noteId))['deleted_at'], isNotNull);
    });

    test(
      'a remote-tombstoned row an untouched stale save carries is NOT revived',
      () async {
        final seeded = await seedPractice();
        final noteId = seeded.vocalTracks.single.notes.single.id;
        final session = okLoaded(await repo.loadPractice(seeded.id));

        await remoteSql(
          'UPDATE practice_notes SET deleted_at = 999, '
          'hlc_wall = 9999999999999, hlc_ctr = 0, '
          "hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' WHERE id = ?",
          [noteId],
        );
        final noteOps = await oplogCountFor('practice_notes', noteId);

        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(title: 'Renamed'),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<SongPractice>>>());
        expect(
          (await rawRow('practice_notes', noteId))['deleted_at'],
          isNotNull,
        );
        expect(await oplogCountFor('practice_notes', noteId), noteOps);
        // The returned aggregate converges: the note is gone from the model.
        expect(okLoaded(saved).aggregate.vocalTracks.single.notes, isEmpty);
      },
    );

    test(
      'edit-vs-delete: a note the user edited revives over a remote tombstone',
      () async {
        final seeded = await seedPractice();
        final noteId = seeded.vocalTracks.single.notes.single.id;
        final session = okLoaded(await repo.loadPractice(seeded.id));

        await remoteSql(
          'UPDATE practice_notes SET deleted_at = 999, '
          'hlc_wall = 9999999999999, hlc_ctr = 0, '
          "hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' WHERE id = ?",
          [noteId],
        );

        final track = session.aggregate.vocalTracks.single;
        final edited = session.aggregate.copyWith(
          vocalTracks: [
            track.copyWith(
              notes: [track.notes.single.copyWith(content: 'my edit')],
            ),
          ],
        );
        final saved = await repo.savePracticeWithBase(
          edited,
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<SongPractice>>>());
        final row = await rawRow('practice_notes', noteId);
        expect(row['deleted_at'], isNull, reason: 'the active editor wins');
        expect(row['content'], 'my edit');
        expect(row['hlc_packed'], isNot('REMOTE-PACKED'));
      },
    );

    test(
      'practice row: a remote title change survives a stale child-only save',
      () async {
        final seeded = await seedPractice();
        final session = okLoaded(await repo.loadPractice(seeded.id));

        await remoteSql(
          'UPDATE practices SET title = ?, hlc_wall = 9999999999999, '
          "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
          'WHERE id = ?',
          ['remote-title', seeded.id],
        );
        final practiceOps = await oplogCountFor('practices', seeded.id);

        // The user only adds a note (a child edit); the practice-level fields
        // in the stale aggregate still carry the OLD title.
        final track = session.aggregate.vocalTracks.single;
        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(
            vocalTracks: [
              track.copyWith(
                notes: [
                  ...track.notes,
                  PracticeNote(
                    id: newId(),
                    positionMs: 42,
                    content: 'new local note',
                    createdAt: DateTime(2026),
                    updatedAt: DateTime(2026),
                  ),
                ],
              ),
            ],
          ),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<SongPractice>>>());
        expect(
          (await rawRow('practices', seeded.id))['title'],
          'remote-title',
          reason: 'the untouched practice row must not be reverted',
        );
        // No practices op was journaled (the local updatedAt ordering bump is
        // deliberately un-journaled).
        expect(await oplogCountFor('practices', seeded.id), practiceOps);
        expect(okLoaded(saved).aggregate.title, 'remote-title');
      },
    );

    test('practice row conflict: the active editor wins the whole row '
        '(documented policy)', () async {
      final seeded = await seedPractice();
      final session = okLoaded(await repo.loadPractice(seeded.id));

      await remoteSql(
        'UPDATE practices SET title = ?, hlc_wall = 9999999999999, '
        "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
        'WHERE id = ?',
        ['remote-title', seeded.id],
      );

      // The user ALSO touched the practice row (its one content hash spans
      // all practice-level fields), so this is a true conflict: local wins.
      final saved = await repo.savePracticeWithBase(
        session.aggregate.copyWith(masterVolume: 0.5),
        baseHashes: session.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<SongPractice>>>());
      final row = await rawRow('practices', seeded.id);
      expect(row['master_volume'], 0.5);
      expect(
        row['title'],
        'Song',
        reason:
            'the whole-row local write includes the stale title — the '
            'documented cost of row-granularity conflict resolution',
      );
      expect(row['hlc_packed'], isNot('REMOTE-PACKED'));
    });

    test(
      'saving over a tombstoned practice still aborts (no zombie children)',
      () async {
        final seeded = await seedPractice();
        final session = okLoaded(await repo.loadPractice(seeded.id));
        await repo.deletePractice(seeded.id);

        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(title: 'Zombie'),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Error<Loaded<SongPractice>>>());
        final live =
            (await db
                    .customSelect(
                      'SELECT COUNT(*) AS n FROM tracks '
                      'WHERE deleted_at IS NULL',
                    )
                    .getSingle())
                .read<int>('n');
        expect(live, 0, reason: 'the abort must leave no live children');
      },
    );

    test(
      'documented edge: a remote-created slot row colliding with a local '
      'slot insert fails the save instead of deleting the remote row',
      () async {
        final seeded = await seedPractice();
        final session = okLoaded(await repo.loadPractice(seeded.id));

        // Remote creates an accompaniment (the single-slot kind) while the
        // local editor also imports one.
        final remoteAccomp = newId();
        await remoteSql(
          'INSERT INTO tracks '
          '(id, practice_id, kind, display_name, created_at, updated_at, '
          ' hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
          "VALUES (?, ?, 'accompaniment', 'Remote backing', 1, 1, "
          "9999999999999, 0, 'REMOTE', 'REMOTE-PACKED')",
          [remoteAccomp, seeded.id],
        );

        final f = File('${tmp.path}/backing.wav');
        await f.writeAsBytes([7, 7, 7]);
        final saved = await repo.savePracticeWithBase(
          session.aggregate.copyWith(
            accompaniment: AudioTrack(
              id: newId(),
              kind: AudioTrackKind.accompaniment,
              displayName: 'My backing',
              createdAt: DateTime(2026),
              clips: [
                AudioClip(id: newId(), audioFilePath: f.path, durationMs: 100),
              ],
            ),
          ),
          baseHashes: session.baseHashes,
        );

        // Rule 4 refuses to tombstone the remote row, so the local insert
        // hits the partial-UNIQUE slot and the whole save fails loudly.
        expect(saved, isA<Error<Loaded<SongPractice>>>());
        expect(
          (await rawRow('tracks', remoteAccomp))['deleted_at'],
          isNull,
          reason: 'the remote-created accompaniment must survive',
        );
      },
    );
  });

  group('technique repository three-way merge', () {
    late AppDatabase db;
    late Directory tmp;
    late TechniqueRepositoryDrift repo;
    late AssetStore assets;

    setUp(() async {
      db = AppDatabase(NativeDatabase.memory());
      tmp = await Directory.systemTemp.createTemp('vocara_three_way_tech');
      final writer = SyncWriter(db, HlcClock(node: 'NODE'.padRight(26, '0')));
      assets = AssetStore(db: db, root: tmp, writer: writer);
      repo = TechniqueRepositoryDrift(db: db, writer: writer, assets: assets);
    });
    tearDown(() async {
      await db.close();
      if (await tmp.exists()) await tmp.delete(recursive: true);
    });

    Loaded<Technique> okLoaded(Result<Loaded<Technique>> r) =>
        (r as Ok<Loaded<Technique>>).value;

    Future<Technique> seedTechnique() async {
      final t = Technique(
        id: newId(),
        name: 'Twang',
        summary: 'original summary',
        createdAt: DateTime(2026),
        updatedAt: DateTime(2026),
      );
      return ((await repo.saveTechnique(t)) as Ok<Technique>).value;
    }

    test('a stale technique save keeps a remote-updated summary the user never '
        'touched', () async {
      final t = await seedTechnique();
      final session = okLoaded(await repo.loadTechnique(t.id));

      await db.customStatement(
        'UPDATE techniques SET summary = ?, hlc_wall = 9999999999999, '
        "hlc_ctr = 0, hlc_node = 'REMOTE', hlc_packed = 'REMOTE-PACKED' "
        'WHERE id = ?',
        ['remote summary', t.id],
      );

      // The user's stale save changes nothing (a pure re-save).
      final saved = await repo.saveTechniqueWithBase(
        session.aggregate,
        baseHashes: session.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<Technique>>>());
      final row =
          (await db
                  .customSelect(
                    'SELECT summary, hlc_packed FROM techniques WHERE id = ?',
                    variables: [Variable.withString(t.id)],
                  )
                  .getSingle())
              .data;
      expect(row['summary'], 'remote summary');
      expect(row['hlc_packed'], 'REMOTE-PACKED');
      expect(okLoaded(saved).aggregate.summary, 'remote summary');
    });

    test(
      'a stale technique save does not tombstone remote-added media',
      () async {
        final t = await seedTechnique();
        final session = okLoaded(await repo.loadTechnique(t.id));

        // Remote adds an example clip (asset row exists; the media row arrives
        // like a synced-in record: raw SQL, foreign HLC, no oplog).
        final f = File('${tmp.path}/remote.wav');
        await f.writeAsBytes([5, 5]);
        final assetId = await assets.importFile(f.path);
        final remoteMedia = newId();
        await db.customStatement(
          'INSERT INTO technique_media '
          '(id, technique_id, asset_id, created_at, updated_at, '
          ' hlc_wall, hlc_ctr, hlc_node, hlc_packed) '
          "VALUES (?, ?, ?, 1, 1, 9999999999999, 0, 'REMOTE', 'REMOTE-PACKED')",
          [remoteMedia, t.id, assetId],
        );

        final saved = await repo.saveTechniqueWithBase(
          session.aggregate.copyWith(name: 'Twang (edited)'),
          baseHashes: session.baseHashes,
        );
        expect(saved, isA<Ok<Loaded<Technique>>>());
        final deleted =
            (await db
                    .customSelect(
                      'SELECT deleted_at AS d FROM technique_media WHERE id = ?',
                      variables: [Variable.withString(remoteMedia)],
                    )
                    .getSingle())
                .data['d'];
        expect(deleted, isNull, reason: 'the remote media row must stay live');
        // The returned aggregate converges onto the remote-added media.
        expect(
          okLoaded(saved).aggregate.media.map((m) => m.id),
          contains(remoteMedia),
        );
      },
    );

    test('an explicitly removed media row still tombstones', () async {
      var t = await seedTechnique();
      final f = File('${tmp.path}/ex.wav');
      await f.writeAsBytes([9]);
      t =
          ((await repo.saveTechnique(
                    t.copyWith(
                      media: [
                        TechniqueMedia(
                          id: newId(),
                          path: f.path,
                          kind: TechniqueMediaKind.audio,
                          addedAt: DateTime(2026),
                        ),
                      ],
                    ),
                  ))
                  as Ok<Technique>)
              .value;
      final mediaId = t.media.single.id;

      final session = okLoaded(await repo.loadTechnique(t.id));
      final saved = await repo.saveTechniqueWithBase(
        session.aggregate.copyWith(media: const []),
        baseHashes: session.baseHashes,
      );
      expect(saved, isA<Ok<Loaded<Technique>>>());
      final deleted =
          (await db
                  .customSelect(
                    'SELECT deleted_at AS d FROM technique_media WHERE id = ?',
                    variables: [Variable.withString(mediaId)],
                  )
                  .getSingle())
              .data['d'];
      expect(deleted, isNotNull);
    });

    test('saving over a tombstoned technique aborts (no zombie media, '
        'Result.error)', () async {
      final t = await seedTechnique();
      final session = okLoaded(await repo.loadTechnique(t.id));
      await repo.deleteTechnique(t.id);

      final f = File('${tmp.path}/zombie.wav');
      await f.writeAsBytes([3]);
      final saved = await repo.saveTechniqueWithBase(
        session.aggregate.copyWith(
          media: [
            TechniqueMedia(
              id: newId(),
              path: f.path,
              kind: TechniqueMediaKind.audio,
              addedAt: DateTime(2026),
            ),
          ],
        ),
        baseHashes: session.baseHashes,
      );
      expect(saved, isA<Error<Loaded<Technique>>>());
      final live =
          (await db
                  .customSelect(
                    'SELECT COUNT(*) AS n FROM technique_media '
                    'WHERE deleted_at IS NULL',
                  )
                  .getSingle())
              .read<int>('n');
      expect(live, 0, reason: 'the abort must leave no live media');
    });
  });
}
