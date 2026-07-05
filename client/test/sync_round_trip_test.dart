import 'dart:io';

import 'package:drift/drift.dart' show Variable, driftRuntimeOptions;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:vocara/data/db/app_database.dart';
import 'package:vocara/data/db/persistence.dart';
import 'package:vocara/data/sync/sync_engine.dart';
import 'package:vocara/data/sync/sync_hashes.dart';
import 'package:vocara/data/sync/sync_row.dart';
import 'package:vocara/domain/models/audio_track.dart';
import 'package:vocara/domain/models/practice_note.dart';
import 'package:vocara/domain/models/song_practice.dart';
import 'package:vocara/utils/id.dart';
import 'package:vocara/utils/result.dart';

import 'support/fake_sync_transport.dart';

/// Two full Persistence stacks (device A and device B) sharing ONE fake cloud
/// — the end-to-end convergence suite for P0 sync: create/edit/delete on one
/// device, push/pull, and prove byte-identical row state on the other.
void main() {
  late FakeCloud cloud;
  late Persistence a;
  late Persistence b;
  late FakeSyncTransport ta;
  late FakeSyncTransport tb;
  late SyncEngine ea;
  late SyncEngine eb;
  late Directory tmp;

  setUpAll(() {
    // Two LIVE AppDatabase instances (device A + device B, each over its own
    // in-memory executor) are the whole point of this suite. Drift's warning
    // guards against two database objects SHARING one executor — which never
    // happens here — so silence it for this file.
    driftRuntimeOptions.dontWarnAboutMultipleDatabases = true;
  });

  setUp(() async {
    cloud = FakeCloud();
    a = Persistence.memory();
    b = Persistence.memory();
    ta = FakeSyncTransport(cloud, name: 'A');
    tb = FakeSyncTransport(cloud, name: 'B');
    ea = a.attachSync(ta);
    eb = b.attachSync(tb);
    tmp = await Directory.systemTemp.createTemp('vocara_round_trip');
  });

  tearDown(() async {
    ea.dispose();
    eb.dispose();
    final roots = [a.assets.root, b.assets.root, tmp];
    await a.db.close();
    await b.db.close();
    for (final d in roots) {
      if (await d.exists()) await d.delete(recursive: true);
    }
  });

  T ok<T>(Result<T> r) {
    expect(r, isA<Ok<T>>(), reason: r is Error<T> ? '${r.error}' : null);
    return (r as Ok<T>).value;
  }

  Future<Loaded<SongPractice>> loadOn(Persistence dev, String id) async =>
      ok(await dev.songRepo.loadPractice(id));

  /// Spins until the wall clock enters the NEXT millisecond, making the HLC
  /// order of two subsequent writes on different devices deterministic.
  Future<void> nextMs() async {
    final start = DateTime.now().millisecondsSinceEpoch;
    while (DateTime.now().millisecondsSinceEpoch <= start) {
      await Future<void>.delayed(Duration.zero);
    }
  }

  /// Full sync rounds until quiescent: pushes then pulls on both devices,
  /// repeated so post-pull integrity repairs propagate too; then asserts both
  /// oplogs fully drained (nothing left unsynced).
  Future<void> settle({int rounds = 4}) async {
    for (var i = 0; i < rounds; i++) {
      expect((await ea.pumpPush()).failed, isFalse);
      expect((await eb.pumpPush()).failed, isFalse);
      expect((await ea.pumpPull()).failed, isFalse);
      expect((await eb.pumpPull()).failed, isFalse);
    }
    expect(await a.db.select(a.db.oplog).get(), isEmpty);
    expect(await b.db.select(b.db.oplog).get(), isEmpty);
  }

  /// Every synced row on a device: "table/id" -> "hlcPacked|deletedAt". Two
  /// converged devices produce IDENTICAL snapshots (entity-level LWW means
  /// equal winning HLC implies equal row content).
  Future<Map<String, String>> snapshot(AppDatabase db) async {
    final out = <String, String>{};
    for (final table in syncedTableRegistry(db).keys) {
      final rows = await db
          .customSelect('SELECT id, hlc_packed, deleted_at FROM $table')
          .get();
      for (final r in rows) {
        out['$table/${r.data['id']}'] =
            '${r.data['hlc_packed']}|${r.data['deleted_at']}';
      }
    }
    return out;
  }

  Future<void> expectConverged() async {
    expect(await snapshot(b.db), equals(await snapshot(a.db)));
  }

  Future<int> count(AppDatabase db, String sql, List<String> args) async {
    final r = await db
        .customSelect(
          sql,
          variables: [for (final v in args) Variable.withString(v)],
        )
        .getSingle();
    return r.data.values.first! as int;
  }

  /// Seeds a practice on [dev]: title, one vocal take with a real (tiny) audio
  /// clip, and one note. Returns the saved aggregate.
  Future<SongPractice> seedPractice(
    Persistence dev, {
    String title = 'Song',
  }) async {
    final created = ok(await dev.songRepo.createPractice(title));
    final f = File(p.join(tmp.path, 'take_${newId()}.wav'));
    await f.writeAsBytes(const [1, 2, 3, 4, 5, 6, 7, 8]);
    final now = DateTime.now();
    final track = AudioTrack(
      id: newId(),
      kind: AudioTrackKind.vocal,
      displayName: 'Take 1',
      audioFilePath: f.path,
      durationMs: 1200,
      createdAt: now,
      versionId: created.activeVersionId,
      notes: [
        PracticeNote(
          id: newId(),
          positionMs: 100,
          content: 'original note',
          createdAt: now,
          updatedAt: now,
        ),
      ],
    );
    // Creation flow: the legacy save is the documented path for a fresh
    // aggregate (base := current); editors below always save with base.
    return ok(
      await dev.songRepo.savePractice(created.copyWith(vocalTracks: [track])),
    );
  }

  SongPractice withNoteContent(SongPractice pr, String content) => pr.copyWith(
    vocalTracks: [
      for (final t in pr.vocalTracks)
        t.copyWith(
          notes: [
            for (final n in t.notes)
              n.copyWith(content: content, updatedAt: DateTime.now()),
          ],
        ),
    ],
  );

  test('A creates a practice with a take and notes; B pulls an identical '
      'aggregate (every row hash equal)', () async {
    final saved = await seedPractice(a, title: 'Duet');
    expect((await ea.pumpPush()).failed, isFalse);
    final pull = await eb.pumpPull();
    expect(pull.failed, isFalse);
    expect(pull.applied, greaterThan(0));

    final la = await loadOn(a, saved.id);
    final lb = await loadOn(b, saved.id);
    // The wave-1 hash maps are the row-content fingerprint of the aggregate:
    // equal maps mean every synced row (practice, interpretation, track,
    // clip, note) is content-identical on both devices.
    expect(lb.baseHashes, equals(la.baseHashes));
    expect(lb.aggregate.title, 'Duet');
    expect(lb.aggregate.vocalTracks.single.displayName, 'Take 1');
    expect(lb.aggregate.vocalTracks.single.clips, hasLength(1));
    expect(
      lb.aggregate.vocalTracks.single.notes.single.content,
      'original note',
    );
    // Applying remote changes journaled NOTHING on B (no echo).
    expect(await b.db.select(b.db.oplog).get(), isEmpty);
    await expectConverged();
  });

  test('concurrent edits to DIFFERENT rows of one practice converge to the '
      'union', () async {
    final seeded = await seedPractice(a);
    await settle();

    // A edits the note; B renames the practice — different rows, no conflict.
    final la = await loadOn(a, seeded.id);
    ok(
      await a.songRepo.savePracticeWithBase(
        withNoteContent(la.aggregate, 'note edited on A'),
        baseHashes: la.baseHashes,
      ),
    );
    final lb = await loadOn(b, seeded.id);
    ok(
      await b.songRepo.savePracticeWithBase(
        lb.aggregate.copyWith(title: 'renamed on B'),
        baseHashes: lb.baseHashes,
      ),
    );

    await settle();

    for (final dev in [a, b]) {
      final loaded = await loadOn(dev, seeded.id);
      expect(loaded.aggregate.title, 'renamed on B');
      expect(
        loaded.aggregate.vocalTracks.single.notes.single.content,
        'note edited on A',
      );
    }
    await expectConverged();
  });

  test('concurrent edits to the SAME row: LWW by hlcPacked, both devices '
      'converge to the later writer', () async {
    final seeded = await seedPractice(a);
    await settle();

    final la = await loadOn(a, seeded.id);
    ok(
      await a.songRepo.savePracticeWithBase(
        la.aggregate.copyWith(title: 'from A'),
        baseHashes: la.baseHashes,
      ),
    );
    await nextMs(); // B's HLC is now strictly greater: B deterministically wins
    final lb = await loadOn(b, seeded.id);
    ok(
      await b.songRepo.savePracticeWithBase(
        lb.aggregate.copyWith(title: 'from B'),
        baseHashes: lb.baseHashes,
      ),
    );

    await settle();

    expect((await loadOn(a, seeded.id)).aggregate.title, 'from B');
    expect((await loadOn(b, seeded.id)).aggregate.title, 'from B');
    await expectConverged();
  });

  test('tombstone propagation: A deletes; B pushes a STALE child edit after '
      'the delete; no zombie live children survive anywhere', () async {
    final seeded = await seedPractice(a);
    await settle();

    // B opens the practice BEFORE the delete reaches it.
    final lb = await loadOn(b, seeded.id);

    // A deletes the whole practice (cascade tombstones) and pushes.
    ok(await a.songRepo.deletePractice(seeded.id));
    expect((await ea.pumpPush()).failed, isFalse);

    // B, unaware, edits a child track of the now-deleted practice — with a
    // strictly newer HLC, so plain LWW alone would keep the track alive.
    await nextMs();
    final stale = lb.aggregate.copyWith(
      vocalTracks: [
        lb.aggregate.vocalTracks.single.copyWith(displayName: 'zombie rename'),
      ],
    );
    ok(await b.songRepo.savePracticeWithBase(stale, baseHashes: lb.baseHashes));

    // Full exchange, including the extra rounds that propagate each side's
    // integrity repairs (the engine runs them after every applied page).
    await settle();

    for (final dev in [a, b]) {
      // The practice is tombstoned...
      final live = await count(
        dev.db,
        'SELECT COUNT(*) AS n FROM practices WHERE id = ? AND deleted_at IS NULL',
        [seeded.id],
      );
      expect(live, 0);
      // ...and NOTHING under it is left alive (the applier + the post-pull
      // IntegrityChecker keep the tombstoned-parent invariant).
      expect(
        await count(
          dev.db,
          'SELECT COUNT(*) AS n FROM tracks WHERE practice_id = ? AND deleted_at IS NULL',
          [seeded.id],
        ),
        0,
        reason: 'no zombie live tracks',
      );
      expect(
        await count(
          dev.db,
          'SELECT COUNT(*) AS n FROM interpretations WHERE practice_id = ? AND deleted_at IS NULL',
          [seeded.id],
        ),
        0,
      );
      expect(
        await count(
          dev.db,
          'SELECT COUNT(*) AS n FROM clips c JOIN tracks t ON t.id = c.track_id '
          'WHERE t.practice_id = ? AND c.deleted_at IS NULL',
          [seeded.id],
        ),
        0,
      );
      expect(
        await count(
          dev.db,
          'SELECT COUNT(*) AS n FROM practice_notes n JOIN tracks t ON t.id = n.track_id '
          'WHERE t.practice_id = ? AND n.deleted_at IS NULL',
          [seeded.id],
        ),
        0,
      );
      // The list no longer shows it.
      final list = ok(await dev.songRepo.listPractices());
      expect(list.where((x) => x.id == seeded.id), isEmpty);
    }
    await expectConverged();
  });

  test('offline device catches up in pages and converges', () async {
    final seeded = await seedPractice(a);
    await settle();

    tb.partitioned = true;

    // Three sequential edits on A, each pushed separately (three log entries).
    var la = await loadOn(a, seeded.id);
    ok(
      await a.songRepo.savePracticeWithBase(
        la.aggregate.copyWith(title: 'title v2'),
        baseHashes: la.baseHashes,
      ),
    );
    expect((await ea.pumpPush()).failed, isFalse);

    la = await loadOn(a, seeded.id);
    ok(
      await a.songRepo.savePracticeWithBase(
        withNoteContent(la.aggregate, 'note v2'),
        baseHashes: la.baseHashes,
      ),
    );
    expect((await ea.pumpPush()).failed, isFalse);

    la = await loadOn(a, seeded.id);
    final track = la.aggregate.vocalTracks.single;
    final now = DateTime.now();
    ok(
      await a.songRepo.savePracticeWithBase(
        la.aggregate.copyWith(
          vocalTracks: [
            track.copyWith(
              notes: [
                ...track.notes,
                PracticeNote(
                  id: newId(),
                  positionMs: 900,
                  content: 'second note',
                  createdAt: now,
                  updatedAt: now,
                ),
              ],
            ),
          ],
        ),
        baseHashes: la.baseHashes,
      ),
    );
    expect((await ea.pumpPush()).failed, isFalse);

    // Partitioned: the pull fails and moves nothing.
    final offline = await eb.pumpPull();
    expect(offline.failed, isTrue);
    expect(offline.pages, 0);

    // Reconnect; catch up in pages of 2 (3 changed rows -> 2 pages).
    tb.partitioned = false;
    tb.pageSize = 2;
    final catchUp = await eb.pumpPull();
    expect(catchUp.failed, isFalse);
    expect(catchUp.pages, 2, reason: '3 rows at page size 2');
    expect(catchUp.applied, 3);

    final onB = await loadOn(b, seeded.id);
    expect(onB.aggregate.title, 'title v2');
    expect(
      onB.aggregate.vocalTracks.single.notes.map((n) => n.content).toSet(),
      {'note v2', 'second note'},
    );
    await expectConverged();
  });

  test(
    'three-way stale editor END TO END: an edit synced in underneath an '
    'open aggregate survives that aggregate\'s later save on both devices',
    () async {
      final seeded = await seedPractice(a);
      await settle();

      // B "opens the editor": holds the aggregate + its base hashes in memory.
      final open = await loadOn(b, seeded.id);

      // A polishes the note and syncs.
      final la = await loadOn(a, seeded.id);
      ok(
        await a.songRepo.savePracticeWithBase(
          withNoteContent(la.aggregate, 'polished by A'),
          baseHashes: la.baseHashes,
        ),
      );
      expect((await ea.pumpPush()).failed, isFalse);

      // A's edit lands in B's DATABASE while B's editor still shows the stale
      // aggregate (this is exactly the window three-way reconcile exists for).
      expect((await eb.pumpPull()).failed, isFalse);

      // B now saves its STALE aggregate (note still 'original note') with an
      // unrelated edit. The base hashes prove B never touched the note, so the
      // save must keep A's version instead of reverting it.
      await nextMs();
      ok(
        await b.songRepo.savePracticeWithBase(
          open.aggregate.copyWith(title: 'renamed by B'),
          baseHashes: open.baseHashes,
        ),
      );

      await settle();

      for (final dev in [a, b]) {
        final loaded = await loadOn(dev, seeded.id);
        expect(
          loaded.aggregate.vocalTracks.single.notes.single.content,
          'polished by A',
          reason:
              "A's edit must survive B's stale save on ${dev == a ? 'A' : 'B'}",
        );
        expect(loaded.aggregate.title, 'renamed by B');
      }
      await expectConverged();
    },
  );
}
