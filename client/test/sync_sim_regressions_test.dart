import 'dart:io';

import 'package:drift/drift.dart' show Variable, driftRuntimeOptions;
import 'package:drift/native.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/sync/hlc.dart';
import 'package:vocara/data/sync/integrity_checker.dart';
import 'package:vocara/data/sync/sync_applier.dart';
import 'package:vocara/data/sync/sync_writer.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

import 'support/fake_sync_transport.dart';

String _id(String s) => s.padRight(26, '0').substring(0, 26);

/// Targeted regressions for the REAL sync bugs found by the convergence
/// simulation (test/sync_simulation_test.dart), one per fix, each named
/// after the seed that first exposed it. The full episodes stay in the
/// simulation battery; these pin the exact mechanism at the unit level.
void main() {
  setUpAll(() {
    driftRuntimeOptions.dontWarnAboutMultipleDatabases = true;
  });

  T ok<T>(Result<T> r) {
    expect(r, isA<Ok<T>>(), reason: r is Error<T> ? '${r.error}' : null);
    return (r as Ok<T>).value;
  }

  /// Spins until the wall clock enters the NEXT millisecond, making the HLC
  /// order of two subsequent writes on different devices deterministic.
  Future<void> nextMs() async {
    final start = DateTime.now().millisecondsSinceEpoch;
    while (DateTime.now().millisecondsSinceEpoch <= start) {
      await Future<void>.delayed(Duration.zero);
    }
  }

  Future<Map<String, String>> rowStates(AppDatabase db, String table) async {
    final rows = await db
        .customSelect('SELECT id, hlc_packed, deleted_at FROM $table')
        .get();
    return {
      for (final r in rows)
        r.data['id']! as String:
            '${r.data['hlc_packed']}|${r.data['deleted_at']}',
    };
  }

  group('seed 0x5eed0001: cascade delete must upload parent-first', () {
    late FakeCloud cloud;
    late Persistence a;
    late Persistence b;
    late FakeSyncTransport ta;
    late FakeSyncTransport tb;
    late Directory tmp;

    setUp(() async {
      cloud = FakeCloud();
      a = Persistence.memory();
      b = Persistence.memory();
      ta = FakeSyncTransport(cloud, name: 'A');
      tb = FakeSyncTransport(cloud, name: 'B');
      a.attachSync(ta);
      b.attachSync(tb);
      tmp = await Directory.systemTemp.createTemp('vocara_sim_reg');
    });

    tearDown(() async {
      a.sync!.dispose();
      b.sync!.dispose();
      final roots = [a.assets.root, b.assets.root, tmp];
      await a.db.close();
      await b.db.close();
      for (final d in roots) {
        if (await d.exists()) await d.delete(recursive: true);
      }
    });

    test('a practice deleted before it ever synced arrives WHOLE on a '
        'page-size-1 puller (no FK-swept row loss)', () async {
      // The oplog collapses each row to its LATEST op, and deletePractice
      // journals children before the practice row — so without the pusher's
      // parent-first sort the cloud log carried the cascade CHILD-FIRST.
      // A device that never saw the aggregate applies those rows as fresh
      // inserts; with one row per page the clip landed before its track (and
      // the track before its practice), the FK sweep hard-deleted them, and
      // the advanced change token meant they never re-arrived: permanent row
      // loss and divergence.
      final created = ok(await a.songRepo.createPractice('Doomed'));
      final f = File(p.join(tmp.path, 'take_${newId()}.wav'));
      await f.writeAsBytes(const [1, 2, 3, 4, 5, 6, 7, 8]);
      final saved = ok(
        await a.songRepo.savePractice(
          created.copyWith(
            vocalTracks: [
              AudioTrack(
                id: newId(),
                kind: AudioTrackKind.vocal,
                displayName: 'Take 1',
                audioFilePath: f.path,
                durationMs: 800,
                createdAt: DateTime.now(),
                versionId: created.activeVersionId,
              ),
            ],
          ),
        ),
      );
      // Delete BEFORE anything was pushed: every row ships as a tombstoned
      // fresh insert for B.
      ok(await a.songRepo.deletePractice(saved.id));
      final push = await a.sync!.pumpPush();
      expect(push.failed, isFalse);

      tb.pageSize = 1; // worst case: every page boundary splits the cascade
      final pull = await b.sync!.pumpPull();
      expect(pull.failed, isFalse);
      expect(
        pull.failedRecords,
        0,
        reason: 'no row of the cascade may be FK-swept',
      );

      // Every synced row of the aggregate exists on B, byte-equal in
      // (hlc, tombstone) to A — nothing was lost.
      for (final table in [
        'practices',
        'interpretations',
        'tracks',
        'clips',
        'assets',
      ]) {
        expect(
          await rowStates(b.db, table),
          equals(await rowStates(a.db, table)),
          reason: '$table must fully replicate',
        );
      }
      final live = await b.db
          .customSelect(
            'SELECT COUNT(*) AS n FROM practices WHERE deleted_at IS NULL',
          )
          .getSingle();
      expect(live.data['n'], 0, reason: 'the practice arrived tombstoned');
    });
  });

  group(
    'seeds 0x5eed0009 / 0xabd782: single-slot resolution must replicate',
    () {
      late FakeCloud cloud;
      late Persistence a;
      late Persistence b;
      late Persistence c;
      late Directory tmp;

      setUp(() async {
        cloud = FakeCloud();
        a = Persistence.memory();
        b = Persistence.memory();
        c = Persistence.memory();
        a.attachSync(FakeSyncTransport(cloud, name: 'A'));
        b.attachSync(FakeSyncTransport(cloud, name: 'B'));
        c.attachSync(FakeSyncTransport(cloud, name: 'C'));
        tmp = await Directory.systemTemp.createTemp('vocara_sim_reg');
      });

      tearDown(() async {
        for (final dev in [a, b, c]) {
          dev.sync!.dispose();
        }
        final roots = [a.assets.root, b.assets.root, c.assets.root, tmp];
        await a.db.close();
        await b.db.close();
        await c.db.close();
        for (final d in roots) {
          if (await d.exists()) await d.delete(recursive: true);
        }
      });

      AudioTrack accompaniment(String path, {String? name}) => AudioTrack(
        id: newId(),
        kind: AudioTrackKind.accompaniment,
        displayName: name ?? 'backing',
        audioFilePath: path,
        durationMs: 1000,
        createdAt: DateTime.now(),
      );

      test(
        'concurrent accompaniments: the losing track ends tombstoned on '
        'EVERY device AND in the cloud, with one identical row state',
        () async {
          // Both devices add an accompaniment while unaware of each other. The
          // applier resolves the (practice, kind) slot by LWW — but the loser's
          // tombstone used to be an un-journaled, un-bumped local write: whether
          // a device even observed the conflict depended on its apply order, so
          // the loser stayed LIVE in the cloud (and on any device that saw the
          // winner's state first) while being dead elsewhere — a permanent
          // divergence under identical HLCs. The resolution must replicate like
          // any write: derived-stamp tombstone + journal.
          final f = File(p.join(tmp.path, 'acc_${newId()}.wav'));
          await f.writeAsBytes(const [9, 9, 9, 9, 1, 2, 3, 4]);

          final seeded = ok(await a.songRepo.createPractice('Slot race'));
          expect((await a.sync!.pumpPush()).failed, isFalse);
          expect((await b.sync!.pumpPull()).failed, isFalse);

          // A adds accA and pushes it (accA is now LIVE in the cloud).
          final la = ok(await a.songRepo.loadPractice(seeded.id));
          final accA = accompaniment(f.path, name: 'accA');
          ok(
            await a.songRepo.savePracticeWithBase(
              la.aggregate.copyWith(accompaniment: accA),
              baseHashes: la.baseHashes,
            ),
          );
          expect((await a.sync!.pumpPush()).failed, isFalse);

          // B, not having pulled accA, adds accB with a strictly later HLC.
          await nextMs();
          final lb = ok(await b.songRepo.loadPractice(seeded.id));
          final accB = accompaniment(f.path, name: 'accB');
          ok(
            await b.songRepo.savePracticeWithBase(
              lb.aggregate.copyWith(accompaniment: accB),
              baseHashes: lb.baseHashes,
            ),
          );
          expect((await b.sync!.pumpPush()).failed, isFalse);

          // Everyone settles (C pulls the whole log from scratch).
          for (var round = 0; round < 4; round++) {
            for (final dev in [a, b, c]) {
              expect((await dev.sync!.pumpPush()).failed, isFalse);
              expect((await dev.sync!.pumpPull()).failed, isFalse);
            }
          }

          // The resolution replicated: the cloud's current accA is TOMBSTONED.
          final cloudAccA = cloud.serverRowFor('tracks', accA.id);
          expect(cloudAccA, isNotNull);
          expect(
            cloudAccA!.fields['deleted_at'],
            isNotNull,
            reason: 'the slot resolution must be pushed, not stay device-local',
          );

          // All three devices hold IDENTICAL row states: accA dead, accB live.
          final onA = await rowStates(a.db, 'tracks');
          expect(await rowStates(b.db, 'tracks'), equals(onA));
          expect(await rowStates(c.db, 'tracks'), equals(onA));
          for (final dev in [a, b, c]) {
            final acc = await dev.db
                .customSelect(
                  'SELECT id FROM tracks WHERE deleted_at IS NULL AND '
                  "kind = 'accompaniment'",
                )
                .get();
            expect(acc, hasLength(1));
            expect(acc.single.data['id'], accB.id);
            // No device is left with a pending resolution un-pushed.
            expect(await dev.db.select(dev.db.oplog).get(), isEmpty);
          }
        },
      );
    },
  );

  group('seed 0xc1c2b7: integrity repair must not run between pages', () {
    late FakeCloud cloud;
    late Persistence a;
    late Persistence b;
    late FakeSyncTransport tb;

    setUp(() {
      cloud = FakeCloud();
      a = Persistence.memory();
      b = Persistence.memory();
      a.attachSync(FakeSyncTransport(cloud, name: 'A'));
      tb = FakeSyncTransport(cloud, name: 'B');
      b.attachSync(tb);
    });

    tearDown(() async {
      a.sync!.dispose();
      b.sync!.dispose();
      final roots = [a.assets.root, b.assets.root];
      await a.db.close();
      await b.db.close();
      for (final d in roots) {
        if (await d.exists()) await d.delete(recursive: true);
      }
    });

    test('a practice whose interpretation arrives on the NEXT page is not '
        '"healed" into a fresh-HLC root write mid-pull', () async {
      // With one row per page, the practice row lands one page before its
      // interpretation. Running the repair after EVERY page treated that
      // half-delivered aggregate as broken: the active-interp scalar heal
      // nulled the pointer under a fresh, LWW-winning HLC — manufactured
      // intent that could later beat a legitimate delete tombstone and
      // resurrect the practice fleet-wide. The repair belongs at the END of
      // the pull run, once the delivered prefix is causally complete.
      final created = ok(await a.songRepo.createPractice('Paged'));
      expect((await a.sync!.pumpPush()).failed, isFalse);

      tb.pageSize = 1;
      final pull = await b.sync!.pumpPull();
      expect(pull.failed, isFalse);
      expect(pull.applied, 2, reason: 'practice + interpretation');

      final row = await b.db
          .customSelect(
            'SELECT active_interpretation_id AS i, hlc_packed AS h '
            'FROM practices WHERE id = ?',
            variables: [Variable.withString(created.id)],
          )
          .getSingle();
      expect(
        row.data['i'],
        created.activeVersionId,
        reason: 'the pointer must survive the page boundary un-healed',
      );
      // No repair ran, so nothing was journaled: applying remote changes
      // still echoes nothing into push.
      expect(await b.db.select(b.db.oplog).get(), isEmpty);
      // And B holds A's row verbatim (no repair-minted HLC).
      final onA = await rowStates(a.db, 'practices');
      expect(await rowStates(b.db, 'practices'), equals(onA));
    });
  });

  group('seed 0xab1a57: repairs are derived state, never fresh intent', () {
    late AppDatabase db;
    late HlcClock clock;
    late SyncWriter writer;
    late IntegrityChecker checker;
    late SyncApplier applier;

    // The healer's clock sits FAR ahead of the row it will repair.
    const int healerNow = 2000000000;

    setUp(() {
      db = AppDatabase(NativeDatabase.memory());
      clock = HlcClock(node: _id('LOCAL'), now: () => healerNow);
      writer = SyncWriter(db, clock);
      checker = IntegrityChecker(db, writer);
      applier = SyncApplier(db, clock);
    });

    tearDown(() => db.close());

    Future<void> insertPractice({
      required String id,
      required Hlc hlc,
      String? activeInterpretationId,
    }) {
      return db.customStatement(
        'INSERT INTO practices (id, title, active_interpretation_id, '
        'created_at, updated_at, deleted_at, hlc_wall, hlc_ctr, hlc_node, '
        'hlc_packed, schema_v) VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 1)',
        [
          id,
          'song',
          activeInterpretationId,
          hlc.wallMs,
          hlc.wallMs,
          hlc.wallMs,
          hlc.counter,
          hlc.node,
          hlc.packed,
        ],
      );
    }

    test('a scalar heal stamps ONE counter above the row (not a clock tick), '
        'so a later real delete still wins LWW', () async {
      // The row was written long "ago" by a remote device...
      final rowHlc = Hlc(wallMs: 1000000, counter: 0, node: _id('REMOTE'));
      await insertPractice(
        id: _id('P'),
        hlc: rowHlc,
        activeInterpretationId: _id('GONE'), // dangling: no such interp
      );

      final violations = await checker.repair();
      expect(violations, isEmpty);
      final healed = await db
          .customSelect(
            'SELECT active_interpretation_id AS i, hlc_wall AS w, '
            'hlc_ctr AS c, hlc_node AS n FROM practices WHERE id = ?',
            variables: [Variable.withString(_id('P'))],
          )
          .getSingle();
      expect(healed.data['i'], isNull, reason: 'the heal itself is right');
      // The stamp is DERIVED: row HLC + 1 counter under OUR node — not the
      // healer's (far-future) clock. A fresh tick here out-ranked genuinely
      // later writes from other devices (the healer merely has to be the
      // device with the fastest clock), letting bookkeeping resurrect a
      // deleted practice (simulation seed 0xab1a57).
      expect(healed.data['w'], rowHlc.wallMs);
      expect(healed.data['c'], rowHlc.counter + 1);
      expect(healed.data['n'], _id('LOCAL'));

      // A delete stamped causally AFTER the original row — but by a device
      // whose wall clock is far BEHIND the healer's — must still win.
      final deleteHlc = Hlc(wallMs: 1500000, counter: 0, node: _id('DELETER'));
      final res = await applier.apply([
        RemoteRecord(
          table: 'practices',
          columns: {
            'id': _id('P'),
            'title': 'song',
            'timeline_duration_ms': 0,
            'master_volume': 1.0,
            'active_interpretation_id': null,
            'separation_asset_id': null,
            'reverb_json': null,
            'lyrics_json': null,
            'created_at': rowHlc.wallMs,
            'updated_at': deleteHlc.wallMs,
            'deleted_at': deleteHlc.wallMs,
            'hlc_wall': deleteHlc.wallMs,
            'hlc_ctr': deleteHlc.counter,
            'hlc_node': deleteHlc.node,
            'hlc_packed': deleteHlc.packed,
            'schema_v': 1,
          },
        ),
      ]);
      expect(res.applied, 1, reason: 'the tombstone must beat the heal');
      final gone = await db
          .customSelect(
            'SELECT deleted_at AS d FROM practices WHERE id = ?',
            variables: [Variable.withString(_id('P'))],
          )
          .getSingle();
      expect(gone.data['d'], isNotNull, reason: 'tombstones never resurrect');
    });

    test('a zombie-child tombstone repair also derives its stamp from the '
        'child row', () async {
      final parentHlc = Hlc(wallMs: 1000000, counter: 3, node: _id('REMOTE'));
      final childHlc = Hlc(wallMs: 1200000, counter: 7, node: _id('REMOTE'));
      await insertPractice(id: _id('P'), hlc: parentHlc);
      await db.customStatement(
        'UPDATE practices SET deleted_at = ? WHERE id = ?',
        [parentHlc.wallMs, _id('P')],
      );
      await db.customStatement(
        'INSERT INTO tracks (id, practice_id, kind, display_name, muted, '
        'soloed, volume, pan, order_index, created_at, updated_at, '
        'deleted_at, hlc_wall, hlc_ctr, hlc_node, hlc_packed, schema_v) '
        "VALUES (?, ?, 'vocal', 'zombie', 0, 0, 1.0, 0.0, 0, ?, ?, NULL, "
        '?, ?, ?, ?, 1)',
        [
          _id('T'),
          _id('P'),
          childHlc.wallMs,
          childHlc.wallMs,
          childHlc.wallMs,
          childHlc.counter,
          childHlc.node,
          childHlc.packed,
        ],
      );

      expect(await checker.repair(), isEmpty);
      final t = await db
          .customSelect(
            'SELECT deleted_at AS d, hlc_wall AS w, hlc_ctr AS c, '
            'hlc_node AS n FROM tracks WHERE id = ?',
            variables: [Variable.withString(_id('T'))],
          )
          .getSingle();
      expect(t.data['d'], isNotNull);
      expect(t.data['w'], childHlc.wallMs);
      expect(t.data['c'], childHlc.counter + 1);
      expect(t.data['n'], _id('LOCAL'));
    });
  });
}
